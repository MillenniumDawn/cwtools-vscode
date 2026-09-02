use cwtools_rules::rules_types::{NewField, RuleSet, RuleType, ValueType};

use super::builders::enum_values_for;

pub(crate) fn generate_node_snippet(
    key: &str,
    child_rules: &[(RuleType, cwtools_rules::rules_types::Options)],
    ruleset: &RuleSet,
) -> String {
    let mut required_parts: Vec<String> = Vec::new();
    let mut tab_stop = 1u32;

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
                required_parts.push(format!("\t{} = {{ ${} }}", k, tab_stop));
                tab_stop += 1;
            }
            _ => {}
        }
    }

    if required_parts.is_empty() {
        format!("{} = {{\n\t$0\n}}", key)
    } else {
        let body = required_parts.join("\n");
        format!("{} = {{\n{}\n}}", key, body)
    }
}

pub(super) fn alias_completion_snippet(
    key: &str,
    rule: &RuleType,
    ruleset: &RuleSet,
) -> Option<String> {
    match rule {
        RuleType::NodeRule { rules, .. } => Some(generate_node_snippet(key, rules, ruleset)),
        RuleType::LeafRule { right, .. } => Some(format!(
            "{} = {}$0",
            key,
            leaf_right_placeholder(right, 1, ruleset)
        )),
        _ => None,
    }
}

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
        NewField::SpecificField(s) if !s.is_empty() => escape_snippet_text(s),
        _ => format!("${{{}}}", tab_stop),
    }
}

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

pub(super) fn choice_list(vals: &[String]) -> String {
    vals.iter()
        .map(|v| escape_choice(&quote_if_needed(v)))
        .collect::<Vec<_>>()
        .join(",")
}

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
