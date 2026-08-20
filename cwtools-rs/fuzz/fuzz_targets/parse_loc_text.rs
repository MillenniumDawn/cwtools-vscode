#![no_main]

use libfuzzer_sys::fuzz_target;

// This covers more than the YAML shape. `parse_loc_text` calls `parse_entry`,
// which calls `parse_loc_elements` on every value, so the `$ref$` and `[...]`
// Jomini command parser is under test here too. That is where the reversed
// slice panic on a lone `'` in a param lived, and why the regression seed for
// it is a loc file rather than a bare string.
fuzz_target!(|text: &str| {
    let _ = cwtools_localization::parse_loc_text(text, "fuzz/l_english.yml");
});
