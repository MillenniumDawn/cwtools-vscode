use std::path::{Path, PathBuf};

use cwtools_file_manager::{FileKind, FileManagerConfig, ScanBudget, discover_paths};

fn localisation_paths(roots: &[&Path], config: &FileManagerConfig) -> Vec<PathBuf> {
    let report = discover_paths(roots, config, &[FileKind::Localisation]).unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    report.files.into_iter().map(|file| file.path).collect()
}

#[test]
fn excluded_loc_dirs_skip_tooling_not_content() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for skip in [
        ".claude",
        ".git",
        ".vscode",
        ".idea",
        "node_modules",
        "target",
        "out",
        "dist",
        "bin",
        "obj",
        "resources",
    ] {
        let dir = root.join(skip).join("localisation");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("skip.yml"), "").unwrap();
    }
    let mut expected = Vec::new();
    for keep in [
        "localisation",
        "localization",
        "common",
        "english",
        "resources",
    ] {
        let path = root.join("localisation").join(keep).join("keep.yml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        expected.push(path);
    }
    let mut files = localisation_paths(&[root], &FileManagerConfig::default());
    files.sort();
    expected.sort();
    assert_eq!(files, expected);
}

#[test]
fn discovery_skips_non_localisation_directories() {
    let tmp = tempfile::tempdir().unwrap();
    for relative in [
        "not_loc/bad_l_english.yml",
        "localisation/good_l_english.yml",
    ] {
        let path = tmp.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "l_english:\n key:0 \"v\"\n").unwrap();
    }
    let files = localisation_paths(&[tmp.path()], &FileManagerConfig::default());
    assert_eq!(
        files,
        vec![tmp.path().join("localisation/good_l_english.yml")]
    );
}

#[test]
fn discovery_loads_the_synced_localisation_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("localisation_synced/good_l_english.yml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "l_english:\n key:0 \"v\"\n").unwrap();
    let files = localisation_paths(&[tmp.path()], &FileManagerConfig::default());
    assert_eq!(files, vec![path]);
}

#[test]
fn discovery_skips_excluded_dot_directories() {
    let tmp = tempfile::tempdir().unwrap();
    for relative in [
        ".claude/localisation/dup_l_english.yml",
        ".hidden/localisation/dup_l_english.yml",
        "localisation/.hidden/dup_l_english.yml",
        "localisation/good_l_english.yml",
    ] {
        let path = tmp.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "l_english:\n key:0 \"v\"\n").unwrap();
    }
    let files = localisation_paths(&[tmp.path()], &FileManagerConfig::default());
    assert_eq!(
        files,
        vec![tmp.path().join("localisation/good_l_english.yml")]
    );
}

#[test]
fn filtered_discovery_prunes_before_the_file_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let ignored = tmp.path().join("ignored/localisation");
    let kept = tmp.path().join("kept/localisation");
    std::fs::create_dir_all(&ignored).unwrap();
    for name in ["a.yml", "b.yml", "c.yml"] {
        std::fs::write(ignored.join(name), "l_english:\n key:0 \"ignored\"\n").unwrap();
    }
    let config = FileManagerConfig {
        exclude_dir_patterns: vec!["ignored".to_string()],
        scan_budget: ScanBudget {
            max_files: 1,
            ..ScanBudget::default()
        },
        ..FileManagerConfig::default()
    };
    assert!(localisation_paths(&[tmp.path()], &config).is_empty());

    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join("keep.yml"), "l_english:\n key:0 \"kept\"\n").unwrap();
    let files = localisation_paths(&[tmp.path()], &config);
    assert_eq!(files, vec![kept.join("keep.yml")]);
}

#[test]
fn filtered_discovery_matches_root_relative_file_and_directory_globs() {
    let tmp = tempfile::tempdir().unwrap();
    for relative in [
        "localisation/keep.yml",
        "localisation/skip.yml",
        "wip/localisation/staged.yml",
    ] {
        let path = tmp.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "l_english:\n key:0 \"v\"\n").unwrap();
    }
    let config = FileManagerConfig {
        exclude_patterns: vec!["localisation/skip.yml".to_string()],
        exclude_dir_patterns: vec!["wip/localisation".to_string()],
        ..FileManagerConfig::default()
    };
    let files = localisation_paths(&[tmp.path()], &config);
    assert_eq!(files, vec![tmp.path().join("localisation/keep.yml")]);
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("localisation");
    std::fs::create_dir_all(&loc).unwrap();
    let good = loc.join("good_l_english.yml");
    std::fs::write(&good, "l_english:\n key:0 \"v\"\n").unwrap();
    symlink(&good, loc.join("link_l_english.yml")).unwrap();
    symlink(&loc, loc.join("cycle")).unwrap();

    let files = localisation_paths(&[tmp.path()], &FileManagerConfig::default());
    assert_eq!(files, vec![good], "symlinked loc paths must be rejected");
}

#[test]
fn discover_files_returns_sorted_order() {
    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("localisation");
    std::fs::create_dir_all(loc.join("zebra")).unwrap();
    std::fs::create_dir_all(loc.join("alpha")).unwrap();
    std::fs::write(loc.join("middle.yml"), "l_english:\n k:0 \"v\"\n").unwrap();
    std::fs::write(loc.join("zebra/z.yml"), "l_english:\n k:0 \"v\"\n").unwrap();
    std::fs::write(loc.join("alpha/a.yml"), "l_english:\n k:0 \"v\"\n").unwrap();

    let files = localisation_paths(&[tmp.path()], &FileManagerConfig::default());
    let names: Vec<_> = files.iter().map(|path| path.file_name().unwrap()).collect();
    assert_eq!(names, ["a.yml", "middle.yml", "z.yml"]);
}

#[test]
fn discover_files_budget_follows_sorted_order() {
    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("localisation");
    std::fs::create_dir_all(&loc).unwrap();
    for name in ["zebra.yml", "alpha.yml", "middle.yml"] {
        std::fs::write(loc.join(name), "l_english:\n k:0 \"v\"\n").unwrap();
    }
    let config = FileManagerConfig {
        scan_budget: ScanBudget {
            max_files: 1,
            ..ScanBudget::default()
        },
        ..FileManagerConfig::default()
    };
    let files = localisation_paths(&[tmp.path()], &config);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name().unwrap(), "alpha.yml");
}
