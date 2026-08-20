//! `explain` and `list-codes`: the diagnostic catalog as a thing you can read
//! from the terminal, so a `CWxxx` in a CI log doesn't need a trip to the docs.

use crate::codes;
use crate::run::EXIT_USAGE;

/// Print everything the tool knows about one code.
pub(super) fn explain(code: String) {
    let Some((const_name, entry)) = codes::entry(&code) else {
        eprintln!(
            "error: unknown diagnostic code '{code}': expected a CWxxx code the validator emits \
             (e.g. CW100, CW113, CW225); `cwtools list-codes` lists them all"
        );
        std::process::exit(EXIT_USAGE);
    };

    println!(
        "{}  {:?}  {}",
        entry.id,
        entry.severity,
        codes::rule_name(const_name)
    );
    println!();
    println!("Message  {}", entry.message_template);

    // The reference is embedded, so a row is only missing if a code was added
    // without one. Say so rather than printing a heading with nothing under it.
    match codes::doc_row(entry.id) {
        Some(row) => {
            println!("Meaning  {}", row.meaning);
            println!("Status   {}", row.status);
        }
        None => println!("Meaning  (not documented in docs/ERROR_CODES.md)"),
    }
    println!();
    println!("{}", cwtools_error_codes::doc_url(entry.id));
}

/// Print every code in the catalog, one per line.
pub(super) fn list() {
    for (const_name, entry) in codes::all() {
        let pending = if codes::is_pending_code(entry.id) {
            "  (emission pending)"
        } else {
            ""
        };
        println!(
            "{}  {:<11}  {}{}",
            entry.id,
            format!("{:?}", entry.severity),
            codes::short_description(const_name, entry),
            pending
        );
    }
}
