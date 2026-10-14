use std::path::PathBuf;

use cwtools_driver::{RulesInput, Session, SessionConfig};
use cwtools_game::constants::Game;

use crate::common::{loc_gate_workspace, parse_cache_entries};

fn load_loc_session(
    workspace: &std::path::Path,
    parse_cache_dir: Option<PathBuf>,
) -> cwtools_driver::SessionWithFiles {
    Session::load_with_parse_cache(
        SessionConfig {
            game: Game::Hoi4,
            rules: RulesInput::Dir(workspace.join("rules")),
            directory: workspace.join("mod"),
            parent_roots: Vec::new(),
            vanilla: None,
            vanilla_cache: None,
            vanilla_cache_auto: None,
            ignore_files: &[],
            ignore_dirs: &[],
            loc_languages: None,
            case_sensitive_files: false,
            on_rules_diagnostic: None,
        },
        parse_cache_dir,
    )
}

#[test]
fn parse_cache_preserves_cold_and_warm_validation_output() {
    let tmp = loc_gate_workspace(None);
    std::fs::write(
        tmp.path().join("mod/common/things/x.txt"),
        "my_thing = { broken =\n",
    )
    .unwrap();
    let uncached = load_loc_session(tmp.path(), None).validate_all();
    let cache_dir = tmp.path().join("cache");
    let cold = load_loc_session(tmp.path(), Some(cache_dir.clone())).validate_all();
    let cache_entries: usize = std::fs::read_dir(cache_dir.join("parse-cache"))
        .unwrap()
        .flatten()
        .map(|workspace| {
            std::fs::read_dir(workspace.path())
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "cwb"))
                .count()
        })
        .sum();
    let warm = load_loc_session(tmp.path(), Some(cache_dir)).validate_all();

    assert!(cache_entries > 0);
    assert!(uncached.iter().any(|(_, errors)| !errors.is_empty()));
    assert_eq!(uncached, cold);
    assert_eq!(cold, warm);
}

#[test]
fn parse_cache_survives_a_ruleset_only_edit() {
    let tmp = loc_gate_workspace(None);
    let cache_dir = tmp.path().join("cache");
    let cold = load_loc_session(tmp.path(), Some(cache_dir.clone())).validate_all();
    let before = parse_cache_entries(&cache_dir);
    assert!(!before.is_empty());

    std::fs::write(
        tmp.path().join("rules/alias.cwt"),
        "alias[effect:foo] = scalar\n",
    )
    .unwrap();

    let warm = load_loc_session(tmp.path(), Some(cache_dir.clone())).validate_all();
    let after = parse_cache_entries(&cache_dir);
    assert_eq!(
        before, after,
        "a rules-only edit must reuse the existing parse-cache entries"
    );
    assert_eq!(cold, warm);
}

#[test]
fn unusable_parse_cache_falls_back_to_uncached_validation() {
    let tmp = loc_gate_workspace(None);
    let uncached = load_loc_session(tmp.path(), None).validate_all();
    let blocker = tmp.path().join("not-a-cache-directory");
    std::fs::write(&blocker, b"x").unwrap();

    let fallback = load_loc_session(tmp.path(), Some(blocker)).validate_all();
    assert_eq!(uncached, fallback);
}
