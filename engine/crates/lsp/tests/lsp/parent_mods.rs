use std::collections::HashMap;

use serde_json::{Value, json};

use crate::common::{TestServer, diagnostics_of, path_uri};

const PARENT_MOD_RULES: &str = r#"
types = {
    type[scripted_effect] = {
        path = "game/common/scripted_effects"
    }
    type[decision] = {
        path = "game/common/decisions"
        unique = yes
    }
}
decision = {
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    desc = localisation
}
alias[effect:<scripted_effect>] = yes
scripted_effect = {
    alias_name[effect] = alias_match_left[effect]
}
"#;

/// Run one workspace scan of a submod, with or without its parent mod named
/// in `parentMods`, and return every `publishDiagnostics` the scan sent,
/// keyed by URI (the last publish for a URI wins).
fn parent_mod_scan(with_parent: bool) -> (String, HashMap<String, Vec<Value>>) {
    let parent = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = parent.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };

    write(
        "common/scripted_effects/parent_effects.txt",
        "parent_eff = {\n}\n",
    );
    // The submod overrides this file, so `shadowed_eff` must stay unknown.
    write(
        "common/scripted_effects/shared_effects.txt",
        "shadowed_eff = {\n}\n",
    );
    // Same path and same unique id as the submod's copy: the load-order
    // override, never a duplicate. The bogus field would be a diagnostic if
    // parent files were validated.
    write(
        "common/decisions/shared.txt",
        "dup_decision = {\n    not_a_field = yes\n}\n",
    );
    write(
        "localisation/english/parent_l_english.yml",
        "l_english:\n PARENT_KEY:0 \"From the parent\"\n",
    );

    let mut options = json!({});
    if with_parent {
        options["parentMods"] = json!([parent.path().to_string_lossy()]);
    }
    let mut server = TestServer::builder()
        .rules(PARENT_MOD_RULES)
        .file(
            "common/scripted_effects/shared_effects.txt",
            "child_eff = {\n}\n",
        )
        .file(
            "common/decisions/shared.txt",
            "dup_decision = {\n    complete_effect = {\n        parent_eff = yes\n        shadowed_eff = yes\n    }\n    desc = PARENT_KEY\n}\n",
        )
        .init_opts(options)
        .spawn();
    server.initialized();

    let mut published: HashMap<String, Vec<Value>> = HashMap::new();
    server.read_until("the scan to finish", |frame| {
        if frame["method"] == "textDocument/publishDiagnostics" {
            let uri = frame["params"]["uri"]
                .as_str()
                .unwrap_or_else(|| panic!("publishDiagnostics without a uri: {frame}"));
            published.insert(uri.to_string(), diagnostics_of(frame).to_vec());
            return false;
        }
        frame["method"] == "loadingBar" && frame["params"]["enable"] == Value::Bool(false)
    });
    server.stop();
    (path_uri(parent.path()), published)
}

fn diagnostic_text(diags: &[Value]) -> String {
    diags
        .iter()
        .map(|d| format!("{} {}", d["code"], d["message"]))
        .collect::<Vec<_>>()
        .join("\n")
}

fn decision_diagnostics(published: &HashMap<String, Vec<Value>>) -> String {
    let (_, diags) = published
        .iter()
        .find(|(uri, _)| uri.ends_with("common/decisions/shared.txt"))
        .expect("the submod decision was never published");
    diagnostic_text(diags)
}

#[test]
fn test_parent_mod_definitions_and_loc_resolve_in_a_submod() {
    let (_, without) = parent_mod_scan(false);
    let control = decision_diagnostics(&without);
    assert!(
        control.contains("parent_eff") && control.contains("PARENT_KEY"),
        "without parentMods the parent's effect and loc key must be unknown, got:\n{control}"
    );

    let (parent_uri, with) = parent_mod_scan(true);
    let text = decision_diagnostics(&with);
    assert!(
        !text.contains("parent_eff") && !text.contains("PARENT_KEY"),
        "the parent's effect and loc key must resolve, got:\n{text}"
    );
    assert!(
        text.contains("shadowed_eff"),
        "a parent file the submod overrides by path must not be indexed, got:\n{text}"
    );
    assert!(
        !text.contains("CW261"),
        "overriding the parent's file is not a duplicate definition, got:\n{text}"
    );
    let parent_prefix = parent_uri.trim_end_matches('/');
    let leaked: Vec<&String> = with
        .keys()
        .filter(|uri| uri.starts_with(parent_prefix))
        .collect();
    assert!(
        leaked.is_empty(),
        "parent-mod files must never be diagnosed, got publishes for {leaked:?}"
    );
}
