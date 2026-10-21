# #1094 cache decode preallocation: no-go

## Result

No implementation or performance claim is kept for #1094. This checkout has
no `cargo` or `rustc`, so I could not reproduce the current allocation behavior,
run the required cache tests, or build baseline and candidate benchmark binaries.
The proposed change is therefore unmeasured and is not retained. This does not
satisfy the issue's performance acceptance and is not a merge-ready implementation.

The `warm_load` benchmark input is available at `/workspace/Millennium-Dawn`,
revision `94712ee02b`. Its `common`, `events` and `history` trees contain 6,505
`.txt` files totaling 98,476,490 bytes (93.91 MiB), as selected by the benchmark.

## Attempt and environment

From `/workspace/cwtools-issue-1094` at base
`b705bd6db4b8d2469433a30d7fe4419043b2effc`, I ran:

```sh
cargo bench -p cwtools_cache
```

It exited 127 with `cargo: command not found`. `rustc --version` likewise failed
with `rustc: command not found`. No Rust toolchain was installed in the
available environment. It is Debian GNU/Linux 13, Linux x86_64, kernel
`6.18.55`, with 17 GiB RAM and no swap. Thus there are no baseline/candidate
samples, variance estimates, or measured memory tradeoff to report. No test or
benchmark result is inferred from source inspection.

The candidate preallocation could reserve as much as the 128 MiB decoded-size
cap for a permitted declared frame, while the current growing buffer only
reserves according to decoded output. Since neither real archive sizes nor the
candidate's benefit and peak-memory cost could be measured here, retaining the
eager allocation would be unjustified.

## Recovery

Retry from the exact base above in a Rust-enabled environment: first establish
the baseline `cargo bench -p cwtools_cache` result, then make the preallocation
candidate and run repeated interleaved baseline/candidate `warm_load` samples.
Add and run a regression test proving a declared-over-cap frame is rejected
before allocating output, alongside the cache limits, corrupt and roundtrip
tests. Keep the change only if the results show a repeatable benefit and report
the memory tradeoff.
