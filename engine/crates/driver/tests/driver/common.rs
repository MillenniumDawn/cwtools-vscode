use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

use cwtools_driver::{RulesInput, Session, SessionConfig, SessionWithFiles};
use cwtools_game::constants::Game;
pub(crate) use cwtools_test_support::write_tree;
use cwtools_test_support::{stellaris_config, testfiles};
use tempfile::TempDir;

/// The `performancetest2` session, loaded once for the tests that only read it.
pub(crate) static PERF_SESSION: LazyLock<SessionWithFiles> = LazyLock::new(load_perf_session);

/// A [`SessionConfig`] that loads `directory` against `rules` with nothing else
/// switched on. Set the fields a test cares about on the result.
pub(crate) fn session_config<'a>(
    game: Game,
    rules: PathBuf,
    directory: PathBuf,
) -> SessionConfig<'a> {
    SessionConfig {
        game,
        rules: RulesInput::Dir(rules),
        directory,
        parent_roots: Vec::new(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    }
}

/// [`session_config`] for a HOI4 workspace laid out as `rules/` and `mod/`.
pub(crate) fn workspace_config<'a>(workspace: &Path) -> SessionConfig<'a> {
    session_config(Game::Hoi4, workspace.join("rules"), workspace.join("mod"))
}

pub(crate) fn load_perf_session() -> SessionWithFiles {
    Session::load(session_config(Game::Stellaris, perf_rules(), perf_mod()))
}

pub(crate) fn perf_mod() -> PathBuf {
    testfiles().join("performancetest2")
}

pub(crate) fn perf_rules() -> PathBuf {
    stellaris_config().to_path_buf()
}

pub(crate) const THING_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
    }
}
"#;

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

const LOC_RULES: &str = r#"
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
pub(crate) fn loc_gate_workspace(loc: Option<&str>) -> TempDir {
    let mut files = vec![
        ("rules/things.cwt", LOC_RULES),
        ("mod/common/things/x.txt", "my_thing = { }\n"),
    ];
    if let Some(text) = loc {
        files.push(("mod/localisation/l_english.yml", text));
    }
    write_tree(&files)
}

pub(crate) fn parse_cache_entries(cache_dir: &Path) -> Vec<(String, SystemTime)> {
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
