use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_parser::ast::{Child, ParsedFile, SourceRange, Value};
use cwtools_rules::rules_types::{NewField, RuleType, TypeDefinition};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::position::{FilePositionResolver, rules_at_pos_with_resolver};

use crate::Backend;
use crate::lines::{DocLines, index_snapshots};
use crate::paths::logical_path_from_uri;

use super::{
    at_var_at_cursor, code_token_cols_in_line, prepare_rename_range, rename_refused,
    word_at_position,
};
use crate::navigation::helpers::{REQUEST_FAILED, TokenCase, loc_ref_key_cols_in_line, loc_root};

struct RenameRequestSnapshot {
    generation: u64,
    open_documents: HashMap<String, (i32, Option<u64>)>,
}

enum RenameNameKind {
    Constant,
    Localisation,
    Type,
}

#[derive(Default)]
struct LocRenameScan {
    edits: Vec<(String, Vec<TextEdit>)>,
    entities: HashMap<(String, String), String>,
    keys: HashMap<String, String>,
}

fn rename_error(message: impl Into<String>) -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED),
        message: format!("Rename cancelled: {}", message.into()).into(),
        data: None,
    }
}

fn add_rename<K: Eq + std::hash::Hash>(
    targets: &mut HashMap<K, String>,
    key: K,
    replacement: String,
) -> Result<bool> {
    match targets.entry(key) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(replacement);
            Ok(true)
        }
        std::collections::hash_map::Entry::Occupied(entry) if entry.get() == &replacement => {
            Ok(false)
        }
        _ => Err(rename_error(
            "coupled symbols require conflicting replacement names",
        )),
    }
}

fn definition_span(
    ast: &ParsedFile,
    table: &StringTable,
    td: &TypeDefinition,
    instance: &cwtools_info::TypeInstance,
) -> Option<SourceRange> {
    if td.type_per_file {
        return None;
    }
    let node = ast.arena.leaves.iter().find(|leaf| {
        leaf.pos.start.line == instance.location.line && leaf.pos.start.col == instance.location.col
    })?;
    if let Some(field) = &td.name_field {
        let Value::Clause(children) = &node.value else {
            return None;
        };
        children.iter().find_map(|child| {
            let Child::Leaf(idx) = child else {
                return None;
            };
            let leaf = &ast.arena.leaves[*idx as usize];
            (table
                .get_string(leaf.key.normal)?
                .eq_ignore_ascii_case(field)
                && cwtools_info::leaf_value_string(&leaf.value, table)
                    .trim_matches('"')
                    .eq_ignore_ascii_case(&instance.name))
            .then_some(leaf.value_pos)
        })
    } else {
        let key = table.get_string(node.key.normal)?;
        key.trim_matches('"')
            .eq_ignore_ascii_case(&instance.name)
            .then_some(SourceRange {
                start: node.pos.start,
                end: cwtools_parser::ast::SourcePos {
                    line: node.pos.start.line,
                    col: node
                        .pos
                        .start
                        .col
                        .saturating_add(key.chars().count() as u16),
                },
            })
    }
}

fn loc_token_range(lines: &DocLines<'_>, span: SourceRange, raw: &str) -> Range {
    let line = span.start.line.saturating_sub(1);
    let quoted = lines.line(line).chars().nth(usize::from(span.start.col)) == Some('"');
    let col = u32::from(span.start.col) + u32::from(quoted);
    lines.token_range(line, col, raw.trim_matches('"'))
}

fn validate_replacement_name(name: &str, kind: RenameNameKind) -> Result<()> {
    let (valid, expected) = match kind {
        RenameNameKind::Constant => (
            name.strip_prefix('@').is_some_and(|name| {
                !name.is_empty() && name.chars().all(super::helpers::is_ident_char)
            }),
            "an @constant with a non-empty identifier after @",
        ),
        RenameNameKind::Localisation => (
            !name.is_empty()
                && name
                    .chars()
                    .all(cwtools_localization::is_valid_loc_key_char),
            "a localisation key containing letters, digits, underscores, dots or hyphens",
        ),
        RenameNameKind::Type => (
            valid_type_replacement(name),
            "one script identifier or a complete quoted name",
        ),
    };
    if valid {
        Ok(())
    } else {
        Err(tower_lsp::jsonrpc::Error {
            code: tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED),
            message: format!("Rename cancelled: replacement name must be {expected}").into(),
            data: None,
        })
    }
}

fn valid_type_replacement(name: &str) -> bool {
    use cwtools_parser::ast::{Child, Operator, Value};
    if name.is_empty()
        || name == "\"\""
        || cwtools_parser::unquote(name).starts_with(['@', '['])
        || name.contains('$')
        || name.chars().any(char::is_control)
    {
        return false;
    }
    // A type name must fit both a definition key and a reference value. Let
    // the script parser check both grammars, including complete quoted forms.
    let table = cwtools_string_table::string_table::StringTable::new();
    let parsed = cwtools_parser::parser::parse_string(&format!("{name} = {name}"), &table);
    let [Child::Leaf(idx)] = parsed.root_children.as_slice() else {
        return false;
    };
    let leaf = &parsed.arena.leaves[*idx as usize];
    let len = name.chars().count();
    parsed.errors.is_empty()
        && leaf.op == Operator::Equals
        && !matches!(leaf.value, Value::Clause(_))
        && leaf.pos.start.col == 0
        && usize::from(leaf.value_pos.start.col) == len + 3
        && usize::from(leaf.value_pos.end.col) == len * 2 + 3
}

// Definitions point at the opening quote, whereas resolved references point
// at the name inside it. Replace the entire token in either case.
fn type_rename_range(lines: &DocLines, line: u32, col: u32, name: &str) -> Range {
    let chars: Vec<char> = lines.line(line).chars().collect();
    let len = name.chars().count() as u32;
    let body_col = if chars.get(col as usize) == Some(&'"') {
        col + 1
    } else {
        col
    };
    if body_col > 0
        && chars.get(body_col as usize - 1) == Some(&'"')
        && chars.get((body_col + len) as usize) == Some(&'"')
    {
        return Range::new(
            lines.position(line, body_col - 1),
            lines.position(line, body_col + len + 1),
        );
    }
    lines.token_range(line, col, name)
}

impl Backend {
    pub(crate) async fn prepare_rename_impl(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri.to_string();
        let pos = params.position;
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);

        let text = self.file_text_for(&uri).await;
        let position_encoding = self.position_encoding();
        let lines = text
            .as_deref()
            .map(|text| DocLines::new(text, position_encoding.clone()));
        if let (Some(text), Some(lines)) = (text.as_deref(), lines.as_ref())
            && let Some((_, range)) = Self::at_var_rename_target(text, lines, pos)
        {
            return Ok(Some(PrepareRenameResponse::Range(range)));
        }

        if let Some(key_lower) = self
            .loc_rename_key_at_cursor(&uri, pos, &logical_path)
            .await?
        {
            // `lines` is `text.as_deref().map(..)`, so it is `Some` on exactly
            // the same condition and the index answers this without the
            // document rescan `lsp_pos_to_source_in_text` would do (#471).
            let token = if let (Some(t), Some(lines)) = (text.as_deref(), lines.as_ref()) {
                let col = lines.source_column(pos);
                word_at_position(t, pos.line, col as u32).unwrap_or_else(|| key_lower.clone())
            } else {
                key_lower.clone()
            };
            let range = prepare_rename_range(text.as_deref(), pos, &token, &position_encoding);
            return Ok(Some(PrepareRenameResponse::Range(range)));
        }

        let type_ref = self.type_ref_at_cursor(&uri, pos, &logical_path);

        if let Some((_, instance_name)) = type_ref {
            let range =
                prepare_rename_range(text.as_deref(), pos, &instance_name, &position_encoding);
            return Ok(Some(PrepareRenameResponse::Range(range)));
        }
        Ok(None)
    }

    /// `lines` is built from the document's text, so it is also the line
    /// source here; there is no second `text.lines()` pass and no `text`
    /// parameter left to take (#471).
    fn rename_at_var(
        &self,
        uri: &str,
        lines: &DocLines,
        name: &str,
        new_name: &str,
        snapshot: &RenameRequestSnapshot,
    ) -> Result<Option<WorkspaceEdit>> {
        let edits: Vec<TextEdit> = lines
            .iter()
            .flat_map(|(line0, line)| {
                code_token_cols_in_line(line, name, TokenCase::Sensitive)
                    .into_iter()
                    .map(move |col| TextEdit {
                        range: lines.token_range(line0, col, name),
                        new_text: new_name.to_string(),
                    })
            })
            .collect();
        if edits.is_empty() {
            return Ok(None);
        }
        let by_uri = vec![(uri.to_string(), edits)];
        if let Some(refused) = self.first_refused_edit_target(&by_uri, uri) {
            return Err(refused);
        }
        Ok(Some(self.build_workspace_edit(by_uri, snapshot)?))
    }

    fn at_var_rename_target(
        text: &str,
        lines: &DocLines,
        pos: Position,
    ) -> Option<(String, Range)> {
        let col = lines.source_column(pos);
        let (name, start_col) = at_var_at_cursor(text, pos.line, col as u32)?;
        let range = lines.token_range(pos.line, start_col, &name);
        Some((name, range))
    }

    async fn rename_loc(
        &self,
        uri: &str,
        key_lower: &str,
        new_name: &str,
        snapshot: &RenameRequestSnapshot,
    ) -> Result<Option<WorkspaceEdit>> {
        let root = loc_root(key_lower);
        let trigger_suffix = key_lower.strip_prefix(&root).unwrap_or("");
        let new_lower = new_name.to_lowercase();
        let new_root_original = if !trigger_suffix.is_empty() && new_lower.ends_with(trigger_suffix)
        {
            let suffix_len = trigger_suffix.len();
            let end = new_name.len().saturating_sub(suffix_len);
            if new_name[end..].eq_ignore_ascii_case(trigger_suffix) {
                new_name[..end].to_string()
            } else {
                new_name.to_string()
            }
        } else {
            new_name.to_string()
        };
        let candidates: Vec<String> = vec![
            root.clone(),
            format!("{root}_desc"),
            format!("{root}_tooltip"),
            format!("{root}_desc_tooltip"),
            format!("{root}_tooltip_desc"),
        ];
        let mut target_to_new: HashMap<String, String> = HashMap::new();
        for cand in candidates {
            if cand == key_lower || self.is_known_loc_key(&cand) {
                let suffix = cand.strip_prefix(&root).unwrap_or("");
                let new_sib = format!("{}{}", new_root_original, suffix);
                target_to_new.entry(cand).or_insert(new_sib);
            }
        }
        if !target_to_new.contains_key(key_lower) {
            target_to_new.insert(key_lower.to_string(), new_name.to_string());
        }
        if uri.parse::<Url>().is_err() {
            return Ok(None);
        }
        let mut entities = HashMap::new();
        let mut script_edits = loop {
            let scans = self
                .collect_loc_rename_usages(Arc::new(target_to_new.clone()))
                .await?;
            let mut expanded = false;
            let mut edits = Vec::new();
            for scan in scans {
                edits.extend(scan.edits);
                for (entity, replacement) in scan.entities {
                    add_rename(&mut entities, entity, replacement)?;
                }
                for (key, replacement) in scan.keys {
                    expanded |= add_rename(&mut target_to_new, key, replacement)?;
                }
            }
            if !expanded {
                break edits;
            }
        };
        let mut destinations = HashSet::new();
        for (key, replacement) in &target_to_new {
            let lower = replacement.to_ascii_lowercase();
            if !destinations.insert(lower.clone())
                || (lower != *key
                    && self.is_known_loc_key(&lower)
                    && !target_to_new.contains_key(&lower))
            {
                return Err(rename_error(format!(
                    "localisation key '{replacement}' already exists"
                )));
            }
        }
        let mut entity_destinations = HashMap::new();
        for ((type_name, instance_name), replacement) in entities {
            add_rename(
                &mut entity_destinations,
                (type_name.clone(), replacement.to_ascii_lowercase()),
                instance_name.to_ascii_lowercase(),
            )?;
            validate_replacement_name(&replacement, RenameNameKind::Type)?;
            if self
                .state
                .info_service
                .read()
                .type_index
                .instances(&type_name)
                .iter()
                .any(|(_, instance)| {
                    instance.name.eq_ignore_ascii_case(&replacement)
                        && !instance.name.eq_ignore_ascii_case(&instance_name)
                })
            {
                return Err(rename_error(format!(
                    "{type_name} '{replacement}' already exists"
                )));
            }
            script_edits.extend(
                self.collect_type_rename_edits(&type_name, &instance_name, &replacement)
                    .await?,
            );
        }
        let loc_uris = self.loc_file_uris().await;
        let loc_edits = self
            .collect_loc_rename_loc_edits(Arc::new(target_to_new), loc_uris)
            .await?;
        let mut by_uri: HashMap<String, Vec<TextEdit>> = HashMap::new();
        for (file_uri, edits) in loc_edits.into_iter().chain(script_edits) {
            by_uri.entry(file_uri).or_default().extend(edits);
        }
        if by_uri.is_empty() {
            return Ok(None);
        }
        let mut deduped: HashMap<String, Vec<TextEdit>> = HashMap::new();
        for (file_uri, mut edits) in by_uri {
            edits.sort_by_key(|a| (a.range.start, a.range.end));
            for pair in edits.windows(2) {
                if pair[0].range.end > pair[1].range.start
                    && (pair[0].range != pair[1].range || pair[0].new_text != pair[1].new_text)
                {
                    return Err(rename_error("coupled symbols produce overlapping edits"));
                }
            }
            edits.dedup_by(|a, b| a.range == b.range && a.new_text == b.new_text);
            deduped.insert(file_uri, edits);
        }
        let by_uri: Vec<(String, Vec<TextEdit>)> = deduped.into_iter().collect();
        if let Some(err) = self.first_refused_edit_target(&by_uri, uri) {
            return Err(err);
        }
        Ok(Some(self.build_workspace_edit(by_uri, snapshot)?))
    }

    /// The edits inside loc files: each key's definition lines, and every
    /// `$key$` reference in a value. One budgeted pass parses each file once;
    /// the definitions come from the parser rather than the loc
    /// index because a rename has to reach every language on disk, and the
    /// index only holds the configured ones (#474).
    async fn collect_loc_rename_loc_edits(
        &self,
        target_to_new: Arc<HashMap<String, String>>,
        loc_uris: Vec<String>,
    ) -> Result<Vec<(String, Vec<TextEdit>)>> {
        if loc_uris.is_empty() {
            return Ok(Vec::new());
        }
        let encoding = self.position_encoding();
        let destinations: HashSet<String> = target_to_new
            .values()
            .map(|key| key.to_ascii_lowercase())
            .collect();
        self.scan_workspace_texts(loc_uris, move |uri, text| {
            let path = crate::paths::uri_to_path_str(uri);
            let files =
                cwtools_localization::parse_loc_files(&path, text, None).unwrap_or_default();
            let lines = DocLines::new(text, encoding.clone());
            let mut edits = Vec::new();
            for entry in files.iter().flat_map(|file| &file.entries) {
                let lower = entry.key.to_lowercase();
                let Some(new_text) = target_to_new.get(&lower) else {
                    if destinations.contains(&lower) {
                        return vec![Err(rename_error(format!(
                            "localisation key '{}' already exists",
                            entry.key
                        )))];
                    }
                    continue;
                };
                let line0 = (entry.position.line.saturating_sub(1)) as u32;
                let line_text = lines.line(line0);
                let col = line_text
                    .find(&entry.key)
                    .map(|b| line_text[..b].chars().count() as u32)
                    .unwrap_or(0);
                edits.push(TextEdit {
                    range: lines.token_range(line0, col, &entry.key),
                    new_text: new_text.clone(),
                });
            }
            let lower = text.to_ascii_lowercase();
            for ((line0, line), lower_line) in lines.iter().zip(lower.lines()) {
                if !lower_line.contains('$') {
                    continue;
                }
                for (key_lower, new_text) in target_to_new.iter() {
                    if !lower_line.contains(key_lower.as_str()) {
                        continue;
                    }
                    for col in loc_ref_key_cols_in_line(line, key_lower) {
                        edits.push(TextEdit {
                            range: lines.token_range(line0, col, key_lower),
                            new_text: new_text.clone(),
                        });
                    }
                }
            }
            if edits.is_empty() {
                Vec::new()
            } else {
                vec![Ok((uri.to_string(), edits))]
            }
        })
        .await?
        .into_iter()
        .collect()
    }

    async fn loc_rename_key_at_cursor(
        &self,
        uri: &str,
        pos: Position,
        logical_path: &str,
    ) -> Result<Option<String>> {
        let Some(key) = self.loc_key_at_cursor(uri, pos, logical_path).await else {
            return Ok(None);
        };
        if crate::paths::is_loc_file(uri) {
            return Ok(Some(key));
        }
        let Some(text) = self.file_text_for(uri).await else {
            return Ok(None);
        };
        let scan =
            self.scan_loc_rename_file(uri, &text, &HashMap::from([(key.clone(), key.clone())]))?;
        Ok(scan
            .edits
            .iter()
            .flat_map(|(_, edits)| edits)
            .any(|edit| edit.range.start <= pos && pos <= edit.range.end)
            .then_some(key))
    }

    async fn collect_loc_rename_usages(
        &self,
        target_to_new: Arc<HashMap<String, String>>,
    ) -> Result<Vec<LocRenameScan>> {
        let mut uris: HashSet<String> = self.script_uris().into_iter().collect();
        let ruleset = self.state.rules.read().ruleset.clone();
        if let Some(ruleset) = ruleset {
            let index = Arc::clone(&self.state.info_service.read().type_index);
            for td in &ruleset.types {
                for (uri, instance) in index.instances(&td.name) {
                    let candidate = target_to_new.contains_key(&instance.name.to_ascii_lowercase())
                        || td
                            .localisation
                            .iter()
                            .chain(td.subtypes.iter().flat_map(|subtype| &subtype.localisation))
                            .any(|loc| {
                                loc.explicit_field.is_none()
                                    && target_to_new.contains_key(
                                        &loc.derived_key(&instance.name).to_ascii_lowercase(),
                                    )
                            });
                    if candidate && crate::paths::is_script_file(uri) {
                        uris.insert(uri.to_string());
                    }
                }
            }
        }
        let backend = self.clone();
        self.scan_workspace_texts(uris.into_iter().collect(), move |uri, text| {
            vec![backend.scan_loc_rename_file(uri, text, &target_to_new)]
        })
        .await?
        .into_iter()
        .collect()
    }

    fn scan_loc_rename_file(
        &self,
        uri: &str,
        text: &str,
        targets: &HashMap<String, String>,
    ) -> Result<LocRenameScan> {
        let mut scan = LocRenameScan::default();
        let (ruleset, modifier_keys, registry) = {
            let rules = self.state.rules.read();
            let Some(ruleset) = rules.ruleset.clone() else {
                return Ok(scan);
            };
            (
                ruleset,
                rules.modifier_keys.clone(),
                rules.scope_registry.clone(),
            )
        };
        let (logical_path, game) = {
            let cfg = self.state.config.read();
            (
                logical_path_from_uri(uri, &cfg.workspace_prefix),
                cfg.game(),
            )
        };
        let type_index = Arc::clone(&self.state.info_service.read().type_index);
        let table = self.state.string_table.with_overlay();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let prepared = crate::validate::make_prepared(
            &ruleset,
            &table,
            game,
            &type_index,
            &modifier_keys,
            None,
            None,
            None,
            registry.as_ref(),
            false,
            false,
        );
        let resolver = FilePositionResolver::new(&logical_path, &prepared);
        let lines = DocLines::new(text, self.position_encoding());
        let mut edits = Vec::new();
        let mut loc_spans = HashSet::new();
        let is_localisation = |span: SourceRange, in_value: bool| {
            rules_at_pos_with_resolver(
                &ast,
                &resolver,
                &prepared,
                span.start.line,
                span.start.col,
                false,
            )
            .is_some_and(|context| {
                context
                    .leaf
                    .as_ref()
                    .is_some_and(|leaf| leaf.in_value == in_value)
                    && context.value_rules.iter().any(|(rule, _)| match rule {
                        RuleType::LeafRule { left, right } => matches!(
                            if in_value { right } else { left },
                            NewField::LocalisationField { .. }
                        ),
                        RuleType::LeafValueRule { right } if in_value => {
                            matches!(right, NewField::LocalisationField { .. })
                        }
                        _ => false,
                    })
            })
        };
        for leaf in &ast.arena.leaves {
            let value = cwtools_info::leaf_value_string(&leaf.value, &table);
            let key = table.get_string(leaf.key.normal).unwrap_or_default();
            for (raw, span, in_value) in [
                (value.as_str(), leaf.value_pos, true),
                (key.as_str(), leaf.pos, false),
            ] {
                let Some(replacement) = targets.get(&raw.trim_matches('"').to_ascii_lowercase())
                else {
                    continue;
                };
                if is_localisation(span, in_value) {
                    let range = loc_token_range(&lines, span, raw);
                    loc_spans.insert((range.start.line, range.start.character));
                    edits.push(TextEdit {
                        range,
                        new_text: replacement.clone(),
                    });
                }
            }
        }
        for leaf in &ast.arena.leaf_values {
            let value = cwtools_info::leaf_value_string(&leaf.value, &table);
            if let Some(replacement) = targets.get(&value.trim_matches('"').to_ascii_lowercase())
                && is_localisation(leaf.pos, true)
            {
                edits.push(TextEdit {
                    range: loc_token_range(&lines, leaf.pos, &value),
                    new_text: replacement.clone(),
                });
            }
        }
        let collected = cwtools_info::collect_type_instances_with_subtypes(
            &ruleset,
            &ast,
            &logical_path,
            &table,
            cwtools_validation::subtype_membership_for_instance,
        );
        for td in &ruleset.types {
            let Some(instances) = collected.instances.get(&td.name) else {
                continue;
            };
            for instance in instances {
                let localisations = td
                    .localisation
                    .iter()
                    .chain(
                        td.subtypes
                            .iter()
                            .filter(|subtype| {
                                collected
                                    .subtype_instances
                                    .get(&format!("{}.{}", td.name, subtype.name))
                                    .is_some_and(|instances| {
                                        instances.iter().any(|subtype_instance| {
                                            subtype_instance.location.line == instance.location.line
                                                && subtype_instance.location.col
                                                    == instance.location.col
                                        })
                                    })
                            })
                            .flat_map(|subtype| &subtype.localisation),
                    )
                    .collect::<Vec<_>>();
                if let Some(node) = ast.arena.leaves.iter().find(|leaf| {
                    leaf.pos.start.line == instance.location.line
                        && leaf.pos.start.col == instance.location.col
                }) && let Value::Clause(children) = &node.value
                {
                    for field in localisations
                        .iter()
                        .filter_map(|loc| loc.explicit_field.as_ref())
                    {
                        for child in children {
                            let Child::Leaf(idx) = child else { continue };
                            let leaf = &ast.arena.leaves[*idx as usize];
                            if table
                                .get_string(leaf.key.normal)
                                .is_some_and(|key| key.eq_ignore_ascii_case(field))
                            {
                                let value = cwtools_info::leaf_value_string(&leaf.value, &table);
                                if let Some(replacement) =
                                    targets.get(&value.trim_matches('"').to_ascii_lowercase())
                                {
                                    let range = loc_token_range(&lines, leaf.value_pos, &value);
                                    loc_spans.insert((range.start.line, range.start.character));
                                    edits.push(TextEdit {
                                        range,
                                        new_text: replacement.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
                let Some(span) = definition_span(&ast, &table, td, instance) else {
                    if localisations.iter().any(|loc| {
                        loc.explicit_field.is_none()
                            && targets
                                .contains_key(&loc.derived_key(&instance.name).to_ascii_lowercase())
                    }) {
                        return Err(rename_error(
                            "a coupled entity has no editable script identifier; filename-derived entities require a file rename",
                        ));
                    }
                    continue;
                };
                let range = loc_token_range(&lines, span, &instance.name);
                let mut replacement = None;
                if loc_spans.contains(&(range.start.line, range.start.character)) {
                    replacement = targets.get(&instance.name.to_ascii_lowercase()).cloned();
                }
                for loc in &localisations {
                    if loc.explicit_field.is_some() {
                        continue;
                    }
                    let key = loc.derived_key(&instance.name).to_ascii_lowercase();
                    let Some(new_key) = targets.get(&key) else {
                        continue;
                    };
                    let lower = new_key.to_ascii_lowercase();
                    if !lower.starts_with(&loc.prefix.to_ascii_lowercase())
                        || !lower.ends_with(&loc.suffix.to_ascii_lowercase())
                        || new_key.len() <= loc.prefix.len() + loc.suffix.len()
                    {
                        return Err(rename_error(
                            "a coupled localisation key must retain its rule-defined prefix and suffix",
                        ));
                    }
                    let name =
                        new_key[loc.prefix.len()..new_key.len() - loc.suffix.len()].to_string();
                    if replacement
                        .as_ref()
                        .is_some_and(|previous| previous != &name)
                    {
                        return Err(rename_error(
                            "coupled localisation bindings disagree on the entity name",
                        ));
                    }
                    replacement = Some(name);
                }
                let Some(replacement) = replacement else {
                    continue;
                };
                add_rename(
                    &mut scan.entities,
                    (td.name.clone(), instance.name.clone()),
                    replacement.clone(),
                )?;
                edits.retain(|edit| edit.range != range);
                edits.push(TextEdit {
                    range: type_rename_range(
                        &lines,
                        span.start.line.saturating_sub(1),
                        u32::from(span.start.col),
                        &instance.name,
                    ),
                    new_text: replacement.clone(),
                });
                for loc in &localisations {
                    if loc.explicit_field.is_none() {
                        add_rename(
                            &mut scan.keys,
                            loc.derived_key(&instance.name).to_ascii_lowercase(),
                            loc.derived_key(&replacement),
                        )?;
                    }
                }
            }
        }
        if !edits.is_empty() {
            scan.edits.push((uri.to_string(), edits));
        }
        Ok(scan)
    }

    /// The refusal for the first URI in `by_uri` a generated edit may not write to, if any. A
    /// rename can reach a base-game definition (#160), so one refused target cancels it all;
    /// an edit touching only the request's own document is exempt (it works in `untitled:`).
    fn first_refused_edit_target(
        &self,
        by_uri: &[(String, Vec<TextEdit>)],
        own_uri: &str,
    ) -> Option<tower_lsp::jsonrpc::Error> {
        if let [(only, _)] = by_uri
            && only == own_uri
        {
            return None;
        }
        let edit_roots = self.state.config.read().editable_roots.clone();
        by_uri.iter().find_map(|(uri, _)| {
            let refusal = crate::access::editable_path(uri, &edit_roots).err()?;
            Some(rename_refused(uri, refusal))
        })
    }

    fn capture_rename_request(&self) -> RenameRequestSnapshot {
        let docs = self.state.documents.lock();
        RenameRequestSnapshot {
            generation: self
                .state
                .edit_generation
                .load(std::sync::atomic::Ordering::Relaxed),
            open_documents: docs
                .iter()
                .map(|(uri, doc)| (uri.clone(), (doc.version, docs.content_hash(uri))))
                .collect(),
        }
    }

    fn build_workspace_edit(
        &self,
        by_uri: Vec<(String, Vec<TextEdit>)>,
        snapshot: &RenameRequestSnapshot,
    ) -> Result<WorkspaceEdit> {
        // Hold the store through construction. Versions alone miss a close /
        // reopen with the same version, and unversioned clients need a refusal.
        let docs = self.state.documents.lock();
        let generation = self
            .state
            .edit_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        let target_changed = by_uri.iter().any(|(uri, _)| {
            let current = docs
                .get(uri)
                .map(|doc| (doc.version, docs.content_hash(uri)));
            snapshot.open_documents.get(uri).copied() != current
        });
        if generation != snapshot.generation || target_changed {
            return Err(tower_lsp::jsonrpc::Error {
                code: tower_lsp::jsonrpc::ErrorCode::ContentModified,
                message: "Rename cancelled: documents changed while rename edits were being collected; try again".into(),
                data: None,
            });
        }
        if self
            .state
            .workspace_edit_document_changes
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let edits = by_uri
                .into_iter()
                .filter_map(|(uri, edits)| {
                    let url = uri.parse::<Url>().ok()?;
                    Some(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: url,
                            version: snapshot
                                .open_documents
                                .get(&uri)
                                .map(|(version, _)| *version),
                        },
                        edits: edits.into_iter().map(OneOf::Left).collect(),
                    })
                })
                .collect();
            Ok(WorkspaceEdit {
                changes: None,
                document_changes: Some(DocumentChanges::Edits(edits)),
                change_annotations: None,
            })
        } else {
            let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
            for (uri, edits) in by_uri {
                if let Ok(url) = uri.parse::<Url>() {
                    changes.entry(url).or_default().extend(edits);
                }
            }
            Ok(WorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            })
        }
    }

    pub(crate) async fn rename_impl(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let snapshot = self.capture_rename_request();
        let uri = params.text_document_position.text_document.uri.to_string();
        let pos = params.text_document_position.position;
        let new_name = params.new_name.clone();
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);

        let position_encoding = self.position_encoding();
        let source_text = self.file_text_for(&uri).await;
        let source_lines = source_text
            .as_deref()
            .map(|text| DocLines::new(text, position_encoding));
        if let (Some(text), Some(lines)) = (source_text.as_deref(), source_lines.as_ref())
            && let Some((name, _)) = Self::at_var_rename_target(text, lines, pos)
        {
            validate_replacement_name(&new_name, RenameNameKind::Constant)?;
            return self.rename_at_var(&uri, lines, &name, &new_name, &snapshot);
        }

        if let Some(key_lower) = self
            .loc_rename_key_at_cursor(&uri, pos, &logical_path)
            .await?
        {
            validate_replacement_name(&new_name, RenameNameKind::Localisation)?;
            match self
                .rename_loc(&uri, &key_lower, &new_name, &snapshot)
                .await
            {
                Ok(Some(edit)) => return Ok(Some(edit)),
                Ok(None) => {}
                Err(e) => return Err(e),
            }
        }

        let type_ref = self.type_ref_at_cursor(&uri, pos, &logical_path);

        let (type_name, instance_name) = match type_ref {
            Some(r) => r,
            None => return Ok(None),
        };
        validate_replacement_name(&new_name, RenameNameKind::Type)?;
        let by_uri = self
            .collect_type_rename_edits(&type_name, &instance_name, &new_name)
            .await?;
        if by_uri.is_empty() {
            return Ok(None);
        }
        if let Some(refused) = self.first_refused_edit_target(&by_uri, &uri) {
            return Err(refused);
        }
        Ok(Some(self.build_workspace_edit(by_uri, &snapshot)?))
    }

    async fn collect_type_rename_edits(
        &self,
        type_name: &str,
        instance_name: &str,
        new_name: &str,
    ) -> Result<Vec<(String, Vec<TextEdit>)>> {
        let mut edits: Vec<(String, u32, u32)> = Vec::new();
        let type_name = type_name.split('.').next().unwrap_or(type_name);
        let (ruleset, modifier_keys, registry) = {
            let rules = self.state.rules.read();
            (
                rules.ruleset.clone(),
                rules.modifier_keys.clone(),
                rules.scope_registry.clone(),
            )
        };
        let reference_types = std::iter::once(type_name.to_string())
            .chain(
                ruleset
                    .iter()
                    .flat_map(|ruleset| &ruleset.types)
                    .filter(|td| td.name == type_name)
                    .flat_map(|td| &td.subtypes)
                    .map(|subtype| format!("{type_name}.{}", subtype.name)),
            )
            .collect::<Vec<_>>();
        let definitions = self
            .state
            .info_service
            .read()
            .type_index
            .instances(type_name)
            .iter()
            .filter(|(_, instance)| instance.name.eq_ignore_ascii_case(instance_name))
            .cloned()
            .collect::<Vec<_>>();
        let (open_uris, open_matches): (HashSet<String>, Vec<String>) = {
            let docs = self.state.documents.lock();
            let open_uris = docs
                .keys()
                .filter(|uri| crate::paths::is_script_file(uri))
                .cloned()
                .collect();
            let lower = instance_name.to_ascii_lowercase();
            let matches = docs
                .iter()
                .filter(|(uri, doc)| {
                    crate::paths::is_script_file(uri)
                        && doc.text.to_ascii_lowercase().contains(&lower)
                })
                .map(|(uri, _)| uri.clone())
                .collect();
            (open_uris, matches)
        };
        let mut sites = reference_types
            .iter()
            .flat_map(|reference_type| self.collect_use_sites(reference_type, instance_name))
            .collect::<Vec<_>>();
        sites.retain(|site| !open_uris.contains(site.file.as_ref()));
        let mut text_uris: Vec<String> =
            definitions.iter().map(|(uri, _)| uri.to_string()).collect();
        text_uris.extend(sites.iter().map(|site| site.file.to_string()));
        text_uris.extend(open_matches.iter().cloned());
        let texts = self.file_text_snapshots_for_navigation(&text_uris).await?;
        if let Some(ruleset) = &ruleset {
            let prefix = self.state.config.read().workspace_prefix.clone();
            let locations = definitions
                .iter()
                .map(|(uri, instance)| (uri.to_string(), instance.location))
                .collect::<Vec<_>>();
            for uri in open_matches {
                let text = texts
                    .get(&uri)
                    .ok_or_else(|| rename_error("an open reference could not be read"))?;
                let table = self.state.string_table.with_overlay();
                let ast = Arc::new(cwtools_parser::parser::parse_string(&text.text, &table));
                for reference_type in &reference_types {
                    sites.extend(super::scan_use_sites(
                        reference_type,
                        instance_name,
                        &[(uri.clone(), Arc::clone(&ast))],
                        ruleset,
                        &prefix,
                        &table,
                        &locations,
                    ));
                }
            }
        }
        let mut definition_files = HashSet::new();
        for (uri, _) in definitions {
            if !definition_files.insert(uri.clone()) {
                continue;
            }
            let text = texts
                .get(uri.as_ref())
                .ok_or_else(|| rename_error("an entity definition could not be read"))?;
            let table = self.state.string_table.with_overlay();
            let ast = cwtools_parser::parser::parse_string(&text.text, &table);
            let ruleset = ruleset
                .as_ref()
                .ok_or_else(|| rename_error("entity rules are not loaded"))?;
            let td = ruleset
                .types
                .iter()
                .find(|td| td.name == type_name)
                .ok_or_else(|| rename_error("entity rules could not be located"))?;
            let prefix = self.state.config.read().workspace_prefix.clone();
            let logical_path = logical_path_from_uri(&uri, &prefix);
            let instances =
                cwtools_info::collect_type_instances(ruleset, &ast, &logical_path, &table);
            let mut found = false;
            for instance in instances
                .get(type_name)
                .into_iter()
                .flatten()
                .filter(|instance| instance.name.eq_ignore_ascii_case(instance_name))
            {
                let span = definition_span(&ast, &table, td, instance).ok_or_else(|| {
                    rename_error("an entity definition could not be located in text")
                })?;
                edits.push((
                    uri.to_string(),
                    span.start.line.saturating_sub(1),
                    u32::from(span.start.col),
                ));
                found = true;
            }
            if !found {
                return Err(rename_error(
                    "an entity definition could not be located in text",
                ));
            }
        }
        let indexed = index_snapshots(&texts, &self.position_encoding());
        let mut resolved = sites
            .iter()
            .filter(|site| !texts.contains_key(site.file.as_ref()))
            .map(|site| {
                (
                    site.file.to_string(),
                    site.value.line.saturating_sub(1),
                    u32::from(site.value.col),
                    false,
                )
            })
            .collect::<Vec<_>>();
        if let Some(ruleset) = &ruleset {
            let (prefix, game) = {
                let cfg = self.state.config.read();
                (cfg.workspace_prefix.clone(), cfg.game())
            };
            let type_index = Arc::clone(&self.state.info_service.read().type_index);
            for (uri, text) in &texts {
                let file_sites = sites
                    .iter()
                    .filter(|site| site.file.as_ref() == uri)
                    .collect::<Vec<_>>();
                if file_sites.is_empty() {
                    continue;
                }
                let table = self.state.string_table.with_overlay();
                let ast = cwtools_parser::parser::parse_string(&text.text, &table);
                let logical_path = logical_path_from_uri(uri, &prefix);
                let prepared = crate::validate::make_prepared(
                    ruleset,
                    &table,
                    game,
                    &type_index,
                    &modifier_keys,
                    None,
                    None,
                    None,
                    registry.as_ref(),
                    false,
                    false,
                );
                let resolver = FilePositionResolver::new(&logical_path, &prepared);
                for site in file_sites {
                    let line = site.value.line.saturating_sub(1);
                    let col = u32::from(site.value.col);
                    let matches = indexed.get(uri.as_str()).is_some_and(|lines| {
                        let token: String = lines
                            .line(line)
                            .chars()
                            .skip(col as usize)
                            .take(instance_name.chars().count())
                            .collect();
                        token.eq_ignore_ascii_case(instance_name)
                            && !lines
                                .line(line)
                                .chars()
                                .nth(col as usize + instance_name.chars().count())
                                .is_some_and(super::helpers::is_ident_char)
                    });
                    if !matches {
                        resolved.push((uri.clone(), line, col, false));
                        continue;
                    }
                    let Some(context) = rules_at_pos_with_resolver(
                        &ast,
                        &resolver,
                        &prepared,
                        site.value.line,
                        site.value.col,
                        false,
                    ) else {
                        continue;
                    };
                    let Some(leaf) = &context.leaf else { continue };
                    let is_reference = context.value_rules.iter().any(|(rule, _)| {
                        let hint = if leaf.in_value {
                            crate::cursor::hint_from_rule_right(rule, &leaf.value, ruleset)
                        } else {
                            crate::cursor::hint_from_rule_left(rule, &leaf.key)
                        };
                        matches!(hint, cwtools_info::ReferenceHint::TypeRef { type_name: reference_type, .. }
                            if reference_type.split('.').next() == Some(type_name))
                    });
                    if is_reference {
                        resolved.push((uri.clone(), line, col, true));
                    }
                }
            }
        }
        let unresolved = resolved.iter().filter(|(_, _, _, ok)| !ok).count();
        if unresolved > 0 {
            return Err(tower_lsp::jsonrpc::Error {
                code: tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED),
                message: format!(
                    "Rename cancelled: {} reference(s) to '{}' could not be located in text; \
                     rename is limited to indexed references.",
                    unresolved, instance_name
                )
                .into(),
                data: None,
            });
        }
        for (file_uri, line0, col, _) in resolved {
            edits.push((file_uri, line0, col));
        }

        if edits.is_empty() {
            return Ok(Vec::new());
        }

        let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
        let mut by_uri: HashMap<String, Vec<TextEdit>> = HashMap::new();
        for (file_uri, line0, col) in edits {
            if !seen.insert((file_uri.clone(), line0, col)) {
                continue;
            }
            let edit = TextEdit {
                range: indexed.get(file_uri.as_str()).map_or_else(
                    || self.source_range_with_lines(None, line0, col, instance_name),
                    |lines| type_rename_range(lines, line0, col, instance_name),
                ),
                new_text: new_name.to_string(),
            };
            by_uri.entry(file_uri).or_default().push(edit);
        }

        Ok(by_uri.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DocumentState, ParsedDoc};

    fn backend() -> (Backend, Arc<DocumentState>) {
        let backend = crate::testing::test_backend();
        let state = backend.state.clone();
        (backend, state)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn loc_rename_discards_definition_edits_when_usage_scan_exceeds_budget() {
        let (backend, state) = backend();
        let ws = tempfile::TempDir::new().expect("workspace");
        let loc_dir = ws.path().join("localisation");
        std::fs::create_dir_all(&loc_dir).unwrap();
        let content = "l_english:\nx:0 \"x\"\n";
        let open_uri = Url::from_file_path(loc_dir.join("open_l_english.yml"))
            .expect("open URI")
            .to_string();
        let root = std::fs::canonicalize(ws.path()).expect("canonical root");
        {
            let mut config = state.config.write();
            config.authorized_roots = Arc::from([root.clone()]);
            config.workspace_roots = vec![root.clone()];
            config.editable_roots = Arc::from([root]);
        }
        state
            .documents
            .lock()
            .open(
                open_uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(content),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        *state.navigation_snapshot_budget_override.lock() =
            Some(crate::access::scan_peak_bytes(content.len()));
        let snapshot = backend.capture_rename_request();
        let edit = backend
            .rename_loc(&open_uri, "x", "renamed_x", &snapshot)
            .await
            .expect("the definition scan fits the peak cap")
            .expect("the definition produces an edit");
        let changes = edit.changes.expect("the client uses unversioned changes");
        let definition_edits = changes.get(&open_uri.parse().unwrap()).unwrap();
        assert_eq!(definition_edits.len(), 1);
        assert_eq!(definition_edits[0].new_text, "renamed_x");

        let script_uri = Url::from_file_path(ws.path().join("large.txt"))
            .expect("script URI")
            .to_string();
        state
            .documents
            .lock()
            .open(
                script_uri,
                ParsedDoc {
                    version: 1,
                    text: Arc::from("value = x\n".repeat(4)),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        let snapshot = backend.capture_rename_request();
        let error = backend
            .rename_loc(&open_uri, "x", "renamed_x", &snapshot)
            .await
            .expect_err("usage exhaustion must discard the already collected definition edits");
        assert_eq!(
            error.code,
            tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED)
        );
        assert!(error.message.contains("total text read budget exceeded"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn loc_rename_refuses_shared_budget_exhaustion_without_partial_edits() {
        let (backend, state) = backend();
        let ws = tempfile::TempDir::new().expect("workspace");
        let loc_dir = ws.path().join("localisation");
        std::fs::create_dir_all(&loc_dir).unwrap();
        let content = "l_english:\nx:0 \"x\"\n";
        let open_path = loc_dir.join("open_l_english.yml");
        let open_uri = Url::from_file_path(&open_path)
            .expect("open URI")
            .to_string();
        let closed_paths = (0..2)
            .map(|idx| {
                let path = loc_dir.join(format!("closed-{idx}_l_english.yml"));
                std::fs::write(&path, content).unwrap();
                path
            })
            .collect::<Vec<_>>();
        let closed_uris = closed_paths
            .iter()
            .map(|path| Url::from_file_path(path).expect("closed URI").to_string())
            .collect::<Vec<_>>();
        let root = std::fs::canonicalize(ws.path()).expect("canonical root");
        {
            let mut config = state.config.write();
            config.authorized_roots = Arc::from([root.clone()]);
            config.workspace_roots = vec![root.clone()];
            config.editable_roots = Arc::from([root]);
        }
        state
            .documents
            .lock()
            .open(
                open_uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(content),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("the store accepts the open loc buffer");

        let limit = content.len() + (content.len() + 1) * 5;
        *state.navigation_snapshot_budget_override.lock() = Some(limit);
        for closed_uri in &closed_uris {
            let individually_fitting = vec![open_uri.clone(), closed_uri.clone()];
            backend
                .file_text_snapshots_for_navigation(&individually_fitting)
                .await
                .expect("the open buffer and one closed file fit the cap");
        }
        let snapshot = backend.capture_rename_request();
        let error = backend
            .rename_loc(&open_uri, "x", "renamed_x", &snapshot)
            .await
            .expect_err("the rename must abort rather than return partial edits");
        assert_eq!(
            error.code,
            tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED)
        );
        assert!(error.message.contains("total text read budget exceeded"));
    }
}
