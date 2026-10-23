use super::*;
use crate::state::ParsedDoc;
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Position, Range};

fn fixture(text: &str) -> (tempfile::TempDir, crate::Backend, Url) {
    let root = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(root.path()).unwrap();
    std::fs::create_dir_all(path.join("common/test")).unwrap();
    std::fs::write(path.join("common/test/one.txt"), text).unwrap();
    let backend = crate::testing::test_backend();
    {
        let mut cfg = backend.state.config.write();
        cfg.workspace_roots = vec![path.clone()];
        let uri = Url::from_directory_path(&path).unwrap();
        cfg.workspace_uri = Some(uri.as_str().into());
        cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(uri.as_str()));
        cfg.refresh_roots();
    }
    (
        root,
        backend,
        Url::from_file_path(path.join("common/test/one.txt")).unwrap(),
    )
}

fn diagnostic(line: u32) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(line, 0), Position::new(line, 3)),
        severity: Some(DiagnosticSeverity::WARNING),
        code: Some(NumberOrString::String("CW282".to_string())),
        source: Some("cwtools".to_string()),
        message: "redundant default".to_string(),
        data: Some(serde_json::json!({"cwtoolsFix": {
            "title": "remove", "edits": [{"startLine": line + 1, "startCol": 0,
            "endLine": line + 1, "endCol": 3, "replacement": ""}]
        }})),
        ..Diagnostic::default()
    }
}

fn rules(backend: &crate::Backend) -> tempfile::TempDir {
    let rules = tempfile::tempdir().unwrap();
    std::fs::write(
        rules.path().join("test.cwt"),
        "types = { type[item] = { path = common/test } }\nitem = { good = bool }\n",
    )
    .unwrap();
    let mut cfg = backend.state.config.write();
    cfg.rules_dir = Some(rules.path().to_path_buf());
    cfg.refresh_roots();
    rules
}

async fn wait_for_hold(gate: &crate::state::HoldGate) {
    let arrived = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !gate.has_arrived() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    if !arrived {
        gate.release();
    }
    assert!(arrived, "writer did not reach the validation gate");
}

async fn enable(backend: &crate::Backend, uri: &Url, line: &str) {
    let root = backend.state.config.read().workspace_roots[0].clone();
    let file = std::fs::canonicalize(uri.to_file_path().unwrap()).unwrap();
    let hash = cwtools_driver::diagnostic_hash::diag_hash(
        &root,
        file.to_str().unwrap(),
        "CW282",
        "redundant default",
        line,
    );
    std::fs::write(root.join("baseline.txt"), hash).unwrap();
    assert!(backend.reload_baseline("baseline.txt".to_string()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suppression_removes_fixes_and_survives_unrelated_line_motion() {
    let (_root, backend, uri) = fixture("bad = yes\n");
    enable(&backend, &uri, "bad = yes").await;
    backend
        .publish_if_closed(
            uri.clone(),
            vec![diagnostic(0)],
            Some(cwtools_cache::workspace::content_hash("bad = yes\n")),
        )
        .await;
    assert_eq!(backend.state.baseline.lock().total, 1);
    assert!(
        !backend
            .state
            .fixable_edits
            .lock()
            .contains_key(uri.as_str())
    );

    std::fs::write(uri.to_file_path().unwrap(), "\nbad = yes\n").unwrap();
    backend
        .publish_if_closed(
            uri.clone(),
            vec![diagnostic(1)],
            Some(cwtools_cache::workspace::content_hash("\nbad = yes\n")),
        )
        .await;
    assert_eq!(backend.state.baseline.lock().total, 1);
    backend.reload_baseline(String::new()).await;
    backend
        .publish_if_closed(
            uri.clone(),
            vec![diagnostic(1)],
            Some(cwtools_cache::workspace::content_hash("\nbad = yes\n")),
        )
        .await;
    assert_eq!(backend.state.baseline.lock().total, 0);
    assert!(
        backend
            .state
            .fixable_edits
            .lock()
            .contains_key(uri.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_closed_sources_fail_open_and_old_epochs_are_rejected() {
    let (_root, backend, uri) = fixture("changed = yes\n");
    enable(&backend, &uri, "bad = yes").await;
    let prepared = backend
        .prepare_baseline(
            &uri,
            &[diagnostic(0)],
            None,
            Some(cwtools_cache::workspace::content_hash("bad = yes\n")),
        )
        .await;
    assert!(prepared.hashes.is_none());
    let mut diagnostics = vec![diagnostic(0)];
    backend
        .state
        .baseline
        .lock()
        .filter(uri.as_str(), &mut diagnostics, prepared, &[], true)
        .unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(backend.state.baseline.lock().total, 0);
    let old = backend
        .prepare_baseline(&uri, &[diagnostic(0)], None, None)
        .await;
    backend.reload_baseline(String::new()).await;
    assert!(
        backend
            .state
            .baseline
            .lock()
            .filter(uri.as_str(), &mut diagnostics, old, &[], true)
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_baselines_do_not_prepare_source_text() {
    let (_root, backend, uri) = fixture("bad = yes\n");
    std::fs::remove_file(uri.to_file_path().unwrap()).unwrap();
    let prepared = backend
        .prepare_baseline(&uri, &[diagnostic(0)], None, None)
        .await;
    assert!(prepared.hashes.is_none());
    assert!(prepared.source.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_tracks_clean_files_and_excludes_ignored_codes() {
    let (_root, backend, uri) = fixture("bad = yes\n");
    backend.state.baseline.lock().capture = Some(Capture::default());
    let prepared = backend
        .prepare_baseline(
            &uri,
            &[],
            None,
            Some(cwtools_cache::workspace::content_hash("bad = yes\n")),
        )
        .await;
    assert!(
        prepared.source.is_some(),
        "clean files must participate in capture freshness checks"
    );
    let mut diagnostics = vec![diagnostic(0)];
    let prepared = backend
        .prepare_baseline(&uri, &diagnostics, None, None)
        .await;
    backend
        .state
        .baseline
        .lock()
        .filter(
            uri.as_str(),
            &mut diagnostics,
            prepared,
            &["cw282".to_string()],
            true,
        )
        .unwrap();
    assert!(diagnostics.is_empty());
    let state = backend.state.baseline.lock();
    assert!(!state.capture.as_ref().unwrap().failed);
    assert_eq!(state.total, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsaved_buffers_supply_the_hash_source() {
    let (_root, backend, uri) = fixture("disk = yes\n");
    let text: Arc<str> = "unsaved = yes\n".into();
    backend
        .state
        .documents
        .lock()
        .open(
            uri.to_string(),
            ParsedDoc {
                version: 7,
                text: text.clone(),
                ast: None,
                ast_version: None,
                ast_source_bytes: 0,
                loc_cache: None,
            },
        )
        .unwrap();
    enable(&backend, &uri, "unsaved = yes").await;
    backend
        .publish_filtered(
            uri.clone(),
            vec![diagnostic(0)],
            Some(7),
            Some(cwtools_cache::workspace::content_hash(&text)),
        )
        .await;
    assert_eq!(backend.state.baseline.lock().total, 1);
    assert!(
        !backend
            .state
            .fixable_edits
            .lock()
            .contains_key(uri.as_str())
    );
    backend
        .publish_filtered(
            uri,
            vec![diagnostic(0)],
            Some(6),
            Some(cwtools_cache::workspace::content_hash(&text)),
        )
        .await;
    assert_eq!(backend.state.baseline.lock().total, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_matches_cli_hashes_and_roundtrips_suppressed_diagnostics() {
    let (workspace, backend, _uri) = fixture("test = { bad = yes }\n");
    let vanilla = tempfile::tempdir().unwrap();
    {
        let mut cfg = backend.state.config.write();
        cfg.language = "hoi4".to_string();
        cfg.vanilla_dir = Some(vanilla.path().to_path_buf());
    }
    let rules = rules(&backend);
    let result = backend
        .write_baseline_command(&[serde_json::json!("baseline.txt")], None)
        .await
        .unwrap()
        .unwrap();
    assert!(result["hashes"].as_u64().unwrap() > 0);
    let root = backend.state.config.read().workspace_roots[0].clone();
    let original = std::fs::read_to_string(root.join("baseline.txt")).unwrap();
    let session = cwtools_driver::Session::load(cwtools_driver::SessionConfig {
        game: backend.state.config.read().game().unwrap(),
        rules: cwtools_driver::RulesInput::from_path(rules.path().to_path_buf()),
        directory: workspace.path().to_path_buf(),
        parent_roots: Vec::new(),
        vanilla: Some(vanilla.path().to_path_buf()),
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    let mut cli_hashes = HashSet::new();
    for (file, errors) in session.validate_all() {
        for error in errors {
            cli_hashes.insert(cwtools_driver::diagnostic_hash::diag_hash_for_native_path(
                workspace.path(),
                &error.file,
                &file,
                error.code.unwrap(),
                &error.message,
                if error.line == 0 {
                    ""
                } else {
                    "test = { bad = yes }"
                },
            ));
        }
    }
    assert_eq!(
        original.lines().map(String::from).collect::<HashSet<_>>(),
        cli_hashes
    );
    backend.reload_baseline("baseline.txt".to_string()).await;
    backend.refresh_baseline_diagnostics().await;
    assert!(backend.state.baseline.lock().total > 0);
    backend.write_baseline_command(&[], None).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("baseline.txt")).unwrap(),
        original
    );
    assert!(backend.state.baseline.lock().capture.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_captures_unsaved_text_and_ignores_its_metadata_in_game_directories() {
    let (_root, backend, uri) = fixture("test = { good = yes }\n");
    let _rules = rules(&backend);
    let text: Arc<str> = "test = { unsaved_unknown = yes }\n".into();
    backend
        .state
        .documents
        .lock()
        .open(
            uri.to_string(),
            ParsedDoc {
                version: 1,
                text,
                ast: None,
                ast_version: None,
                ast_source_bytes: 0,
                loc_cache: None,
            },
        )
        .unwrap();
    let value = backend
        .write_baseline_command(&[serde_json::json!("common/test/baseline.txt")], None)
        .await
        .unwrap()
        .unwrap();
    assert!(value["hashes"].as_u64().unwrap() > 0);
    backend
        .reload_baseline("common/test/baseline.txt".to_string())
        .await;
    let root = backend.state.config.read().workspace_roots[0].clone();
    let baseline = Url::from_file_path(root.join("common/test/baseline.txt")).unwrap();
    assert!(backend.is_ignored_uri(baseline.as_str()));
    backend.write_baseline_command(&[], None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_watches_reload_baselines_without_queueing_them_as_script() {
    let (_root, backend, uri) = fixture("bad = yes\n");
    enable(&backend, &uri, "bad = yes").await;
    let file = backend.state.baseline.lock().file.clone().unwrap();
    std::fs::write(&file, "not a hash").unwrap();
    let baseline_uri = Url::from_file_path(file).unwrap();
    backend
        .did_change_watched_files_impl(tower_lsp::lsp_types::DidChangeWatchedFilesParams {
            changes: vec![tower_lsp::lsp_types::FileEvent {
                uri: baseline_uri.clone(),
                typ: tower_lsp::lsp_types::FileChangeType::CHANGED,
            }],
        })
        .await;
    let state = backend.state.baseline.lock();
    assert!(state.error.is_some());
    assert!(matches!(&*state.status, BaselineStatus::Disabled));
    assert!(
        state.file.is_some(),
        "a corrected file must still be watched"
    );
    assert!(
        !backend
            .state
            .watched_pending
            .lock()
            .contains(baseline_uri.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_leaves_existing_baseline_intact_and_releases_capture() {
    let (_root, backend, _uri) = fixture("test = { bad = yes }\n");
    let _rules = rules(&backend);
    let root = backend.state.config.read().workspace_roots[0].clone();
    let original = "0123456789abcdef";
    std::fs::write(root.join("baseline.txt"), original).unwrap();
    let gate = crate::state::HoldGate::new(crate::state::HoldPoint::Pass2Mid);
    *backend.state.hold_gate.lock() = Some(gate.clone());
    let worker = crate::Backend {
        client: backend.client.clone(),
        state: backend.state.clone(),
    };
    let task = tokio::spawn(async move {
        worker
            .write_baseline_command(
                &[serde_json::json!("baseline.txt")],
                Some(ProgressToken::String("baseline-test".to_string())),
            )
            .await
    });
    wait_for_hold(&gate).await;
    let cancel = backend
        .state
        .command_cancels
        .lock()
        .get("s:baseline-test")
        .cloned();
    if let Some(cancel) = &cancel {
        cancel.store(true, Ordering::Release);
    }
    gate.release();
    assert!(cancel.is_some());
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("baseline.txt")).unwrap(),
        original
    );
    assert!(backend.state.baseline.lock().capture.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_changes_reject_an_incomplete_capture() {
    let (_root, backend, uri) = fixture("test = { bad = yes }\n");
    let _rules = rules(&backend);
    let gate = crate::state::HoldGate::new(crate::state::HoldPoint::Pass2Mid);
    *backend.state.hold_gate.lock() = Some(gate.clone());
    let worker = crate::Backend {
        client: backend.client.clone(),
        state: backend.state.clone(),
    };
    let task = tokio::spawn(async move {
        worker
            .write_baseline_command(&[serde_json::json!("baseline.txt")], None)
            .await
    });
    wait_for_hold(&gate).await;
    std::fs::write(
        uri.to_file_path().unwrap(),
        "test = { different_bad = yes }\n",
    )
    .unwrap();
    backend
        .state
        .baseline
        .lock()
        .invalidate_capture(Some(&uri.to_file_path().unwrap()));
    gate.release();
    assert!(task.await.unwrap().is_err());
    assert!(backend.state.baseline.lock().capture.is_none());
    let root = backend.state.config.read().workspace_roots[0].clone();
    assert!(!root.join("baseline.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rule_and_localization_hashes_use_their_own_roots_and_whole_file_lines() {
    let (_root, backend, _uri) = fixture("good = yes\n");
    let rules = rules(&backend);
    let path = rules.path().join("bad.cwt");
    std::fs::write(&path, "bad = yes\n").unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    let mut whole_file = diagnostic(0);
    whole_file.data = Some(serde_json::json!({"baselineLine": 0}));
    backend.state.baseline.lock().capture = Some(Capture::default());
    let prepared = backend
        .prepare_baseline(&uri, &[whole_file], None, None)
        .await;
    assert_eq!(
        prepared.hashes.unwrap()[0],
        (
            cwtools_driver::diagnostic_hash::diag_hash(
                rules.path(),
                path.to_str().unwrap(),
                "CW282",
                "redundant default",
                ""
            ),
            cwtools_driver::diagnostic_hash::legacy_diag_hash(
                rules.path(),
                path.to_str().unwrap(),
                "CW282",
                "redundant default",
                0
            ),
        )
    );
    let root = backend.state.config.read().workspace_roots[0].clone();
    std::fs::create_dir(root.join("localisation")).unwrap();
    let path = root.join("localisation/test_l_english.yml");
    std::fs::write(&path, "l_english:\n key:0 \"missing\"\n").unwrap();
    let prepared = backend
        .prepare_baseline(
            &Url::from_file_path(&path).unwrap(),
            &[diagnostic(1)],
            None,
            None,
        )
        .await;
    assert_eq!(
        prepared.hashes.unwrap()[0].0,
        cwtools_driver::diagnostic_hash::diag_hash(
            &root,
            path.to_str().unwrap(),
            "CW282",
            "redundant default",
            "key:0 \"missing\"",
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_refuses_escaping_and_busy_targets_without_creating_files() {
    let (_root, backend, _uri) = fixture("test = { }\n");
    assert!(
        backend
            .write_baseline_command(&[serde_json::json!("../outside")], None)
            .await
            .is_err()
    );
    assert!(
        backend
            .write_baseline_command(&[serde_json::json!("common/test/one.txt")], None)
            .await
            .is_err()
    );
    let root = backend.state.config.read().workspace_roots[0].clone();
    assert_eq!(
        std::fs::read_to_string(root.join("common/test/one.txt")).unwrap(),
        "test = { }\n"
    );
    backend
        .state
        .scan_in_progress
        .store(true, Ordering::Release);
    assert!(
        backend
            .write_baseline_command(&[serde_json::json!("baseline.txt")], None)
            .await
            .is_err()
    );
    assert!(backend.state.baseline.lock().capture.is_none());
}
