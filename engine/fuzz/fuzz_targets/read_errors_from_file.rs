#![no_main]

use cwtools_cache::io::read_errors_from_file;
use libfuzzer_sys::fuzz_target;
use std::io::Write;

// The .cwe reader is path-based so the fuzz input goes through its bounded
// file read as well as its header and rkyv validation.
fuzz_target!(|data: &[u8]| {
    let Ok(mut file) = tempfile::NamedTempFile::new() else {
        return;
    };
    if file.write_all(data).is_err() {
        return;
    }
    let _ = read_errors_from_file(file.path());
});
