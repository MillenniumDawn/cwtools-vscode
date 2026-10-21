use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::lines::DocLines;
use crate::navigation::source_range_without_text;
use crate::paths::parse_uri;

fn is_scripted_path(uri: &str) -> bool {
    let path = uri.to_ascii_lowercase();
    path.contains("/common/scripted_effects/") || path.contains("/common/scripted_triggers/")
}

fn is_callable_type(name: &str) -> bool {
    matches!(name, "scripted_effect" | "scripted_trigger")
}

fn in_definition(location: cwtools_info::SourceLocation, line: u32, col: u32) -> bool {
    (line, col) >= (location.line, u32::from(location.col))
        && (line, col) < (location.end.0, u32::from(location.end.1))
}

impl Backend {
    fn call_hierarchy_item(
        &self,
        uri: &str,
        type_name: &str,
        name: &str,
        location: cwtools_info::SourceLocation,
        fallback: &Url,
        lines: Option<&DocLines<'_>>,
    ) -> CallHierarchyItem {
        let range = lines.map_or_else(
            || {
                Range::new(
                    Position::new(location.line.saturating_sub(1), u32::from(location.col)),
                    Position::new(location.end.0.saturating_sub(1), u32::from(location.end.1)),
                )
            },
            |lines| lines.related_range(location.line, location.col, location.end),
        );
        let selection_range = lines.map_or_else(
            || {
                source_range_without_text(
                    location.line.saturating_sub(1),
                    u32::from(location.col),
                    name,
                    &self.position_encoding(),
                )
            },
            |lines| {
                lines.token_range(
                    location.line.saturating_sub(1),
                    u32::from(location.col),
                    name,
                )
            },
        );
        CallHierarchyItem {
            name: name.to_string(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            detail: Some(type_name.to_string()),
            uri: parse_uri(uri, fallback),
            range,
            selection_range,
            data: None,
        }
    }

    fn call_site_range(
        &self,
        location: cwtools_info::SourceLocation,
        name: &str,
        lines: Option<&DocLines<'_>>,
    ) -> Range {
        lines.map_or_else(
            || {
                source_range_without_text(
                    location.line.saturating_sub(1),
                    u32::from(location.col),
                    name,
                    &self.position_encoding(),
                )
            },
            |lines| {
                lines.token_range(
                    location.line.saturating_sub(1),
                    u32::from(location.col),
                    name,
                )
            },
        )
    }

    pub(crate) async fn prepare_call_hierarchy_impl(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let text_doc = &params.text_document_position_params.text_document;
        let uri = text_doc.uri.to_string();
        let pos = params.text_document_position_params.position;
        if !is_scripted_path(&uri) {
            return Ok(None);
        }
        let text = self.file_text_for(&uri).await;
        let lines = text
            .as_deref()
            .map(|text| DocLines::new(text, self.position_encoding()));
        let source_col = lines
            .as_ref()
            .map(|lines| u32::from(lines.source_column(pos)))
            .unwrap_or(pos.character.min(u16::MAX as u32));
        let found = {
            let info = self.state.info_service.read();
            info.type_index
                .instances_in_file(&uri)
                .into_iter()
                .find(|(type_name, inst)| {
                    is_callable_type(type_name)
                        && in_definition(inst.location, pos.line.saturating_add(1), source_col)
                })
                .map(|(type_name, inst)| (type_name.to_string(), inst.name.clone(), inst.location))
        };
        let Some((type_name, name, location)) = found else {
            return Ok(None);
        };
        Ok(Some(vec![self.call_hierarchy_item(
            &uri,
            &type_name,
            &name,
            location,
            &text_doc.uri,
            lines.as_ref(),
        )]))
    }

    pub(crate) async fn incoming_calls_impl(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let item = params.item;
        let target_type = item.detail.clone().unwrap_or_default();
        if target_type.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let sites = self.collect_use_sites(&target_type, &item.name);
        let uris = sites
            .iter()
            .map(|site| site.file.to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let snapshots = self.file_text_snapshots_for(&uris).await;
        let lines = crate::lines::index_snapshots(&snapshots, &self.position_encoding());
        let mut calls = Vec::new();
        for site in sites {
            let owner = {
                let info = self.state.info_service.read();
                info.type_index
                    .instances_in_file(&site.file)
                    .into_iter()
                    .find(|(_, inst)| {
                        in_definition(inst.location, site.key.line, u32::from(site.key.col))
                    })
                    .map(|(owner_type, inst)| (owner_type.to_string(), inst.clone()))
            };
            let Some((owner_type, owner)) = owner else {
                continue;
            };
            calls.push(CallHierarchyIncomingCall {
                from: self.call_hierarchy_item(
                    &site.file,
                    &owner_type,
                    &owner.name,
                    owner.location,
                    &item.uri,
                    lines.get(site.file.as_ref()),
                ),
                from_ranges: vec![self.call_site_range(
                    site.key,
                    &item.name,
                    lines.get(site.file.as_ref()),
                )],
            });
        }
        Ok(Some(calls))
    }

    pub(crate) async fn outgoing_calls_impl(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let item = params.item;
        let owner = {
            let info = self.state.info_service.read();
            info.type_index
                .instances_in_file(item.uri.as_str())
                .into_iter()
                .find(|(_, inst)| inst.name == item.name)
                .map(|(type_name, inst)| (type_name.to_string(), inst.clone()))
        };
        let Some((_owner_type, owner)) = owner else {
            return Ok(Some(Vec::new()));
        };
        let targets = {
            let info = self.state.info_service.read();
            info.type_index
                .map()
                .iter()
                .flat_map(|(type_name, entries)| {
                    entries
                        .iter()
                        .filter(move |(uri, _)| {
                            is_callable_type(type_name) && is_scripted_path(uri)
                        })
                        .map(move |(uri, inst)| (type_name.clone(), uri.to_string(), inst.clone()))
                })
                .collect::<Vec<_>>()
        };
        let mut edges = Vec::new();
        for (target_type, target_uri, target) in targets {
            for site in self.collect_use_sites(&target_type, &target.name) {
                if site.file.as_ref() != item.uri.as_str()
                    || !in_definition(owner.location, site.key.line, u32::from(site.key.col))
                {
                    continue;
                }
                edges.push((
                    target_type.clone(),
                    target_uri.clone(),
                    target.name.clone(),
                    target.location,
                    site.key,
                ));
            }
        }
        edges.sort_by_key(|(_, _, _, _, site)| (site.line, site.col));
        let mut uri_set = edges
            .iter()
            .map(|(_, uri, _, _, _)| uri.clone())
            .collect::<std::collections::HashSet<_>>();
        uri_set.insert(item.uri.to_string());
        let uris = uri_set.into_iter().collect::<Vec<_>>();
        let snapshots = self.file_text_snapshots_for(&uris).await;
        let lines = crate::lines::index_snapshots(&snapshots, &self.position_encoding());
        let mut calls = Vec::with_capacity(edges.len());
        for (target_type, target_uri, target_name, target_location, site) in edges {
            calls.push(CallHierarchyOutgoingCall {
                to: self.call_hierarchy_item(
                    &target_uri,
                    &target_type,
                    &target_name,
                    target_location,
                    &item.uri,
                    lines.get(target_uri.as_str()),
                ),
                from_ranges: vec![self.call_site_range(
                    site,
                    &target_name,
                    lines.get(item.uri.as_str()),
                )],
            });
        }
        Ok(Some(calls))
    }
}
