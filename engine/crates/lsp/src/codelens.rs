use serde_json::{Value, json};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{CodeLens, CodeLensParams, Command, Location, Url};

use crate::Backend;
use crate::lines::DocLines;
use crate::navigation::{dedup_locations, source_range_without_text};
use crate::paths::parse_uri;

const SHOW_REFERENCES_COMMAND: &str = "cwtools.showReferences";

struct LensData<'a> {
    uri: &'a str,
    type_name: &'a str,
    instance_name: &'a str,
    line: u32,
    column: u16,
}

impl Backend {
    fn indexed_code_lens_use_sites(
        &self,
        type_name: &str,
        instance_name: &str,
    ) -> Vec<cwtools_info::UseSite> {
        let mut sites = self
            .state
            .documents
            .lock()
            .reference_sites(type_name, instance_name);
        let mut indexed_sites = {
            let info = self.state.info_service.read();
            let mut indexed = info.reference_index.references(type_name, instance_name);
            indexed.extend(info.alias_key_index.references_ci(type_name, instance_name));
            indexed
        };
        let documents = self.state.documents.lock();
        indexed_sites.retain(|site| !documents.contains_key(site.file.as_ref()));
        sites.extend(indexed_sites);
        sites
    }

    pub(crate) async fn code_lens_impl(
        &self,
        params: CodeLensParams,
    ) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri.to_string();
        let instances = {
            let info = self.state.info_service.read();
            info.type_index
                .instances_in_file(&uri)
                .into_iter()
                .map(|(type_name, instance)| {
                    (
                        type_name.to_string(),
                        instance.name.clone(),
                        instance.location,
                    )
                })
                .collect::<Vec<_>>()
        };
        if instances.is_empty() {
            return Ok(None);
        }

        let text = self.file_text_for(&uri).await;
        let lines = text
            .as_deref()
            .map(|text| DocLines::new(text, self.position_encoding()));
        let lenses = instances
            .into_iter()
            .map(|(type_name, instance_name, location)| CodeLens {
                range: self.source_range_with_lines(
                    lines.as_ref(),
                    location.line.saturating_sub(1),
                    location.col as u32,
                    "",
                ),
                command: None,
                data: Some(json!({
                    "uri": uri,
                    "typeName": type_name,
                    "instanceName": instance_name,
                    "line": location.line,
                    "column": location.col,
                })),
            })
            .collect();
        Ok(Some(lenses))
    }

    pub(crate) async fn code_lens_resolve_impl(&self, mut lens: CodeLens) -> Result<CodeLens> {
        let Some(data) = lens_data(lens.data.as_ref()) else {
            return Ok(lens);
        };
        let Some(uri) = Url::parse(data.uri).ok() else {
            return Ok(lens);
        };
        let (location, type_names) = {
            let info = self.state.info_service.read();
            let Some(instance) = info.type_index.instance_at(
                data.uri,
                data.type_name,
                data.instance_name,
                data.line,
                data.column,
            ) else {
                return Ok(lens);
            };
            let type_names = info
                .type_index
                .instance_type_names_in_file(
                    data.uri,
                    data.type_name,
                    &instance.name,
                    instance.location,
                )
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            (instance.location, type_names)
        };

        let encoding = self.position_encoding();
        let text = self.file_text_for(data.uri).await;
        let lines = text
            .as_deref()
            .map(|text| DocLines::new(text, encoding.clone()));
        let range = self.source_range_with_lines(
            lines.as_ref(),
            location.line.saturating_sub(1),
            location.col as u32,
            "",
        );
        // The per-open-document overlay and closed-file indexes are keyed by
        // reference type and lowercase name, so resolving one lens never walks
        // unrelated ASTs or reads a referencing file (#472, #865).
        // `source_range_without_text` converts the parser's char column rather
        // than using
        // `DocLines`, which differs only on a line carrying astral-plane
        // characters ahead of the reference — and is already what every
        // unreadable file falls back to.
        let mut locations: Vec<Location> = Vec::new();
        for type_name in type_names {
            let sites = self.indexed_code_lens_use_sites(&type_name, data.instance_name);
            for site in sites {
                locations.push(Location {
                    uri: parse_uri(&site.file, &uri),
                    range: source_range_without_text(
                        site.value.line.saturating_sub(1),
                        site.value.col as u32,
                        data.instance_name,
                        &encoding,
                    ),
                });
            }
        }
        let locations = dedup_locations(locations);
        let count = locations.len();
        let title = if count == 1 {
            "1 reference".to_string()
        } else {
            format!("{count} references")
        };

        lens.range = range;
        lens.command = Some(Command::new(
            title,
            SHOW_REFERENCES_COMMAND.to_string(),
            Some(vec![
                Value::String(uri.to_string()),
                json!(range.start),
                json!(locations),
            ]),
        ));
        Ok(lens)
    }
}

fn lens_data(data: Option<&Value>) -> Option<LensData<'_>> {
    let data = data?.as_object()?;
    Some(LensData {
        uri: data.get("uri")?.as_str()?,
        type_name: data.get("typeName")?.as_str()?,
        instance_name: data.get("instanceName")?.as_str()?,
        line: u32::try_from(data.get("line")?.as_u64()?).ok()?,
        column: u16::try_from(data.get("column")?.as_u64()?).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cwtools_parser::parser::parse_string;
    use serde_json::json;
    use tower_lsp::lsp_types::{
        PartialResultParams, Position, TextDocumentIdentifier, WorkDoneProgressParams,
    };

    use super::*;
    use crate::Backend;
    use crate::access::recorded_reads_under;
    use crate::navigation::test_rules::type_ref_ruleset;
    use crate::navigation::{ast_leaf_visits, reset_ast_leaf_visits};
    use crate::paths::{path_to_uri, workspace_prefix_of};
    use crate::state::ParsedDoc;
    use crate::testing::test_backend;

    const DEFINITION: &str = "foo = { id = my_instance }\n";
    const REFERENCE: &str = "use_type = { base = \"my_instance\" }\n";

    struct Fixture {
        backend: Backend,
        workspace: tempfile::TempDir,
        definition_uri: String,
        reference_uri: String,
    }

    /// A workspace with the definition file **open** and the referencing file
    /// left **closed** on disk, which is the arrangement that used to make
    /// every lens resolve read `use.txt` (#472).
    fn fixture() -> Fixture {
        let workspace = tempfile::tempdir().expect("tempdir");
        let events = workspace.path().join("events");
        std::fs::create_dir_all(&events).expect("events dir");
        std::fs::write(events.join("def.txt"), DEFINITION).expect("write def");
        std::fs::write(events.join("use.txt"), REFERENCE).expect("write use");

        let backend = test_backend();
        {
            let mut config = backend.state.config.write();
            config.workspace_roots = vec![workspace.path().to_path_buf()];
            config.refresh_roots();
            config.workspace_prefix = Some(workspace_prefix_of(&path_to_uri(workspace.path())));
        }
        backend.state.rules.write().ruleset = Some(Arc::new(type_ref_ruleset()));

        let definition_uri = path_to_uri(&events.join("def.txt"));
        let reference_uri = path_to_uri(&events.join("use.txt"));

        // The referencing file is indexed from disk once, as a workspace scan
        // would, and then never opened.
        let parsed = parse_string(REFERENCE, &backend.state.string_table);
        backend.index_parsed_file(&reference_uri, &parsed, None);

        let parsed = Arc::new(parse_string(DEFINITION, &backend.state.string_table));
        backend
            .state
            .documents
            .lock()
            .open(
                definition_uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(DEFINITION),
                    ast: Some(Arc::clone(&parsed)),
                    ast_version: Some(1),
                    ast_source_bytes: DEFINITION.len(),
                    loc_cache: None,
                },
            )
            .expect("open definition");
        backend.index_parsed_file(&definition_uri, &parsed, Some(1));
        backend.update_doc_tokens(&definition_uri, Some(&parsed));

        Fixture {
            backend,
            workspace,
            definition_uri,
            reference_uri,
        }
    }

    /// Resolve the lens on `my_instance` and return its command title plus the
    /// locations baked into the command arguments.
    async fn resolve(fixture: &Fixture) -> (String, Vec<Location>) {
        let lenses = fixture
            .backend
            .code_lens_impl(CodeLensParams {
                text_document: TextDocumentIdentifier {
                    uri: Url::parse(&fixture.definition_uri).expect("definition url"),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .expect("code lens request")
            .expect("lenses for a file with one definition");
        let lens = lenses
            .into_iter()
            .find(|lens| {
                lens.data
                    .as_ref()
                    .and_then(|data| data.get("instanceName"))
                    .and_then(Value::as_str)
                    == Some("my_instance")
            })
            .expect("a lens for my_instance");
        let resolved = fixture
            .backend
            .code_lens_resolve_impl(lens)
            .await
            .expect("resolve");
        let command = resolved.command.expect("a resolved lens carries a command");
        let locations: Vec<Location> = command
            .arguments
            .as_ref()
            .and_then(|args| args.get(2))
            .map(|value| serde_json::from_value(value.clone()).expect("locations"))
            .expect("locations argument");
        (command.title, locations)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolving_a_lens_reads_no_referencing_file() {
        let fixture = fixture();
        let before = recorded_reads_under(fixture.workspace.path());

        let (title, locations) = resolve(&fixture).await;
        assert_eq!(title, "1 reference");
        assert_eq!(locations.len(), 1);
        let location = &locations[0];
        assert_eq!(location.uri.to_string(), fixture.reference_uri);
        // `use_type = { base = "my_instance" }` — the opening quote sits at
        // column 20, so the name itself runs from 21 to 32. Getting that from
        // the index rather than a text scan is the whole point.
        assert_eq!(location.range.start, Position::new(0, 21));
        assert_eq!(location.range.end, Position::new(0, 32));

        assert_eq!(
            recorded_reads_under(fixture.workspace.path()),
            before,
            "lens resolution must answer from indexed data, not the disk"
        );

        // A keystroke in the definition file, as `debounced_validate` leaves
        // the state: new text, reindexed, tokens refreshed.
        let edited = "foo = { id = my_instance }\n\n";
        let parsed = Arc::new(parse_string(edited, &fixture.backend.state.string_table));
        {
            let mut docs = fixture.backend.state.documents.lock();
            docs.change(&fixture.definition_uri, 2, Arc::from(edited))
                .expect("change");
            docs.set_ast(&fixture.definition_uri, 2, Arc::clone(&parsed));
        }
        fixture
            .backend
            .index_parsed_file(&fixture.definition_uri, &parsed, Some(2));
        fixture
            .backend
            .update_doc_tokens(&fixture.definition_uri, Some(&parsed));

        let (title, locations) = resolve(&fixture).await;
        assert_eq!(title, "1 reference");
        assert_eq!(locations.len(), 1);
        assert_eq!(
            recorded_reads_under(fixture.workspace.path()),
            before,
            "typing must not put the referencing file back on the disk path"
        );

        // Reading the referencing file *is* recorded, so the assertions above
        // cannot pass because the instrumentation went quiet.
        let roots = fixture.backend.state.config.read().authorized_roots.clone();
        assert!(
            crate::access::read_authorized_text(
                &fixture.reference_uri,
                &roots,
                crate::access::MAX_URI_READ_BYTES,
            )
            .is_some(),
            "the fixture's referencing file is readable"
        );
        assert_eq!(
            recorded_reads_under(fixture.workspace.path()),
            before + 1,
            "the read recorder must see a real read"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn moving_definition_refreshes_lenses_without_export_or_reference_changes() {
        let fixture = fixture();
        let uri = &fixture.definition_uri;
        let before = {
            let info = fixture.backend.state.info_service.read();
            (
                info.export_fingerprint(uri),
                info.file_reference_sites(uri).len(),
                info.reference_fingerprint(uri),
            )
        };
        let edited = format!("\n{DEFINITION}");
        let parsed = Arc::new(parse_string(&edited, &fixture.backend.state.string_table));
        {
            let mut docs = fixture.backend.state.documents.lock();
            docs.change(uri, 2, Arc::from(edited)).unwrap();
            assert!(docs.set_ast(uri, 2, Arc::clone(&parsed)));
        }
        fixture.backend.index_parsed_file(uri, &parsed, Some(2));
        let after = {
            let info = fixture.backend.state.info_service.read();
            (
                info.export_fingerprint(uri),
                info.file_reference_sites(uri).len(),
                info.reference_fingerprint(uri),
            )
        };
        assert_eq!(before.0, after.0);
        assert_eq!((before.1, after.1), (0, 0));
        assert!(crate::validate::code_lens_refresh_needed(
            false, false, before.2, after.2
        ));
        let lenses = fixture
            .backend
            .code_lens_impl(CodeLensParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap()
            .unwrap();
        assert!(lenses.iter().all(|lens| lens.range.start.line == 1));
        let (title, locations) = resolve(&fixture).await;
        assert_eq!(title, "1 reference");
        assert_eq!(locations.len(), 1);
    }

    #[tokio::test]
    async fn ordinary_open_reference_case_matching_matches_the_closed_index() {
        let fixture = fixture();
        let uri = "file:///events/case-mismatch.txt";
        let text = REFERENCE.replace("my_instance", "MY_INSTANCE");
        let parsed = Arc::new(parse_string(&text, &fixture.backend.state.string_table));
        fixture
            .backend
            .state
            .documents
            .lock()
            .open(
                uri.to_string(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(text.as_str()),
                    ast: Some(Arc::clone(&parsed)),
                    ast_version: Some(1),
                    ast_source_bytes: text.len(),
                    loc_cache: None,
                },
            )
            .expect("open case-mismatched reference");
        fixture.backend.index_parsed_file(uri, &parsed, Some(1));
        // This fixture's `base` rule is also an alias pattern; isolate the
        // ordinary type-reference index for exact-spelling parity here.
        fixture.backend.state.info_service.write().alias_key_index =
            cwtools_info::ReferenceIndex::default();
        let indexed_sites = fixture
            .backend
            .state
            .info_service
            .read()
            .file_reference_sites(uri);
        assert_eq!(indexed_sites.len(), 1);
        assert_eq!(indexed_sites[0].name, "MY_INSTANCE");
        assert!(!indexed_sites[0].case_insensitive);
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(uri, 1, parsed, indexed_sites);

        let open_legacy = fixture.backend.collect_use_sites("my_type", "my_instance");
        let open_indexed = fixture
            .backend
            .indexed_code_lens_use_sites("my_type", "my_instance");
        assert_eq!(open_indexed.len(), open_legacy.len());
        assert!(open_indexed.iter().all(|site| site.file.as_ref() != uri));

        fixture.backend.state.documents.lock().remove(uri);
        let closed_legacy = fixture.backend.collect_use_sites("my_type", "my_instance");
        let closed_indexed = fixture
            .backend
            .indexed_code_lens_use_sites("my_type", "my_instance");
        assert_eq!(closed_indexed.len(), closed_legacy.len());
        assert!(closed_indexed.iter().all(|site| site.file.as_ref() != uri));
    }

    #[tokio::test]
    async fn same_reference_edit_keeps_overlay_until_current_ast_is_accepted() {
        let fixture = fixture();
        let uri = fixture.reference_uri.clone();
        let parsed = Arc::new(parse_string(REFERENCE, &fixture.backend.state.string_table));
        fixture
            .backend
            .state
            .documents
            .lock()
            .open(
                uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(REFERENCE),
                    ast: Some(Arc::clone(&parsed)),
                    ast_version: Some(1),
                    ast_source_bytes: REFERENCE.len(),
                    loc_cache: None,
                },
            )
            .expect("open indexed reference");
        fixture.backend.index_parsed_file(&uri, &parsed, Some(1));
        let sites = fixture
            .backend
            .state
            .info_service
            .read()
            .file_reference_sites(&uri);
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(&uri, 1, parsed, sites);
        let fingerprint_before = fixture
            .backend
            .state
            .info_service
            .read()
            .reference_fingerprint(&uri);

        let edited = format!("{REFERENCE}# comment only\n");
        fixture
            .backend
            .state
            .documents
            .lock()
            .change(&uri, 2, Arc::from(edited.as_str()))
            .expect("edit reference document");
        assert_eq!(
            fixture
                .backend
                .state
                .documents
                .lock()
                .reference_sites("my_type", "my_instance")
                .len(),
            1,
            "the last accepted AST remains usable during debounce"
        );

        let reparsed = Arc::new(parse_string(&edited, &fixture.backend.state.string_table));
        fixture.backend.index_parsed_file(&uri, &reparsed, Some(2));
        let sites = fixture
            .backend
            .state
            .info_service
            .read()
            .file_reference_sites(&uri);
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(&uri, 2, reparsed, sites);
        let fingerprint_after = fixture
            .backend
            .state
            .info_service
            .read()
            .reference_fingerprint(&uri);
        assert_eq!(fingerprint_before, fingerprint_after);
        assert!(!crate::validate::code_lens_refresh_needed(
            false,
            false,
            fingerprint_before,
            fingerprint_after,
        ));
        assert_eq!(
            fixture
                .backend
                .state
                .documents
                .lock()
                .reference_sites("my_type", "my_instance")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn debounce_overlay_uses_parsed_ast_when_disk_index_is_stale() {
        let fixture = fixture();
        let uri = fixture.reference_uri.clone();
        let unsaved = REFERENCE.replace("my_instance", "unsaved_instance");
        fixture
            .backend
            .state
            .documents
            .lock()
            .open(
                uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(unsaved.as_str()),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("open unsaved reference before its first parse");
        assert_eq!(
            fixture
                .backend
                .state
                .info_service
                .read()
                .file_reference_sites(&uri)[0]
                .name,
            "my_instance",
            "the workspace index still describes the on-disk file"
        );

        let parsed = Arc::new(parse_string(&unsaved, &fixture.backend.state.string_table));
        let sites = fixture
            .backend
            .reference_sites_for_open_ast(&uri, &parsed)
            .expect("current rules classify the parsed AST");
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(&uri, 1, parsed, sites);

        let docs = fixture.backend.state.documents.lock();
        assert_eq!(docs.reference_sites("my_type", "unsaved_instance").len(), 1);
        assert!(docs.reference_sites("my_type", "my_instance").is_empty());
    }

    #[tokio::test]
    async fn did_open_first_ast_refreshes_lenses_hidden_by_closed_index_filter() {
        let fixture = fixture();
        let uri = fixture.reference_uri.clone();
        let fingerprint_before = fixture
            .backend
            .state
            .info_service
            .read()
            .reference_fingerprint(&uri);
        fixture
            .backend
            .state
            .documents
            .lock()
            .open(
                uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(REFERENCE),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("didOpen before initial parse");
        assert!(
            fixture
                .backend
                .indexed_code_lens_use_sites("my_type", "my_instance")
                .iter()
                .all(|site| site.file.as_ref() != uri)
        );

        let parsed = Arc::new(parse_string(REFERENCE, &fixture.backend.state.string_table));
        let sites = fixture
            .backend
            .reference_sites_for_open_ast(&uri, &parsed)
            .expect("current rules classify the parsed AST");
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(&uri, 1, parsed, sites);
        assert_eq!(
            fixture
                .backend
                .indexed_code_lens_use_sites("my_type", "my_instance")
                .iter()
                .filter(|site| site.file.as_ref() == uri)
                .count(),
            1,
            "the accepted open AST supplies the reference overlay"
        );
        let fingerprint_after = fixture
            .backend
            .state
            .info_service
            .read()
            .reference_fingerprint(&uri);
        assert_eq!(fingerprint_before, fingerprint_after);
        assert!(crate::validate::code_lens_refresh_needed(
            true,
            false,
            fingerprint_before,
            fingerprint_after,
        ));
        assert!(!crate::validate::code_lens_refresh_needed(
            false,
            false,
            fingerprint_after,
            fingerprint_after,
        ));
    }

    #[tokio::test]
    async fn post_scan_refresh_reclassifies_the_accepted_open_ast_not_disk_index() {
        let fixture = fixture();
        let uri = fixture.reference_uri.clone();
        let unsaved = REFERENCE.replace("my_instance", "unsaved_instance");
        let parsed = Arc::new(parse_string(&unsaved, &fixture.backend.state.string_table));
        fixture
            .backend
            .state
            .documents
            .lock()
            .open(
                uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(unsaved.as_str()),
                    ast: Some(Arc::clone(&parsed)),
                    ast_version: Some(1),
                    ast_source_bytes: unsaved.len(),
                    loc_cache: None,
                },
            )
            .expect("open unsaved reference");
        fixture.backend.index_parsed_file(&uri, &parsed, Some(1));
        let sites = fixture
            .backend
            .state
            .info_service
            .read()
            .file_reference_sites(&uri);
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(&uri, 1, parsed, sites);

        // A scan indexes the on-disk text with no document-version guard.
        let disk_ast = parse_string(REFERENCE, &fixture.backend.state.string_table);
        fixture.backend.index_parsed_file(&uri, &disk_ast, None);
        assert_eq!(
            fixture
                .backend
                .state
                .info_service
                .read()
                .file_reference_sites(&uri)[0]
                .name,
            "my_instance"
        );

        fixture.backend.refresh_open_reference_sites();
        let docs = fixture.backend.state.documents.lock();
        assert_eq!(docs.reference_sites("my_type", "unsaved_instance").len(), 1);
        assert!(docs.reference_sites("my_type", "my_instance").is_empty());
    }

    #[tokio::test]
    async fn ruleset_reload_and_membership_changes_reclassify_open_alias_keys() {
        use cwtools_rules::rules_types::{NewField, Options, RuleType, TypeType, ValueType};

        let fixture = fixture();
        let uri = fixture.reference_uri.clone();
        let text = "use_type = { base = my_instance use_MY_INSTANCE_ref = yes }\n";
        let parsed = Arc::new(parse_string(text, &fixture.backend.state.string_table));
        fixture
            .backend
            .state
            .documents
            .lock()
            .open(
                uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(text),
                    ast: Some(Arc::clone(&parsed)),
                    ast_version: Some(1),
                    ast_source_bytes: text.len(),
                    loc_cache: None,
                },
            )
            .expect("open reference document");
        fixture.backend.index_parsed_file(&uri, &parsed, Some(1));
        let sites = fixture
            .backend
            .state
            .info_service
            .read()
            .file_reference_sites(&uri);
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(&uri, 1, parsed, sites);

        let mut reloaded = type_ref_ruleset();
        for root in &mut reloaded.root_rules {
            if let cwtools_rules::rules_types::RootRule::TypeRule(
                name,
                (RuleType::NodeRule { rules, .. }, _),
            ) = root
                && name == "my_type"
            {
                let mut children = rules.to_vec();
                for (rule, _) in &mut children {
                    if let RuleType::LeafRule {
                        left: NewField::SpecificField(key),
                        right: NewField::TypeField(TypeType::Simple(type_name)),
                    } = rule
                        && key == "base"
                    {
                        *type_name = "other_type".to_string();
                    }
                }
                *rules = children.into();
            }
        }
        reloaded.aliases.push((
            "effect:use_<my_type>_ref".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("yes".to_string()),
                    right: NewField::ValueField(ValueType::Bool),
                },
                Options::default(),
            ),
        ));
        reloaded.reindex();
        fixture.backend.set_ruleset(reloaded);

        {
            let docs = fixture.backend.state.documents.lock();
            assert_eq!(docs.reference_sites("other_type", "my_instance").len(), 1);
            assert_eq!(docs.reference_sites("my_type", "my_instance").len(), 1);
            assert_eq!(docs.reference_sites("my_type", "MY_INSTANCE").len(), 1);
        }

        for (version, definition, expected_alias_sites) in [
            (2, DEFINITION.replace("my_instance", "other_instance"), 0),
            (3, DEFINITION.to_string(), 1),
        ] {
            let parsed = Arc::new(parse_string(
                &definition,
                &fixture.backend.state.string_table,
            ));
            {
                let mut docs = fixture.backend.state.documents.lock();
                docs.change(&fixture.definition_uri, version, Arc::from(definition))
                    .expect("change definition");
                docs.set_ast(&fixture.definition_uri, version, Arc::clone(&parsed));
            }
            fixture
                .backend
                .index_parsed_file(&fixture.definition_uri, &parsed, Some(version));
            let docs = fixture.backend.state.documents.lock();
            assert_eq!(docs.reference_sites("other_type", "my_instance").len(), 1);
            assert_eq!(
                docs.reference_sites("my_type", "my_instance").len(),
                expected_alias_sites,
            );
            assert_eq!(
                docs.reference_sites("my_type", "MY_INSTANCE").len(),
                expected_alias_sites,
            );
        }
    }

    #[tokio::test]
    async fn resolving_a_lens_does_not_walk_unrelated_open_asts() {
        let fixture = fixture();
        for document in 0..32 {
            let uri = format!("file:///events/unrelated-{document}.txt");
            let text = (0..128)
                .map(|leaf| format!("noise_{leaf} = {leaf}\n"))
                .collect::<String>();
            let parsed = Arc::new(parse_string(&text, &fixture.backend.state.string_table));
            fixture
                .backend
                .state
                .documents
                .lock()
                .open(
                    uri.clone(),
                    ParsedDoc {
                        version: 1,
                        text: Arc::from(text.as_str()),
                        ast: Some(Arc::clone(&parsed)),
                        ast_version: Some(1),
                        ast_source_bytes: text.len(),
                        loc_cache: None,
                    },
                )
                .expect("open unrelated document");
            fixture.backend.index_parsed_file(&uri, &parsed, Some(1));
        }

        let open_reference_uri = "file:///events/open-use.txt";
        let parsed = Arc::new(parse_string(REFERENCE, &fixture.backend.state.string_table));
        fixture
            .backend
            .state
            .documents
            .lock()
            .open(
                open_reference_uri.to_string(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(REFERENCE),
                    ast: Some(Arc::clone(&parsed)),
                    ast_version: Some(1),
                    ast_source_bytes: REFERENCE.len(),
                    loc_cache: None,
                },
            )
            .expect("open reference document");
        fixture
            .backend
            .index_parsed_file(open_reference_uri, &parsed, Some(1));
        let use_sites = fixture
            .backend
            .state
            .info_service
            .read()
            .file_reference_sites(open_reference_uri);
        assert_eq!(use_sites.len(), 1, "open file reference index entry");
        fixture
            .backend
            .state
            .documents
            .lock()
            .set_ast_with_use_sites(open_reference_uri, 1, parsed, use_sites);
        assert_eq!(
            fixture
                .backend
                .state
                .documents
                .lock()
                .reference_sites("my_type", "my_instance")
                .len(),
            1,
            "open reference overlay entry"
        );

        reset_ast_leaf_visits();
        let (title, locations) = resolve(&fixture).await;
        assert_eq!(title, "2 references");
        assert_eq!(locations.len(), 2);
        assert_eq!(
            locations
                .iter()
                .filter(|location| location.uri.as_str() == open_reference_uri)
                .count(),
            1,
            "indexed references from an open document must be included"
        );
        assert_eq!(
            ast_leaf_visits(),
            0,
            "resolving one lens must not visit leaves in any open AST"
        );
    }

    #[test]
    fn lens_data_requires_a_complete_definition_identity() {
        assert!(
            lens_data(Some(&json!({
                "uri": "file:///events.txt",
                "typeName": "event",
                "instanceName": "test.1",
                "line": 2,
                "column": 1,
            })))
            .is_some()
        );
        assert!(
            lens_data(Some(&json!({
                "uri": "file:///events.txt",
                "typeName": "event",
                "instanceName": "test.1",
                "line": -1,
                "column": 1,
            })))
            .is_none()
        );
    }
}
