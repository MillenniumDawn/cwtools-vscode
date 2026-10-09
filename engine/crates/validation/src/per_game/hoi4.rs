use super::common::{as_block, child_key_eq, walk_blocks};
use crate::{ValidationError, error_codes};
use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_parser::fix::SuggestedFix;
use cwtools_string_table::string_table::{StringId, StringTable};

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

/// Blocks whose sole `always = <default>` body is redundant, keyed by the block's
/// lowered id.
pub(super) fn redundant_block_defaults(table: &StringTable) -> [(StringId, bool); 1] {
    [(table.intern("allowed_civil_war").lower, false)]
}

pub fn validate_hoi4(
    ast: &ParsedFile,
    _ruleset: &cwtools_rules::rules_types::RuleSet,
    table: &StringTable,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    validate_equipment_icons(ast, table, file_path, errors);
    let defaults = redundant_block_defaults(table);

    walk_blocks(&ast.root_children, ast, &mut |block| {
        let Some(&(_, default)) = defaults.iter().find(|(id, _)| *id == block.key_lower) else {
            return;
        };
        if sole_always_value(block.children, ast, table) == Some(default) {
            let key = block.key_string_lower(table);
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

fn validate_equipment_icons(
    ast: &ParsedFile,
    table: &StringTable,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    let variant = table.intern("create_equipment_variant").lower;
    let pool = table.intern("pool").lower;
    let designer_file = file_path.to_ascii_lowercase().ends_with(".txt")
        && file_path
            .replace('\\', "/")
            .split('/')
            .collect::<Vec<_>>()
            .windows(5)
            .any(|parts| {
                parts[..4]
                    .iter()
                    .zip(["gfx", "interface", "equipmentdesigner", "graphic_db"])
                    .all(|(part, expected)| part.eq_ignore_ascii_case(expected))
            });
    walk_blocks(&ast.root_children, ast, &mut |block| {
        if block.key_lower == variant {
            for child in block.children {
                if child_key_eq(child, ast, table, "icon") {
                    let Child::Leaf(idx) = child else { continue };
                    let leaf = &ast.arena.leaves[*idx as usize];
                    warn_raw_icon(&leaf.value, leaf.pos, table, file_path, errors);
                }
            }
        }
        if designer_file && block.key_lower == pool {
            for child in block.children {
                let Some(icons) = as_block(child, ast) else {
                    continue;
                };
                if !table
                    .with_string(icons.key_lower, |key| key == "icons")
                    .unwrap_or(false)
                {
                    continue;
                }
                for child in icons.children {
                    if let Child::LeafValue(idx) = child {
                        let entry = &ast.arena.leaf_values[*idx as usize];
                        warn_raw_icon(&entry.value, entry.pos, table, file_path, errors);
                    }
                }
            }
        }
    });
}

fn warn_raw_icon(
    value: &Value,
    range: cwtools_parser::ast::SourceRange,
    table: &StringTable,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    let (Value::String(tokens) | Value::QString(tokens)) = value else {
        return;
    };
    table.with_string(tokens.normal, |raw| {
        let value = cwtools_parser::unquote(raw).trim();
        if value.is_empty() || value.starts_with('@') || value.contains(['$', '[', '<']) {
            return;
        }
        let lower = value.to_ascii_lowercase();
        if lower.starts_with("gfx/")
            || [".dds", ".tga", ".png"]
                .iter()
                .any(|ext| lower.ends_with(ext))
        {
            errors.push(
                ValidationError::from_code(
                    &error_codes::CW284_RAW_EQUIPMENT_ICON,
                    file_path,
                    range.start.line,
                    range.start.col,
                    &[value],
                )
                .with_end(range.end),
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
        let errors = run("my_idea = {\n allowed_civil_war = { always = yes }\n}\n");
        assert!(errors.is_empty());
    }

    #[test]
    fn ignores_real_trigger_body() {
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

#[cfg(test)]
mod raw_icon_tests {
    use super::*;
    use cwtools_parser::parser::parse_string;

    fn run(source: &str, path: &str) -> Vec<ValidationError> {
        let table = StringTable::new();
        let ast = parse_string(source, &table);
        let mut errors = Vec::new();
        validate_hoi4(
            &ast,
            &cwtools_rules::rules_types::RuleSet::new(),
            &table,
            &path.into(),
            &mut errors,
        );
        errors
            .into_iter()
            .filter(|e| e.code == Some("CW284"))
            .collect()
    }

    #[test]
    fn raw_variant_icons_warn_from_every_effect_caller() {
        for caller in [
            "history/countries/USA.txt",
            "common/national_focus/usa.txt",
            "events/usa.txt",
            "common/decisions/usa.txt",
            "common/scripted_effects/usa.txt",
        ] {
            for value in [
                "gfx/interface/technologies/USA/AIR/A-4E.dds",
                "gfx/texture",
                "plane.dds",
                "plane.tga",
                "plane.png",
                "PLANE.DDS",
            ] {
                let errors = run(
                    &format!(
                        "wrapper = {{ create_equipment_variant = {{\n icon = \"{value}\"\n }} }}"
                    ),
                    caller,
                );
                assert_eq!(errors.len(), 1, "{caller}: {value}");
                assert_eq!(errors[0].severity, crate::ErrorSeverity::Warning);
                assert_eq!(errors[0].line, 2);
                assert!(errors[0].message.contains(value));
                assert!(errors[0].message.contains("macOS and Linux"));
            }
        }
    }

    #[test]
    fn raw_designer_pool_icons_warn_on_the_entry_line() {
        let source = "USA = { plane = { pool = { icons = {\n \"gfx/plane.dds\"\n \"GFX_PLANE\"\n } models = { \"gfx/model.dds\" } } } }";
        let errors = run(
            source,
            "gfx/interface/equipmentdesigner/graphic_db/planes.txt",
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 2);
        assert_eq!(errors[0].severity, crate::ErrorSeverity::Warning);
        assert_eq!(
            run(
                source,
                "gfx\\interface\\equipmentdesigner\\graphic_db\\planes.txt"
            )
            .len(),
            1
        );
    }

    #[test]
    fn registered_or_dynamic_icons_are_exempt() {
        for value in [
            "GFX_USA_AIR_A_4E",
            "$ICON$",
            "[GetIcon].dds",
            "<icon>.dds",
            "gfx/$ICON$.dds",
            "@icon",
            "unregistered_sprite_name",
        ] {
            assert!(
                run(
                    &format!("create_equipment_variant = {{ icon = \"{value}\" }}"),
                    "events/usa.txt"
                )
                .is_empty(),
                "{value}"
            );
            assert!(
                run(
                    &format!("USA = {{ plane = {{ pool = {{ icons = {{ \"{value}\" }} }} }} }}"),
                    "gfx/interface/equipmentdesigner/graphic_db/planes.txt"
                )
                .is_empty(),
                "{value}"
            );
        }
    }

    #[test]
    fn raw_paths_outside_the_two_icon_contexts_are_exempt() {
        for (source, path) in [
            (
                "country_event = { icon = \"gfx/icon.dds\" }",
                "events/usa.txt",
            ),
            (
                "create_equipment_variant = { modules = { icon = \"gfx/icon.dds\" } }",
                "events/usa.txt",
            ),
            (
                "USA = { plane = { pool = { icons = { \"gfx/icon.dds\" } } } }",
                "other/graphic_db/planes.txt",
            ),
            (
                "USA = { plane = { pool = { icons = { \"gfx/icon.dds\" } } } }",
                "gfx/interface/equipmentdesigner/graphic_db_backup/planes.txt",
            ),
            (
                "USA = { plane = { pool = { icons = { \"gfx/icon.dds\" } } } }",
                "gfx/interface/equipmentdesigner/graphic_db/planes.gfx",
            ),
            (
                "icons = { \"gfx/icon.dds\" }",
                "gfx/interface/equipmentdesigner/graphic_db/planes.txt",
            ),
        ] {
            assert!(run(source, path).is_empty(), "{source}: {path}");
        }
    }
}
