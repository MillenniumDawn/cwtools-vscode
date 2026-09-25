#![no_main]

use cwtools_cache::io::with_archived_file;
use libfuzzer_sys::fuzz_target;
use std::io::Write;

// The .cwb reader takes a path because it enforces the on-disk size cap before
// decoding. A fresh temp file gives the fuzzer's bytes the same path-based
// handling as a cache selected by the CLI or an LSP client.
fuzz_target!(|data: &[u8]| {
    let Ok(mut file) = tempfile::NamedTempFile::new() else {
        return;
    };
    if file.write_all(data).is_err() {
        return;
    }
    let _ = with_archived_file(file.path(), |_| ());
});
