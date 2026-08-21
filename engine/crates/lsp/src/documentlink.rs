//! `textDocument/documentLink`: filepath/icon leaves render as clickable
//! links. Leaves are found by the same rule-matched walk semantic tokens use
//! (`block_rules_for` + matched-rule descent), so only fields the ruleset
//! types as `filepath[..]` / `icon[..]` produce links; targets are probed
//! against the workspace root then the vanilla install, and only a file that
//! exists inside one of them links.

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_rules::rules_types::{NewField, Options, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::Prepared;
use cwtools_validation::position::value_rules_for_key;

use crate::Backend;
use crate::navigation::{unquote, value_col_in_line, value_start_after_eq};
use crate::paths::source_position_to_lsp;
use crate::semantic::{block_rules_for, node_bodies, valueclause_bodies};

/// Upper bound on emitted links per file, so a pathological file can't turn
/// one request into thousands of filesystem stats.
const MAX_LINKS: usize = 200;

/// One filepath/icon leaf found by the walk: where its key sits (1-based
/// line, char col), the raw leaf value, and the game-relative path it names.
struct LinkCandidate {
    key_line: u32,
    key_col: u16,
    raw_value: String,
    rel_path: String,
}

impl Backend {
    pub(crate) async fn document_link_impl(
        &self,
        params: DocumentLinkParams,
    ) -> Result<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri.to_string();
        if crate::paths::is_loc_file(&uri) || crate::paths::is_cwt_file(&uri) {
            return Ok(None);
        }
        let Some(ast) = self.ast_for(&uri) else {
            return Ok(None);
        };
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(None);
        };
        let (game, scope_checks, var_checks, encoding, ws_prefix) = {
            let cfg = self.state.config.read();
            (
                cfg.game(),
                cfg.scope_checks,
                cfg.var_checks,
                cfg.position_encoding.clone(),
                cfg.workspace_prefix.clone(),
            )
        };
        let logical_path = crate::paths::logical_path_from_uri(&uri, &ws_prefix);
        let (ruleset, modifier_keys, scope_registry) = {
            let rules = self.state.rules.read();
            (
                rules.ruleset.clone(),
                rules.modifier_keys.clone(),
                rules.scope_registry.clone(),
            )
        };
        let Some(ruleset) = ruleset else {
            return Ok(None);
        };

        // `Prepared` borrows the index guard, so the walk happens inside this
        // scope; probing runs after the guards drop.
        let candidates = {
            let info = self.state.info_service.read();
            let prepared = crate::validate::make_prepared(
                &ruleset,
                &self.state.string_table,
                game,
                &info.type_index,
                &modifier_keys,
                None,
                None,
                scope_registry.as_ref(),
                scope_checks,
                var_checks,
            );
            let mut out = Vec::new();
            let walk = LinkWalk {
                ast: &ast,
                table: &self.state.string_table,
                prepared: &prepared,
                ruleset: &ruleset,
                logical_path: &logical_path,
            };
            walk.collect(&ast.root_children, None, &mut out);
            out
        };

        // Resolve each candidate's value token span from text and probe the
        // workspace root, then vanilla, for the referenced file.
        let roots = self.search_roots();
        let lines: Vec<&str> = text.lines().collect();
        let fallback = &params.text_document.uri;
        let mut links = Vec::new();
        for c in candidates.into_iter().take(MAX_LINKS) {
            let value = unquote(&c.raw_value);
            let line0 = c.key_line.saturating_sub(1);
            let Some(line) = lines.get(line0 as usize) else {
                continue;
            };
            let Some(from) = value_start_after_eq(line, c.key_col as u32) else {
                continue;
            };
            let Some(col) = value_col_in_line(line, value, from) else {
                continue;
            };
            let rel = std::path::Path::new(c.rel_path.trim_start_matches(['/', '\\']));
            // Async, and contained before it is probed: a link request must not
            // block the runtime on a sync filesystem syscall, and a leaf value
            // is mod content that must not reach outside the roots (#176).
            let Some(target) = crate::access::contained_search_path(&roots, rel).await else {
                continue;
            };
            let start = source_position_to_lsp(&text, line0, col, &encoding);
            let end =
                source_position_to_lsp(&text, line0, col + value.chars().count() as u32, &encoding);
            links.push(DocumentLink {
                range: Range { start, end },
                target: Some(crate::paths::parse_uri(
                    crate::paths::path_to_uri(&target),
                    fallback,
                )),
                tooltip: None,
                data: None,
            });
        }
        if links.is_empty() {
            Ok(None)
        } else {
            Ok(Some(links))
        }
    }
}

/// The invariants of one file's walk, threaded by reference through the
/// recursion (mirrors semantic.rs's `Ctx`/`RuleCtx`, minus token state).
struct LinkWalk<'a> {
    ast: &'a ParsedFile,
    table: &'a StringTable,
    prepared: &'a Prepared<'a>,
    ruleset: &'a RuleSet,
    logical_path: &'a str,
}

impl LinkWalk<'_> {
    /// Walk `children`, recording every String-valued leaf whose matched rule
    /// right-hand side is a filepath or icon field. Rule descent mirrors
    /// semantic tokens: one bootstrap per top-level entity, matched `NodeRule`
    /// bodies below.
    fn collect(
        &self,
        children: &[Child],
        block_rules: Option<&[(RuleType, Options)]>,
        out: &mut Vec<LinkCandidate>,
    ) {
        for child in children {
            match child {
                Child::Comment(_) => {}
                Child::LeafValue(idx) => {
                    let lv = &self.ast.arena.leaf_values[*idx as usize];
                    if let Value::Clause(inner) = &lv.value {
                        let inner_rules = block_rules.map(valueclause_bodies);
                        self.collect(inner, inner_rules.as_deref(), out);
                    }
                }
                Child::Leaf(idx) => {
                    let leaf = &self.ast.arena.leaves[*idx as usize];
                    let raw_key = self.table.get_string(leaf.key.normal).unwrap_or_default();
                    let key = raw_key.trim_matches('"');
                    let matched = match block_rules {
                        Some(rules) => {
                            value_rules_for_key(self.ruleset, self.prepared.type_index, rules, key)
                        }
                        None => Vec::new(),
                    };
                    if let Value::Clause(inner) = &leaf.value {
                        let inner_rules = match block_rules {
                            None => {
                                block_rules_for(self.ast, self.prepared, self.logical_path, inner)
                            }
                            Some(_) => Some(node_bodies(&matched)),
                        };
                        self.collect(inner, inner_rules.as_deref(), out);
                        continue;
                    }
                    let raw_value = match &leaf.value {
                        Value::String(t) | Value::QString(t) => {
                            self.table.get_string(t.normal).unwrap_or_default()
                        }
                        _ => continue,
                    };
                    let value = unquote(&raw_value);
                    let Some(rel_path) = matched.iter().find_map(|(rt, _)| {
                        let RuleType::LeafRule { right, .. } = rt else {
                            return None;
                        };
                        match right {
                            NewField::FilepathField { prefix, extension } => {
                                filepath_link_candidate(
                                    prefix.as_deref(),
                                    extension.as_deref(),
                                    value,
                                )
                            }
                            NewField::IconField(folder) => icon_link_candidate(folder, value),
                            _ => None,
                        }
                    }) else {
                        continue;
                    };
                    out.push(LinkCandidate {
                        key_line: leaf.pos.start.line,
                        key_col: leaf.pos.start.col,
                        raw_value: raw_value.clone(),
                        rel_path,
                    });
                }
            }
        }
    }
}

/// The game-relative path a `filepath[prefix,ext]` field's `value` references:
/// the configured extension is appended unless already present, the prefix
/// prepended unless the value already starts with it (mirrors the CW113
/// existence check).
fn filepath_link_candidate(
    prefix: Option<&str>,
    extension: Option<&str>,
    value: &str,
) -> Option<String> {
    let value = value.trim();
    // Dynamic / templated paths can't resolve statically.
    if value.is_empty() || value.contains('$') || value.contains('[') || value.contains('<') {
        return None;
    }
    let mut rel = value.to_string();
    if let Some(ext) = extension
        && !ext.is_empty()
        && !rel
            .to_ascii_lowercase()
            .ends_with(&ext.to_ascii_lowercase())
    {
        rel.push_str(ext);
    }
    match prefix {
        Some(p)
            if !value
                .to_ascii_lowercase()
                .starts_with(&p.to_ascii_lowercase()) =>
        {
            Some(format!("{}{}", p, rel))
        }
        _ => Some(rel),
    }
}

/// The game-relative path an `icon[folder]` field's `value` references:
/// `folder/value.dds`.
fn icon_link_candidate(folder: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.contains('$') || value.contains('[') || value.contains('<') {
        return None;
    }
    Some(format!("{}/{}.dds", folder.trim_end_matches('/'), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filepath_candidate_applies_prefix_and_extension() {
        assert_eq!(
            filepath_link_candidate(Some("gfx/"), Some(".dds"), "pic"),
            Some("gfx/pic.dds".to_string())
        );
        // Extension already present: not doubled.
        assert_eq!(
            filepath_link_candidate(Some("gfx/"), Some(".dds"), "pic.dds"),
            Some("gfx/pic.dds".to_string())
        );
        // Value already carries the prefix: not doubled.
        assert_eq!(
            filepath_link_candidate(Some("gfx/"), Some(".dds"), "gfx/pic.dds"),
            Some("gfx/pic.dds".to_string())
        );
        // Bare filepath field: the value is the path.
        assert_eq!(
            filepath_link_candidate(None, None, "gfx/pic.dds"),
            Some("gfx/pic.dds".to_string())
        );
        // Dynamic/templated paths can't resolve statically.
        assert_eq!(filepath_link_candidate(None, None, "gfx/$NAME$.dds"), None);
        assert_eq!(filepath_link_candidate(None, None, ""), None);
    }

    /// The probe the request runs on each candidate: the same trim and
    /// containment call `document_link_impl` makes, so a leaf value that climbs
    /// out of the roots links to nothing instead of reporting the file exists
    /// (#176).
    #[tokio::test]
    async fn a_candidate_that_climbs_out_of_the_roots_links_to_nothing() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path().join("mod");
        std::fs::create_dir_all(root.join("gfx")).unwrap();
        std::fs::write(root.join("gfx/pic.dds"), "x").unwrap();
        std::fs::write(tmp.path().join("secret.dds"), "secret").unwrap();
        let roots = vec![root.clone()];

        let escaping = filepath_link_candidate(Some("gfx/"), Some(".dds"), "gfx/../../secret")
            .expect("a candidate");
        assert_eq!(probe(&roots, &escaping).await, None);

        let inside = filepath_link_candidate(Some("gfx/"), Some(".dds"), "pic").expect("candidate");
        assert_eq!(probe(&roots, &inside).await, Some(root.join("gfx/pic.dds")));
    }

    async fn probe(roots: &[std::path::PathBuf], rel_path: &str) -> Option<std::path::PathBuf> {
        let rel = std::path::Path::new(rel_path.trim_start_matches(['/', '\\']));
        crate::access::contained_search_path(roots, rel).await
    }

    #[test]
    fn icon_candidate_joins_folder_and_dds() {
        assert_eq!(
            icon_link_candidate("gfx/interface", "myicon"),
            Some("gfx/interface/myicon.dds".to_string())
        );
        assert_eq!(icon_link_candidate("gfx/interface", ""), None);
    }
}
