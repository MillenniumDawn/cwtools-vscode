mod builders;
mod cwt;
mod filter;
mod loc_keys;
mod request;
mod resolve;
mod scope_names;
mod snippets;

pub(crate) use builders::{
    ValueCompletionSets, apply_label_details, completions_from_rules, expanded_modifier_scopes,
    root_type_snippets, value_completions, value_rules_need_loc_keys,
};
pub(crate) use loc_keys::LocKeyIndex;
pub(crate) use scope_names::loc_completions;

// Siblings and this module's tests import these via `super::`.
pub(crate) use filter::{
    CONTEXT_CAP, CONTEXT_COMPLETE_THRESHOLD, anchor_items, filter_by_token, prepare_context_items,
    sort_by_token, sort_for_kind, subsequence_match, token_matches,
};
pub(crate) use snippets::generate_node_snippet;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat};

    use cwtools_rules::rules_types::{
        EnumDefinition, NewField, NewRule, Options, PathOptions, RootRule, RuleSet, RuleType,
        TypeDefinition, ValueType,
    };

    use super::filter::filter_and_cap;
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_leaf_rule(key: &str, right: NewField) -> NewRule {
        (
            RuleType::LeafRule {
                left: NewField::SpecificField(key.to_string()),
                right,
            },
            Options::default(),
        )
    }

    fn make_node_rule(key: &str, children: Vec<NewRule>) -> NewRule {
        (
            RuleType::NodeRule {
                left: NewField::SpecificField(key.to_string()),
                rules: children.into(),
            },
            Options::default(),
        )
    }

    fn bool_enum_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();

        // enum: my_enum = { alpha beta gamma }
        rs.enums.push(EnumDefinition {
            key: "my_enum".to_string(),
            description: String::new(),
            values: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        });

        // type: my_type paths = { events }
        rs.types.push(TypeDefinition {
            name: "my_type".to_string(),
            name_field: Some("id".to_string()),
            path_options: PathOptions {
                paths: vec!["events".to_string()],
                path_strict: false,
                path_file: None,
                path_extension: None,
                paths_lower: Vec::new(),
                ..Default::default()
            },
            subtypes: Vec::new(),
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

        // TypeRule for my_type with child fields
        rs.root_rules.push(RootRule::TypeRule(
            "my_type".to_string(),
            make_node_rule(
                "my_type",
                vec![
                    make_leaf_rule(
                        "kind",
                        NewField::ValueField(ValueType::Enum("my_enum".to_string())),
                    ),
                    make_leaf_rule("active", NewField::ValueField(ValueType::Bool)),
                    make_leaf_rule("name", NewField::ScalarField),
                ],
            ),
        ));

        rs.reindex();
        rs
    }

    // ── completion context tests ─────────────────────────────────────────────

    #[test]
    fn test_completions_from_rules_enum() {
        let rs = bool_enum_ruleset();
        let info = cwtools_info::InfoService::new();

        // Grab the inner rules from the TypeRule
        let rules = if let Some(RootRule::TypeRule(_, (RuleType::NodeRule { rules, .. }, _))) =
            rs.root_rules.first()
        {
            rules.as_ref()
        } else {
            panic!("expected TypeRule");
        };

        let items = completions_from_rules(
            rules,
            &rs,
            &info,
            "stellaris",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;

        // "kind" should appear with a snippet containing enum values
        let kind_item = items.iter().find(|i| i.label == "kind");
        assert!(
            kind_item.is_some(),
            "expected 'kind' completion, got: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        let kind = kind_item.unwrap();
        assert_eq!(kind.insert_text_format, Some(InsertTextFormat::SNIPPET));
        let snippet = kind.insert_text.as_deref().unwrap_or("");
        assert!(snippet.contains("alpha"), "snippet: {}", snippet);

        // "active" should have yes/no snippet
        let active_item = items.iter().find(|i| i.label == "active");
        assert!(active_item.is_some(), "expected 'active' completion");
        let active = active_item.unwrap();
        let asnip = active.insert_text.as_deref().unwrap_or("");
        assert!(asnip.contains("yes"), "active snippet: {}", asnip);
    }

    #[test]
    fn test_completion_scalar_key_inserts_equals() {
        // A plain field (scalar/int/type value) must autocomplete to `name = `,
        // not a bare `name` (cwtools-vscode#16).
        let rs = bool_enum_ruleset();
        let info = cwtools_info::InfoService::new();
        let first_root = rs.root_rules.first().expect("expected root rule");
        let rules: &[(RuleType, cwtools_rules::rules_types::Options)] = match first_root {
            RootRule::TypeRule(_, (RuleType::NodeRule { rules, .. }, _)) => rules.as_ref(),
            _ => panic!("expected TypeRule"),
        };
        let items = completions_from_rules(
            rules,
            &rs,
            &info,
            "stellaris",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;
        let name = items
            .iter()
            .find(|i| i.label == "name")
            .expect("name completion");
        let snip = name.insert_text.as_deref().unwrap_or("");
        assert!(
            snip.starts_with("name = "),
            "scalar key should insert 'name = ', got: {:?}",
            name.insert_text
        );
    }

    #[test]
    fn test_completion_items_have_kind_aware_sort_text() {
        // Every item in a key-context list must carry a sortText so VS Code
        // orders them by usefulness as the user types. Concrete leaf fields
        // sort ahead of node blocks, which sort ahead of aliases, which sort
        // ahead of type instances, which sort ahead of enum values, which sort
        // ahead of scope names. The user-visible "iteration" feel depends on
        // this — without it, the popup sorts purely alphabetically and a
        // common prefix keeps many similarly-named items in the same row.
        let rs = bool_enum_ruleset();
        let info = cwtools_info::InfoService::new();
        let first_root = rs.root_rules.first().expect("expected root rule");
        let rules: &[(RuleType, cwtools_rules::rules_types::Options)] =
            if let RootRule::TypeRule(_, (RuleType::NodeRule { rules, .. }, _)) = first_root {
                rules.as_ref()
            } else {
                panic!("expected TypeRule");
            };
        let items = completions_from_rules(
            rules,
            &rs,
            &info,
            "stellaris",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;
        assert!(!items.is_empty(), "expected some completions");
        for item in &items {
            assert!(
                item.sort_text.is_some(),
                "completion {:?} has no sortText, will sort alphabetically",
                item.label
            );
        }
        // The first item by sortText should be a concrete leaf field (the
        // bool `active` from the fixture), not an enum value or alias.
        let mut sorted = items.clone();
        sorted.sort_by(|a, b| {
            a.sort_text
                .as_deref()
                .unwrap()
                .cmp(b.sort_text.as_deref().unwrap())
        });
        let first = sorted.first().unwrap();
        assert_eq!(
            first.kind,
            Some(CompletionItemKind::FIELD),
            "first item by sort should be a concrete field, got {:?}",
            first.label
        );
    }

    #[test]
    fn test_completion_sort_key_buckets() {
        // The bucket prefix is fixed-width (single digit 0-9) so the secondary
        // sort by label stays stable when the same item kind appears in two
        // different rule lists. The scope-aware bucket for `required_scopes`
        // is `0_` and must always lead the list.
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::FIELD), "x"),
            Some("1_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::STRUCT), "x"),
            Some("2_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::KEYWORD), "x"),
            Some("3_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::ENUM_MEMBER), "x"),
            Some("4_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::VALUE), "x"),
            Some("5_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::CONSTANT), "x"),
            Some("6_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::REFERENCE), "x"),
            Some("7_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::FUNCTION), "x"),
            Some("8_x".to_string())
        );
        assert_eq!(
            sort_for_kind(Some(CompletionItemKind::TEXT), "x"),
            Some("9_x".to_string())
        );
        assert_eq!(sort_for_kind(None, "x"), None);
    }

    // ── snippet generation tests ─────────────────────────────────────────────

    #[test]
    fn test_generate_node_snippet_no_required_fields() {
        let rs = bool_enum_ruleset();
        // Build a rule with no required children (min=0)
        let snippet = generate_node_snippet("my_block", &[], &rs);
        assert!(snippet.contains("my_block = {"), "got: {}", snippet);
        assert!(
            snippet.contains("$0"),
            "expected cursor $0, got: {}",
            snippet
        );
    }

    #[test]
    fn test_generate_node_snippet_with_required_bool() {
        let rs = bool_enum_ruleset();
        // Build rules with min=1
        let required_rules = vec![(
            RuleType::LeafRule {
                left: NewField::SpecificField("active".to_string()),
                right: NewField::ValueField(ValueType::Bool),
            },
            Options {
                min: 1,
                ..Options::default()
            },
        )];
        let snippet = generate_node_snippet("my_type", &required_rules, &rs);
        assert!(snippet.contains("my_type = {"), "got: {}", snippet);
        assert!(
            snippet.contains("active"),
            "expected 'active' in snippet: {}",
            snippet
        );
        assert!(
            snippet.contains("yes") || snippet.contains("${1"),
            "expected bool placeholder: {}",
            snippet
        );
    }

    #[test]
    fn test_generate_node_snippet_with_required_enum() {
        let rs = bool_enum_ruleset();
        let required_rules = vec![(
            RuleType::LeafRule {
                left: NewField::SpecificField("kind".to_string()),
                right: NewField::ValueField(ValueType::Enum("my_enum".to_string())),
            },
            Options {
                min: 1,
                ..Options::default()
            },
        )];
        let snippet = generate_node_snippet("my_type", &required_rules, &rs);
        // The enum values alpha, beta, gamma should appear as choices
        assert!(
            snippet.contains("alpha"),
            "expected enum choices in snippet: {}",
            snippet
        );
    }

    #[test]
    fn test_generate_node_snippet_ignores_optional_fields() {
        let rs = bool_enum_ruleset();
        // Only the min=1 field should appear; min=0 should not.
        let rules = vec![
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("required_field".to_string()),
                    right: NewField::ValueField(ValueType::Bool),
                },
                Options {
                    min: 1,
                    ..Options::default()
                },
            ),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("optional_field".to_string()),
                    right: NewField::ValueField(ValueType::Bool),
                },
                Options {
                    min: 0,
                    ..Options::default()
                },
            ),
        ];
        let snippet = generate_node_snippet("my_type", &rules, &rs);
        assert!(
            snippet.contains("required_field"),
            "should have required: {}",
            snippet
        );
        assert!(
            !snippet.contains("optional_field"),
            "should not have optional: {}",
            snippet
        );
    }

    // ── alias (effect/trigger) snippet tests ─────────────────────────────────

    /// A ruleset with two effect aliases: `if` (a block effect with a required
    /// `limit` child) and `add_political_power` (a value effect).
    fn alias_effect_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();
        // alias[effect:if] = { limit = { } alias_name[effect] = ... }
        rs.aliases.push((
            "effect:if".to_string(),
            (
                RuleType::NodeRule {
                    left: NewField::SpecificField("alias[effect:if]".to_string()),
                    rules: [
                        // `limit` has no ## cardinality -> required (1..1).
                        (
                            RuleType::NodeRule {
                                left: NewField::SpecificField("limit".to_string()),
                                rules: [].into(),
                            },
                            Options {
                                min: 1,
                                ..Options::default()
                            },
                        ),
                        // The effect-recursion alias child is not a SpecificField,
                        // so it must not appear in the snippet.
                        (
                            RuleType::LeafRule {
                                left: NewField::AliasField("effect".to_string()),
                                right: NewField::AliasField("effect".to_string()),
                            },
                            Options::default(),
                        ),
                    ]
                    .into(),
                },
                Options::default(),
            ),
        ));
        // alias[effect:add_political_power] = variable_field
        rs.aliases.push((
            "effect:add_political_power".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[effect:add_political_power]".to_string()),
                    right: NewField::ScalarField,
                },
                Options::default(),
            ),
        ));
        rs.reindex();
        rs
    }

    /// The rule context inside an effect block: a single `alias_name[effect]`
    /// usage, which drives the alias-expansion arm for category `effect`.
    fn effect_alias_usage() -> Vec<NewRule> {
        vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("effect".to_string()),
                right: NewField::AliasField("effect".to_string()),
            },
            Options::default(),
        )]
    }

    #[test]
    fn alias_block_effect_completes_to_block_with_required_child() {
        // `if` should tab-complete to a block that pre-fills its required
        // `limit = { }` with proper tab stops (cwtools-vscode autocomplete ask).
        let rs = alias_effect_ruleset();
        let info = cwtools_info::InfoService::new();
        let rules = effect_alias_usage();
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;

        let if_item = items
            .iter()
            .find(|i| i.label == "if")
            .expect("'if' completion");
        assert_eq!(if_item.insert_text_format, Some(InsertTextFormat::SNIPPET));
        let snip = if_item.insert_text.as_deref().unwrap_or("");
        assert!(snip.starts_with("if = {"), "if snippet: {}", snip);
        assert!(
            snip.contains("limit ="),
            "if snippet missing limit: {}",
            snip
        );
    }

    #[test]
    fn alias_value_effect_completes_with_equals() {
        // `add_political_power` should tab-complete to `add_political_power = `
        // with the cursor after the `=`, ready for the value.
        let rs = alias_effect_ruleset();
        let info = cwtools_info::InfoService::new();
        let rules = effect_alias_usage();
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;

        let appp = items
            .iter()
            .find(|i| i.label == "add_political_power")
            .expect("'add_political_power' completion");
        assert_eq!(appp.insert_text_format, Some(InsertTextFormat::SNIPPET));
        let snip = appp.insert_text.as_deref().unwrap_or("");
        assert!(
            snip.starts_with("add_political_power = "),
            "value-effect snippet: {}",
            snip
        );
        // A value effect is a single line, not a `{ … }` block.
        assert!(!snip.contains('\n'), "should not be a block: {}", snip);
        assert!(!snip.contains("= {"), "should not open a clause: {}", snip);
    }

    // ── #94: control-flow keys must not sink below scope-matched effects ─────

    /// Effect ruleset mirroring the real hoi4 config shape: a plain effect
    /// carrying `## scope = country`, `if` carrying `## scope = any`, and
    /// `else` with no scope annotation at all. Both `if` and `else` recurse
    /// into `alias_name[effect]`.
    fn scoped_effect_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();
        let recursive_body = || {
            vec![(
                RuleType::LeafRule {
                    left: NewField::AliasField("effect".to_string()),
                    right: NewField::AliasField("effect".to_string()),
                },
                Options::default(),
            )]
        };
        rs.aliases.push((
            "effect:if".to_string(),
            (
                RuleType::NodeRule {
                    left: NewField::SpecificField("alias[effect:if]".to_string()),
                    rules: recursive_body().into(),
                },
                Options {
                    required_scopes: vec!["any".to_string()],
                    ..Options::default()
                },
            ),
        ));
        rs.aliases.push((
            "effect:else".to_string(),
            (
                RuleType::NodeRule {
                    left: NewField::SpecificField("alias[effect:else]".to_string()),
                    rules: recursive_body().into(),
                },
                Options::default(),
            ),
        ));
        rs.aliases.push((
            "effect:add_political_power".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[effect:add_political_power]".to_string()),
                    right: NewField::ScalarField,
                },
                Options {
                    required_scopes: vec!["country".to_string()],
                    ..Options::default()
                },
            ),
        ));
        rs.reindex();
        rs
    }

    #[test]
    fn control_flow_effects_rank_with_scope_matched_effects() {
        // In a country-scope effect block every `## scope = country` effect
        // ranks in the top bucket, and `if`/`else` (valid in ANY scope) must
        // not sink below them (#94).
        let rs = scoped_effect_ruleset();
        let info = cwtools_info::InfoService::new();
        // Hoi4's registry is config-driven (empty here); Stellaris has the same
        // country scope hardcoded, which is all this test needs.
        let reg = cwtools_game::scope_registry::ScopeRegistry::from_hardcoded(
            cwtools_game::constants::Game::Stellaris,
        );
        let country = reg.id_of("country").expect("country scope");
        let items = completions_from_rules(
            &effect_alias_usage(),
            &rs,
            &info,
            "stellaris",
            &HashSet::new(),
            &Default::default(),
            Some(&reg),
            Some(country),
            "",
        )
        .0;
        let sort = |label: &str| {
            items
                .iter()
                .find(|i| i.label == label)
                .unwrap_or_else(|| panic!("no '{}' item", label))
                .sort_text
                .clone()
                .expect("sort_text")
        };
        let plain = sort("add_political_power");
        assert!(plain.starts_with("0_"), "scope match bucket: {}", plain);
        for label in ["if", "else"] {
            let s = sort(label);
            assert!(
                s.starts_with("0_"),
                "'{}' must rank with scope-matched effects, got sort_text {:?}",
                label,
                s
            );
        }
    }

    #[test]
    fn typed_token_ranks_exact_match_first_and_survives_cap() {
        // A capped list must keep and lead with the exact match for the typed
        // token, even when >cap better-bucketed items subsequence-match it —
        // otherwise typing `if` in a big effect block buries or drops `if` (#94).
        let mut items: Vec<CompletionItem> = (0..1500)
            .map(|i| {
                let label = format!("ai_f_{:04}", i);
                CompletionItem {
                    sort_text: Some(format!("0_{}", label)),
                    label,
                    ..Default::default()
                }
            })
            .collect();
        items.push(CompletionItem {
            label: "if".to_string(),
            sort_text: Some("3_if".to_string()),
            ..Default::default()
        });
        items.push(CompletionItem {
            label: "iff_prefixed".to_string(),
            sort_text: Some("3_iff_prefixed".to_string()),
            ..Default::default()
        });
        let (filtered, dropped) = filter_and_cap(items, "if", 1000);
        assert!(dropped);
        assert_eq!(filtered.len(), 1000);
        assert_eq!(filtered[0].label, "if", "exact match must lead");
        assert_eq!(
            filtered[1].label, "iff_prefixed",
            "prefix match ranks ahead of subsequence matches"
        );
    }

    // ── root-type snippets tests ─────────────────────────────────────────────

    #[test]
    fn test_root_type_snippets_path_match() {
        let rs = bool_enum_ruleset();
        // The type "my_type" is in path "events"
        let items = root_type_snippets(&rs, "events/test.txt");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"my_type") || !labels.is_empty(),
            "expected type items: {:?}",
            labels
        );
    }

    #[test]
    fn test_root_type_snippets_path_mismatch() {
        let rs = bool_enum_ruleset();
        // The type "my_type" is in path "events", not "common"
        let items = root_type_snippets(&rs, "common/foo.txt");
        assert!(
            items.is_empty(),
            "should not offer types for wrong path, got: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    // ── #67: bool trigger alias must insert `key = ${yes/no}` ────────────────

    fn bool_trigger_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();
        rs.aliases.push((
            "trigger:always".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[trigger:always]".to_string()),
                    right: NewField::ValueField(ValueType::Bool),
                },
                Options::default(),
            ),
        ));
        rs.reindex();
        rs
    }

    #[test]
    fn alias_bool_trigger_completes_with_equals_and_yesno() {
        // #67: `alias[trigger:always] = bool` must complete to
        // `always = ${1|yes,no|}$0`, not a bare `${1|yes,no|}` with no `=`.
        let rs = bool_trigger_ruleset();
        let info = cwtools_info::InfoService::new();
        let rules = vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("trigger".to_string()),
                right: NewField::AliasField("trigger".to_string()),
            },
            Options::default(),
        )];
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;

        let always = items
            .iter()
            .find(|i| i.label == "always")
            .expect("'always' completion missing");
        let snip = always.insert_text.as_deref().unwrap_or("");
        assert!(
            snip.starts_with("always = "),
            "bool trigger must insert 'always = ', got: {:?}",
            always.insert_text
        );
        // #77: pin the corrected shape `always = ${1|yes,no|}$0`. A choice on the
        // final `$0` tab stop (`${0|…|}`) is inserted literally by VS Code, so the
        // choice must sit on tab stop 1 with a trailing `$0`.
        assert!(
            snip.contains("${1|") && snip.ends_with("$0") && !snip.contains("${0|"),
            "bool trigger must use a non-zero choice tab stop ending in $0, got: {:?}",
            always.insert_text
        );
        assert!(
            snip.contains("yes") && snip.contains("no"),
            "bool trigger must offer yes/no choices, got: {:?}",
            always.insert_text
        );
    }

    // ── #77: has_dlc enum snippet — tab stops, escaping, quoting ──────────────

    /// A ruleset with `alias[trigger:has_dlc] = enum[dlc]` whose enum mixes a
    /// multi-word value, a value with an embedded comma, a colon value, and a
    /// bare identifier — the shapes that exercise all three snippet defects.
    fn dlc_enum_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();
        rs.enums.push(EnumDefinition {
            key: "dlc".to_string(),
            description: String::new(),
            values: vec![
                "Together for Victory".to_string(),
                "No Compromise, No Surrender".to_string(),
                "expansion:foo".to_string(),
                "base_game".to_string(),
            ],
        });
        rs.aliases.push((
            "trigger:has_dlc".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[trigger:has_dlc]".to_string()),
                    right: NewField::ValueField(ValueType::Enum("dlc".to_string())),
                },
                Options::default(),
            ),
        ));
        rs.reindex();
        rs
    }

    #[test]
    fn alias_dlc_enum_snippet_escapes_and_quotes_choices() {
        // #77: an enum alias must complete to `has_dlc = ${1|...|}$0` — a choice
        // on tab stop 1 (not the unsupported `$0`), with each choice value quoted
        // when it has whitespace and its delimiters escaped.
        let rs = dlc_enum_ruleset();
        let info = cwtools_info::InfoService::new();
        let rules = vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("trigger".to_string()),
                right: NewField::AliasField("trigger".to_string()),
            },
            Options::default(),
        )];
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;

        let has_dlc = items
            .iter()
            .find(|i| i.label == "has_dlc")
            .expect("'has_dlc' completion missing");
        assert_eq!(has_dlc.insert_text_format, Some(InsertTextFormat::SNIPPET));
        let snip = has_dlc.insert_text.as_deref().unwrap_or("");
        assert!(
            snip.starts_with("has_dlc = ${1|"),
            "must be a choice on tab stop 1, got: {:?}",
            has_dlc.insert_text
        );
        assert!(
            snip.ends_with("|}$0"),
            "must end with a trailing $0, got: {:?}",
            has_dlc.insert_text
        );
        // Multi-word values are quoted.
        assert!(
            snip.contains("\"Together for Victory\""),
            "multi-word value must be quoted, got: {:?}",
            has_dlc.insert_text
        );
        // The comma inside a value is escaped so it can't split the choice, and
        // the quotes are kept around the whitespace-bearing value.
        assert!(
            snip.contains("\"No Compromise\\, No Surrender\""),
            "embedded comma must be escaped and value quoted, got: {:?}",
            has_dlc.insert_text
        );
        // A bare identifier stays unquoted.
        assert!(
            snip.contains("base_game") && !snip.contains("\"base_game\""),
            "bare identifier must stay unquoted, got: {:?}",
            has_dlc.insert_text
        );
    }

    #[test]
    fn value_completions_enum_quotes_spaced_values() {
        // #77: at a value position, an enum member with whitespace inserts quoted
        // (so it parses as one token); a bare identifier inserts as its label.
        let rs = dlc_enum_ruleset();
        let info = cwtools_info::InfoService::new();
        let value_rules = vec![(
            RuleType::LeafValueRule {
                right: NewField::ValueField(ValueType::Enum("dlc".to_string())),
            },
            Options::default(),
        )];
        let items = value_completions(
            &value_rules,
            &rs,
            &info,
            None,
            "hoi4",
            ValueCompletionSets {
                modifier_keys: &HashSet::new(),
                modifier_scopes: &Default::default(),
                loc_keys: &HashSet::new(),
            },
            None,
            "",
        )
        .0;

        let spaced = items
            .iter()
            .find(|i| i.label == "Together for Victory")
            .expect("spaced enum value missing");
        assert_eq!(
            spaced.insert_text.as_deref(),
            Some("\"Together for Victory\""),
            "spaced value must insert quoted, got: {:?}",
            spaced.insert_text
        );
        let bare = items
            .iter()
            .find(|i| i.label == "base_game")
            .expect("bare enum value missing");
        assert_eq!(
            bare.insert_text, None,
            "bare identifier must not carry a quoted insert_text, got: {:?}",
            bare.insert_text
        );
    }

    // ── #64: type-pattern alias expands to type instances ────────────────────

    fn scripted_effect_alias_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();
        rs.types.push(TypeDefinition {
            name: "scripted_effect".to_string(),
            name_field: None,
            path_options: PathOptions {
                paths: vec!["common/scripted_effects".to_string()],
                path_strict: false,
                path_file: None,
                path_extension: None,
                paths_lower: Vec::new(),
                ..Default::default()
            },
            subtypes: Vec::new(),
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
        // alias[effect:<scripted_effect>] = yes
        rs.aliases.push((
            "effect:<scripted_effect>".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[effect:<scripted_effect>]".to_string()),
                    right: NewField::SpecificField("yes".to_string()),
                },
                Options::default(),
            ),
        ));
        rs.reindex();
        rs
    }

    #[test]
    fn alias_type_pattern_expands_to_instances() {
        // #64: type-pattern aliases like `alias[effect:<scripted_effect>] = yes`
        // must emit one KEYWORD item per known instance, NOT the raw placeholder
        // label `<scripted_effect>`.
        let rs = scripted_effect_alias_ruleset();
        let mut info = cwtools_info::InfoService::new();
        let mut per_type: std::collections::HashMap<String, Vec<cwtools_info::TypeInstance>> =
            std::collections::HashMap::new();
        per_type.insert(
            "scripted_effect".to_string(),
            vec![cwtools_info::TypeInstance {
                name: "my_special_effect".to_string(),
                location: cwtools_info::SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        info.type_index
            .merge("file:///scripted_effects/se.txt", per_type);

        let rules = vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("effect".to_string()),
                right: NewField::AliasField("effect".to_string()),
            },
            Options::default(),
        )];
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;

        assert!(
            items.iter().any(|i| i.label == "my_special_effect"),
            "type-pattern alias must expand to type instances, got: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        assert!(
            !items.iter().any(|i| i.label == "<scripted_effect>"),
            "raw pattern placeholder must not appear in labels, got: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        // The instance's snippet should be `my_special_effect = yes` because the
        // alias rule has `right = SpecificField("yes")`.
        let item = items
            .iter()
            .find(|i| i.label == "my_special_effect")
            .unwrap();
        let snip = item.insert_text.as_deref().unwrap_or("");
        assert!(
            snip.contains("= yes"),
            "scripted_effect snippet should contain '= yes', got: {:?}",
            item.insert_text
        );
    }

    // ── #65: alias_keys_field[modifier] must emit modifier keys ──────────────

    #[test]
    fn alias_keys_field_emits_modifier_keys() {
        // #65: a rule with `alias_keys_field[modifier]` on its left side (as in
        // `dynamic_modifier` blocks) must offer modifier keys as completions.
        let rs = bool_enum_ruleset(); // arbitrary ruleset with reindex() called
        let info = cwtools_info::InfoService::new();
        let modifier_keys: HashSet<String> = ["my_modifier".to_string(), "other_mod".to_string()]
            .into_iter()
            .collect();
        let rules = vec![(
            RuleType::LeafRule {
                left: NewField::AliasValueKeysField("modifier".to_string()),
                right: NewField::ValueField(ValueType::Float {
                    min: -1e8,
                    max: 1e8,
                }),
            },
            Options::default(),
        )];
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &modifier_keys,
            &Default::default(),
            None,
            None,
            "",
        )
        .0;

        assert!(
            items.iter().any(|i| i.label == "my_modifier"),
            "alias_keys_field[modifier] must offer modifier keys, got: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        assert!(
            items.iter().any(|i| i.label == "other_mod"),
            "alias_keys_field[modifier] must offer all modifier keys, got: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    // ── #66: duplicate labels are removed from the completion list ───────────

    #[test]
    fn completions_from_rules_deduplicates() {
        // #66: when the same concrete field appears in multiple rule entries
        // (e.g. from subtype-flattening), the label must appear only once.
        let rs = bool_enum_ruleset();
        let info = cwtools_info::InfoService::new();
        // Two identical `active = bool` rules.
        let rules = vec![
            make_leaf_rule("active", NewField::ValueField(ValueType::Bool)),
            make_leaf_rule("active", NewField::ValueField(ValueType::Bool)),
        ];
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;
        let count = items.iter().filter(|i| i.label == "active").count();
        assert_eq!(
            count, 1,
            "duplicate label 'active' must appear exactly once, got {} copies",
            count
        );
    }

    // ── snippet grammar validity (cwtools-vscode#89 snippet hardening) ───────

    /// A focused check mirroring VS Code's `snippetParser.ts`: rejects constructs
    /// the editor inserts literally or mishandles. Stricter than the (lenient)
    /// real parser about a literal `{`/`}` inside a placeholder default, which is
    /// where the node-required prefill used to leak an unescaped `}`.
    fn snippet_defect(s: &str) -> std::result::Result<(), String> {
        let c: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < c.len() {
            match c[i] {
                '\\' => i += 2,
                '$' => i = scan_dollar(&c, i)?,
                _ => i += 1,
            }
        }
        Ok(())
    }

    /// Consume a `$` construct at `i` (`c[i] == '$'`), returning the next index.
    /// A bare `$` (or a `$name` variable) is literal text to the parser.
    fn scan_dollar(c: &[char], i: usize) -> std::result::Result<usize, String> {
        match c.get(i + 1) {
            Some(d) if d.is_ascii_digit() => {
                let mut j = i + 1;
                while j < c.len() && c[j].is_ascii_digit() {
                    j += 1;
                }
                Ok(j)
            }
            Some('{') => scan_brace(c, i),
            _ => Ok(i + 1),
        }
    }

    /// Consume a `${ … }` construct starting at `i` (`c[i..i+2] == "${"`).
    fn scan_brace(c: &[char], i: usize) -> std::result::Result<usize, String> {
        let digits_start = i + 2;
        let mut j = digits_start;
        while j < c.len() && c[j].is_ascii_digit() {
            j += 1;
        }
        if j == digits_start {
            return Err("`${` without a tab-stop number".into());
        }
        let is_zero = c[digits_start..j].iter().all(|d| *d == '0');
        match c.get(j) {
            Some('}') => Ok(j + 1),
            Some('|') => scan_choice(c, j, is_zero),
            Some(':') => scan_default(c, j + 1),
            _ => Err("malformed `${…}`".into()),
        }
    }

    /// Consume a choice body from its opening `|` (`c[j] == '|'`) to the `|}` close.
    fn scan_choice(c: &[char], j: usize, is_zero: bool) -> std::result::Result<usize, String> {
        if is_zero {
            return Err("choice on tab stop $0 is inserted literally".into());
        }
        let mut k = j + 1;
        while k < c.len() {
            match c[k] {
                '\\' => k += 2,
                '|' if c.get(k + 1) == Some(&'}') => return Ok(k + 2),
                '|' => return Err("unescaped `|` in a choice value".into()),
                _ => k += 1,
            }
        }
        Err("unterminated choice".into())
    }

    /// Consume a placeholder default from the first default char to the matching
    /// unescaped `}`. A bare `{` here is the `${1:{ }}` defect (the `}` closes early).
    fn scan_default(c: &[char], mut k: usize) -> std::result::Result<usize, String> {
        while k < c.len() {
            match c[k] {
                '\\' => k += 2,
                '}' => return Ok(k + 1),
                '$' => k = scan_dollar(c, k)?,
                '{' => return Err("bare `{` in a placeholder default".into()),
                _ => k += 1,
            }
        }
        Err("unterminated placeholder default".into())
    }

    #[test]
    fn snippet_checker_accepts_valid_and_rejects_defects() {
        for good in [
            "k = { $1 }",
            "k = {\n\t$0\n}",
            "add = ${1}$0",
            "always = ${1|yes,no|}$0",
            "has_dlc = ${1|\"a b\",c|}",
            "lit = a\\$b\\}c$0",
            "plain = yes$0",
        ] {
            assert!(
                snippet_defect(good).is_ok(),
                "should accept {:?}: {:?}",
                good,
                snippet_defect(good)
            );
        }
        for bad in [
            "k = ${1:{ }}",  // bare `{` default — the old node-required bug
            "c = ${0|a,b|}", // choice on $0
            "u = ${1:foo",   // unterminated default
            "u = ${1|a,b|",  // unterminated choice
            "u = ${}",       // no tab-stop number
        ] {
            assert!(snippet_defect(bad).is_err(), "should reject {:?}", bad);
        }
    }

    /// The SNIPPET-format `insert_text` of every item, for the sweep below.
    fn snippet_texts(items: &[CompletionItem]) -> Vec<String> {
        items
            .iter()
            .filter(|it| it.insert_text_format == Some(InsertTextFormat::SNIPPET))
            .filter_map(|it| it.insert_text.clone())
            .collect()
    }

    #[test]
    fn all_generated_snippets_are_grammar_valid() {
        let info = cwtools_info::InfoService::new();
        let empty = HashSet::new();
        let mut snips: Vec<String> = Vec::new();

        // Key-context snippets across the leaf/node/enum builder arms.
        let rs = bool_enum_ruleset();
        let rules = match rs.root_rules.first() {
            Some(RootRule::TypeRule(_, (RuleType::NodeRule { rules, .. }, _))) => rules.as_ref(),
            _ => panic!("expected TypeRule"),
        };
        snips.extend(snippet_texts(
            &completions_from_rules(
                rules,
                &rs,
                &info,
                "hoi4",
                &empty,
                &Default::default(),
                None,
                None,
                "",
            )
            .0,
        ));
        snips.extend(snippet_texts(&root_type_snippets(&rs, "events/x.txt")));

        // Alias block (required child prefill) + value alias.
        let rs = alias_effect_ruleset();
        snips.extend(snippet_texts(
            &completions_from_rules(
                &effect_alias_usage(),
                &rs,
                &info,
                "hoi4",
                &empty,
                &Default::default(),
                None,
                None,
                "",
            )
            .0,
        ));

        // Enum choice with spaced / comma / colon values.
        let rs = dlc_enum_ruleset();
        let trigger_usage = vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("trigger".to_string()),
                right: NewField::AliasField("trigger".to_string()),
            },
            Options::default(),
        )];
        snips.extend(snippet_texts(
            &completions_from_rules(
                &trigger_usage,
                &rs,
                &info,
                "hoi4",
                &empty,
                &Default::default(),
                None,
                None,
                "",
            )
            .0,
        ));

        // A required NODE child — the case that used to emit `${1:{ }}`.
        let node_required = vec![(
            RuleType::NodeRule {
                left: NewField::SpecificField("child".to_string()),
                rules: [].into(),
            },
            Options {
                min: 1,
                ..Options::default()
            },
        )];
        let node_snip = generate_node_snippet("outer", &node_required, &rs);
        assert!(
            node_snip.contains("child = { $1 }"),
            "required node child must use an interior tab stop, got: {}",
            node_snip
        );
        assert!(
            !node_snip.contains("${1:{"),
            "required node child must not use a brace default, got: {}",
            node_snip
        );
        snips.push(node_snip);

        assert!(!snips.is_empty(), "sweep produced no snippets");
        for s in &snips {
            assert!(
                snippet_defect(s).is_ok(),
                "generated snippet {:?} is invalid: {}",
                s,
                snippet_defect(s).unwrap_err()
            );
        }
    }

    #[test]
    fn specific_field_literal_is_snippet_escaped() {
        // A concrete alias value carrying `$`/`}` must be escaped so VS Code
        // doesn't read it as a variable/tab stop or truncate the snippet.
        let mut rs = RuleSet::new();
        rs.aliases.push((
            "effect:danger".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[effect:danger]".to_string()),
                    right: NewField::SpecificField("a$b}c".to_string()),
                },
                Options::default(),
            ),
        ));
        rs.reindex();
        let info = cwtools_info::InfoService::new();
        let usage = vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("effect".to_string()),
                right: NewField::AliasField("effect".to_string()),
            },
            Options::default(),
        )];
        let items = completions_from_rules(
            &usage,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;
        let danger = items
            .iter()
            .find(|i| i.label == "danger")
            .expect("danger completion");
        let snip = danger.insert_text.as_deref().unwrap_or("");
        assert!(
            snip.contains("a\\$b\\}c"),
            "literal must be snippet-escaped, got: {:?}",
            snip
        );
        assert!(
            snippet_defect(snip).is_ok(),
            "escaped snippet must be valid, got: {:?}",
            snip
        );
    }

    // keep Arc in scope to avoid unused-import warning when no test uses it
    const _: fn() = || {
        let _ = Arc::new(());
    };
}

// ── MD-scale completion micro-benchmark (ignored, manual) ────────────────────
//
// Synthetic ruleset + type index sized like Millennium Dawn (thousands of
// pattern-expanded scripted effects, thousands of modifiers, high-cardinality
// type refs). Run with:
//
//   cargo test --release -p cwtools_lsp --bin cwtools-server -- \
//     --ignored --nocapture perf_completion_synthetic
#[cfg(test)]
mod perf_bench {
    use std::collections::{HashMap, HashSet};

    use cwtools_rules::rules_types::{NewField, NewRule, Options, RuleSet, RuleType};

    use super::*;

    const EXACT_EFFECTS: usize = 600;
    const SCRIPTED_EFFECTS: usize = 8_000;
    const PLAIN_MODIFIERS: usize = 5_000;
    const TEMPLATED_BUILDINGS: usize = 3_000;
    const STATES: usize = 2_000;

    fn alias_usage(cat: &str) -> Vec<NewRule> {
        vec![(
            RuleType::LeafRule {
                left: NewField::AliasField(cat.to_string()),
                right: NewField::AliasField(cat.to_string()),
            },
            Options::default(),
        )]
    }

    fn synthetic_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();
        for i in 0..EXACT_EFFECTS {
            let scopes = if i % 2 == 0 {
                vec!["country".to_string()]
            } else {
                Vec::new()
            };
            rs.aliases.push((
                format!("effect:eff_{:04}", i),
                (
                    RuleType::LeafRule {
                        left: NewField::SpecificField(format!("alias[effect:eff_{:04}]", i)),
                        right: NewField::ScalarField,
                    },
                    Options {
                        required_scopes: scopes,
                        ..Options::default()
                    },
                ),
            ));
        }
        for name in ["if", "else_if", "else"] {
            rs.aliases.push((
                format!("effect:{}", name),
                (
                    RuleType::NodeRule {
                        left: NewField::SpecificField(format!("alias[effect:{}]", name)),
                        rules: alias_usage("effect").into(),
                    },
                    Options::default(),
                ),
            ));
        }
        // Pattern alias expanded against the type index (scripted effects).
        rs.aliases.push((
            "effect:<scripted_effect>".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[effect:<scripted_effect>]".to_string()),
                    right: NewField::ValueField(cwtools_rules::rules_types::ValueType::Bool),
                },
                Options::default(),
            ),
        ));
        for i in 0..PLAIN_MODIFIERS {
            rs.modifiers
                .push((format!("mod_{:04}", i), "country".to_string()));
        }
        rs.modifiers.push((
            "production_speed_<building>_factor".to_string(),
            "state".to_string(),
        ));
        rs.modifier_categories
            .insert("country".to_string(), vec!["country".to_string()]);
        rs.modifier_categories
            .insert("state".to_string(), vec!["state".to_string()]);
        rs.reindex();
        rs
    }

    fn synthetic_info() -> cwtools_info::InfoService {
        let mut info = cwtools_info::InfoService::new();
        let inst = |name: String| cwtools_info::TypeInstance {
            name,
            location: cwtools_info::SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        };
        let mut per_type: HashMap<String, Vec<cwtools_info::TypeInstance>> = HashMap::new();
        per_type.insert(
            "scripted_effect".to_string(),
            (0..SCRIPTED_EFFECTS)
                .map(|i| inst(format!("se_do_things_{:05}", i)))
                .collect(),
        );
        per_type.insert(
            "building".to_string(),
            (0..TEMPLATED_BUILDINGS)
                .map(|i| inst(format!("building_{:04}", i)))
                .collect(),
        );
        per_type.insert(
            "state".to_string(),
            (0..STATES).map(|i| inst(format!("{}", i + 1))).collect(),
        );
        info.type_index.merge("file:///bench/defs.txt", per_type);
        info
    }

    fn bench<F: FnMut() -> usize>(label: &str, mut f: F) {
        const WARMUP: usize = 3;
        const ITERS: usize = 30;
        for _ in 0..WARMUP {
            f();
        }
        let mut times = Vec::with_capacity(ITERS);
        let mut items = 0;
        for _ in 0..ITERS {
            let t = std::time::Instant::now();
            items = f();
            times.push(t.elapsed());
        }
        times.sort();
        let mean = times.iter().sum::<std::time::Duration>() / ITERS as u32;
        eprintln!(
            "{:>28}: mean {:>10.1?}  min {:>10.1?}  max {:>10.1?}  ({} items, n={})",
            label,
            mean,
            times[0],
            times[ITERS - 1],
            items,
            ITERS
        );
    }

    #[test]
    #[ignore]
    fn perf_completion_synthetic() {
        let rs = synthetic_ruleset();
        let info = synthetic_info();
        let reg = cwtools_game::scope_registry::ScopeRegistry::from_hardcoded(
            cwtools_game::constants::Game::Stellaris,
        );
        let country = reg.id_of("country").expect("country scope");
        let modifier_keys: HashSet<String> =
            cwtools_validation::build_modifier_keys(&rs, &info.type_index);
        eprintln!(
            "fixture: {} aliases, {} modifier keys, {} scripted effects, {} states",
            rs.aliases.len(),
            modifier_keys.len(),
            SCRIPTED_EFFECTS,
            STATES
        );

        let modifier_scopes = expanded_modifier_scopes(&rs, &info.type_index);
        let effect_rules = alias_usage("effect");
        let modifier_rules = alias_usage("modifier");

        for token in ["", "if", "add_p"] {
            let label = if token.is_empty() {
                "effect key (no token)".to_string()
            } else {
                format!("effect key (token {:?})", token)
            };
            bench(&label, || {
                let (items, dropped) = completions_from_rules(
                    &effect_rules,
                    &rs,
                    &info,
                    "stellaris",
                    &modifier_keys,
                    &modifier_scopes,
                    Some(&reg),
                    Some(country),
                    token,
                );
                let (items, _, _) = prepare_context_items(
                    items,
                    dropped,
                    token,
                    true,
                    true,
                    CONTEXT_COMPLETE_THRESHOLD,
                    CONTEXT_CAP,
                );
                items.len()
            });
        }

        // Duplicated alias rule (subtype flattening can repeat one): the
        // seen-categories guard should make the repeat free.
        let effect_rules_dup: Vec<NewRule> = effect_rules
            .iter()
            .cloned()
            .chain(effect_rules.iter().cloned())
            .collect();
        bench("effect key (dup arm)", || {
            let (items, dropped) = completions_from_rules(
                &effect_rules_dup,
                &rs,
                &info,
                "stellaris",
                &modifier_keys,
                &modifier_scopes,
                Some(&reg),
                Some(country),
                "",
            );
            let (items, _, _) = prepare_context_items(
                items,
                dropped,
                "",
                true,
                true,
                CONTEXT_COMPLETE_THRESHOLD,
                CONTEXT_CAP,
            );
            items.len()
        });

        bench("modifier key (scoped)", || {
            let (items, dropped) = completions_from_rules(
                &modifier_rules,
                &rs,
                &info,
                "stellaris",
                &modifier_keys,
                &modifier_scopes,
                Some(&reg),
                Some(country),
                "",
            );
            let (items, _, _) = prepare_context_items(
                items,
                dropped,
                "",
                true,
                true,
                CONTEXT_COMPLETE_THRESHOLD,
                CONTEXT_CAP,
            );
            items.len()
        });

        let state_value_rules: Vec<NewRule> = vec![(
            RuleType::LeafRule {
                left: NewField::SpecificField("add_state_core".to_string()),
                right: NewField::TypeField(cwtools_rules::rules_types::TypeType::Simple(
                    "state".to_string(),
                )),
            },
            Options::default(),
        )];
        bench("state value (token 28)", || {
            let (items, dropped) = value_completions(
                &state_value_rules,
                &rs,
                &info,
                Some(&reg),
                "stellaris",
                ValueCompletionSets {
                    modifier_keys: &modifier_keys,
                    modifier_scopes: &modifier_scopes,
                    loc_keys: &HashSet::new(),
                },
                Some(country),
                "28",
            );
            let (items, _, _) = prepare_context_items(
                items,
                dropped,
                "28",
                true,
                true,
                CONTEXT_COMPLETE_THRESHOLD,
                CONTEXT_CAP,
            );
            items.len()
        });
    }

    /// The loc-key selection exactly as it stood before this pass: a linear
    /// sweep of the whole union with a matcher that kept walking the haystack
    /// after the needle was exhausted. Kept as the measurement baseline and as
    /// the oracle the current implementation is checked against.
    mod reference {
        use std::collections::{BTreeSet, HashSet};

        pub(super) fn subsequence_match(haystack: &str, needle: &str) -> bool {
            if needle.is_empty() {
                return true;
            }
            let mut needle_it = needle.chars().flat_map(char::to_lowercase).peekable();
            for c in haystack.chars().flat_map(char::to_lowercase) {
                if needle_it.peek() == Some(&c) {
                    needle_it.next();
                }
            }
            needle_it.peek().is_none()
        }

        pub(super) fn select<'a>(
            keys: impl Iterator<Item = &'a str>,
            token: &str,
            cap: usize,
        ) -> HashSet<String> {
            let mut selected = BTreeSet::new();
            for key in keys.filter(|key| subsequence_match(key, token)) {
                let ranked = (
                    !key.get(..token.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(token)),
                    key,
                );
                if selected.len() < cap {
                    selected.insert(ranked);
                } else if selected.last().is_some_and(|largest| ranked < *largest)
                    && selected.insert(ranked)
                {
                    selected.pop_last();
                }
            }
            selected
                .into_iter()
                .map(|(_, key)| key.to_owned())
                .collect()
        }
    }

    #[test]
    fn subsequence_match_agrees_with_reference() {
        let haystacks = [
            "has_completed_focus",
            "MDS_focus_title",
            "",
            "a",
            "ünïcode_kéy",
            "\u{212A}elvin",
            "\u{130}stanbul",
            "focus",
        ];
        let needles = [
            "", "f", "hcf", "focus", "FOCUS", "xyz", "kelvin", "ü", "Ü", "istanbul", "a", "focuss",
        ];
        for hay in haystacks {
            for needle in needles {
                assert_eq!(
                    subsequence_match(hay, needle),
                    reference::subsequence_match(hay, needle),
                    "diverged on haystack {:?} needle {:?}",
                    hay,
                    needle
                );
            }
        }
    }

    /// Loc-key selection at Millennium-Dawn scale (mod + vanilla loc merged,
    /// 399,781 unique keys). `reference` is the sweep every keystroke used to
    /// pay, `linear` the same sweep with the early-exit matcher, `indexed` the
    /// selection served from the scan-built [`LocKeyIndex`]. All three are
    /// asserted to return identical key sets.
    #[test]
    #[ignore]
    fn perf_loc_completion_keys() {
        const LOC_KEYS: usize = 399_781;
        const OWNERS: [&str; 14] = [
            "mds",
            "politics",
            "focus",
            "hol",
            "eng",
            "usa",
            "ger",
            "sov",
            "generic",
            "decision",
            "idea",
            "event",
            "state",
            "equipment",
        ];
        const STEMS: [&str; 8] = [
            "title", "desc", "tooltip", "effect", "flavor", "name", "option", "log",
        ];

        let keys: HashSet<String> = (0..LOC_KEYS)
            .map(|i| {
                format!(
                    "{}_{}_{:06}",
                    OWNERS[i % OWNERS.len()],
                    STEMS[(i / OWNERS.len()) % STEMS.len()],
                    i
                )
            })
            .collect();
        let overlay: HashSet<String> = (0..400)
            .map(|i| format!("unsaved_open_yml_key_{:04}", i))
            .collect();
        let bytes: usize = keys.iter().map(|k| k.len()).sum();
        eprintln!(
            "fixture: {} loc keys ({} KiB of key text), {} overlay keys, cap {}",
            keys.len(),
            bytes / 1024,
            overlay.len(),
            CONTEXT_CAP
        );

        let t = std::time::Instant::now();
        let index = LocKeyIndex::build(keys.iter().map(String::as_str));
        eprintln!(
            "{:>28}: {:?} (once per workspace scan, {} keys)",
            "LocKeyIndex::build",
            t.elapsed(),
            index.len()
        );

        // "f"/"" are the first-keystroke cases; "mdsfoc" is subsequence-only;
        // "zqxv" matches nothing. "lltt" is the adversarial one: common enough
        // characters to clear the index's per-key character filter, rare enough
        // as a subsequence that the sweep can't fill the cap and stop early.
        for token in ["", "f", "mds_f", "mdsfoc", "zqxv", "lltt"] {
            let linear = loc_keys::select_loc_keys(
                keys.iter()
                    .map(String::as_str)
                    .chain(overlay.iter().map(String::as_str)),
                token,
                CONTEXT_CAP,
            );
            let indexed = index.select(token, overlay.iter().map(String::as_str), CONTEXT_CAP);
            let baseline = reference::select(
                keys.iter()
                    .map(String::as_str)
                    .chain(overlay.iter().map(String::as_str)),
                token,
                CONTEXT_CAP,
            );
            assert_eq!(baseline, linear, "token {:?} diverged (linear)", token);
            assert_eq!(baseline, indexed, "token {:?} diverged (indexed)", token);

            bench(&format!("before  (token {:?})", token), || {
                reference::select(
                    keys.iter()
                        .map(String::as_str)
                        .chain(overlay.iter().map(String::as_str)),
                    token,
                    CONTEXT_CAP,
                )
                .len()
            });
            bench(&format!("linear  (token {:?})", token), || {
                loc_keys::select_loc_keys(
                    keys.iter()
                        .map(String::as_str)
                        .chain(overlay.iter().map(String::as_str)),
                    token,
                    CONTEXT_CAP,
                )
                .len()
            });
            bench(&format!("indexed (token {:?})", token), || {
                index
                    .select(token, overlay.iter().map(String::as_str), CONTEXT_CAP)
                    .len()
            });
        }
    }
}
