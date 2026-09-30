//! Header validation and corruption handling for `.cwe` sidecars.

use cwtools_cache::cache_format::{CachedErrors, CachedParseError};
use cwtools_cache::io::{self, CacheError};

const MAGIC: [u8; 4] = *b"CWE\x00";
const FORMAT_VERSION: u8 = 2;
const HEADER_LEN: usize = MAGIC.len() + 1;

fn sample_bytes() -> Vec<u8> {
    let cached = CachedErrors {
        errors: vec![CachedParseError::Pos(3, 4, "invalid value".into())],
    };
    let tmp = tempfile::NamedTempFile::with_suffix(".cwe").unwrap();
    io::serialize_errors_to_file(&cached, tmp.path()).unwrap();
    std::fs::read(tmp.path()).unwrap()
}

fn read(bytes: &[u8]) -> Result<CachedErrors, CacheError> {
    let tmp = tempfile::NamedTempFile::with_suffix(".cwe").unwrap();
    std::fs::write(tmp.path(), bytes).unwrap();
    io::read_errors_from_file(tmp.path())
}

fn is_bad_header(error: &CacheError) -> bool {
    matches!(
        error,
        CacheError::Deserialize {
            msg: "incompatible or missing error-cache header",
            source: None,
        }
    )
}

#[test]
fn valid_sidecar_round_trips_errors() {
    let bytes = sample_bytes();
    assert!(bytes.starts_with(&MAGIC));
    assert_eq!(bytes[MAGIC.len()], FORMAT_VERSION);

    let parsed = cwtools_cache::convert::cached_errors_to_parse(read(&bytes).unwrap());
    assert!(matches!(
        parsed.as_slice(),
        [cwtools_parser::ast::ParseError::Pos(3, 4, message)] if message == "invalid value"
    ));
}

#[test]
fn bad_magic_and_wrong_version_are_rejected() {
    let bytes = sample_bytes();

    let mut bad_magic = bytes.clone();
    bad_magic[0] ^= 0xff;
    assert!(is_bad_header(&read(&bad_magic).unwrap_err()));

    let mut wrong_version = bytes;
    wrong_version[MAGIC.len()] = FORMAT_VERSION + 1;
    assert!(is_bad_header(&read(&wrong_version).unwrap_err()));
}

#[test]
fn truncated_sidecars_are_rejected() {
    let bytes = sample_bytes();

    for len in 0..HEADER_LEN {
        let error = read(&bytes[..len]).unwrap_err();
        assert!(is_bad_header(&error), "length {len}: {error:?}");
    }

    // Keep a valid header but remove the archive body entirely, as when a
    // write is interrupted immediately after the header has landed.
    let error = read(&bytes[..HEADER_LEN]).unwrap_err();
    assert!(
        matches!(
            error,
            CacheError::Deserialize {
                msg: "error-cache rkyv access failed",
                ..
            }
        ),
        "got {error:?}"
    );

    let truncated_len = HEADER_LEN + (bytes.len() - HEADER_LEN) / 2;
    let error = read(&bytes[..truncated_len]).unwrap_err();
    assert!(
        matches!(
            error,
            CacheError::Deserialize {
                msg: "error-cache rkyv access failed",
                ..
            }
        ),
        "truncated payload: {error:?}"
    );
}

#[test]
fn garbage_and_corrupted_archive_bodies_are_rejected() {
    let mut garbage = MAGIC.to_vec();
    garbage.push(FORMAT_VERSION);
    garbage.extend_from_slice(b"not an rkyv archive");
    assert!(matches!(
        read(&garbage),
        Err(CacheError::Deserialize {
            msg: "error-cache rkyv access failed",
            source: Some(_),
        })
    ));

    let valid = sample_bytes();
    let mut corrupt = valid[..HEADER_LEN].to_vec();
    corrupt.extend(std::iter::repeat_n(0xff, valid.len() - HEADER_LEN));
    assert!(matches!(
        read(&corrupt),
        Err(CacheError::Deserialize {
            msg: "error-cache rkyv access failed",
            source: Some(_),
        })
    ));
}
