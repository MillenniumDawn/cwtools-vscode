pub use cwtools_game::constants::Game;
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeContext, ScopeId};
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_localization::LocIndex;
use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;
use std::collections::HashSet;

use cwtools_error_codes as error_codes;

pub mod inline_ignore;
pub mod inline_script;
pub mod missing_loc;
pub mod per_game;
pub mod position;
pub mod references;

mod common;
mod ctx;
mod loc_field;
mod resolve;
mod rule_core;
mod scope;
mod subtype;

pub use common::{ErrorSeverity, FilePath, RelatedSpan, ValidationError, error_hash};
pub use inline_script::InlineScripts;
pub use loc_field::build_modifier_keys;
pub use scope::scope_matches_required;
pub use subtype::{collect_subtype_instances, subtype_membership_for_instance};

use common::{leaf_value_to_string, path_contains_segment};
use ctx::{AliasBranchBudget, ValidationCtx};
use resolve::{
    DispatchInput, PathCandidate, ResolvedType, find_rules_by_name, find_type_from_candidates,
    grandchild_candidates_for_wrapper, path_candidates_for_file, refine_grandchild_type,
    resolve_root_child, type_has_content,
};
use rule_core::validate_with_type;
use scope::build_scope_registry;

/// Iterate grandchildren of a skip_root_key wrapper and validate each one uniformly.
///
/// `skip_tail` is the remaining skip-stack after the level that led here was
/// consumed.  When non-empty each grandchild that matches the next level is
/// itself a skip wrapper and we recurse rather than validate directly (mirrors
/// the indexer's `[head, tail..]` descent in `walk_skip_root_child`).
#[allow(clippy::too_many_arguments)]
fn validate_wrapper_grandchildren(
    ctx: &ValidationCtx,
    grandchildren: &[Child],
    path_candidates: &[PathCandidate],
    type_def: &TypeDefinition,
    wrapper_root_key: &str,
    inner_rules: &[(RuleType, Options)],
    skip_tail: &[SkipRootKey],
    scope_context: &mut Option<ScopeContext>,
    errors: &mut Vec<ValidationError>,
) {
    if ctx.alias_branch_budget_exhausted() {
        return;
    }
    let ast = ctx.ast;
    let table = ctx.table;
    let file_path = ctx.file_path;
    let ruleset = ctx.ruleset;
    let candidates = grandchild_candidates_for_wrapper(path_candidates, wrapper_root_key);
    for grandchild in grandchildren {
        if ctx.alias_branch_budget_exhausted() {
            break;
        }
        let (gc_key, gc_children, gc_pos): (String, &[Child], (u32, u16)) = match grandchild {
            Child::Leaf(gc_idx) => {
                let gc_leaf = &ast.arena.leaves[*gc_idx as usize];
                let pos = (gc_leaf.pos.start.line, gc_leaf.pos.start.col);
                match &gc_leaf.value {
                    Value::Clause(gc_children) => (
                        table.get_string(gc_leaf.key.normal).unwrap_or_default(),
                        gc_children.as_slice(),
                        pos,
                    ),
                    // Non-clause scalar leaf inside wrapper: leave as-is (no error).
                    _ => continue,
                }
            }
            Child::LeafValue(idx) => {
                // Only emit a bare-value error when we are at the instance level
                // (no more skip levels to consume).  Inside a multi-level skip
                // wrapper (e.g. `ideas = { country_ideas = { ... } }`) the
                // grandchildren here are the next skip layer, not bare values.
                if skip_tail.is_empty() {
                    let lv = &ast.arena.leaf_values[*idx as usize];
                    let value = leaf_value_to_string(&lv.value, table);
                    errors.push(
                        ValidationError::from_code(
                            &error_codes::CW264_UNEXPECTED_PROPERTY_LEAF_VALUE,
                            file_path,
                            lv.pos.start.line,
                            lv.pos.start.col,
                            &[&format!("Unexpected bare value '{}'", value)],
                        )
                        .with_end(lv.pos.end),
                    );
                }
                continue;
            }
            _ => continue,
        };

        // If there are more skip levels to consume, check whether this grandchild
        // matches the next level and recurse rather than validate.
        if let [next_level, deeper_tail @ ..] = skip_tail {
            if cwtools_index::skip_root_key_matches(next_level, &gc_key) {
                validate_wrapper_grandchildren(
                    ctx,
                    gc_children,
                    path_candidates,
                    type_def,
                    &gc_key,
                    inner_rules,
                    deeper_tail,
                    scope_context,
                    errors,
                );
            }
            // grandchildren that don't match the next level are silently skipped
            // (they are in a sibling wrapper that doesn't lead to instances of
            // this type).
            continue;
        }

        // At the instance level (skip_tail is empty): validate normally.

        // A wrapper like `objectTypes` can hold instances of several types
        // (pdxmesh, pdxparticle, entity, …) that share a path; pick the type that
        // `## type_key_filter` assigns to THIS grandchild's key rather than
        // validating every grandchild against whichever type won the path lookup.
        let Some((gc_type_def, gc_rules)) =
            refine_grandchild_type(&candidates, &gc_key, type_def, inner_rules, ruleset)
        else {
            continue;
        };

        validate_with_type(
            ctx,
            gc_type_def,
            gc_children,
            gc_rules,
            scope_context,
            Some(&gc_key),
            gc_pos,
            errors,
        );
    }
}

/// Validate a parsed file against the ruleset. Localisation-key checks
/// (CW100/CW122) are skipped; use [`validate_ast_with_loc`] to enable them.
pub fn validate_ast(
    ast: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    file_path: &str,
    game: Option<Game>,
    type_index: Option<&cwtools_index::TypeIndex>,
    modifier_keys: Option<&HashSet<String>>,
) -> Vec<ValidationError> {
    validate_ast_with_loc(
        ast,
        ruleset,
        table,
        file_path,
        game,
        type_index,
        modifier_keys,
        None,
    )
}

/// As [`validate_ast`], but with a loaded [`LocIndex`] so `LocalisationField`
/// references are checked for existence and scope-correct loc commands.
#[tracing::instrument(skip_all)]
#[allow(clippy::too_many_arguments)]
pub fn validate_ast_with_loc(
    ast: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    file_path: &str,
    game: Option<Game>,
    type_index: Option<&cwtools_index::TypeIndex>,
    modifier_keys: Option<&HashSet<String>>,
    loc_index: Option<&LocIndex>,
) -> Vec<ValidationError> {
    // Single-file/test entry point: build the per-run shared state (scope
    // registry) here and delegate. Hot multi-file callers should instead
    // build a `Prepared` ONCE outside their loop and call `validate_prepared`.
    let registry = build_scope_registry_arc(ruleset, game);
    let (scope_checks, var_checks) = checks_from_env();
    validate_prepared(
        ast,
        file_path,
        &Prepared {
            ruleset,
            table,
            game,
            type_index,
            modifier_keys,
            loc_index,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks,
            var_checks,
        },
    )
}

/// Build the config-driven scope/link registry once, wrapped in an `Arc` so it
/// can be shared (cheaply cloned) across every file in a validation run. Returns
/// `None` when no game is set (no scope checks).
pub fn build_scope_registry_arc(
    ruleset: &RuleSet,
    game: Option<Game>,
) -> Option<std::sync::Arc<ScopeRegistry>> {
    game.map(|g| std::sync::Arc::new(build_scope_registry(ruleset, g)))
}

/// Whether the scope checks (CW104/105/106/243/244/245/248) and the "variable has
/// not been set" check (CW246) are on. Both are ON by default; set
/// `CWTOOLS_NO_SCOPE_CHECKS=1` / `CWTOOLS_NO_VAR_CHECKS=1` as escape hatches.
/// Read once at context-construction time.
pub fn checks_from_env() -> (bool, bool) {
    (
        std::env::var("CWTOOLS_NO_SCOPE_CHECKS").is_err(),
        std::env::var("CWTOOLS_NO_VAR_CHECKS").is_err(),
    )
}

/// The per-run shared validation state, built once and reused across every file
/// in a run. Bundles everything [`validate_prepared`] needs beyond the per-file
/// `ast` and `file_path`, so callers pass one value instead of a ten-argument
/// call. All fields are borrows, so it is cheap to copy.
#[derive(Clone, Copy)]
pub struct Prepared<'a> {
    pub ruleset: &'a RuleSet,
    pub table: &'a StringTable,
    pub game: Option<Game>,
    pub type_index: Option<&'a cwtools_index::TypeIndex>,
    pub modifier_keys: Option<&'a HashSet<String>>,
    pub loc_index: Option<&'a LocIndex>,
    /// Extra loc keys to treat as existing (the LSP live overlay of unsaved keys
    /// in open `.yml` files). Lowercased. `None` outside the LSP single-file path.
    pub extra_loc_keys: Option<&'a HashSet<String>>,
    /// The mod's `common/inline_scripts` bodies, so a call site can be checked
    /// against what it actually pulls in. `None` leaves every `inline_script`
    /// call accepted unexpanded.
    pub inline_scripts: Option<&'a inline_script::InlineScripts>,
    pub registry: Option<&'a std::sync::Arc<ScopeRegistry>>,
    pub scope_checks: bool,
    pub var_checks: bool,
}

/// Build the per-file starting scope context — shared by `validate_prepared`
/// and the position resolver so both seed the same root scope.
///
/// Scope-agnostic content is reused from many calling scopes (or operates on a
/// data-dependent element scope), so it can't be pinned to one. Seed ANY so its
/// body isn't scope-checked against an arbitrary default. Everything else starts
/// at the game's primary scope (HOI4 country = 100).
///   - scripted_effects/triggers/localisation: called from any scope.
///   - collections: the `limit`/`operators` run in the input element's scope
///     (`game:all_states` -> state, `game:all_countries` -> country); per the
///     HOI4 collections docs the element scope is data-dependent.
///   - dynamic_modifiers: the `enable`/`remove_trigger` run in the scope the
///     modifier is applied to (country, state, or unit leader; "root is the
///     effect scope" per the HOI4 docs).
pub(crate) fn initial_scope_context(
    file_path: &str,
    registry: Option<&std::sync::Arc<ScopeRegistry>>,
) -> Option<ScopeContext> {
    let clean = file_path.to_ascii_lowercase().replace('\\', "/");
    let scope_agnostic = path_contains_segment(&clean, "scripted_effects")
        || path_contains_segment(&clean, "scripted_triggers")
        || path_contains_segment(&clean, "scripted_localisation")
        || path_contains_segment(&clean, "collections")
        || path_contains_segment(&clean, "dynamic_modifiers");
    let default_root = registry
        .and_then(|r| r.id_of("country"))
        .unwrap_or(ScopeId(100));
    let initial_scope = if scope_agnostic {
        SCOPE_ANY
    } else {
        default_root
    };
    registry.map(|r| ScopeContext::from_registry(std::sync::Arc::clone(r), initial_scope))
}

/// Validate one parsed file against prebuilt per-run state. The hot path: build
/// [`Prepared`] once (scope registry + enum map + indexes) and call this per file
/// instead of rebuilding that state for every file.
#[tracing::instrument(skip_all)]
pub fn validate_prepared(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
) -> Vec<ValidationError> {
    validate_prepared_inner(ast, file_path, prepared, None).0
}

/// As [`validate_prepared`], but also reporting which instances of the
/// reference-tracked types this file uses, for the project-wide unused check
/// (CW239/CW231). The batch driver calls this once per file, merges the sets,
/// and then runs [`references::check_unused_instances`] over each file's own
/// definitions. See [`references`] for why it can't be decided per file.
pub fn validate_prepared_tracking_uses(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
) -> (Vec<ValidationError>, references::UsedInstances) {
    let used = std::cell::RefCell::new(references::UsedInstances::default());
    let (errors, _) = validate_prepared_inner(ast, file_path, prepared, Some(&used));
    (errors, used.into_inner())
}

fn append_alias_branch_budget_error(ctx: &ValidationCtx, errors: &mut Vec<ValidationError>) {
    let Some(exhaustion) = ctx.alias_branch_budget_exhaustion() else {
        return;
    };
    let mut error = ValidationError::from_code(
        &error_codes::CW277_ALIAS_BRANCH_LIMIT,
        ctx.file_path,
        exhaustion.pos.line,
        exhaustion.pos.col,
        &[] as &[&str],
    );
    if let Some(end) = exhaustion.end {
        error = error.with_end(end);
    }
    errors.push(error);
}

/// The body behind both `validate_prepared` entry points. Returns the file's
/// diagnostics and the number of alias branches evaluated producing them — the
/// second is what the memo tests assert on, and no caller acts on it.
fn validate_prepared_inner(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
    type_uses: Option<&std::cell::RefCell<references::UsedInstances>>,
) -> (Vec<ValidationError>, usize) {
    let Prepared {
        ruleset,
        table,
        game,
        type_index,
        modifier_keys,
        loc_index,
        extra_loc_keys,
        inline_scripts,
        registry,
        scope_checks,
        var_checks,
    } = *prepared;
    let mut errors = Vec::new();

    let mut scope_context = initial_scope_context(file_path, registry);

    // Interned once per file; every diagnostic this run emits (including the
    // candidate errors the disjunctions discard) shares it.
    let file_arc: common::FilePath = std::sync::Arc::from(file_path);

    // Owned out here so an expanded inline_script body can borrow the same two
    // and spend the file's budget rather than a fresh one.
    let alias_branch_budget = std::cell::RefCell::new(AliasBranchBudget::default());
    let inline_stack = std::cell::RefCell::new(Vec::new());

    let ctx = ValidationCtx {
        ast,
        ruleset,
        table,
        file_path: &file_arc,
        game,
        type_index,
        modifier_keys,
        loc_index,
        extra_loc_keys,
        inline_scripts,
        scope_checks,
        var_checks,
        loop_vars: std::cell::RefCell::new(Vec::new()),
        alias_branch_budget: &alias_branch_budget,
        inline_stack: &inline_stack,
        alias_memo: std::cell::RefCell::new(ctx::AliasMemo::default()),
        type_uses,
    };

    // Pre-compute path-based type match (most specific wins).
    // Lowercase once and filter path-matching type candidates once per file so
    // the per-child loop only runs key-dependent scoring over the small candidate
    // set rather than scanning all types N_children times.
    let file_path_lower = file_path.to_lowercase();
    let path_candidates = path_candidates_for_file(&file_path_lower, ruleset);
    let path_type = find_type_from_candidates(&path_candidates, None);

    // type_per_file: the WHOLE file is a single instance of this type (e.g. an
    // OOB file). Its root children ARE the instance body — there is no per-entry
    // wrapper key — so validate them once against the type's rules and stop.
    if let Some(td) = path_type
        && td.type_per_file
    {
        let inner_rules = find_rules_by_name(&td.name, ruleset);
        if type_has_content(td, inner_rules) {
            validate_with_type(
                &ctx,
                td,
                &ast.root_children,
                inner_rules,
                &mut scope_context,
                None,
                (0, 0), // type_per_file: whole file is one entity, no single node pos
                &mut errors,
            );
        }
        if !ctx.alias_branch_budget_exhausted()
            && let Some(g) = game
        {
            errors.extend(per_game::run_game_validators(&ctx, g));
        }
        append_alias_branch_budget_error(&ctx, &mut errors);
        return (errors, ctx.alias_branches_evaluated());
    }

    // Resolve each root child's owning type (exact root-key match, then path
    // fallback) via the shared dispatch, then validate accordingly. The navigator
    // (`rules_at_pos`) runs the identical resolution; `allow_content_fallback` is
    // the one place they differ — the validator never content-validates an
    // index-only path match (it skips), so it passes false.
    let dispatch = DispatchInput {
        ruleset,
        file_path,
        path_candidates: &path_candidates,
        allow_content_fallback: false,
    };
    for child in &ast.root_children {
        if ctx.alias_branch_budget_exhausted() {
            break;
        }
        let Child::Leaf(leaf_idx) = child else {
            continue;
        };
        let leaf = &ast.arena.leaves[*leaf_idx as usize];
        let Value::Clause(children) = &leaf.value else {
            continue;
        };
        let root_key = table.get_string(leaf.key.normal).unwrap_or_default();
        match resolve_root_child(&dispatch, &root_key) {
            ResolvedType::Entity {
                type_def,
                inner_rules,
            } => validate_with_type(
                &ctx,
                type_def,
                children.as_slice(),
                inner_rules,
                &mut scope_context,
                Some(&root_key),
                (leaf.pos.start.line, leaf.pos.start.col),
                &mut errors,
            ),
            ResolvedType::Wrapper {
                type_def,
                inner_rules,
                skip_tail,
            } => validate_wrapper_grandchildren(
                &ctx,
                children.as_slice(),
                &path_candidates,
                type_def,
                &root_key,
                inner_rules,
                skip_tail,
                &mut scope_context,
                &mut errors,
            ),
            ResolvedType::None => {}
        }
    }

    // Run game-specific validators if game is provided.
    if !ctx.alias_branch_budget_exhausted()
        && let Some(g) = game
    {
        let game_errors = per_game::run_game_validators(&ctx, g);
        errors.extend(game_errors);
    }

    append_alias_branch_budget_error(&ctx, &mut errors);
    let branches = ctx.alias_branches_evaluated();
    (errors, branches)
}
