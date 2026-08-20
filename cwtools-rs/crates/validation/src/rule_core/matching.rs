//! Rule-key matching: which rules a key selects, alias-pattern classification,
//! and scope-key recognition.

use cwtools_rules::rules_types::*;
use smallvec::SmallVec;

use crate::common::*;

/// Collect the rules whose key matches `key`. If any rule keys on a literal
/// `SpecificField` equal to `key`, ONLY those are returned — a specific rule
/// (e.g. `milestones = { ... }`) wins over catch-all rules (`enum[x] = ...`,
/// `<type> = ...`, `alias_name[...]`) that match the same key permissively.
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
    // A literal `SpecificField` rule wins over catch-all matches; scan once to see
    // if any exists, then collect only the relevant subset (one heap alloc, not two).
    let has_specific = rules
        .iter()
        .any(|(rt, _)| is_specific(rt) && matcher(rt, key, ruleset, type_index));
    // Cheap specificity test first: both operands are pure, so short-circuiting
    // on it skips the matcher for every rule a specific one already outranks.
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
        // Cross-kind fallback: a NodeRule can also match a leaf key (e.g. alias blocks)
        RuleType::LeafRule { left, .. } | RuleType::NodeRule { left, .. } => {
            field_matches_key(left, key, ruleset, type_index)
        }
        _ => false,
    }
}

/// Whether a key is a scope-switching command — valid wherever an alias category
/// declares `alias[cat:scope_field]` (e.g. `ROOT = { ... }`, `SOV = { ... }`,
/// `FROM.owner = { ... }`, `event_target:x = { ... }`). Deep scope resolution is
/// the scope engine's job; here we just recognise the shape so the nested block
/// still gets validated instead of the whole key reading as unexpected.
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
    // Scope chains (ROOT.owner) and prefixed refs (event_target:x, var:x).
    if key.contains('.') || key.contains(':') {
        return true;
    }
    // A bare numeric id opens a state/province scope: `642 = { ... }`.
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // Country tag: 2-4 chars, all uppercase letters/digits, at least one letter.
    let len = key.len();
    (2..=4).contains(&len)
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && key.chars().any(|c| c.is_ascii_uppercase())
}

/// Whether `key` can open a scope in an effect/trigger block: a scope command
/// (ROOT/FROM/tag/id/chain), an instance of any type, or a member of a value-set
/// that a `from_data` scope link draws from. HOI4 from-data scope links let an
/// instance (character, state, ideology, ...) — or a dynamically-defined token
/// (`generate_character`'s `token_base`) — open its own scope.
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

/// Whether `key` is a member of a value-set that a `from_data` scope link draws
/// from (`links.cwt`: `data_source = value[<set>]`). Such a link makes any of that
/// set's members a valid scope-opening key (e.g. the `character_token` link's
/// `data_source = value[character_token]` lets a `generate_character` token open a
/// `character` scope). Checked last because it scans `link_inputs`; only reached
/// when the cheaper scope-command / link-name / type-instance checks all miss.
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

/// Extract `<set>` from a `data_source = value[<set>]` entry; `None` for any other
/// data-source shape (`<type>`, `enum[..]`, a bare scalar).
fn value_set_name(data_source: &str) -> Option<&str> {
    data_source
        .strip_prefix("value[")
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::trim)
}

/// Case-insensitive membership in `ruleset.scope_links` (a lowercase `String`
/// set), allocating a lowercased key only when `key` actually has uppercase
/// bytes — the common all-lowercase case probes the set directly.
fn scope_links_contains(ruleset: &RuleSet, key: &str) -> bool {
    if key.bytes().any(|b| b.is_ascii_uppercase()) {
        ruleset
            .scope_links
            .contains(&key.to_ascii_lowercase() as &str)
    } else {
        ruleset.scope_links.contains(key)
    }
}

/// Test whether `key` matches the pre-parsed alias pattern.
///
/// The pattern was already split into (prefix, kind, placeholder_name, suffix)
/// at ruleset build time by `ParsedAliasPattern::parse`; this function only
/// does the per-call key-matching work (prefix/suffix strip + membership test).
/// `permissive` controls the fallback when a pattern's backing data is absent or
/// empty (a game-derived `enum[..]`/`value[..]` that wasn't populated, e.g. when
/// vanilla isn't indexed). `true` accepts the key (the key-existence checks want
/// this, to avoid flooding "unknown key" errors); `false` rejects it, so the
/// scope check only trusts a match it can actually verify and never inherits an
/// unrelated alias's `## scope` from a coincidental empty-enum match.
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

/// Tri-state result of matching a key against an alias pattern, distinguishing a
/// verified match (`Confident`) from a match that only holds under the permissive
/// fallback because the pattern's backing enum/value set is absent or empty
/// (`PermissiveOnly`). Lets `alias_overloads_with_confidence` compute the pattern
/// match once and derive both the permissive and confident overload sets.
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
            // `<type.subtype>` → check the base type (subtype is a refinement).
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
        // Paradox script keys (field and command names) are case-insensitive — the
        // game lowercases them — so `Country_event` matches the `country_event`
        // rule. Values (tags, ids, enum members) stay case-sensitive; those are
        // handled by the value-typed arms below.
        NewField::SpecificField(s) => s.eq_ignore_ascii_case(key),
        NewField::AliasField(category) => {
            // Resolved through the precomputed alias index (ruleset.reindex()) so
            // this is O(1)+O(patterns) instead of a linear scan over every alias.
            // The name part can be a literal (`trigger:original_tag`), a `<type>`
            // reference (`trigger:<scripted_trigger>`, `modifier:..<building>..`),
            // or `scope_field` (any scope-switching key).
            if ruleset
                .alias_exact()
                .get(category.as_str())
                .is_some_and(|m| m.contains_key(key))
            {
                return true;
            }
            // Case-insensitive retry: command names like `Country_event` resolve to
            // the lowercase `country_event` alias (config alias names are lowercase).
            // Only allocate the lowercased form when `key` actually has uppercase.
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
                // Category has no aliases at all — be permissive (avoid floods).
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
        NewField::SingleAliasField(alias_name) => {
            // SingleAliasField matches if the key is exactly this alias name.
            alias_name == key
        }
        // `key = ignore_field` wraps the key in IgnoreField — it matches the inner
        // field's key; the value is then accepted unvalidated (see the IgnoreField
        // short-circuit in validate_{leaf,node}_against_rule).
        NewField::IgnoreField(inner) => field_matches_key(inner, key, ruleset, type_index),
        NewField::IgnoreMarkerField => true,
        NewField::ScalarField => true,
        // A rule keyed by `enum[x] = ...`: the key must be a member of enum x.
        // Mirrors `enum_contains` from common.rs: case-insensitive, permissive on
        // absent/empty enums, permissive when any member is an @-constant.
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
        // Numeric-keyed rules: `ordered = { int = { ... } }` uses integer keys.
        NewField::ValueField(ValueType::Int { .. }) => key.parse::<i64>().is_ok(),
        NewField::ValueField(ValueType::Float { .. } | ValueType::Percent) => {
            key.parse::<f64>().is_ok()
        }
        // `date_field = { ... }` (history dated blocks like `2000.1.1 = { ... }`).
        NewField::ValueField(ValueType::Date) => is_date_shape(key),
        NewField::ValueField(ValueType::DateTime) => is_datetime_shape(key),
        // Keys that reference a type instance (`<focus> = ...`), a scope, a
        // variable, a filepath/loc/icon, etc. CWT allows these on the left-hand
        // side. Existence is verified by other passes (type index, scope engine);
        // here we accept the key so the rule body still gets validated.
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
        // These value types never appear on the left-hand side of a rule.
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

/// Decision-table tests for the false arms #287 made explicit. The corpus guard
/// only runs the HOI4 rules, so the CK2/IR/Stellaris value types below are never
/// exercised there; this pins that they stay non-matching as keys.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_typed_and_marker_fields_never_match_a_key() {
        let ruleset = RuleSet::default();
        // Fixture guard: the same ruleset does match where an arm says yes, so
        // the rejections below cannot pass by everything being false.
        assert!(field_matches_key(
            &NewField::SpecificField("focus".to_string()),
            "Focus",
            &ruleset,
            None
        ));
        // Each key is shaped to satisfy the field's own value check, so a pass
        // proves the arm is a hard false rather than a lucky key mismatch.
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
