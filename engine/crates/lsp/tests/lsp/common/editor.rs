use super::fixtures::EDITOR_RULES;
use super::server::TestServer;

/// A server on a temp workspace with `EDITOR_RULES`, every file opened and
/// indexed, handed back live so the caller can issue requests.
pub fn editor_server(files: &[(&str, &str)]) -> TestServer {
    editor_server_with_rules(EDITOR_RULES, files)
}

pub fn editor_server_with_rules(rules: &str, files: &[(&str, &str)]) -> TestServer {
    let mut builder = TestServer::builder().rules_file("editor_rules.cwt", rules);
    for (rel, content) in files {
        builder = builder.file(rel, *content);
    }
    let mut server = builder.spawn();
    for (rel, content) in files {
        server.open_and_wait(rel, content);
    }
    server
}
