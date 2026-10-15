//! What `corrupt.rs` and `errors_corrupt.rs` both need. Every item here is used
//! by both binaries, so neither trips `dead_code`.

use std::path::Path;

use cwtools_cache::io::CacheError;

/// The bytes `write` leaves in a fresh temp file ending in `suffix`.
pub fn written_bytes(suffix: &str, write: impl FnOnce(&Path)) -> Vec<u8> {
    let tmp = tempfile::NamedTempFile::with_suffix(suffix).unwrap();
    write(tmp.path());
    std::fs::read(tmp.path()).unwrap()
}

/// Whether `error` is the header refusal carrying `msg`.
pub fn is_bad_header(error: &CacheError, msg: &str) -> bool {
    matches!(error, CacheError::Deserialize { msg: got, source: None } if *got == msg)
}
