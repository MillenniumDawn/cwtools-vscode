//! Cost of the editor's vanilla auxiliary payload consumption during a merge (#1154).
//!
//! `merge_pending_vanilla_index` used to deep-clone the retained
//! file_paths, var_names, scripted_loc_names and scripted_gui_names on every
//! invocation before consuming them; it now reads the retained payload through
//! borrows (`Arc<Vec<String>>` snapshots, `set_vanilla_names(&[String])`,
//! `VanillaFiles::Cached(&[String])`). The payload cannot be rebuilt per merge
//! — a workspace scan stages it once — so the whole-payload clone was overhead.
//!
//! Two measurement layers:
//!
//! `vanilla_payload_merge` (criterion) times the old clone-then-consume sequence
//! against the new borrow-and-consume sequence on identical payloads in one
//! process. The baseline is clone-plus-current-consume, not the literal
//! pre-#1154 binary: the consumer setters now borrow, so the old side is the
//! old snapshot cost plus the same consume the new code pays. Its file-index
//! arm goes through the actual owned `Cached(Cow::Owned)` driver route, which
//! moves the owned paths as the CLI's cache load always did.
//!
//! `paired_interleaved` is a bounded balanced paired run: each pair times one
//! old-side and one new-side call back to back on the identical payload, with
//! the starting side alternating every pair so ordering effects cancel. It
//! prints raw per-pair CSV (also retained under `target/criterion/`) and a
//! paired difference with a 95% CI, which is what a real before/after claim
//! rests on; criterion's separate blocks only show per-variant distributions.

use std::borrow::Cow;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use criterion::{BenchmarkId, Criterion};
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
/// Paired sample budget per variant: enough pairs for the small variants to
/// resolve sub-percent differences, bounded for the HOI4-scale ones.
const SMALL_PAIRS: usize = 20_000;
const LARGE_PAIRS: usize = 400;

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
/// when a new spelling is inserted.
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
        let mut owner = FileIndex::new();
        owner.set_case_sensitive(case_sensitive);
        let mut group = c.benchmark_group("vanilla_payload_add_paths");
        group.bench_with_input(
            BenchmarkId::new("owned_items", label),
            &paths,
            |b, paths| {
                b.iter(|| {
                    owner.add_paths(black_box(paths).iter().map(String::clone));
                    black_box(owner.paths().count())
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("borrowed_items", label),
            &paths,
            |b, paths| {
                b.iter(|| {
                    owner.add_paths(black_box(paths).iter().map(String::as_str));
                    black_box(owner.paths().count())
                })
            },
        );
        group.finish();
    }
}

/// Median of a sample; even-sized samples average the two middle values.
fn median(samples: &[u128]) -> f64 {
    let mut sorted: Vec<f64> = samples.iter().map(|s| *s as f64).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite durations"));
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

/// One bounded balanced paired run of old-versus-new on one variant.
///
/// Each pair times one old-side and one new-side consume back to back on the
/// identical payload and reusable indexes, with the side measured first
/// alternating every pair so warm-up or ordering bias cancels. Raw pairs are
/// printed and retained under `target/criterion/`; the summary reports both
/// medians and the paired difference with a 95% CI.
fn paired_interleaved(
    label: &str,
    payload: &VanillaPayload,
    case_sensitive: bool,
    workspace: &Path,
    pairs: usize,
) {
    let mut old_index = TypeIndex::new();
    let mut new_index = TypeIndex::new();
    // Warm both sides equally before pairing so neither variant carries
    // first-touch costs into the measured pairs.
    for _ in 0..50 {
        consume_old_clone(&mut old_index, payload, case_sensitive, workspace);
        consume_new_borrow(&mut new_index, payload, case_sensitive, workspace);
    }
    let mut old_ns = Vec::with_capacity(pairs);
    let mut new_ns = Vec::with_capacity(pairs);
    let mut csv = String::from("pair,old_ns,new_ns,old_measured_first\n");
    for pair in 0..pairs {
        let old_first = pair % 2 == 0;
        let mut slot_ns = [0u128; 2];
        for (slot, measure_old) in [(0usize, old_first), (1usize, !old_first)] {
            let start = Instant::now();
            if measure_old {
                consume_old_clone(&mut old_index, payload, case_sensitive, workspace);
            } else {
                consume_new_borrow(&mut new_index, payload, case_sensitive, workspace);
            }
            slot_ns[slot] = start.elapsed().as_nanos();
        }
        let (old, new) = if old_first {
            (slot_ns[0], slot_ns[1])
        } else {
            (slot_ns[1], slot_ns[0])
        };
        old_ns.push(old);
        new_ns.push(new);
        csv.push_str(&format!("{pair},{old},{new},{old_first}\n"));
    }

    let old_median = median(&old_ns);
    let new_median = median(&new_ns);
    let diffs: Vec<f64> = old_ns
        .iter()
        .zip(&new_ns)
        .map(|(o, n)| *o as f64 - *n as f64)
        .collect();
    let mean_diff: f64 = diffs.iter().sum::<f64>() / diffs.len() as f64;
    let variance = diffs
        .iter()
        .map(|d| (d - mean_diff) * (d - mean_diff))
        .sum::<f64>()
        / (diffs.len() - 1) as f64;
    let sd = variance.sqrt();
    let ci95 = 1.959_964 * sd / (diffs.len() as f64).sqrt();
    let new_faster = diffs.iter().filter(|d| **d > 0.0).count();

    println!(
        "paired {label}: pairs={pairs} old_median={old_median:.0}ns new_median={new_median:.0}ns \
         diff_mean={mean_diff:.0}ns diff_sd={sd:.0}ns diff_ci95=±{ci95:.0}ns \
         new_faster_pairs={}/{}",
        new_faster,
        diffs.len()
    );
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root above the crate")
        .to_path_buf();
    path.push("target/criterion");
    path.push(format!(
        "vanilla_payload_pairs_{}.csv",
        label.replace('/', "_")
    ));
    match std::fs::write(&path, csv) {
        Ok(()) => println!("paired {label}: raw pairs retained at {}", path.display()),
        Err(e) => println!("paired {label}: raw pairs not retained ({e}); printed summary only"),
    }
}

/// The bounded balanced paired portion of the bench: every variant gets an
/// alternating-order old/new measurement with raw output retained.
fn paired_interleaved_measurements() {
    let workspace = workspace_root();
    for (name, (file_paths, var_names, scripted), pairs) in [
        ("small", SMALL, SMALL_PAIRS),
        ("hoi4_scale", LARGE, LARGE_PAIRS),
    ] {
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
            paired_interleaved(&label, &payload, case_sensitive, workspace.path(), pairs);
        }
    }
}

fn main() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_merge(&mut criterion);
    bench_add_paths(&mut criterion);
    criterion.final_summary();
    paired_interleaved_measurements();
}
