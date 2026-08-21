use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_info::PositionElement;
use cwtools_info::ReferenceHint;

use crate::paths::{logical_path_from_uri, lsp_pos_to_source_in_text, parse_uri};
use crate::{Backend, RuleCursorInfo};

use super::{
    cwt_ref_at, dedup_locations, locations_at, member_pos_in_block, resolve_file_ref, unquote,
};

impl Backend {
    pub(crate) fn type_ref_at_cursor(
        &self,
        uri: &str,
        pos: tower_lsp::lsp_types::Position,
        logical_path: &str,
    ) -> Option<(String, String)> {
        match self.rule_info_at_cursor(uri, pos, logical_path) {
            Some(RuleCursorInfo {
                hint: ReferenceHint::TypeRef { type_name, value },
                ..
            }) => Some((type_name, unquote(&value).to_string())),
            _ => None,
        }
    }

    /// Goto for a `$KEY$` reference in a `.yml` loc file: jump to the entry the
    /// key names. `None` when the cursor isn't on a known loc-key reference.
    async fn loc_ref_goto(
        &self,
        uri: &str,
        pos: Position,
        fallback: &Url,
    ) -> Option<GotoDefinitionResponse> {
        let (key, _, _) = self.loc_ref_at_cursor_doc(uri, pos)?;
        let key = key.to_lowercase();
        let target = {
            let map = self.state.loc_locations.read();
            map.get(key.as_str()).cloned()
        }?;
        let text = self.file_text_for(target.0.as_ref()).await;
        Some(GotoDefinitionResponse::Array(vec![
            self.source_location_with_text(
                target.0.as_ref(),
                target.1,
                0,
                &key,
                fallback,
                text.as_deref(),
            ),
        ]))
    }

    pub(crate) async fn goto_definition_impl(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let pos = params.text_document_position_params.position;
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();

        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);
        let fallback = &params.text_document_position_params.text_document.uri;

        // Localisation file: goto on a `$KEY$` reference jumps to the loc entry
        // it names. .yml isn't a game AST, so handle it before the rule walk.
        if crate::paths::is_loc_file(&uri) {
            return Ok(self.loc_ref_goto(&uri, pos, fallback).await);
        }

        // `.cwt` rule file: a `<type>` / `enum[..]` / `single_alias_right[..]`
        // reference jumps to its definition in the loaded rules folder.
        if crate::paths::is_cwt_file(&uri) {
            return Ok(self.cwt_goto(&uri, pos, fallback).await);
        }

        // Rule-aware lookup via the position resolver. The classified hint tells
        // us how to find the definition; mirror the kinds hover handles.
        if let Some(info) = self.rule_info_at_cursor(&uri, pos, &logical_path) {
            let locations = match &info.hint {
                ReferenceHint::TypeRef { type_name, value } => {
                    let value = unquote(value);
                    let defs = {
                        let svc = self.state.info_service.read();
                        svc.type_index
                            .instances(type_name)
                            .iter()
                            .filter(|(_, inst)| inst.name == value)
                            .map(|(file_uri, inst)| (file_uri.to_string(), inst.location))
                            .collect::<Vec<_>>()
                    };
                    locations_at(self, defs, value, fallback).await
                }
                ReferenceHint::Variable { name, .. } => {
                    let defs = {
                        let svc = self.state.info_service.read();
                        svc.find_variable_definitions(name)
                    };
                    locations_at(self, defs, name, fallback).await
                }
                ReferenceHint::LocRef { key } => {
                    let key = key.to_lowercase();
                    let target = {
                        let map = self.state.loc_locations.read();
                        map.get(key.as_str()).cloned()
                    };
                    if let Some((file_uri, line)) = target {
                        let text = self.file_text_for(file_uri.as_ref()).await;
                        vec![self.source_location_with_text(
                            file_uri.as_ref(),
                            line,
                            0,
                            &key,
                            fallback,
                            text.as_deref(),
                        )]
                    } else {
                        Vec::new()
                    }
                }
                ReferenceHint::FileRef { path } => self.file_ref_locations(path, fallback).await,
                // An enum member has no definition of its own in the game
                // files; the config is where it comes from, so jump to the
                // member inside its `enum[..]` block.
                ReferenceHint::EnumRef { enum_name, value } => self
                    .cwt_def_location(
                        cwtools_rules::rules_types::CwtDefKind::Enum,
                        enum_name,
                        Some(unquote(value)),
                        fallback,
                    )
                    .await
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            };
            let locations = dedup_locations(locations);
            if !locations.is_empty() {
                return Ok(Some(GotoDefinitionResponse::Array(locations)));
            }
        }

        // Fallback: heuristic symbol-based lookup. Try the leaf VALUE before the
        // key — an event/decision reference like `id = some.1` or
        // `trigger_event = some.1` resolves by its dotted id (the instance name),
        // which the rule-aware path misses when the field is typed `scalar`. The
        // key is tried second so a definition node (e.g. `decision = { … }`)
        // still resolves. (#39)
        if let Some(element) = self.element_at_cursor(&uri, pos) {
            let candidates: Vec<String> = match &element {
                PositionElement::Leaf { key, value } if !value.is_empty() => {
                    vec![unquote(value).to_string(), key.clone()]
                }
                PositionElement::Leaf { key, .. } => vec![key.clone()],
                PositionElement::LeafValue { value } => vec![unquote(value).to_string()],
            };
            let candidates_with_locations = {
                let info = self.state.info_service.read();
                candidates
                    .iter()
                    .map(|symbol| {
                        // Type-instance index first: events/decisions are keyed by id.
                        let instances = info
                            .type_index
                            .instance_locations(symbol)
                            .into_iter()
                            .map(|(uri, loc)| (uri.to_string(), loc))
                            .collect::<Vec<_>>();
                        let definitions = if instances.is_empty() {
                            info.find_definitions(symbol).cloned().unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        (symbol.clone(), instances, definitions)
                    })
                    .collect::<Vec<_>>()
            };
            for (symbol, instances, definitions) in candidates_with_locations {
                let pairs = if instances.is_empty() {
                    definitions
                } else {
                    instances
                };
                let locations = dedup_locations(locations_at(self, pairs, &symbol, fallback).await);
                if !locations.is_empty() {
                    return Ok(Some(GotoDefinitionResponse::Array(locations)));
                }
            }
        }
        Ok(None)
    }

    /// Resolve a `FilepathField` reference (a game-relative path like
    /// `gfx/…/foo.dds`) to a file Location by probing the workspace root, then
    /// the configured vanilla install. Returns an empty Vec when nothing exists
    /// inside either of them.
    async fn file_ref_locations(&self, path: &str, fallback: &Url) -> Vec<Location> {
        resolve_file_ref(&self.search_roots(), path)
            .await
            .map(|candidate| Location {
                uri: parse_uri(crate::paths::path_to_uri(&candidate), fallback),
                range: Range::default(),
            })
            .into_iter()
            .collect()
    }

    /// The classified `.cwt` reference under the cursor, read from the line
    /// text (rule files aren't game ASTs, so no rule walk).
    pub(crate) async fn cwt_ref_at_cursor(
        &self,
        uri: &str,
        pos: Position,
    ) -> Option<(cwtools_rules::rules_types::CwtDefKind, String)> {
        let text = self.file_text_for(uri).await?;
        let encoding = self.state.config.read().position_encoding.clone();
        let (_, col) = lsp_pos_to_source_in_text(&text, pos, &encoding);
        let line = text.lines().nth(pos.line as usize)?;
        cwt_ref_at(line, col as u32)
    }

    /// Goto inside a `.cwt`: jump to the referenced definition recorded by the
    /// ruleset loader. `None` when the cursor isn't on a resolvable reference.
    async fn cwt_goto(
        &self,
        uri: &str,
        pos: Position,
        fallback: &Url,
    ) -> Option<GotoDefinitionResponse> {
        let (kind, name) = self.cwt_ref_at_cursor(uri, pos).await?;
        let loc = self.cwt_def_location(kind, &name, None, fallback).await?;
        Some(GotoDefinitionResponse::Array(vec![loc]))
    }

    /// Where the ruleset loader recorded `name`'s definition, as a Location.
    /// `member` anchors on that token inside the definition's block (an enum
    /// value in its `enum[..]` body), falling back to the defining key when the
    /// block doesn't spell it out — a complex enum's members come from the game
    /// files, not the config. `None` when nothing defines `name`.
    async fn cwt_def_location(
        &self,
        kind: cwtools_rules::rules_types::CwtDefKind,
        name: &str,
        member: Option<&str>,
        fallback: &Url,
    ) -> Option<Location> {
        let def = {
            let rules = self.state.rules.read();
            let rs = rules.ruleset.as_ref()?;
            rs.def_positions
                .iter()
                .find(|d| d.kind == kind && d.name == name)
                .cloned()
        }?;
        let target_uri = crate::paths::path_to_uri(&def.file);
        let text = self.file_text_for(&target_uri).await;
        let line0 = def.line.saturating_sub(1);
        let (line0, col, token) = member
            .zip(text.as_deref())
            .and_then(|(m, text)| {
                let (line, col) = member_pos_in_block(text, line0, def.col as u32, m)?;
                Some((line, col, m))
            })
            .unwrap_or((line0, def.col as u32, name));
        Some(self.source_location_with_text(
            &target_uri,
            line0,
            col,
            token,
            fallback,
            text.as_deref(),
        ))
    }

    /// The roots a game-relative path resolves against, in probe order: the
    /// workspace, then the configured vanilla install.
    pub(crate) fn search_roots(&self) -> Vec<std::path::PathBuf> {
        let (ws_uri, vanilla_dir) = {
            let cfg = self.state.config.read();
            (cfg.workspace_uri.clone(), cfg.vanilla_dir.clone())
        };
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        if let Some(ws) = ws_uri
            && let Ok(url) = Url::parse(&ws)
            && url.scheme() == "file"
            && let Ok(path) = url.to_file_path()
        {
            roots.push(path);
        }
        if let Some(v) = vanilla_dir {
            roots.push(v);
        }
        roots
    }
}
