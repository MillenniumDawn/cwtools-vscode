//! Golden format/layout contract for the `.cwb` rkyv payload.

use cwtools_cache::cache_format::CachedFile;
use cwtools_cache::{convert, io};
use cwtools_parser::ast::{Arena, Child, Value};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::fmt::Write as _;

const MAGIC: &[u8; 4] = b"CWB\0";
const FORMAT_VERSION: u8 = 4;
const HEADER_LEN: usize = MAGIC.len() + 1;
const SOURCE: &str = r#"# issue 520 layout fixture
alpha = "one"
nested = {
    count = 42
    enabled = yes
}
"#;
const FIXTURE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/layout-v4.cwb");

fn canonical(arena: &Arena, roots: &[Child], table: &StringTable) -> String {
    fn canonical_value(value: &Value, table: &StringTable, out: &mut String) {
        match value {
            Value::String(token) => {
                let _ = write!(out, "String({:?})", table.get_string(token.normal));
            }
            Value::QString(token) => {
                let _ = write!(out, "QString({:?})", table.get_string(token.normal));
            }
            Value::Float(number) => {
                let _ = write!(out, "Float({number:?})");
            }
            Value::Int(number) => {
                let _ = write!(out, "Int({number})");
            }
            Value::Bool(value) => {
                let _ = write!(out, "Bool({value})");
            }
            Value::Clause(children) => {
                let _ = write!(out, "Clause({children:?})");
            }
        }
    }

    let mut out = format!("roots={roots:?}");
    for leaf in &arena.leaves {
        let _ = write!(
            out,
            "|leaf key={:?} op={:?} pos={:?} value_pos={:?} value=",
            table.get_string(leaf.key.normal),
            leaf.op,
            leaf.pos,
            leaf.value_pos
        );
        canonical_value(&leaf.value, table, &mut out);
    }
    for leaf_value in &arena.leaf_values {
        let _ = write!(out, "|leaf_value pos={:?} value=", leaf_value.pos);
        canonical_value(&leaf_value.value, table, &mut out);
    }
    for comment in &arena.comments {
        let _ = write!(
            out,
            "|comment text={:?} pos={:?}",
            comment.text, comment.pos
        );
    }
    out
}

fn payload(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len() >= HEADER_LEN);
    zstd::decode_all(&bytes[HEADER_LEN..]).unwrap()
}

fn serialized_sample() -> (CachedFile, String) {
    let table = StringTable::new();
    let parsed = parse_string(SOURCE, &table);
    let canonical = canonical(&parsed.arena, &parsed.root_children, &table);
    let cached = convert::arena_to_cached(&parsed.arena, &parsed.root_children, &table);
    (cached, canonical)
}

#[test]
fn versioned_layout_fixture_has_expected_ast_and_rkyv_payload() {
    let (cached, expected_semantics) = serialized_sample();
    let generated = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, generated.path()).unwrap();
    let generated = std::fs::read(generated.path()).unwrap();

    if std::env::var_os("CWTOOLS_UPDATE_LAYOUT_FIXTURE").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        let parent = std::path::Path::new(FIXTURE_PATH).parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(FIXTURE_PATH, &generated).unwrap();
    }

    let fixture = std::fs::read(FIXTURE_PATH).expect("committed cache format fixture");
    let mut expected_header = MAGIC.to_vec();
    expected_header.push(FORMAT_VERSION);
    assert_eq!(&fixture[..HEADER_LEN], expected_header);
    assert_eq!(
        &generated[..HEADER_LEN],
        &fixture[..HEADER_LEN],
        "serializer header/version differs from the committed layout fixture"
    );

    let fixture_tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    std::fs::write(fixture_tmp.path(), &fixture).unwrap();
    let table = StringTable::new();
    let actual_semantics = io::with_archived_file(fixture_tmp.path(), |archived| {
        let (arena, roots) = convert::archived_to_arena(archived, &table).unwrap();
        canonical(&arena, &roots, &table)
    })
    .unwrap();
    assert_eq!(actual_semantics, expected_semantics);

    assert_eq!(
        payload(&generated),
        payload(&fixture),
        "serialized rkyv payload changed without updating the tagged fixture"
    );
}
