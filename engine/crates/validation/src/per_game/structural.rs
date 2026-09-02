use super::common::{as_block, child_is_always_no, under_dir_segment};
use crate::{ValidationError, error_codes};
use cwtools_game::constants::Game;
use cwtools_parser::ast::{Child, ParsedFile, SourceRange, Value};
use cwtools_parser::fix::{SuggestedFix, key_token_range};
use cwtools_string_table::string_table::{StringId, StringTable};

#[derive(Clone, Copy, PartialEq)]
enum BoolState {
    And,
    Or,
    Neutral,
}

struct Keywords {
    not: StringId,
    if_: StringId,
    else_if: StringId,
    else_: StringId,
    and: StringId,
    or: StringId,
    nor: StringId,
    limit: StringId,
    modifier: StringId,
    count_triggers: StringId,
    value: StringId,
    mean_time_to_happen: StringId,
    is_triggered_only: StringId,
    fire_only_once: StringId,
    base: StringId,
    trigger: StringId,
}

impl Keywords {
    fn new(table: &StringTable) -> Self {
        Self {
            not: table.intern("not").lower,
            if_: table.intern("if").lower,
            else_if: table.intern("else_if").lower,
            else_: table.intern("else").lower,
            and: table.intern("and").lower,
            or: table.intern("or").lower,
            nor: table.intern("nor").lower,
            limit: table.intern("limit").lower,
            modifier: table.intern("modifier").lower,
            count_triggers: table.intern("count_triggers").lower,
            value: table.intern("value").lower,
            mean_time_to_happen: table.intern("mean_time_to_happen").lower,
            is_triggered_only: table.intern("is_triggered_only").lower,
            fire_only_once: table.intern("fire_only_once").lower,
            base: table.intern("base").lower,
            trigger: table.intern("trigger").lower,
        }
    }
}

fn is_bool_operator(key: StringId, kw: &Keywords) -> bool {
    key == kw.not || key == kw.and || key == kw.or || key == kw.nor
}

fn is_math_expression(children: &[Child], ast: &ParsedFile, kw: &Keywords) -> bool {
    children.iter().any(|c| match c {
        Child::Leaf(idx) => ast.arena.leaves[*idx as usize].key.lower == kw.value,
        _ => false,
    })
}

fn non_comment_count(children: &[Child]) -> usize {
    children
        .iter()
        .filter(|c| !matches!(c, Child::Comment(_)))
        .count()
}

fn is_empty_if(children: &[Child], ast: &ParsedFile, kw: &Keywords) -> bool {
    for child in children {
        match child {
            Child::Leaf(idx) => {
                let l = &ast.arena.leaves[*idx as usize];
                if !matches!(l.value, Value::Clause(_)) {
                    return false;
                }
                if l.key.lower != kw.limit {
                    return false;
                }
            }
            Child::LeafValue(_) => return false,
            Child::Comment(_) => {}
        }
    }
    true
}

fn chain_follows(children: &[Child], idx: usize, ast: &ParsedFile, kw: &Keywords) -> bool {
    for child in &children[idx + 1..] {
        match child {
            Child::Comment(_) => {}
            Child::Leaf(i) => {
                let key = ast.arena.leaves[*i as usize].key.lower;
                return key == kw.else_if || key == kw.else_;
            }
            Child::LeafValue(_) => return false,
        }
    }
    false
}

fn push(
    errors: &mut Vec<ValidationError>,
    code: &error_codes::ErrorCode,
    msg: String,
    r: SourceRange,
    file: &crate::FilePath,
) {
    errors.push(
        ValidationError::from_code_with(code, code.severity, file, r.start.line, r.start.col, msg)
            .with_end(r.end),
    );
}

fn push_fix(
    errors: &mut Vec<ValidationError>,
    code: &error_codes::ErrorCode,
    msg: String,
    r: SourceRange,
    file: &crate::FilePath,
    fix: SuggestedFix,
) {
    errors.push(
        ValidationError::from_code_with(code, code.severity, file, r.start.line, r.start.col, msg)
            .with_fix(fix)
            .with_end(r.end),
    );
}

fn validate_event_every_tick(
    ast: &ParsedFile,
    table: &StringTable,
    kw: &Keywords,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    if !under_dir_segment(file_path, "events") {
        return;
    }
    for child in &ast.root_children {
        let Some(block) = as_block(child, ast) else {
            continue;
        };
        let Some((is_event, key_len)) = table.with_string(block.key_lower, |k| {
            (k.ends_with("_event") || k == "event", k.chars().count())
        }) else {
            continue;
        };
        if !is_event {
            continue;
        }

        let has_guard = block.children.iter().any(|c| {
            let Child::Leaf(idx) = c else { return false };
            let key = ast.arena.leaves[*idx as usize].key.lower;
            key == kw.mean_time_to_happen
                || key == kw.is_triggered_only
                || key == kw.fire_only_once
                || key == kw.base
                || (key == kw.trigger && child_is_always_no(c, ast, table))
        });

        if !has_guard {
            push(
                errors,
                &error_codes::CW107_EVENT_EVERY_TICK,
                error_codes::CW107_EVENT_EVERY_TICK.message().to_string(),
                key_token_range(block.range.start, key_len),
                file_path,
            );
        }
    }
}

fn validate_if_else_order(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    kw: &Keywords,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    for child in children {
        let Some(block) = as_block(child, ast) else {
            continue;
        };
        let key = block.key_lower;

        if key != kw.limit && key != kw.modifier {
            let mut prev_was_if = key == kw.if_ || key == kw.else_if;
            for c in block.children {
                let Some(inner) = as_block(c, ast) else {
                    continue;
                };
                let k = inner.key_lower;
                if k != kw.if_ && k != kw.else_ && k != kw.else_if {
                    continue;
                }
                if !prev_was_if && k != kw.if_ {
                    let key_len = table.with_string(key, |s| s.chars().count()).unwrap_or(0);
                    let follower = table.with_string(k, |s| s.to_string()).unwrap_or_default();
                    let follower_range =
                        key_token_range(inner.range.start, follower.chars().count());
                    let code = &error_codes::CW238_IF_ELSE_ORDER;
                    let range = key_token_range(block.range.start, key_len);
                    errors.push(
                        ValidationError::from_code(
                            code,
                            file_path,
                            range.start.line,
                            range.start.col,
                            &[],
                        )
                        .with_end(range.end)
                        .with_related(
                            format!("this {follower} has no preceding if"),
                            follower_range,
                        ),
                    );
                    break;
                }
                prev_was_if = k == kw.if_ || k == kw.else_if;
            }
        }

        validate_if_else_order(block.children, ast, table, kw, file_path, errors);
    }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    children: &[Child],
    ast: &ParsedFile,
    kw: &Keywords,
    file_path: &crate::FilePath,
    parent: BoolState,
    cw223_msg: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (idx, child) in children.iter().enumerate() {
        let Some(block) = as_block(child, ast) else {
            continue;
        };
        let key = block.key_lower;

        if is_bool_operator(key, kw) && is_math_expression(block.children, ast, kw) {
            walk(
                block.children,
                ast,
                kw,
                file_path,
                BoolState::Neutral,
                cw223_msg,
                errors,
            );
            continue;
        }

        if key == kw.not && non_comment_count(block.children) > 1 {
            push(
                errors,
                &error_codes::CW223_INCORRECT_NOT_USAGE,
                cw223_msg.to_string(),
                key_token_range(block.range.start, 3),
                file_path,
            );
        }

        if (key == kw.if_ || key == kw.else_if) && is_empty_if(block.children, ast, kw) {
            let msg = error_codes::CW121_EMPTY_IF.message().to_string();
            if chain_follows(children, idx, ast, kw) {
                push(
                    errors,
                    &error_codes::CW121_EMPTY_IF,
                    msg,
                    block.range,
                    file_path,
                );
            } else {
                push_fix(
                    errors,
                    &error_codes::CW121_EMPTY_IF,
                    msg,
                    block.range,
                    file_path,
                    SuggestedFix::delete(
                        cwtools_i18n::t(cwtools_i18n::Key::ActionRemoveEmptyIf),
                        block.range,
                    ),
                );
            }
        }

        if key == kw.limit && non_comment_count(block.children) == 0 {
            push_fix(
                errors,
                &error_codes::CW281_EMPTY_LIMIT,
                error_codes::CW281_EMPTY_LIMIT.message().to_string(),
                block.range,
                file_path,
                SuggestedFix::delete(
                    cwtools_i18n::t(cwtools_i18n::Key::ActionRemoveEmptyLimit),
                    block.range,
                ),
            );
        }

        // NOR/ NOT/ count_triggers all map to Or/Neutral but for distinct reasons
        #[allow(clippy::if_same_then_else)]
        let state = if key == kw.and {
            if parent == BoolState::And {
                push(
                    errors,
                    &error_codes::CW251_UNNECESSARY_BOOLEAN,
                    error_codes::CW251_UNNECESSARY_BOOLEAN.format(&["AND"]),
                    key_token_range(block.range.start, 3),
                    file_path,
                );
            }
            BoolState::And
        } else if key == kw.or {
            if parent == BoolState::Or {
                push(
                    errors,
                    &error_codes::CW251_UNNECESSARY_BOOLEAN,
                    error_codes::CW251_UNNECESSARY_BOOLEAN.format(&["OR"]),
                    key_token_range(block.range.start, 2),
                    file_path,
                );
            }
            BoolState::Or
        } else if key == kw.nor {
            BoolState::Or
        } else if key == kw.not {
            BoolState::Neutral
        } else if key == kw.count_triggers {
            BoolState::Neutral
        } else {
            BoolState::And
        };

        walk(block.children, ast, kw, file_path, state, cw223_msg, errors);
    }
}

pub fn validate_structural(
    ast: &ParsedFile,
    table: &StringTable,
    file_path: &crate::FilePath,
    game: Game,
    errors: &mut Vec<ValidationError>,
) {
    let cw223_msg = match game {
        Game::Hoi4 => error_codes::cw223_hoi4_message(),
        _ => error_codes::CW223_INCORRECT_NOT_USAGE.message(),
    };
    let kw = Keywords::new(table);
    walk(
        &ast.root_children,
        ast,
        &kw,
        file_path,
        BoolState::And,
        cw223_msg,
        errors,
    );

    validate_event_every_tick(ast, table, &kw, file_path, errors);
    validate_if_else_order(&ast.root_children, ast, table, &kw, file_path, errors);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::fix::apply_edits;
    use cwtools_parser::parser::parse_string;

    fn codes(src: &str) -> Vec<&'static str> {
        codes_at("test.txt", src)
    }

    fn codes_at(path: &str, src: &str) -> Vec<&'static str> {
        let table = StringTable::new();
        let ast = parse_string(src, &table);
        let mut errors = Vec::new();
        validate_structural(&ast, &table, &path.into(), Game::Hoi4, &mut errors);
        errors.iter().filter_map(|e| e.code).collect()
    }

    fn assert_fix(code: &str, src: &str, expected: &str) {
        let table = StringTable::new();
        let ast = parse_string(src, &table);
        let mut errors = Vec::new();
        validate_structural(&ast, &table, &"test.txt".into(), Game::Hoi4, &mut errors);

        let err = errors
            .iter()
            .find(|e| e.code == Some(code))
            .unwrap_or_else(|| panic!("{code} emitted for {src:?}, got {errors:?}"));
        let fix = err.fix.as_ref().expect("diagnostic carries a fix");
        let fixed = apply_edits(src, &fix.edits);
        assert_eq!(fixed, expected, "{code} fix output");

        let ast2 = parse_string(&fixed, &table);
        let mut errors2 = Vec::new();
        validate_structural(&ast2, &table, &"test.txt".into(), Game::Hoi4, &mut errors2);
        assert!(
            !errors2.iter().any(|e| e.code == Some(code)),
            "{code} must be gone after applying the fix"
        );
    }

    #[test]
    fn cw223_underlines_only_the_not_key() {
        let src = "x = {\n    NOT = {\n        a = 1\n        b = 2\n    }\n}\n";
        let table = StringTable::new();
        let ast = parse_string(src, &table);
        let mut errors = Vec::new();
        validate_structural(&ast, &table, &"test.txt".into(), Game::Hoi4, &mut errors);

        let err = errors
            .iter()
            .find(|e| e.code == Some("CW223"))
            .expect("CW223 emitted");

        let x_block = as_block(&ast.root_children[0], &ast).expect("x is a block");
        let not_block = x_block
            .children
            .iter()
            .find_map(|c| as_block(c, &ast))
            .expect("NOT block present");
        assert_eq!(err.line, not_block.range.start.line);
        assert_eq!(err.col, not_block.range.start.col);

        let (end_line, end_col) = err.end.expect("CW223 carries an end");
        assert_eq!(
            end_line, not_block.range.start.line,
            "stays on the NOT line"
        );
        assert_eq!(
            end_col,
            not_block.range.start.col + 3,
            "spans `NOT` and nothing else"
        );
        assert!(
            end_line < not_block.range.end.line,
            "must not reach the block body"
        );
    }

    #[test]
    fn cw251_underlines_only_the_operator_key() {
        let and_src = "AND = {\n    tag = GER\n    has_war = no\n}\n";
        let or_src = "OR = {\n    OR = {\n        tag = GER\n        tag = FRA\n    }\n}\n";

        for (src, line, col, len) in [(and_src, 1, 0, 3), (or_src, 2, 4, 2)] {
            let table = StringTable::new();
            let ast = parse_string(src, &table);
            let mut errors = Vec::new();
            validate_structural(&ast, &table, &"test.txt".into(), Game::Hoi4, &mut errors);

            let err = errors
                .iter()
                .find(|e| e.code == Some("CW251"))
                .unwrap_or_else(|| panic!("CW251 emitted for {src:?}, got {errors:?}"));
            assert_eq!((err.line, err.col), (line, col), "{src:?}");
            assert_eq!(
                err.end,
                Some((line, col + len)),
                "CW251 must span only the operator key in {src:?}"
            );
        }
    }

    #[test]
    fn cw121_fix_deletes_empty_if() {
        assert_fix("CW121", "x = { if = { } }\n", "x = { }\n");
    }

    #[test]
    fn cw121_offers_no_fix_when_a_chain_follows() {
        for src in [
            "x = { if = { } else_if = { a = 1 } }\n",
            "x = { if = { } else = { a = 1 } }\n",
            "x = { if = { a = 1 } else_if = { } else = { b = 2 } }\n",
        ] {
            let table = StringTable::new();
            let ast = parse_string(src, &table);
            let mut errors = Vec::new();
            validate_structural(&ast, &table, &"test.txt".into(), Game::Hoi4, &mut errors);

            let err = errors
                .iter()
                .find(|e| e.code == Some("CW121"))
                .unwrap_or_else(|| panic!("CW121 emitted for {src:?}, got {errors:?}"));
            assert!(
                err.fix.is_none(),
                "CW121 must not offer a chain-breaking delete for {src:?}"
            );
        }
    }

    #[test]
    fn cw121_still_fixes_a_trailing_empty_else_if() {
        assert_fix(
            "CW121",
            "x = { if = { a = 1 } else_if = { } }\n",
            "x = { if = { a = 1 } }\n",
        );
    }

    #[test]
    fn cw281_fix_deletes_empty_limit() {
        assert_fix("CW281", "x = { limit = { } }\n", "x = { }\n");
    }

    #[test]
    fn not_flagged_in_every_casing() {
        for key in ["NOT", "not", "Not"] {
            let src = format!("x = {{ {key} = {{ has_war = yes tag = GER }} }}\n");
            assert_eq!(codes(&src), ["CW223"], "{key}");
        }
    }

    #[test]
    fn empty_if_flagged_in_every_casing() {
        for key in ["if", "IF", "If"] {
            let src = format!("x = {{ {key} = {{ }} }}\n");
            assert_eq!(codes(&src), ["CW121"], "{key}");
        }
        for key in ["else_if", "ELSE_IF", "Else_if"] {
            let src = format!("x = {{ {key} = {{ }} }}\n");
            assert_eq!(codes(&src), ["CW121", "CW238"], "{key}");
        }
    }

    #[test]
    fn empty_limit_flagged_in_every_casing() {
        for key in ["limit", "LIMIT", "Limit"] {
            let src = format!("x = {{ {key} = {{ }} }}\n");
            assert_eq!(codes(&src), ["CW281"], "{key}");
        }
    }

    #[test]
    fn if_with_only_a_limit_is_empty_in_every_casing() {
        let src = "x = { IF = { LIMIT = { tag = GER } } }\n";
        assert_eq!(codes(src), ["CW121"]);
    }

    #[test]
    fn redundant_and_flagged_in_every_casing() {
        for key in ["AND", "and", "And"] {
            let src = format!("{key} = {{ tag = GER }}\n");
            assert_eq!(codes(&src), ["CW251"], "{key}");
        }
    }

    #[test]
    fn redundant_or_flagged_in_every_casing() {
        for (outer, inner) in [("OR", "OR"), ("or", "OR"), ("OR", "or"), ("or", "or")] {
            let src = format!("{outer} = {{ {inner} = {{ tag = GER }} }}\n");
            assert_eq!(codes(&src), ["CW251"], "{outer} / {inner}");
        }
    }

    #[test]
    fn nor_opens_an_or_context_in_every_casing() {
        for key in ["NOR", "nor"] {
            let src = format!("{key} = {{ OR = {{ tag = GER }} }}\n");
            assert_eq!(codes(&src), ["CW251"], "{key}");
        }
    }

    #[test]
    fn and_inside_or_is_not_redundant_in_every_casing() {
        for key in ["OR", "or"] {
            let src = format!(
                "{key} = {{ AND = {{ has_war = yes tag = GER }} has_capitulated = yes }}\n"
            );
            assert!(codes(&src).is_empty(), "{key}: {:?}", codes(&src));
        }
    }

    #[test]
    fn and_inside_not_is_not_redundant_in_every_casing() {
        for key in ["NOT", "not"] {
            let src = format!("{key} = {{ AND = {{ has_war = yes tag = GER }} }}\n");
            assert!(codes(&src).is_empty(), "{key}: {:?}", codes(&src));
        }
    }

    #[test]
    fn math_expression_operators_are_not_boolean() {
        let src = "x = { check_expr = {\n\
                   value = { value = global.num_days mod = 365 greater_than = 90 }\n\
                   and = { value = global.num_days mod = 365 less_than = 300 }\n\
                   } }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    #[test]
    fn math_expression_not_is_not_a_trigger() {
        let src = "x = { not = { value = current_level equals = 3 } }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    #[test]
    fn triggers_below_a_math_block_are_still_checked() {
        let src = "x = { set_temp_variable = { v = { value = 0\n\
                   if = { limit = { NOT = { has_war = yes tag = GER } } add = 1 }\n\
                   } } }\n";
        assert_eq!(codes(src), ["CW223"]);
    }

    #[test]
    fn count_triggers_is_neutral_in_every_casing() {
        for key in ["count_triggers", "COUNT_TRIGGERS"] {
            let src = format!("{key} = {{ amount = 2 AND = {{ has_war = yes tag = GER }} }}\n");
            assert!(codes(&src).is_empty(), "{key}: {:?}", codes(&src));
        }
    }

    #[test]
    fn event_without_mtth_or_trigger_is_cw107() {
        let c = codes_at("events/test.txt", "my_event = { }\n");
        assert!(
            c.contains(&"CW107"),
            "event with no MTTH/trigger/once should emit CW107, got: {:?}",
            c
        );
    }

    #[test]
    fn event_with_mtth_is_clean() {
        let c = codes_at(
            "events/test.txt",
            "my_event = { mean_time_to_happen = { years = 5 } }\n",
        );
        assert!(!c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn event_is_triggered_only_is_clean() {
        let c = codes_at(
            "events/test.txt",
            "my_event = { is_triggered_only = yes }\n",
        );
        assert!(!c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn event_fire_only_once_is_clean() {
        let c = codes_at("events/test.txt", "my_event = { fire_only_once = yes }\n");
        assert!(!c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn event_trigger_always_no_is_clean() {
        let c = codes_at(
            "events/test.txt",
            "my_event = { trigger = { always = no } }\n",
        );
        assert!(!c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn event_trigger_always_yes_still_cw107() {
        let c = codes_at(
            "events/test.txt",
            "my_event = { trigger = { always = yes } }\n",
        );
        assert!(c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn non_event_root_is_not_cw107() {
        let c = codes_at("events/test.txt", "foo = { }\n");
        assert!(!c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn event_key_outside_events_dir_is_not_cw107() {
        let c = codes_at("common/scripted_effects/test.txt", "my_event = { }\n");
        assert!(!c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn mixed_case_event_key_is_cw107() {
        let c = codes_at("events/test.txt", "My_Event = { }\n");
        assert!(c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn mixed_case_events_dir_is_cw107() {
        let c = codes_at("Events/test.txt", "my_event = { }\n");
        assert!(c.contains(&"CW107"), "got: {:?}", c);
    }

    #[test]
    fn cw107_underlines_only_the_event_key() {
        let table = StringTable::new();
        let ast = parse_string("my_event = {\n    id = test.1\n}\n", &table);
        let mut errors = Vec::new();
        validate_structural(
            &ast,
            &table,
            &"events/test.txt".into(),
            Game::Hoi4,
            &mut errors,
        );

        let err = errors
            .iter()
            .find(|e| e.code == Some("CW107"))
            .expect("CW107 emitted");

        assert_eq!((err.line, err.col), (1, 0));
        assert_eq!(
            err.end,
            Some((1, "my_event".len() as u16)),
            "CW107 must span only the key"
        );
    }

    #[test]
    fn else_without_preceding_if_is_cw238() {
        let c = codes("foo = { else = { a = 1 } }\n");
        assert!(c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn cw238_relates_the_follower_that_has_no_if() {
        let table = StringTable::new();
        let ast = parse_string("foo = {\n    else_if = { a = 1 }\n}\n", &table);
        let mut errors = Vec::new();
        validate_structural(&ast, &table, &"test.txt".into(), Game::Hoi4, &mut errors);

        let err = errors
            .iter()
            .find(|e| e.code == Some("CW238"))
            .expect("CW238 emitted");

        assert_eq!((err.line, err.col), (1, 0), "squiggle stays on the parent");
        let related = &err.related;
        assert_eq!(related.len(), 1, "got: {related:?}");
        assert_eq!((related[0].line, related[0].col), (2, 4));
        assert_eq!(related[0].end, (2, 4 + "else_if".len() as u16));
        assert!(
            related[0].message.contains("else_if"),
            "got: {}",
            related[0].message
        );
    }

    #[test]
    fn if_then_else_is_clean_order() {
        let c = codes("foo = { if = { limit = { } a = 1 } else = { b = 2 } }\n");
        assert!(!c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn properly_ordered_if_else_if_is_clean() {
        let c = codes("foo = { if = { limit = { } a = 1 } else_if = { limit = { } b = 2 } }\n");
        assert!(!c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn nested_limit_and_modifier_do_not_false_positive_cw238() {
        for key in ["limit", "modifier"] {
            let src = format!("foo = {{ {key} = {{ else = {{ a = 1 }} }} }}\n");
            let c = codes(&src);
            assert!(!c.contains(&"CW238"), "{key}: {:?}", c);
        }
    }

    #[test]
    fn nested_else_inside_if_is_clean() {
        let c = codes("foo = { if = { limit = { tag = GER } a = 1 else = { b = 2 } } }\n");
        assert!(!c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn nested_else_if_without_an_else_is_clean() {
        let c = codes(
            "foo = { if = { limit = { tag = GER } a = 1 else_if = { limit = { tag = FRA } b = 2 } } }\n",
        );
        assert!(!c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn nested_else_if_then_else_is_clean() {
        let c = codes(
            "foo = { if = { limit = { tag = GER } a = 1 else_if = { limit = { tag = FRA } b = 2 } else = { c = 3 } } }\n",
        );
        assert!(!c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn second_nested_else_is_cw238() {
        let c = codes(
            "foo = { if = { limit = { tag = GER } a = 1 else = { b = 2 } else = { c = 3 } } }\n",
        );
        assert!(c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn else_if_after_else_is_cw238() {
        let c = codes("foo = { if = { a = 1 } else = { b = 2 } else_if = { c = 3 } }\n");
        assert!(c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn else_nested_inside_else_is_cw238() {
        let c = codes("foo = { if = { a = 1 } else = { b = 2 else = { c = 3 } } }\n");
        assert!(c.contains(&"CW238"), "got: {:?}", c);
    }

    #[test]
    fn cw238_underlines_only_the_containing_key() {
        let table = StringTable::new();
        let ast = parse_string("foo = {\n    else = { a = 1 }\n}\n", &table);
        let mut errors = Vec::new();
        validate_structural(&ast, &table, &"test.txt".into(), Game::Hoi4, &mut errors);

        let err = errors
            .iter()
            .find(|e| e.code == Some("CW238"))
            .expect("CW238 emitted");

        assert_eq!((err.line, err.col), (1, 0));
        assert_eq!(
            err.end,
            Some((1, "foo".len() as u16)),
            "CW238 must span only the key"
        );
    }
}
