//! #1067: a watched workspace-file delete must drop only the workspace's own
//! file-index contribution. The builder records lower-layer fallback spelling, and
//! [`FileIndex::release_workspace`] falls back to it — including the on-disk case a
//! case-sensitive check will resolve against.

use std::path::{Path, PathBuf};

use cwtools_driver::{VanillaFiles, build_file_index_with_vanilla_shadow};
use cwtools_index::FileIndex;

use crate::common::write_tree;

const NO_IGNORES: &[String] = &[];
const NO_VANILLA: &[&str] = &[];
const SHARED: &str = "gfx/shared.dds";
const ONLY: &str = "gfx/only_workspace.dds";
const VANILLA_SHIPMENT: &[&str] = &["gfx/shared.dds", "gfx/base_only.dds"];

/// Build the index with the same shadow the LSP merge passes, and drop the workspace
/// file so the scenario matches a watched delete.
fn built_index(
    workspace: &Path,
    parents: &[PathBuf],
    vanilla_paths: &[&str],
    ignore_files: &[String],
    ignore_dirs: &[String],
    case_sensitive: bool,
) -> FileIndex {
    let shadow = cwtools_driver::vanilla_replacement_shadow(workspace, parents);
    build_file_index_with_vanilla_shadow(
        workspace,
        parents,
        ignore_files,
        ignore_dirs,
        VanillaFiles::Cached(vanilla_paths.iter().map(|s| s.to_string()).collect()),
        case_sensitive,
        Some(&shadow),
    )
}

/// A full rebuild after deleting the workspace file: the reference for what the watched
/// delete must leave behind.
fn rebuilt_contains(
    workspace: &Path,
    workspace_rel: &str,
    parents: &[PathBuf],
    vanilla_paths: &[&str],
    ignore_files: &[String],
    ignore_dirs: &[String],
) -> bool {
    let file = workspace.join(workspace_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    std::fs::remove_file(&file).unwrap();
    built_index(
        workspace,
        parents,
        vanilla_paths,
        ignore_files,
        ignore_dirs,
        false,
    )
    .contains(workspace_rel)
}

/// One workspace file with no lower-layer copy: the delete drops it.
#[test]
fn workspace_only_delete_drops_its_contribution() {
    let tmp = write_tree(&[
        ("mod/gfx/only_workspace.dds", ""),
        ("parent/placeholder.txt", ""),
    ]);
    let primary = tmp.path().join("mod");
    let parents: Vec<PathBuf> = vec![tmp.path().join("parent")];

    let mut index = built_index(
        &primary, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES, false,
    );
    assert!(
        index.contains(ONLY),
        "setup: the workspace file must be indexed"
    );
    assert!(
        index.release_workspace(ONLY),
        "an unshipped path must leave the index with the workspace file"
    );
    assert!(!index.contains(ONLY));
    assert!(
        !rebuilt_contains(&primary, ONLY, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES),
        "a full rebuild must lose the entry too"
    );
}

/// A workspace file shadowing a parent mod's same-path file: the delete keeps the
/// entry, and a case-sensitive run falls back to the parent's on-disk spelling
/// (`Shared.DDS` vs `shared.dds`, #1067).
#[test]
fn parent_override_delete_falls_back_to_the_parent_spelling() {
    let tmp = write_tree(&[("mod/gfx/FoO.DDS", ""), ("parent/gfx/foo.dds", "")]);
    let primary = tmp.path().join("mod");
    let parents: Vec<PathBuf> = vec![tmp.path().join("parent")];

    let mut index = built_index(&primary, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES, true);
    assert!(
        index.contains("gfx/FoO.DDS"),
        "setup: the workspace spelling is exact while present"
    );
    assert!(
        !index.release_workspace("gfx/FoO.DDS"),
        "the parent still provides the path"
    );
    assert!(
        index.contains("gfx/foo.dds"),
        "the kept entry must resolve under the parent's on-disk case"
    );
    assert!(
        !index.contains("gfx/FoO.DDS"),
        "the workspace spelling must not survive its own delete"
    );
    assert_eq!(
        index.on_disk_case("gfx/foo.dds"),
        None,
        "the fallback spelling must be the parent's exact case"
    );
    assert!(
        rebuilt_contains(
            &primary,
            "gfx/FoO.DDS",
            &parents,
            NO_VANILLA,
            NO_IGNORES,
            NO_IGNORES
        ),
        "a full rebuild keeps the parent's contribution"
    );
}

/// A primary `replace_path` hides a lower layer permanently: deleting the workspace
/// file under a replaced directory must drop the entry even though the parent ships it.
#[test]
fn replace_path_keeps_hiding_the_parent_after_a_delete() {
    let tmp = write_tree(&[
        (
            "mod/descriptor.mod",
            "name = \"Primary\"\nreplace_path = \"gfx\"\n",
        ),
        ("mod/gfx/shared.dds", ""),
        ("parent/gfx/shared.dds", ""),
    ]);
    let primary = tmp.path().join("mod");
    let parents: Vec<PathBuf> = vec![tmp.path().join("parent")];

    let mut index = built_index(
        &primary, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES, false,
    );
    assert!(
        index.release_workspace(SHARED),
        "a replaced directory stays shadowed by the primary, delete or not"
    );
    assert!(!index.contains(SHARED));
    assert!(
        !rebuilt_contains(
            &primary, SHARED, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES
        ),
        "a full rebuild must agree with the shadow"
    );
}

/// The indexed base game ships the path: an override delete keeps the entry with the
/// base-game spelling as fallback.
#[test]
fn vanilla_override_delete_falls_back_to_the_base_game() {
    let tmp = write_tree(&[("mod/gfx/shared.dds", "")]);
    let primary = tmp.path().join("mod");

    let mut index = built_index(
        &primary,
        &[],
        VANILLA_SHIPMENT,
        NO_IGNORES,
        NO_IGNORES,
        true,
    );
    assert!(
        !index.release_workspace(SHARED),
        "the base game still provides the path"
    );
    assert!(index.contains(SHARED));
    assert_eq!(
        index.on_disk_case(SHARED),
        None,
        "the fallback spelling must be the base game's exact case"
    );
    assert!(
        rebuilt_contains(
            &primary,
            SHARED,
            &[],
            VANILLA_SHIPMENT,
            NO_IGNORES,
            NO_IGNORES
        ),
        "a full rebuild keeps the cached base-game paths"
    );
}

/// Mixed case against the base game: while the override exists the workspace's exact
/// spelling is the visible one, and the delete hands visibility to the base game's
/// spelling (`Shared.DDS` over cached `shared.dds`, #1067).
#[test]
fn vanilla_add_keeps_the_workspace_spelling_until_the_delete() {
    let tmp = write_tree(&[("mod/gfx/Shared.DDS", "")]);
    let primary = tmp.path().join("mod");

    let mut index = built_index(
        &primary,
        &[],
        VANILLA_SHIPMENT,
        NO_IGNORES,
        NO_IGNORES,
        true,
    );
    assert!(
        index.contains("gfx/Shared.DDS"),
        "the override's exact spelling is visible while it exists"
    );
    assert!(
        !index.contains(SHARED),
        "the base-game spelling must not replace the override's"
    );
    assert!(
        !index.release_workspace(SHARED),
        "the base game still provides the path"
    );
    assert!(
        index.contains(SHARED),
        "the delete must hand visibility to the base game's spelling"
    );
    assert!(
        !index.contains("gfx/Shared.DDS"),
        "the deleted override's spelling must not survive"
    );
}

/// A parent's `replace_path` shadows the base game: deleting the workspace override in
/// a replaced directory must drop the entry even though the base game ships it.
#[test]
fn parent_replace_path_hides_the_base_game_after_a_delete() {
    let tmp = write_tree(&[
        (
            "parent/descriptor.mod",
            "name = \"Parent\"\nreplace_path = \"gfx\"\n",
        ),
        ("mod/gfx/shared.dds", ""),
    ]);
    let primary = tmp.path().join("mod");
    let parents: Vec<PathBuf> = vec![tmp.path().join("parent")];

    let mut index = built_index(
        &primary,
        &parents,
        VANILLA_SHIPMENT,
        NO_IGNORES,
        NO_IGNORES,
        false,
    );
    assert!(
        index.release_workspace(SHARED),
        "the parent's replacement shadows the base-game path"
    );
    assert!(!index.contains(SHARED));
    assert!(
        !rebuilt_contains(
            &primary,
            SHARED,
            &parents,
            VANILLA_SHIPMENT,
            NO_IGNORES,
            NO_IGNORES
        ),
        "a full rebuild must agree with the parent-layer shadow"
    );
}

/// Workspace ignore patterns hide a parent file from discovery entirely, so it was
/// never a fallback provider and never an index entry either.
#[test]
fn ignored_parent_file_does_not_keep_the_entry() {
    let tmp = write_tree(&[("mod/gfx/shared.dds", ""), ("parent/gfx/shared.dds", "")]);
    let primary = tmp.path().join("mod");
    let parents: Vec<PathBuf> = vec![tmp.path().join("parent")];
    let ignore_files: Vec<String> = vec!["*.dds".to_string()];

    let index = built_index(
        &primary,
        &parents,
        NO_VANILLA,
        &ignore_files,
        NO_IGNORES,
        false,
    );
    assert!(
        !index.contains(SHARED),
        "an ignore-patterned path is never indexed, whichever layer ships it"
    );
    assert!(
        !rebuilt_contains(
            &primary,
            SHARED,
            &parents,
            NO_VANILLA,
            &ignore_files,
            NO_IGNORES
        ),
        "deleting the ignored workspace file cannot resurrect it either"
    );
}

/// Two parents ship the path: the highest-priority provider's spelling wins the
/// fallback, and the entry stays when the workspace override goes.
#[test]
fn highest_priority_providing_parent_wins_the_fallback() {
    let tmp = write_tree(&[
        ("lower/gfx/shared.dds", ""),
        ("lower/other/keep.txt", ""),
        ("higher/gfx/Shared.DDS", ""),
        ("mod/gfx/shared.dds", ""),
    ]);
    let primary = tmp.path().join("mod");
    let parents = vec![tmp.path().join("lower"), tmp.path().join("higher")];

    let mut index = built_index(&primary, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES, true);
    assert!(
        !index.release_workspace(SHARED),
        "some parent still provides the path"
    );
    assert!(
        index.contains("gfx/Shared.DDS"),
        "the fallback spelling must be the highest-priority parent's"
    );
    assert!(
        rebuilt_contains(
            &primary, SHARED, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES
        ),
        "a full rebuild keeps the surviving parent's contribution"
    );
}

/// A higher-priority parent's `replace_path` covers the directory the lower parent's
/// file lives in: no provider remains, so the delete must drop the entry.
#[test]
fn parent_replace_path_shadowing_the_last_provider() {
    let tmp = write_tree(&[
        ("lower/gfx/shared.dds", ""),
        (
            "higher/descriptor.mod",
            "name = \"Higher\"\nreplace_path = \"gfx\"\n",
        ),
        ("mod/gfx/shared.dds", ""),
        ("mod/other/keep.txt", ""),
    ]);
    let primary = tmp.path().join("mod");
    let parents = vec![tmp.path().join("lower"), tmp.path().join("higher")];

    let mut index = built_index(
        &primary, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES, false,
    );
    assert!(
        index.release_workspace(SHARED),
        "the higher parent's replacement shadows the lower parent's file"
    );
    assert!(!index.contains(SHARED));
    assert!(
        !rebuilt_contains(
            &primary, SHARED, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES
        ),
        "a full rebuild must agree with the inter-parent shadow"
    );
}

/// Re-creating the file re-claims the workspace spelling over the same fallback, and
/// another delete falls back again (#1067).
#[test]
fn repeated_claim_and_release_idempotent() {
    let tmp = write_tree(&[("mod/gfx/shared.dds", ""), ("parent/gfx/shared.DDS", "")]);
    let primary = tmp.path().join("mod");
    let parents: Vec<PathBuf> = vec![tmp.path().join("parent")];

    let mut index = built_index(&primary, &parents, NO_VANILLA, NO_IGNORES, NO_IGNORES, true);
    assert!(!index.release_workspace(SHARED), "first delete keeps it");
    assert!(
        index.contains("gfx/shared.DDS"),
        "falls back to the parent's case"
    );
    // Re-creating with a different case than the visible fallback is an effective
    // case change, so the claim must report one.
    assert!(index.claim_workspace(SHARED));
    assert!(
        index.contains(SHARED),
        "the re-created file restores its own exact case"
    );
    assert!(
        !index.release_workspace(SHARED),
        "a second delete falls back again"
    );
    assert!(
        index.contains("gfx/shared.DDS"),
        "the lower layer's case is restored again"
    );
}
