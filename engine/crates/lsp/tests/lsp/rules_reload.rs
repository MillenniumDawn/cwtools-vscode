use serde_json::{Value, json};

use crate::common::{TestServer, diagnostics_of};

const RULES_FILE: &str = "test_rules.cwt";
const RELOAD: &str = "reloadrulesconfig";

fn stats_rules(dir: &str) -> String {
    format!(
        r#"
types = {{
    type[thing] = {{
        path = "game/common/things"
    }}
}}
enums = {{
    complex_enum[stats] = {{
        path = "game/common/{dir}"
        start_from_root = yes
        name = {{
            enum_name = scalar
        }}
    }}
}}
thing = {{
    ## cardinality = 0..inf
    enum[stats] = scalar
}}
"#
    )
}

const THING_REL: &str = "common/things/t.txt";
const THING: &str = "my_thing = {\n    \n}\n";

fn reload(server: &mut TestServer) {
    let response = server.call(
        "workspace/executeCommand",
        json!({ "command": RELOAD, "arguments": [] }),
    );
    assert!(response.get("error").is_none(), "reload failed: {response}");
}

fn completion_labels(server: &mut TestServer, rel: &str, line: u32, character: u32) -> Vec<String> {
    let uri = server.uri(rel);
    let response = server.call(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let result = &response["result"];
    let items = result.as_array().or_else(|| result["items"].as_array());
    items
        .into_iter()
        .flatten()
        .filter_map(|item| item["label"].as_str().map(str::to_string))
        .collect()
}

/// A rules reload that moves a complex enum to another directory must replace the
/// base-game members the old rules collected, as a fresh start under the new rules
/// would have.
#[test]
fn test_reload_recollects_base_game_complex_enum_members() {
    let cache = tempfile::tempdir().unwrap();
    let mut server = TestServer::builder()
        .rules(&stats_rules("alpha"))
        .vanilla_file("common/alpha/a.txt", "VALUE_ALPHA = 1\n")
        .vanilla_file("common/beta/b.txt", "VALUE_BETA = 1\n")
        .file(THING_REL, THING)
        .init_opts(json!({ "cacheDir": cache.path().to_string_lossy() }))
        .spawn();
    server.initialized();
    server.wait_for_scan_done();
    server.open_and_wait(THING_REL, THING);

    let before = completion_labels(&mut server, THING_REL, 1, 4);
    assert!(before.iter().any(|l| l == "VALUE_ALPHA"), "{before:?}");
    assert!(!before.iter().any(|l| l == "VALUE_BETA"), "{before:?}");

    std::fs::write(
        server.rules_dir().join(RULES_FILE),
        stats_rules("beta").as_bytes(),
    )
    .unwrap();
    reload(&mut server);

    let after = completion_labels(&mut server, THING_REL, 1, 4);
    server.stop();
    assert!(after.iter().any(|l| l == "VALUE_BETA"), "{after:?}");
    assert!(!after.iter().any(|l| l == "VALUE_ALPHA"), "{after:?}");
}

fn trigger_rules(supplies_params: bool) -> String {
    let params = if supplies_params {
        "enum[scripted_effect_params] = scalar"
    } else {
        "scalar = scalar"
    };
    format!(
        r#"
types = {{
    type[scripted_trigger] = {{
        path = "game/common/scripted_triggers"
    }}
    type[thing] = {{
        path = "game/common/things"
    }}
}}

thing = {{
    trigger = {{
        alias_name[trigger] = alias_match_left[trigger]
    }}
}}

alias[trigger:<scripted_trigger>] = bool

alias[trigger:<scripted_trigger>] = {{
    ## cardinality = 1..inf
    {params}
}}
"#
    )
}

const CALLS_REL: &str = "common/things/calls.txt";

/// A rules reload that starts using `enum[scripted_effect_params]` must bring in the
/// base game's parameters, not check calls against the mod's alone.
#[test]
fn test_reload_collects_base_game_scripted_params_for_new_rules() {
    let cache = tempfile::tempdir().unwrap();
    let calls = "ok_thing = { trigger = { vanilla_trigger = { VANILLA_PARAM = 1 } } }\n\
                 mod_thing = { trigger = { mod_trigger = { MOD_PARAM = 1 } } }\n\
                 bad_thing = { trigger = { mod_trigger = { BOGUS = 1 } } }\n";
    let mut server = TestServer::builder()
        .rules(&trigger_rules(false))
        .vanilla_file(
            "common/scripted_triggers/v.txt",
            "vanilla_trigger = { has_flag = $VANILLA_PARAM$ }\n",
        )
        .file(
            "common/scripted_triggers/m.txt",
            "mod_trigger = { has_flag = $MOD_PARAM$ }\n",
        )
        .file(CALLS_REL, calls)
        .init_opts(json!({ "cacheDir": cache.path().to_string_lossy() }))
        .spawn();
    server.initialized();
    server.wait_for_scan_done();

    std::fs::write(
        server.rules_dir().join(RULES_FILE),
        trigger_rules(true).as_bytes(),
    )
    .unwrap();
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({ "command": RELOAD, "arguments": [] }),
    );
    let mut last_calls_publish: Option<Value> = None;
    server.read_until("the reload to answer", |frame| {
        let publishes_calls = frame["method"] == "textDocument/publishDiagnostics"
            && frame["params"]["uri"]
                .as_str()
                .is_some_and(|uri| uri.ends_with(CALLS_REL));
        if publishes_calls {
            last_calls_publish = Some(frame.clone());
        }
        frame["id"] == 2 && frame.get("method").is_none()
    });
    server.stop();

    let publish = last_calls_publish.expect("the reload scan should publish the calls file");
    let lines: Vec<(u64, &str)> = diagnostics_of(&publish)
        .iter()
        .filter_map(|d| Some((d["range"]["start"]["line"].as_u64()?, d["code"].as_str()?)))
        .collect();
    assert_eq!(
        lines,
        [(2, "CW240")],
        "only the call passing BOGUS should be reported, got: {publish}"
    );
}
