use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncBufRead, AsyncRead, BufReader, ReadBuf};
use tower::Service;
use tower_lsp::jsonrpc::{Error, Request, Response};

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

/// How many requests `Server::serve` will keep in flight at once. tower-lsp's
/// own default is 4, which was enough while every handler ran to completion on
/// the pump. Now that [`SpawnRequests`] puts each one on its own task a slot is
/// usually held only for a `JoinHandle` await — except a long `executeCommand`
/// (`reindexWorkspace` and friends), which holds one for its whole run. Sized
/// well above the number of those a client can have outstanding so a slow
/// command can never wall off the queue.
pub(crate) const CONCURRENCY_LEVEL: usize = 64;

/// Runs request handlers on a task of their own so they stop owning the message
/// pump (#470).
///
/// `Server::serve` ends in `join!(print_output, read_input,
/// process_server_tasks)` — one task for all three — and tower-lsp never
/// `tokio::spawn`s a handler future, deliberately, to stay executor agnostic. A
/// handler that blocks its thread therefore stalls the whole protocol channel:
/// stdin is not read, stdout is not written, and no notification is dispatched.
/// `block_in_place` does not help, because it moves *other* tasks off the worker
/// and never unblocks its own caller — so the fenced sections on the hover,
/// close and scan paths used to freeze the channel for their whole duration, and
/// a `window/workDoneProgress/cancel` aimed at a running scan could not even be
/// read until that scan had finished.
///
/// Notifications pass through untouched. They have to stay in the order the
/// client sent them, and the two that matter most here — `$/cancelRequest` and
/// `window/workDoneProgress/cancel` — are a map lookup and an atomic store, so
/// answering them inline is exactly what makes a cancel land promptly. The
/// document notifications that do carry real work go through the serial queue in
/// `Backend::enqueue_notification` instead.
pub(crate) struct SpawnRequests<S> {
    inner: S,
}

impl<S> SpawnRequests<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self { inner }
    }
}

/// Aborts the spawned handler if the future awaiting it is dropped.
///
/// tower-lsp answers `$/cancelRequest` by dropping the handler future
/// (`Pending::execute` wraps it in `future::abortable`), and a bare `JoinHandle`
/// detaches rather than cancels, so without this a cancelled request would keep
/// running to completion. Aborting a task that already finished is a no-op, so
/// the guard can simply drop on the success path too.
///
/// An abort only lands at a yield point: a `block_in_place` section still runs
/// to its next poll. Stopping a long scan early remains the job of
/// `CommandProgress`'s `CancelFlag`, which this change is what finally lets the
/// server observe.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<S> Service<Request> for SpawnRequests<S>
where
    S: Service<Request, Response = Option<Response>>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Option<Response>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Option<Response>, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let Some(id) = request.id().cloned() else {
            return Box::pin(self.inner.call(request));
        };
        // Only for the panic line below, and only worth the allocation on the
        // path that can produce one.
        let method = request.method().to_string();
        let handler = self.inner.call(request);

        Box::pin(async move {
            let handle = tokio::spawn(handler);
            let _abort = AbortOnDrop(handle.abort_handle());
            match handle.await {
                Ok(response) => response,
                // Only reachable through `_abort`, which fires when nobody is
                // left to read this value; answering with nothing is right
                // either way.
                Err(error) if error.is_cancelled() => Ok(None),
                Err(error) => {
                    // On the pump a panicking handler unwound into `serve()` and
                    // took the process with it. Off the pump it is one failed
                    // request.
                    tracing::error!(%error, %method, "request handler panicked");
                    Ok(Some(Response::from_error(id, Error::internal_error())))
                }
            }
        })
    }
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

    /// A handler that owns its thread the way a scan pass does. `Blocking`
    /// answers a request by parking the thread until `release` is set, and
    /// answers a notification immediately; the difference is what the two tests
    /// below measure.
    #[derive(Clone)]
    struct Blocking {
        release: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Service<Request> for Blocking {
        type Response = Option<Response>;
        type Error = std::convert::Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            let release = self.release.clone();
            let id = request.id().cloned();
            Box::pin(async move {
                let Some(id) = id else {
                    return Ok(None);
                };
                while !release.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(Some(Response::from_ok(id, serde_json::Value::Null)))
            })
        }
    }

    fn request(id: i64) -> Request {
        Request::build("textDocument/hover").id(id).finish()
    }

    fn notification() -> Request {
        Request::build("window/workDoneProgress/cancel").finish()
    }

    /// The whole point of #470: a request that owns its thread must not stop the
    /// notification behind it from being dispatched. The two futures are polled
    /// together on one task, which is what `Server::serve`'s
    /// `buffer_unordered` does — poll them on tasks of their own and the bug
    /// cannot reproduce at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_blocking_request_does_not_hold_up_the_notification_behind_it() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let release = std::sync::Arc::new(AtomicBool::new(false));
        let mut service = SpawnRequests::new(Blocking {
            release: release.clone(),
        });

        let blocked = service.call(request(1));
        let passed_through = service.call(notification());
        let dispatched = std::sync::Arc::new(AtomicBool::new(false));
        let marker = dispatched.clone();
        let pump = std::pin::pin!(async move {
            tokio::join!(blocked, async move {
                let response = passed_through.await;
                marker.store(true, Ordering::Relaxed);
                response
            })
        });

        // The request is still parked, so the pair cannot have finished.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), pump)
                .await
                .is_err()
        );
        assert!(
            dispatched.load(Ordering::Relaxed),
            "a notification behind a blocking request must still be dispatched"
        );

        // Let the parked handler's thread go; it busy-waits, so an abort alone
        // would never reach it.
        release.store(true, Ordering::Relaxed);
    }

    /// Dropping the returned future is how tower-lsp delivers `$/cancelRequest`,
    /// so it has to reach the spawned task rather than detaching it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_future_aborts_the_spawned_handler() {
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        struct Marked {
            started: std::sync::Arc<std::sync::atomic::AtomicBool>,
            finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }

        impl Service<Request> for Marked {
            type Response = Option<Response>;
            type Error = std::convert::Infallible;
            type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

            fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, _: Request) -> Self::Future {
                let started = self.started.clone();
                let finished = self.finished.clone();
                Box::pin(async move {
                    started.store(true, std::sync::atomic::Ordering::Relaxed);
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    finished.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(None)
                })
            }
        }

        let mut service = SpawnRequests::new(Marked {
            started: started.clone(),
            finished: finished.clone(),
        });
        let in_flight = service.call(request(2));
        let in_flight = std::pin::pin!(in_flight);
        // One poll to get the task spawned, then walk away from it.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), in_flight)
                .await
                .is_err()
        );

        assert!(
            started.load(std::sync::atomic::Ordering::Relaxed),
            "the handler must have been spawned at all"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !finished.load(std::sync::atomic::Ordering::Relaxed),
            "a dropped request future must abort its handler, not detach it"
        );
    }

    /// On the pump a panicking handler unwound into `serve()`. Off it, the
    /// request has to come back as an error instead of taking the server down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_handler_answers_with_an_internal_error() {
        struct Panicking;

        impl Service<Request> for Panicking {
            type Response = Option<Response>;
            type Error = std::convert::Infallible;
            type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

            fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, _: Request) -> Self::Future {
                Box::pin(async { panic!("handler exploded") })
            }
        }

        let mut service = SpawnRequests::new(Panicking);

        let response = service
            .call(request(3))
            .await
            .expect("infallible")
            .expect("a request must be answered");

        assert!(
            response.is_error(),
            "a panicking handler must answer with an error: {response:?}"
        );
    }
}
