use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let Some(md) = cwtools_test_support::checkout("CWTOOLS_CORPUS", "Millennium-Dawn") else {
        eprintln!(
            "md_bench: no corpus. Set CWTOOLS_CORPUS to a Millennium-Dawn checkout (or \
             CWTOOLS_PROJECTS to the folder holding it)"
        );
        return;
    };
    let md = md.display();
    let dirs: Vec<String> = [
        "common/countries",
        "common/ideas",
        "common/national_focus",
        "common/decisions",
        "events",
        "history",
    ]
    .iter()
    .map(|sub| format!("{md}/{sub}"))
    .collect();

    let mut total_files = 0usize;
    let mut total_leaves = 0usize;
    let start = Instant::now();

    let mut all_files: Vec<Vec<cwtools_file_manager::file_manager::ParsedFile>> = Vec::new();

    for dir in &dirs {
        let config = cwtools_file_manager::file_manager::FileManagerConfig {
            root: PathBuf::from(dir),
            include_dirs: vec![".".into()],
            file_patterns: vec!["*.txt".into()],
            exclude_patterns: vec![],
            ..Default::default()
        };
        let mut manager = cwtools_file_manager::file_manager::FileManager::new(config);
        match manager.discover_and_parse() {
            Ok(files) => {
                let leaves: usize = files.iter().map(|f| f.arena.leaves.len()).sum();
                total_files += files.len();
                total_leaves += leaves;
                println!("  {}: {} files, {} leaves", dir, files.len(), leaves);
                all_files.push(files);
            }
            Err(e) => {
                eprintln!("Error in {}: {}", dir, e);
            }
        }
    }

    let elapsed = start.elapsed();
    println!(
        "\n  BENCH: {} files, {} leaves in {:.3}s",
        total_files,
        total_leaves,
        elapsed.as_secs_f64()
    );
    println!(
        "  Holding {} batch objects in memory for RSS measurement...",
        all_files.len()
    );
    std::thread::sleep(std::time::Duration::from_secs(10));
    println!("  Done. Held in memory.");
}
