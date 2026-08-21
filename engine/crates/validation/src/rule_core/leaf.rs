//! Per-field value validation: checking a leaf's value against its rule's
//! right-hand field (types, filepaths, variables, localisation, scopes).

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

/// Check a `value[variable]` (VariableGetField) read against the project-wide
/// variable index. Emits CW246 when the value names a variable that was never
/// set. Mirrors F# `checkVariableGetField`: bypasses @-vars, inline math, and
/// loc embeds (those resolve dynamically), and only fires when the index is
/// populated AND the variable checks are enabled.
/// Whether `token` names a config-declared built-in variable: a member of the
/// `value[variable]` set (variables.cwt lists engine-provided reads like
/// `faction_leader`, `num_days`, `threat`). These are valid variable references
/// even without the `var:` prefix and are never dynamically "set", so they must
/// not flag CW246. Both sides may carry a scope suffix — the member declares the
/// family (`party_popularity@<ideology>`) and the read names one of it
/// (`party_popularity@social_democrat`) — so match on the base before the `@`.
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
    // Only the `variable` namespace is backed by an index. Every other
    // `value[ns]` (country_flag, array, division_template_name, font_name, …)
    // is a different value set the variable index never holds, so checking it
    // here would flag every flag/array/token read in the corpus.
    if !ctx.var_checks || namespace != "variable" {
        return;
    }
    let v = raw.trim_matches('"').trim();
    // Dynamic / non-variable forms that resolve at runtime are accepted: @-vars,
    // inline math, a `$ARG$`-concatenated name (the written token is not the
    // runtime name), and any `prefix:` read (`var:`, `token:`, `event_target:`).
    if v.is_empty()
        || v.starts_with('@')
        || v.starts_with('[')
        || v.contains('$')
        || v.contains(':')
    {
        return;
    }
    // Strip a `?`/`^` default-value selector before the lookup.
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

/// The engine resolves textures by stem: a `.dds` reference is satisfied by a
/// shipped `.tga` and vice versa (e.g. vanilla `core.gfx` points at
/// `sort_button_83x29.tga` while only the `.dds` ships). Returns true when the
/// candidate is a texture whose sibling-extension file exists in the index, so
/// CW113 only fires when neither extension is present.
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
        // MathExpr operand: a bare leaf (number or variable reference) is
        // accepted; a `{block}` is a nested math expression validated strictly.
        // This is the path for an operator argument (`subtract = { … }`,
        // expanded from `alias[mathexpr:subtract] = math_expr`) and for a bare
        // operand in the candidate disjunction (`calc = 5`).
        if let NewField::ValueField(ValueType::MathExpr) = right {
            if let Value::Clause(math_children) = &leaf.value {
                let pos = (leaf.pos.start.line, leaf.pos.start.col);
                validate_math_clause(ctx, math_children, &mut scope_context.cloned(), pos, errors);
            }
            return;
        }
        // LocalisationField: check the referenced loc key exists (CW100/CW122)
        // and, when we know the scope, validate the loc string's commands
        // (CW260/CW262). See `validate_localisation_field`.
        if let NewField::LocalisationField { synced, is_inline } = right {
            validate_localisation_field(ctx, leaf, *synced, *is_inline, scope_context, errors);
            return;
        }
        // TypeField: check type_index when available and the index is complete
        // (includes vanilla). When validating a mod without vanilla data the type
        // index only contains mod-defined instances; vanilla instances are absent,
        // so every valid cross-reference would be a false positive.
        if let NewField::TypeField(type_type) = right {
            with_leaf_value_str(&leaf.value, table, |raw_value| {
                // Strip a surrounding quote pair by slice (no allocation).
                let value_str: &str = raw_value
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(raw_value);
                // An empty value (`soundeffect = ""`, `textureFile = ""`) is the
                // engine's "none" — there's nothing to resolve, so don't flag it.
                if value_str.is_empty() {
                    return;
                }
                // A `[...]` value is inline scripted localisation / a defined_text
                // reference (e.g. `picture = "[GetCivilWarVictorPicture]"`) that the
                // engine resolves at runtime, so it can't be checked against a literal
                // type instance.
                if value_str.starts_with('[') {
                    return;
                }
                let type_name = match type_type {
                    TypeType::Simple(n) => n.as_str(),
                    TypeType::Complex { name, .. } => name.as_str(),
                };
                // Record the use for the project-wide unused check (CW239/CW231)
                // before the CW500 gate: a run without vanilla data still knows
                // this value referenced the instance, it just can't say whether
                // the instance exists.
                crate::references::mark_type_field_use(ctx, type_type, value_str);
                // Subtype-qualified references (`<type.subtype>`, e.g.
                // `<event.country_event>` / `<equipment.naval_equip>`) resolve
                // permissively. The index's `type.subtype` membership is derived from
                // each instance's own discriminators for subtype *activation* and is
                // intentionally incomplete for *references*: a variant that inherits a
                // subtype through `archetype = <type.subtype>` isn't listed, so a
                // strict check would false-flag valid references to it. (Precise
                // subtype-reference validation would need full membership, as F#'s
                // invertedTypeMap has.)
                if let Some(idx) = type_index
                && !cwtools_index::is_subtype_key(type_name)
                // Only flag when the index is complete (vanilla loaded) AND we have
                // known instances for this type. Check this BEFORE any lookup so a
                // clean resolve pays for no membership probes.
                && idx.complete
                && !idx.instances(type_name).is_empty()
                {
                    // Complex TypeField (`prefix<type>suffix`) maps a value to an
                    // instance and the game accepts any of these forms, tried in order:
                    //   (a) strip: the value carries the affixes and the instance is
                    //       stored without them (`GFX_event_x` -> `x`).
                    //   (b) raw: the value IS already the full instance name
                    //       (HOI4 ideas may write `picture = GFX_idea_x` directly).
                    //   (c) prepend: the value is bare and the affixed form is the real
                    //       instance (HOI4 ideas: `picture = x` -> `GFX_idea_x`). Built
                    //       lazily only when (a)/(b) miss.
                    // The reference resolves if ANY candidate is a known instance, so
                    // this branch can only ever REMOVE false positives, never add them.
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
            // TypeField is otherwise accepted (non-empty check done by field_matches_value).
            return;
        }
        // FilepathField: check the referenced file exists (CW113). Only when the
        // file index is populated (vanilla loaded); otherwise stay silent.
        if let NewField::FilepathField { prefix, extension } = right {
            if let Some(idx) = type_index
                && !idx.file_index.is_empty()
            {
                with_leaf_value_str(&leaf.value, table, |raw| {
                    let value = raw.trim_matches('"').trim();
                    // Skip dynamic / templated paths we can't resolve statically.
                    let dynamic = value.is_empty()
                        || value.contains('$')
                        || value.contains('[')
                        || value.contains('<');
                    if !dynamic {
                        // The reference with the field's configured extension applied
                        // (if any), without the root prefix. Used for the root-prefixed
                        // lookup and the `.asset`-relative fallback below.
                        let mut rel_value = String::with_capacity(value.len() + 8);
                        rel_value.push_str(value);
                        if let Some(ext) = extension
                            && !ext.is_empty()
                            && !ends_with_ci(&rel_value, ext)
                        {
                            rel_value.push_str(ext);
                        }
                        // Prefixed form, built only when the value doesn't already
                        // carry the root prefix; otherwise the reference IS the
                        // candidate and needs no second copy.
                        let prefixed;
                        let candidate: &str = match prefix {
                            Some(p) if !starts_with_ci(value, p) => {
                                prefixed = format!("{}{}", p, rel_value);
                                &prefixed
                            }
                            _ => &rel_value,
                        };
                        // A `.asset` `file =` (sound/entity assets) resolves relative
                        // to the .asset's own directory, not the field's root prefix
                        // (e.g. `sound/zom/zom_vo.asset` -> `zom_idle_001.wav` beside
                        // it). Genuinely-missing siblings still fail to resolve.
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
                            // A path that is indexed under a different case is
                            // the one CW113 the reader can fix by reading it:
                            // say which spelling the probe found.
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

        // VariableField: a value that must be a number-in-range or a defined
        // variable reference (`add = 5`, `value = my_var`). Mirrors F#
        // `checkVariableField`. Two parts:
        //   - numeric checks (CW271 int-only / CW270 3-decimal precision) run
        //     always — they only fire on a value that parses as a number and
        //     violates the field's int/precision constraint, so they cannot
        //     flood valid config.
        //   - the "variable has not been set" check (CW246) needs a populated
        //     variable index, so it also honours `ctx.var_checks`.
        if let NewField::VariableField {
            is_int, is_32bit, ..
        } = right
        {
            // Fast path: a parsed integer literal is numeric with no fractional
            // part, so it can never violate the int-only (CW271) or 3-decimal
            // 32-bit (CW270) check, and never reaches the CW246 name check.
            // Skip the stringify + reparse. (A `Value::Float` still needs the
            // string form for the precise `decimal_places` count, so only `Int`
            // is short-circuited here.)
            if matches!(leaf.value, Value::Int(_)) {
                return;
            }
            with_leaf_value_str(&leaf.value, table, |raw| {
                let v = raw.trim_matches('"').trim();
                // Accept at-vars (@x), inline math ([...]), `$ARG$`-built names and
                // boolean literals (`yes`/`no`, used by boolean modifiers) — all
                // valid in a value slot (F# FieldValidators bypasses).
                let is_bool = matches!(leaf.value, Value::Bool(_))
                    || v.eq_ignore_ascii_case("yes")
                    || v.eq_ignore_ascii_case("no");
                let bypass = v.is_empty()
                    || v.starts_with('@')
                    || v.starts_with('[')
                    || v.contains('$')
                    || is_bool;
                if !bypass {
                    // Strip a `?`/`^` default-value selector before parsing.
                    let core = v.split(['?', '^']).next().unwrap_or(v).trim();
                    if let Ok(f) = core.parse::<f64>() {
                        // Numeric value: enforce int-ness / decimal precision.
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
                        // Non-numeric value: it must name a defined variable. Stay
                        // lenient: only flag a single bare token (a `.`-chain is a
                        // scope/target, handled elsewhere) that isn't a scope
                        // keyword/link and isn't in the project variable index.
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

        // VariableGetField (rules `value[ns]`): a bare read of a member of the
        // `ns` value set. Mirrors F# `checkVariableGetField` — for the `variable`
        // namespace the value must name a variable that was set somewhere.
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

        // Scope-target validation (CW243 target-wrong-scope / CW245 error-in-target):
        // resolve the chain from the current scope. Gated with the other scope checks.
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
    // Item 2: VALUE-VALIDATOR BYPASSES (F# FieldValidators.fs:82-83, 836-839).
    // Before any type-specific checks, accept scripted variables (@...), localisation
    // references ($$), and inline math ([...]).  These are valid CW script idioms that
    // can legitimately appear in place of any typed value.
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
        // --- Boolean ---
        (NewField::ValueField(ValueType::Bool), Value::Bool(_)) => true,
        (NewField::ValueField(ValueType::Bool), Value::String(t))
        | (NewField::ValueField(ValueType::Bool), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("no")
            })
        }

        // --- Int with range enforcement (item 4) ---
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

        // --- Float with range enforcement (item 4) ---
        (NewField::ValueField(ValueType::Float { min, max }), Value::Float(v)) => {
            *v >= *min && *v <= *max
        }
        // An integer literal is a valid float (the parser emits Int for `1000`).
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

        // --- Enum ---
        // An enum that is absent OR loaded-but-empty is one whose members come
        // from game data not statically available (provinces, ship_units, ...).
        // Be permissive there rather than flag every value. Integer members
        // (e.g. province ids) are compared by their string form.
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

        // --- Percent (item 3): value ends with '%' or is a number ---
        (NewField::ValueField(ValueType::Percent), Value::String(t))
        | (NewField::ValueField(ValueType::Percent), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                text.ends_with('%') || text.parse::<f64>().is_ok()
            })
        }
        (NewField::ValueField(ValueType::Percent), Value::Float(_) | Value::Int(_)) => true,

        // --- Date / DateTime (item 3): basic YYYY.MM.DD[.HH] shape ---
        (NewField::ValueField(ValueType::Date), Value::String(t))
        | (NewField::ValueField(ValueType::Date), Value::QString(t)) => {
            with_match_text(table, t, is_date_shape)
        }
        (NewField::ValueField(ValueType::DateTime), Value::String(t))
        | (NewField::ValueField(ValueType::DateTime), Value::QString(t)) => {
            with_match_text(table, t, is_datetime_shape)
        }

        // --- Ck2Dna (item 3): exactly 32 hex chars (F# FieldValidators.fs:194-204) ---
        (NewField::ValueField(ValueType::Ck2Dna), Value::String(t))
        | (NewField::ValueField(ValueType::Ck2Dna), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                text.len() == 32 && text.chars().all(|c| c.is_ascii_hexdigit())
            })
        }

        // --- Ck2DnaProperty (item 3): length 8 or 32, hex chars (F# FieldValidators.fs:205-211) ---
        (NewField::ValueField(ValueType::Ck2DnaProperty), Value::String(t))
        | (NewField::ValueField(ValueType::Ck2DnaProperty), Value::QString(t)) => {
            with_match_text(table, t, |text| {
                (text.len() == 8 || text.len() == 32) && text.chars().all(|c| c.is_ascii_hexdigit())
            })
        }

        // --- IrFamilyName / StlNameFormat (item 3): accept any string ---
        (NewField::ValueField(ValueType::IrFamilyName), Value::String(_) | Value::QString(_)) => {
            true
        }
        (
            NewField::ValueField(ValueType::StlNameFormat(_)),
            Value::String(_) | Value::QString(_),
        ) => true,

        // --- Scalar: accept anything ---
        (NewField::ScalarField, _) => true,

        // --- MathExpr: a math operand — a number/variable leaf or a `{block}`.
        // The block's contents are validated strictly elsewhere
        // (`validate_math_clause`); here it just qualifies as a candidate for
        // either value shape. ---
        (NewField::ValueField(ValueType::MathExpr), _) => true,

        // --- SpecificField: case-insensitive string match ---
        (NewField::SpecificField(s), Value::String(t))
        | (NewField::SpecificField(s), Value::QString(t)) => table
            .with_string(t.normal, |text| unquote_key(text).eq_ignore_ascii_case(s))
            .unwrap_or(false),
        // A `= yes` / `= no` rule literal is a SpecificField, but the parser emits
        // Bool for those values — match them up (affects every boolean rule field).
        (NewField::SpecificField(s), Value::Bool(b)) => (s == "yes" && *b) || (s == "no" && !*b),
        (NewField::SpecificField(s), Value::Int(i)) => s == &i.to_string(),
        (NewField::SpecificField(s), Value::Float(f)) => s == &f.to_string(),
        // In Paradox script, `key = yes` and `key = { ... }` are often
        // interchangeable (e.g. `create_intelligence_agency = { ... }`).
        // The parser stores blocks as Value::Clause on a Leaf — accept them
        // when the rule expects a specific scalar.
        (NewField::SpecificField(_), Value::Clause(_)) => true,

        // --- TypeField: accept string (cross-file existence is a separate pass) ---
        (NewField::TypeField(TypeType::Simple(type_name)), Value::String(t))
        | (NewField::TypeField(TypeType::Simple(type_name)), Value::QString(t)) => table
            .with_string(t.normal, |s| validate_type_reference(s, type_name))
            .unwrap_or(false),
        (NewField::TypeField(TypeType::Complex { name, .. }), Value::String(t))
        | (NewField::TypeField(TypeType::Complex { name, .. }), Value::QString(t)) => table
            .with_string(t.normal, |s| validate_type_reference(s, name))
            .unwrap_or(false),
        // Numeric type instances — state/province ids are written as bare integers
        // (`states = { 599 600 }`, `<state>`). Accept; existence is a separate pass.
        (NewField::TypeField(_), Value::Int(_) | Value::Float(_)) => true,

        // --- ScopeField ---
        // A scope slot (`scope[country]`, `scope[state]`, ...) is satisfied by far
        // more than the literal scope keywords: country tags (USA), state ids (410),
        // event_target/variable references, and scope chains. Deep resolution is the
        // scope engine's job; at the field level accept any non-empty token rather
        // than flag every tag/id as an error.
        (NewField::ScopeField(_), Value::String(t))
        | (NewField::ScopeField(_), Value::QString(t)) => table
            .with_string(t.normal, |s| !s.is_empty())
            .unwrap_or(false),
        (NewField::ScopeField(_), Value::Int(_)) | (NewField::ScopeField(_), Value::Float(_)) => {
            true
        }

        // --- VariableField with range enforcement (item 4) ---
        (NewField::VariableField { min, max, .. }, Value::Float(v)) => *v >= *min && *v <= *max,
        (NewField::VariableField { min, max, .. }, Value::Int(v)) => {
            (*v as f64) >= *min && (*v as f64) <= *max
        }
        // yes/no are acceptable in variable contexts.
        (NewField::VariableField { .. }, Value::Bool(_)) => true,
        (NewField::VariableField { min, max, .. }, Value::String(t))
        | (NewField::VariableField { min, max, .. }, Value::QString(t)) => {
            with_match_text(table, t, |text| {
                if let Ok(v) = text.parse::<f64>() {
                    v >= *min && v <= *max
                } else {
                    // non-numeric string: accept (could be a scripted variable not caught by bypass)
                    true
                }
            })
        }

        // --- LocalisationField / FilepathField ---
        (NewField::LocalisationField { .. }, Value::String(_) | Value::QString(_)) => true,
        // A localisation slot also accepts the meta-localisation block form
        // `{ localization_key = X PARAM = value ... }` (used in tooltip,
        // custom_override_tooltip, etc.). Accept any clause here.
        (NewField::LocalisationField { .. }, Value::Clause(_)) => true,
        (NewField::FilepathField { .. }, Value::String(_) | Value::QString(_)) => true,

        // --- IconField (item 3): accept any string ---
        (NewField::IconField(_), Value::String(_) | Value::QString(_)) => true,

        // --- VariableGetField / VariableSetField (item 3): accept any string or numeric ---
        (NewField::VariableGetField(_), _) => true,
        (NewField::VariableSetField(_), _) => true,

        // --- ValueScopeField / ValueScopeMarkerField (item 3): accept number, @var, or scope chain ---
        (NewField::ValueScopeField { .. }, Value::Float(_) | Value::Int(_)) => true,
        (NewField::ValueScopeField { .. }, Value::String(_) | Value::QString(_)) => true,
        (NewField::ValueScopeMarkerField { .. }, Value::Float(_) | Value::Int(_)) => true,
        (NewField::ValueScopeMarkerField { .. }, Value::String(_) | Value::QString(_)) => true,

        // --- AliasValueKeysField (item 3): accept any string key ---
        (NewField::AliasValueKeysField(_), Value::String(_) | Value::QString(_)) => true,

        // --- AliasField / SingleAliasField: shape check only (accept clause or
        // string). Deep validation of alias bodies happens in validate_alias_usage,
        // not here — this path is the secondary value-matching fallback. ---
        (NewField::AliasField(_), Value::Clause(_)) => true,
        (NewField::AliasField(_), Value::String(_) | Value::QString(_)) => true,
        (NewField::SingleAliasField(_), Value::Clause(_)) => true,
        (NewField::SingleAliasField(_), Value::String(_) | Value::QString(_)) => true,

        // --- MarkerField: accept anything (validated elsewhere) ---
        (NewField::MarkerField(_), _) => true,

        // --- IgnoreMarkerField / IgnoreField: always accept ---
        (NewField::IgnoreMarkerField, _) => true,
        (NewField::IgnoreField(_), _) => true,

        // Value shapes the arms above did not accept. Wildcard on the value side
        // only, so a new NewField or ValueType variant fails to compile here
        // instead of silently never matching.
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
    // A TypeField references an *instance* of the named type (e.g. a `node_type`
    // rule is satisfied by `node_type_one`, a defined instance), not the literal
    // type name. Verifying the instance actually exists needs a cross-file type
    // index (built in the info crate); until that is wired in, accept any
    // non-empty token rather than flag every valid reference as an error.
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

/// Decision-table tests for the fall-through arms #287 made explicit: the value
/// shapes each field rejects. Non-string shapes only, so the `@`/`$$`/`[` bypass
/// and the string arms above stay with the integration tests and corpus guard.
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

    /// Contrast guard for the table above: the same fixture accepts shapes the
    /// arms explicitly allow, so the rejections cannot pass by the matcher
    /// rejecting everything.
    #[test]
    fn listed_value_shapes_are_accepted() {
        let table = StringTable::new();
        let ruleset = RuleSet::default();
        let cases = [
            (NewField::ValueField(ValueType::Bool), Value::Bool(true)),
            // Range max is inclusive.
            (
                NewField::ValueField(ValueType::Int { min: 0, max: 10 }),
                Value::Int(10),
            ),
            (NewField::ScalarField, clause()),
            // yes/no are acceptable in variable contexts.
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
