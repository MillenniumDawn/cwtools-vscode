#![no_main]

use cwtools_string_table::string_table::StringTable;
use libfuzzer_sys::fuzz_target;

// A fresh string table prevents a fuzz campaign from growing state across
// invocations. The target runs both the text parser and the public CWT
// converter against each input.
fuzz_target!(|text: &str| {
    let table = StringTable::new();
    let parsed = cwtools_parser::parser::parse_string(text, &table);
    let _ = cwtools_rules::rules_converter::ast_to_ruleset(&parsed, &table);
});
