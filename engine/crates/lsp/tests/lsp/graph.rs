use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{TestServer, poll_until_attempts, storm_base, storm_builder, storm_start};

/// A focus type whose instances reference each other, plus a decision type that
/// references a focus. Both reference keys sit at depth 1 of a type rule
/// (`relative_position_id = <focus>` is copied from the real HOI4
/// national_focus.cwt): that is the shape `is_type_ref_leaf` /
/// `build_type_ref_keys` classify, so it is what the workspace reference index
/// — and therefore the graph — can see.
const GRAPH_RULES: &str = r#"
types = {
    type[focus] = {
        path = "game/common/national_focus"
        name_field = "id"
    }
    type[decision] = { path = "game/common/decisions" }
}
focus = {
    id = scalar
    ## cardinality = 0..1
    cost = int
    ## cardinality = 0..1
    relative_position_id = <focus>
}
decision = {
    ## cardinality = 0..1
    has_focus = <focus>
}
"#;

const GRAPH_FILES: &[(&str, &str)] = &[
    (
        "common/national_focus/tree.txt",
        "focus = {\n\tid = FOCUS_A\n\tcost = 1\n}\n\
         focus = {\n\tid = FOCUS_B\n\tcost = 1\n\trelative_position_id = FOCUS_A\n}\n\
         focus = {\n\tid = FOCUS_C\n\tcost = 1\n\trelative_position_id = FOCUS_B\n}\n",
    ),
    (
        "common/decisions/d.txt",
        "my_decision = {\n\thas_focus = FOCUS_C\n}\n",
    ),
];

/// Assert `value` is exactly the `GraphData` the client declares in
/// `extension/src/common/graphTypes.ts`: an array of `GraphNode`, every
/// documented member typed as declared, every required member present, and no
/// member the client's interfaces don't know about.
fn assert_graph_data_shape(value: &serde_json::Value) {
    let nodes = value.as_array().expect("GraphData must be an array");
    assert!(!nodes.is_empty(), "expected at least one node");
    for node in nodes {
        let obj = node.as_object().expect("GraphNode must be an object");
        for (key, member) in obj {
            let ok = match key.as_str() {
                "id" | "entityType" | "name" | "entityTypeDisplayName" | "abbreviation" => {
                    member.is_string()
                }
                "isPrimary" => member.is_boolean(),
                "references" => member
                    .as_array()
                    .is_some_and(|refs| refs.iter().all(is_graph_reference)),
                "location" => is_graph_location(member),
                "details" => member
                    .as_array()
                    .is_some_and(|rows| rows.iter().all(is_graph_node_detail)),
                _ => false,
            };
            assert!(
                ok,
                "GraphNode member `{key}` = {member} is not graphTypes.ts"
            );
        }
        for required in ["id", "references", "isPrimary", "entityType"] {
            assert!(
                obj.contains_key(required),
                "GraphNode is missing the required member `{required}`: {node}"
            );
        }
    }
}

fn is_graph_reference(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.contains_key("key")
        && obj.contains_key("isOutgoing")
        && obj.iter().all(|(k, v)| match k.as_str() {
            "key" | "label" => v.is_string(),
            "isOutgoing" => v.is_boolean(),
            _ => false,
        })
}

fn is_graph_location(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.len() == 3
        && obj
            .get("filename")
            .is_some_and(serde_json::Value::is_string)
        && obj.get("line").is_some_and(serde_json::Value::is_u64)
        && obj.get("column").is_some_and(serde_json::Value::is_u64)
}

fn is_graph_node_detail(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.len() == 2
        && obj.get("key").is_some_and(serde_json::Value::is_string)
        && obj
            .get("values")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|vs| vs.iter().all(serde_json::Value::is_string))
}

/// Every `(source, target)` edge the webview would build from the payload.
fn graph_edges(nodes: &[serde_json::Value]) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for node in nodes {
        let id = node["id"].as_str().unwrap().to_string();
        for r in node["references"].as_array().unwrap() {
            let key = r["key"].as_str().unwrap().to_string();
            if r["isOutgoing"].as_bool().unwrap() {
                edges.push((id.clone(), key));
            } else {
                edges.push((key, id.clone()));
            }
        }
    }
    edges.sort();
    edges
}

/// Spawn a server over a workspace of `files`, then run `getGraphData` with
/// `arguments`, polling until the workspace scan has populated the index.
fn graph_data_response(files: &[(&str, &str)], arguments: Value) -> Value {
    let mut builder = storm_builder().rules_file("r.cwt", GRAPH_RULES);
    for (rel, content) in files {
        builder = builder.file(rel, *content);
    }
    let mut server = storm_start(builder);
    let mut response = Value::Null;
    let _ = poll_until_attempts(20, Duration::from_millis(300), || {
        response = server.call(
            "workspace/executeCommand",
            json!({ "command": "getGraphData", "arguments": arguments }),
        );
        // The reference index lands with the async workspace scan; a graph that
        // is still only seeds means the scan hasn't finished writing it.
        let settled = response["result"].as_array().is_none_or(|nodes| {
            nodes
                .iter()
                .any(|n| !n["references"].as_array().unwrap().is_empty())
        });
        settled.then_some(())
    });
    server.stop();
    response
}

#[test]
fn test_get_graph_data_is_advertised_in_capabilities() {
    // graphAvailability.ts greys the whole graph feature out unless this exact
    // name is in executeCommandProvider.commands.
    let mut server = TestServer::builder().spawn();
    let resp = server.init_response().clone();
    server.stop();

    let commands = resp["result"]["capabilities"]["executeCommandProvider"]["commands"]
        .as_array()
        .expect("executeCommandProvider.commands");
    assert!(
        commands.contains(&Value::String("getGraphData".to_string())),
        "getGraphData not advertised: {commands:?}"
    );
}

#[test]
fn test_get_graph_data_returns_client_graph_shape() {
    let response = graph_data_response(GRAPH_FILES, serde_json::json!(["focus", 3]));
    assert!(
        response["error"].is_null(),
        "getGraphData failed: {}",
        response["error"]
    );
    assert_graph_data_shape(&response["result"]);

    let nodes = response["result"].as_array().unwrap();
    let mut ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["FOCUS_A", "FOCUS_B", "FOCUS_C", "my_decision"],
        "got: {}",
        response["result"]
    );

    // The prerequisite chain, plus the decision that reaches it at depth 1.
    assert_eq!(
        graph_edges(nodes),
        [
            ("FOCUS_B".to_string(), "FOCUS_A".to_string()),
            ("FOCUS_C".to_string(), "FOCUS_B".to_string()),
            ("my_decision".to_string(), "FOCUS_C".to_string()),
        ],
        "got: {}",
        response["result"]
    );

    let focus_b = nodes.iter().find(|n| n["id"] == "FOCUS_B").unwrap();
    assert_eq!(focus_b["isPrimary"], true);
    assert_eq!(focus_b["entityType"], "focus");
    assert_eq!(focus_b["entityTypeDisplayName"], "Focus");
    // 1-based line/column pointing at the `focus = {` that opens FOCUS_B.
    assert_eq!(focus_b["location"]["line"], 5, "got: {focus_b}");
    assert_eq!(focus_b["location"]["column"], 1, "got: {focus_b}");
    assert!(
        focus_b["location"]["filename"]
            .as_str()
            .unwrap()
            .ends_with("common/national_focus/tree.txt"),
        "got: {focus_b}"
    );

    // A related type joins the graph but is not primary, and its crossing edge
    // is labelled with the type it points at.
    let decision = nodes.iter().find(|n| n["id"] == "my_decision").unwrap();
    assert_eq!(decision["isPrimary"], false);
    assert_eq!(decision["entityType"], "decision");
    assert_eq!(decision["references"][0]["label"], "focus");

    // Nothing was dropped, so no node carries a truncation notice.
    assert!(
        nodes.iter().all(|n| n["details"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["key"] != "truncated")),
        "unexpected truncation: {}",
        response["result"]
    );
}

#[test]
fn test_get_graph_data_depth_bounds_the_walk() {
    // Depth 1 reaches the decision that references FOCUS_C directly. The focus
    // instances are all seeds, so what depth prunes here is the referring type.
    let response = graph_data_response(GRAPH_FILES, serde_json::json!(["focus", 1]));
    let nodes = response["result"].as_array().unwrap();
    assert_eq!(nodes.len(), 4, "got: {}", response["result"]);

    // A decision is only reachable through a focus, so seeding on `decision`
    // with depth 1 never walks back out to the focuses.
    let response = graph_data_response(GRAPH_FILES, serde_json::json!(["decision", 1]));
    let nodes = response["result"].as_array().unwrap();
    let ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["my_decision"], "got: {}", response["result"]);
}

#[test]
fn test_get_graph_data_rejects_bad_requests() {
    let mut builder = storm_builder().rules_file("r.cwt", GRAPH_RULES);
    for (rel, content) in GRAPH_FILES {
        builder = builder.file(rel, *content);
    }
    let mut server = storm_start(builder);

    // (arguments, a phrase the error message must name)
    let cases: [(Value, &str); 6] = [
        (json!([]), "entity type"),
        (json!([42, 3]), "entity type"),
        (json!(["focus"]), "missing depth"),
        (json!(["focus", 0]), "at least 1"),
        (json!(["focus", -2]), "at least 1"),
        (json!(["not_a_type", 3]), "unknown entity type"),
    ];
    for (arguments, phrase) in &cases {
        let resp = server.call(
            "workspace/executeCommand",
            json!({ "command": "getGraphData", "arguments": arguments }),
        );
        assert!(
            resp["result"].is_null(),
            "{arguments} should not have produced a result: {resp}"
        );
        let message = resp["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(phrase),
            "{arguments}: error should name `{phrase}`, got {resp}"
        );
        assert_eq!(resp["error"]["code"], -32602, "{arguments}: got {resp}");
    }
    server.stop();
}

#[test]
fn test_get_graph_data_before_initialize_keeps_handshake_rejection() {
    let mut server = TestServer::builder().spawn_uninitialized();
    server.send_request(
        720,
        "workspace/executeCommand",
        json!({ "command": "getGraphData", "arguments": ["focus", 3] }),
    );
    let response = server.read_response_for_id(720);
    assert!(response["result"].is_null(), "got: {response}");
    assert_eq!(response["error"]["code"], -32002, "got: {response}");
    assert_eq!(response["error"]["message"], "Server not initialized");
}

#[test]
fn test_get_graph_data_while_initial_scan_builds_reports_request_failed() {
    let markers = tempfile::tempdir().unwrap();
    let gate = markers.path().join("scan-hold");
    std::fs::write(&gate, b"hold").unwrap();
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GRAPH_RULES)
        .vanilla()
        .env("CWTOOLS_SCAN_HOLD_FILE", &gate)
        .spawn();
    assert!(
        server.init_response()["error"].is_null(),
        "got: {}",
        server.init_response()
    );
    server.initialized();
    server.wait_for_scan_started(Duration::from_secs(30), "the initial scan to start");
    server.send_request(
        720,
        "workspace/executeCommand",
        json!({ "command": "getGraphData", "arguments": ["focus", 3] }),
    );
    let response = server.read_response_for_id(720);
    assert!(
        gate.exists(),
        "scan must stay held until the graph request answers"
    );
    std::fs::remove_file(gate).unwrap();
    server.wait_for_scan_done();
    assert!(response["result"].is_null(), "got: {response}");
    assert_eq!(response["error"]["code"], -32803, "got: {response}");
    assert_eq!(
        response["error"]["message"],
        "getGraphData: the workspace index is still building; try again once the initial scan finishes"
    );
}

#[test]
fn test_get_graph_data_without_rules_reports_request_failed() {
    let mut server = storm_start(storm_base());
    let response = server.call(
        "workspace/executeCommand",
        json!({ "command": "getGraphData", "arguments": ["focus", 3] }),
    );
    server.stop();
    assert!(response["result"].is_null(), "got: {response}");
    assert_eq!(response["error"]["code"], -32803, "got: {response}");
    assert_eq!(
        response["error"]["message"],
        "getGraphData: no rules config is loaded, so no entity types are known"
    );
}

#[test]
fn test_get_graph_data_on_empty_workspace_reports_not_ready() {
    // No script files at all: the command must name the problem rather than
    // hand the webview an empty array.
    let mut server = storm_start(storm_builder().rules_file("r.cwt", GRAPH_RULES));
    let resp = server.call(
        "workspace/executeCommand",
        json!({ "command": "getGraphData", "arguments": ["focus", 3] }),
    );
    server.stop();

    assert!(resp["result"].is_null(), "got: {resp}");
    assert_eq!(resp["error"]["code"], -32803, "got: {resp}");
    assert_eq!(
        resp["error"]["message"],
        "getGraphData: the workspace index is empty; no entities have been indexed"
    );
}
