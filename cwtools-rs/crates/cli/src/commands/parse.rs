//! `parse`: summarize one script file, or a directory of `.cwt` rule files.

use cwtools_file_manager::file_manager::{FileManager, FileManagerConfig, ScanBudget};
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;
use std::path::PathBuf;

use super::rules::print_ruleset_summary;

pub(super) fn run(file: PathBuf) {
    if file.is_dir() {
        // Treat as a directory of .cwt rule files
        let table = StringTable::new();
        let (ruleset, errors) = load_ruleset_from_dir(&file, &table, ScanBudget::default());
        for err in &errors {
            eprintln!("warn: {}", err);
        }
        println!("Parsed rule directory: {}", file.display());
        print_ruleset_summary(&ruleset, false);
    } else {
        let mut manager = FileManager::new(FileManagerConfig::default());
        match manager.parse_single_file(&file) {
            Ok(parsed) => {
                println!("Parsed: {}", file.display());
                println!("  Logical path:  {}", parsed.logical_path);
                println!("  Leaves:        {}", parsed.arena.leaves.len());
                println!("  Values:        {}", parsed.arena.leaf_values.len());
                println!("  Comments:      {}", parsed.arena.comments.len());
                println!("  Root children: {}", parsed.root_children.len());
                // The parser recovers rather than bailing, so a malformed
                // file still returns Ok with a partial AST. Reporting only
                // the summary made `a = { b =` look clean.
                if !parsed.errors.is_empty() {
                    eprintln!(
                        "\n{} parse error(s) in {}:",
                        parsed.errors.len(),
                        file.display()
                    );
                    for e in &parsed.errors {
                        eprintln!("  {}", e);
                    }
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Error parsing {}: {}", file.display(), e);
                std::process::exit(1);
            }
        }
    }
}
