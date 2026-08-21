//! HOI4-specific cleanup hints.
//!
//! Currently: CW280, flagging a field whose body is exactly `{ always = <bool> }`
//! where that bool matches the field's game default, so the whole field is a
//! no-op and can be removed (e.g. `allowed_civil_war = { always = no }`).
//!
//! The table is deliberately explicit, not "any `{ always = no }` is redundant":
//! `always = no` is a recommended guard in other contexts (an event's
//! `trigger = { always = no }`, see CW107), so only fields whose default is
//! known are listed.

use super::common::walk_blocks;
use crate::{ValidationError, error_codes};
use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_parser::fix::SuggestedFix;
use cwtools_string_table::string_table::StringTable;

/// If the block's only non-comment child is `always = <bool>`, return that bool;
/// otherwise `None` (anything else means the block does real work).
fn sole_always_value(children: &[Child], ast: &ParsedFile, table: &StringTable) -> Option<bool> {
    let mut found: Option<bool> = None;
    for child in children {
        match child {
            Child::Comment(_) => {}
            Child::Leaf(idx) => {
                let l = &ast.arena.leaves[*idx as usize];
                if !table
                    .with_string(l.key.lower, |k| k == "always")
                    .unwrap_or(false)
                {
                    return None;
                }
                match l.value {
                    Value::Bool(b) if found.is_none() => found = Some(b),
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    found
}

/// Run the HOI4-specific cleanup hints over a whole file.
pub fn validate_hoi4(
    ast: &ParsedFile,
    _ruleset: &cwtools_rules::rules_types::RuleSet,
    table: &StringTable,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    // Fields whose body `{ always = <bool> }` matches the game default (so the
    // field is a no-op) -> the default the `always` value must equal. Listed
    // explicitly: an idea/spirit's allowed_civil_war defaults to "no". Interned
    // once, so the walk compares token ids instead of pulling every block's key
    // out of the string table.
    let defaults = [(table.intern("allowed_civil_war").lower, false)];

    walk_blocks(&ast.root_children, ast, &mut |block| {
        let Some(&(_, default)) = defaults.iter().find(|(id, _)| *id == block.key_lower) else {
            return;
        };
        if sole_always_value(block.children, ast, table) == Some(default) {
            let key = block.key_string_lower(table);
            // Fix: delete the whole redundant `key = { always = <default> }`
            // field. `block.range` spans the leaf; its end lands at the start of
            // the next token, so the line (and its newline) go with it.
            let fix = SuggestedFix::delete(
                cwtools_i18n::format(cwtools_i18n::Key::ActionRemoveRedundant, &[&key]),
                block.range,
            );
            errors.push(
                ValidationError::from_code(
                    &error_codes::CW280_REDUNDANT_DEFAULT_FIELD,
                    file_path,
                    block.range.start.line,
                    block.range.start.col,
                    &[&key],
                )
                .with_fix(fix)
                .with_end(block.range.end),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::parser::parse_string;

    fn run(src: &str) -> Vec<ValidationError> {
        let table = StringTable::new();
        let ast = parse_string(src, &table);
        let ruleset = cwtools_rules::rules_types::RuleSet::new();
        let mut errors = Vec::new();
        validate_hoi4(&ast, &ruleset, &table, &"test.txt".into(), &mut errors);
        errors
    }

    #[test]
    fn flags_redundant_allowed_civil_war() {
        let errors = run("my_idea = {\n allowed_civil_war = { always = no }\n}\n");
        assert_eq!(errors.len(), 1, "expected one CW280");
        assert_eq!(errors[0].code, Some("CW280"));
    }

    #[test]
    fn ignores_non_default_value() {
        // always = yes is not the default for allowed_civil_war, so not redundant.
        let errors = run("my_idea = {\n allowed_civil_war = { always = yes }\n}\n");
        assert!(errors.is_empty());
    }

    #[test]
    fn ignores_real_trigger_body() {
        // A real trigger (not a bare always) does work — leave it alone.
        let errors = run("my_idea = {\n allowed_civil_war = { has_war = no }\n}\n");
        assert!(errors.is_empty());
    }

    #[test]
    fn flags_mixed_case_key() {
        let errors = run("my_idea = {\n Allowed_Civil_War = { Always = no }\n}\n");
        assert_eq!(errors.len(), 1, "expected one CW280");
        assert_eq!(errors[0].code, Some("CW280"));
    }

    #[test]
    fn ignores_unlisted_fields() {
        // always = no on an unlisted field is not flagged (e.g. event guard).
        let errors = run("country_event = {\n trigger = { always = no }\n}\n");
        assert!(errors.is_empty());
    }

    #[test]
    fn cw280_fix_deletes_redundant_field() {
        use cwtools_parser::fix::apply_edits;
        let src = "my_idea = { allowed_civil_war = { always = no } }\n";
        let table = StringTable::new();
        let ast = parse_string(src, &table);
        let ruleset = cwtools_rules::rules_types::RuleSet::new();
        let mut errors = Vec::new();
        validate_hoi4(&ast, &ruleset, &table, &"test.txt".into(), &mut errors);

        let err = errors
            .iter()
            .find(|e| e.code == Some("CW280"))
            .expect("CW280 emitted");
        let fix = err.fix.as_ref().expect("CW280 carries a fix");
        let fixed = apply_edits(src, &fix.edits);
        assert_eq!(fixed, "my_idea = { }\n");

        let ast2 = parse_string(&fixed, &table);
        let mut errors2 = Vec::new();
        validate_hoi4(&ast2, &ruleset, &table, &"test.txt".into(), &mut errors2);
        assert!(
            !errors2.iter().any(|e| e.code == Some("CW280")),
            "CW280 must be gone after applying the fix"
        );
    }
}
