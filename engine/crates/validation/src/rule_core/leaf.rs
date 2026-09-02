use cwtools_game::scope_engine::ScopeContext;
use cwtools_parser::ast::{SourcePos, Value};
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;

use crate::common::*;
use crate::ctx::ValidationCtx;
use crate::loc_field::validate_localisation_field;
use crate::scope::validate_scope_target;
use cwtools_error_codes as error_codes;

use super::children::validate_math_clause;
use super::suggest::best_suggestion;

pub(crate) fn is_builtin_variable(ruleset: &RuleSet, token: &str) -> bool {
    let token_base = token.split('@').next().unwrap_or(token);
    ruleset.is_builtin_variable_base(token_base)
}

pub(super) fn check_variable_get(
    ctx: &ValidationCtx,
    namespace: &str,
    raw: &str,
    line: u32,
    col: u16,
    end: SourcePos,
    errors: &mut Vec<ValidationError>,
) {
    if !ctx.var_checks || namespace != "variable" {
        return;
    }
    let v = raw.trim_matches('"').trim();
    if v.is_empty()
        || v.starts_with('@')
        || v.starts_with('[')
        || v.contains('$')
        || v.contains(':')
    {
        return;
    }
    let core = v.split(['?', '^']).next().unwrap_or(v).trim();
    if core.is_empty() {
        return;
    }
    if !is_builtin_variable(ctx.ruleset, core)
        && !ctx.is_loop_var(core)
        && let Some(idx) = ctx.type_index
        && !idx.var_index.is_empty()
        && !idx.var_index.contains(core)
    {
        errors.push(
            ValidationError::from_code(
                &error_codes::CW246_UNSET_VARIABLE,
                ctx.file_path,
                line,
                col,
                &[core],
            )
            .with_end(end),
        );
    }
}

fn texture_sibling_exists(candidate: &str, file_index: &cwtools_index::FileIndex) -> bool {
    let swap = if ends_with_ci(candidate, ".dds") {
        ".tga"
    } else if ends_with_ci(candidate, ".tga") {
        ".dds"
    } else {
        return false;
    };
    let mut sibling = candidate.to_ascii_lowercase();
    sibling.truncate(sibling.len() - 4);
    sibling.push_str(swap);
    file_index.contains(&sibling)
}

#[tracing::instrument(level = "trace", skip_all)]
pub(super) fn validate_leaf(
    ctx: &ValidationCtx,
    leaf: &cwtools_parser::ast::Leaf,
    rule_type: &RuleType,
    scope_context: Option<&ScopeContext>,
    errors: &mut Vec<ValidationError>,
) {
    let table = ctx.table;
    let file_path = ctx.file_path;
    let type_index = ctx.type_index;
    if let RuleType::LeafRule { right, .. } = rule_type {
        if let NewField::ValueField(ValueType::MathExpr) = right {
            if let Value::Clause(math_children) = &leaf.value {
                let pos = (leaf.pos.start.line, leaf.pos.start.col);
                validate_math_clause(ctx, math_children, &mut scope_context.cloned(), pos, errors);
            }
            return;
        }
        if let NewField::LocalisationField { synced, is_inline } = right {
            validate_localisation_field(ctx, leaf, *synced, *is_inline, scope_context, errors);
            return;
        }
        if let NewField::TypeField(type_type) = right {
            with_leaf_value_str(&leaf.value, table, |raw_value| {
                let value_str: &str = raw_value
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(raw_value);
                if value_str.is_empty() {
                    return;
                }
                if value_str.starts_with('[') {
                    return;
                }
                let type_name = match type_type {
                    TypeType::Simple(n) => n.as_str(),
                    TypeType::Complex { name, .. } => name.as_str(),
                };
                crate::references::mark_type_field_use(ctx, type_type, value_str);
                if let Some(idx) = type_index
                    && !cwtools_index::is_subtype_key(type_name)
                    && idx.complete
                    && !idx.instances(type_name).is_empty()
                {
                    let (lookup_value, resolved): (&str, bool) = match type_type {
                        TypeType::Complex { prefix, suffix, .. } => {
                            let mut v = value_str;
                            if !prefix.is_empty() {
                                v = v.strip_prefix(prefix.as_str()).unwrap_or(v);
                            }
                            if !suffix.is_empty() {
                                v = v.strip_suffix(suffix.as_str()).unwrap_or(v);
                            }
                            let resolved = idx.contains(type_name, v)
                                || idx.contains(type_name, value_str)
                                || idx.contains(
                                    type_name,
                                    &format!("{}{}{}", prefix, value_str, suffix),
                                );
                            (v, resolved)
                        }
                        _ => (value_str, idx.contains(type_name, value_str)),
                    };
                    if !resolved {
                        let is_event = type_name == "event" || type_name.starts_with("event.");
                        let (code, message) = if is_event {
                            let c = &error_codes::CW222_UNDEFINED_EVENT;
                            (c, c.format(&[lookup_value]))
                        } else {
                            let key = table
                                .with_string(leaf.key.normal, |s| s.to_string())
                                .unwrap_or_default();
                            (
                                &error_codes::CW500_TYPE_NOT_FOUND,
                                format!(
                                    "Field '{}' references '{}' which is not a known instance of type '{}'",
                                    key, lookup_value, type_name
                                ),
                            )
                        };
                        errors.push(
                            ValidationError::from_code_with(
                                code,
                                code.severity,
                                file_path,
                                leaf.pos.start.line,
                                leaf.pos.start.col,
                                message,
                            )
                            .with_end(leaf.pos.end),
                        );
                    }
                }
            });
            return;
        }
        if let NewField::FilepathField { prefix, extension } = right {
            if let Some(idx) = type_index
                && !idx.file_index.is_empty()
            {
                with_leaf_value_str(&leaf.value, table, |raw| {
                    let value = raw.trim_matches('"').trim();
                    let dynamic = value.is_empty()
                        || value.contains('$')
                        || value.contains('[')
                        || value.contains('<');
                    if !dynamic {
                        let mut rel_value = String::with_capacity(value.len() + 8);
                        rel_value.push_str(value);
                        if let Some(ext) = extension
                            && !ext.is_empty()
                            && !ends_with_ci(&rel_value, ext)
                        {
                            rel_value.push_str(ext);
                        }
                        let prefixed;
                        let candidate: &str = match prefix {
                            Some(p) if !starts_with_ci(value, p) => {
                                prefixed = format!("{}{}", p, rel_value);
                                &prefixed
                            }
                            _ => &rel_value,
                        };
                        let asset_relative = ends_with_ci(file_path, ".asset")
                            && idx.file_index.resolve_relative(file_path, &rel_value);
                        if !idx.file_index.contains(candidate)
                            && !texture_sibling_exists(candidate, &idx.file_index)
                            && !asset_relative
                        {
                            let code = &error_codes::CW113_MISSING_FILE;
                            let mut err = ValidationError::from_code(
                                code,
                                file_path,
                                leaf.pos.start.line,
                                leaf.pos.start.col,
                                &[candidate],
                            )
                            .with_end(leaf.pos.end);
                            if let Some(on_disk) = idx.file_index.on_disk_case(candidate) {
                                err = err.with_related(format!("indexed as {on_disk}"), leaf.pos);
                            }
                            errors.push(err);
                        }
                    }
                });
            }
            return;
        }

        if let NewField::VariableField {
            is_int, is_32bit, ..
        } = right
        {
            if matches!(leaf.value, Value::Int(_)) {
                return;
            }
            with_leaf_value_str(&leaf.value, table, |raw| {
                let v = raw.trim_matches('"').trim();
                let is_bool = matches!(leaf.value, Value::Bool(_))
                    || v.eq_ignore_ascii_case("yes")
                    || v.eq_ignore_ascii_case("no");
                let bypass = v.is_empty()
                    || v.starts_with('@')
                    || v.starts_with('[')
                    || v.contains('$')
                    || is_bool;
                if !bypass {
                    let core = v.split(['?', '^']).next().unwrap_or(v).trim();
                    if let Ok(f) = core.parse::<f64>() {
                        if *is_int && f.fract() != 0.0 {
                            let code = &error_codes::CW271_VARIABLE_INT_ONLY;
                            errors.push(
                                ValidationError::from_code(
                                    code,
                                    file_path,
                                    leaf.pos.start.line,
                                    leaf.pos.start.col,
                                    &[],
                                )
                                .with_end(leaf.pos.end),
                            );
                        } else if *is_32bit && decimal_places(core) > 3 {
                            let code = &error_codes::CW270_VARIABLE_TOO_SMALL;
                            errors.push(
                                ValidationError::from_code(
                                    code,
                                    file_path,
                                    leaf.pos.start.line,
                                    leaf.pos.start.col,
                                    &[],
                                )
                                .with_end(leaf.pos.end),
                            );
                        }
                    } else if ctx.var_checks {
                        let single_token = !core.contains('.') && !core.contains(':');
                        let is_scopeish = scope_context
                            .map(|sc| resolves_as_scope_key(sc, core))
                            .unwrap_or(false);
                        if single_token
                            && !is_scopeish
                            && !is_builtin_variable(ctx.ruleset, core)
                            && !ctx.is_loop_var(core)
                            && let Some(idx) = type_index
                            && !idx.var_index.is_empty()
                            && !idx.var_index.contains(core)
                        {
                            let code = &error_codes::CW246_UNSET_VARIABLE;
                            errors.push(
                                ValidationError::from_code(
                                    code,
                                    file_path,
                                    leaf.pos.start.line,
                                    leaf.pos.start.col,
                                    &[core],
                                )
                                .with_end(leaf.pos.end),
                            );
                        }
                    }
                }
            });
            return;
        }

        if let NewField::VariableGetField(ns) = right {
            with_leaf_value_str(&leaf.value, table, |raw| {
                check_variable_get(
                    ctx,
                    ns,
                    raw,
                    leaf.pos.start.line,
                    leaf.pos.start.col,
                    leaf.pos.end,
                    errors,
                );
            });
            return;
        }

        if let NewField::ScopeField(expected) = right
            && ctx.scope_checks
            && let Some(ctx) = scope_context
        {
            with_leaf_value_str(&leaf.value, table, |value| {
                validate_scope_target(ctx, value, expected, leaf, file_path, errors);
            });
        }

        if !field_matches_value(right, &leaf.value, table, ctx.ruleset) {
            let expected = field_to_description(right);
            let actual = leaf_value_to_string(&leaf.value, table);
            let key = table
                .with_string(leaf.key.normal, |s| s.to_string())
                .unwrap_or_default();
            let mut err = ValidationError::from_code(
                &error_codes::CW240_UNEXPECTED_VALUE,
                file_path,
                leaf.pos.start.line,
                leaf.pos.start.col,
                &[&format!(
                    "Field '{}' has value '{}', expected {}",
                    key, actual, expected
                )],
            )
            .with_end(leaf.pos.end);
            if let NewField::ValueField(ValueType::Enum(enum_name)) = right
                && let Some(&idx) = ctx.ruleset.enum_by_name().get(enum_name)
                && let Some(cand) = best_suggestion(
                    actual
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                        .unwrap_or(&actual),
                    ctx.ruleset.enums[idx].values.iter().map(String::as_str),
                )
            {
                let replacement =
                    format!("\"{}\"", cand.replace('\\', "\\\\").replace('"', "\\\""));
                err = err.with_fix(cwtools_parser::fix::SuggestedFix::replace(
                    cwtools_i18n::format(cwtools_i18n::Key::ActionDidYouMean, &[cand]),
                    leaf.value_pos,
                    replacement,
                ));
            }
            errors.push(err);
        }
    }
}

pub(crate) fn field_matches_value(
    field: &NewField,
    value: &Value,
    table: &StringTable,
    ruleset: &RuleSet,
) -> bool {
    match value {
        Value::String(t) | Value::QString(t)
            if with_match_text(table, t, |text| {
                text.starts_with('@') || text.contains("$$") || text.starts_with('[')
            }) =>
        {
            return true;
        }
        _ => {}
    }

    match (field, value) {
        (NewField::ValueField(ValueType::Bool), Value::Bool(_)) => true,
        (NewField::ValueField(ValueType::Bool), Value::String(t))
        | (NewField::ValueField(ValueType::Bool), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("no")
            })
        }

        (NewField::ValueField(ValueType::Int { min, max }), Value::Int(v)) => {
            let v_i64 = *v;
            v_i64 >= i64::from(*min) && v_i64 <= i64::from(*max)
        }
        (NewField::ValueField(ValueType::Int { min, max }), Value::String(t))
        | (NewField::ValueField(ValueType::Int { min, max }), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                if let Ok(v) = text.parse::<i64>() {
                    v >= i64::from(*min) && v <= i64::from(*max)
                } else {
                    false
                }
            })
        }

        (NewField::ValueField(ValueType::Float { min, max }), Value::Float(v)) => {
            *v >= *min && *v <= *max
        }
        (NewField::ValueField(ValueType::Float { min, max }), Value::Int(v)) => {
            (*v as f64) >= *min && (*v as f64) <= *max
        }
        (NewField::ValueField(ValueType::Float { min, max }), Value::String(t))
        | (NewField::ValueField(ValueType::Float { min, max }), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                if let Ok(v) = text.parse::<f64>() {
                    v >= *min && v <= *max
                } else {
                    false
                }
            })
        }

        (NewField::ValueField(ValueType::Enum(enum_name)), Value::String(t))
        | (NewField::ValueField(ValueType::Enum(enum_name)), Value::QString(t)) => {
            with_match_text(table, t, |text| enum_contains(ruleset, enum_name, text))
        }
        (NewField::ValueField(ValueType::Enum(enum_name)), Value::Int(i)) => {
            enum_contains(ruleset, enum_name, &i.to_string())
        }
        (NewField::ValueField(ValueType::Enum(enum_name)), Value::Float(f)) => {
            enum_contains(ruleset, enum_name, &f.to_string())
        }

        (NewField::ValueField(ValueType::Percent), Value::String(t))
        | (NewField::ValueField(ValueType::Percent), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                text.ends_with('%') || text.parse::<f64>().is_ok()
            })
        }
        (NewField::ValueField(ValueType::Percent), Value::Float(_) | Value::Int(_)) => true,

        (NewField::ValueField(ValueType::Date), Value::String(t))
        | (NewField::ValueField(ValueType::Date), Value::QString(t)) => {
            with_match_text(table, t, is_date_shape)
        }
        (NewField::ValueField(ValueType::DateTime), Value::String(t))
        | (NewField::ValueField(ValueType::DateTime), Value::QString(t)) => {
            with_match_text(table, t, is_datetime_shape)
        }

        (NewField::ValueField(ValueType::Ck2Dna), Value::String(t))
        | (NewField::ValueField(ValueType::Ck2Dna), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                text.len() == 32 && text.chars().all(|c| c.is_ascii_hexdigit())
            })
        }

        (NewField::ValueField(ValueType::Ck2DnaProperty), Value::String(t))
        | (NewField::ValueField(ValueType::Ck2DnaProperty), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                (text.len() == 8 || text.len() == 32) && text.chars().all(|c| c.is_ascii_hexdigit())
            })
        }

        (NewField::ValueField(ValueType::IrFamilyName), Value::String(_) | Value::QString(_)) => {
            true
        }
        (
            NewField::ValueField(ValueType::StlNameFormat(_)),
            Value::String(_) | Value::QString(_),
        ) => true,

        (NewField::ScalarField, _) => true,

        (NewField::ValueField(ValueType::MathExpr), _) => true,

        (NewField::SpecificField(s), Value::String(t))
        | (NewField::SpecificField(s), Value::QString(t)) => table
            .with_string(t.normal, |text| unquote_key(text).eq_ignore_ascii_case(s))
            .unwrap_or(false),
        (NewField::SpecificField(s), Value::Bool(b)) => (s == "yes" && *b) || (s == "no" && !*b),
        (NewField::SpecificField(s), Value::Int(i)) => s == &i.to_string(),
        (NewField::SpecificField(s), Value::Float(f)) => s == &f.to_string(),
        (NewField::SpecificField(_), Value::Clause(_)) => true,

        (NewField::TypeField(TypeType::Simple(type_name)), Value::String(t))
        | (NewField::TypeField(TypeType::Simple(type_name)), Value::QString(t)) => table
            .with_string(t.normal, |s| validate_type_reference(s, type_name))
            .unwrap_or(false),
        (NewField::TypeField(TypeType::Complex { name, .. }), Value::String(t))
        | (NewField::TypeField(TypeType::Complex { name, .. }), Value::QString(t)) => table
            .with_string(t.normal, |s| validate_type_reference(s, name))
            .unwrap_or(false),
        (NewField::TypeField(_), Value::Int(_) | Value::Float(_)) => true,

        (NewField::ScopeField(_), Value::String(t))
        | (NewField::ScopeField(_), Value::QString(t)) => table
            .with_string(t.normal, |s| !s.is_empty())
            .unwrap_or(false),
        (NewField::ScopeField(_), Value::Int(_)) | (NewField::ScopeField(_), Value::Float(_)) => {
            true
        }

        (NewField::VariableField { min, max, .. }, Value::Float(v)) => *v >= *min && *v <= *max,
        (NewField::VariableField { min, max, .. }, Value::Int(v)) => {
            (*v as f64) >= *min && (*v as f64) <= *max
        }
        (NewField::VariableField { .. }, Value::Bool(_)) => true,
        (NewField::VariableField { min, max, .. }, Value::String(t))
        | (NewField::VariableField { min, max, .. }, Value::QString(t)) => {
            with_match_text(table, t, |text| {
                if let Ok(v) = text.parse::<f64>() {
                    v >= *min && v <= *max
                } else {
                    true
                }
            })
        }

        (NewField::LocalisationField { .. }, Value::String(_) | Value::QString(_)) => true,
        (NewField::LocalisationField { .. }, Value::Clause(_)) => true,
        (NewField::FilepathField { .. }, Value::String(_) | Value::QString(_)) => true,

        (NewField::IconField(_), Value::String(_) | Value::QString(_)) => true,

        (NewField::VariableGetField(_), _) => true,
        (NewField::VariableSetField(_), _) => true,

        (NewField::ValueScopeField { .. }, Value::Float(_) | Value::Int(_)) => true,
        (NewField::ValueScopeField { .. }, Value::String(_) | Value::QString(_)) => true,
        (NewField::ValueScopeMarkerField { .. }, Value::Float(_) | Value::Int(_)) => true,
        (NewField::ValueScopeMarkerField { .. }, Value::String(_) | Value::QString(_)) => true,

        (NewField::AliasValueKeysField(_), Value::String(_) | Value::QString(_)) => true,

        (NewField::AliasField(_), Value::Clause(_)) => true,
        (NewField::AliasField(_), Value::String(_) | Value::QString(_)) => true,
        (NewField::SingleAliasField(_), Value::Clause(_)) => true,
        (NewField::SingleAliasField(_), Value::String(_) | Value::QString(_)) => true,

        (NewField::MarkerField(_), _) => true,

        (NewField::IgnoreMarkerField, _) => true,
        (NewField::IgnoreField(_), _) => true,

        (
            NewField::ValueField(
                ValueType::Bool
                | ValueType::Int { .. }
                | ValueType::Float { .. }
                | ValueType::Enum(_)
                | ValueType::Percent
                | ValueType::Date
                | ValueType::DateTime
                | ValueType::Ck2Dna
                | ValueType::Ck2DnaProperty
                | ValueType::IrFamilyName
                | ValueType::StlNameFormat(_),
            ),
            _,
        ) => false,
        (
            NewField::TypeField(_)
            | NewField::ScopeField(_)
            | NewField::VariableField { .. }
            | NewField::LocalisationField { .. }
            | NewField::FilepathField { .. }
            | NewField::IconField(_)
            | NewField::ValueScopeField { .. }
            | NewField::ValueScopeMarkerField { .. }
            | NewField::AliasValueKeysField(_)
            | NewField::AliasField(_)
            | NewField::SingleAliasField(_),
            _,
        ) => false,
    }
}

fn validate_type_reference(text: &str, _expected_type: &str) -> bool {
    !text.is_empty()
}

fn field_to_description(field: &NewField) -> String {
    match field {
        NewField::ValueField(vt) => format!("{:?}", vt),
        NewField::ScalarField => "any value".to_string(),
        NewField::SpecificField(s) => format!("'{}'", s),
        NewField::TypeField(tt) => format!("{:?}", tt),
        NewField::ScopeField(scopes) => format!("scope {:?}", scopes),
        NewField::LocalisationField { synced, .. } => format!("localisation (synced={})", synced),
        _ => "unknown field type".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clause() -> Value {
        Value::Clause(Vec::new())
    }

    #[test]
    fn unlisted_value_shapes_are_rejected() {
        let table = StringTable::new();
        let ruleset = RuleSet::default();
        let cases = [
            (NewField::ValueField(ValueType::Bool), Value::Int(1)),
            (NewField::ValueField(ValueType::Bool), clause()),
            (
                NewField::ValueField(ValueType::Int { min: 0, max: 10 }),
                Value::Bool(true),
            ),
            (
                NewField::ValueField(ValueType::Int { min: 0, max: 10 }),
                Value::Float(1.5),
            ),
            (
                NewField::ValueField(ValueType::Float { min: 0.0, max: 1.0 }),
                clause(),
            ),
            (
                NewField::ValueField(ValueType::Enum("e".to_string())),
                Value::Bool(false),
            ),
            (NewField::ValueField(ValueType::Percent), clause()),
            (NewField::ValueField(ValueType::Date), Value::Int(2000)),
            (
                NewField::ValueField(ValueType::DateTime),
                Value::Float(2000.1),
            ),
            (NewField::ValueField(ValueType::Ck2Dna), Value::Int(0)),
            (NewField::ValueField(ValueType::Ck2DnaProperty), clause()),
            (
                NewField::ValueField(ValueType::IrFamilyName),
                Value::Bool(true),
            ),
            (
                NewField::ValueField(ValueType::StlNameFormat("f".to_string())),
                Value::Int(1),
            ),
            (
                NewField::TypeField(TypeType::Simple("t".to_string())),
                Value::Bool(true),
            ),
            (
                NewField::TypeField(TypeType::Simple("t".to_string())),
                clause(),
            ),
            (NewField::ScopeField(vec!["country".to_string()]), clause()),
            (
                NewField::VariableField {
                    is_int: false,
                    is_32bit: false,
                    min: 0.0,
                    max: 10.0,
                },
                clause(),
            ),
            (
                NewField::LocalisationField {
                    synced: false,
                    is_inline: false,
                },
                Value::Int(1),
            ),
            (
                NewField::FilepathField {
                    prefix: None,
                    extension: None,
                },
                Value::Bool(true),
            ),
            (NewField::IconField("gfx".to_string()), Value::Int(1)),
            (
                NewField::ValueScopeField {
                    is_int: false,
                    min: 0.0,
                    max: 10.0,
                },
                clause(),
            ),
            (
                NewField::ValueScopeMarkerField {
                    is_int: false,
                    min: 0.0,
                    max: 10.0,
                },
                clause(),
            ),
            (
                NewField::AliasValueKeysField("a".to_string()),
                Value::Int(1),
            ),
            (NewField::AliasField("effect".to_string()), Value::Int(1)),
            (
                NewField::SingleAliasField("s".to_string()),
                Value::Bool(true),
            ),
        ];
        for (field, value) in &cases {
            assert!(
                !field_matches_value(field, value, &table, &ruleset),
                "{field:?} must reject {value:?}"
            );
        }
    }

    #[test]
    fn listed_value_shapes_are_accepted() {
        let table = StringTable::new();
        let ruleset = RuleSet::default();
        let cases = [
            (NewField::ValueField(ValueType::Bool), Value::Bool(true)),
            (
                NewField::ValueField(ValueType::Int { min: 0, max: 10 }),
                Value::Int(10),
            ),
            (NewField::ScalarField, clause()),
            (
                NewField::VariableField {
                    is_int: false,
                    is_32bit: false,
                    min: 0.0,
                    max: 10.0,
                },
                Value::Bool(true),
            ),
        ];
        for (field, value) in &cases {
            assert!(
                field_matches_value(field, value, &table, &ruleset),
                "{field:?} must accept {value:?}"
            );
        }
    }
}
