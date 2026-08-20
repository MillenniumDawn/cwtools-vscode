# Building cwtools-rs

## Prerequisites

- **Rust toolchain** — stable, installed via [rustup](https://rustup.rs/).
  `rust-toolchain.toml` pins the channel (`stable`) and pulls in rustfmt and
  clippy, so rustup sets itself up on the first `cargo` command in the tree.

No other prerequisites, on any platform. See [Platform notes](#platform-notes).

## MSRV

**1.88.0**, declared as `rust-version` in `[workspace.package]` and inherited by
every crate, so `cargo` refuses the build per crate rather than failing halfway
through with a confusing error.

1.88 is where let-chains (`if let Some(x) = a && let Some(y) = b`) stabilized
for edition 2024. There are about 60 of them across the workspace, so 1.87 and
1.86 both reject the parser and the scope registry outright with E0658. The
dependency tree wants 1.86 on its own account (criterion, the `icu_*` crates via
`url`), which is the lower bound you'd get from `cargo` alone.

Develop on stable. The MSRV is a floor for downstream consumers, not the
version you're expected to use, and the `msrv-rs` CI job builds at exactly it
so it can't creep upward unnoticed. To check locally:

```plaintext
rustup toolchain install 1.88.0
cargo +1.88.0 build --workspace --all-targets
```

The `+1.88.0` matters. `rust-toolchain.toml` selects stable and a toolchain
file outranks `rustup default`, so without the explicit override you will build
on stable and learn nothing.

## Build

```plaintext
cd cwtools-rs
cargo build --release
```

Produces two binaries in `target/release/`:

| Binary | Purpose |
|---|---|
| `cwtools` | CLI validator (`validate`, `cache-vanilla`, etc.) |
| `cwtools-server` | LSP server (used by the VS Code extension) |

## CLI subcommands

`cwtools <subcommand> --help` prints the flags for each. The full set:

- `validate`: validate a directory of game files against `.cwt` rules (the main command).
- `cache-vanilla`: pre-generate a vanilla type index from a base-game install, for later use with `validate --vanilla-cache`.
- `parse`: parse a single script file (or a directory of `.cwt` rule files) and print a summary.
- `discover`: discover and parse every file under a directory (no validation).
- `serialize`: serialize a parsed AST to a `.cwb` cache file.
- `deserialize`: read a `.cwb` cache file back and verify it.
- `rules`: parse a `.cwt` rules file or directory and print a summary.
- `loc`: parse and validate localisation `.yml` files.
- `fix`: apply the machine-applicable fixes for diagnostics that carry one. Dry-run by default; `--apply` writes.
- `explain`: print what one `CWxxx` code means, from the same reference `docs/ERROR_CODES.md` holds.
- `list-codes`: list every diagnostic code with its severity and a one-line summary.
- `completions`: print a shell completion script (`bash`, `elvish`, `fish`, `powershell`, `zsh`) on stdout.

`--quiet` (`-q`) and `--no-color` are accepted on any subcommand.

## Build all targets (debug + tests)

```plaintext
cargo build --workspace --all-targets
cargo test --workspace --all-features --no-fail-fast
```

## Release profile

The workspace uses `lto = "thin"` (not fat), `codegen-units = 1`, and
`strip = true`. Thin LTO parallelizes well and links much faster than fat LTO,
especially on Windows where MSVC `link.exe` is the bottleneck.

See `PROFILING.md` for runtime tracing.

## Platform notes

`.cargo/config.toml` sets no linker overrides. Every platform builds with its
default linker and needs no extra install.

### Windows

- MSVC `link.exe`, the rustup default. It is the slow part of a release build.
- `rust-lld` would link LTO faster, but on stable Windows MSVC it ships as a
  self-contained binary rather than an installable rustup component
  (`rustup component add rust-lld` does not work), so nothing here selects it.
  See the comment at the top of `.cargo/config.toml`.

### macOS

- The system linker (`ld64`). No special setup needed.

### Linux

- The system linker (GNU `ld` on most distros). No special setup needed.

## CI

The `build-bench` workflow (`.github/workflows/build-bench.yml`) measures clean
release-build time across all three platforms. Run it manually from the Actions
tab or push to the `perf/build-time-improvements` branch.

The `release` workflow (`.github/workflows/release.yml`) builds and archives
binaries for all three platforms and runs the release-profile workspace tests
in parallel when a `v*` tag is pushed. Publishing waits for every build and the
test suite. `workflow_dispatch` is a manual re-run: off a tag ref it builds the
archives and runs the tests but skips the publish step.
