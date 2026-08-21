use cwtools_cache::{convert, io};
use cwtools_file_manager::file_manager::{FileManager, FileManagerConfig};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;

#[test]
fn roundtrip_simple_file() {
    let input = r#"
# This is a comment
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

    // Serialize to temp file
    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, tmp.path()).unwrap();

    // Reload via the archived path and convert back to an arena.
    let table2 = StringTable::new();
    let (arena2, root2) = io::with_archived_file(tmp.path(), |archived| {
        convert::archived_to_arena(archived, &table2).unwrap()
    })
    .unwrap();

    // Verify structure counts match
    assert_eq!(arena2.leaves.len(), parsed.arena.leaves.len());
    assert_eq!(arena2.leaf_values.len(), parsed.arena.leaf_values.len());
    assert_eq!(arena2.comments.len(), parsed.arena.comments.len());
    assert_eq!(root2.len(), parsed.root_children.len());
    for (actual, expected) in arena2.leaves.iter().zip(&parsed.arena.leaves) {
        assert_eq!(actual.value_pos, expected.value_pos);
    }
}

#[test]
fn roundtrip_real_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testfiles/performancetest2/common/static_modifiers/cc_colony_events_static_modifiers.txt"
    );
    let input = std::fs::read_to_string(path).unwrap();
    let table = StringTable::new();
    let parsed = parse_string(&input, &table);
    let cached = convert::arena_to_cached(&parsed.arena, &parsed.root_children, &table);

    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, tmp.path()).unwrap();

    let table2 = StringTable::new();
    let (arena2, root2) = io::with_archived_file(tmp.path(), |archived| {
        convert::archived_to_arena(archived, &table2).unwrap()
    })
    .unwrap();

    assert_eq!(arena2.leaves.len(), parsed.arena.leaves.len());
    assert_eq!(root2.len(), parsed.root_children.len());

    // Verify some basic leaf content
    // First root child should be a leaf in this file
    if let cwtools_parser::ast::Child::Leaf(idx) = &root2[0] {
        let leaf = &arena2.leaves[*idx as usize];
        let key_str = table2.get_string(leaf.key.normal).unwrap();
        assert!(!key_str.is_empty());
    }
}

/// The batched `archived_to_arena` must produce a StringTable identical to one
/// built by interning each string individually in traversal order: same ids,
/// same resolved text for every node.
#[test]
fn archived_to_arena_matches_per_string_interning() {
    use cwtools_parser::ast::Value;

    let input = r#"
foo = bar
FOO = Bar
empty = ""
nested = {
    a = 1
    b = "hello"
    c = yes
    nope = no
}
key_a key_b = { x = 1 }
"#;
    let table = StringTable::new();
    let parsed = parse_string(input, &table);
    let cached = convert::arena_to_cached(&parsed.arena, &parsed.root_children, &table);

    // Archived path.
    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, tmp.path()).unwrap();
    let arch_table = StringTable::new();
    let (arch_arena, _) = io::with_archived_file(tmp.path(), |archived| {
        convert::archived_to_arena(archived, &arch_table).unwrap()
    })
    .unwrap();

    // Reference path: intern every string by hand, same traversal order as
    // archived_to_arena (leaves, then leaf_values).
    let ref_table = StringTable::new();
    let mut expected = Vec::new();
    for l in &cached.leaves {
        expected.push(ref_table.intern(&l.key));
        push_value(&l.value, &ref_table, &mut expected);
    }
    for lv in &cached.leaf_values {
        push_value(&lv.value, &ref_table, &mut expected);
    }

    // Collect the archived arena's tokens in the identical order and compare.
    let mut actual = Vec::new();
    for l in &arch_arena.leaves {
        actual.push(l.key);
        collect_arena_value(&l.value, &mut actual);
    }
    for lv in &arch_arena.leaf_values {
        collect_arena_value(&lv.value, &mut actual);
    }

    assert_eq!(expected, actual, "archived tokens diverge from per-string");
    for tok in &actual {
        assert_eq!(
            arch_table.get_string(tok.normal),
            ref_table.get_string(tok.normal)
        );
        assert_eq!(
            arch_table.get_string(tok.lower),
            ref_table.get_string(tok.lower)
        );
    }

    fn push_value(
        v: &cwtools_cache::cache_format::CachedValue,
        t: &StringTable,
        out: &mut Vec<cwtools_string_table::string_table::StringTokens>,
    ) {
        use cwtools_cache::cache_format::CachedValue;
        match v {
            CachedValue::String(s) | CachedValue::QString(s) => out.push(t.intern(s)),
            _ => {}
        }
    }
    fn collect_arena_value(
        v: &Value,
        out: &mut Vec<cwtools_string_table::string_table::StringTokens>,
    ) {
        match v {
            Value::String(t) | Value::QString(t) => out.push(*t),
            _ => {}
        }
    }
}

/// The zero-copy archived path must rebuild an arena equivalent to the source
/// parse: token-independent fields (ops, positions, comment text, root children)
/// match the original arena exactly, and every rebuilt token resolves to the same
/// text as a hand-interned reference walked in the identical traversal order.
#[test]
fn archived_to_arena_matches_reference_over_corpus() {
    use cwtools_parser::ast::Value;

    let test_dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testfiles/performancetest2/"
    );

    let config = FileManagerConfig {
        root: std::path::PathBuf::from(test_dir),
        ..Default::default()
    };
    let mut manager = FileManager::new(config);
    let files = manager.discover_and_parse().unwrap();
    assert!(!files.is_empty());

    for parsed in files {
        let cached =
            convert::arena_to_cached(&parsed.arena, &parsed.root_children, &manager.string_table);
        let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
        io::serialize_to_file(&cached, tmp.path()).unwrap();

        let arch_table = StringTable::new();
        let (arch_arena, arch_root) = io::with_archived_file(tmp.path(), |archived| {
            convert::archived_to_arena(archived, &arch_table).unwrap()
        })
        .unwrap();

        let ctx = parsed.path.display();

        // Token-independent structure must match the source parse exactly.
        assert_eq!(parsed.arena.leaves.len(), arch_arena.leaves.len(), "{ctx}");
        for (a, b) in parsed.arena.leaves.iter().zip(arch_arena.leaves.iter()) {
            assert_eq!(a.op, b.op, "{ctx}");
            assert_eq!(a.pos, b.pos, "{ctx}");
            assert_eq!(a.value_pos, b.value_pos, "{ctx}");
        }
        assert_eq!(
            parsed.arena.leaf_values.len(),
            arch_arena.leaf_values.len(),
            "{ctx}"
        );
        for (a, b) in parsed
            .arena
            .leaf_values
            .iter()
            .zip(arch_arena.leaf_values.iter())
        {
            assert_eq!(a.pos, b.pos, "{ctx}");
        }
        assert_eq!(
            parsed.arena.comments.len(),
            arch_arena.comments.len(),
            "{ctx}"
        );
        for (a, b) in parsed.arena.comments.iter().zip(arch_arena.comments.iter()) {
            assert_eq!(a.text, b.text, "{ctx}");
            assert_eq!(a.pos, b.pos, "{ctx}");
        }
        assert_eq!(parsed.root_children, arch_root, "{ctx}");

        // Tokens: hand-intern the cached strings in the traversal order the
        // archived rebuild uses (leaves, then leaf_values) and compare ids and
        // resolved text against the rebuilt arena.
        let ref_table = StringTable::new();
        let mut expected = Vec::new();
        for l in &cached.leaves {
            expected.push(ref_table.intern(&l.key));
            push_value(&l.value, &ref_table, &mut expected);
        }
        for lv in &cached.leaf_values {
            push_value(&lv.value, &ref_table, &mut expected);
        }
        let mut actual = Vec::new();
        for l in &arch_arena.leaves {
            actual.push(l.key);
            collect_arena_value(&l.value, &mut actual);
        }
        for lv in &arch_arena.leaf_values {
            collect_arena_value(&lv.value, &mut actual);
        }
        assert_eq!(
            expected, actual,
            "archived tokens diverge from reference in {ctx}"
        );
        for tok in &actual {
            assert_eq!(
                arch_table.get_string(tok.normal),
                ref_table.get_string(tok.normal),
                "token text diverged in {ctx}"
            );
        }
    }

    fn push_value(
        v: &cwtools_cache::cache_format::CachedValue,
        t: &StringTable,
        out: &mut Vec<cwtools_string_table::string_table::StringTokens>,
    ) {
        use cwtools_cache::cache_format::CachedValue;
        match v {
            CachedValue::String(s) | CachedValue::QString(s) => out.push(t.intern(s)),
            _ => {}
        }
    }
    fn collect_arena_value(
        v: &Value,
        out: &mut Vec<cwtools_string_table::string_table::StringTokens>,
    ) {
        match v {
            Value::String(t) | Value::QString(t) => out.push(*t),
            _ => {}
        }
    }
}

/// Gate: Cache round-trip verified for all 63 test files.
#[test]
fn roundtrip_all_performancetest_files() {
    let test_dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testfiles/performancetest2/"
    );

    let config = FileManagerConfig {
        root: std::path::PathBuf::from(test_dir),
        ..Default::default()
    };
    let mut manager = FileManager::new(config);
    let files = manager.discover_and_parse().unwrap();

    assert!(!files.is_empty(), "No files discovered in {}", test_dir);

    let mut total_files = 0;
    let mut total_leaves = 0;

    for parsed in files {
        let cached =
            convert::arena_to_cached(&parsed.arena, &parsed.root_children, &manager.string_table);

        let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
        io::serialize_to_file(&cached, tmp.path()).unwrap();

        let table2 = StringTable::new();
        let (arena2, root2) = io::with_archived_file(tmp.path(), |archived| {
            convert::archived_to_arena(archived, &table2).unwrap()
        })
        .unwrap();

        // Verify counts match
        assert_eq!(
            arena2.leaves.len(),
            parsed.arena.leaves.len(),
            "Leaf count mismatch for {}",
            parsed.path.display()
        );
        assert_eq!(
            arena2.leaf_values.len(),
            parsed.arena.leaf_values.len(),
            "LeafValue count mismatch for {}",
            parsed.path.display()
        );
        assert_eq!(
            arena2.comments.len(),
            parsed.arena.comments.len(),
            "Comment count mismatch for {}",
            parsed.path.display()
        );
        assert_eq!(
            root2.len(),
            parsed.root_children.len(),
            "Root children count mismatch for {}",
            parsed.path.display()
        );

        total_files += 1;
        total_leaves += parsed.arena.leaves.len();
    }

    println!(
        "Round-trip verified for {} files, {} total leaves",
        total_files, total_leaves
    );
    assert!(
        total_files >= 60,
        "Expected at least 60 files, got {}",
        total_files
    );
}

/// A cache whose child index points past the arena vectors must be rejected at
/// the load boundary. rkyv access still succeeds (the raw u32 is a valid
/// archive), so without the bounds check the structurally-inconsistent arena
/// leaks out and panics when a downstream consumer indexes it (e.g.
/// `Arena::keyed_clause`), crashing the process instead of degrading to a
/// cache-miss re-parse.
#[test]
fn out_of_bounds_child_index_is_rejected() {
    use cwtools_cache::cache_format::{CachedChild, CachedFile};

    // Root references leaf 0, but there are no leaves: index 0 is out of bounds.
    let cached = CachedFile {
        root_children: vec![CachedChild::Leaf(0)],
        leaves: vec![],
        leaf_values: vec![],
        comments: vec![],
    };
    let tmp = tempfile::NamedTempFile::with_suffix(".cwb").unwrap();
    io::serialize_to_file(&cached, tmp.path()).unwrap();

    let table = StringTable::new();
    let result = io::with_archived_file(tmp.path(), |archived| {
        convert::archived_to_arena(archived, &table)
    })
    .expect("header and rkyv access should succeed");

    assert!(
        result.is_err(),
        "out-of-bounds child index must be rejected at load, got Ok"
    );
}

/// The guarantee every cache writer leans on: a write that fails partway leaves
/// the file that was already there untouched, rather than a truncated one the
/// next run has to throw away. Both `.cwb` and `.cwv` route through this.
#[test]
fn a_failed_write_leaves_the_previous_file_intact() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.bin");
    io::write_atomically(&path, |file| file.write_all(b"original")).unwrap();

    let err = io::write_atomically(&path, |file| {
        file.write_all(b"half a cache")?;
        Err(std::io::Error::other("disk full"))
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "disk full");

    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"original",
        "the previous file must survive a failed write"
    );
    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .filter(|name| name != "cache.bin")
        .collect();
    assert!(strays.is_empty(), "temp file left behind: {strays:?}");
}
