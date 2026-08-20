use cwtools_file_manager::file_manager::{FileManager, FileManagerConfig};
use std::fs;

/// Write a minimal .gfx and .gui file into a tempdir and verify both are discovered.
#[test]
fn discovers_gui_and_gfx_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    fs::write(root.join("sprites.gfx"), "spriteTypes = { }\n").unwrap();
    fs::write(root.join("ui.gui"), "guiTypes = { }\n").unwrap();

    let config = FileManagerConfig {
        root: root.to_path_buf(),
        include_dirs: vec![".".into()],
        ..Default::default()
    };
    let mut manager = FileManager::new(config);
    let files = manager.discover_and_parse().expect("discover");

    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    assert!(
        names.iter().any(|n| n == "sprites.gfx"),
        "expected sprites.gfx in discovered files, got: {:?}",
        names
    );
    assert!(
        names.iter().any(|n| n == "ui.gui"),
        "expected ui.gui in discovered files, got: {:?}",
        names
    );
}
