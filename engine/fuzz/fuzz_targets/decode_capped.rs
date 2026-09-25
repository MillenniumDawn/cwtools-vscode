#![no_main]

use cwtools_cache::io::{MAX_ARCHIVE_DECODED_BYTES, decode_capped};
use libfuzzer_sys::fuzz_target;

// The archive readers use this bounded decoder for every compressed cache body.
// Keep the sink empty: this target is for the decoder's acceptance and error
// paths, not for retaining attacker-controlled decompressed data.
fuzz_target!(|data: &[u8]| {
    let _ = decode_capped(data, MAX_ARCHIVE_DECODED_BYTES, std::io::sink());
});
