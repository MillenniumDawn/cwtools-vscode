# Issue #868: full-index snapshot/COW measurement prerequisite

This manual benchmark extends `engine/crates/driver/benches/snapshot_clone.rs` to load a real HOI4 `Session` with both the Millennium Dawn workspace and an explicitly supplied base-game install. The session builds the layered index with the existing driver path and merge semantics. The benchmark compares Criterion's Arc snapshot, first-write COW, and deep-clone cases on that full index; it also reports process RSS observations before writing, after an unshared write, with a retained snapshot, and after the snapshot-retaining first write.

This is only the full-index COW memory/cost prerequisite. It does **not** measure real LSP Backend hover latency, queued InfoService writer contention, or diagnostics equality, and is not a complete #868 result. No speed or latency claim is made.

## Inputs and invocation

Use the pinned HOI4 rules checkout, Millennium Dawn checkout, and a local HOI4 installation containing `common/events/localisation`. `CWTOOLS_VANILLA` is an explicit path; there is no guessed Steam path or fallback. For example:

```text
CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config \
CWTOOLS_CORPUS=/path/to/Millennium-Dawn \
CWTOOLS_VANILLA=/path/to/Hearts-of-Iron-IV \
cargo bench --manifest-path engine/Cargo.toml -p cwtools_driver --bench snapshot_clone -- full_hoi4_md
```

The criterion filter selects the `full_hoi4_md` group. Without `CWTOOLS_VANILLA`, the full tier prints a skip reason; the existing synthetic and mod-only cases remain available. The full-tier Criterion sample count is bounded to 10. Criterion retains its sample data and reports under `target/criterion`.

The full-tier setup prints the three paths and best-effort Git revisions (the game install may not be a Git checkout), primary MD parsed-file count, index type/instance/file-path/value counts, and sample count. It reports `/proc/self/status` VmRSS and VmHWM where available. RSS is whole-process resident memory, VmHWM is a process-lifetime high-water mark, and neither isolates TypeIndex allocations; `/proc` readings are Linux-only. Criterion timings are not raw per-operation logs in stdout.

## Current implementation evidence

- `cargo check --manifest-path engine/Cargo.toml -p cwtools_driver --bench snapshot_clone` passed after the changes.
- No timing samples have been run. Wait for an exclusive measurement slot before invoking the command above.
- Remaining #868 work: measure real LSP hover/writer contention and verify diagnostics equality; this benchmark does not answer those questions.
