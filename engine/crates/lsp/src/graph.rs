use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Map, Value};
use tower_lsp::jsonrpc::{Error, ErrorCode, Result};

use cwtools_info::SourceLocation;

use crate::Backend;

pub(crate) const MAX_GRAPH_NODES: usize = 500;

const SERVER_NOT_INITIALIZED: i64 = -32002;

const MAX_LABEL_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphLocation {
    pub(crate) filename: String,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphReference {
    pub(crate) key: String,
    pub(crate) is_outgoing: bool,
    pub(crate) label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphNodeDetail {
    pub(crate) key: String,
    pub(crate) values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphNode {
    pub(crate) id: String,
    pub(crate) name: Option<String>,
    pub(crate) references: Vec<GraphReference>,
    pub(crate) location: Option<GraphLocation>,
    pub(crate) details: Vec<GraphNodeDetail>,
    pub(crate) is_primary: bool,
    pub(crate) entity_type: String,
    pub(crate) entity_type_display_name: Option<String>,
    pub(crate) abbreviation: Option<String>,
}

impl GraphLocation {
    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("filename".to_string(), Value::from(self.filename.clone()));
        o.insert("line".to_string(), Value::from(self.line));
        o.insert("column".to_string(), Value::from(self.column));
        Value::Object(o)
    }
}

impl GraphReference {
    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("key".to_string(), Value::from(self.key.clone()));
        o.insert("isOutgoing".to_string(), Value::from(self.is_outgoing));
        if let Some(label) = &self.label {
            o.insert("label".to_string(), Value::from(label.clone()));
        }
        Value::Object(o)
    }
}

impl GraphNodeDetail {
    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("key".to_string(), Value::from(self.key.clone()));
        o.insert(
            "values".to_string(),
            Value::Array(self.values.iter().cloned().map(Value::from).collect()),
        );
        Value::Object(o)
    }
}

impl GraphNode {
    pub(crate) fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".to_string(), Value::from(self.id.clone()));
        if let Some(name) = &self.name {
            o.insert("name".to_string(), Value::from(name.clone()));
        }
        o.insert(
            "references".to_string(),
            Value::Array(
                self.references
                    .iter()
                    .map(GraphReference::to_json)
                    .collect(),
            ),
        );
        if let Some(loc) = &self.location {
            o.insert("location".to_string(), loc.to_json());
        }
        if !self.details.is_empty() {
            o.insert(
                "details".to_string(),
                Value::Array(self.details.iter().map(GraphNodeDetail::to_json).collect()),
            );
        }
        o.insert("isPrimary".to_string(), Value::from(self.is_primary));
        o.insert(
            "entityType".to_string(),
            Value::from(self.entity_type.clone()),
        );
        if let Some(display) = &self.entity_type_display_name {
            o.insert(
                "entityTypeDisplayName".to_string(),
                Value::from(display.clone()),
            );
        }
        if let Some(abbrev) = &self.abbreviation {
            o.insert("abbreviation".to_string(), Value::from(abbrev.clone()));
        }
        Value::Object(o)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GraphEntity {
    pub(crate) type_name: String,
    pub(crate) name: String,
    pub(crate) file_uri: String,
    pub(crate) location: SourceLocation,
}

pub(crate) trait GraphSource {
    fn instances(&self, type_name: &str) -> Vec<GraphEntity>;
    fn use_sites(&self, type_name: &str, name: &str) -> Vec<(String, SourceLocation)>;
    fn instances_in_file(&self, file_uri: &str) -> Vec<GraphEntity>;
}

#[derive(Debug, Clone)]
pub(crate) struct GraphRequest {
    pub(crate) entity_type: String,
    pub(crate) depth: u32,
    pub(crate) related_types: Vec<String>,
    pub(crate) max_nodes: usize,
    pub(crate) workspace_prefix: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphBuild {
    pub(crate) nodes: Vec<GraphNode>,
    pub(crate) seed_total: usize,
    pub(crate) omitted: usize,
    pub(crate) cap: usize,
}

impl GraphBuild {
    pub(crate) fn truncated(&self) -> bool {
        self.omitted > 0
    }

    pub(crate) fn add_truncation_notice(&mut self, entity_type: &str) {
        if !self.truncated() {
            return;
        }
        let detail = GraphNodeDetail {
            key: "truncated".to_string(),
            values: vec![format!(
                "showing {} of {} nodes (cap {}); {} {entity_type} instance(s) matched",
                self.nodes.len(),
                self.nodes.len() + self.omitted,
                self.cap,
                self.seed_total,
            )],
        };
        for node in &mut self.nodes {
            node.details.push(detail.clone());
        }
    }
}

enum Slot {
    New(usize),
    Existing(usize),
    Full,
}

struct GraphBuilder {
    nodes: Vec<GraphNode>,
    entities: Vec<GraphEntity>,
    by_id: HashMap<String, usize>,
    edges: HashSet<(usize, usize)>,
    dropped: HashSet<String>,
}

impl GraphBuilder {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            entities: Vec::new(),
            by_id: HashMap::new(),
            edges: HashSet::new(),
            dropped: HashSet::new(),
        }
    }

    fn push(&mut self, entity: GraphEntity, req: &GraphRequest, limit: usize) -> Slot {
        let key = entity.name.to_ascii_lowercase();
        if let Some(&idx) = self.by_id.get(&key) {
            return Slot::Existing(idx);
        }
        if self.nodes.len() >= limit {
            self.dropped.insert(key);
            return Slot::Full;
        }
        let idx = self.nodes.len();
        self.by_id.insert(key, idx);
        self.nodes.push(GraphNode {
            id: entity.name.clone(),
            name: None,
            references: Vec::new(),
            location: Some(GraphLocation {
                filename: uri_to_display_path(&entity.file_uri),
                line: entity.location.line,
                column: u32::from(entity.location.col) + 1,
            }),
            details: vec![GraphNodeDetail {
                key: "file".to_string(),
                values: vec![crate::paths::logical_path_from_uri(
                    &entity.file_uri,
                    &req.workspace_prefix,
                )],
            }],
            is_primary: entity.type_name.eq_ignore_ascii_case(&req.entity_type),
            entity_type: entity.type_name.clone(),
            entity_type_display_name: Some(humanize_type_name(&entity.type_name)),
            abbreviation: Some(abbreviate_type_name(&entity.type_name)),
        });
        self.entities.push(entity);
        Slot::New(idx)
    }

    fn add_edge(&mut self, from: usize, to: usize) {
        if from == to || !self.edges.insert((from, to)) {
            return;
        }
        let label = (self.entities[from].type_name != self.entities[to].type_name)
            .then(|| self.entities[to].type_name.clone());
        let key = self.nodes[to].id.clone();
        self.nodes[from].references.push(GraphReference {
            key,
            is_outgoing: true,
            label,
        });
    }
}

fn contains_site(entity: &GraphEntity, site: SourceLocation) -> bool {
    let start = (entity.location.line, entity.location.col);
    let at = (site.line, site.col);
    start <= at && at <= entity.location.end
}

fn is_whole_file(entity: &GraphEntity) -> bool {
    entity.location.line == 1 && entity.location.col == 0 && entity.location.end == (1, 0)
}

fn innermost_owner(owners: &[GraphEntity], site: SourceLocation) -> Option<&GraphEntity> {
    owners
        .iter()
        .filter(|e| contains_site(e, site))
        .max_by_key(|e| (e.location.line, e.location.col))
        .or_else(|| owners.iter().find(|e| is_whole_file(e)))
}

pub(crate) fn build_graph<S: GraphSource + ?Sized>(src: &S, req: &GraphRequest) -> GraphBuild {
    let mut builder = GraphBuilder::new();
    let mut queue: VecDeque<(usize, u32)> = VecDeque::new();
    let mut seed_total = 0;
    let seed_budget = req.max_nodes.div_ceil(2).max(1);

    let mut seed_types = vec![req.entity_type.clone()];
    seed_types.extend(
        req.related_types
            .iter()
            .filter(|t| !t.eq_ignore_ascii_case(&req.entity_type))
            .cloned(),
    );

    for type_name in &seed_types {
        let mut instances = src.instances(type_name);
        instances.sort_by(|a, b| {
            (&a.file_uri, a.location.line, a.location.col, &a.name).cmp(&(
                &b.file_uri,
                b.location.line,
                b.location.col,
                &b.name,
            ))
        });
        seed_total += instances.len();
        for entity in instances {
            if let Slot::New(idx) = builder.push(entity, req, seed_budget) {
                queue.push_back((idx, 0));
            }
        }
    }

    let mut file_cache: HashMap<String, Vec<GraphEntity>> = HashMap::new();
    while let Some((idx, level)) = queue.pop_front() {
        if level >= req.depth {
            continue;
        }
        let (type_name, name) = {
            let e = &builder.entities[idx];
            (e.type_name.clone(), e.name.clone())
        };
        for (file_uri, site) in src.use_sites(&type_name, &name) {
            let owners = file_cache
                .entry(file_uri.clone())
                .or_insert_with(|| src.instances_in_file(&file_uri));
            let Some(owner) = innermost_owner(owners, site) else {
                continue;
            };
            if !type_allowed(req, &owner.type_name) {
                continue;
            }
            let owner = owner.clone();
            match builder.push(owner, req, req.max_nodes) {
                Slot::New(owner_idx) => {
                    queue.push_back((owner_idx, level + 1));
                    builder.add_edge(owner_idx, idx);
                }
                Slot::Existing(owner_idx) => builder.add_edge(owner_idx, idx),
                Slot::Full => {}
            }
        }
    }

    GraphBuild {
        nodes: builder.nodes,
        seed_total,
        omitted: builder.dropped.len(),
        cap: req.max_nodes,
    }
}

fn type_allowed(req: &GraphRequest, type_name: &str) -> bool {
    req.related_types.is_empty()
        || type_name.eq_ignore_ascii_case(&req.entity_type)
        || req
            .related_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(type_name))
}

fn uri_to_display_path(uri: &str) -> String {
    crate::paths::uri_to_path_str(uri).replace('\\', "/")
}

fn humanize_type_name(type_name: &str) -> String {
    let mut out = String::with_capacity(type_name.len());
    for word in type_name.split(['_', '.']).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        type_name.to_string()
    } else {
        out
    }
}

fn abbreviate_type_name(type_name: &str) -> String {
    let abbrev: String = type_name
        .split(['_', '.'])
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    if abbrev.is_empty() {
        type_name.to_uppercase()
    } else {
        abbrev
    }
}

fn truncate_label(label: &str) -> String {
    if label.chars().count() <= MAX_LABEL_CHARS {
        return label.to_string();
    }
    let mut s: String = label.chars().take(MAX_LABEL_CHARS).collect();
    s.push('…');
    s
}

fn not_initialized(message: impl Into<String>) -> Error {
    Error {
        code: ErrorCode::ServerError(SERVER_NOT_INITIALIZED),
        message: message.into().into(),
        data: None,
    }
}

struct BackendGraphSource<'a> {
    backend: &'a Backend,
}

impl GraphSource for BackendGraphSource<'_> {
    fn instances(&self, type_name: &str) -> Vec<GraphEntity> {
        let info = self.backend.state.info_service.read();
        info.type_index
            .instances(type_name)
            .iter()
            .map(|(file_uri, inst)| GraphEntity {
                type_name: type_name.to_string(),
                name: inst.name.clone(),
                file_uri: file_uri.to_string(),
                location: inst.location,
            })
            .collect()
    }

    fn use_sites(&self, type_name: &str, name: &str) -> Vec<(String, SourceLocation)> {
        self.backend.collect_use_sites(type_name, name)
    }

    fn instances_in_file(&self, file_uri: &str) -> Vec<GraphEntity> {
        let info = self.backend.state.info_service.read();
        info.type_index
            .instances_in_file(file_uri)
            .into_iter()
            .map(|(type_name, inst)| GraphEntity {
                type_name: type_name.to_string(),
                name: inst.name.clone(),
                file_uri: file_uri.to_string(),
                location: inst.location,
            })
            .collect()
    }
}

impl Backend {
    pub(crate) async fn get_graph_data(&self, arguments: &[Value]) -> Result<Option<Value>> {
        let requested_type = arguments
            .first()
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::invalid_params(
                    "getGraphData: first argument must be a non-empty entity type name",
                )
            })?
            .to_string();
        let depth = arguments
            .get(1)
            .ok_or_else(|| Error::invalid_params("getGraphData: missing depth argument"))?;
        // A whole float (`3.0`) is a legitimate JSON encoding of an integer.
        let depth = depth
            .as_i64()
            .or_else(|| {
                depth
                    .as_f64()
                    .filter(|f| f.fract() == 0.0)
                    .map(|f| f as i64)
            })
            .ok_or_else(|| {
                Error::invalid_params(format!(
                    "getGraphData: depth must be an integer, got {depth}"
                ))
            })?;
        if depth < 1 {
            return Err(Error::invalid_params(format!(
                "getGraphData: depth must be at least 1, got {depth}"
            )));
        }

        if !self.state.index_ready.load(Ordering::Relaxed) {
            return Err(not_initialized(
                "getGraphData: the workspace index is still building; try again once the initial scan finishes",
            ));
        }

        let (canonical, related_types, rules_loaded) = {
            let rules = self.state.rules.read();
            match rules.ruleset.as_ref() {
                Some(rs) => {
                    let idx = rs.type_by_name().get(&requested_type).copied().or_else(|| {
                        rs.types
                            .iter()
                            .position(|t| t.name.eq_ignore_ascii_case(&requested_type))
                    });
                    match idx {
                        Some(i) => (
                            Some(rs.types[i].name.clone()),
                            rs.types[i].graph_related_types.clone(),
                            true,
                        ),
                        None => (None, Vec::new(), true),
                    }
                }
                None => (None, Vec::new(), false),
            }
        };
        if !rules_loaded {
            return Err(not_initialized(
                "getGraphData: no rules config is loaded, so no entity types are known",
            ));
        }

        let (entity_type, index_empty) = {
            let info = self.state.info_service.read();
            let index_empty = info.type_index.map.values().all(Vec::is_empty);
            let resolved = canonical.or_else(|| {
                info.type_index
                    .map
                    .keys()
                    .find(|k| k.eq_ignore_ascii_case(&requested_type))
                    .cloned()
            });
            (resolved, index_empty)
        };
        if index_empty {
            return Err(not_initialized(
                "getGraphData: the workspace index is empty; no entities have been indexed",
            ));
        }
        let entity_type = entity_type.ok_or_else(|| {
            Error::invalid_params(format!(
                "getGraphData: unknown entity type '{requested_type}' \
                 (not defined by the loaded rules and not present in the workspace index)"
            ))
        })?;

        let req = GraphRequest {
            entity_type,
            depth: u32::try_from(depth).unwrap_or(u32::MAX),
            related_types,
            max_nodes: MAX_GRAPH_NODES,
            workspace_prefix: self.state.config.read().workspace_prefix.clone(),
        };

        let mut build = tokio::task::block_in_place(|| {
            let src = BackendGraphSource { backend: self };
            build_graph(&src, &req)
        });

        if build.nodes.is_empty() {
            return Err(Error::invalid_params(format!(
                "getGraphData: no instances of entity type '{}' in the workspace",
                req.entity_type
            )));
        }

        self.fill_display_names(&mut build.nodes);
        build.add_truncation_notice(&req.entity_type);
        if build.truncated() {
            self.client
                .log_message(
                    tower_lsp::lsp_types::MessageType::WARNING,
                    format!(
                        "getGraphData({}, depth {}): capped at {} nodes, {} omitted",
                        req.entity_type, req.depth, MAX_GRAPH_NODES, build.omitted
                    ),
                )
                .await;
        }
        tracing::info!(
            entity_type = %req.entity_type,
            depth = req.depth,
            nodes = build.nodes.len(),
            omitted = build.omitted,
            "getGraphData"
        );

        Ok(Some(Value::Array(
            build.nodes.iter().map(GraphNode::to_json).collect(),
        )))
    }

    fn fill_display_names(&self, nodes: &mut [GraphNode]) {
        // Lock order: rules -> info_service -> loc_text (as in hover).
        let rules = self.state.rules.read();
        let Some(ruleset) = rules.ruleset.as_ref() else {
            return;
        };
        let info = self.state.info_service.read();
        let loc_text = self.state.loc_text.read();
        if loc_text.is_empty() {
            return;
        }
        for node in nodes {
            let mut keys: Vec<String> = Vec::new();
            if let Some(k) = info.type_index.primary_loc_key(&node.entity_type, &node.id) {
                keys.push(k.to_ascii_lowercase());
            }
            if let Some(&i) = ruleset.type_by_name().get(&node.entity_type) {
                for loc in &ruleset.types[i].localisation {
                    if loc.explicit_field.is_none() && (loc.primary || loc.required) {
                        keys.push(loc.derived_key(&node.id).to_ascii_lowercase());
                    }
                }
            }
            node.name = keys
                .iter()
                .find_map(|key| loc_text.get(key.as_str()))
                .and_then(|translations| translations.first())
                .map(|(_, text)| truncate_label(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(line: u32, col: u16, end_line: u32, end_col: u16) -> SourceLocation {
        SourceLocation {
            line,
            col,
            end: (end_line, end_col),
        }
    }

    fn entity(type_name: &str, name: &str, file: &str, location: SourceLocation) -> GraphEntity {
        GraphEntity {
            type_name: type_name.to_string(),
            name: name.to_string(),
            file_uri: file.to_string(),
            location,
        }
    }

    #[derive(Default)]
    struct FakeSource {
        entities: Vec<GraphEntity>,
        sites: HashMap<(String, String), Vec<(String, SourceLocation)>>,
    }

    impl FakeSource {
        fn with_entities(entities: Vec<GraphEntity>) -> Self {
            Self {
                entities,
                sites: HashMap::new(),
            }
        }

        fn add_site(&mut self, type_name: &str, name: &str, file: &str, site: SourceLocation) {
            self.sites
                .entry((type_name.to_string(), name.to_ascii_lowercase()))
                .or_default()
                .push((file.to_string(), site));
        }
    }

    impl GraphSource for FakeSource {
        fn instances(&self, type_name: &str) -> Vec<GraphEntity> {
            self.entities
                .iter()
                .filter(|e| e.type_name == type_name)
                .cloned()
                .collect()
        }

        fn use_sites(&self, type_name: &str, name: &str) -> Vec<(String, SourceLocation)> {
            self.sites
                .get(&(type_name.to_string(), name.to_ascii_lowercase()))
                .cloned()
                .unwrap_or_default()
        }

        fn instances_in_file(&self, file_uri: &str) -> Vec<GraphEntity> {
            self.entities
                .iter()
                .filter(|e| e.file_uri == file_uri)
                .cloned()
                .collect()
        }
    }

    fn request(entity_type: &str, depth: u32) -> GraphRequest {
        GraphRequest {
            entity_type: entity_type.to_string(),
            depth,
            related_types: Vec::new(),
            max_nodes: MAX_GRAPH_NODES,
            workspace_prefix: None,
        }
    }

    fn prereq_chain() -> FakeSource {
        let mut src = FakeSource::with_entities(vec![
            entity("focus", "focus_a", "file:///f.txt", loc(1, 0, 10, 1)),
            entity("focus", "focus_b", "file:///f.txt", loc(11, 0, 20, 1)),
            entity("focus", "focus_c", "file:///f.txt", loc(21, 0, 30, 1)),
        ]);
        src.add_site("focus", "focus_a", "file:///f.txt", loc(15, 8, 15, 20));
        src.add_site("focus", "focus_b", "file:///f.txt", loc(25, 8, 25, 20));
        src
    }

    fn edges(build: &GraphBuild) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for node in &build.nodes {
            for r in &node.references {
                if r.is_outgoing {
                    out.push((node.id.clone(), r.key.clone()));
                } else {
                    out.push((r.key.clone(), node.id.clone()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn test_graph_nodes_and_edges_from_use_sites() {
        let src = prereq_chain();
        let build = build_graph(&src, &request("focus", 3));

        let mut ids: Vec<&str> = build.nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["focus_a", "focus_b", "focus_c"]);
        assert_eq!(
            edges(&build),
            [
                ("focus_b".to_string(), "focus_a".to_string()),
                ("focus_c".to_string(), "focus_b".to_string()),
            ]
        );
        assert_eq!(build.seed_total, 3);
        assert!(!build.truncated());
    }

    #[test]
    fn test_graph_node_conforms_to_wire_format() {
        let src = prereq_chain();
        let build = build_graph(&src, &request("focus", 3));
        let node = build
            .nodes
            .iter()
            .find(|n| n.id == "focus_b")
            .expect("focus_b node");
        let json = node.to_json();

        assert_eq!(json["id"], "focus_b");
        assert_eq!(json["isPrimary"], true);
        assert_eq!(json["entityType"], "focus");
        assert_eq!(json["entityTypeDisplayName"], "Focus");
        assert_eq!(json["abbreviation"], "F");
        assert_eq!(json["location"]["filename"], "/f.txt");
        assert_eq!(json["location"]["line"], 11);
        assert_eq!(json["location"]["column"], 1);
        assert_eq!(json["references"][0]["key"], "focus_a");
        assert_eq!(json["references"][0]["isOutgoing"], true);
        assert!(json.get("name").is_none());
        assert!(json["references"][0].get("label").is_none());
    }

    #[test]
    fn test_graph_depth_bounds_the_walk() {
        let src = prereq_chain();
        let build = build_graph(&src, &request("focus", 1));
        assert_eq!(build.nodes.len(), 3);
        assert_eq!(edges(&build).len(), 2);

        let mut src = FakeSource::with_entities(vec![
            entity("focus", "focus_a", "file:///f.txt", loc(1, 0, 10, 1)),
            entity("decision", "dec_b", "file:///d.txt", loc(1, 0, 10, 1)),
            entity("event", "evt_c", "file:///e.txt", loc(1, 0, 10, 1)),
        ]);
        src.add_site("focus", "focus_a", "file:///d.txt", loc(5, 4, 5, 9));
        src.add_site("decision", "dec_b", "file:///e.txt", loc(5, 4, 5, 9));

        let shallow = build_graph(&src, &request("focus", 1));
        let mut ids: Vec<&str> = shallow.nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["dec_b", "focus_a"]);

        let deep = build_graph(&src, &request("focus", 2));
        let mut ids: Vec<&str> = deep.nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["dec_b", "evt_c", "focus_a"]);
        assert_eq!(
            edges(&deep),
            [
                ("dec_b".to_string(), "focus_a".to_string()),
                ("evt_c".to_string(), "dec_b".to_string()),
            ]
        );
    }

    #[test]
    fn test_graph_cycle_terminates_without_duplicates() {
        let mut src = FakeSource::with_entities(vec![
            entity("focus", "a", "file:///f.txt", loc(1, 0, 10, 1)),
            entity("focus", "b", "file:///f.txt", loc(11, 0, 20, 1)),
            entity("focus", "c", "file:///f.txt", loc(21, 0, 30, 1)),
        ]);
        src.add_site("focus", "a", "file:///f.txt", loc(15, 4, 15, 9));
        src.add_site("focus", "b", "file:///f.txt", loc(25, 4, 25, 9));
        src.add_site("focus", "c", "file:///f.txt", loc(5, 4, 5, 9));
        src.add_site("focus", "a", "file:///f.txt", loc(6, 4, 6, 9));

        let build = build_graph(&src, &request("focus", 100));
        assert_eq!(build.nodes.len(), 3);
        assert_eq!(
            edges(&build),
            [
                ("a".to_string(), "c".to_string()),
                ("b".to_string(), "a".to_string()),
                ("c".to_string(), "b".to_string()),
            ]
        );
    }

    #[test]
    fn test_graph_node_cap_truncates_and_reports() {
        let entities: Vec<GraphEntity> = (0..10)
            .map(|i| {
                entity(
                    "focus",
                    &format!("focus_{i:02}"),
                    "file:///f.txt",
                    loc(1 + i * 10, 0, 9 + i * 10, 1),
                )
            })
            .collect();
        let src = FakeSource::with_entities(entities);
        let mut req = request("focus", 3);
        req.max_nodes = 4;

        let mut build = build_graph(&src, &req);
        assert_eq!(build.nodes.len(), 2);
        assert_eq!(build.seed_total, 10);
        assert_eq!(build.omitted, 8);
        assert!(build.truncated());
        let ids: Vec<&str> = build.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["focus_00", "focus_01"]);

        build.add_truncation_notice("focus");
        for node in &build.nodes {
            let notice = node
                .details
                .iter()
                .find(|d| d.key == "truncated")
                .expect("every node carries the truncation notice");
            assert_eq!(
                notice.values[0],
                "showing 2 of 10 nodes (cap 4); 10 focus instance(s) matched"
            );
        }
    }

    #[test]
    fn test_graph_walk_uses_the_budget_the_seeds_left() {
        let mut entities: Vec<GraphEntity> = (0..3)
            .map(|i| {
                entity(
                    "focus",
                    &format!("focus_{i}"),
                    "file:///f.txt",
                    loc(1 + i * 10, 0, 9 + i * 10, 1),
                )
            })
            .collect();
        let mut src_entities: Vec<GraphEntity> = (0..3)
            .map(|i| {
                entity(
                    "decision",
                    &format!("dec_{i}"),
                    &format!("file:///d{i}.txt"),
                    loc(1, 0, 9, 1),
                )
            })
            .collect();
        entities.append(&mut src_entities);
        let mut src = FakeSource::with_entities(entities);
        for i in 0..3 {
            src.add_site(
                "focus",
                &format!("focus_{i}"),
                &format!("file:///d{i}.txt"),
                loc(5, 4, 5, 9),
            );
        }

        let mut req = request("focus", 3);
        req.max_nodes = 4;
        let build = build_graph(&src, &req);
        let ids: Vec<&str> = build.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["focus_0", "focus_1", "dec_0", "dec_1"]);
        assert_eq!(
            edges(&build),
            [
                ("dec_0".to_string(), "focus_0".to_string()),
                ("dec_1".to_string(), "focus_1".to_string()),
            ]
        );
    }

    #[test]
    fn test_graph_related_types_gate_which_types_join() {
        let mut src = FakeSource::with_entities(vec![
            entity("focus", "focus_a", "file:///f.txt", loc(1, 0, 10, 1)),
            entity("decision", "dec_b", "file:///d.txt", loc(1, 0, 10, 1)),
            entity("event", "evt_c", "file:///e.txt", loc(1, 0, 10, 1)),
        ]);
        src.add_site("focus", "focus_a", "file:///d.txt", loc(5, 4, 5, 9));
        src.add_site("focus", "focus_a", "file:///e.txt", loc(5, 4, 5, 9));

        let open = build_graph(&src, &request("focus", 2));
        assert_eq!(open.nodes.len(), 3);

        let mut req = request("focus", 2);
        req.related_types = vec!["decision".to_string()];
        let gated = build_graph(&src, &req);
        let mut ids: Vec<&str> = gated.nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["dec_b", "focus_a"]);
        assert_eq!(
            edges(&gated),
            [("dec_b".to_string(), "focus_a".to_string())]
        );
    }

    #[test]
    fn test_graph_marks_seed_type_primary_and_labels_crossing_edges() {
        let mut src = FakeSource::with_entities(vec![
            entity("focus", "focus_a", "file:///f.txt", loc(1, 0, 10, 1)),
            entity("decision", "dec_b", "file:///d.txt", loc(1, 0, 10, 1)),
        ]);
        src.add_site("focus", "focus_a", "file:///d.txt", loc(5, 4, 5, 9));

        let build = build_graph(&src, &request("focus", 2));
        let focus = build.nodes.iter().find(|n| n.id == "focus_a").unwrap();
        let decision = build.nodes.iter().find(|n| n.id == "dec_b").unwrap();
        assert!(focus.is_primary);
        assert!(!decision.is_primary);
        assert_eq!(decision.references[0].label.as_deref(), Some("focus"));
        assert_eq!(
            decision.entity_type_display_name.as_deref(),
            Some("Decision")
        );
        assert_eq!(decision.abbreviation.as_deref(), Some("D"));
    }

    #[test]
    fn test_graph_attributes_a_use_site_to_the_innermost_definition() {
        let mut src = FakeSource::with_entities(vec![
            entity("focus_tree", "tree", "file:///f.txt", loc(1, 0, 40, 1)),
            entity("focus", "focus_a", "file:///f.txt", loc(3, 4, 10, 5)),
            entity("focus", "focus_b", "file:///f.txt", loc(12, 4, 20, 5)),
        ]);
        src.add_site("focus", "focus_a", "file:///f.txt", loc(15, 8, 15, 20));

        let build = build_graph(&src, &request("focus", 2));
        assert_eq!(
            edges(&build),
            [("focus_b".to_string(), "focus_a".to_string())]
        );
        assert!(build.nodes.iter().all(|n| n.id != "tree"));
    }

    #[test]
    fn test_graph_attributes_a_use_site_to_a_whole_file_definition() {
        let mut src = FakeSource::with_entities(vec![
            entity("state", "12", "file:///states/12.txt", loc(1, 0, 40, 1)),
            entity(
                "country_history",
                "GER",
                "file:///history/GER.txt",
                loc(1, 0, 1, 0),
            ),
        ]);
        src.add_site("state", "12", "file:///history/GER.txt", loc(3, 0, 3, 12));

        let build = build_graph(&src, &request("state", 2));
        assert_eq!(edges(&build), [("GER".to_string(), "12".to_string())]);
    }

    #[test]
    fn test_graph_ignores_use_sites_outside_any_definition() {
        let mut src = FakeSource::with_entities(vec![entity(
            "focus",
            "focus_a",
            "file:///f.txt",
            loc(1, 0, 10, 1),
        )]);
        src.add_site("focus", "focus_a", "file:///loose.txt", loc(5, 4, 5, 9));

        let build = build_graph(&src, &request("focus", 5));
        assert_eq!(build.nodes.len(), 1);
        assert!(edges(&build).is_empty());
    }

    #[test]
    fn test_graph_duplicate_names_collapse_to_one_node() {
        let src = FakeSource::with_entities(vec![
            entity("focus", "dup", "file:///a.txt", loc(1, 0, 10, 1)),
            entity("focus", "DUP", "file:///b.txt", loc(1, 0, 10, 1)),
        ]);
        let build = build_graph(&src, &request("focus", 2));
        assert_eq!(build.nodes.len(), 1);
        assert_eq!(build.seed_total, 2);
    }

    #[test]
    fn test_humanize_and_abbreviate_handle_odd_type_names() {
        assert_eq!(humanize_type_name("national_focus"), "National Focus");
        assert_eq!(abbreviate_type_name("national_focus"), "NF");
        assert_eq!(abbreviate_type_name("_leading"), "L");
        assert_eq!(humanize_type_name("a__b"), "A B");
        assert_eq!(abbreviate_type_name(""), "");
    }
}
