use cwtools_rules::rules_types::{NewField, RuleSet, RuleType, ValueType};

use super::builders::enum_values_for;

/// Build an LSP snippet body for a NodeRule, pre-populating required child fields
/// (those with cardinality min >= 1 and a SpecificField left-side).
///
/// Mirrors F# createSnippetForClause:346-390. Tab-stop numbering starts at 1.
pub(crate) fn generate_node_snippet(
    key: &str,
    child_rules: &[(RuleType, cwtools_rules::rules_types::Options)],
    ruleset: &RuleSet,
) -> String {
    // Collect required SpecificField leaves/nodes (min >= 1).
    let mut required_parts: Vec<String> = Vec::new();
    let mut tab_stop = 1u32;

    // Use a seen-set so duplicate keys (e.g. from subtype rules) don't repeat.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (rule_type, opts) in child_rules {
        if opts.min < 1 {
            continue;
        }
        match rule_type {
            RuleType::LeafRule {
                left: NewField::SpecificField(k),
                right,
            } => {
                if seen.contains(k) {
                    continue;
                }
                seen.insert(k.clone());
                let placeholder = leaf_right_placeholder(right, tab_stop, ruleset);
                required_parts.push(format!("\t{} = {}", k, placeholder));
                tab_stop += 1;
            }
            RuleType::NodeRule {
                left: NewField::SpecificField(k),
                ..
            } => {
                if seen.contains(k) {
                    continue;
                }
                seen.insert(k.clone());
                // Interior tab stop, not a `${n:{ }}` default: a literal `}` in a
                // placeholder default closes it early, so `${1:{ }}` mis-parses.
                required_parts.push(format!("\t{} = {{ ${} }}", k, tab_stop));
                tab_stop += 1;
            }
            _ => {}
        }
    }

    if required_parts.is_empty() {
        // No required fields — just a block with cursor inside.
        format!("{} = {{\n\t$0\n}}", key)
    } else {
        let body = required_parts.join("\n");
        format!("{} = {{\n{}\n}}", key, body)
    }
}

/// Build the snippet body for an alias (effect/trigger) completion item from the
/// alias's rule shape. A block alias (`if`, `random`, `every_state`) expands to
/// `key = { …required fields… }` via [`generate_node_snippet`] (so e.g. `if`
/// pre-fills its required `limit = { }`); a value alias (`add_political_power`,
/// `set_country_flag`) expands to `key = <placeholder>` so the cursor lands after
/// the `=`, ready for the value. Returns `None` for shapes that have no snippet.
pub(super) fn alias_completion_snippet(
    key: &str,
    rule: &RuleType,
    ruleset: &RuleSet,
) -> Option<String> {
    match rule {
        RuleType::NodeRule { rules, .. } => Some(generate_node_snippet(key, rules, ruleset)),
        // Tab stop 1 (not 0) with a trailing `$0`: VS Code does not support a
        // choice on the final `$0` tab stop and inserts `${0|a,b|}` literally.
        RuleType::LeafRule { right, .. } => Some(format!(
            "{} = {}$0",
            key,
            leaf_right_placeholder(right, 1, ruleset)
        )),
        _ => None,
    }
}

/// Produce a snippet placeholder string for the right-hand side of a leaf rule.
pub(crate) fn leaf_right_placeholder(right: &NewField, tab_stop: u32, ruleset: &RuleSet) -> String {
    match right {
        NewField::ValueField(ValueType::Bool) => {
            format!("${{{}|yes,no|}}", tab_stop)
        }
        NewField::ValueField(ValueType::Enum(e)) => {
            let vals = enum_values_for(ruleset, e);
            if !vals.is_empty() && vals.len() <= 20 {
                format!("${{{}|{}|}}", tab_stop, choice_list(vals))
            } else {
                format!("${{{}}}", tab_stop)
            }
        }
        // A concrete literal value (e.g. `alias[effect:<se>] = yes`): insert it
        // directly so the snippet reads `my_se = yes` rather than `my_se = ${0}`.
        // Escaped in case the config literal carries a `$` or `}`.
        NewField::SpecificField(s) if !s.is_empty() => escape_snippet_text(s),
        _ => format!("${{{}}}", tab_stop),
    }
}

/// Escape a raw literal for snippet *text* context: backslash first, then `$` and
/// `}`, the characters VS Code's snippet parser treats as active outside a choice.
/// Without this a config value containing `}` or `$…` truncates the snippet or
/// starts a spurious tab stop, and VS Code inserts the whole thing literally.
pub(super) fn escape_snippet_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '$' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Build the comma-joined body of a `${n|...|}` choice from enum values: quote
/// each value that needs it (whitespace / special chars), then escape the choice
/// delimiters. Quoting first so the quoted string is the literal text to insert,
/// then the delimiter escape keeps an embedded comma from splitting the choice.
pub(super) fn choice_list(vals: &[String]) -> String {
    vals.iter()
        .map(|v| escape_choice(&quote_if_needed(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Backslash-escape the characters that are special inside a `${n|...|}` snippet
/// choice (`\`, `,`, `|`, `}`). Backslash is handled by the loop itself, so it is
/// escaped before the delimiters it might precede.
pub(super) fn escape_choice(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | ',' | '|' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Wrap a value in double quotes when the raw token would not parse as a single
/// bare value — i.e. it is empty or contains whitespace or a character outside
/// the identifier-ish set. Conservative on purpose: bare-identifier enum values
/// (the common case, and other games' enums) must stay unquoted.
pub(super) fn quote_if_needed(v: &str) -> String {
    let needs_quote = v.is_empty()
        || v.chars()
            .any(|c| !(c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')));
    if needs_quote {
        format!("\"{}\"", v)
    } else {
        v.to_string()
    }
}
