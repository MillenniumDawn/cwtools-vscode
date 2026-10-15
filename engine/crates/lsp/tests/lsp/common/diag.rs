use serde_json::{Value, json};

use super::server::TestServer;

const PUBLISH: &str = "textDocument/publishDiagnostics";

/// The diagnostics array of a `publishDiagnostics` notification.
pub fn diagnostics_of(publish: &Value) -> &[Value] {
    publish["params"]["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("publishDiagnostics without a diagnostics array: {publish}"))
}

/// Whether a publish notification's diagnostics include `code`.
pub fn publish_has_code(publish: &Value, code: &str) -> bool {
    diagnostics_of(publish).iter().any(|d| d["code"] == code)
}

fn publishes_for(frame: &Value, suffix: &str) -> bool {
    frame["method"] == PUBLISH
        && frame["params"]["uri"]
            .as_str()
            .is_some_and(|uri| uri.ends_with(suffix))
}

impl TestServer {
    /// The `occurrence`th `publishDiagnostics` (counting from this call) for the
    /// URI ending in `suffix`.
    pub fn publish_for(&mut self, suffix: &str, occurrence: usize) -> Value {
        let mut seen = 0;
        self.read_until(
            &format!("publishDiagnostics #{occurrence} for {suffix}"),
            |frame| {
                if !publishes_for(frame, suffix) {
                    return false;
                }
                seen += 1;
                seen >= occurrence
            },
        )
    }

    /// The diagnostic codes of the `occurrence`th publish for `suffix`.
    pub fn diag_codes_for(&mut self, suffix: &str, occurrence: usize) -> Vec<String> {
        let publish = self.publish_for(suffix, occurrence);
        diagnostics_of(&publish)
            .iter()
            .filter_map(|d| d["code"].as_str().map(str::to_string))
            .collect()
    }

    /// As [`TestServer::diag_codes_for`], but each diagnostic's `(code, message)`
    /// pair, for a test that needs a message to name the right file and line.
    pub fn diag_messages_for(&mut self, suffix: &str, occurrence: usize) -> Vec<(String, String)> {
        let publish = self.publish_for(suffix, occurrence);
        diagnostics_of(&publish)
            .iter()
            .filter_map(|d| {
                Some((
                    d["code"].as_str()?.to_string(),
                    d["message"].as_str()?.to_string(),
                ))
            })
            .collect()
    }

    /// The whole diagnostics array of the next publish for `rel_path`, for the
    /// tests that need the payloads and not just the readiness signal.
    pub fn wait_for_diags(&mut self, rel_path: &str) -> Vec<Value> {
        let publish = self.publish_for(rel_path, 1);
        diagnostics_of(&publish).to_vec()
    }

    /// Read until a publish for `suffix` carries a diagnostic whose `code`
    /// matches, and return that diagnostic (range and data included).
    pub fn wait_for_diag_object(&mut self, suffix: &str, code: &str) -> Value {
        let mut found = None;
        self.read_until(&format!("a {code} diagnostic for {suffix}"), |frame| {
            if !publishes_for(frame, suffix) {
                return false;
            }
            found = diagnostics_of(frame)
                .iter()
                .find(|d| d["code"] == code)
                .cloned();
            found.is_some()
        });
        found.expect("read_until returned without a match")
    }

    /// Ask for the code actions on `diag`, a diagnostic published for
    /// `rel_path`, optionally only the quickfixes. Returns the whole response.
    pub fn code_actions(&mut self, rel_path: &str, diag: &Value, only_quickfix: bool) -> Value {
        let mut context = json!({ "diagnostics": [diag] });
        if only_quickfix {
            context["only"] = json!(["quickfix"]);
        }
        let uri = self.uri(rel_path);
        self.call(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": diag["range"],
                "context": context,
            }),
        )
    }
}
