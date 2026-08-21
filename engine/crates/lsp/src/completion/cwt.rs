use tower_lsp::lsp_types::*;

use super::sort_for_kind;
use crate::paths::{encoded_position_len, line_prefix_with_encoding};

const FIELD_TYPES: &[(&str, &str)] = &[
    ("scalar", "scalar"),
    ("bool", "bool"),
    ("int", "int"),
    ("int range", "int[${1:min}..${2:max}]"),
    ("float", "float"),
    ("float range", "float[${1:min}..${2:max}]"),
    ("percentage_field", "percentage_field"),
    ("percent_field", "percent_field"),
    ("date_field", "date_field"),
    ("datetime_field", "datetime_field"),
    ("localisation", "localisation"),
    ("localisation_synced", "localisation_synced"),
    ("localisation_inline", "localisation_inline"),
    ("filepath", "filepath"),
    ("filepath[folder]", "filepath[${1:folder}]"),
    (
        "filepath[folder,extension]",
        "filepath[${1:folder},${2:.ext}]",
    ),
    ("icon[folder]", "icon[${1:folder}]"),
    ("enum[name]", "enum[${1:name}]"),
    ("complex_enum[name]", "complex_enum[${1:name}]"),
    ("<type>", "<${1:type}>"),
    ("value[name]", "value[${1:name}]"),
    ("value_set[name]", "value_set[${1:name}]"),
    ("scope_field", "scope_field"),
    ("scope[name]", "scope[${1:name}]"),
    ("scope_group[name]", "scope_group[${1:name}]"),
    ("event_target[name]", "event_target[${1:name}]"),
    ("alias_name[category]", "alias_name[${1:category}]"),
    (
        "alias_match_left[category]",
        "alias_match_left[${1:category}]",
    ),
    (
        "alias_keys_field[category]",
        "alias_keys_field[${1:category}]",
    ),
    ("single_alias_right[name]", "single_alias_right[${1:name}]"),
    ("variable_field", "variable_field"),
    ("variable_field range", "variable_field[${1:min}..${2:max}]"),
    ("int_variable_field", "int_variable_field"),
    (
        "int_variable_field range",
        "int_variable_field[${1:min}..${2:max}]",
    ),
    ("variable_field_32", "variable_field_32"),
    (
        "variable_field_32 range",
        "variable_field_32[${1:min}..${2:max}]",
    ),
    ("int_variable_field_32", "int_variable_field_32"),
    (
        "int_variable_field_32 range",
        "int_variable_field_32[${1:min}..${2:max}]",
    ),
    ("value_field", "value_field"),
    ("value_field range", "value_field[${1:min}..${2:max}]"),
    ("int_value_field", "int_value_field"),
    (
        "int_value_field range",
        "int_value_field[${1:min}..${2:max}]",
    ),
    ("math_expr", "math_expr"),
    ("portrait_dna_field", "portrait_dna_field"),
    ("portrait_properties_field", "portrait_properties_field"),
    ("ck2_dna_field", "ck2_dna_field"),
    ("ck2_dna_property_field", "ck2_dna_property_field"),
    ("ir_family_name_field", "ir_family_name_field"),
    ("ir_country_tag_field", "ir_country_tag_field"),
    ("stellaris_name_format", "stellaris_name_format"),
    (
        "stellaris_name_format[name]",
        "stellaris_name_format[${1:name}]",
    ),
    ("colour_field", "colour_field"),
    ("color_field", "color_field"),
    ("colour[rgb]", "colour[rgb]"),
    ("colour[hsv]", "colour[hsv]"),
    ("ignore_field", "ignore_field"),
];

const DIRECTIVES: &[(&str, &str)] = &[
    ("cardinality", "cardinality = ${1:0..1}"),
    ("scope", "scope = ${1:any}"),
    ("push_scope", "push_scope = ${1:scope}"),
    (
        "replace_scope",
        "replace_scope = { ${1:this} = ${2:scope} }",
    ),
    (
        "replace_scopes",
        "replace_scopes = { ${1:this} = ${2:scope} }",
    ),
    (
        "severity",
        "severity = ${1|error,warning,information,hint|}",
    ),
    ("required", "required"),
    ("optional", "optional"),
    ("primary", "primary"),
    ("explicit", "explicit"),
    ("default_bool", "default_bool = ${1|yes,no|}"),
    (
        "outgoingReferenceLabel",
        "outgoingReferenceLabel = ${1:label}",
    ),
    (
        "incomingReferenceLabel",
        "incomingReferenceLabel = ${1:label}",
    ),
    ("error_if_only_match", "error_if_only_match = ${1:message}"),
    ("type_key_filter", "type_key_filter = ${1:key}"),
    ("graph_related_types", "graph_related_types = { ${1:type} }"),
    ("display_name", "display_name = ${1:name}"),
    ("abbreviation", "abbreviation = ${1:name}"),
    ("starts_with", "starts_with = ${1:prefix}"),
    ("only_if_not", "only_if_not = { ${1:subtype} }"),
];

const ROOT_CONSTRUCTS: &[(&str, &str)] = &[
    ("types", "types = {\n\t$0\n}"),
    ("enums", "enums = {\n\t$0\n}"),
    ("values", "values = {\n\t$0\n}"),
    ("scopes", "scopes = {\n\t$0\n}"),
    ("links", "links = {\n\t$0\n}"),
    ("modifiers", "modifiers = {\n\t$0\n}"),
    ("modifier_categories", "modifier_categories = {\n\t$0\n}"),
    ("alias", "alias[${1:category}:${2:name}] = $0"),
    ("single_alias", "single_alias[${1:name}] = $0"),
];

const TYPE_OPTIONS: &[(&str, &str)] = &[
    ("path", "path = \"${1:game/common}\""),
    ("path_file", "path_file = \"${1:file.txt}\""),
    ("path_extension", "path_extension = ${1:txt}"),
    ("path_strict", "path_strict = ${1|yes,no|}"),
    ("name_field", "name_field = \"${1:name}\""),
    ("type_per_file", "type_per_file = ${1|yes,no|}"),
    ("unique", "unique = ${1|yes,no|}"),
    ("skip_root_key", "skip_root_key = ${1:key}"),
    ("type_key_prefix", "type_key_prefix = ${1:prefix}"),
    ("starts_with", "starts_with = ${1:prefix}"),
    ("severity", "severity = warning"),
    ("should_be_used", "should_be_used = ${1|yes,no|}"),
    ("subtype", "subtype[${1:name}] = {\n\t$0\n}"),
    ("localisation", "localisation = {\n\t$0\n}"),
    ("modifiers", "modifiers = {\n\t$0\n}"),
];

const COMPLEX_ENUM_OPTIONS: &[(&str, &str)] = &[
    ("path", "path = \"${1:game/common}\""),
    ("path_file", "path_file = \"${1:file.txt}\""),
    ("path_extension", "path_extension = ${1:txt}"),
    ("path_strict", "path_strict = ${1|yes,no|}"),
    ("start_from_root", "start_from_root = ${1|yes,no|}"),
    ("name", "name = {\n\t$0\n}"),
];

const SUBTYPE_OPTIONS: &[(&str, &str)] = &[("type_key_field", "type_key_field = ${1:key}")];

const SCOPE_OPTIONS: &[(&str, &str)] = &[
    ("aliases", "aliases = { ${1:alias} }"),
    ("is_subscope_of", "is_subscope_of = { ${1:scope} }"),
];

const LINK_OPTIONS: &[(&str, &str)] = &[
    ("output_scope", "output_scope = ${1:scope}"),
    ("input_scopes", "input_scopes = { ${1:scope} }"),
    ("from_data", "from_data = ${1|yes,no|}"),
    ("data_source", "data_source = ${1:type}"),
    ("prefix", "prefix = ${1:prefix}"),
];

fn item(label: &str, insert_text: &str, kind: CompletionItemKind, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        detail: Some(detail.to_string()),
        insert_text: Some(insert_text.to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        sort_text: sort_for_kind(Some(kind), label),
        ..Default::default()
    }
}

fn brace_stack(text: &str, pos: Position, encoding: &PositionEncodingKind) -> Vec<String> {
    let mut source = String::new();
    for (line, raw) in text.split('\n').enumerate() {
        if line < pos.line as usize {
            source.push_str(raw);
            source.push('\n');
        } else if line == pos.line as usize {
            source
                .push_str(&raw[..crate::paths::position_byte_index(raw, pos.character, encoding)]);
            break;
        } else {
            break;
        }
    }

    let mut stack = Vec::new();
    let mut field = String::new();
    let mut pending_key: Option<String> = None;
    let mut quoted = false;
    let mut escaped = false;
    let mut comment = false;

    for ch in source.chars() {
        if comment {
            if ch == '\n' {
                comment = false;
                field.clear();
                pending_key = None;
            }
            continue;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            field.push(ch);
            continue;
        }
        match ch {
            '#' => comment = true,
            '"' => {
                quoted = true;
                field.push(ch);
            }
            '=' => {
                pending_key = Some(field.trim().trim_matches('"').to_string());
                field.clear();
            }
            '{' => {
                stack.push(pending_key.take().unwrap_or_default());
                field.clear();
            }
            '}' => {
                stack.pop();
                field.clear();
                pending_key = None;
            }
            '\n' => {
                field.clear();
                pending_key = None;
            }
            _ => field.push(ch),
        }
    }
    stack
}

pub(crate) fn cwt_completion_range(
    text: &str,
    pos: Position,
    encoding: &PositionEncodingKind,
) -> Range {
    let prefix = line_prefix_with_encoding(text, pos.line, pos.character, encoding);
    let start_byte = prefix
        .char_indices()
        .rev()
        .find_map(|(byte, ch)| {
            (ch.is_whitespace() || matches!(ch, '=' | '{' | '}' | '#' | '"'))
                .then_some(byte + ch.len_utf8())
        })
        .unwrap_or(0);
    Range::new(
        Position::new(
            pos.line,
            encoded_position_len(&prefix[..start_byte], encoding),
        ),
        Position::new(pos.line, encoded_position_len(prefix, encoding)),
    )
}

fn items_from(
    entries: &[(&str, &str)],
    kind: CompletionItemKind,
    detail: &str,
) -> Vec<CompletionItem> {
    entries
        .iter()
        .map(|(label, insert)| item(label, insert, kind, detail))
        .collect()
}

pub(crate) fn cwt_completions(
    text: &str,
    pos: Position,
    encoding: &PositionEncodingKind,
) -> Vec<CompletionItem> {
    let prefix = line_prefix_with_encoding(text, pos.line, pos.character, encoding);
    let trimmed = prefix.trim_start();

    if trimmed.starts_with("##") && !trimmed.starts_with("###") {
        return items_from(DIRECTIVES, CompletionItemKind::PROPERTY, "rule option");
    }

    let stack = brace_stack(text, pos, encoding);
    let in_subtype_rules = stack.iter().any(|key| key.starts_with("subtype["));
    let in_metadata = matches!(
        stack.first().map(String::as_str),
        Some(
            "types" | "enums" | "values" | "scopes" | "links" | "modifiers" | "modifier_categories"
        )
    ) && !in_subtype_rules;
    if trimmed.contains('=') && !trimmed.ends_with('{') && !in_metadata {
        return items_from(
            FIELD_TYPES,
            CompletionItemKind::TYPE_PARAMETER,
            "field type",
        );
    }

    if stack.is_empty() {
        return items_from(
            ROOT_CONSTRUCTS,
            CompletionItemKind::STRUCT,
            "rule construct",
        );
    }

    let first = stack.first().map(String::as_str);
    let last = stack.last().map(String::as_str);
    match (first, last, stack.len()) {
        (Some("types"), Some("types"), 1) => items_from(
            &[("type", "type[${1:name}] = {\n\t$0\n}")],
            CompletionItemKind::STRUCT,
            "type construct",
        ),
        (Some("types"), Some(key), _) if key.starts_with("type[") => {
            items_from(TYPE_OPTIONS, CompletionItemKind::PROPERTY, "type option")
        }
        (Some("types"), Some(key), _) if key.starts_with("subtype[") => items_from(
            SUBTYPE_OPTIONS,
            CompletionItemKind::PROPERTY,
            "subtype option",
        ),
        (Some("enums"), Some("enums"), 1) => items_from(
            &[
                ("enum", "enum[${1:name}] = { ${2:value} }"),
                ("complex_enum", "complex_enum[${1:name}] = {\n\t$0\n}"),
            ],
            CompletionItemKind::STRUCT,
            "enum construct",
        ),
        (Some("enums"), Some(key), _) if key.starts_with("complex_enum[") => items_from(
            COMPLEX_ENUM_OPTIONS,
            CompletionItemKind::PROPERTY,
            "complex enum option",
        ),
        (Some("values"), Some("values"), 1) => items_from(
            &[("value", "value[${1:name}] = { ${2:value} }")],
            CompletionItemKind::STRUCT,
            "value construct",
        ),
        (Some("scopes"), Some(key), _) if key != "scopes" => {
            items_from(SCOPE_OPTIONS, CompletionItemKind::PROPERTY, "scope option")
        }
        (Some("links"), Some(key), _) if key != "links" => {
            items_from(LINK_OPTIONS, CompletionItemKind::PROPERTY, "link option")
        }
        (Some("modifier_categories"), Some(key), _) if key != "modifier_categories" => items_from(
            &[("supported_scopes", "supported_scopes = { ${1:scope} }")],
            CompletionItemKind::PROPERTY,
            "modifier category option",
        ),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(text: &str, line: u32, col: u32) -> Vec<String> {
        cwt_completions(text, Position::new(line, col), &PositionEncodingKind::UTF16)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn cwt_completion_respects_syntax_context() {
        assert!(labels("", 0, 0).contains(&"types".to_string()));
        assert!(labels("rule = ", 0, 7).contains(&"filepath[folder,extension]".to_string()));
        assert!(labels("## ", 0, 3).contains(&"cardinality".to_string()));
        let type_body = "types = {\n  type[thing] = {\n    \n  }\n}";
        let type_labels = labels(type_body, 2, 4);
        for expected in [
            "path",
            "severity",
            "should_be_used",
            "subtype",
            "localisation",
            "modifiers",
        ] {
            assert!(
                type_labels.iter().any(|label| label == expected),
                "{expected}"
            );
        }
        let subtype_body =
            "types = {\n  type[thing] = {\n    subtype[x] = {\n      key = \n    }\n  }\n}";
        assert!(labels(subtype_body, 3, 12).contains(&"scalar".to_string()));
    }

    #[test]
    fn cwt_context_scanner_tracks_order_strings_comments_and_cursor() {
        let inline = "types = { type[thing] = { path = scalar } }\n";
        assert!(labels(inline, 1, 0).contains(&"types".to_string()));

        let before_close = "types = { }";
        assert!(labels(before_close, 0, 10).contains(&"type".to_string()));

        let quoted = "types = {\n  type[thing] = {\n    path = \"foo}#{\" # }\n    \n  }\n}";
        let type_labels = labels(quoted, 3, 4);
        assert!(type_labels.contains(&"path".to_string()));
        assert!(!type_labels.contains(&"types".to_string()));
    }

    #[test]
    fn cwt_completion_range_replaces_bracketed_partial_and_uses_utf16() {
        let text = "😀 rule = filepath[";
        let pos = Position::new(0, crate::paths::utf16_len(text));
        assert_eq!(
            cwt_completion_range(text, pos, &PositionEncodingKind::UTF16),
            Range::new(Position::new(0, 10), pos)
        );

        let directive = "## car";
        let pos = Position::new(0, crate::paths::utf16_len(directive));
        assert_eq!(
            cwt_completion_range(directive, pos, &PositionEncodingKind::UTF16),
            Range::new(Position::new(0, 3), pos)
        );
    }

    #[test]
    fn cwt_completion_range_uses_utf32() {
        let text = "😀 rule = filepath[";
        let pos = Position::new(0, text.chars().count() as u32);
        assert_eq!(
            cwt_completion_range(text, pos, &PositionEncodingKind::UTF32),
            Range::new(Position::new(0, 9), pos)
        );
    }

    #[test]
    fn cwt_field_inventory_covers_converter_keywords() {
        let labels: std::collections::HashSet<&str> =
            FIELD_TYPES.iter().map(|(label, _)| *label).collect();
        for keyword in [
            "scalar",
            "math_expr",
            "bool",
            "int",
            "float",
            "percentage_field",
            "localisation",
            "localisation_synced",
            "localisation_inline",
            "filepath",
            "date_field",
            "datetime_field",
            "scope_field",
            "variable_field",
            "int_variable_field",
            "variable_field_32",
            "int_variable_field_32",
            "value_field",
            "int_value_field",
            "portrait_dna_field",
            "portrait_properties_field",
            "ck2_dna_field",
            "ck2_dna_property_field",
            "ir_family_name_field",
            "ir_country_tag_field",
            "colour_field",
            "color_field",
            "ignore_field",
            "stellaris_name_format",
            "percent_field",
        ] {
            assert!(
                labels.contains(keyword),
                "missing converter field {keyword}"
            );
        }
        for form in [
            "filepath[folder]",
            "filepath[folder,extension]",
            "enum[name]",
            "complex_enum[name]",
            "value[name]",
            "value_set[name]",
            "scope[name]",
            "event_target[name]",
            "scope_group[name]",
            "alias_keys_field[category]",
            "alias_name[category]",
            "alias_match_left[category]",
            "single_alias_right[name]",
            "icon[folder]",
            "stellaris_name_format[name]",
            "colour[rgb]",
            "colour[hsv]",
            "<type>",
        ] {
            assert!(labels.contains(form), "missing converter form {form}");
        }
    }

    #[test]
    fn directive_snippets_do_not_duplicate_comment_prefix() {
        let item = cwt_completions("## car", Position::new(0, 6), &PositionEncodingKind::UTF16)
            .into_iter()
            .find(|item| item.label == "cardinality")
            .unwrap();
        assert_eq!(item.insert_text.as_deref(), Some("cardinality = ${1:0..1}"));
    }

    #[test]
    fn documentation_comments_do_not_offer_directives() {
        assert!(!labels("### car", 0, 7).contains(&"cardinality".to_string()));
    }
}
