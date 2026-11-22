//! Fixtures shared by the unit tests in this crate's modules.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::state::{Backend, DocumentState, HoldGate, HoldPoint};

/// Fails unless the only async worker stays free to run other tasks while
/// the `request` is parked at `point`. Use it on a
/// `#[tokio::test(flavor = "multi_thread", worker_threads = 1)]` runtime.
///
/// A parked worker also stops the timer driver, so the timeouts below cannot
/// fire without a fence. A watchdog thread releases the gate after a longer
/// wait, so a missing fence fails the test instead of hanging it.
pub(crate) async fn assert_walk_releases_worker<F>(backend: &Backend, point: HoldPoint, request: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let gate = HoldGate::new(point);
    *backend.state.hold_gate.lock() = Some(gate.clone());
    let watchdog_fired = Arc::new(AtomicBool::new(false));
    let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
    let watchdog = std::thread::spawn({
        let gate = gate.clone();
        let fired = watchdog_fired.clone();
        move || {
            if cancelled.recv_timeout(Duration::from_secs(15)).is_err() {
                fired.store(true, Ordering::Release);
                gate.release();
            }
        }
    });
    let walk = tokio::spawn(request);
    let arrived = tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.has_arrived() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let ping = match arrived {
        Ok(()) => tokio::time::timeout(Duration::from_secs(5), tokio::spawn(async { 42 }))
            .await
            .ok(),
        Err(_) => None,
    };
    gate.release();
    let _ = cancel.send(());
    watchdog.join().unwrap();
    assert!(arrived.is_ok(), "walk did not reach its gate: {point:?}");
    walk.await.unwrap();
    assert!(
        !watchdog_fired.load(Ordering::Acquire),
        "watchdog released a blocked runtime during {point:?}"
    );
    assert_eq!(
        ping.transpose().unwrap(),
        Some(42),
        "worker blocked by {point:?}"
    );
}

/// A `Backend` over fresh state and a live `Client`, for tests that call its
/// methods directly. The socket is dropped, so nothing it sends is read.
pub(crate) fn test_backend() -> Backend {
    test_backend_with_socket().0
}

/// A [`test_backend`] whose workspace root is a fresh directory, so a file
/// written there is readable through the access boundary and a read of it
/// shows in `access::recorded_reads_under`.
pub(crate) fn workspace_backend() -> (Backend, tempfile::TempDir) {
    let workspace = tempfile::tempdir().expect("tempdir");
    let backend = test_backend();
    {
        let mut config = backend.state.config.write();
        config.workspace_roots = vec![workspace.path().to_path_buf()];
        config.refresh_roots();
    }
    (backend, workspace)
}

/// Open `uri` with `text`, as the editor holds it.
pub(crate) fn open_document(backend: &Backend, uri: &str, text: &str) {
    backend
        .state
        .documents
        .lock()
        .open(
            uri.to_string(),
            crate::state::ParsedDoc {
                version: 1,
                text: Arc::from(text),
                ast: None,
                ast_version: None,
                ast_source_bytes: 0,
                loc_cache: None,
            },
        )
        .expect("the store accepts one small doc");
}

/// [`test_backend`] with the socket kept, for tests that read what the server
/// sends to the client.
pub(crate) fn test_backend_with_socket() -> (Backend, tower_lsp::ClientSocket) {
    let state = Arc::new(DocumentState::new());
    let captured = Arc::new(parking_lot::Mutex::new(None));
    let slot = captured.clone();
    let server_state = state.clone();
    let (_service, socket) = tower_lsp::LspService::new(move |client| {
        *slot.lock() = Some(client.clone());
        Backend {
            client,
            state: server_state.clone(),
        }
    });
    let client = captured.lock().take().unwrap();
    (Backend { client, state }, socket)
}
