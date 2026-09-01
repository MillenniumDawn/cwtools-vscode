//! Localisation-field checks (CW100/CW122 existence, CW260/CW266 loc commands)
//! and modifier-key set construction.

use cwtools_game::scope_engine::ScopeContext;
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_parser::ast::Value;
use cwtools_rules::rules_types::*;

use crate::common::{ValidationError, with_leaf_value_str};
use crate::ctx::ValidationCtx;
use crate::rule_core::is_builtin_variable;
use cwtools_error_codes as error_codes;

/// Build the set of valid modifier names for `alias_name[modifier]` slots from
/// the ruleset's `modifiers = { ... }` block. Templated entries like
/// `production_speed_<building>_factor` / `<ideology>_drift` are expanded against
/// the type index, one instance each. Single source of truth so the CLI and LSP
/// agree on what counts as a modifier.
pub fn build_modifier_keys(
    ruleset: &RuleSet,
    type_index: &cwtools_index::TypeIndex,
) -> std::collections::HashSet<String> {
    let mut mk = std::collections::HashSet::new();
    // One expansion buffer for the whole (template x instance) product; only the
    // lowercased result is handed to the set.
    let mut expanded = String::new();
    for (m, _category) in &ruleset.modifiers {
        match (m.find('<'), m.find('>')) {
            (Some(open), Some(close)) if open < close => {
                let tn = &m[open + 1..close];
                let pre = &m[..open];
                let suf = &m[close + 1..];
                for (_uri, inst) in type_index.instances(tn) {
                    expanded.clear();
                    expanded.push_str(pre);
                    expanded.push_str(&inst.name);
                    expanded.push_str(suf);
                    mk.insert(expanded.to_lowercase());
                }
            }
            _ => {
                mk.insert(m.to_lowercase());
            }
        }
    }
    mk
}

/// Validate a `LocalisationField` leaf: that the referenced loc key exists
/// (CW100 / CW122) and, when the scope is known, that the loc string's commands
/// are valid in that scope (CW260 / CW262). Mirrors F# `checkLocKey*` plus the
/// scope-aware loc-command checks.
pub(crate) fn validate_localisation_field(
    ctx: &ValidationCtx,
    leaf: &cwtools_parser::ast::Leaf,
    synced: bool,
    is_inline: bool,
    scope_context: Option<&ScopeContext>,
    errors: &mut Vec<ValidationError>,
) {
    // The meta-localisation block form `{ localization_key = X PARAM = ... }` is
    // accepted unconditionally (its inner key is validated as its own leaf).
    if let Value::Clause(_) = &leaf.value {
        return;
    }
    let was_quoted = matches!(leaf.value, Value::QString(_));
    // Borrow the reference text rather than copying it out of the string table:
    // every loc-bearing field in the corpus comes through here.
    with_leaf_value_str(&leaf.value, ctx.table, |raw| {
        check_loc_key(
            ctx,
            leaf,
            raw.trim_matches('"'),
            LocKeyOpts {
                was_quoted,
                synced,
                is_inline,
            },
            scope_context,
            errors,
        )
    });
}

struct LocKeyOpts {
    was_quoted: bool,
    synced: bool,
    is_inline: bool,
}

/// The body of [`validate_localisation_field`], with the reference text already
/// unquoted and borrowed.
fn check_loc_key(
    ctx: &ValidationCtx,
    leaf: &cwtools_parser::ast::Leaf,
    key_raw: &str,
    opts: LocKeyOpts,
    scope_context: Option<&ScopeContext>,
    errors: &mut Vec<ValidationError>,
) {
    let file_path = ctx.file_path;
    let game = ctx.game;
    let loc_index = ctx.loc_index;

    // F# skip rules: empty keys, keys with spaces (prose / compound), `[...]`
    // inline command blocks, `$VAR$` scripted references, and `@`-vars are not
    // plain key references and are accepted. The `[...]` command may be embedded
    // with a literal suffix/prefix (e.g. a meta_effect variable
    // `"[?ROOT...GetTokenKey]_subtype"`), so test for brackets anywhere — a real
    // loc key never contains them.
    if key_raw.is_empty()
        || key_raw.contains(' ')
        || (key_raw.contains('[') && key_raw.contains(']'))
        || key_raw.contains('$')
        || key_raw.starts_with('@')
    {
        return;
    }

    // No loc data loaded → accept leniently (e.g. vanilla loc absent).
    let Some(idx) = loc_index else {
        return;
    };
    // Loc keys are ASCII in practice, so fold on the stack; anything else falls
    // back to `to_lowercase` so the Unicode special cases still hold.
    let mut lower_buf: smallvec::SmallVec<[u8; 64]> = smallvec::SmallVec::new();
    let lower_owned;
    let key_lower: &str = if key_raw.is_ascii() {
        lower_buf.extend_from_slice(key_raw.as_bytes());
        lower_buf.make_ascii_lowercase();
        std::str::from_utf8(&lower_buf).unwrap_or_default()
    } else {
        lower_owned = key_raw.to_lowercase();
        &lower_owned
    };
    // A key present in the live overlay (just typed into an open `.yml`, not yet
    // rescanned) counts as existing — keeps live editing from flagging a key the
    // user already added. (#36)
    let in_overlay = ctx.extra_loc_keys.is_some_and(|e| e.contains(key_lower));
    let exists = idx.exists_any(key_lower) || in_overlay;

    let push_missing = |errors: &mut Vec<ValidationError>, lang: &str| {
        let code = &error_codes::CW100_MISSING_LOCALISATION;
        errors.push(
            ValidationError::from_code(
                code,
                file_path,
                leaf.pos.start.line,
                leaf.pos.start.col,
                &[key_raw, lang],
            )
            .with_end(leaf.pos.end),
        );
    };

    if opts.is_inline {
        // F# four-way logic for inline loc keys.
        match (opts.was_quoted, exists) {
            (true, true) => {
                let code = &error_codes::CW122_LOC_KEY_IN_INLINE;
                let mut err = ValidationError::from_code(
                    code,
                    file_path,
                    leaf.pos.start.line,
                    leaf.pos.start.col,
                    &[key_raw],
                )
                .with_end(leaf.pos.end);
                if cwtools_parser::parser::is_bare_string_value(key_raw) {
                    err = err.with_fix(cwtools_parser::fix::SuggestedFix::replace(
                        cwtools_i18n::t(cwtools_i18n::Key::ActionRemoveUnnecessaryQuotes),
                        leaf.value_pos,
                        key_raw,
                    ));
                }
                errors.push(err);
            }
            (true, false) => {} // quoted + missing → skip (lenient, matches F#)
            (false, true) => {} // unquoted + exists → ok
            (false, false) => push_missing(errors, "any language"),
        }
    } else if opts.synced && !in_overlay {
        // Must exist in every language the project ships loc data for. A key in
        // the live overlay is accepted leniently (no per-language data there).
        for lang in idx.missing_synced_languages(key_lower) {
            push_missing(errors, &lang.to_string());
        }
    } else if !exists {
        push_missing(errors, "any language");
    }

    // Scope-aware loc-command validation at the reference site: validate the
    // referenced loc string's `[command]` chains against the scope of THIS field.
    if exists && let Some(entry) = idx.entry(key_lower) {
        let initial = scope_context
            .map(|c| c.current())
            .unwrap_or(cwtools_game::scope_engine::SCOPE_ANY);
        // The scripted-variable registry a chain segment is judged against: the
        // config's built-in reads plus every variable the project sets. Withheld
        // while the variable index is empty (an unscanned workspace), which
        // leaves multi-segment chains lenient rather than flagging all of them.
        let ruleset = ctx.ruleset;
        let type_index = ctx.type_index;
        let var_index = type_index.map(|i| &i.var_index).filter(|v| !v.is_empty());
        // Sync: closures capture only &RuleSet / Option<&TypeIndex> (both Sync),
        // not &ValidationCtx (contains RefCell).
        let names_variable = |name: &str| {
            is_builtin_variable(ruleset, name) || var_index.is_some_and(|v| v.contains(name))
        };
        let lookup: cwtools_localization::ScriptedVariables<'_> = &names_variable;
        // Two sources: the ruleset's `scripted_loc` type where its declared path
        // matches (Stellaris), and the path-driven index where it does not (the
        // HOI4 config points that type at Stellaris's folder, #348).
        let names_scripted_loc = move |name: &str| {
            type_index.is_some_and(|ti| {
                ti.contains("scripted_loc", name) || ti.scripted_loc_index.contains(name)
            })
        };
        let loc_lookup: cwtools_localization::ScriptedVariables<'_> = &names_scripted_loc;
        let has_scripted_loc = type_index.is_some_and(|ti| {
            !ti.instances("scripted_loc").is_empty() || !ti.scripted_loc_index.is_empty()
        });
        let names_scripted_gui =
            |name: &str| type_index.is_some_and(|ti| ti.scripted_gui_index.contains(name));
        let gui_lookup: cwtools_localization::ScriptedVariables<'_> = &names_scripted_gui;
        let has_scripted_gui = type_index.is_some_and(|ti| !ti.scripted_gui_index.is_empty());
        let data = cwtools_localization::LocScopeData {
            game,
            terminal_commands: std::borrow::Cow::Borrowed(&ctx.ruleset.localisation_commands),
            registry: scope_context.map(|c| c.registry.clone()),
            scripted_variables: var_index.is_some().then_some(lookup),
            scripted_locs: has_scripted_loc.then_some(loc_lookup),
            scripted_guis: has_scripted_gui.then_some(gui_lookup),
            ..Default::default()
        };
        for diag in cwtools_localization::validate_loc_commands(entry, initial, &data) {
            push_loc_command_diagnostic(
                &diag,
                key_raw,
                leaf,
                file_path,
                scope_context.map(|c| c.registry.as_ref()),
                errors,
            );
        }
    }
}

/// Convert a `LocCommandDiagnostic` (from the loc scope engine) into a
/// `ValidationError` with the matching F# numeric code. The code and message
/// come from the loc crate, shared with the standalone `cwtools loc` pass.
fn push_loc_command_diagnostic(
    diag: &cwtools_localization::LocCommandDiagnostic,
    loc_key: &str,
    leaf: &cwtools_parser::ast::Leaf,
    file_path: &crate::FilePath,
    registry: Option<&ScopeRegistry>,
    errors: &mut Vec<ValidationError>,
) {
    let (code, message) = cwtools_localization::loc_command_parts(diag, loc_key, registry);
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
