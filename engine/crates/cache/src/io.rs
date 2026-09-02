use crate::cache_format::{ArchivedCachedFile, CachedErrors, CachedFile};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error")]
    Serialize(#[source] rkyv::rancor::Error),
    // `Deserialize` covers both rkyv failures (with a source) and the
    #[error("deserialization error: {msg}")]
    Deserialize {
        msg: &'static str,
        #[source]
        source: Option<rkyv::rancor::Error>,
    },
    // zstd returns `io::Error`; a `#[from]` here would collide with `Io`'s, so
    #[error("compression error")]
    Compression(#[source] std::io::Error),
}

/// zstd compression level for cache bodies. Shared by the `.cwb` parse cache
/// checksum on top; see `serialize_to_file`.
pub const ZSTD_LEVEL: i32 = 3;

/// reject files written by an incompatible layout before rkyv gets confused.
const MAGIC: &[u8; 4] = b"CWB\x00";

/// Format version. Bump whenever the rkyv layout changes (e.g. widening a field
/// v1: initial versioned format (adds magic+version header to the raw zstd).
const FORMAT_VERSION: u8 = 4;

const ERRORS_MAGIC: &[u8; 4] = b"CWE\x00";
const ERRORS_FORMAT_VERSION: u8 = 2;

/// Hard caps on a `.cwb` read and on what its body may decompress to. A cache
/// path is chosen by a CLI flag or an LSP client (#162), so neither the read nor
/// the decode may run unbounded: a few kilobytes of magic-prefixed zstd can
pub const MAX_ARCHIVE_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ARCHIVE_DECODED_BYTES: u64 = 128 * 1024 * 1024;

/// Cap on a `.cwe` sidecar. Uncompressed rkyv, and it only ever holds one file's
pub const MAX_ERRORS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// runs past `max_bytes`.
/// between the stat and the read still cannot outrun the cap.
pub fn read_capped(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let mut data = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut data)?;
    if data.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is over the {max_bytes} byte cache read cap",
                path.display()
            ),
        ));
    }
    Ok(data)
}

/// Decompress `compressed` into `out`, refusing to write past `max_bytes`.
/// would cross the cap, before `out` grows to hold it.
pub fn decode_capped<W: Write>(compressed: &[u8], max_bytes: u64, out: W) -> std::io::Result<()> {
    if let Ok(Some(declared)) = zstd::zstd_safe::get_frame_content_size(compressed)
        && declared > max_bytes
    {
        return Err(over_decode_cap(max_bytes));
    }
    zstd::stream::copy_decode(
        compressed,
        CappedWriter {
            inner: out,
            remaining: max_bytes,
            max: max_bytes,
        },
    )
}

fn over_decode_cap(max_bytes: u64) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("cache body decompresses past the {max_bytes} byte cap"),
    )
}

struct CappedWriter<W> {
    inner: W,
    remaining: u64,
    max: u64,
}

impl<W: Write> Write for CappedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.remaining {
            return Err(over_decode_cap(self.max));
        }
        let written = self.inner.write(buf)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

fn temp_path(path: &Path) -> PathBuf {
    let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{}-{id}", std::process::id()));
    PathBuf::from(temp)
}

pub fn write_atomically(
    path: &Path,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let temp = temp_path(path);
    let write_result = (|| -> std::io::Result<()> {
        let mut file = File::create(&temp)?;
        write(&mut file)
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }

    if let Err(error) = std::fs::rename(&temp, path) {
        #[cfg(windows)]
        if path.exists() {
            // Windows rename can refuse an existing destination (a reader holding
            let replaced = std::fs::remove_file(path).and_then(|()| std::fs::rename(&temp, path));
            if let Err(error) = replaced {
                let _ = std::fs::remove_file(&temp);
                return Err(error);
            }
            return Ok(());
        }
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

/// Serialize a `CachedFile` to a `.cwb` file (zstd-compressed rkyv).
/// Layout: `MAGIC (4 bytes) | FORMAT_VERSION (1 byte) | zstd(rkyv bytes)`.
pub fn serialize_to_file(cached: &CachedFile, path: &Path) -> Result<(), CacheError> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(cached).map_err(CacheError::Serialize)?;

    // Frame checksum on. rkyv's checked `access` validates structure, not
    // archive and get served as a cache hit. The checksum turns that into a
    let compressed = {
        let mut encoder =
            zstd::stream::Encoder::new(Vec::new(), ZSTD_LEVEL).map_err(CacheError::Compression)?;
        encoder
            .include_checksum(true)
            .map_err(CacheError::Compression)?;
        encoder.write_all(&bytes).map_err(CacheError::Compression)?;
        encoder.finish().map_err(CacheError::Compression)?
    };

    write_atomically(path, |file| {
        file.write_all(MAGIC)?;
        file.write_all(&[FORMAT_VERSION])?;
        file.write_all(&compressed)
    })
    .map_err(CacheError::Io)
}

/// Read a `.cwb` file, validate its header, and return the decompressed rkyv
fn read_archive_bytes(path: &Path) -> Result<rkyv::util::AlignedVec, CacheError> {
    let data = read_capped(path, MAX_ARCHIVE_FILE_BYTES)?;

    // letting rkyv silently misread mismatched bytes.
    if data.len() < MAGIC.len() + 1
        || &data[..MAGIC.len()] != MAGIC
        || data[MAGIC.len()] != FORMAT_VERSION
    {
        return Err(CacheError::Deserialize {
            msg: "incompatible or missing cache header",
            source: None,
        });
    }
    let compressed = &data[MAGIC.len() + 1..];

    let mut aligned = rkyv::util::AlignedVec::new();
    decode_capped(compressed, MAX_ARCHIVE_DECODED_BYTES, &mut aligned)
        .map_err(CacheError::Compression)?;
    Ok(aligned)
}

pub fn with_archived_file<R>(
    path: &Path,
    f: impl FnOnce(&ArchivedCachedFile) -> R,
) -> Result<R, CacheError> {
    let bytes = read_archive_bytes(path)?;
    let archived =
        rkyv::access::<ArchivedCachedFile, rkyv::rancor::Error>(&bytes).map_err(|e| {
            CacheError::Deserialize {
                msg: "rkyv access failed",
                source: Some(e),
            }
        })?;
    Ok(f(archived))
}

pub fn serialize_errors_to_file(cached: &CachedErrors, path: &Path) -> Result<(), CacheError> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(cached).map_err(CacheError::Serialize)?;
    write_atomically(path, |file| {
        file.write_all(ERRORS_MAGIC)?;
        file.write_all(&[ERRORS_FORMAT_VERSION])?;
        file.write_all(&bytes)
    })
    .map_err(CacheError::Io)
}

pub fn read_errors_from_file(path: &Path) -> Result<CachedErrors, CacheError> {
    let data = read_capped(path, MAX_ERRORS_FILE_BYTES)?;
    if data.len() < ERRORS_MAGIC.len() + 1
        || &data[..ERRORS_MAGIC.len()] != ERRORS_MAGIC
        || data[ERRORS_MAGIC.len()] != ERRORS_FORMAT_VERSION
    {
        return Err(CacheError::Deserialize {
            msg: "incompatible or missing error-cache header",
            source: None,
        });
    }
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(&data[ERRORS_MAGIC.len() + 1..]);
    rkyv::from_bytes::<CachedErrors, rkyv::rancor::Error>(&aligned).map_err(|error| {
        CacheError::Deserialize {
            msg: "error-cache rkyv access failed",
            source: Some(error),
        }
    })
}
