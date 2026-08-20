use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncBufRead, AsyncRead, BufReader, ReadBuf};

pub(crate) const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
pub(crate) const MAX_LSP_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct BoundedLspReader<R> {
    inner: BufReader<R>,
    header: Vec<u8>,
    state: ReadState,
}

#[derive(Clone, Copy)]
enum ReadState {
    Header,
    EmitHeader { offset: usize, body_len: usize },
    Body { remaining: usize },
    Closed,
}

impl<R: AsyncRead> BoundedLspReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self::with_capacity(inner, MAX_LSP_HEADER_BYTES)
    }

    fn with_capacity(inner: R, capacity: usize) -> Self {
        Self {
            inner: BufReader::with_capacity(capacity, inner),
            header: Vec::with_capacity(MAX_LSP_HEADER_BYTES),
            state: ReadState::Header,
        }
    }

    fn fail(&mut self, kind: io::ErrorKind, message: &'static str) -> Poll<io::Result<()>> {
        self.state = ReadState::Closed;
        Poll::Ready(Err(io::Error::new(kind, message)))
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BoundedLspReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let this = self.as_mut().get_mut();

        loop {
            match this.state {
                ReadState::Header => {
                    let consumed = {
                        let inner = &mut this.inner;
                        let header = &mut this.header;
                        let available = ready!(Pin::new(inner).poll_fill_buf(cx))?;
                        if available.is_empty() {
                            if header.is_empty() {
                                this.state = ReadState::Closed;
                                return Poll::Ready(Ok(()));
                            }
                            return this.fail(
                                io::ErrorKind::UnexpectedEof,
                                "LSP input ended inside a frame header",
                            );
                        }

                        let mut consumed = 0;
                        while consumed < available.len() && header.len() < MAX_LSP_HEADER_BYTES {
                            header.push(available[consumed]);
                            consumed += 1;
                            if header.ends_with(b"\r\n\r\n") {
                                break;
                            }
                        }
                        consumed
                    };
                    Pin::new(&mut this.inner).consume(consumed);

                    if this.header.ends_with(b"\r\n\r\n") {
                        let body_len = match content_length(&this.header) {
                            Ok(len) if len <= MAX_LSP_FRAME_BYTES => len,
                            Ok(_) => {
                                return this.fail(
                                    io::ErrorKind::InvalidData,
                                    "LSP frame exceeds the inbound byte limit",
                                );
                            }
                            Err(error) => {
                                this.state = ReadState::Closed;
                                return Poll::Ready(Err(error));
                            }
                        };
                        this.state = ReadState::EmitHeader {
                            offset: 0,
                            body_len,
                        };
                    } else if this.header.len() == MAX_LSP_HEADER_BYTES {
                        return this.fail(
                            io::ErrorKind::InvalidData,
                            "LSP frame header exceeds the inbound byte limit",
                        );
                    }
                }
                ReadState::EmitHeader { offset, body_len } => {
                    let count = output.remaining().min(this.header.len() - offset);
                    output.put_slice(&this.header[offset..offset + count]);
                    let next = offset + count;
                    if next == this.header.len() {
                        this.header.clear();
                        this.state = ReadState::Body {
                            remaining: body_len,
                        };
                    } else {
                        this.state = ReadState::EmitHeader {
                            offset: next,
                            body_len,
                        };
                    }
                    return Poll::Ready(Ok(()));
                }
                ReadState::Body { remaining: 0 } => {
                    this.state = ReadState::Header;
                }
                ReadState::Body { remaining } => {
                    let (count, exhausted) = {
                        let available = ready!(Pin::new(&mut this.inner).poll_fill_buf(cx))?;
                        if available.is_empty() {
                            return this.fail(
                                io::ErrorKind::UnexpectedEof,
                                "LSP input ended inside a frame body",
                            );
                        }
                        let count = output.remaining().min(remaining).min(available.len());
                        output.put_slice(&available[..count]);
                        (count, count == remaining)
                    };
                    Pin::new(&mut this.inner).consume(count);
                    this.state = if exhausted {
                        ReadState::Header
                    } else {
                        ReadState::Body {
                            remaining: remaining - count,
                        }
                    };
                    return Poll::Ready(Ok(()));
                }
                ReadState::Closed => return Poll::Ready(Ok(())),
            }
        }
    }
}

fn content_length(header: &[u8]) -> io::Result<usize> {
    let header = std::str::from_utf8(header)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "LSP frame header is not UTF-8"))?;
    let mut length = None;
    for line in header
        .strip_suffix("\r\n\r\n")
        .unwrap_or(header)
        .split("\r\n")
    {
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP frame header is malformed",
            ));
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP frame has duplicate Content-Length headers",
                ));
            }
            length = Some(value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP frame Content-Length is invalid",
                )
            })?);
        }
    }
    length.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP frame is missing Content-Length",
        )
    })
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    fn frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[tokio::test]
    async fn passes_multiple_frames_without_crossing_boundaries() {
        let mut input = frame("first");
        input.extend(frame("second"));
        let mut reader = BoundedLspReader::with_capacity(input.as_slice(), 2);
        let mut output = Vec::new();

        reader.read_to_end(&mut output).await.unwrap();

        assert_eq!(output, input);
    }

    #[tokio::test]
    async fn returns_one_complete_frame_without_waiting_for_the_next() {
        let input = frame("body");
        let (mut writer, reader) = tokio::io::duplex(1024);
        writer.write_all(&input).await.unwrap();
        let mut reader = BoundedLspReader::new(reader);
        let mut output = vec![0; input.len()];

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reader.read_exact(&mut output),
        )
        .await
        .expect("reader waited for another frame")
        .unwrap();

        assert_eq!(output, input);
    }

    #[tokio::test]
    async fn rejects_an_oversized_frame_before_reading_its_body() {
        let input = format!("Content-Length: {}\r\n\r\n", MAX_LSP_FRAME_BYTES + 1);
        let mut reader = BoundedLspReader::new(input.as_bytes());
        let mut output = Vec::new();

        let error = reader.read_to_end(&mut output).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn rejects_a_header_over_the_limit() {
        let input = vec![b'x'; MAX_LSP_HEADER_BYTES + 1];
        let mut reader = BoundedLspReader::new(input.as_slice());
        let mut output = Vec::new();

        let error = reader.read_to_end(&mut output).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn accepts_a_header_at_the_limit() {
        let prefix = b"Content-Length: 0\r\nX-Pad: ";
        let suffix = b"\r\n\r\n";
        let padding = MAX_LSP_HEADER_BYTES - prefix.len() - suffix.len();
        let mut input = prefix.to_vec();
        input.extend(vec![b'x'; padding]);
        input.extend(suffix);
        let mut reader = BoundedLspReader::new(input.as_slice());
        let mut output = Vec::new();

        reader.read_to_end(&mut output).await.unwrap();

        assert_eq!(output, input);
    }

    #[tokio::test]
    async fn accepts_a_declared_body_at_the_limit() {
        let input = format!("Content-Length: {MAX_LSP_FRAME_BYTES}\r\n\r\n");
        let mut reader = BoundedLspReader::new(input.as_bytes());
        let mut output = Vec::new();

        let error = reader.read_to_end(&mut output).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(output, input.as_bytes());
    }

    #[tokio::test]
    async fn passes_a_zero_length_frame_before_the_next_frame() {
        let mut input = frame("");
        input.extend(frame("next"));
        let mut reader = BoundedLspReader::with_capacity(input.as_slice(), 2);
        let mut output = Vec::new();

        reader.read_to_end(&mut output).await.unwrap();

        assert_eq!(output, input);
    }

    #[tokio::test]
    async fn rejects_input_ending_inside_a_header() {
        let input = b"Content-Length: 4\r\n";
        let mut reader = BoundedLspReader::new(input.as_slice());
        let mut output = Vec::new();

        let error = reader.read_to_end(&mut output).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn rejects_input_ending_inside_a_body() {
        let input = b"Content-Length: 4\r\n\r\nab";
        let mut reader = BoundedLspReader::new(input.as_slice());
        let mut output = Vec::new();

        let error = reader.read_to_end(&mut output).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(output, input);
    }

    #[test]
    fn rejects_missing_duplicate_and_invalid_content_lengths() {
        for header in [
            b"Content-Type: application/json\r\n\r\n".as_slice(),
            b"Content-Length: 1\r\nContent-Length: 2\r\n\r\n".as_slice(),
            b"Content-Length: nope\r\n\r\n".as_slice(),
            b"Content-Length 1\r\n\r\n".as_slice(),
            b"Content-Length: \xff\r\n\r\n".as_slice(),
        ] {
            assert_eq!(
                content_length(header).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn parses_content_length_case_insensitively() {
        assert_eq!(content_length(b"content-length: 12\r\n\r\n").unwrap(), 12);
    }
}
