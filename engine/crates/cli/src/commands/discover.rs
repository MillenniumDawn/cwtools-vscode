//! `discover`: walk a directory, parse everything found, and list it.

use cwtools_driver::search_config_for;
use cwtools_file_manager::file_manager::FileManager;
use std::path::PathBuf;

pub(super) fn run(directory: PathBuf) {
    let config = search_config_for(&directory);
    let mut manager = FileManager::new(config);
    match manager.discover_and_parse() {
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
