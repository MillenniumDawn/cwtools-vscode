//! `textDocument/codeAction`: turn a diagnostic's [`SuggestedFix`] into a
//! QUICKFIX code action with a `WorkspaceEdit`.
//!
//! The fix is serialized into `Diagnostic.data` at publish time (see
//! [`fix_to_data`], called from `validate.rs`) because the AST span is only in
//! scope there — a diagnostic's start position alone can't reconstruct it. The
//! client round-trips `data` back on a codeAction request, where the raw source
//! range is converted into an LSP range with the document text and the
//! negotiated position encoding (the same `source_position_to_lsp` helper
//! hover/rename use) and wrapped into a `TextEdit`.
//!
//! The payload stores ranges in the parser convention (1-based line, 0-based
//! char column) verbatim; the LSP conversion is deferred to the handler, the one
//! place with both the text and the negotiated encoding.
//!
//! Two kinds are offered: one QUICKFIX per fixable diagnostic, and one
//! `source.fixAll` that applies all of them at once — the kind
//! `editor.codeActionsOnSave` binds to. `source.fixAll` resolves overlaps with
//! `cwtools_parser::fix::plan_file_edits`, the same code the CLI `fix`
//! subcommand runs, so the two agree on what a fixed file looks like.
//!
//! A third kind is CW100-specific: its fix carries no span edit (there's
//! nothing to replace — the key doesn't exist anywhere yet), only a
//! `create_loc_key`. `create_loc_key_actions` builds a dedicated cross-file
//! QUICKFIX for it instead, inserting a stub line into a loc file (or a new
//! one) rather than editing the diagnostic's own document. See
//! `resolve_loc_insert_target` for the three-tier site it picks, and
//! `build_create_loc_key_batch` for how a batch of several missing keys
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
        // Without it (doc neither open nor readable) no correct edit can be
        // produced, so offer no action rather than a mis-ranged one.
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
            // The ignore action edits the workspace's settings file, not this
            // document; the document text above is irrelevant to it.
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
