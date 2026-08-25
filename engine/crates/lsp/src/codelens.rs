use serde_json::{Value, json};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{CodeLens, CodeLensParams, Command, Location, Url};

use crate::Backend;
use crate::navigation::dedup_locations;
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
        let lenses = instances
            .into_iter()
            .map(|(type_name, instance_name, location)| CodeLens {
                range: self.source_range_with_text(
                    text.as_deref(),
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

        let text = self.file_text_for(data.uri).await;
        let range = self.source_range_with_text(
            text.as_deref(),
            location.line.saturating_sub(1),
            location.col as u32,
            "",
        );
        let mut sites = Vec::new();
        for type_name in type_names {
            sites.extend(self.collect_use_sites(&type_name, data.instance_name));
        }
        let site_uris: Vec<String> = sites.iter().map(|(uri, _)| uri.clone()).collect();
        let texts = self.file_text_snapshots_for(&site_uris).await;
        let locations: Vec<Location> = self
            .resolve_value_sites(&sites, data.instance_name, &texts)
            .into_iter()
            .map(|(file_uri, line, column, _)| Location {
                uri: parse_uri(&file_uri, &uri),
                range: self.source_range_with_text(
                    texts.get(&file_uri).map(|snapshot| snapshot.text.as_str()),
                    line,
                    column,
                    data.instance_name,
                ),
            })
            .collect();
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
    use serde_json::json;

    use super::lens_data;

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
