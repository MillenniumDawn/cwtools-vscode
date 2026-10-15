//! Fixtures shared by the unit tests in this crate's modules.

use std::sync::Arc;

use crate::state::{Backend, DocumentState};

/// A `Backend` over fresh state and a live `Client`, for tests that call its
/// methods directly. The socket is dropped, so nothing it sends is read.
pub(crate) fn test_backend() -> Backend {
    test_backend_with_socket().0
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
