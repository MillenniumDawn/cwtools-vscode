//! #1067: a watched workspace-file delete drops only the workspace's own file-index
//! contribution. Each case checks [`FileIndex::release_workspace`] against a full
//! rebuild without the file.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use cwtools_driver::{VanillaFiles, build_file_index_with_vanilla_shadow};
use cwtools_index::FileIndex;

use crate::common::write_tree;

struct Case {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
    parents: &'static [&'static str],
    vanilla: &'static [&'static str],
    ignore_files: &'static [&'static str],
    rel: &'static str,
    survives: bool,
}

const REPLACE_GFX: &str = "name = \"Layer\"\nreplace_path = \"gfx\"\n";

const CASES: &[Case] = &[
    Case {
        name: "workspace only",
        files: &[("mod/gfx/only.dds", ""), ("parent/keep.txt", "")],
        parents: &["parent"],
        vanilla: &[],
        ignore_files: &[],
        rel: "gfx/only.dds",
        survives: false,
    },
    Case {
        name: "parent override in another case",
        files: &[("mod/gfx/Shared.DDS", ""), ("parent/gfx/shared.dds", "")],
        parents: &["parent"],
        vanilla: &[],
        ignore_files: &[],
        rel: "gfx/Shared.DDS",
        survives: true,
    },
    Case {
        name: "base-game override",
        files: &[("mod/gfx/shared.dds", "")],
        parents: &[],
        vanilla: &["gfx/shared.dds"],
        ignore_files: &[],
        rel: "gfx/shared.dds",
        survives: true,
    },
    Case {
        name: "workspace replace_path hides the parent",
        files: &[
            ("mod/descriptor.mod", REPLACE_GFX),
            ("mod/gfx/shared.dds", ""),
            ("parent/gfx/shared.dds", ""),
        ],
        parents: &["parent"],
        vanilla: &[],
        ignore_files: &[],
        rel: "gfx/shared.dds",
        survives: false,
    },
    Case {
        name: "parent replace_path hides the base game",
        files: &[
            ("parent/descriptor.mod", REPLACE_GFX),
            ("mod/gfx/shared.dds", ""),
        ],
        parents: &["parent"],
        vanilla: &["gfx/shared.dds"],
        ignore_files: &[],
        rel: "gfx/shared.dds",
        survives: false,
    },
    Case {
        name: "higher parent replace_path hides the lower parent",
        files: &[
            ("lower/gfx/shared.dds", ""),
            ("higher/descriptor.mod", REPLACE_GFX),
            ("mod/gfx/shared.dds", ""),
        ],
        parents: &["lower", "higher"],
        vanilla: &[],
        ignore_files: &[],
        rel: "gfx/shared.dds",
        survives: false,
    },
    Case {
        name: "ignored in every layer",
        files: &[("mod/gfx/shared.dds", ""), ("parent/gfx/shared.dds", "")],
        parents: &["parent"],
        vanilla: &[],
        ignore_files: &["*.dds"],
        rel: "gfx/shared.dds",
        survives: false,
    },
];

fn build(primary: &Path, parents: &[PathBuf], case: &Case) -> FileIndex {
    let ignore_files: Vec<String> = case.ignore_files.iter().map(|s| s.to_string()).collect();
    build_file_index_with_vanilla_shadow(
        primary,
        parents,
        &ignore_files,
        &[],
        VanillaFiles::Cached(Cow::Owned(
            case.vanilla
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )),
        false,
        Some(&cwtools_driver::vanilla_replacement_shadow(
            primary, parents,
        )),
    )
}

#[test]
fn watched_delete_matches_a_rebuild() {
    for case in CASES {
        let tmp = write_tree(case.files);
        let primary = tmp.path().join("mod");
        let parents: Vec<PathBuf> = case.parents.iter().map(|p| tmp.path().join(p)).collect();
        let mut index = build(&primary, &parents, case);
        index.release_workspace(case.rel);
        std::fs::remove_file(primary.join(case.rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
            .unwrap();
        let rebuilt = build(&primary, &parents, case);
        assert_eq!(index.contains(case.rel), case.survives, "{}", case.name);
        assert_eq!(
            rebuilt.contains(case.rel),
            case.survives,
            "{}: rebuild",
            case.name
        );
    }
}

/// Case-sensitive runs keep the workspace's spelling over the base game's (#1067).
#[test]
fn case_sensitive_build_keeps_the_workspace_spelling() {
    let tmp = write_tree(&[("mod/gfx/Shared.DDS", "")]);
    let index = build_file_index_with_vanilla_shadow(
        &tmp.path().join("mod"),
        &[],
        &[],
        &[],
        VanillaFiles::Cached(Cow::Borrowed(&["gfx/shared.dds".to_string()])),
        true,
        None,
    );
    assert!(index.contains("gfx/Shared.DDS"));
    assert!(!index.contains("gfx/shared.dds"));
}

/// Owned cached spellings must move into the case-sensitive index, as the CLI's
/// one-shot cache load did before the #1154 borrow split. A copied spelling would
/// allocate a fresh buffer, so its heap pointer cannot match the original: the
/// moved original is still alive when a copy would allocate, and each path has a
/// distinct length so no freed block can be reused for another's copy.
#[test]
fn owned_cached_spellings_move_without_copying() {
    let tmp = write_tree(&[]);
    let originals: Vec<String> = [41, 81, 161, 321]
        .into_iter()
        .map(|len| format!("gfx/owned_{}.dds{}", len, "a".repeat(len)))
        .collect();
    let before: std::collections::HashSet<usize> = originals
        .iter()
        .map(|path| path.as_ptr() as usize)
        .collect();
    let index = build_file_index_with_vanilla_shadow(
        &tmp.path().join("mod"),
        &[],
        &[],
        &[],
        VanillaFiles::Cached(Cow::Owned(originals)),
        true,
        None,
    );
    let after: std::collections::HashSet<usize> = index
        .paths_exact()
        .map(|path| path.as_ptr() as usize)
        .collect();
    assert_eq!(
        before, after,
        "owned cached spellings must move into the exact-case index unmoved-copy-free"
    );
    for len in [41usize, 81, 161, 321] {
        let expected = format!("gfx/owned_{len}.dds{}", "a".repeat(len));
        assert!(
            index.contains(&expected),
            "{expected} must resolve from the owned cache"
        );
    }
}
