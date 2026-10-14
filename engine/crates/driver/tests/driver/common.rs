use std::path::PathBuf;

use cwtools_driver::{RulesInput, Session, SessionConfig};
use cwtools_game::constants::Game;
use cwtools_test_support::{stellaris_config, testfiles};

pub(crate) fn perf_mod() -> PathBuf {
    testfiles().join("performancetest2")
}

pub(crate) fn perf_rules() -> PathBuf {
    stellaris_config().to_path_buf()
}

pub(crate) fn load_perf_session() -> cwtools_driver::SessionWithFiles {
    Session::load(SessionConfig {
        game: Game::Stellaris,
        rules: RulesInput::Dir(perf_rules()),
        directory: perf_mod(),
        parent_roots: Vec::new(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    })
}

pub(crate) const LOC_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
        }
    }
}
"#;

/// A temp workspace: `rules/` with a `## required` loc rule, and `mod/` with one
/// `thing` instance plus an optional `localisation/` file.
pub(crate) fn loc_gate_workspace(loc: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(tmp.path().join("rules").join("things.cwt"), LOC_RULES).unwrap();
    let things = tmp.path().join("mod").join("common").join("things");
    std::fs::create_dir_all(&things).unwrap();
    std::fs::write(things.join("x.txt"), "my_thing = { }\n").unwrap();
    if let Some(text) = loc {
        let loc_dir = tmp.path().join("mod").join("localisation");
        std::fs::create_dir_all(&loc_dir).unwrap();
        std::fs::write(loc_dir.join("l_english.yml"), text).unwrap();
    }
    tmp
}

pub(crate) const FILEPATH_RULES: &str = r#"
spriteType = {
    texturefile = filepath
}
types = {
    type[spriteType] = {
        path = "gfx"
    }
}
"#;

pub(crate) const THING_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
    }
}
"#;

pub(crate) fn parse_cache_entries(
    cache_dir: &std::path::Path,
) -> Vec<(String, std::time::SystemTime)> {
    let mut entries = Vec::new();
    let root = cache_dir.join("parse-cache");
    for workspace in std::fs::read_dir(&root).unwrap().flatten() {
        let dir_name = workspace.file_name().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(workspace.path()).unwrap().flatten() {
            let path = entry.path();
            let Some(ext) = path.extension() else {
                continue;
            };
            if ext != "cwb" && ext != "cwe" {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let mtime = entry.metadata().unwrap().modified().unwrap();
            entries.push((format!("{dir_name}/{name}"), mtime));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}
