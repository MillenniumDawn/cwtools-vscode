# TODO

- Rust engine parity. The `[rust]` tests in `client/test/parity` are the to-do list; they go green as the Rust port catches up to the F# reference.
- F# server rebuild is blocked by a compile break in the `cwtools` submodule. The parity harness works around it by sending a complete config instead of patching the server.
- Native `osx-arm64` server build. macOS currently ships the `osx-x64` binary, which runs under Rosetta on Apple Silicon.
