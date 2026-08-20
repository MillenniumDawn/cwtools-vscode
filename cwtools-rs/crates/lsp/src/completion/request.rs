use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_parser::ast::ParsedFile;
use cwtools_rules::rules_types::RuleSet;
use cwtools_validation::position::{rules_at_pos, value_rules_for_key};

use crate::paths::{
    current_token_range_with_encoding, current_token_text_with_encoding,
    line_value_key_with_encoding, logical_path_from_uri, lsp_pos_to_source_in_text,
};
use crate::{AstSource, Backend, CompletionCacheEntry};

use super::{
    CONTEXT_CAP, CONTEXT_COMPLETE_THRESHOLD, ValueCompletionSets, anchor_items,
    completions_from_rules, cwt, filter_by_token, loc_completions, loc_keys, prepare_context_items,
    root_type_snippets, scope_names, sort_by_token, sort_for_kind, token_matches,
    value_completions, value_rules_need_loc_keys,
};

fn completion_request_is_current(
    generations: &HashMap<String, u64>,
    uri: &str,
    request_id: u64,
) -> bool {
    generations.get(uri).copied() == Some(request_id)
}

/// Snapshotted ruleset-derived state for one completion request. The `Arc`s
/// carry the lifetime across the request so the helpers can take borrows
/// without holding the rules read guard.
type RulesSnapshot = (
    Option<Arc<RuleSet>>,
    Arc<HashSet<String>>,
    Arc<HashMap<String, Vec<String>>>,
    Option<Arc<cwtools_game::scope_registry::ScopeRegistry>>,
);

/// One line per completion request on `target: "cwtools_completion"`, emitted
/// before every return path. Cheap when the level is disabled — tracing
/// checks that before formatting the fields — so it stays on unconditionally.
/// `strategy` is one of `"complete"` (small unfiltered list), `"filtered"`
/// (subsequence-prefiltered and capped), or `"none"` (nothing to offer);
/// `incomplete` — whether the response is flagged `is_incomplete` — follows
/// directly from it, so it isn't a separate parameter.
#[allow(clippy::too_many_arguments)]
fn log_completion_summary(
    total: Duration,
    ast: Duration,
    rules: Duration,
    build: Duration,
    items: usize,
    strategy: &str,
    path: &str,
    ast_source: &str,
) {
    let incomplete = strategy == "filtered";
    tracing::info!(
        target: "cwtools_completion",
        total_us = total.as_micros() as u64,
        ast_us = ast.as_micros() as u64,
        rules_us = rules.as_micros() as u64,
        build_us = build.as_micros() as u64,
        items,
        incomplete,
        strategy,
        path,
        ast_source,
    );
}

impl Backend {
    /// The best-ranked loc keys for the typed token, capped at [`CONTEXT_CAP`].
    /// Served from the scan-built [`LocKeyIndex`] when there is one — a linear
    /// sweep of the ~400K-key union per keystroke is the single biggest cost on
    /// the loc completion path. Same result either way (see the equivalence
    /// test in `loc_keys`).
    fn completion_loc_keys(&self, token: &str) -> HashSet<String> {
        let index = self.state.loc_key_index.read().clone();
        if let Some(index) = index {
            // The overlays are read directly rather than snapshotted: they are
            // only the open `.yml` files' keys plus the watched files' keys, and
            // cloning them per keystroke was itself thousands of allocations.
            // No other lock is held here; live before watched, matching the
            // documented order.
            let overlay = self.state.loc_live_overlay.read();
            let watched = self.state.loc_watched_overlay.read();
            return index.select(
                token,
                overlay
                    .values()
                    .chain(watched.values())
                    .flat_map(|keys| keys.iter().map(String::as_str)),
                CONTEXT_CAP,
            );
        }
        // Before the first scan builds the index (and in tests that never
        // scan): sweep whatever the union holds.
        let overlay_keys = self.loc_overlay_keys();
        let index_guard = self.state.loc_index.read();
        let keys = index_guard
            .as_deref()
            .map(|index| index.union())
            .into_iter()
            .flat_map(|keys| keys.iter().map(AsRef::as_ref))
            .chain(overlay_keys.iter().map(String::as_str));
        loc_keys::select_loc_keys(keys, token, CONTEXT_CAP)
    }

    #[tracing::instrument(
        skip_all,
        fields(
            uri = %params.text_document_position.text_document.uri,
            line = params.text_document_position.position.line,
            col = params.text_document_position.position.character,
        )
    )]
    pub(crate) async fn completion_impl(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        self.mark_activity();
        let t_start = Instant::now();
        let mut ast_dur = Duration::ZERO;
        let mut rules_dur = Duration::ZERO;
        let mut build_dur = Duration::ZERO;

        let uri = params.text_document_position.text_document.uri.to_string();
        let pos = params.text_document_position.position;
        let position_encoding = self.state.config.read().position_encoding.clone();

        if crate::paths::is_cwt_file(&uri) {
            let text = self
                .state
                .documents
                .lock()
                .get(&uri)
                .map(|doc| Arc::clone(&doc.text))
                .unwrap_or_default();
            let range = cwt::cwt_completion_range(&text, pos, &position_encoding);
            let token = current_token_text_with_encoding(
                &text,
                pos.line,
                pos.character,
                range.start.character,
                &position_encoding,
            );
            let filter_token = token
                .split_once('[')
                .map_or(token.as_str(), |(head, _)| head);
            let mut items = filter_by_token(
                cwt::cwt_completions(&text, pos, &position_encoding),
                filter_token,
            );
            anchor_items(&mut items, range);
            let strategy = if items.is_empty() { "none" } else { "complete" };
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                items.len(),
                strategy,
                "cwt",
                AstSource::None.as_str(),
            );
            return Ok(
                (!items.is_empty()).then_some(CompletionResponse::List(CompletionList {
                    is_incomplete: true,
                    items,
                })),
            );
        }

        // Resource and unsupported files never enter the game-script fallback.
        // Localisation has a separate path below.
        if !crate::paths::is_loc_file(&uri) && !crate::paths::is_script_file(&uri) {
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                0,
                "none",
                "unsupported",
                AstSource::None.as_str(),
            );
            return Ok(None);
        }
        // Fast-typing cancel: each new request stores a unique per-URI id.
        // The request captures the value at entry and
        // re-checks it before doing any heavy work; if a newer request has
        // already started, this one returns `None` so the runtime can drop
        // the work. Stops a burst of N keystrokes from stacking N parallel
        // AST walks when only the latest one matters.
        let my_generation = {
            let documents = self.state.documents.lock();
            if !documents.contains_key(&uri) {
                return Ok(None);
            }
            let request_id = self
                .state
                .next_completion_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.state
                .completion_generation
                .lock()
                .insert(uri.clone(), request_id);
            request_id
        };
        let is_stale = || {
            !completion_request_is_current(
                &self.state.completion_generation.lock(),
                &uri,
                my_generation,
            )
        };
        if is_stale() {
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                0,
                "none",
                "none",
                AstSource::None.as_str(),
            );
            return Ok(None);
        }

        // Try context-aware completions first: resolve the rules at the cursor
        // with the validation engine's own descent (aliases, typed keys,
        // subtypes, skip_root_key — see cwtools_validation::position).
        let (ws_prefix, language, scope_checks, var_checks) = {
            let cfg = self.state.config.read();
            (
                cfg.workspace_prefix.clone(),
                cfg.language.clone(),
                cfg.scope_checks,
                cfg.var_checks,
            )
        };
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);

        // Snapshot the doc text + AST into owned data, then drop the
        // `documents` guard before any heavy work. `documents.lock()` is the
        // only exclusive lock in the LSP state, so holding it across the whole
        // completion blocks `did_open`/`did_change`/`did_close` and the
        // debounced validate's AST update for the duration — the worst case
        // being the user typing into a file whose previous validate is still
        // running. The same pattern for the rules guard: clone the Arcs and
        // drop the guard. The helpers below take borrows, so the Arcs carry
        // the lifetime across the work without holding the lock. `text` is an
        // `Arc<str>`, so this clone is a refcount bump, not a copy of the
        // whole document.
        let doc_text: Arc<str> = {
            let docs = self.state.documents.lock();
            docs.get(&uri).map(|d| d.text.clone()).unwrap_or_default()
        };
        let (lsp_line, lsp_col) = lsp_pos_to_source_in_text(&doc_text, pos, &position_encoding);
        // Replace-range for every item the script paths return: the identifier
        // token under the cursor. Loc completion keeps its own behavior (the
        // cached items are shared and the token shape differs), so it is not
        // anchored here.
        let replace_range = current_token_range_with_encoding(
            &doc_text,
            pos.line,
            pos.character,
            &position_encoding,
        );
        // The typed token so far (up to the cursor, not the whole replace
        // range): the subsequence prefilter below matches candidates against
        // this, not the range end, since the range may extend past the cursor
        // mid-word.
        let token = current_token_text_with_encoding(
            &doc_text,
            pos.line,
            pos.character,
            replace_range.start.character,
            &position_encoding,
        );
        let (ruleset_arc, modifier_keys_arc, modifier_scopes_arc, scope_registry_arc): RulesSnapshot = {
            let rules_guard = self.state.rules.read();
            (
                rules_guard.ruleset.clone(),
                rules_guard.modifier_keys.clone(),
                rules_guard.modifier_scopes.clone(),
                rules_guard.scope_registry.clone(),
            )
        };
        // Drop the read guard before the heavy work. Bump the generation
        // check here too — the rule-snapshot block is cheap, but we want the
        // staleness gate to cover the full body of the function from here.
        if is_stale() {
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                0,
                "none",
                "none",
                AstSource::None.as_str(),
            );
            return Ok(None);
        }

        // Localisation completion is syntax-sensitive: ordinary text and `$...$`
        // references offer loc keys, while an open `[...]` offers data functions.
        if crate::paths::is_loc_file(&uri) {
            let context = scope_names::loc_completion_context(&doc_text, pos, &position_encoding);
            let loc_range =
                scope_names::loc_completion_range(&doc_text, pos, context, &position_encoding);
            let loc_token = current_token_text_with_encoding(
                &doc_text,
                pos.line,
                pos.character,
                loc_range.start.character,
                &position_encoding,
            );
            let t_build = Instant::now();
            let loc_keys = if context == scope_names::LocCompletionContext::DataFunction {
                HashSet::new()
            } else {
                self.completion_loc_keys(&loc_token)
            };
            let items =
                loc_completions(&loc_keys, &language, scope_registry_arc.as_deref(), context);
            build_dur = t_build.elapsed();
            let mut items = filter_by_token(items, &loc_token);
            anchor_items(&mut items, loc_range);
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                items.len(),
                if items.is_empty() { "none" } else { "filtered" },
                "loc",
                AstSource::None.as_str(),
            );
            return Ok(
                (!items.is_empty()).then_some(CompletionResponse::List(CompletionList {
                    is_incomplete: true,
                    items,
                })),
            );
        }

        // `Some(items)` = the rule context resolved (items may still be empty:
        // an unknown block where suggestions from any other level would be
        // wrong). `None` = no doc/ruleset/AST — fall through to the flat list.
        //
        // `ast_for` returns the last good parse, or (when the last parse failed)
        // a fresh parse of the live text for this request only, so a half-typed
        // buffer still resolves a context.
        //
        // The bool paired with the item list is `resolved_value_pos`: true when
        // the cursor sat at a leaf VALUE position whose rule set was concretely
        // matched (non-empty value rules). When such a position yields no items,
        // the flat variable dump below must NOT stand in for it — that dump is
        // the #74/#75/#79 bug. Key positions and unresolved contexts keep the
        // bool false so the fallback still fires for them.
        let t_ast = Instant::now();
        let mut ast_source = AstSource::None;
        let effective_ast: Option<Arc<ParsedFile>> = self.ast_snapshot_for(&uri).map(|snapshot| {
            ast_source = snapshot.source;
            snapshot.ast
        });
        ast_dur = t_ast.elapsed();
        // Whether the AST the context resolved against parsed with no errors.
        // A buffer with an unclosed clause elsewhere is still in flux — the
        // resolved context can flip on the very next keystroke — so a small
        // list from a dirty parse must stay `is_incomplete: true` even though
        // its size alone would otherwise qualify it as `"complete"` below.
        let mut context_is_clean = false;
        let context_items: Option<(Vec<CompletionItem>, usize, bool)> =
            match (effective_ast, ruleset_arc.as_ref()) {
                (Some(ast), Some(rs)) => {
                    if is_stale() {
                        log_completion_summary(
                            t_start.elapsed(),
                            ast_dur,
                            rules_dur,
                            build_dur,
                            0,
                            "none",
                            "none",
                            ast_source.as_str(),
                        );
                        return Ok(None);
                    }
                    context_is_clean = ast.errors.is_empty();
                    let info_guard = self.state.info_service.read();
                    let game = cwtools_game::constants::Game::from_str(&language);
                    let prepared = crate::validate::make_prepared(
                        rs,
                        &self.state.string_table,
                        game,
                        &info_guard.type_index,
                        &modifier_keys_arc,
                        None,
                        None,
                        scope_registry_arc.as_ref(),
                        scope_checks,
                        var_checks,
                    );
                    let t_rules = Instant::now();
                    let rctx_opt =
                        rules_at_pos(&ast, &logical_path, &prepared, lsp_line, lsp_col, true);
                    rules_dur = t_rules.elapsed();
                    let t_build = Instant::now();
                    let items = match rctx_opt {
                        // Outside any known entity — offer root-type snippets.
                        None => Some((root_type_snippets(rs, &logical_path), 0, false)),
                        Some(rctx) => {
                            let ((items, built_dropped), resolved_value_pos) =
                                if rctx.leaf.as_ref().is_some_and(|l| l.in_value) {
                                    let is_bare_key = rctx.leaf.as_ref().is_some_and(|l| {
                                        l.key.is_empty() && rctx.value_rules.is_empty()
                                    });
                                    if is_bare_key {
                                        // Bare token (no `=`): treat as a key being typed, not a value.
                                        (
                                            completions_from_rules(
                                                &rctx.child_rules,
                                                rs,
                                                &info_guard,
                                                &language,
                                                &modifier_keys_arc,
                                                &modifier_scopes_arc,
                                                scope_registry_arc.as_deref(),
                                                rctx.scope.as_ref().map(|s| s.current()),
                                                &token,
                                            ),
                                            false,
                                        )
                                    } else {
                                        let loc_keys =
                                            if value_rules_need_loc_keys(&rctx.value_rules) {
                                                self.completion_loc_keys(&token)
                                            } else {
                                                Default::default()
                                            };
                                        (
                                            value_completions(
                                                &rctx.value_rules,
                                                rs,
                                                &info_guard,
                                                scope_registry_arc.as_deref(),
                                                &language,
                                                ValueCompletionSets {
                                                    modifier_keys: &modifier_keys_arc,
                                                    modifier_scopes: &modifier_scopes_arc,
                                                    loc_keys: &loc_keys,
                                                },
                                                rctx.scope.as_ref().map(|s| s.current()),
                                                &token,
                                            ),
                                            !rctx.value_rules.is_empty(),
                                        )
                                    }
                                } else if let Some(key) = line_value_key_with_encoding(
                                    &doc_text,
                                    pos.line,
                                    pos.character,
                                    &position_encoding,
                                ) {
                                    // Mid-edit `key = |`: the last good parse has no such
                                    // leaf yet; resolve the value rules from the live line.
                                    let vr: Vec<cwtools_rules::rules_types::NewRule> =
                                        value_rules_for_key(
                                            rs,
                                            Some(&info_guard.type_index),
                                            &rctx.child_rules,
                                            &key,
                                        )
                                        .into_iter()
                                        .cloned()
                                        .collect();
                                    let resolved = !vr.is_empty();
                                    let loc_keys = if value_rules_need_loc_keys(&vr) {
                                        self.completion_loc_keys(&token)
                                    } else {
                                        Default::default()
                                    };
                                    (
                                        value_completions(
                                            &vr,
                                            rs,
                                            &info_guard,
                                            scope_registry_arc.as_deref(),
                                            &language,
                                            ValueCompletionSets {
                                                modifier_keys: &modifier_keys_arc,
                                                modifier_scopes: &modifier_scopes_arc,
                                                loc_keys: &loc_keys,
                                            },
                                            rctx.scope.as_ref().map(|s| s.current()),
                                            &token,
                                        ),
                                        resolved,
                                    )
                                } else {
                                    (
                                        completions_from_rules(
                                            &rctx.child_rules,
                                            rs,
                                            &info_guard,
                                            &language,
                                            &modifier_keys_arc,
                                            &modifier_scopes_arc,
                                            scope_registry_arc.as_deref(),
                                            rctx.scope.as_ref().map(|s| s.current()),
                                            &token,
                                        ),
                                        false,
                                    )
                                };
                            Some((items, built_dropped, resolved_value_pos))
                        }
                    };
                    build_dur = t_build.elapsed();
                    items
                }
                _ => None,
            };

        if let Some((items, built_dropped, resolved_value_pos)) = context_items {
            // `built_dropped > 0` with an empty list still means the context
            // resolved (every candidate was prefiltered out) — return the empty
            // incomplete list rather than falling into the flat fallback dump.
            if !items.is_empty() || built_dropped > 0 {
                let (mut items, is_incomplete, strategy) = prepare_context_items(
                    items,
                    built_dropped,
                    &token,
                    context_is_clean,
                    ast_source.is_current(),
                    CONTEXT_COMPLETE_THRESHOLD,
                    CONTEXT_CAP,
                );
                anchor_items(&mut items, replace_range);
                log_completion_summary(
                    t_start.elapsed(),
                    ast_dur,
                    rules_dur,
                    build_dur,
                    items.len(),
                    strategy,
                    "context",
                    ast_source.as_str(),
                );
                return Ok(Some(CompletionResponse::List(CompletionList {
                    is_incomplete,
                    items,
                })));
            }
            // Value position matched a concrete rule but had nothing to offer
            // (empty dynamic set, or a value type with no enumerable members):
            // return no completions rather than the flat variable dump.
            if resolved_value_pos {
                log_completion_summary(
                    t_start.elapsed(),
                    ast_dur,
                    rules_dur,
                    build_dur,
                    0,
                    "none",
                    "none",
                    ast_source.as_str(),
                );
                return Ok(None);
            }
        }

        // Fallback: flat global list (original behavior) when context-aware
        // matching produced nothing (no rules loaded, unrecognised path, or a
        // context `rules_at_pos` can't reach, e.g. inside an alias-driven block
        // like `check_variable = { … }`). On a large mod the workspace has tens
        // of thousands of variables/targets/keys, so cap the dump and flag the
        // result `is_incomplete` — the client re-requests as the user narrows.
        //
        // Cached by info revision: a hit skips the `info.files` walk that
        // dominates the build time on a 7k-file mod. This is the case that
        // fires on every keystroke in the half-typed state — the user is in a
        // position the AST doesn't know about, context-aware returns None,
        // and the fallback is the only thing returned. Without the cache,
        // every keystroke re-walks every file's top-level keys.
        // Only the dynamic value sets — variables and event targets — are
        // capped this low; see the comment below on why the old fallback's
        // full type/enum/key dump was cut down to just these.
        const FALLBACK_CAP: usize = 2000;
        let revision = self
            .state
            .info_revision
            .load(std::sync::atomic::Ordering::Relaxed);
        // Filter under the lock and clone only the survivors. The cache never
        // sees the token (it is shared across every request), and cloning the
        // whole capped list per keystroke just to throw most of it away in the
        // filter was the bulk of a cache-hit request.
        let hit = {
            let guard = self.state.fallback_cache.lock();
            match guard.as_ref() {
                Some(cached) if cached.revision == revision && !cached.items.is_empty() => Some(
                    cached
                        .items
                        .iter()
                        .filter(|it| token_matches(it, &token))
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            }
        };
        if let Some(mut items) = hit {
            sort_by_token(&mut items, &token);
            items.truncate(FALLBACK_CAP);
            anchor_items(&mut items, replace_range);
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                items.len(),
                "filtered",
                "fallback",
                ast_source.as_str(),
            );
            return Ok(Some(CompletionResponse::List(CompletionList {
                is_incomplete: true,
                items,
            })));
        }
        // Narrowed fallback: only the dynamic value sets — variables and event
        // targets. The old fallback also dumped every type, enum, and top-level
        // key in the workspace; that flood is exactly the "irrelevant context"
        // that appears the moment a backspace drops the cursor into a position
        // the AST can't resolve (most often a math / `check_variable` block,
        // where variables and event targets are the only things you'd type
        // anyway). Types/enums/keys are still offered wherever the context-aware
        // path resolves a real rule. The `text_edit` anchor below filters this
        // set to the typed token client-side.
        let mut items = Vec::new();

        let t_fallback_build = Instant::now();
        let info = self.state.info_service.read();
        for var in info.variable_counts.keys() {
            if items.len() >= FALLBACK_CAP {
                break;
            }
            items.push(CompletionItem {
                label: var.clone(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some("Variable".to_string()),
                sort_text: sort_for_kind(Some(CompletionItemKind::CONSTANT), var),
                ..Default::default()
            });
        }
        for et in info.event_target_counts.keys() {
            if items.len() >= FALLBACK_CAP {
                break;
            }
            let label = format!("event_target:{}", et);
            items.push(CompletionItem {
                label: label.clone(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("Event target".to_string()),
                sort_text: sort_for_kind(Some(CompletionItemKind::VARIABLE), &label),
                ..Default::default()
            });
        }
        build_dur += t_fallback_build.elapsed();

        if items.is_empty() {
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                0,
                "none",
                "none",
                ast_source.as_str(),
            );
            Ok(None)
        } else {
            // Always flag the fallback `is_incomplete` so the client re-requests
            // on each keystroke. Otherwise VS Code caches this generic dump and
            // keeps filtering it client-side even after the parse recovers and a
            // real rule context becomes available — the "stuck on abc suggestions"
            // symptom. With is_incomplete, the next keystroke re-queries and the
            // context-aware list replaces it. (#41)
            // Cache the un-anchored, un-filtered items: the replace-range is
            // per-request and the token narrows on every keystroke, so filter
            // and anchor only what is returned, not the cached copy. Clone the
            // survivors out first, then hand the full list to the cache — the
            // reverse cloned every item whether or not it matched.
            let mut returned: Vec<CompletionItem> = items
                .iter()
                .filter(|it| token_matches(it, &token))
                .cloned()
                .collect();
            *self.state.fallback_cache.lock() = Some(CompletionCacheEntry { revision, items });
            sort_by_token(&mut returned, &token);
            returned.truncate(FALLBACK_CAP);
            anchor_items(&mut returned, replace_range);
            log_completion_summary(
                t_start.elapsed(),
                ast_dur,
                rules_dur,
                build_dur,
                returned.len(),
                "filtered",
                "fallback",
                ast_source.as_str(),
            );
            Ok(Some(CompletionResponse::List(CompletionList {
                is_incomplete: true,
                items: returned,
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn completion_generation_rejects_closed_and_replaced_requests() {
        let mut generations = HashMap::from([("file:///doc".to_string(), 1)]);
        assert!(completion_request_is_current(
            &generations,
            "file:///doc",
            1
        ));

        generations.remove("file:///doc");
        assert!(!completion_request_is_current(
            &generations,
            "file:///doc",
            1
        ));
        generations.insert("file:///doc".to_string(), 2);
        assert!(!completion_request_is_current(
            &generations,
            "file:///doc",
            1
        ));
    }
}
