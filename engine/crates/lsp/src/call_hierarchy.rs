use std::collections::HashMap;

use cwtools_info::{IndexedUseSite, SourceLocation, TypeInstance};
use serde_json::json;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::lines::{DocLines, index_snapshots};
use crate::paths::{logical_path_from_uri, parse_uri};

#[derive(Clone)]
struct Callable {
    uri: String,
    type_name: String,
    name: String,
    location: SourceLocation,
}

impl Callable {
    fn new(uri: &str, type_name: &str, instance: &TypeInstance) -> Self {
        Self {
            uri: uri.to_string(),
            type_name: type_name.to_string(),
            name: instance.name.clone(),
            location: instance.location,
        }
    }

    fn key(&self) -> (&str, &str, u32, u16) {
        (
            &self.uri,
            &self.type_name,
            self.location.line,
            self.location.col,
        )
    }
}

fn is_callable_type(name: &str) -> bool {
    matches!(name, "scripted_effect" | "scripted_trigger")
}

fn in_definition(location: SourceLocation, line: u32, col: u32) -> bool {
    (line, col) >= (location.line, u32::from(location.col))
        && (line, col) < (location.end.0, u32::from(location.end.1))
}

/// `type_per_file` instances carry this empty location and own their whole file.
fn is_whole_file(location: SourceLocation) -> bool {
    location.line == 1 && location.col == 0 && location.end == (1, 0)
}

/// The innermost definition containing a site, as the graph's owner lookup picks it:
/// a focus inside its focus tree is the focus. A whole-file instance owns the rest.
fn owner_of<'a>(
    instances: &[(&'a str, &'a TypeInstance)],
    site: SourceLocation,
) -> Option<(&'a str, &'a TypeInstance)> {
    instances
        .iter()
        .filter(|(_, instance)| in_definition(instance.location, site.line, u32::from(site.col)))
        .max_by_key(|(type_name, instance)| {
            (instance.location.line, instance.location.col, *type_name)
        })
        .or_else(|| {
            instances
                .iter()
                .find(|(_, instance)| is_whole_file(instance.location))
        })
        .copied()
}

/// One entry per distinct callable with every one of its ranges, in definition order.
fn group_calls(mut edges: Vec<(Callable, Range)>) -> Vec<(Callable, Vec<Range>)> {
    edges.sort_by(|(a, a_range), (b, b_range)| {
        a.key()
            .cmp(&b.key())
            .then(a_range.start.cmp(&b_range.start))
    });
    edges
        .chunk_by(|(a, _), (b, _)| a.key() == b.key())
        .map(|group| {
            let ranges = group.iter().map(|(_, range)| *range).collect();
            (group[0].0.clone(), ranges)
        })
        .collect()
}

impl Backend {
    pub(crate) async fn prepare_call_hierarchy_impl(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let TextDocumentPositionParams {
            text_document,
            position,
        } = params.text_document_position_params;
        let uri = text_document.uri.to_string();
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);
        let called = self
            .type_ref_at_cursor(&uri, position, &logical_path)
            .filter(|(type_name, _)| is_callable_type(type_name));
        let callables: Vec<Callable> = if let Some((type_name, name)) = called {
            let info = self.state.info_service.read();
            info.type_index
                .instances(&type_name)
                .iter()
                .filter(|(_, instance)| instance.name == name)
                .map(|(file, instance)| Callable::new(file, &type_name, instance))
                .collect()
        } else {
            let text = self.file_text_for(&uri).await;
            let col = text
                .as_deref()
                .map_or(position.character.min(u32::from(u16::MAX)), |text| {
                    let lines = DocLines::new(text, self.position_encoding());
                    u32::from(lines.source_column(position))
                });
            let on_key = |instance: &TypeInstance| {
                let start = u32::from(instance.location.col);
                let end = start + instance.name.chars().count() as u32;
                instance.location.line == position.line.saturating_add(1)
                    && (start..=end).contains(&col)
            };
            let info = self.state.info_service.read();
            info.type_index
                .instances_in_file(&uri)
                .into_iter()
                .filter(|(type_name, instance)| is_callable_type(type_name) && on_key(instance))
                .map(|(type_name, instance)| Callable::new(&uri, type_name, instance))
                .collect()
        };
        if callables.is_empty() {
            return Ok(None);
        }
        let uris: Vec<String> = callables
            .iter()
            .map(|callable| callable.uri.clone())
            .collect();
        let texts = self.file_text_snapshots_for_navigation(&uris).await?;
        let lines = index_snapshots(&texts, &self.position_encoding());
        let items = callables
            .iter()
            .map(|callable| {
                let lines = lines.get(callable.uri.as_str());
                self.callable_item(callable, &text_document.uri, lines)
            })
            .collect();
        Ok(Some(items))
    }

    pub(crate) async fn incoming_calls_impl(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let item = params.item;
        let Some(target) = self.item_callable(&item) else {
            return Ok(None);
        };
        let sites = self.collect_use_sites(&target.type_name, &target.name);
        let edges = {
            let info = self.state.info_service.read();
            let mut by_file: HashMap<&str, Vec<(&str, &TypeInstance)>> = HashMap::new();
            let mut edges = Vec::new();
            for site in &sites {
                let instances = by_file
                    .entry(&site.file)
                    .or_insert_with(|| info.type_index.instances_in_file(&site.file));
                if let Some((type_name, owner)) = owner_of(instances, site.key) {
                    edges.push((Callable::new(&site.file, type_name, owner), site.value));
                }
            }
            edges
        };
        let uris: Vec<String> = edges.iter().map(|(caller, _)| caller.uri.clone()).collect();
        let texts = self.file_text_snapshots_for_navigation(&uris).await?;
        let lines = index_snapshots(&texts, &self.position_encoding());
        let edges = edges
            .into_iter()
            .map(|(caller, value)| {
                let range = self.source_range_with_lines(
                    lines.get(caller.uri.as_str()),
                    value.line.saturating_sub(1),
                    u32::from(value.col),
                    &target.name,
                );
                (caller, range)
            })
            .collect();
        let calls = group_calls(edges)
            .into_iter()
            .map(|(caller, from_ranges)| CallHierarchyIncomingCall {
                from: self.callable_item(&caller, &item.uri, lines.get(caller.uri.as_str())),
                from_ranges,
            })
            .collect();
        Ok(Some(calls))
    }

    pub(crate) async fn outgoing_calls_impl(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let item = params.item;
        let Some(owner) = self.item_callable(&item) else {
            return Ok(None);
        };
        let sites = self.file_use_sites(&owner.uri);
        let edges = {
            let info = self.state.info_service.read();
            let mut edges = Vec::new();
            for use_site in &sites {
                let key = use_site.site.key;
                if !is_callable_type(&use_site.type_name)
                    || !in_definition(owner.location, key.line, u32::from(key.col))
                {
                    continue;
                }
                for (uri, callee) in info.type_index.instances(&use_site.type_name) {
                    if callee.name.eq_ignore_ascii_case(&use_site.name) {
                        let callee = Callable::new(uri, &use_site.type_name, callee);
                        edges.push((callee, use_site.site.value));
                    }
                }
            }
            edges
        };
        let uris: Vec<String> = std::iter::once(owner.uri.clone())
            .chain(edges.iter().map(|(callee, _)| callee.uri.clone()))
            .collect();
        let texts = self.file_text_snapshots_for_navigation(&uris).await?;
        let lines = index_snapshots(&texts, &self.position_encoding());
        let edges = edges
            .into_iter()
            .map(|(callee, value)| {
                let range = self.source_range_with_lines(
                    lines.get(owner.uri.as_str()),
                    value.line.saturating_sub(1),
                    u32::from(value.col),
                    &callee.name,
                );
                (callee, range)
            })
            .collect();
        let calls = group_calls(edges)
            .into_iter()
            .map(|(callee, from_ranges)| CallHierarchyOutgoingCall {
                to: self.callable_item(&callee, &item.uri, lines.get(callee.uri.as_str())),
                from_ranges,
            })
            .collect();
        Ok(Some(calls))
    }

    /// The definition an item was prepared from, found through the index
    /// spelling it carries rather than the client's `uri`.
    fn item_callable(&self, item: &CallHierarchyItem) -> Option<Callable> {
        let data = item.data.as_ref()?.as_object()?;
        let uri = data.get("uri")?.as_str()?;
        let type_name = data.get("typeName")?.as_str()?;
        let line = u32::try_from(data.get("line")?.as_u64()?).ok()?;
        let column = u16::try_from(data.get("column")?.as_u64()?).ok()?;
        let info = self.state.info_service.read();
        let instance = info
            .type_index
            .instance_at(uri, type_name, &item.name, line, column)?;
        Some(Callable::new(uri, type_name, instance))
    }

    fn callable_item(
        &self,
        callable: &Callable,
        fallback: &Url,
        lines: Option<&DocLines<'_>>,
    ) -> CallHierarchyItem {
        let Callable {
            uri,
            type_name,
            name,
            location,
        } = callable;
        let range = lines.map_or_else(
            || {
                Range::new(
                    Position::new(location.line.saturating_sub(1), u32::from(location.col)),
                    Position::new(location.end.0.saturating_sub(1), u32::from(location.end.1)),
                )
            },
            |lines| lines.related_range(location.line, location.col, location.end),
        );
        let selection_range = self.source_range_with_lines(
            lines,
            location.line.saturating_sub(1),
            u32::from(location.col),
            name,
        );
        CallHierarchyItem {
            name: name.clone(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            detail: Some(type_name.clone()),
            uri: parse_uri(uri, fallback),
            range,
            selection_range,
            data: Some(json!({
                "uri": uri,
                "typeName": type_name,
                "line": location.line,
                "column": location.col,
            })),
        }
    }

    /// The references made from `uri`: an open buffer's accepted AST, else the index.
    fn file_use_sites(&self, uri: &str) -> Vec<IndexedUseSite> {
        let open_ast = self
            .state
            .documents
            .lock()
            .get(uri)
            .filter(|doc| doc.ast_version == Some(doc.version))
            .and_then(|doc| doc.ast.clone());
        open_ast
            .and_then(|ast| self.reference_sites_for_open_ast(uri, &ast))
            .unwrap_or_else(|| self.state.info_service.read().file_reference_sites(uri))
    }
}
