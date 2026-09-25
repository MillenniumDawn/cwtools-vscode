#![no_main]

use cwtools_index::vanilla_cache::load;
use libfuzzer_sys::fuzz_target;
use std::io::Write;

// The .cwv reader is path-based so the fuzz input goes through its bounded
// file read as well as its header, zstd, and rkyv validation.
fuzz_target!(|data: &[u8]| {
    let Ok(mut file) = tempfile::NamedTempFile::new() else {
        return;
    };
    if file.write_all(data).is_err() {
        return;
    }
    let _ = load(file.path());
});
