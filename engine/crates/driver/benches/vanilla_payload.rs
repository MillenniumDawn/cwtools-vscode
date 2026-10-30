//! Cost of the editor's vanilla auxiliary payload consumption during a merge (#1154).
//!
//! `merge_pending_vanilla_index` used to deep-clone the retained
//! file_paths, var_names, scripted_loc_names and scripted_gui_names on every
//! invocation before consuming them. It now reads the retained payload through
//! borrows (`Arc<Vec<String>>` snapshots, `set_vanilla_names(&[String])`,
//! `VanillaFiles::Cached(Cow<'a, [String]>)`). The payload cannot be rebuilt
//! per merge, since a workspace scan stages it once, so the whole-payload clone
//! was overhead.
//!
//! `vanilla_payload_merge` times the old clone-then-consume sequence against
//! the new borrow-and-consume sequence on identical payloads in one process.
//! The old side is a reconstruction, not the literal pre-#1154 binary: it is
//! the old snapshot clone plus the same consume the new code pays, because the
//! consumer setters now borrow. Its file-index arm goes through the owned
//! `Cached(Cow::Owned)` driver route, which moves the owned paths as the CLI's
//! cache load always did.
//!
//! `vanilla_payload_add_paths` isolates the `FileIndex::add_paths` input
//! change on a fresh index per iteration.

use std::borrow::Cow;
use std::hint::black_box;
use std::path::Path;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use cwtools_driver::{VanillaFiles, build_file_index_with_vanilla_shadow};
use cwtools_index::{FileIndex, TypeIndex};
use tempfile::TempDir;

/// Small payload: a low-instance mod's worth of base-game data.
const SMALL: (usize, usize, usize) = (500, 200, 50);
/// Large payload: the order of magnitude of a full HOI4 vanilla scan's aux.
const LARGE: (usize, usize, usize) = (50_000, 20_000, 1_000);
/// Files in the workspace the rebuild walks; kept tiny so the walk is a fixed
/// cost both variants pay.
const WORKSPACE_FILES: usize = 32;

struct VanillaPayload {
    file_paths: Vec<String>,
    var_names: Vec<String>,
    scripted_loc_names: Vec<String>,
    scripted_gui_names: Vec<String>,
}

impl VanillaPayload {
    fn new(file_paths: usize, var_names: usize, scripted_names: usize) -> Self {
        Self {
            file_paths: (0..file_paths)
                .map(|i| format!("gfx/models/unit_{i:06}.dds"))
                .collect(),
            var_names: (0..var_names)
                .map(|i| {
                    if i % 8 == 0 {
                        format!("vanilla_var_{i:06}@GER")
                    } else {
                        format!("vanilla_var_{i:06}")
                    }
                })
                .collect(),
            scripted_loc_names: (0..scripted_names)
                .map(|i| format!("vanilla_loc_{i:05}"))
                .collect(),
            scripted_gui_names: (0..scripted_names)
                .map(|i| format!("vanilla_gui_{i:05}"))
                .collect(),
        }
    }
}

fn workspace_root() -> TempDir {
    let tmp = tempfile::tempdir().expect("workspace tempdir");
    for i in 0..WORKSPACE_FILES {
        let path = tmp.path().join(format!("common/things/{i}.txt"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    tmp
}

/// The old payload handling: snapshot the retained vectors wholesale, then
/// consume them. The snapshot vectors are dropped as soon as each setter is
/// done with them, exactly as the pre-#1154 merge's per-merge clones went out
/// of scope, so the baseline does not carry them live through the file-index
/// rebuild. The consume side is the actual owned `Cached(Cow::Owned)` driver
/// route, which moves the owned paths as the CLI's cache load always did; the
/// setters being borrowed makes this clone-plus-current-consume rather than
/// the literal pre-#1154 binary.
fn consume_old_clone(
    idx: &mut TypeIndex,
    payload: &VanillaPayload,
    case_sensitive: bool,
    workspace: &Path,
) {
    let var_names = payload.var_names.clone();
    idx.var_index.set_vanilla_names(&var_names);
    drop(var_names);
    let scripted_loc_names = payload.scripted_loc_names.clone();
    idx.scripted_loc_index
        .set_vanilla_names(&scripted_loc_names);
    drop(scripted_loc_names);
    let scripted_gui_names = payload.scripted_gui_names.clone();
    idx.scripted_gui_index
        .set_vanilla_names(&scripted_gui_names);
    drop(scripted_gui_names);
    let file_paths = payload.file_paths.clone();
    let file_index = build_file_index_with_vanilla_shadow(
        workspace,
        &[],
        &[],
        &[],
        VanillaFiles::Cached(Cow::Owned(file_paths)),
        case_sensitive,
        None,
    );
    black_box(file_index.len());
}

/// The #1154 call: consume the retained payload through borrows, including the
/// retained borrowed `Cached(Cow::Borrowed)` route.
fn consume_new_borrow(
    idx: &mut TypeIndex,
    payload: &VanillaPayload,
    case_sensitive: bool,
    workspace: &Path,
) {
    idx.var_index.set_vanilla_names(&payload.var_names);
    idx.scripted_loc_index
        .set_vanilla_names(&payload.scripted_loc_names);
    idx.scripted_gui_index
        .set_vanilla_names(&payload.scripted_gui_names);
    let file_index = build_file_index_with_vanilla_shadow(
        workspace,
        &[],
        &[],
        &[],
        VanillaFiles::Cached(Cow::Borrowed(&payload.file_paths)),
        case_sensitive,
        None,
    );
    black_box(file_index.len());
}

fn bench_merge(c: &mut Criterion) {
    let workspace = workspace_root();
    for (name, (file_paths, var_names, scripted)) in [("small", SMALL), ("hoi4_scale", LARGE)] {
        let payload = VanillaPayload::new(file_paths, var_names, scripted);
        for case_sensitive in [false, true] {
            let label = format!(
                "{name}/{}",
                if case_sensitive {
                    "case_sensitive"
                } else {
                    "case_insensitive"
                }
            );
            let mut group = c.benchmark_group("vanilla_payload_merge");
            let mut old_index = TypeIndex::new();
            group.bench_with_input(
                BenchmarkId::new("old_clone_plus_consume", &label),
                &payload,
                |b, payload| {
                    b.iter(|| {
                        consume_old_clone(
                            black_box(&mut old_index),
                            black_box(payload),
                            case_sensitive,
                            workspace.path(),
                        )
                    })
                },
            );
            let mut new_index = TypeIndex::new();
            group.bench_with_input(
                BenchmarkId::new("new_borrow_consume", &label),
                &payload,
                |b, payload| {
                    b.iter(|| {
                        consume_new_borrow(
                            black_box(&mut new_index),
                            black_box(payload),
                            case_sensitive,
                            workspace.path(),
                        )
                    })
                },
            );
            group.finish();
        }
    }
}

/// Isolates the `FileIndex::add_paths` input change: owned items keep the
/// move into the case-sensitive exact-spelling map, borrowed items copy only
/// when a new spelling is inserted. Every iteration starts from an empty
/// index, so each timed call inserts every path.
fn bench_add_paths(c: &mut Criterion) {
    let paths: Vec<String> = (0..LARGE.0)
        .map(|i| format!("gfx/models/unit_{i:06}.dds"))
        .collect();
    for case_sensitive in [false, true] {
        let label = if case_sensitive {
            "case_sensitive"
        } else {
            "case_insensitive"
        };
        let fresh_index = || {
            let mut index = FileIndex::new();
            index.set_case_sensitive(case_sensitive);
            index
        };
        let mut group = c.benchmark_group("vanilla_payload_add_paths");
        group.bench_with_input(
            BenchmarkId::new("owned_items", label),
            &paths,
            |b, paths| {
                b.iter_batched(
                    || (fresh_index(), black_box(paths).clone()),
                    |(mut index, owned)| {
                        index.add_paths(owned);
                        index
                    },
                    BatchSize::LargeInput,
                )
            },
        );
        group.bench_with_input(
            BenchmarkId::new("borrowed_items", label),
            &paths,
            |b, paths| {
                b.iter_batched(
                    fresh_index,
                    |mut index| {
                        index.add_paths(black_box(paths).iter().map(String::as_str));
                        index
                    },
                    BatchSize::LargeInput,
                )
            },
        );
        group.finish();
    }
}

criterion_group!(benches, bench_merge, bench_add_paths);
criterion_main!(benches);
