# Issue #1094 cache decode preallocation measurement

## Result

Sizing the decode buffer from a frame's declared content size made no measurable difference. On declared-size frames the candidate's median per-run Criterion slope was 355.36 ms against 356.11 ms for the baseline (-0.21%), and three of the four paired 95% intervals overlap. The undeclared-size control runs the same code in both builds and still moved +0.86%, so run-to-run noise is larger than the effect.

The change is **not kept**. It would also need a writer change: `io::serialize_to_file` streams frames without `set_pledged_src_size`, so no `.cwb` declares a size today and the current decoder never takes the new branch. The declared-size twins below stand in for a writer that pledges its size. A valid declared frame could also reserve up to the 128 MiB decode cap at once, where the growing buffer only reserves what it decodes. The vanilla `.cwv` decoder has the same pattern; #1094 made that follow-up conditional on a gain here, so it is not planned either.

## Setup

- Base: `b705bd6d` (`main`).
- Candidate: a patch to `read_archive_bytes` in `engine/crates/cache/src/io.rs` that reserved `AlignedVec` capacity equal to the declared size once it passed the `MAX_ARCHIVE_DECODED_BYTES` check. It was never committed.
- Harness: an `archive_decode` group in `engine/crates/cache/benches/cache.rs`, at commit `a90da9b4` in PR #1103's history. It was not merged, because on `main` both of its workloads run the same decoder.
- Input: 6,505 `.txt` files under `common`, `events` and `history` in Millennium Dawn `94712ee0`, 93.9 MiB of source. Setup stored each as a native `.cwb` with no declared size, then recompressed the same decoded bytes with `set_pledged_src_size` to make a declared-size twin. Decoded bodies total 323.5 MB; the native archives total 71.0 MB and the twins 71.3 MB. Setup checked every frame's content-size flag and validated every archive through `io::with_archived_file` before timing.
- Environment: Debian 13 VM, kernel 6.18.55, AMD EPYC 9V45 with 5 vCPUs, 17 GiB RAM, Rust 1.99.1. Load average was not recorded.

Each run was one process from a saved binary, interleaved B/C/C/B then C/B/B/C, with 10 Criterion samples per workload, a 2 s warm-up and a 12 s measurement target. Decode is single-threaded and the page cache was warm.

## Measurements

Criterion regression slope estimates in milliseconds, with 95% intervals.

| Run | Variant | Undeclared frames | Declared frames |
| --- | --- | ---: | ---: |
| B1 | Baseline | 350.98 [348.82–352.37] | 355.21 [354.36–356.34] |
| C1 | Candidate | 354.58 [351.61–356.78] | 356.59 [355.23–358.23] |
| C2 | Candidate | 354.56 [350.10–358.97] | 354.43 [352.78–357.12] |
| B2 | Baseline | 355.04 [351.72–357.96] | 369.89 [365.52–374.18] |
| C3 | Candidate | 354.40 [351.58–359.34] | 354.67 [354.09–355.44] |
| B3 | Baseline | 350.85 [349.55–352.20] | 356.43 [354.64–358.70] |
| B4 | Baseline | 351.91 [350.93–352.85] | 355.78 [354.58–357.36] |
| C4 | Candidate | 352.37 [350.48–355.20] | 356.04 [354.82–357.24] |

Medians of the four runs: undeclared 351.45 ms baseline and 354.48 ms candidate (+0.86%); declared 356.11 ms and 355.36 ms (-0.21%). B2's declared run is the outlier. Without it the baseline spans 355.21–356.43 ms, which sits inside the candidate's 354.43–356.59 ms.

An earlier pair of runs on the full `parse_cache/warm_load` bench, where neither build saw a declared frame, put the noise between identical decoders at 0.55% (medians 852.70 ms and 848.05 ms over four runs each). The decode measured above is about 350 ms of that 850 ms load, which bounds what the change could save end to end.

Peak RSS, sampled from `VmHWM` every 50 ms, overlapped between the variants. It covers the whole process, setup included, so it says nothing about the declared-frame reservation.

No Windows or macOS runs were made.
