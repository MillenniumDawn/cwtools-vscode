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
        let Some((location, type_names)) = ({
            let info = self.state.info_service.read();
            info.type_index
                .instances_in_file(data.uri)
                .into_iter()
                .find_map(|(type_name, instance)| {
                    if type_name != data.type_name
                        || instance.name != data.instance_name
                        || instance.location.line != data.line
                        || instance.location.col != data.column
                    {
                        return None;
                    }
                    let type_names: Vec<String> = info
                        .type_index
                        .instance_type_names_in_file(
                            data.uri,
                            data.type_name,
                            &instance.name,
                            instance.location,
                        )
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                    Some((instance.location, type_names))
                })
        }) else {
            return Ok(lens);
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
        // The reference index and the open-document walk both record where the
        // referenced name starts, so a lens answers from indexed data and never
        // reads a referencing file (#472). `source_range_without_text` converts
        // the parser's char column by char count rather than through
        // `DocLines`, which differs only on a line carrying astral-plane
        // characters ahead of the reference — and is already what every
        // unreadable file falls back to.
        let mut locations: Vec<Location> = Vec::new();
        for type_name in type_names {
            for site in self.collect_use_sites(&type_name, data.instance_name) {
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
    use crate::access::recorded_reads_under;
    use crate::navigation::test_rules::type_ref_ruleset;
    use crate::paths::{path_to_uri, workspace_prefix_of};
    use crate::state::ParsedDoc;
    use crate::{Backend, DocumentState};

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

        let state = Arc::new(DocumentState::new());
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let slot = captured.clone();
        let server_state = state.clone();
        let (_service, _socket) = tower_lsp::LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let client = captured.lock().take().expect("client");
        {
            let mut config = state.config.write();
            config.workspace_roots = vec![workspace.path().to_path_buf()];
            config.refresh_roots();
            config.workspace_prefix = Some(workspace_prefix_of(&path_to_uri(workspace.path())));
        }
        state.rules.write().ruleset = Some(Arc::new(type_ref_ruleset()));
        let backend = Backend { client, state };

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
