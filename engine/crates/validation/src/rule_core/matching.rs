use cwtools_rules::rules_types::*;
use smallvec::SmallVec;

use crate::common::*;

pub(crate) fn matching_candidates<'a, F>(
    rules: &'a [(RuleType, Options)],
    key: &str,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
    matcher: F,
) -> SmallVec<[&'a (RuleType, Options); 4]>
where
    F: Fn(&RuleType, &str, &RuleSet, Option<&cwtools_index::TypeIndex>) -> bool,
{
    let is_specific = |rt: &RuleType| {
        matches!(rt,
        RuleType::LeafRule { left: NewField::SpecificField(s), .. }
        | RuleType::NodeRule { left: NewField::SpecificField(s), .. } if s.eq_ignore_ascii_case(key))
    };
    let has_specific = rules
        .iter()
        .any(|(rt, _)| is_specific(rt) && matcher(rt, key, ruleset, type_index));
    rules
        .iter()
        .filter(|(rt, _)| {
            (!has_specific || is_specific(rt)) && matcher(rt, key, ruleset, type_index)
        })
        .collect()
}

pub(crate) fn rule_matches_leaf_key(
    rule_type: &RuleType,
    key: &str,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    match rule_type {
        RuleType::LeafRule { left, .. } | RuleType::NodeRule { left, .. } => {
            field_matches_key(left, key, ruleset, type_index)
        }
        _ => false,
    }
}

fn looks_like_scope_command(key: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "THIS",
        "ROOT",
        "PREV",
        "FROM",
        "FROMFROM",
        "FROMFROMFROM",
        "FROMFROMFROMFROM",
        "PREVPREV",
        "PREVPREVPREV",
        "OWNER",
        "CONTROLLER",
        "CAPITAL",
        "OVERLORD",
    ];
    if KEYWORDS.iter().any(|kw| key.eq_ignore_ascii_case(kw)) {
        return true;
    }
    if key.contains('.') || key.contains(':') {
        return true;
    }
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let len = key.len();
    (2..=4).contains(&len)
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && key.chars().any(|c| c.is_ascii_uppercase())
}

pub(super) fn is_scope_key(
    key: &str,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    looks_like_scope_command(key)
        || scope_links_contains(ruleset, key)
        || type_index.is_some_and(|idx| {
            idx.is_any_instance(key) || is_from_data_value_set_member(key, ruleset, idx)
        })
}

fn is_from_data_value_set_member(
    key: &str,
    ruleset: &RuleSet,
    type_index: &cwtools_index::TypeIndex,
) -> bool {
    if type_index.value_set_values.is_empty() {
        return false;
    }
    ruleset
        .link_inputs
        .iter()
        .filter(|li| li.from_data)
        .flat_map(|li| li.data_source.iter())
        .filter_map(|src| value_set_name(src))
        .any(|set| type_index.value_set_values.contains(set, key))
}

fn value_set_name(data_source: &str) -> Option<&str> {
    data_source
        .strip_prefix("value[")
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::trim)
}

fn scope_links_contains(ruleset: &RuleSet, key: &str) -> bool {
    if key.bytes().any(|b| b.is_ascii_uppercase()) {
        ruleset
            .scope_links
            .contains(&key.to_ascii_lowercase() as &str)
    } else {
        ruleset.scope_links.contains(key)
    }
}

fn parsed_pattern_matches(
    pat: &ParsedAliasPattern,
    key: &str,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
    permissive: bool,
) -> bool {
    match classify_pattern_match(pat, key, ruleset, type_index) {
        PatternMatch::Confident => true,
        PatternMatch::PermissiveOnly => permissive,
        PatternMatch::No => false,
    }
}

pub(super) enum PatternMatch {
    No,
    Confident,
    PermissiveOnly,
}

pub(super) fn classify_pattern_match(
    pat: &ParsedAliasPattern,
    key: &str,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> PatternMatch {
    let pre = pat.prefix.as_str();
    let suf = pat.suffix.as_str();
    if key.len() < pre.len() + suf.len() || !key.starts_with(pre) || !key.ends_with(suf) {
        return PatternMatch::No;
    }
    let middle = &key[pre.len()..key.len() - suf.len()];
    let name = pat.placeholder_name.as_str();
    match pat.kind {
        PatternKind::Type => {
            let base = name.split('.').next().unwrap_or(name);
            if type_index
                .map(|idx| idx.contains(base, middle))
                .unwrap_or(false)
            {
                PatternMatch::Confident
            } else {
                PatternMatch::No
            }
        }
        PatternKind::Enum => match ruleset.enum_by_name().get(name) {
            Some(&idx) if !ruleset.enums[idx].values.is_empty() => {
                if ruleset.enum_values_contains_ci(idx, middle)
                    || ruleset.enum_has_at_constant(idx)
                    || enum_is_authoritative(&ruleset.enums[idx])
                {
                    PatternMatch::Confident
                } else {
                    PatternMatch::No
                }
            }
            _ => PatternMatch::PermissiveOnly, // enum absent/empty (game-derived)
        },
        PatternKind::Value => match ruleset.value_set_lookup(name, middle) {
            Some(is_member) => {
                if is_member {
                    PatternMatch::Confident
                } else {
                    PatternMatch::No
                }
            }
            None => PatternMatch::PermissiveOnly, // value set not collected
        },
    }
}

fn strip_affix_ci<'a>(key: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    if key.len() <= prefix.len() + suffix.len() {
        return None;
    }
    if !starts_with_ci(key, prefix) || !ends_with_ci(key, suffix) {
        return None;
    }
    Some(&key[prefix.len()..key.len() - suffix.len()])
}

fn type_instance_known_or_lenient(
    type_name: &str,
    lookup: &str,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    let Some(idx) = type_index else {
        return true;
    };
    // Same gate as CW500 so missing vanilla does not flood false positives.
    if !idx.complete || idx.instances(type_name).is_empty() {
        return true;
    }
    idx.contains(type_name, lookup)
}

fn type_field_matches_key(
    type_type: &TypeType,
    key: &str,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    if key.starts_with('@') || key.starts_with('[') || key.contains('$') {
        return true;
    }
    let (type_name, lookup) = match type_type {
        TypeType::Simple(name) => (name.as_str(), key),
        TypeType::Complex {
            prefix,
            name,
            suffix,
        } => match strip_affix_ci(key, prefix, suffix) {
            Some(middle) => (name.as_str(), middle),
            None => return false,
        },
    };
    type_instance_known_or_lenient(type_name, lookup, type_index)
}

pub(crate) fn field_matches_key(
    field: &NewField,
    key: &str,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    match field {
        NewField::SpecificField(s) => s.eq_ignore_ascii_case(key),
        NewField::AliasField(category) => {
            if ruleset
                .alias_exact()
                .get(category.as_str())
                .is_some_and(|m| m.contains_key(key))
            {
                return true;
            }
            if key.bytes().any(|b| b.is_ascii_uppercase()) {
                let lower = key.to_ascii_lowercase();
                if ruleset
                    .alias_exact()
                    .get(category.as_str())
                    .is_some_and(|m| m.contains_key(lower.as_str()))
                {
                    return true;
                }
            }
            match ruleset.alias_categories().get(category.as_str()) {
                None => true,
                Some(cat) => {
                    for pat in &cat.parsed_patterns {
                        if parsed_pattern_matches(pat, key, ruleset, type_index, true) {
                            return true;
                        }
                    }
                    cat.scope_field_idx.is_some() && is_scope_key(key, ruleset, type_index)
                }
            }
        }
        NewField::SingleAliasField(alias_name) => alias_name == key,
        NewField::IgnoreField(inner) => field_matches_key(inner, key, ruleset, type_index),
        NewField::IgnoreMarkerField => true,
        NewField::ScalarField => true,
        NewField::ValueField(ValueType::Enum(enum_name)) => {
            match ruleset.enum_by_name().get(enum_name.as_str()) {
                Some(&idx) => {
                    let def = &ruleset.enums[idx];
                    if def.values.is_empty() {
                        return true;
                    }
                    if ruleset.enum_values_contains_ci(idx, key) {
                        return true;
                    }
                    if ruleset.enum_has_at_constant(idx) {
                        return true;
                    }
                    enum_is_authoritative(def)
                }
                None => true,
            }
        }
        NewField::ValueField(ValueType::Int { .. }) => key.parse::<i64>().is_ok(),
        NewField::ValueField(ValueType::Float { .. } | ValueType::Percent) => {
            key.parse::<f64>().is_ok()
        }
        NewField::ValueField(ValueType::Date) => is_date_shape(key),
        NewField::ValueField(ValueType::DateTime) => is_datetime_shape(key),
        NewField::TypeField(tt) => type_field_matches_key(tt, key, type_index),
        NewField::ScopeField(_)
        | NewField::VariableField { .. }
        | NewField::VariableGetField(_)
        | NewField::VariableSetField(_)
        | NewField::ValueScopeField { .. }
        | NewField::ValueScopeMarkerField { .. }
        | NewField::LocalisationField { .. }
        | NewField::FilepathField { .. }
        | NewField::IconField(_)
        | NewField::AliasValueKeysField(_) => true,
        NewField::ValueField(
            ValueType::Bool
            | ValueType::Ck2Dna
            | ValueType::Ck2DnaProperty
            | ValueType::IrFamilyName
            | ValueType::StlNameFormat(_)
            | ValueType::MathExpr,
        ) => false,
        NewField::MarkerField(_) => false,
    }
}

pub(super) fn get_rule_key(rule_type: &RuleType) -> Option<&str> {
    match rule_type {
        RuleType::LeafRule { left, .. } | RuleType::NodeRule { left, .. } => field_to_key(left),
        _ => None,
    }
}

fn field_to_key(field: &NewField) -> Option<&str> {
    match field {
        NewField::SpecificField(s) => Some(s.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_index::{SourceLocation, TypeIndex, TypeInstance};
    use std::collections::HashMap;

    fn resource_index(complete: bool) -> TypeIndex {
        let mut idx = TypeIndex::new();
        let mut map = HashMap::new();
        map.insert(
            "resource".to_string(),
            vec![TypeInstance {
                name: "steel".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        idx.merge("file://resources.txt", map);
        idx.complete = complete;
        idx
    }

    fn resource_field() -> NewField {
        NewField::TypeField(TypeType::Simple("resource".to_string()))
    }

    fn local_resources_field() -> NewField {
        NewField::TypeField(TypeType::Complex {
            prefix: "local_resources_".to_string(),
            name: "resource".to_string(),
            suffix: String::new(),
        })
    }

    #[test]
    fn value_typed_and_marker_fields_never_match_a_key() {
        let ruleset = RuleSet::default();
        assert!(field_matches_key(
            &NewField::SpecificField("focus".to_string()),
            "Focus",
            &ruleset,
            None
        ));
        let cases = [
            (NewField::ValueField(ValueType::Bool), "yes"),
            (
                NewField::ValueField(ValueType::Ck2Dna),
                "0123456789abcdef0123456789abcdef",
            ),
            (NewField::ValueField(ValueType::Ck2DnaProperty), "01234567"),
            (NewField::ValueField(ValueType::IrFamilyName), "some_family"),
            (
                NewField::ValueField(ValueType::StlNameFormat("format".to_string())),
                "some_name",
            ),
            (NewField::ValueField(ValueType::MathExpr), "value"),
            (NewField::MarkerField(Marker::ColourField), "color"),
        ];
        for (field, key) in &cases {
            assert!(
                !field_matches_key(field, key, &ruleset, None),
                "{field:?} must not match key {key:?}"
            );
        }
    }

    #[test]
    fn type_field_keys_match_known_instances_when_index_is_complete() {
        let ruleset = RuleSet::default();
        let idx = resource_index(true);
        let field = resource_field();
        assert!(field_matches_key(&field, "steel", &ruleset, Some(&idx)));
        assert!(field_matches_key(&field, "Steel", &ruleset, Some(&idx)));
        assert!(!field_matches_key(
            &field,
            "unobtainium",
            &ruleset,
            Some(&idx)
        ));
    }

    #[test]
    fn type_field_keys_are_lenient_without_a_complete_index() {
        let ruleset = RuleSet::default();
        let field = resource_field();
        assert!(field_matches_key(&field, "unobtainium", &ruleset, None));
        let idx = resource_index(false);
        assert!(field_matches_key(
            &field,
            "unobtainium",
            &ruleset,
            Some(&idx)
        ));
    }

    #[test]
    fn complex_type_field_keys_require_the_affix() {
        let ruleset = RuleSet::default();
        let field = local_resources_field();
        assert!(!field_matches_key(&field, "foo", &ruleset, None));
        assert!(field_matches_key(
            &field,
            "local_resources_steel",
            &ruleset,
            None
        ));
        let idx = resource_index(true);
        assert!(field_matches_key(
            &field,
            "local_resources_steel",
            &ruleset,
            Some(&idx)
        ));
        assert!(!field_matches_key(
            &field,
            "local_resources_unobtainium",
            &ruleset,
            Some(&idx)
        ));
    }

    #[test]
    fn type_field_keys_are_lenient_when_the_type_has_no_instances() {
        let ruleset = RuleSet::default();
        let mut idx = TypeIndex::new();
        idx.complete = true;
        assert!(field_matches_key(
            &resource_field(),
            "unobtainium",
            &ruleset,
            Some(&idx)
        ));
    }

    #[test]
    fn complex_type_field_keys_reject_an_empty_middle() {
        let ruleset = RuleSet::default();
        assert!(!field_matches_key(
            &local_resources_field(),
            "local_resources_",
            &ruleset,
            None
        ));
    }

    #[test]
    fn complex_type_field_keys_honor_suffix_and_ascii_case() {
        let ruleset = RuleSet::default();
        let field = NewField::TypeField(TypeType::Complex {
            prefix: "GFX_".to_string(),
            name: "resource".to_string(),
            suffix: "_icon".to_string(),
        });
        let idx = resource_index(true);
        assert!(field_matches_key(
            &field,
            "GFX_steel_icon",
            &ruleset,
            Some(&idx)
        ));
        assert!(field_matches_key(
            &field,
            "gfx_Steel_ICON",
            &ruleset,
            Some(&idx)
        ));
        assert!(!field_matches_key(
            &field,
            "GFX_unobtainium_icon",
            &ruleset,
            Some(&idx)
        ));
        assert!(!field_matches_key(
            &field,
            "GFX_steel",
            &ruleset,
            Some(&idx)
        ));
    }

    #[test]
    fn type_field_keys_accept_scripted_and_interpolated_names() {
        let ruleset = RuleSet::default();
        let idx = resource_index(true);
        let field = resource_field();
        assert!(field_matches_key(
            &field,
            "@my_resource",
            &ruleset,
            Some(&idx)
        ));
        assert!(field_matches_key(&field, "$RES$", &ruleset, Some(&idx)));
        assert!(field_matches_key(
            &field,
            "[GetResource]",
            &ruleset,
            Some(&idx)
        ));
    }
}
