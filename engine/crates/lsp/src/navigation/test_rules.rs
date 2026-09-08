//! Ruleset fixtures shared by the tests that exercise type-instance use sites
//! — the AST walk in [`super::use_sites`] and the code lens counts built on top
//! of it.

use cwtools_rules::rules_types::{
    EnumDefinition, NewField, Options, PathOptions, RootRule, RuleSet, RuleType, TypeDefinition,
    ValueType,
};

fn make_leaf_rule(key: &str, right: NewField) -> cwtools_rules::rules_types::NewRule {
    (
        RuleType::LeafRule {
            left: NewField::SpecificField(key.to_string()),
            right,
        },
        Options::default(),
    )
}

fn make_node_rule(
    key: &str,
    children: Vec<cwtools_rules::rules_types::NewRule>,
) -> cwtools_rules::rules_types::NewRule {
    (
        RuleType::NodeRule {
            left: NewField::SpecificField(key.to_string()),
            rules: children.into(),
        },
        Options::default(),
    )
}

/// A single `my_type` under `events/`, named by its `id` leaf.
pub(crate) fn bool_enum_ruleset() -> RuleSet {
    let mut rs = RuleSet::new();

    rs.enums.push(EnumDefinition {
        key: "my_enum".to_string(),
        description: String::new(),
        values: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
    });

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

/// [`bool_enum_ruleset`] plus a `base = <my_type>` type-ref leaf, the shape the
/// use-site walk and the reference index both classify against.
pub(crate) fn type_ref_ruleset() -> RuleSet {
    let mut rs = bool_enum_ruleset();
    rs.root_rules.push(RootRule::AliasRule(
        "effect:use_type".to_string(),
        (
            RuleType::NodeRule {
                left: NewField::SpecificField("use_type".to_string()),
                rules: [(
                    RuleType::LeafRule {
                        left: NewField::SpecificField("base".to_string()),
                        right: NewField::TypeField(cwtools_rules::rules_types::TypeType::Simple(
                            "my_type".to_string(),
                        )),
                    },
                    Options::default(),
                )]
                .into(),
            },
            Options::default(),
        ),
    ));
    rs.reindex();
    rs
}
