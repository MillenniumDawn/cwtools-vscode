# Diagnostic baseline storage measurement

The guard baselines remain plain CSV. Gzip makes the Millennium Dawn file much
smaller in a checkout, but Git already compresses text blobs; in the measured
object database the additional saving was small, the diff became opaque, and
reading the report became slower and used more peak memory.

## Recorded results

The measurement used the unchanged baselines at `bffea3567c534d21c995fa2adb957aea78efef55`
and the deterministic gzip candidate from local experiment commit
`d8e1fdb1cf50c4bbc2abbf3fb6a3de9764a1c7ea`. It contains the same CSV bytes,
including the same header and all **19,591 diagnostic rows**.

Measurements ran on Linux x86_64 with Python 3.12.14, pytest 9.1.1, Git 2.52.0,
and zlib 1.3.2 (build and runtime). Each file was read in the same process and
each gzip file was written to a local temporary file. The paired samples vary
substantially by format, so the raw repetitions and ranges are included below;
these microbenchmarks describe this guard's file I/O only and do not estimate
whole-application performance.

| Baseline | Plain checkout | Gzip checkout | Plain Git object | Gzip Git object |
| --- | ---: | ---: | ---: | ---: |
| Millennium Dawn | 4,092,688 B | 168,231 B | 187,321 B | 167,075 B |
| Vanilla | 832 B | 447 B | 437 B | 467 B |

The MD file shrank **95.9%** in the checkout but only **10.8%** in Git's stored
blob size (20,246 B). The vanilla blob grew by 30 B. `git cat-file
--batch-check='%(objectname) %(objecttype) %(objectsize) %(objectsize:disk)'`
reported the object sizes above for these object IDs:

```text
1950b612b1ebb56a729054479753d5cbad33be4b  blob 4092688 187321  # plain MD
2f24c4188e13fcb7e9f806a76a71db0b68d28db4  blob 168231  167075  # gzip MD
ede70a423513c808b5b7c1dcab0194ea58ed758a  blob 832     437     # plain vanilla
e1a7678bc07fea8cb1c4433bb3680be3198d560f  blob 447     467     # gzip vanilla
```

The compressed diff replaced **19,595 readable MD lines** with one binary
blob. Reviewers could no longer inspect a changed diagnostic row in the diff.

An interleaved read test ran 20 repetitions per format, alternating which
format ran first. Each repetition read the complete file and checked its bytes
against the original CSV. Median read time was **1.386 ms** for plain CSV and
**7.405 ms** for gzip; ranges were **0.803–2.803 ms** and **5.050–10.711 ms**.
Peak traced memory was **4,097,567 B** for CSV and **8,207,685 B** for gzip.
The compression experiment ran 10 deterministic gzip encodes: median **51.084
ms**, range **49.877–52.045 ms**, and **628,755 B** peak traced memory.

Raw interleaved read times, in milliseconds, in execution order; the first
format alternated between CSV and gzip:

```text
1  2.803, 10.711
2  10.344, 2.228
3  1.067, 7.402
4  5.050, 0.975
5  1.910, 8.665
6  6.039, 1.037
7  2.278, 7.407
8  5.914, 0.803
9  1.707, 7.836
10 6.552, 0.910
11 2.411, 8.516
12 6.439, 0.917
13 1.749, 8.763
14 6.282, 0.808
15 1.789, 8.062
16 5.585, 0.860
17 1.704, 7.793
18 6.530, 0.947
19 1.722, 7.683
20 6.590, 0.894
```

## Reproduction

From a checkout at the plain-CSV baseline, regenerate the exact gzip candidate
bytes and measure both blobs in Git's object database:

```sh
baseline_tmp=$(mktemp -d)
python - "$baseline_tmp" <<'PY'
from pathlib import Path
import gzip
import sys

output = Path(sys.argv[1])
for name in ("md-baseline.csv", "vanilla-baseline.csv"):
    raw = Path("scripts", name).read_bytes()
    with (output / f"{name}.gz").open("wb") as target:
        with gzip.GzipFile(
            filename="", fileobj=target, mode="wb", compresslevel=9, mtime=0
        ) as compressed:
            compressed.write(raw)
PY
for baseline_file in \
  scripts/md-baseline.csv \
  "$baseline_tmp/md-baseline.csv.gz" \
  scripts/vanilla-baseline.csv \
  "$baseline_tmp/vanilla-baseline.csv.gz"
do
  git hash-object -w "$baseline_file"
done | git cat-file --batch-check='%(objectname) %(objecttype) %(objectsize) %(objectsize:disk)'
gzip -cd "$baseline_tmp/md-baseline.csv.gz" | cmp - scripts/md-baseline.csv
gzip -cd "$baseline_tmp/vanilla-baseline.csv.gz" | cmp - scripts/vanilla-baseline.csv
rm -rf "$baseline_tmp"
```

The working-tree read and encode measurements can be repeated from the current
plain CSV with this Python snippet. It uses `time.perf_counter_ns()` and
`tracemalloc`, alternates which file is read first, and checks all decoded
bytes against the source:

```python
from pathlib import Path
import gzip
import os
import statistics
import tempfile
import time
import tracemalloc

source = Path("scripts/md-baseline.csv")
raw = source.read_bytes()
encoded = gzip.compress(raw, compresslevel=9, mtime=0)
fd, compressed_name = tempfile.mkstemp(suffix=".csv.gz")
os.close(fd)
compressed = Path(compressed_name)
compressed.write_bytes(encoded)

def read_gzip():
    with gzip.open(compressed, "rb") as report:
        return report.read()

readers = {
    "csv": lambda: source.read_bytes(),
    "gzip": read_gzip,
}
times = {name: [] for name in readers}
peaks = {name: [] for name in readers}
for repetition in range(20):
    order = list(readers.items())
    if repetition % 2:
        order.reverse()
    for name, read in order:
        tracemalloc.start()
        started = time.perf_counter_ns()
        assert read() == raw
        times[name].append((time.perf_counter_ns() - started) / 1e6)
        peaks[name].append(tracemalloc.get_traced_memory()[1])
        tracemalloc.stop()
for name in readers:
    print(name, "ms", times[name])
    print(name, "median_ms", statistics.median(times[name]))
    print(name, "peak_bytes", max(peaks[name]))
```

The recorded compression timings used the same clock and memory tracer around
10 `gzip.compress(raw, compresslevel=9, mtime=0)` calls and verified each result
with `gzip.decompress`. Git object sizes were measured separately with the
`git cat-file` command above; those physical sizes depend on the repository's
pack layout. The earlier gzip implementation passed its guard unit tests for a
clean report and for a deliberately changed code/message. Its full script test
run before a round-trip test was added reported **394 passed, 5 skipped**; the
focused guard and pin run after that test reported **47 passed**. Commands were
`pytest tests/scripts -q` and
`pytest tests/scripts/test_guard.py tests/scripts/test_resolve_pins.py -q`.
On the final plain-CSV branch, those same suites reported **392 passed, 5
skipped** and **44 passed**, respectively. No CI-time benchmark was run: the
final branch retains the original guard and baseline format, so it introduces
no CI behavior change.
The decompressed MD and vanilla files compared byte-for-byte with the original
tracked CSVs. The real MD guard could not
provide a clean corpus parity run: the available corpus and rules revisions
differed from the baseline pins, so it correctly reported input and diagnostic
drift.
