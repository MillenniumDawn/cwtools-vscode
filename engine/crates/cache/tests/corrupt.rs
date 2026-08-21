//! Header validation and corruption handling for `.cwb` files.
//!
//! Every case here has to come back as a `CacheError`, because that is what the
//! consumers rely on: `cwtools_cache::workspace::load` collapses any error to
//! `None` and re-parses the source. A panic instead would take the server or CLI
//! down over a cache file that a crash, a full disk, or a stale format left behind.

use cwtools_cache::cache_format::{
    CachedChild, CachedFile, CachedLeaf, CachedOperator, CachedValue,
};
use cwtools_cache::convert;
use cwtools_cache::io::{self, CacheError};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::path::Path;

const MAGIC: [u8; 4] = *b"CWB\x00";
const FORMAT_VERSION: u8 = 4;

/// A small but structurally complete cache: leaves, a nested clause, a comment.
fn sample_bytes() -> Vec<u8> {
    let input = r#"
# a comment
foo = bar
nested = {
    a = 1
    b = "hello"
    c = yes
}
"#;
    let table = StringTable::new();
    let parsed = parse_string(input, &table);
    let cached = convert::arena_to_cached(&parsed.arena, &parsed.root_children, &table);

    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, tmp.path()).unwrap();
    std::fs::read(tmp.path()).unwrap()
}

/// Write `bytes` to a temp `.cwb` and take it through the full load path the
/// consumers use: header check, rkyv access, arena rebuild.
fn load(bytes: &[u8]) -> Result<Result<(), CacheError>, CacheError> {
    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    std::fs::write(tmp.path(), bytes).unwrap();
    load_path(tmp.path())
}

fn load_path(path: &Path) -> Result<Result<(), CacheError>, CacheError> {
    fingerprint_path(path).map(|inner| inner.map(|_| ()))
}

/// Load and reduce the rebuilt arena to a comparable string, so a corruption
/// that decodes into a *different* AST is distinguishable from one that
/// reproduces the original.
fn fingerprint_path(path: &Path) -> Result<Result<String, CacheError>, CacheError> {
    let table = StringTable::new();
    io::with_archived_file(path, |archived| {
        convert::archived_to_arena(archived, &table).map(|(arena, root)| {
            use std::fmt::Write;
            let mut out = String::new();
            let _ = write!(out, "{root:?}");
            for l in &arena.leaves {
                let _ = write!(
                    out,
                    "|L {:?} {:?} {:?} {:?} {:?}",
                    table.get_string(l.key.normal),
                    l.value,
                    l.op,
                    l.pos,
                    l.value_pos
                );
            }
            for lv in &arena.leaf_values {
                let _ = write!(out, "|V {:?} {:?}", lv.value, lv.pos);
            }
            for c in &arena.comments {
                let _ = write!(out, "|C {:?} {:?}", c.text, c.pos);
            }
            out
        })
    })
}

fn fingerprint(bytes: &[u8]) -> Result<Result<String, CacheError>, CacheError> {
    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    std::fs::write(tmp.path(), bytes).unwrap();
    fingerprint_path(tmp.path())
}

fn is_bad_header(e: &CacheError) -> bool {
    matches!(
        e,
        CacheError::Deserialize {
            msg: "incompatible or missing cache header",
            ..
        }
    )
}

/// Sanity anchor: the unmodified bytes every other test mutates do load.
#[test]
fn sample_bytes_load_clean() {
    let bytes = sample_bytes();
    assert!(bytes.starts_with(&MAGIC), "sample lost its magic");
    assert_eq!(bytes[MAGIC.len()], FORMAT_VERSION, "sample version drifted");
    load(&bytes)
        .expect("sample must load")
        .expect("sample must convert");
}

/// An empty file is what a crash mid-write leaves behind. It must not reach
/// rkyv at all.
#[test]
fn zero_byte_file_is_rejected_as_a_bad_header() {
    let err = load(&[]).expect_err("empty file must not load");
    assert!(is_bad_header(&err), "got {err:?}");
}

/// Any prefix shorter than magic+version is the same crash-mid-write case.
#[test]
fn header_shorter_than_magic_plus_version_is_rejected() {
    let bytes = sample_bytes();
    for len in 0..MAGIC.len() + 1 {
        let err = load(&bytes[..len]).unwrap_err();
        assert!(is_bad_header(&err), "len {len} gave {err:?}");
    }
}

/// The point of `FORMAT_VERSION`: a `.cwb` from an older layout (v1, v2) or a
/// newer one is refused rather than reinterpreted. Two format bumps have
/// happened and neither was covered.
#[test]
fn wrong_format_version_is_rejected() {
    let bytes = sample_bytes();
    for version in [0u8, 1, 2, FORMAT_VERSION + 1, 255] {
        let mut corrupt = bytes.clone();
        corrupt[MAGIC.len()] = version;
        let err = load(&corrupt).unwrap_err();
        assert!(is_bad_header(&err), "version {version} gave {err:?}");
    }
}

/// A pre-v1 file was raw zstd with no header at all, and the first four bytes
/// of a zstd frame are not `CWB\0`.
#[test]
fn missing_magic_is_rejected() {
    let bytes = sample_bytes();
    let headerless = &bytes[MAGIC.len() + 1..];
    let err = load(headerless).unwrap_err();
    assert!(is_bad_header(&err), "got {err:?}");

    // And a single wrong magic byte is enough.
    for i in 0..MAGIC.len() {
        let mut corrupt = bytes.clone();
        corrupt[i] ^= 0xff;
        let err = load(&corrupt).unwrap_err();
        assert!(is_bad_header(&err), "magic byte {i} gave {err:?}");
    }
}

/// A valid header over a body that stops early: the write was interrupted after
/// the header landed. zstd has to reject it, not the caller.
#[test]
fn truncated_payload_is_rejected() {
    let bytes = sample_bytes();
    let body_start = MAGIC.len() + 1;
    assert!(
        bytes.len() > body_start + 8,
        "sample body too small to truncate"
    );

    for len in body_start..bytes.len() {
        let truncated = &bytes[..len];
        match load(truncated) {
            Err(_) => {}
            Ok(inner) => {
                // A short read that still decompresses must at least be caught
                // by rkyv or the arena bounds check.
                assert!(
                    inner.is_err(),
                    "truncating to {len} of {} bytes loaded as valid",
                    bytes.len()
                );
            }
        }
    }
}

/// Bit rot in the compressed body. Every single-bit flip must either fail to
/// load or rebuild the original AST byte for byte. Silently decoding into a
/// *different* AST is the bad case: rkyv's checked `access` validates structure,
/// not content, so a flipped string byte or line number would sail through and
/// the editor would work off a parse tree that never existed.
///
/// The zstd frame checksum is what closes that. With it off, 66 of 507 flips
/// here decoded into an altered arena and were served as cache hits.
#[test]
fn bit_flipped_payload_never_yields_an_altered_ast() {
    let bytes = sample_bytes();
    let body_start = MAGIC.len() + 1;
    let clean = fingerprint(&bytes).unwrap().unwrap();
    let mut same = 0usize;
    let mut altered = 0usize;

    for i in body_start..bytes.len() {
        for bit in [0u8, 3, 7] {
            let mut corrupt = bytes.clone();
            corrupt[i] ^= 1 << bit;
            // Reaching the next line at all is the test: a panic inside the
            // load path fails here instead of degrading to a cache miss.
            if let Ok(Ok(got)) = fingerprint(&corrupt) {
                if got == clean {
                    same += 1;
                } else {
                    altered += 1;
                }
            }
        }
    }

    let attempts = (bytes.len() - body_start) * 3;
    assert_eq!(
        altered, 0,
        "{altered} of {attempts} bit flips decoded into a different AST and \
         were accepted as a cache hit"
    );
    // A handful of flips land in frame bits the decoder ignores and reproduce
    // the original exactly. Harmless, but if it were most of them the checksum
    // would not be doing anything.
    assert!(
        same * 4 < attempts,
        "{same} of {attempts} bit flips round-tripped unchanged"
    );
}

/// The frame checksum is a write-side setting only. A `.cwb` written before it
/// was turned on has no checksum in its frame and must still load, which is why
/// enabling it did not need a FORMAT_VERSION bump or a cache wipe.
#[test]
fn checksumless_frame_still_loads() {
    let input = "foo = bar\n";
    let table = StringTable::new();
    let parsed = parse_string(input, &table);
    let cached = convert::arena_to_cached(&parsed.arena, &parsed.root_children, &table);
    let rkyv_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cached).unwrap();

    // `zstd::encode_all` is what the crate used before, and it writes no
    // frame checksum.
    let mut old_style = MAGIC.to_vec();
    old_style.push(FORMAT_VERSION);
    old_style.extend_from_slice(&zstd::encode_all(&rkyv_bytes[..], 3).unwrap());

    let fp = fingerprint(&old_style)
        .expect("a checksumless frame must still pass the header + zstd path")
        .expect("and still rebuild an arena");

    let mut current = Vec::new();
    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, tmp.path()).unwrap();
    current.extend_from_slice(&std::fs::read(tmp.path()).unwrap());
    let fp_current = fingerprint(&current).unwrap().unwrap();

    assert_eq!(
        fp, fp_current,
        "old and new frames must rebuild the same AST"
    );
}

/// A body that is not zstd at all (someone dropped a text file in the cache
/// dir and it happened to get the right header).
#[test]
fn non_zstd_payload_is_a_compression_error() {
    let mut bytes = MAGIC.to_vec();
    bytes.push(FORMAT_VERSION);
    bytes.extend_from_slice(b"this is not a zstd frame, it is prose");
    let err = load(&bytes).unwrap_err();
    assert!(
        matches!(err, CacheError::Compression(_)),
        "expected a compression error, got {err:?}"
    );
}

/// A missing cache file is the ordinary cold-start miss, and has to surface as
/// IO rather than anything the caller must special-case.
#[test]
fn missing_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_path(&dir.path().join("absent.cwb")).unwrap_err();
    assert!(matches!(err, CacheError::Io(_)), "got {err:?}");
}

/// Round-trip through a well-formed but semantically impossible archive: the
/// header and rkyv both accept it, so only the arena bounds check stands
/// between a corrupt index and a panicking consumer. Complements
/// `roundtrip::out_of_bounds_child_index_is_rejected` by covering the nested
/// clause path as well as the root list.
#[test]
fn out_of_bounds_index_inside_a_clause_is_rejected() {
    let cached = CachedFile {
        root_children: vec![CachedChild::Leaf(0)],
        leaves: vec![CachedLeaf {
            key: "outer".to_string(),
            // The clause points at leaf 7, and there is only leaf 0.
            value: CachedValue::Clause(vec![CachedChild::Leaf(7)]),
            op: CachedOperator::Equals,
            start_line: 1,
            start_col: 0,
            end_line: 1,
            end_col: 5,
            value_start_line: 1,
            value_start_col: 0,
            value_end_line: 1,
            value_end_col: 5,
        }],
        leaf_values: vec![],
        comments: vec![],
    };

    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, tmp.path()).unwrap();

    let inner = load_path(tmp.path()).expect("header and rkyv access should succeed");
    assert!(
        inner.is_err(),
        "out-of-bounds index inside a clause must be rejected at load"
    );
}
