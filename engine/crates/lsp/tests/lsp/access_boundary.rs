use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{GOTO_RULES, TestServer, path_uri};

const BOUNDARY_REL: &str = "common/national_focus/f.txt";
const BOUNDARY_TEXT: &str = "outer = {\n    inner = {\n        x = 1\n    }\n}\n";

/// `initializationOptions.vanillaCache` is a client-chosen path read inside
/// `initialize` itself. Pointed at a character device it read to EOF there, so
/// the handshake never came back and the window sat dead with no diagnostics.
/// The server must refuse the input and finish the handshake, then still be
/// answering afterwards rather than wedged behind the same read.
#[cfg(unix)]
#[test]
fn test_a_vanilla_cache_naming_a_character_device_does_not_stall_initialize() {
    let mut server = TestServer::builder()
        .file("probe.txt", "a = { b = 1 }\n")
        .init_opts(json!({ "language": "hoi4", "vanillaCache": "/dev/zero" }))
        .deadline(Duration::from_secs(30))
        .spawn();
    server.initialized();
    let uri = server.uri("probe.txt");
    let response = folding_ranges(&mut server, &uri);
    assert!(
        response.get("error").is_none(),
        "the server stopped answering after refusing the cache: {response}"
    );
}

/// Assert `response` is a *refusal*: a successful JSON-RPC reply carrying an
/// empty result. A transport-level error reply also has a null `result`, so
/// checking that alone would let a crashed handler pass as a clean refusal.
fn assert_refused(response: &Value, what: &str) {
    assert!(
        response.get("error").is_none(),
        "{what} must be refused with a successful empty reply, got a JSON-RPC error: {response}"
    );
    let result = &response["result"];
    let empty = result.is_null() || result.as_array().is_some_and(|a| a.is_empty());
    assert!(empty, "{what} must be refused, got: {result}");
}

/// The folding ranges from a successful reply.
fn expect_ranges(response: &Value) -> &Vec<Value> {
    assert!(
        response.get("error").is_none(),
        "expected a successful reply, got a JSON-RPC error: {response}"
    );
    response["result"].as_array().expect("folding ranges")
}

/// `textDocument/foldingRange` is the cleanest probe: it needs nothing but the
/// file's text, so its answer is a direct read-out of whether the server was
/// willing to read the URI. Ranges = allowed, null = refused. Returns the whole
/// JSON-RPC response so callers can tell a refusal from an error reply.
fn folding_ranges(server: &mut TestServer, uri: &str) -> Value {
    server.call(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    )
}

/// A server rooted at a workspace holding one foldable script file, past its
/// startup scan.
fn boundary_server() -> TestServer {
    let mut server = TestServer::builder()
        .file(BOUNDARY_REL, BOUNDARY_TEXT)
        .init_opts(json!({ "language": "hoi4" }))
        .spawn();
    server.initialized();
    server.wait_for_scan_done();
    server
}

#[test]
fn test_access_boundary_allows_a_closed_workspace_file() {
    // The no-regression half: the boundary must not cost the server its ability
    // to read files it was pointed at but that aren't open in a buffer.
    let mut server = boundary_server();
    let uri = server.uri(BOUNDARY_REL);
    let response = folding_ranges(&mut server, &uri);
    server.stop();
    let ranges = expect_ranges(&response);
    assert!(
        ranges
            .iter()
            .any(|r| r["startLine"] == 0 && r["endLine"] == 4),
        "expected the outer fold, got: {response}"
    );
}

#[test]
fn test_access_boundary_refuses_a_file_outside_the_workspace() {
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("f.txt");
    std::fs::write(&file, BOUNDARY_TEXT).unwrap();
    let mut server = boundary_server();
    let response = folding_ranges(&mut server, &path_uri(&file));
    server.stop();
    assert_refused(&response, "a readable file outside every root");
}

#[test]
fn test_access_boundary_refuses_an_open_buffer_outside_the_workspace() {
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("f.txt");
    std::fs::write(&file, BOUNDARY_TEXT).unwrap();
    let uri = path_uri(&file);
    let mut server = boundary_server();
    server.open_uri(&uri, BOUNDARY_TEXT);
    // An outside open is intentionally silent, so it cannot be used as a
    // readiness signal. Reissue the request long enough for didOpen to land;
    // serving the buffer after the initial race would expose a folding range.
    for _ in 0..20 {
        let response = folding_ranges(&mut server, &uri);
        assert_refused(&response, "an open buffer outside every workspace folder");
        std::thread::sleep(Duration::from_millis(50));
    }
    server.stop();
}

#[test]
fn test_access_boundary_refuses_a_non_file_uri() {
    // The same in-workspace file under a non-`file` scheme. `Url::to_file_path`
    // ignores the scheme, so this used to be read exactly like the `file:` form
    // — pointing it at a file that IS allowed is what makes the scheme the only
    // thing under test.
    let mut server = boundary_server();
    let http_uri = server
        .uri(BOUNDARY_REL)
        .replacen("file://", "http://localhost", 1);
    for uri in [http_uri.as_str(), "untitled:Untitled-1"] {
        let response = folding_ranges(&mut server, uri);
        assert_refused(&response, uri);
    }
    server.stop();
}

#[cfg(unix)]
#[test]
fn test_access_boundary_refuses_a_character_device() {
    // The reported crash: `file:///dev/zero` was converted to a path and read to
    // EOF, which never comes. The server must answer, and answer nothing.
    if !std::path::Path::new("/dev/zero").exists() {
        return;
    }
    let mut server = boundary_server();
    let response = folding_ranges(&mut server, "file:///dev/zero");
    server.stop();
    assert_refused(&response, "/dev/zero");
}

enum VanillaRootFixture {
    AutoDiscovered,
    RelativeSetting,
    #[cfg(unix)]
    SymlinkedSteam,
}

fn assert_loaded_files_reports_resolved_vanilla_roots(fixture: VanillaRootFixture) {
    let home = tempfile::tempdir().unwrap();
    let vanilla = home
        .path()
        .join(".steam/steam/steamapps/common/Hearts of Iron IV");
    #[cfg(unix)]
    if matches!(fixture, VanillaRootFixture::SymlinkedSteam) {
        let steam = home.path().join(".local/share/Steam");
        std::fs::create_dir_all(&steam).unwrap();
        std::fs::create_dir_all(home.path().join(".steam")).unwrap();
        std::os::unix::fs::symlink(steam, home.path().join(".steam/steam")).unwrap();
    }
    std::fs::create_dir_all(vanilla.join("common/national_focus")).unwrap();
    std::fs::write(
        vanilla.join("common/national_focus/base.txt"),
        "focus_tree = { id = base }\n",
    )
    .unwrap();
    let mut builder = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .env("HOME", home.path())
        .current_dir(home.path());
    if matches!(fixture, VanillaRootFixture::RelativeSetting) {
        builder = builder.init_opts(json!({
            "vanilla": std::path::Path::new(".steam").join("steam/steamapps/common/Hearts of Iron IV"),
        }));
    }
    let mut server = builder.spawn();
    server.initialized();
    let frame = server.read_until("the loaded files notification", |frame| {
        frame["method"] == "updateFileList"
    });
    server.stop();
    let notification = &frame["params"];
    assert!(notification["fileList"].is_array());
    let roots = notification["vanillaRoots"]
        .as_array()
        .expect("loaded files must include the resolved vanilla roots");
    let roots: Vec<&str> = roots.iter().map(|root| root.as_str().unwrap()).collect();
    let physical = std::fs::canonicalize(&vanilla).unwrap();
    assert!(roots.contains(&path_uri(&physical).as_str()));
    if !matches!(fixture, VanillaRootFixture::RelativeSetting) {
        assert!(roots.contains(&path_uri(&vanilla).as_str()));
    }
    for root in roots {
        let path = tower_lsp::lsp_types::Url::parse(root)
            .unwrap()
            .to_file_path()
            .unwrap();
        assert!(path.is_absolute());
        // A relative config may resolve a symlinked temp-dir prefix through
        // the child's cwd, such as /private/var on macOS.
        assert_eq!(std::fs::canonicalize(path).unwrap(), physical);
    }
}

#[test]
fn test_loaded_files_reports_auto_discovered_vanilla_without_an_init_setting() {
    // Matches an unset cwtools.cache.hoi4 setting in the client.
    assert_loaded_files_reports_resolved_vanilla_roots(VanillaRootFixture::AutoDiscovered);
}

#[test]
fn test_loaded_files_reports_absolute_vanilla_root_for_a_relative_init_setting() {
    assert_loaded_files_reports_resolved_vanilla_roots(VanillaRootFixture::RelativeSetting);
}

#[cfg(unix)] // Creating the Steam symlink is Unix-only; the URI protocol is portable.
#[test]
fn test_loaded_files_reports_alias_and_physical_roots_for_symlinked_steam() {
    assert_loaded_files_reports_resolved_vanilla_roots(VanillaRootFixture::SymlinkedSteam);
}

#[cfg(unix)]
#[test]
fn test_access_boundary_allows_an_auto_discovered_vanilla_install() {
    // A `vanilla` init option is optional: with none, the server probes the
    // usual Steam library paths under $HOME. That install is indexed, so goto
    // and hover land in it — and the boundary has to allow reading it, which it
    // only does if the resolved dir makes it back into the config.
    let home = tempfile::tempdir().unwrap();
    let vanilla_file = home
        .path()
        .join(".steam/steam/steamapps/common/Hearts of Iron IV/common/national_focus/base.txt");
    std::fs::create_dir_all(vanilla_file.parent().unwrap()).unwrap();
    std::fs::write(&vanilla_file, BOUNDARY_TEXT).unwrap();

    // No `vanilla`: auto-discovery is the thing under test.
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .env("HOME", home.path())
        .spawn();
    server.initialized();
    server.wait_for_scan_done();

    let response = folding_ranges(&mut server, &path_uri(&vanilla_file));
    server.stop();

    let ranges = expect_ranges(&response);
    assert!(
        ranges
            .iter()
            .any(|r| r["startLine"] == 0 && r["endLine"] == 4),
        "an auto-discovered base-game file must be readable, got: {response}"
    );
}

/// Every game id `Game::from_str` accepts, with the Steam `steamapps/common`
/// folder `discover_vanilla_dir` has to map it to. ARCHITECTURE.md lists that
/// mapping as one of the sites a new game touches in lockstep, and as one
/// of the ones the compiler cannot catch: the match ends in `_ => None`, so a
/// missing or misspelled arm compiles and quietly discovers nothing forever.
/// `custom` is deliberately absent — it ships under no fixed Steam folder, and
/// takes the `None` arm today.
#[cfg(unix)]
const STEAM_INSTALL_FOLDERS: &[(&str, &str)] = &[
    ("hoi4", "Hearts of Iron IV"),
    ("stellaris", "Stellaris"),
    ("eu4", "Europa Universalis IV"),
    ("ck2", "Crusader Kings II"),
    ("ck3", "Crusader Kings III"),
    ("vic2", "Victoria 2"),
    ("vic3", "Victoria 3"),
    ("ir", "ImperatorRome"),
    ("eu5", "Europa Universalis V"),
];

/// Boot a server for `game` against a `$HOME` holding a base-game file under
/// `folder`, and ask for that file's folding ranges. Ranges mean the install
/// was discovered — nothing else puts a path outside the workspace inside the
/// access boundary — and an empty result means it was not.
#[cfg(unix)]
fn auto_discovered_vanilla_response(game: &str, folder: &str) -> Value {
    let home = tempfile::tempdir().unwrap();
    let vanilla_file = home
        .path()
        .join(".steam/steam/steamapps/common")
        .join(folder)
        .join("common/national_focus/base.txt");
    std::fs::create_dir_all(vanilla_file.parent().unwrap()).unwrap();
    std::fs::write(&vanilla_file, BOUNDARY_TEXT).unwrap();

    // No `vanilla`: auto-discovery is the thing under test.
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .init_opts(json!({ "language": game }))
        .env("HOME", home.path())
        .spawn();
    server.initialized();
    server.wait_for_scan_done();
    let response = folding_ranges(&mut server, &path_uri(&vanilla_file));
    server.stop();
    response
}

#[cfg(unix)]
#[test]
fn test_steam_folder_is_mapped_for_every_supported_game() {
    for (game, folder) in STEAM_INSTALL_FOLDERS {
        let response = auto_discovered_vanilla_response(game, folder);
        assert!(
            !response["result"].is_null(),
            "{game} must auto-discover its install under {folder:?}; an empty \
             result means the file was refused as outside the boundary, so the \
             mapping produced no install: {response}"
        );
        let ranges = expect_ranges(&response);
        assert!(
            ranges
                .iter()
                .any(|r| r["startLine"] == 0 && r["endLine"] == 4),
            "{game} must auto-discover its install under {folder:?}, got: {response}"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_steam_folder_mapping_is_per_game_not_a_wildcard() {
    // The positive case above passes for any implementation that discovers
    // *something* under `steamapps/common`. This is what makes it a mapping:
    // an install sitting under another game's folder name is not this game's.
    let response = auto_discovered_vanilla_response("hoi4", "Stellaris");
    assert_refused(
        &response,
        "an install under a different game's Steam folder",
    );
}
