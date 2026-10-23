# Issue #868: full-index snapshot/COW measurement prerequisite

This manual benchmark extends `engine/crates/driver/benches/snapshot_clone.rs` to load a real HOI4 `Session` with both the Millennium Dawn workspace and an explicitly supplied base-game install. The session builds the layered index with the existing driver path and merge semantics. The benchmark compares Criterion's Arc snapshot, first-write COW, and deep-clone cases on that full index; it also reports process RSS observations before writing, after an unshared write, with a retained snapshot, and after the snapshot-retaining first write.

This is only the full-index COW memory/cost prerequisite. It does **not** measure real LSP Backend hover latency, queued InfoService writer contention, or diagnostics equality, and is not a complete #868 result. No speed or latency claim is made.

## Inputs and invocation

Use the pinned HOI4 rules checkout, Millennium Dawn checkout, and a local HOI4 installation containing `common`, `events`, and `localisation`. `CWTOOLS_VANILLA` is an explicit path; there is no guessed Steam path or fallback. For example:

```text
CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config \
CWTOOLS_CORPUS=/path/to/Millennium-Dawn \
CWTOOLS_VANILLA=/path/to/Hearts-of-Iron-IV \
cargo bench --manifest-path engine/Cargo.toml -p cwtools_driver --bench snapshot_clone -- full_hoi4_md
```

The criterion filter selects the `full_hoi4_md` group. Without `CWTOOLS_VANILLA`, the full tier prints a skip reason; the existing synthetic and mod-only cases remain available. The full-tier Criterion sample count is bounded to 10. Criterion retains its sample data and reports under `target/criterion`.

The full tier fails closed unless the supplied game root contains `common`, `events`, and `localisation`, the layered Session index is nonempty, and that index contains indexed instances whose source paths are under the vanilla root. The last check uses the existing TypeIndex map entries (source file identities); it reports the count of vanilla-root indexed files and instances rather than treating MD-only index contents as proof of a game index. It also prints the three paths, best-effort Git revisions (a game install may not be a Git checkout), primary MD parsed-file count, and total index shape.

Synthetic and mod-only cases retain the original Criterion sample size of 30. Only the full-index Criterion group uses 10 samples. Its COW benchmark uses `BatchSize::PerIteration`, bounding each measured batch to one full-index COW result instead of retaining multiple giant copies. Criterion sample data/reports are kept under `target/criterion`; they are not printed as raw per-operation lines. The setup prints `/proc/self/status` VmRSS and VmHWM before/after the unshared and retained-snapshot mutable-access cases where available. These are whole-process observations: VmHWM is a process-lifetime high-water mark, and RSS differences are allocator-history/order dependent, not isolated TypeIndex allocations or a production peak-memory measurement. `/proc` readings are Linux-only.

## Current implementation evidence

- `cargo check --manifest-path engine/Cargo.toml -p cwtools_driver --bench snapshot_clone` passed after the review fixes.
- `cargo clippy --manifest-path engine/Cargo.toml -p cwtools_driver --bench snapshot_clone -- -D warnings` passed.
- One exclusive Linux run used MD `587750f5f7`, rules `40a563c`, and the local HOI4 installation, with one-second warmup and measurement windows and 10 Criterion samples per case. The index had 357,866 instances, including 83,423 vanilla instances from 1,523 vanilla files.
- Arc snapshot estimate: 11.151 ns. First-write COW: 33.332 ms (interval 32.650–34.081 ms). Full deep clone, including its normal drop: 54.409 ms (53.887–54.979 ms). These are different operations, not before/after optimization results.
- Process RSS was 2,100,768 KiB before retaining the snapshot and 2,140,204 KiB after the first COW write. The process high-water mark remained 2,202,004 KiB. This single, allocator-history-dependent observation does not establish an isolated allocation delta or an acceptable production memory budget.
- Remaining #868 work: measure real LSP hover/writer contention and verify diagnostics equality; this benchmark does not answer those questions.
