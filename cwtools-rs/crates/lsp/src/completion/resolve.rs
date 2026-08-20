use std::time::Instant;

use tower_lsp::lsp_types::{CompletionItem, Documentation};

use cwtools_info::InfoService;
use cwtools_rules::rules_types::RuleSet;

use crate::Backend;

use super::builders::{ResolveData, alias_documentation, enum_member_detail, type_instance_detail};

/// Recompute the `documentation`/`detail` that `completions_from_rules` /
/// `value_completions` deferred onto `item.data` (see `builders::ResolveData`).
/// Returns the item (touched or not) plus whether the recompute produced
/// anything — a "miss" is any of: no `data` (most items — nothing was
/// deferred), unparseable `data`, or the referenced alias/type-instance/
/// enum-member no longer existing in the current ruleset/index (the ruleset
/// reloaded or the file that defined it closed between the completion
/// request and this one). All of those are best-effort no-ops, never an
/// error — resolve only adds detail, it's never load-bearing for accepting
/// the item.
///
/// `type`/`enum` data doesn't carry the instance name/enum value — it's
/// already `item.label` (see `ResolveData`'s doc comment for why), so both
/// arms read it back from there instead.
fn resolve_item(
    mut item: CompletionItem,
    ruleset: Option<&RuleSet>,
    info: &InfoService,
) -> (CompletionItem, bool) {
    let Some(data) = item.data.as_ref().and_then(|d| d.as_str()) else {
        return (item, false);
    };
    let Some(parsed) = ResolveData::parse(data) else {
        return (item, false);
    };
    let label = item.label.clone();
    let hit = match parsed {
        ResolveData::Alias { cat, name } => ruleset
            .and_then(|rs| alias_documentation(rs, &cat, &name))
            .inspect(|desc| item.documentation = Some(Documentation::String(desc.clone())))
            .is_some(),
        ResolveData::Type { t } => type_instance_detail(ruleset, info, &t, &label)
            .inspect(|detail| item.detail = Some(detail.clone()))
            .is_some(),
        ResolveData::Enum { id } => ruleset
            .and_then(|rs| enum_member_detail(rs, info, &id, &label))
            .inspect(|detail| item.detail = Some(detail.clone()))
            .is_some(),
    };
    (item, hit)
}

/// One line per `completionItem/resolve` call, on the same
/// `"cwtools_completion"` target `log_completion_summary` uses. `hit` is
/// `false` for the common case (an item with no deferred `data` at all —
/// concrete fields, static one-word details) as well as a genuine miss
/// (the referenced entity is gone).
fn log_resolve_summary(resolve_us: u64, hit: bool) {
    tracing::info!(
        target: "cwtools_completion",
        resolve_us,
        hit,
    );
}

impl Backend {
    /// `completionItem/resolve`: fill in the `documentation`/`detail` this
    /// item's `data` describes, using the current ruleset + type index
    /// (never the snapshot from when the item was built — the two are
    /// usually the same request-to-resolve, but resolve always reads live
    /// state rather than caching anything keyed to the completion request).
    pub(crate) fn completion_resolve_impl(&self, item: CompletionItem) -> CompletionItem {
        let t_start = Instant::now();
        let ruleset = self.state.rules.read().ruleset.clone();
        let info = self.state.info_service.read();
        let (item, hit) = resolve_item(item, ruleset.as_deref(), &info);
        log_resolve_summary(t_start.elapsed().as_micros() as u64, hit);
        item
    }
}

#[cfg(test)]
mod tests {
    use cwtools_info::{SourceLocation, TypeInstance};
    use cwtools_rules::rules_types::{NewField, Options, RuleType};

    use super::*;

    fn item_with_data(label: &str, data: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            data: Some(serde_json::Value::String(data.to_string())),
            ..Default::default()
        }
    }

    fn alias_ruleset(cat: &str, name: &str, description: &str) -> RuleSet {
        let mut rs = RuleSet::new();
        rs.aliases.push((
            format!("{}:{}", cat, name),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField(format!("alias[{}:{}]", cat, name)),
                    right: NewField::ScalarField,
                },
                Options {
                    description: Some(description.to_string()),
                    ..Options::default()
                },
            ),
        ));
        rs.reindex();
        rs
    }

    #[test]
    fn resolve_alias_repopulates_documentation() {
        let rs = alias_ruleset("effect", "add_political_power", "Adds political power.");
        let info = InfoService::new();
        let item = item_with_data("add_political_power", "alias:effect:add_political_power");
        let (resolved, hit) = resolve_item(item, Some(&rs), &info);
        assert!(hit, "expected a hit");
        assert_eq!(
            resolved.documentation,
            Some(Documentation::String("Adds political power.".to_string()))
        );
    }

    #[test]
    fn resolve_alias_missing_entity_is_noop() {
        let rs = RuleSet::new();
        let info = InfoService::new();
        let item = item_with_data("gone", "alias:effect:gone");
        let (resolved, hit) = resolve_item(item, Some(&rs), &info);
        assert!(!hit, "no such alias, must be a miss");
        assert!(resolved.documentation.is_none());
    }

    #[test]
    fn resolve_type_repopulates_detail() {
        let mut info = InfoService::new();
        let mut per_type: std::collections::HashMap<String, Vec<TypeInstance>> =
            std::collections::HashMap::new();
        per_type.insert(
            "state".to_string(),
            vec![TypeInstance {
                name: "STATE_123".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        info.type_index.merge("file:///states/s.txt", per_type);
        // label carries the instance name — `data` doesn't repeat it.
        let item = item_with_data("STATE_123", "type:state");
        let (resolved, hit) = resolve_item(item, None, &info);
        assert!(hit);
        assert_eq!(resolved.detail, Some("state instance".to_string()));
    }

    #[test]
    fn resolve_type_prefers_subtype_display_name() {
        use cwtools_rules::rules_types::{PathOptions, SubTypeDefinition, TypeDefinition};

        let mut rs = RuleSet::new();
        rs.types.push(TypeDefinition {
            name: "event".to_string(),
            name_field: None,
            path_options: PathOptions::default(),
            subtypes: vec![SubTypeDefinition {
                name: "country".to_string(),
                display_name: Some("Country Event".to_string()),
                abbreviation: None,
                rules: Vec::new(),
                type_key_field: None,
                starts_with: None,
                push_scope: None,
                localisation: Vec::new(),
                only_if_not: Vec::new(),
                modifiers: Vec::new(),
                type_key_filter: Vec::new(),
            }],
            type_key_filter: None,
            skip_root_key: Vec::new(),
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: false,
            localisation: Vec::new(),
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        rs.reindex();

        let mut info = InfoService::new();
        let mut per_type: std::collections::HashMap<String, Vec<TypeInstance>> =
            std::collections::HashMap::new();
        per_type.insert(
            "event.country".to_string(),
            vec![TypeInstance {
                name: "my_event".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        info.type_index.merge("file:///events/e.txt", per_type);
        let item = item_with_data("my_event", "type:event.country");
        let (resolved, hit) = resolve_item(item, Some(&rs), &info);
        assert!(hit);
        assert_eq!(
            resolved.detail,
            Some("event.Country Event instance".to_string())
        );
    }

    #[test]
    fn resolve_type_missing_instance_is_noop() {
        let info = InfoService::new();
        let item = item_with_data("STATE_GONE", "type:state");
        let (resolved, hit) = resolve_item(item, None, &info);
        assert!(!hit);
        assert!(resolved.detail.is_none());
    }

    #[test]
    fn resolve_enum_repopulates_detail() {
        let mut rs = RuleSet::new();
        rs.enums.push(cwtools_rules::rules_types::EnumDefinition {
            key: "my_enum".to_string(),
            description: String::new(),
            values: vec!["alpha".to_string()],
        });
        rs.reindex();
        let info = InfoService::new();
        let item = item_with_data("alpha", "enum:my_enum");
        let (resolved, hit) = resolve_item(item, Some(&rs), &info);
        assert!(hit);
        assert_eq!(resolved.detail, Some("enum my_enum".to_string()));
    }

    #[test]
    fn resolve_no_data_is_noop() {
        let info = InfoService::new();
        let item = CompletionItem {
            label: "plain".to_string(),
            ..Default::default()
        };
        let (resolved, hit) = resolve_item(item, None, &info);
        assert!(!hit);
        assert!(resolved.documentation.is_none());
        assert!(resolved.detail.is_none());
    }

    #[test]
    fn resolve_unparseable_data_is_noop() {
        let info = InfoService::new();
        let item = item_with_data("x", "not_a_real_kind");
        let (resolved, hit) = resolve_item(item, None, &info);
        assert!(!hit);
        assert!(resolved.documentation.is_none());
    }
}
