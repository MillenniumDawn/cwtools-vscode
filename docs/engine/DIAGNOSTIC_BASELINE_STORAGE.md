# Diagnostic baseline storage measurement

The guard baselines remain plain CSV. Gzip makes the Millennium Dawn file much
smaller in a checkout, but Git already compresses text blobs; in the measured
object database the additional saving was small, the diff became opaque, and
reading the report became slower and used more peak memory. Per-code summaries
were also prototyped: a digest preserves aggregate pass/fail detection but
throws away row-level review, while sample hashes can miss a changed row. The
useful improvement is a `--bless` preview: show row changes before replacing
the CSV and retain a full diff when rows move.

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

## Other representations considered

The MD baseline contains 19,591 diagnostic rows across 20 codes. A temporary
prototype grouped those codes and compared three small text summaries. The
sizes below are generated representations, not measured Git object sizes; the
zlib column is a level-9 estimate that explains why Git can already pack the
plain baseline efficiently.

| Representation | MD bytes | MD zlib-9 bytes | Vanilla bytes | Vanilla zlib-9 bytes |
| --- | ---: | ---: | ---: | ---: |
| Plain CSV | 4,092,688 | 168,219 | 832 | 435 |
| Per-code counts | 193 | 115 | 53 | 44 |
| Per-code count + SHA-256 of all sorted rows | 1,515 | 879 | 400 | 260 |
| Per-code count + hashes of first 3 sorted rows | 1,137 | 675 | 164 | 130 |

Counts alone do not detect a changed message or line if the number of
diagnostics for that code stays the same. Adding a SHA-256 over all sorted rows
detects that drift, but it replaces thousands of actionable rows with one
opaque hash per code. Sampling the first three rows is smaller but weaker: in
the 1,555-row CW100 group, changing its last sorted row leaves the count and
all three sample hashes unchanged, while the full digest changes. Keeping the
CSV retains the exact row that changed and the ordinary readable Git diff.

This snippet reproduces the candidate byte and zlib-9 sizes above. It is a
size experiment only; it does not change or write the committed baselines.

```python
import csv
import hashlib
import zlib
from pathlib import Path


def encode_row(row):
    return ",".join(
        '"' + value.replace('"', '""') + '"'
        if any(char in value for char in ',"\n')
        else value
        for value in row
    )


for name in ("md-baseline.csv", "vanilla-baseline.csv"):
    source = Path("scripts") / name
    lines = [line for line in source.read_text().splitlines() if not line.startswith("#")]
    rows = list(csv.reader(lines))[1:]
    groups = {}
    for row in rows:
        groups.setdefault(row[3], []).append(encode_row(row))
    counts = "# code,count\n" + "".join(
        f"{code},{len(values)}\n" for code, values in sorted(groups.items())
    )
    digests = "# code,count,sha256-of-sorted-rows\n" + "".join(
        f"{code},{len(values)},{hashlib.sha256(chr(10).join(values).encode()).hexdigest()}\n"
        for code, values in sorted(groups.items())
    )
    samples = "# code,count,first-3-sorted-row-hashes\n" + "".join(
        f"{code},{len(values)},"
        + ";".join(hashlib.sha256(row.encode()).hexdigest()[:16] for row in values[:3])
        + "\n"
        for code, values in sorted(groups.items())
    )
    variants = {
        "plain": source.read_bytes(),
        "counts": counts.encode(),
        "per-code-sha256": digests.encode(),
        "first-three-samples": samples.encode(),
    }
    for label, data in variants.items():
        print(name, label, len(data), len(zlib.compress(data, 9)))
```

Storing the baseline in the corpus checkout would split the expected output
away from the guard code and its CI review. It also complicates the pin
relationship because the baseline is derived from the same corpus revision it
would live beside. No versioned capture file or external artifact was added.

## Re-blessing and guard parity

When `--bless` finds an existing report with changed rows, it now prints the
removed/added counts and first 40 unified-diff lines before writing the new
baseline. It keeps `bless.diff` and `current.csv` in the reported temporary
directory so the complete change remains inspectable. If rows are unchanged,
the temporary directory is still removed; changed input pins are reported
separately even when there is no row diff. A first baseline has no prior rows
to preview.

Before/after guard checks used the same release binary (SHA-256
`d2b4decb35f2c787a297f63d47aee9d43d353e66e89202e6a453d8a60035c78a`), identical
vanilla fixture bytes, separate empty cache directories, and the same pinned
baseline. The base was
`bffea3567c534d21c995fa2adb957aea78efef55`; the candidate code and tests were
`b9f69b91ec8d075f55380f72760acf517768665c`. On both revisions, the clean
fixture matched 5 rows and exited 0. Changing one missing event reference from
`no.such.event` to `no.such.perturbed.event` changed one CW222 row; both
revisions reported `-1 +1` and exited 1.

Ten interleaved repetitions per case measured the entire small-fixture guard
run, including process startup and validation, on Linux x86_64 with Python
3.12.14 and Git 2.52.0:

| Case | Base median (range) | Candidate median (range) |
| --- | ---: | ---: |
| Clean, 5 rows | 0.134 s (0.097–0.265) | 0.129 s (0.113–0.268) |
| One CW222 row changed | 0.131 s (0.097–0.198) | 0.119 s (0.104–0.172) |

Ten separate 1 ms-sampled process-tree RSS runs per case gave these median
peaks and ranges:

| Case | Base peak RSS | Candidate peak RSS |
| --- | ---: | ---: |
| Clean | 26.3 MiB (24.7–26.8) | 26.1 MiB (14.8–26.8) |
| One CW222 row changed | 25.9 MiB (14.8–26.4) | 26.1 MiB (24.8–26.7) |

The timing ranges overlap widely, and sampled RSS occasionally missed a short
process peak; summed RSS also double-counts shared pages. These tiny-fixture
results are noisy and do not establish a whole-suite performance change. CI
invokes the guard without `--bless`; the new diff preview runs only for an
explicit re-bless. The normal guard path does no diff work on a clean report.
No full CI timing or MD-scale re-bless timing was measured.

The full Millennium Dawn guard was not used for this before/after check because
the available corpus and rules revisions differ from the committed MD baseline
pins. These results establish the required clean/drift parity on the committed
vanilla fixture, not MD-wide diagnostic parity or CI wall time.

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
pack layout. The earlier isolated gzip prototype passed guard unit tests for a
clean report and a deliberately changed code/message. Its test totals are
historical and are not the validation results for this branch. The current
plain-CSV branch with the re-bless preview passed **393 tests, 5 skipped**;
`tests/scripts/test_guard.py` passed **26 tests**. Ruff, Black, Pylint (10/10),
and strict mypy all passed. The five skips require unavailable esbuild,
`@vscode/test-cli`, or `xvfb-run`. No MD-wide parity run was possible: the
available corpus and rules revisions differ from the committed MD baseline
pins. The vanilla before/after parity run above covers the acceptance fixture,
not the MD corpus or CI wall time.
