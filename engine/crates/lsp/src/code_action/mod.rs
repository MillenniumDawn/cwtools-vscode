//! char column) verbatim; the LSP conversion is deferred to the handler, the one
//! shares one not-yet-existing file instead of each recreating it (#142).

mod create_loc_key;
mod fix_all;
mod ignore_code;
mod payload;

pub(crate) use payload::{fix_to_data, fixable_span_edits};

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;

use payload::{code_actions_from_diagnostics, fix_all_action, wants};

impl Backend {
    pub(crate) async fn code_action_impl(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        // The document text is needed for the encoding-aware column conversion.
        let Some(text) = self.file_text_for(uri.as_str()).await else {
            return Ok(None);
        };
        let encoding = self.state.config.read().position_encoding.clone();
        let only = params.context.only.as_ref();
        let mut actions = Vec::new();
        if wants(only, &CodeActionKind::QUICKFIX) {
            actions.extend(code_actions_from_diagnostics(
                &uri,
                &params.context.diagnostics,
                &text,
                &encoding,
            ));
            actions.extend(
                self.create_loc_key_actions(&uri, &params.context.diagnostics, &encoding)
                    .await,
            );
            let (ignored, root) = {
                let cfg = self.state.config.read();
                (
                    cfg.ignored_error_codes.clone(),
                    cfg.workspace_roots.first().cloned(),
                )
            };
            let settings_content = if let Some(root) = root.as_ref() {
                let path = root.join(".vscode").join("settings.json");
                tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok())
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            actions.extend(ignore_code::ignore_code_actions(
                &params.context.diagnostics,
                &ignored,
                root.as_deref(),
                settings_content.as_deref(),
            ));
        }
        if wants(only, &CodeActionKind::SOURCE_FIX_ALL)
            && let Some(action) =
                fix_all_action(&uri, &params.context.diagnostics, &text, &encoding)
        {
            actions.push(action);
        }
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }
}
