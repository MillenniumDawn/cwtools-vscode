#![no_main]

use cwtools_string_table::string_table::StringTable;
use libfuzzer_sys::fuzz_target;

// `&str`, not `&[u8]`: libfuzzer-sys hands the whole input to
// `Arbitrary::arbitrary_take_rest`, which for `&str` is the longest valid
// UTF-8 prefix. A valid-UTF-8 seed therefore reaches the parser byte for byte,
// and a crash artifact is readable as text instead of a hex dump. Production
// only ever calls this with `&str` anyway; the file reader decodes first.
//
// A fresh table per run is deliberate. Sharing one across runs would grow
// without bound and trip libFuzzer's RSS limit long before it found anything.
fuzz_target!(|text: &str| {
    let table = StringTable::new();
    let _ = cwtools_parser::parser::parse_string(text, &table);
});
