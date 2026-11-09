use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::*;

use cwtools_localization::Lang;

use crate::Backend;
use crate::paths::source_column_to_lsp;

use super::payload::fix_from_data;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum LocInsertTarget {
    ExistingFileAfterKey { uri: Url, key: String },
    ExistingFileAppend { uri: Url },
    NewFile { uri: Url },
}

fn sibling_loc_keys(
    defs: &[cwtools_rules::rules_types::TypeLocalisation],
    instance_name: &str,
    create_loc_key: &str,
) -> Vec<String> {
    defs.iter()
        .filter(|loc| loc.is_required_name_derived())
        .map(|loc| loc.derived_key(instance_name))
        .filter(|k| k != create_loc_key)
        .collect()
}

fn resolve_sibling_site(
    siblings: &[String],
    loc_locations: &crate::LocLocations,
    edit_roots: &[PathBuf],
    lang: Lang,
) -> Option<(std::sync::Arc<str>, String)> {
    let marker = format!("l_{lang}").to_ascii_lowercase();
    siblings.iter().find_map(|sib| {
        let (uri, _) = loc_locations.get(sib.to_ascii_lowercase().as_str())?;
        let fname = uri.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        if !fname.contains(&marker) {
            return None;
        }
        crate::access::editable_path(uri, edit_roots).ok()?;
        Some((std::sync::Arc::clone(uri), sib.clone()))
    })
}

fn pick_lang_file(discovered: &[PathBuf], lang: Lang, edit_roots: &[PathBuf]) -> Option<PathBuf> {
    let marker = format!("l_{lang}").to_ascii_lowercase();
    let mut matches: Vec<&PathBuf> = discovered
        .iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_ascii_lowercase().contains(&marker))
        })
        .collect();
    matches.sort();
    matches
        .into_iter()
        .find(|p| crate::access::editable_target(p, edit_roots).is_ok())
        .cloned()
}

fn generated_loc_file_path(workspace_root: &Path, lang: Lang) -> PathBuf {
    workspace_root
        .join("localisation")
        .join(format!("cwtools_generated_l_{lang}.yml"))
}

/// Check each insertion tier against editable roots so a refused target falls through (#160).
fn resolve_loc_insert_target(
    lang: Lang,
    siblings: &[String],
    loc_locations: &crate::LocLocations,
    edit_roots: &[PathBuf],
    discovered_loc_files: &[PathBuf],
    workspace_root: &Path,
) -> Option<LocInsertTarget> {
    if let Some((uri, key)) = resolve_sibling_site(siblings, loc_locations, edit_roots, lang) {
        return Some(LocInsertTarget::ExistingFileAfterKey {
            uri: Url::parse(&uri).ok()?,
            key,
        });
    }
    if let Some(path) = pick_lang_file(discovered_loc_files, lang, edit_roots) {
        return Some(LocInsertTarget::ExistingFileAppend {
            uri: Url::from_file_path(&path).ok()?,
        });
    }
    let new_path = generated_loc_file_path(workspace_root, lang);
    crate::access::editable_target(&new_path, edit_roots).ok()?;
    Some(LocInsertTarget::NewFile {
        uri: Url::from_file_path(new_path).ok()?,
    })
}

fn stub_line(key: &str) -> String {
    format!(" {key}:0 \"TODO\"")
}

fn eol_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Position and text to insert `stub` as a new line after 0-based line `after_line0`. A file
/// with no trailing newline anchors at the end of its last line and prepends the EOL; an
/// empty file has nothing to anchor on.
fn insert_stub_after_line(
    text: &str,
    after_line0: u32,
    stub: &str,
    encoding: &PositionEncodingKind,
) -> (Position, String) {
    let eol = eol_of(text);
    if text.is_empty() {
        return (Position::new(0, 0), format!("{stub}{eol}"));
    }
    let ends_with_nl = text.ends_with('\n');
    let line_count = text.lines().count() as u32;
    let next_line_exists =
        after_line0 + 1 < line_count || (after_line0 + 1 == line_count && ends_with_nl);
    if next_line_exists {
        (Position::new(after_line0 + 1, 0), format!("{stub}{eol}"))
    } else {
        let line_text = text.lines().nth(after_line0 as usize).unwrap_or("");
        let col = source_column_to_lsp(line_text, line_text.chars().count() as u32, encoding);
        (Position::new(after_line0, col), format!("{eol}{stub}"))
    }
}

/// A file whose whole content is a BOM and/or whitespace holds no loc entries, so it is
/// still a header-initialization target (#1255); `text.is_empty()` alone would hand a
/// stub a bare append under no `l_x:` header. The BOM is not whitespace, so it is
/// named explicitly.
fn is_effectively_empty(text: &str) -> bool {
    text.chars().all(|c| c == '\u{FEFF}' || c.is_whitespace())
}

/// The position at the very end of `text`, encoding-aware, so an edit can consume the
/// whole effectively-empty body (a stray BOM must be replaced, not left after the
/// header as a second BOM) or compose just past it. Non-trailing-newline files end on
/// their last line; trailing-newline files end on the empty line after it.
fn end_of_text_position(text: &str, encoding: &PositionEncodingKind) -> Position {
    let line_count = text.lines().count() as u32;
    if text.ends_with('\n') {
        return Position::new(line_count, 0);
    }
    let last = text.lines().last().unwrap_or("");
    Position::new(
        line_count.saturating_sub(1),
        source_column_to_lsp(last, last.chars().count() as u32, encoding),
    )
}

fn insert_edit_op(
    uri: Url,
    version: Option<i32>,
    pos: Position,
    new_text: String,
) -> DocumentChangeOperation {
    range_edit_op(uri, version, Range::new(pos, pos), new_text)
}

fn range_edit_op(
    uri: Url,
    version: Option<i32>,
    range: Range,
    new_text: String,
) -> DocumentChangeOperation {
    DocumentChangeOperation::Edit(TextDocumentEdit {
        text_document: OptionalVersionedTextDocumentIdentifier { uri, version },
        edits: vec![OneOf::Left(TextEdit { range, new_text })],
    })
}

struct LocKeyCandidate<'d> {
    diag: &'d Diagnostic,
    title: String,
    key: String,
    targets: Vec<(Lang, LocInsertTarget)>,
}

/// The first candidate (batch order) needing a not-yet-existing loc file, per language.
/// A later candidate needing the same new file folds into it instead of re-creating the
/// file (#142): two `NewFile` targets from one discovery snapshot would duplicate the header.
fn new_file_owners<'a>(
    all_targets: impl IntoIterator<Item = &'a [(Lang, LocInsertTarget)]>,
) -> HashMap<Lang, usize> {
    let mut owners = HashMap::new();
    for (idx, targets) in all_targets.into_iter().enumerate() {
        for (lang, target) in targets {
            if matches!(target, LocInsertTarget::NewFile { .. }) {
                owners.entry(*lang).or_insert(idx);
            }
        }
    }
    owners
}

fn candidate_operations(
    cand: &LocKeyCandidate<'_>,
    owners: &HashMap<Lang, usize>,
    self_idx: usize,
    headed: &mut HashSet<(Lang, String)>,
    empty_targets: &HashSet<(Lang, String)>,
    existing_file_ops: &mut impl FnMut(
        &LocInsertTarget,
        Lang,
        bool,
        &str,
    ) -> Option<Vec<DocumentChangeOperation>>,
) -> Option<Vec<DocumentChangeOperation>> {
    let stub = stub_line(&cand.key);
    let mut operations = Vec::new();
    // Failed candidates must not reserve headers their discarded edits never insert.
    let mut headed_here = HashSet::new();
    for (lang, target) in &cand.targets {
        let ops = match target {
            LocInsertTarget::NewFile { uri } if owners.get(lang) == Some(&self_idx) => Some(vec![
                DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                    uri: uri.clone(),
                    options: Some(CreateFileOptions {
                        overwrite: Some(false),
                        ignore_if_exists: Some(true),
                    }),
                    annotation_id: None,
                })),
                insert_edit_op(
                    uri.clone(),
                    None,
                    Position::new(0, 0),
                    format!("\u{FEFF}l_{lang}:\n{stub}\n"),
                ),
            ]),
            LocInsertTarget::NewFile { uri } => Some(vec![insert_edit_op(
                uri.clone(),
                None,
                Position::new(0, 0),
                format!("{stub}\n"),
            )]),
            other => {
                let site = match other {
                    LocInsertTarget::ExistingFileAppend { uri } => Some((*lang, uri.to_string())),
                    _ => None,
                };
                let write_header = site.as_ref().is_some_and(|site| {
                    empty_targets.contains(site)
                        && !headed.contains(site)
                        && !headed_here.contains(site)
                });
                let ops = existing_file_ops(other, *lang, write_header, &stub)?;
                if let Some(site) = write_header.then_some(site).flatten() {
                    headed_here.insert(site);
                }
                Some(ops)
            }
        };
        operations.extend(ops?);
    }
    headed.extend(headed_here);
    Some(operations)
}

fn merge_operations(
    into: &mut Vec<DocumentChangeOperation>,
    new_ops: Vec<DocumentChangeOperation>,
) {
    for new_op in new_ops {
        let DocumentChangeOperation::Edit(new_edit) = new_op else {
            into.push(new_op);
            continue;
        };
        let existing = into.iter_mut().find_map(|op| match op {
            DocumentChangeOperation::Edit(e)
                if e.text_document.uri == new_edit.text_document.uri =>
            {
                Some(e)
            }
            _ => None,
        });
        match existing {
            Some(e) => e.edits.extend(new_edit.edits),
            None => into.push(DocumentChangeOperation::Edit(new_edit)),
        }
    }
}

/// Initialize each empty target once per returned action.
fn build_create_loc_key_batch<'d>(
    candidates: &[LocKeyCandidate<'d>],
    empty_targets: &HashSet<(Lang, String)>,
    mut existing_file_ops: impl FnMut(
        &LocInsertTarget,
        Lang,
        bool,
        &str,
    ) -> Option<Vec<DocumentChangeOperation>>,
) -> Vec<CodeAction> {
    let owners = new_file_owners(candidates.iter().map(|c| c.targets.as_slice()));
    let fold_into: Vec<Option<usize>> = candidates
        .iter()
        .enumerate()
        .map(|(idx, cand)| {
            cand.targets.iter().find_map(|(lang, target)| {
                if !matches!(target, LocInsertTarget::NewFile { .. }) {
                    return None;
                }
                let &owner_idx = owners.get(lang)?;
                (owner_idx != idx).then_some(owner_idx)
            })
        })
        .collect();

    let mut actions: Vec<CodeAction> = Vec::new();
    let mut built_index: Vec<Option<usize>> = vec![None; candidates.len()];
    let mut headed_by_action: Vec<HashSet<(Lang, String)>> = Vec::new();

    for (idx, cand) in candidates.iter().enumerate() {
        if fold_into[idx].is_some() {
            continue;
        }
        let mut headed = HashSet::new();
        let Some(operations) = candidate_operations(
            cand,
            &owners,
            idx,
            &mut headed,
            empty_targets,
            &mut existing_file_ops,
        ) else {
            continue;
        };
        built_index[idx] = Some(actions.len());
        headed_by_action.push(headed);
        actions.push(CodeAction {
            title: cand.title.clone(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![cand.diag.clone()]),
            edit: Some(WorkspaceEdit {
                changes: None,
                document_changes: Some(DocumentChanges::Operations(operations)),
                change_annotations: None,
            }),
            ..Default::default()
        });
    }

    for (idx, cand) in candidates.iter().enumerate() {
        let Some(owner_idx) = fold_into[idx] else {
            continue;
        };
        let Some(owner_pos) = built_index[owner_idx] else {
            continue; // the owner's own action failed to build; drop this too
        };
        let Some(folded_ops) = candidate_operations(
            cand,
            &owners,
            idx,
            &mut headed_by_action[owner_pos],
            empty_targets,
            &mut existing_file_ops,
        ) else {
            continue; // this candidate's own languages didn't all build
        };
        let Some(owner_action) = actions.get_mut(owner_pos) else {
            continue;
        };
        if let Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(ops)),
            ..
        }) = owner_action.edit.as_mut()
        {
            merge_operations(ops, folded_ops);
        }
        match owner_action.diagnostics.as_mut() {
            Some(diags) => diags.push(cand.diag.clone()),
            None => owner_action.diagnostics = Some(vec![cand.diag.clone()]),
        }
    }

    for action in &mut actions {
        if let Some(DocumentChanges::Operations(ops)) = action
            .edit
            .as_ref()
            .and_then(|edit| edit.document_changes.as_ref())
        {
            let versions: Vec<_> = ops
                .iter()
                .filter_map(|op| match op {
                    DocumentChangeOperation::Edit(edit) => Some(&edit.text_document),
                    DocumentChangeOperation::Op(_) => None,
                })
                .collect();
            action.data = Some(serde_json::json!({ "cwtoolsDocumentVersions": versions }));
        }
    }
    actions
}

impl Backend {
    /// Build the "Create localisation key" action for every context diagnostic carrying a
    /// `create_loc_key` fix payload (CW100). Two keys landing in the same not-yet-existing file
    /// share one header instead of each inserting their own (#142).
    pub(super) async fn create_loc_key_actions(
        &self,
        uri: &Url,
        diagnostics: &[Diagnostic],
        encoding: &PositionEncodingKind,
    ) -> Vec<CodeActionOrCommand> {
        let candidates: Vec<(&Diagnostic, String, String)> = diagnostics
            .iter()
            .filter_map(|d| {
                let payload = d.data.as_ref().and_then(fix_from_data)?;
                let key = payload.create_loc_key?;
                Some((d, payload.title, key))
            })
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }
        let (workspace_root, edit_roots, ignore_files, ignore_dirs) = {
            let cfg = self.state.config.read();
            let Some(ws_uri) = cfg.workspace_uri.clone() else {
                return Vec::new();
            };
            let Some(root) = crate::access::file_uri_to_path(&ws_uri) else {
                return Vec::new();
            };
            (
                root,
                cfg.editable_roots.clone(),
                cfg.ignore_file_patterns.clone(),
                cfg.ignore_dir_patterns.clone(),
            )
        };
        let langs: Vec<Lang> = {
            let cfg = self.state.config.read();
            cfg.loc_languages
                .clone()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| vec![Lang::English])
        };
        // Cache discovery because code-action requests also fire on cursor movement (#134).
        let discovered = {
            let cur_sig = *self.state.last_loc_signature.lock();
            let cached = self.state.loc_discovery_cache.lock().clone();
            match cached {
                Some((root, files, sig)) if root == workspace_root && sig == cur_sig => files,
                _ => {
                    let discovered_root = workspace_root.clone();
                    let files = match tokio::task::spawn_blocking(move || {
                        cwtools_driver::discover_localisation_files(
                            &[discovered_root],
                            &ignore_files,
                            &ignore_dirs,
                            cwtools_driver::DiscoveryPolicy::Workspace,
                        )
                    })
                    .await
                    {
                        Ok(Ok(discovery)) => {
                            for failure in discovery.failures {
                                tracing::warn!(
                                    path = %failure.path.display(),
                                    error = %failure.error,
                                    "localisation discovery skipped path"
                                );
                            }
                            discovery
                                .files
                                .into_iter()
                                .filter(|file| {
                                    file.kind == cwtools_file_manager::FileKind::Localisation
                                })
                                .map(|file| file.path)
                                .collect()
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(error = %error, "localisation discovery failed");
                            Vec::new()
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "localisation discovery task failed");
                            Vec::new()
                        }
                    };
                    *self.state.loc_discovery_cache.lock() =
                        Some((workspace_root.clone(), files.clone(), cur_sig));
                    files
                }
            }
        };

        let resolved: Vec<LocKeyCandidate<'_>> = {
            let loc_locations = self.state.loc_locations.read();
            candidates
                .into_iter()
                .filter_map(|(diag, title, key)| {
                    let siblings = self.sibling_loc_keys_for_diagnostic(uri, diag, &key);
                    let targets = langs
                        .iter()
                        .map(|&lang| {
                            resolve_loc_insert_target(
                                lang,
                                &siblings,
                                &loc_locations,
                                &edit_roots,
                                &discovered,
                                &workspace_root,
                            )
                            .map(|target| (lang, target))
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(LocKeyCandidate {
                        diag,
                        title,
                        key,
                        targets,
                    })
                })
                .collect()
        };

        let target_uris: Vec<String> = resolved
            .iter()
            .flat_map(|candidate| candidate.targets.iter())
            .map(|(_, target)| match target {
                LocInsertTarget::ExistingFileAfterKey { uri, .. }
                | LocInsertTarget::ExistingFileAppend { uri }
                | LocInsertTarget::NewFile { uri } => uri.to_string(),
            })
            .collect();
        let texts = self.file_text_snapshots_for(&target_uris).await;
        let empty_targets: HashSet<(Lang, String)> = resolved
            .iter()
            .flat_map(|candidate| candidate.targets.iter())
            .filter_map(|(lang, target)| match target {
                LocInsertTarget::ExistingFileAppend { uri }
                    if texts
                        .get(uri.as_str())
                        .is_some_and(|s| is_effectively_empty(&s.text)) =>
                {
                    Some((*lang, uri.to_string()))
                }
                _ => None,
            })
            .collect();

        build_create_loc_key_batch(
            &resolved,
            &empty_targets,
            |target, lang, write_header, stub| {
                self.loc_insert_operations(target, lang, write_header, stub, encoding, &texts)
            },
        )
        .into_iter()
        .map(CodeActionOrCommand::CodeAction)
        .collect()
    }

    fn sibling_loc_keys_for_diagnostic(
        &self,
        uri: &Url,
        diag: &Diagnostic,
        create_loc_key: &str,
    ) -> Vec<String> {
        let rules = self.state.rules.read();
        let Some(ruleset) = rules.ruleset.as_ref() else {
            return Vec::new();
        };
        let info = self.state.info_service.read();
        let target_line = diag.range.start.line + 1;
        let instances = info.type_index.instances_in_file(uri.as_str());
        instances
            .into_iter()
            .find_map(|(type_name, inst)| {
                if inst.location.line != target_line {
                    return None;
                }
                let td = ruleset.types.iter().find(|td| td.name == type_name)?;
                let owns_key = td.localisation.iter().any(|loc| {
                    loc.is_required_name_derived() && loc.derived_key(&inst.name) == create_loc_key
                });
                owns_key.then(|| sibling_loc_keys(&td.localisation, &inst.name, create_loc_key))
            })
            .unwrap_or_default()
    }

    /// The `Edit` operations for the two existing-file `LocInsertTarget` variants.
    /// `NewFile` is `None` here: `build_create_loc_key_batch` builds and dedupes it
    /// directly instead, since it needs no file read (#142).
    fn loc_insert_operations(
        &self,
        target: &LocInsertTarget,
        lang: Lang,
        write_header: bool,
        stub: &str,
        encoding: &PositionEncodingKind,
        texts: &HashMap<String, crate::FileTextSnapshot>,
    ) -> Option<Vec<DocumentChangeOperation>> {
        match target {
            LocInsertTarget::ExistingFileAfterKey { uri, key } => {
                let snapshot = texts.get(uri.as_str())?;
                let parsed =
                    cwtools_localization::parse_loc_text(&snapshot.text, uri.as_str()).ok()?;
                let anchor = parsed
                    .entries
                    .iter()
                    .find(|entry| entry.key.eq_ignore_ascii_case(key))?;
                let (pos, insert_text) = insert_stub_after_line(
                    &snapshot.text,
                    anchor.position.line.saturating_sub(1) as u32,
                    stub,
                    encoding,
                );
                Some(vec![insert_edit_op(
                    uri.clone(),
                    snapshot.version,
                    pos,
                    insert_text,
                )])
            }
            LocInsertTarget::ExistingFileAppend { uri } => {
                let snapshot = texts.get(uri.as_str())?;
                if is_effectively_empty(&snapshot.text) {
                    // The header edit consumes the whole effectively-empty body
                    // (#1255): a pre-existing BOM must not survive as a second
                    // BOM inside the initialized content. A folded stub on a
                    // site another candidate already headed is a zero-length
                    // insert at the end of that body instead, so the two edits
                    // never overlap the same range.
                    let end = end_of_text_position(&snapshot.text, encoding);
                    let op = if write_header {
                        range_edit_op(
                            uri.clone(),
                            snapshot.version,
                            Range::new(Position::new(0, 0), end),
                            format!("\u{FEFF}l_{lang}:\n{stub}\n"),
                        )
                    } else {
                        range_edit_op(
                            uri.clone(),
                            snapshot.version,
                            Range::new(end, end),
                            format!("{stub}\n"),
                        )
                    };
                    return Some(vec![op]);
                }
                let last_line0 = snapshot.text.lines().count().saturating_sub(1) as u32;
                let (pos, insert_text) =
                    insert_stub_after_line(&snapshot.text, last_line0, stub, encoding);
                Some(vec![insert_edit_op(
                    uri.clone(),
                    snapshot.version,
                    pos,
                    insert_text,
                )])
            }
            LocInsertTarget::NewFile { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn loc_def(
        name: &str,
        prefix: &str,
        suffix: &str,
    ) -> cwtools_rules::rules_types::TypeLocalisation {
        cwtools_rules::rules_types::TypeLocalisation {
            name: name.to_string(),
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
            required: true,
            optional: false,
            explicit_field: None,
            replace_scopes: None,
            primary: false,
        }
    }

    #[test]
    fn sibling_loc_keys_excludes_self_and_non_required() {
        let mut optional_def = loc_def("desc", "", "_desc");
        optional_def.optional = true;
        let defs = vec![
            loc_def("name", "", ""),
            loc_def("desc", "", "_desc"),
            optional_def,
            loc_def("title", "", "_title"),
        ];
        let siblings = sibling_loc_keys(&defs, "my_thing", "my_thing_desc");
        assert_eq!(siblings, vec!["my_thing", "my_thing_title"]);
    }

    fn loc_locations_with(entries: &[(&str, &str, u32)]) -> crate::LocLocations {
        entries
            .iter()
            .map(|(key, uri, line)| {
                (
                    std::sync::Arc::from(key.to_ascii_lowercase()),
                    (std::sync::Arc::from(*uri), *line),
                )
            })
            .collect()
    }

    fn edit_roots(dirs: [&Path; 1]) -> Vec<PathBuf> {
        dirs.iter()
            .map(|d| std::fs::canonicalize(d).expect("canonical root"))
            .collect()
    }

    fn write_loc_file(root: &Path, rel: &str) -> (PathBuf, String) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
        std::fs::write(&path, "l_english:\n my_thing:0 \"Thing\"\n").unwrap();
        let uri = Url::from_file_path(&path)
            .expect("absolute path")
            .to_string();
        (path, uri)
    }

    #[test]
    fn resolve_sibling_site_picks_the_matching_workspace_language_file() {
        let ws = tempfile::tempdir().unwrap();
        let (_, uri) = write_loc_file(ws.path(), "localisation/things_l_english.yml");
        let locs = loc_locations_with(&[("my_thing", &uri, 4)]);
        let siblings = vec!["my_thing".to_string()];
        let hit = resolve_sibling_site(&siblings, &locs, &edit_roots([ws.path()]), Lang::English)
            .expect("a hit");
        assert_eq!(hit.0.as_ref(), uri);
        assert_eq!(hit.1, "my_thing");
    }

    #[test]
    fn resolve_sibling_site_rejects_a_vanilla_definition() {
        let ws = tempfile::tempdir().unwrap();
        let vanilla = tempfile::tempdir().unwrap();
        let (_, uri) = write_loc_file(vanilla.path(), "localisation/things_l_english.yml");
        let locs = loc_locations_with(&[("my_thing", &uri, 4)]);
        let siblings = vec!["my_thing".to_string()];
        assert!(
            resolve_sibling_site(&siblings, &locs, &edit_roots([ws.path()]), Lang::English)
                .is_none()
        );
    }

    /// The reported bug (#160): workspace `…/mod` and base game `…/mod-vanilla`.
    #[test]
    fn resolve_sibling_site_rejects_a_sibling_directory_sharing_a_name_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("mod");
        std::fs::create_dir(&ws).unwrap();
        let (_, uri) = write_loc_file(
            &tmp.path().join("mod-vanilla"),
            "localisation/things_l_english.yml",
        );
        let locs = loc_locations_with(&[("my_thing", &uri, 4)]);
        let siblings = vec!["my_thing".to_string()];
        assert!(
            resolve_sibling_site(&siblings, &locs, &edit_roots([&ws]), Lang::English).is_none(),
            "`mod-vanilla` is not inside `mod`"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_sibling_site_skips_a_symlinked_site_and_falls_through() {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (target, _) = write_loc_file(outside.path(), "secrets_l_english.yml");
        let link = ws.path().join("localisation/linked_l_english.yml");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let link_uri = Url::from_file_path(&link).unwrap().to_string();
        let (_, real_uri) = write_loc_file(ws.path(), "localisation/real_l_english.yml");

        let locs =
            loc_locations_with(&[("my_thing", &link_uri, 1), ("my_thing_title", &real_uri, 9)]);
        let siblings = vec!["my_thing".to_string(), "my_thing_title".to_string()];
        let hit = resolve_sibling_site(&siblings, &locs, &edit_roots([ws.path()]), Lang::English)
            .expect("falls through to the real site");
        assert_eq!(hit.0.as_ref(), real_uri);
        assert_eq!(hit.1, "my_thing_title");
    }

    #[test]
    fn resolve_sibling_site_rejects_the_wrong_language_file() {
        let ws = tempfile::tempdir().unwrap();
        let (_, uri) = write_loc_file(ws.path(), "localisation/things_l_french.yml");
        let locs = loc_locations_with(&[("my_thing", &uri, 4)]);
        let siblings = vec!["my_thing".to_string()];
        assert!(
            resolve_sibling_site(&siblings, &locs, &edit_roots([ws.path()]), Lang::English)
                .is_none()
        );
    }

    #[test]
    fn resolve_sibling_site_tries_siblings_in_order() {
        let ws = tempfile::tempdir().unwrap();
        let (_, uri) = write_loc_file(ws.path(), "localisation/things_l_english.yml");
        let locs = loc_locations_with(&[("my_thing_title", &uri, 9)]);
        let siblings = vec!["my_thing".to_string(), "my_thing_title".to_string()];
        let hit = resolve_sibling_site(&siblings, &locs, &edit_roots([ws.path()]), Lang::English)
            .expect("a hit");
        assert_eq!(hit.1, "my_thing_title");
    }

    #[test]
    fn pick_lang_file_matches_by_name_and_sorts_for_determinism() {
        let ws = tempfile::tempdir().unwrap();
        let (z, _) = write_loc_file(ws.path(), "localisation/z_l_english.yml");
        let (a, _) = write_loc_file(ws.path(), "localisation/a_l_english.yml");
        let (fr, _) = write_loc_file(ws.path(), "localisation/other_l_french.yml");
        let picked = pick_lang_file(&[z, a.clone(), fr], Lang::English, &edit_roots([ws.path()]))
            .expect("a match");
        assert_eq!(picked, a);
    }

    #[test]
    fn pick_lang_file_none_when_no_file_matches() {
        let ws = tempfile::tempdir().unwrap();
        let (fr, _) = write_loc_file(ws.path(), "localisation/other_l_french.yml");
        assert!(pick_lang_file(&[fr], Lang::English, &edit_roots([ws.path()])).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn pick_lang_file_skips_a_symlinked_candidate() {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (target, _) = write_loc_file(outside.path(), "secrets_l_english.yml");
        let link = ws.path().join("localisation/a_l_english.yml");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (real, _) = write_loc_file(ws.path(), "localisation/b_l_english.yml");

        let picked = pick_lang_file(
            &[link, real.clone()],
            Lang::English,
            &edit_roots([ws.path()]),
        )
        .expect("the real file");
        assert_eq!(picked, real);
    }

    #[test]
    fn insert_stub_after_line_mid_file_inserts_on_the_next_line() {
        let text = "l_english:\n my_thing:0 \"Thing\"\n my_thing_title:0 \"Title\"\n";
        let (pos, insert) = insert_stub_after_line(
            text,
            1,
            " my_thing_desc:0 \"TODO\"",
            &PositionEncodingKind::UTF16,
        );
        assert_eq!(pos, Position::new(2, 0));
        assert_eq!(insert, " my_thing_desc:0 \"TODO\"\n");
    }

    #[test]
    fn insert_stub_after_line_at_eof_with_trailing_newline() {
        let text = "l_english:\n my_thing:0 \"Thing\"\n";
        let (pos, insert) = insert_stub_after_line(
            text,
            1,
            " my_thing_desc:0 \"TODO\"",
            &PositionEncodingKind::UTF16,
        );
        assert_eq!(pos, Position::new(2, 0));
        assert_eq!(insert, " my_thing_desc:0 \"TODO\"\n");
    }

    #[test]
    fn insert_stub_after_line_at_eof_without_trailing_newline() {
        let text = "l_english:\n my_thing:0 \"Thing\"";
        let (pos, insert) = insert_stub_after_line(
            text,
            1,
            " my_thing_desc:0 \"TODO\"",
            &PositionEncodingKind::UTF16,
        );
        assert_eq!(
            pos,
            Position::new(1, " my_thing:0 \"Thing\"".chars().count() as u32)
        );
        assert_eq!(insert, "\n my_thing_desc:0 \"TODO\"");
    }

    #[test]
    fn insert_stub_after_line_on_an_empty_file() {
        let (pos, insert) =
            insert_stub_after_line("", 0, " my_thing:0 \"TODO\"", &PositionEncodingKind::UTF16);
        assert_eq!(pos, Position::new(0, 0));
        assert_eq!(insert, " my_thing:0 \"TODO\"\n");
    }

    /// A file holding only a BOM and/or whitespace carries no loc entries, so it is
    /// still a header-initialization target (#1255) and its stray body must not
    /// survive the edit (as or after the header).
    #[test]
    fn is_effectively_empty_accepts_only_bom_and_whitespace() {
        for empty in [
            "",
            "\u{FEFF}",
            " \t\n ",
            "\u{FEFF}\u{FEFF} \n",
            "\u{FEFF} \t\n",
        ] {
            assert!(
                is_effectively_empty(empty),
                "expected effectively empty: {empty:?}"
            );
        }
        for content in ["l_english:", " my_key:0 \"x\"", "\nx", "\u{FEFF}l_english:"] {
            assert!(
                !is_effectively_empty(content),
                "expected content: {content:?}"
            );
        }
    }

    #[test]
    fn end_of_text_position_spans_an_empty_file() {
        assert_eq!(
            end_of_text_position("", &PositionEncodingKind::UTF16),
            Position::new(0, 0)
        );
    }

    #[test]
    fn end_of_text_position_spans_a_bom_only_file() {
        // The BOM occupies one UTF-16 unit, so the whole-content range ends just
        // past it; edits anchored there compose after that range non-overlapping.
        assert_eq!(
            end_of_text_position("\u{FEFF}", &PositionEncodingKind::UTF16),
            Position::new(0, 1)
        );
    }

    #[test]
    fn end_of_text_position_spans_a_whitespace_file_with_a_trailing_newline() {
        assert_eq!(
            end_of_text_position(" \n \t\n", &PositionEncodingKind::UTF16),
            Position::new(2, 0)
        );
    }

    #[test]
    fn end_of_text_position_lands_at_the_last_character_without_a_trailing_newline() {
        assert_eq!(
            end_of_text_position("ab\n\t", &PositionEncodingKind::UTF16),
            Position::new(1, 1)
        );
    }

    #[test]
    fn end_of_text_position_columns_are_encoding_aware() {
        let text = "x\n\u{1F600}";
        assert_eq!(
            end_of_text_position(text, &PositionEncodingKind::UTF16),
            Position::new(1, 2),
            "a non-BMP char on the last line takes two UTF-16 units"
        );
        assert_eq!(
            end_of_text_position(text, &PositionEncodingKind::UTF32),
            Position::new(1, 1)
        );
    }

    #[test]
    fn insert_stub_after_line_uses_the_files_dominant_crlf_eol() {
        let text = "l_english:\r\n my_thing:0 \"Thing\"\r\n";
        let (pos, insert) = insert_stub_after_line(
            text,
            1,
            " my_thing_desc:0 \"TODO\"",
            &PositionEncodingKind::UTF16,
        );
        assert_eq!(pos, Position::new(2, 0));
        assert_eq!(insert, " my_thing_desc:0 \"TODO\"\r\n");
    }

    #[test]
    fn insert_stub_after_line_end_column_is_encoding_aware() {
        // A non-BMP char before the anchor column makes the UTF-16 column (3)
        // encoding, since loc lines can carry non-ASCII translations.
        let text = "l_english:\n 😀desc:0 \"x\"";
        let (pos, _) =
            insert_stub_after_line(text, 1, " k:0 \"TODO\"", &PositionEncodingKind::UTF16);
        assert_eq!(
            pos,
            Position::new(1, " 😀desc:0 \"x\"".encode_utf16().count() as u32)
        );
    }

    #[test]
    fn insert_stub_after_line_degrades_when_the_anchor_line_no_longer_exists() {
        let text = "l_english:\n my_thing:0 \"Thing\"\n";
        let (pos, insert) = insert_stub_after_line(
            text,
            10,
            " my_thing_desc:0 \"TODO\"",
            &PositionEncodingKind::UTF16,
        );
        assert_eq!(
            pos,
            Position::new(10, 0),
            "anchors past the real content rather than splicing mid-file"
        );
        assert_eq!(insert, "\n my_thing_desc:0 \"TODO\"");
    }

    #[test]
    fn resolve_loc_insert_target_resolves_independently_per_language() {
        let ws = tempfile::tempdir().unwrap();
        let roots = edit_roots([ws.path()]);
        let (_, uri) = write_loc_file(ws.path(), "localisation/things_l_english.yml");
        let locs = loc_locations_with(&[("my_thing", &uri, 4)]);
        let siblings = vec!["my_thing".to_string()];

        let en = resolve_loc_insert_target(Lang::English, &siblings, &locs, &roots, &[], ws.path())
            .unwrap();
        assert_eq!(
            en,
            LocInsertTarget::ExistingFileAfterKey {
                uri: uri.parse().unwrap(),
                key: "my_thing".into(),
            }
        );

        let fr = resolve_loc_insert_target(Lang::French, &siblings, &locs, &roots, &[], ws.path())
            .unwrap();
        assert_eq!(
            fr,
            LocInsertTarget::NewFile {
                uri: Url::from_file_path(generated_loc_file_path(ws.path(), Lang::French)).unwrap(),
            }
        );
    }

    #[test]
    fn resolve_loc_insert_target_prefers_sibling_over_existing_file_over_new() {
        let ws = tempfile::tempdir().unwrap();
        let roots = edit_roots([ws.path()]);
        let (path, uri) = write_loc_file(ws.path(), "localisation/things_l_english.yml");
        let siblings = vec!["my_thing".to_string()];

        let locs = loc_locations_with(&[("my_thing", &uri, 4)]);
        let target =
            resolve_loc_insert_target(Lang::English, &siblings, &locs, &roots, &[], ws.path())
                .unwrap();
        assert_eq!(
            target,
            LocInsertTarget::ExistingFileAfterKey {
                uri: uri.parse().unwrap(),
                key: "my_thing".into(),
            }
        );

        let empty_locs = crate::LocLocations::default();
        let target = resolve_loc_insert_target(
            Lang::English,
            &siblings,
            &empty_locs,
            &roots,
            std::slice::from_ref(&path),
            ws.path(),
        )
        .unwrap();
        assert_eq!(
            target,
            LocInsertTarget::ExistingFileAppend {
                uri: Url::from_file_path(&path).unwrap(),
            }
        );

        let target = resolve_loc_insert_target(
            Lang::English,
            &siblings,
            &empty_locs,
            &roots,
            &[],
            ws.path(),
        )
        .unwrap();
        assert_eq!(
            target,
            LocInsertTarget::NewFile {
                uri: Url::from_file_path(generated_loc_file_path(ws.path(), Lang::English))
                    .unwrap(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_loc_insert_target_refuses_a_new_file_under_a_symlinked_loc_dir() {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.path().join("localisation")).unwrap();
        assert_eq!(
            resolve_loc_insert_target(
                Lang::English,
                &[],
                &crate::LocLocations::default(),
                &edit_roots([ws.path()]),
                &[],
                ws.path(),
            ),
            None
        );
    }

    #[test]
    fn new_file_owners_assigns_the_first_candidate_per_language() {
        let en_uri: Url = "file:///ws/localisation/cwtools_generated_l_english.yml"
            .parse()
            .unwrap();
        let fr_uri: Url = "file:///ws/localisation/cwtools_generated_l_french.yml"
            .parse()
            .unwrap();
        let targets = [
            vec![(Lang::English, LocInsertTarget::NewFile { uri: en_uri })],
            vec![
                (
                    Lang::English,
                    LocInsertTarget::NewFile {
                        uri: "file:///ws/localisation/cwtools_generated_l_english.yml"
                            .parse()
                            .unwrap(),
                    },
                ),
                (Lang::French, LocInsertTarget::NewFile { uri: fr_uri }),
            ],
        ];
        let owners = new_file_owners(targets.iter().map(Vec::as_slice));
        assert_eq!(
            owners.get(&Lang::English),
            Some(&0),
            "the first candidate claims english"
        );
        assert_eq!(
            owners.get(&Lang::French),
            Some(&1),
            "only the second candidate needs french at all"
        );
    }

    fn edit_texts<'a>(ops: &'a [DocumentChangeOperation], uri: &Url) -> Vec<&'a str> {
        ops.iter()
            .filter_map(|op| match op {
                DocumentChangeOperation::Edit(e) if &e.text_document.uri == uri => Some(e),
                _ => None,
            })
            .flat_map(|e| &e.edits)
            .filter_map(|c| match c {
                OneOf::Left(t) => Some(t.new_text.as_str()),
                OneOf::Right(_) => None,
            })
            .collect()
    }

    #[test]
    fn build_create_loc_key_batch_dedupes_a_shared_new_file() {
        // #142: two candidates resolving to the identical not-yet-existing `NewFile`
        // target must not each carry their own `CreateFile` + header; the merged action
        // opens the file once, with both stubs.
        let uri: Url = "file:///ws/localisation/cwtools_generated_l_english.yml"
            .parse()
            .unwrap();
        let diag_a = super::super::payload::create_loc_key_diag();
        let diag_b = super::super::payload::create_loc_key_diag();
        let candidates = vec![
            LocKeyCandidate {
                diag: &diag_a,
                title: "Create localisation key my_thing_desc".to_string(),
                key: "my_thing_desc".to_string(),
                targets: vec![(Lang::English, LocInsertTarget::NewFile { uri: uri.clone() })],
            },
            LocKeyCandidate {
                diag: &diag_b,
                title: "Create localisation key my_thing_title".to_string(),
                key: "my_thing_title".to_string(),
                targets: vec![(Lang::English, LocInsertTarget::NewFile { uri: uri.clone() })],
            },
        ];

        let actions = build_create_loc_key_batch(&candidates, &HashSet::new(), |_, _, _, _| {
            panic!("no existing-file target in this batch")
        });

        assert_eq!(
            actions.len(),
            1,
            "the second candidate must join the first's action, not get its own"
        );
        assert_eq!(
            actions[0].diagnostics.as_ref().map(Vec::len),
            Some(2),
            "both diagnostics must be attributed to the merged action"
        );

        let Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(ops)),
            ..
        }) = &actions[0].edit
        else {
            panic!("expected document_changes operations");
        };
        assert_eq!(
            ops.len(),
            2,
            "one Create plus a SINGLE TextDocumentEdit for the file, not one per key: {ops:?}"
        );
        assert!(matches!(
            ops[0],
            DocumentChangeOperation::Op(ResourceOp::Create(_))
        ));

        let texts = edit_texts(ops, &uri);
        assert_eq!(
            texts.len(),
            2,
            "the header edit and the folded stub, as two edits in one TextDocumentEdit: {texts:?}"
        );
        let combined = texts.concat();
        assert_eq!(
            combined.matches("l_english:").count(),
            1,
            "the header must appear exactly once: {combined:?}"
        );
        assert!(combined.contains(" my_thing_desc:0 "), "got: {combined:?}");
        assert!(combined.contains(" my_thing_title:0 "), "got: {combined:?}");
    }

    #[test]
    fn build_create_loc_key_batch_folds_a_whole_candidate_across_mixed_tiers() {
        // #142 follow-up: English has a loc file but French does not. A candidate whose French
        // half folds into another's action must not keep a separate English action, or
        // applying only one would leave its diagnostic still firing.
        let en_uri: Url = "file:///ws/localisation/things_l_english.yml"
            .parse()
            .unwrap();
        let fr_uri: Url = "file:///ws/localisation/cwtools_generated_l_french.yml"
            .parse()
            .unwrap();
        let diag_a = super::super::payload::create_loc_key_diag();
        let diag_b = super::super::payload::create_loc_key_diag();
        let candidates = vec![
            LocKeyCandidate {
                diag: &diag_a,
                title: "Create localisation key my_thing_desc".to_string(),
                key: "my_thing_desc".to_string(),
                targets: vec![
                    (
                        Lang::English,
                        LocInsertTarget::ExistingFileAppend {
                            uri: en_uri.clone(),
                        },
                    ),
                    (
                        Lang::French,
                        LocInsertTarget::NewFile {
                            uri: fr_uri.clone(),
                        },
                    ),
                ],
            },
            LocKeyCandidate {
                diag: &diag_b,
                title: "Create localisation key my_thing_title".to_string(),
                key: "my_thing_title".to_string(),
                targets: vec![
                    (
                        Lang::English,
                        LocInsertTarget::ExistingFileAppend {
                            uri: en_uri.clone(),
                        },
                    ),
                    (
                        Lang::French,
                        LocInsertTarget::NewFile {
                            uri: fr_uri.clone(),
                        },
                    ),
                ],
            },
        ];

        let actions = build_create_loc_key_batch(
            &candidates,
            &HashSet::new(),
            |target, _lang, write_header, stub| {
                let LocInsertTarget::ExistingFileAppend { uri } = target else {
                    panic!("no other existing-file target in this batch: {target:?}");
                };
                debug_assert!(!write_header);
                Some(vec![insert_edit_op(
                    uri.clone(),
                    Some(7),
                    Position::new(3, 0),
                    format!("{stub}\n"),
                )])
            },
        );

        assert_eq!(
            actions.len(),
            1,
            "the second candidate's whole fix must fold into the first's action, \
             not split across two: got {} actions",
            actions.len()
        );
        assert_eq!(
            actions[0].diagnostics.as_ref().map(Vec::len),
            Some(2),
            "both diagnostics must be attributed to the merged action"
        );

        let Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(ops)),
            ..
        }) = &actions[0].edit
        else {
            panic!("expected document_changes operations");
        };
        let creates = ops
            .iter()
            .filter(|op| matches!(op, DocumentChangeOperation::Op(ResourceOp::Create(_))))
            .count();
        assert_eq!(
            creates, 1,
            "exactly one Create for the shared file: {ops:?}"
        );

        let en_texts = edit_texts(ops, &en_uri);
        assert_eq!(
            en_texts.len(),
            2,
            "both candidates' English edits must land in the merged action: {ops:?}"
        );
        assert!(en_texts.iter().any(|t| t.contains(" my_thing_desc:0 ")));
        assert!(en_texts.iter().any(|t| t.contains(" my_thing_title:0 ")));

        let fr_texts = edit_texts(ops, &fr_uri);
        assert_eq!(
            fr_texts.len(),
            2,
            "the header edit and the folded stub, as two edits in one TextDocumentEdit: {fr_texts:?}"
        );
        let fr_combined = fr_texts.concat();
        assert_eq!(
            fr_combined.matches("l_french:").count(),
            1,
            "the French header must appear exactly once: {fr_combined:?}"
        );
        assert!(
            fr_combined.contains(" my_thing_desc:0 "),
            "got: {fr_combined:?}"
        );
        assert!(
            fr_combined.contains(" my_thing_title:0 "),
            "got: {fr_combined:?}"
        );
    }

    #[test]
    fn build_create_loc_key_batch_commits_nothing_when_a_candidate_fails_partway() {
        let en_uri: Url = "file:///ws/localisation/things_l_english.yml"
            .parse()
            .unwrap();
        let fr_uri: Url = "file:///ws/localisation/unreadable_l_german.yml"
            .parse()
            .unwrap();
        let gen_fr_uri: Url = "file:///ws/localisation/cwtools_generated_l_french.yml"
            .parse()
            .unwrap();
        let diag_a = super::super::payload::create_loc_key_diag();
        let diag_b = super::super::payload::create_loc_key_diag();
        let diag_c = super::super::payload::create_loc_key_diag();
        let empty_targets: HashSet<(Lang, String)> =
            [(Lang::English, en_uri.to_string())].into_iter().collect();

        fn candidate<'d>(
            diag: &'d Diagnostic,
            key: &'static str,
            targets: Vec<(Lang, LocInsertTarget)>,
        ) -> LocKeyCandidate<'d> {
            LocKeyCandidate {
                diag,
                title: format!("Create localisation key {key}"),
                key: key.to_string(),
                targets,
            }
        }
        let append = |uri: &Url| LocInsertTarget::ExistingFileAppend { uri: uri.clone() };
        let candidates = vec![
            candidate(
                &diag_a,
                "my_thing",
                vec![(
                    Lang::French,
                    LocInsertTarget::NewFile {
                        uri: gen_fr_uri.clone(),
                    },
                )],
            ),
            candidate(
                &diag_b,
                "my_thing_name",
                vec![
                    (Lang::English, append(&en_uri)),
                    (Lang::German, append(&fr_uri)),
                    (
                        Lang::French,
                        LocInsertTarget::NewFile {
                            uri: gen_fr_uri.clone(),
                        },
                    ),
                ],
            ),
            candidate(
                &diag_c,
                "my_thing_title",
                vec![
                    (Lang::English, append(&en_uri)),
                    (
                        Lang::French,
                        LocInsertTarget::NewFile {
                            uri: gen_fr_uri.clone(),
                        },
                    ),
                ],
            ),
        ];

        let actions = build_create_loc_key_batch(
            &candidates,
            &empty_targets,
            |target, lang, write_header, stub| {
                let LocInsertTarget::ExistingFileAppend { uri } = target else {
                    panic!("the batch must pass only existing-file targets: {target:?}");
                };
                if uri == &fr_uri {
                    return None; // no text snapshot: the candidate cannot build
                }
                let text = if write_header {
                    format!("\u{FEFF}l_{lang}:\n{stub}\n")
                } else {
                    format!("{stub}\n")
                };
                Some(vec![insert_edit_op(
                    uri.clone(),
                    Some(9),
                    Position::new(0, 0),
                    text,
                )])
            },
        );

        assert_eq!(
            actions.len(),
            1,
            "the failing candidate is dropped; the owner action absorbs the third: {actions:?}"
        );
        assert_eq!(
            actions[0].diagnostics.as_ref().map(Vec::len),
            Some(2),
            "the folded candidate's diagnostic lands with the owner's"
        );

        let Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(ops)),
            ..
        }) = &actions[0].edit
        else {
            panic!("expected document_changes operations");
        };

        let en_texts = edit_texts(ops, &en_uri);
        assert_eq!(
            en_texts.len(),
            1,
            "successful folded candidate: {en_texts:?}"
        );
        assert!(
            en_texts[0].contains("l_english:") && en_texts[0].contains("\u{FEFF}"),
            "the successful folded candidate initializes the empty file: {en_texts:?}"
        );
        let en_combined = en_texts.concat();
        assert_eq!(
            en_combined.matches("l_english:").count(),
            1,
            "exactly one header across the merged action: {en_combined:?}"
        );
        assert!(
            en_combined.contains(" my_thing_title:0 "),
            "got: {en_combined:?}"
        );
        assert!(
            !en_combined.contains(" my_thing_name:0 "),
            "every op of the failed candidate is discarded: {en_combined:?}"
        );

        let creates = ops
            .iter()
            .filter(|op| matches!(op, DocumentChangeOperation::Op(ResourceOp::Create(_))))
            .count();
        assert_eq!(creates, 1);
        let fr_texts = edit_texts(ops, &gen_fr_uri);
        assert_eq!(fr_texts.len(), 2, "header edit + folded stub: {fr_texts:?}");
        assert_eq!(
            fr_texts.concat().matches("l_french:").count(),
            1,
            "new-file header still exactly once: {fr_texts:?}"
        );
    }
}
