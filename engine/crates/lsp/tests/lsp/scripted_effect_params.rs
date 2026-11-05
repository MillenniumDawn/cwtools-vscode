use serde_json::Value;

use crate::common::TestServer;

const RULES: &str = r#"
types = {
    type[scripted_trigger] = {
        path = "game/common/scripted_triggers"
    }
    type[thing] = {
        path = "game/common/things"
    }
}

thing = {
    trigger = {
        alias_name[trigger] = alias_match_left[trigger]
    }
}

alias[trigger:<scripted_trigger>] = bool

alias[trigger:<scripted_trigger>] = {
    ## cardinality = 1..inf
    enum[scripted_effect_params] = scalar
}
"#;

const MOD_TRIGGER_REL: &str = "common/scripted_triggers/m.txt";
const MOD_TRIGGER: &str = "mod_trigger = { has_flag = $MOD_PARAM$ }\n";
const CALLS_REL: &str = "common/things/calls.txt";

fn line_codes(diags: &[Value]) -> Vec<(u64, &str)> {
    diags
        .iter()
        .filter_map(|d| Some((d["range"]["start"]["line"].as_u64()?, d["code"].as_str()?)))
        .collect()
}

#[test]
fn test_scan_checks_scripted_trigger_calls_against_base_game_parameters() {
    let cache = tempfile::tempdir().unwrap();
    let calls = "ok_thing = { trigger = { vanilla_trigger = { VANILLA_PARAM = 1 } } }\n\
                 bad_thing = { trigger = { mod_trigger = { BOGUS = 1 } } }\n";
    let mut server = TestServer::builder()
        .rules(RULES)
        .vanilla_file(
            "common/scripted_triggers/v.txt",
            "vanilla_trigger = { has_flag = $VANILLA_PARAM$ }\n",
        )
        .file(MOD_TRIGGER_REL, MOD_TRIGGER)
        .file(CALLS_REL, calls)
        .init_opts(serde_json::json!({ "cacheDir": cache.path().to_string_lossy() }))
        .spawn();
    server.initialized();
    let diags = server.wait_for_diags(CALLS_REL);
    server.stop();

    assert_eq!(
        line_codes(&diags),
        [(1, "CW240")],
        "only the call passing the undeclared BOGUS should be reported, got: {diags:?}"
    );
}

#[test]
fn test_scan_does_not_check_scripted_trigger_calls_without_the_base_game() {
    let calls = "bad_thing = { trigger = { mod_trigger = { BOGUS = 1 } } }\n";
    let mut server = TestServer::builder()
        .rules(RULES)
        .file(MOD_TRIGGER_REL, MOD_TRIGGER)
        .file(CALLS_REL, calls)
        .spawn();
    server.initialized();
    let diags = server.wait_for_diags(CALLS_REL);
    server.stop();

    assert!(
        diags.is_empty(),
        "without the base game the parameter check stays lenient, got: {diags:?}"
    );
}
