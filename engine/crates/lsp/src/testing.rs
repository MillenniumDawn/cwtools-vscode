//! Fixtures shared by the unit tests in this crate's modules.

use std::sync::Arc;

use crate::state::{Backend, DocumentState};

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
