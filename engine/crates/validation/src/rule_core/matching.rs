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
        NewField::TypeField(_)
        | NewField::ScopeField(_)
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
}
