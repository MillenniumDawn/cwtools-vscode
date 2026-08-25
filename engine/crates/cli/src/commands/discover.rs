//! `discover`: walk a directory, parse everything found, and list it.

use cwtools_driver::{discover_and_parse_workspace, search_config_for};
use std::path::PathBuf;

pub(super) fn run(directory: PathBuf) {
    let config = search_config_for(&directory);
    match discover_and_parse_workspace(config) {
        Ok(files) => {
            println!(
                "Discovered and parsed {} files in {}",
                files.len(),
                directory.display()
            );
            for f in files {
                println!(
                    "  {} [{}] — leaves: {}",
                    f.logical_path,
                    f.path.display(),
                    f.arena.leaves.len()
                );
            }
        }
        Err(e) => {
            eprintln!("Error discovering files in {}: {}", directory.display(), e);
            std::process::exit(1);
        }
    }
}
