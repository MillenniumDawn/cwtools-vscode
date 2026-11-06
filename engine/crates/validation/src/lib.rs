pub use cwtools_game::constants::Game;
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeContext};
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_localization::LocIndex;
use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;
use std::collections::HashSet;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

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
    if ctx.stop_traversal() {
        return;
    }
    let ast = ctx.ast;
    let table = ctx.table;
    let file_path = ctx.file_path;
    let ruleset = ctx.ruleset;
    let candidates = grandchild_candidates_for_wrapper(path_candidates, wrapper_root_key);
    for grandchild in grandchildren {
        if ctx.stop_traversal() {
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
                    _ => continue,
                }
            }
            Child::LeafValue(idx) => {
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
            continue;
        }

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

pub fn build_scope_registry_arc(
    ruleset: &RuleSet,
    game: Option<Game>,
) -> Option<std::sync::Arc<ScopeRegistry>> {
    game.map(|g| std::sync::Arc::new(build_scope_registry(ruleset, g)))
}

pub fn checks_from_env() -> (bool, bool) {
    (
        std::env::var("CWTOOLS_NO_SCOPE_CHECKS").is_err(),
        std::env::var("CWTOOLS_NO_VAR_CHECKS").is_err(),
    )
}

#[derive(Clone, Copy)]
pub struct Prepared<'a> {
    pub ruleset: &'a RuleSet,
    pub table: &'a StringTable,
    pub game: Option<Game>,
    pub type_index: Option<&'a cwtools_index::TypeIndex>,
    pub modifier_keys: Option<&'a HashSet<String>>,
    pub loc_index: Option<&'a LocIndex>,
    pub extra_loc_keys: Option<&'a HashSet<String>>,
    pub inline_scripts: Option<&'a inline_script::InlineScripts>,
    pub registry: Option<&'a std::sync::Arc<ScopeRegistry>>,
    pub scope_checks: bool,
    pub var_checks: bool,
}

/// Cooperative cancellation for one rule-traversal run (#1196): the LSP's
/// debounced validation sets it on supersede or close so the sync traversal
/// inside `block_in_place`, which a task abort cannot reach, stops early.
#[derive(Default)]
pub struct CancelFlag {
    cancelled: AtomicBool,
    #[cfg(test)]
    polls: AtomicUsize,
    #[cfg(test)]
    trip_at_poll: AtomicUsize,
}

impl CancelFlag {
    /// Signals cancellation. Idempotent.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Polled by the traversal at every AST block boundary.
    pub fn is_cancelled(&self) -> bool {
        #[cfg(test)]
        {
            let poll = self.polls.fetch_add(1, Ordering::Relaxed) + 1;
            let trip = self.trip_at_poll.load(Ordering::Relaxed);
            if trip != 0 && poll >= trip {
                self.cancel();
            }
        }
        self.cancelled.load(Ordering::Acquire)
    }

    /// Test-only: cancel from the `n`th poll on, for a deterministic mid-run stop.
    #[cfg(test)]
    pub fn cancel_at_poll(&self, n: usize) {
        self.trip_at_poll.store(n.max(1), Ordering::Relaxed);
    }

    /// Test-only: how many block boundaries the traversal has polled.
    #[cfg(test)]
    pub fn polls(&self) -> usize {
        self.polls.load(Ordering::Relaxed)
    }
}

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
        .unwrap_or(SCOPE_ANY);
    let initial_scope = if scope_agnostic {
        SCOPE_ANY
    } else {
        default_root
    };
    registry.map(|r| ScopeContext::from_registry(std::sync::Arc::clone(r), initial_scope))
}

#[tracing::instrument(skip_all)]
pub fn validate_prepared(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
) -> Vec<ValidationError> {
    validate_prepared_inner(ast, file_path, prepared, None, None).0
}

/// `validate_prepared` for a cancellable run (#1196). `None` means cancelled,
/// which is distinct from a complete run with no errors.
pub fn validate_prepared_cancellable(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
    cancel: &CancelFlag,
) -> Option<Vec<ValidationError>> {
    let (errors, _) = validate_prepared_inner(ast, file_path, prepared, None, Some(cancel));
    (!cancel.is_cancelled()).then_some(errors)
}

pub fn validate_prepared_tracking_uses(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
) -> (Vec<ValidationError>, references::UsedInstances) {
    let used = std::cell::RefCell::new(references::UsedInstances::default());
    let (errors, _) = validate_prepared_inner(ast, file_path, prepared, Some(&used), None);
    (errors, used.into_inner())
}

/// `validate_prepared_tracking_uses` for a cancellable run (#1196). `None`
/// means cancelled: partial errors and tracked uses must not be used.
pub fn validate_prepared_tracking_uses_cancellable(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
    cancel: &CancelFlag,
) -> Option<(Vec<ValidationError>, references::UsedInstances)> {
    let used = std::cell::RefCell::new(references::UsedInstances::default());
    let (errors, _) = validate_prepared_inner(ast, file_path, prepared, Some(&used), Some(cancel));
    (!cancel.is_cancelled()).then_some((errors, used.into_inner()))
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
    if let Some(source) = ctx.alias_branch_budget_inline_script_source() {
        error.message = format!(
            "{} (in {}:{})",
            error.message, source.logical_path, source.line
        );
    }
    errors.push(error);
}

fn append_inline_script_expansion_budget_error(
    ctx: &ValidationCtx,
    errors: &mut Vec<ValidationError>,
) {
    let Some(exhaustion) = ctx.inline_script_expansion_budget_exhaustion() else {
        return;
    };
    let message = inline_script::ExpandError::BudgetExceeded.to_string();
    let mut error = ValidationError::from_code(
        &error_codes::CW274_INLINE_SCRIPT_ERROR,
        ctx.file_path,
        exhaustion.pos.line,
        exhaustion.pos.col,
        &[message.as_str()],
    );
    if let Some(end) = exhaustion.end {
        error = error.with_end(end);
    }
    errors.push(error);
}

fn validate_prepared_inner(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
    type_uses: Option<&std::cell::RefCell<references::UsedInstances>>,
    cancel: Option<&CancelFlag>,
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

    let file_arc: common::FilePath = std::sync::Arc::from(file_path);

    let alias_branch_budget = std::cell::RefCell::new(AliasBranchBudget::default());
    let inline_script_expansion_budget =
        std::cell::RefCell::new(ctx::InlineScriptExpansionBudget::default());
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
        inline_script_expansion_budget: &inline_script_expansion_budget,
        inline_stack: &inline_stack,
        alias_memo: std::cell::RefCell::new(ctx::AliasMemo::default()),
        subtype_merge_memo: std::cell::RefCell::new(ctx::SubtypeMergeMemo::default()),
        type_uses,
        cancel,
    };

    let file_path_lower = file_path.to_lowercase();
    let path_candidates = path_candidates_for_file(&file_path_lower, ruleset);
    let path_type = find_type_from_candidates(&path_candidates, None);

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
        if !ctx.stop_traversal()
            && let Some(g) = game
        {
            errors.extend(per_game::run_game_validators(&ctx, g));
        }
        append_alias_branch_budget_error(&ctx, &mut errors);
        append_inline_script_expansion_budget_error(&ctx, &mut errors);
        return (errors, ctx.alias_branches_evaluated());
    }

    let dispatch = DispatchInput {
        ruleset,
        file_path,
        path_candidates: &path_candidates,
        allow_content_fallback: false,
    };
    for child in &ast.root_children {
        if ctx.stop_traversal() {
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

    if !ctx.stop_traversal()
        && let Some(g) = game
    {
        let game_errors = per_game::run_game_validators(&ctx, g);
        errors.extend(game_errors);
    }

    append_alias_branch_budget_error(&ctx, &mut errors);
    append_inline_script_expansion_budget_error(&ctx, &mut errors);
    let branches = ctx.alias_branches_evaluated();
    (errors, branches)
}

#[cfg(test)]
mod cancel_tests {
    use super::*;
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    const USE_RULES: &str = r#"
types = {
    type[thing] = { path = "game/common/things" should_be_used = yes }
    type[user] = { path = "game/common/users" }
}
thing = { x = scalar }
user = { uses = <thing> }
"#;

    fn use_doc(count: usize) -> String {
        let mut doc = String::new();
        for i in 0..count {
            doc.push_str(&format!("u{i} = {{ uses = t{i} }}\n"));
        }
        doc
    }

    fn marked_names(uses: &references::UsedInstances) -> Vec<String> {
        let mut names: Vec<String> = uses
            .changed_names(&references::UsedInstances::default())
            .into_iter()
            .collect();
        names.sort();
        names
    }

    /// The tracked-use path must stop mid-traversal and never return its
    /// partial use set (#1196).
    #[test]
    fn cancelled_tracked_use_traversal_returns_none_instead_of_partial_uses() {
        const USERS: usize = 10;
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(USE_RULES, &table), &table);
        let parsed = parse_string(&use_doc(USERS), &table);
        let registry = build_scope_registry_arc(&ruleset, None);
        let prepared = Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        };
        let path = "common/users/test.txt";

        let control = CancelFlag::default();
        let (_, control_uses) =
            validate_prepared_tracking_uses_cancellable(&parsed, path, &prepared, &control)
                .expect("an uncancelled run must complete");
        let control_names = marked_names(&control_uses);
        assert_eq!(
            control_names.len(),
            USERS,
            "the control must mark every use"
        );

        let trip = control.polls() / 2;
        assert!(trip > 0, "the traversal must poll the flag");
        let tripped = CancelFlag::default();
        tripped.cancel_at_poll(trip);
        assert!(
            validate_prepared_tracking_uses_cancellable(&parsed, path, &prepared, &tripped)
                .is_none(),
            "a cancelled tracked-use traversal must not return partial success"
        );
        assert!(
            tripped.polls() < control.polls(),
            "the cancelled traversal must exit early: {} vs {}",
            tripped.polls(),
            control.polls()
        );

        // The same trip with a caller-owned sink proves the stop was mid-traversal.
        let sink = std::cell::RefCell::new(references::UsedInstances::default());
        let tripped = CancelFlag::default();
        tripped.cancel_at_poll(trip);
        validate_prepared_inner(&parsed, path, &prepared, Some(&sink), Some(&tripped));
        let partial = marked_names(&sink.borrow());
        assert!(
            !partial.is_empty(),
            "the trip must land after at least one use was marked"
        );
        assert!(
            partial.len() < control_names.len(),
            "the trip must land before the last use: {partial:?} vs {control_names:?}"
        );
    }

    const INLINE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { cost = int }
"#;

    const INLINE_BODY: &str = "bad_field = 4\n";

    fn inline_doc(calls: usize) -> String {
        let mut doc = String::from("foo = {\n    cost = 1\n");
        for _ in 0..calls {
            doc.push_str("    inline_script = { script = one }\n");
        }
        doc.push_str("}\n");
        doc
    }

    /// Polling must reach inside inline-script expansion too (#1196).
    #[test]
    fn cancelled_inline_script_traversal_returns_none_instead_of_partial_results() {
        const CALLS: usize = 8;
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(INLINE_RULES, &table), &table);
        let parsed = parse_string(&inline_doc(CALLS), &table);
        let registry = build_scope_registry_arc(&ruleset, None);
        let mut scripts = inline_script::InlineScripts::default();
        assert!(
            scripts
                .insert(
                    "common/inline_scripts/one.txt",
                    parse_string(INLINE_BODY, &table)
                )
                .is_some()
        );
        let prepared = Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: Some(&scripts),
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        };
        let path = "common/foo/test.txt";

        let control = CancelFlag::default();
        let full = validate_prepared_cancellable(&parsed, path, &prepared, &control)
            .expect("an uncancelled run must complete");
        assert_eq!(
            full.len(),
            CALLS,
            "the control must expand every call and report its body error: {full:?}"
        );
        assert!(control.polls() > 2, "the expanded bodies must poll");

        let trip = control.polls() / 2;
        let tripped = CancelFlag::default();
        tripped.cancel_at_poll(trip);
        assert!(
            validate_prepared_cancellable(&parsed, path, &prepared, &tripped).is_none(),
            "a cancelled inline-script traversal must not return partial success"
        );
        assert!(
            tripped.polls() < control.polls(),
            "the cancelled traversal must exit inside the expansion: {} vs {}",
            tripped.polls(),
            control.polls()
        );
    }

    /// `None` (cancelled) must stay distinct from `Some(vec!)` (clean) (#1196).
    #[test]
    fn cancellation_is_distinct_from_a_clean_result() {
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(INLINE_RULES, &table), &table);
        let parsed = parse_string("foo = { cost = 1 }", &table);
        let prepared = Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: None,
            scope_checks: false,
            var_checks: false,
        };
        let path = "common/foo/test.txt";

        let complete = CancelFlag::default();
        assert_eq!(
            validate_prepared_cancellable(&parsed, path, &prepared, &complete),
            Some(Vec::new()),
            "a clean complete run is an empty success"
        );

        let cancelled = CancelFlag::default();
        cancelled.cancel();
        assert_eq!(
            validate_prepared_cancellable(&parsed, path, &prepared, &cancelled),
            None,
            "a cancelled run must not masquerade as an empty success"
        );
    }
}
