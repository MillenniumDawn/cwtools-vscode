//! Shared immutable validation context.
//!
//! The rule-vs-AST walkers all thread the same bag of per-file context (the
//! parsed AST, the ruleset, the string table, the game, and the optional
//! type/modifier/loc indexes). Bundling it into one borrow struct keeps the
//! recursive signatures small: each call passes `&ValidationCtx` plus only the
//! genuinely per-call varying args (the current node/rules, the mutable
//! `scope_context`, and the `errors` sink).

use cwtools_game::constants::Game;
use cwtools_game::scope_engine::{ScopeContext, ScopeId};
use cwtools_localization::LocIndex;
use cwtools_parser::ast::{Leaf, ParsedFile, SourcePos};
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringTable;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::HashSet;

const ALIAS_BRANCH_BUDGET: usize = 65_536;

/// Branches a file has to spend before the alias memo starts recording. Real
/// script does not revisit a subtree (a valid usage matches its first candidate
/// and the disjunction stops there), so recording from the first usage would be
/// pure overhead on every file in a mod. Half the budget is past anything real: the
/// busiest file in the pinned Kaiserreich corpus spends 31,703 branches, and
/// what is left after arming is far more than a memoized walk needs.
const ALIAS_MEMO_ARM_AFTER: usize = ALIAS_BRANCH_BUDGET / 2;

/// Cap on live memo entries. Past it the memo stops recording rather than
/// evicting: a miss only costs the work the budget already bounds.
const ALIAS_MEMO_ENTRIES: usize = 4_096;

/// Cap on the diagnostics one entry may carry. A subtree that produced more than
/// this is cheaper to revalidate than to keep 4,096 copies of.
const ALIAS_MEMO_ENTRY_ERRORS: usize = 128;

#[derive(Clone, Copy)]
pub(crate) struct AliasBranchBudgetExhaustion {
    pub(crate) pos: SourcePos,
    pub(crate) end: Option<SourcePos>,
}

pub(crate) struct AliasBranchBudget {
    remaining: usize,
    exhaustion: Option<AliasBranchBudgetExhaustion>,
}

impl Default for AliasBranchBudget {
    fn default() -> Self {
        Self {
            remaining: ALIAS_BRANCH_BUDGET,
            exhaustion: None,
        }
    }
}

impl AliasBranchBudget {
    fn reserve(&mut self, branches: usize, pos: SourcePos, end: Option<SourcePos>) -> bool {
        if branches <= self.remaining {
            self.remaining -= branches;
            true
        } else {
            self.exhaustion
                .get_or_insert(AliasBranchBudgetExhaustion { pos, end });
            false
        }
    }

    /// Branches evaluated so far in this file.
    fn spent(&self) -> usize {
        ALIAS_BRANCH_BUDGET - self.remaining
    }
}

/// The scope state an alias usage is validated in. Everything a rule can read
/// off the context: the root, the current-scope stack and the FROM chain. The
/// registry is per-run, not per-usage, so it is not part of the identity.
#[derive(PartialEq, Eq, Hash)]
struct ScopeKey {
    root: ScopeId,
    scopes: SmallVec<[ScopeId; 8]>,
    from: SmallVec<[ScopeId; 4]>,
}

impl ScopeKey {
    fn of(sc: &ScopeContext) -> Self {
        Self {
            root: sc.root,
            scopes: sc.scopes.iter().copied().collect(),
            from: sc.from.iter().copied().collect(),
        }
    }
}

/// Everything about an alias usage that can change what validating it produces.
///
/// The usage itself is identified by its source span rather than by the address
/// of the `Leaf`: spans are unique within a file and, unlike a borrowed pointer,
/// cannot be recycled by a later temporary. `fallback_pos` is not part of it
/// because only the node form (no leaf) reads it, and the node form is not
/// memoized. Everything else the walk reads (the ruleset, the indexes, the
/// check flags) is fixed for the file the memo belongs to.
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct AliasMemoKey {
    category: Box<str>,
    key: Box<str>,
    span: (u32, u16, u32, u16),
    scope: Option<ScopeKey>,
    loop_vars: Box<[Box<str>]>,
}

/// A memoized usage's diagnostics. Diagnostics are the only output a replay has
/// to reproduce: the type uses the walk records (CW239/CW231) go into a per-file
/// sink that only ever grows, and an entry exists because the usage was walked
/// in full once, which is when its uses were recorded. Skipping the repeat
/// cannot lose one.
struct AliasMemoEntry {
    errors: Vec<crate::common::ValidationError>,
}

/// Per-file memo of alias-usage results. Empty and untouched until the file's
/// branch spend arms it.
#[derive(Default)]
pub(crate) struct AliasMemo {
    entries: FxHashMap<AliasMemoKey, AliasMemoEntry>,
}

/// Immutable shared context for one file's validation pass. Holds only borrows,
/// so it is cheap to copy a `&ValidationCtx` into every recursive call.
pub(crate) struct ValidationCtx<'a> {
    pub(crate) ast: &'a ParsedFile,
    pub(crate) ruleset: &'a RuleSet,
    pub(crate) table: &'a StringTable,
    pub(crate) file_path: &'a crate::common::FilePath,
    pub(crate) game: Option<Game>,
    pub(crate) type_index: Option<&'a cwtools_index::TypeIndex>,
    pub(crate) modifier_keys: Option<&'a HashSet<String>>,
    pub(crate) loc_index: Option<&'a LocIndex>,
    /// Extra loc keys to treat as existing, on top of `loc_index` — the LSP's
    /// live overlay of unsaved keys in open `.yml` files, so a key just typed
    /// resolves immediately without waiting for a full rescan (#36). Lowercased,
    /// like the keys the existence checks compare against.
    pub(crate) extra_loc_keys: Option<&'a HashSet<String>>,
    /// The `common/inline_scripts` bodies a call site may pull in. `None` on the
    /// paths that never loaded them (single-file entry points, the LSP), where an
    /// `inline_script` call is accepted unexpanded rather than guessed at.
    pub(crate) inline_scripts: Option<&'a crate::inline_script::InlineScripts>,
    pub(crate) scope_checks: bool,
    pub(crate) var_checks: bool,
    /// Stack of implicit/explicit loop-variable names (normalized) in scope for
    /// the block currently being validated. Loop effects (`for_each_loop`, …)
    /// expose `value`/`index`/`break` temp variables their body can read bare;
    /// entering such a block pushes the names here and leaving truncates them, so
    /// a bare read in the body isn't flagged CW246 without leaking the names to
    /// sibling/parent blocks. The single `ValidationCtx` is shared by `&`, so
    /// this uses interior mutability.
    pub(crate) loop_vars: RefCell<Vec<String>>,
    /// Per-file cap on candidate branches from overloaded aliases. The first
    /// branch over the cap records the one diagnostic emitted after validation.
    /// Borrowed rather than owned so an expanded `inline_script` body spends the
    /// calling file's budget instead of being handed a fresh one.
    pub(crate) alias_branch_budget: &'a RefCell<AliasBranchBudget>,
    /// Lookup names of the `inline_script`s currently being expanded, outermost
    /// first. Shared with every expanded body's context, which is what makes a
    /// cycle or a runaway nesting reportable at the call site that started it.
    pub(crate) inline_stack: &'a RefCell<Vec<String>>,
    /// Per-file memo of alias-usage results, so a subtree reached again in the
    /// same state is replayed instead of revalidated. Armed only once the file
    /// has spent [`ALIAS_MEMO_ARM_AFTER`] branches.
    pub(crate) alias_memo: RefCell<AliasMemo>,
    /// Sink for the project-wide unused check (CW239/CW231): the instances of
    /// reference-tracked types this file uses. `None` on every path that didn't
    /// ask for the tracking, which is all of them but the batch driver's, so the
    /// recording sites cost one branch. Shared by `&` like the rest of the
    /// context, hence the `RefCell`.
    pub(crate) type_uses: Option<&'a RefCell<crate::references::UsedInstances>>,
}

impl<'a> ValidationCtx<'a> {
    /// The context an expanded `inline_script` body is walked in: the same rules,
    /// indexes, file and branch budget as the call site, over the rebuilt body's
    /// own arena.
    ///
    /// The loop-variable stack is copied rather than shared — the body reads the
    /// names in scope where it was called, and anything it pushes belongs to the
    /// blocks inside it. The alias memo starts empty because its keys are source
    /// spans, which only identify a usage within one arena.
    pub(crate) fn for_inline_body<'b>(&'b self, ast: &'b ParsedFile) -> ValidationCtx<'b>
    where
        'a: 'b,
    {
        ValidationCtx {
            ast,
            ruleset: self.ruleset,
            table: self.table,
            file_path: self.file_path,
            game: self.game,
            type_index: self.type_index,
            modifier_keys: self.modifier_keys,
            loc_index: self.loc_index,
            extra_loc_keys: self.extra_loc_keys,
            inline_scripts: self.inline_scripts,
            scope_checks: self.scope_checks,
            var_checks: self.var_checks,
            loop_vars: RefCell::new(self.loop_vars.borrow().clone()),
            alias_branch_budget: self.alias_branch_budget,
            inline_stack: self.inline_stack,
            alias_memo: RefCell::new(AliasMemo::default()),
            type_uses: self.type_uses,
        }
    }

    /// Whether uses of `type_name`'s instances are being recorded this run.
    /// Checked before the affix forms a complex `<type>` reference expands to,
    /// so a run that tracks nothing never builds them.
    pub(crate) fn tracks_type_uses(&self, type_name: &str) -> bool {
        self.type_uses.is_some()
            && crate::references::is_tracked(self.ruleset, self.game, type_name)
    }

    /// Record `instance` as a use of a `type_name` instance.
    pub(crate) fn mark_type_use(&self, type_name: &str, instance: &str) {
        if let Some(sink) = self.type_uses {
            sink.borrow_mut().mark(type_name, instance);
        }
    }

    pub(crate) fn reserve_alias_branches(
        &self,
        branches: usize,
        pos: SourcePos,
        end: Option<SourcePos>,
    ) -> bool {
        let mut budget = self.alias_branch_budget.borrow_mut();
        let was_exhausted = budget.exhaustion.is_some();
        let accepted = budget.reserve(branches, pos, end);
        let newly_exhausted = !accepted && !was_exhausted;
        drop(budget);
        if newly_exhausted {
            self.mark_all_tracked_type_uses();
        }
        accepted
    }

    /// A capped file cannot establish every use, so suppress false unused-instance errors.
    fn mark_all_tracked_type_uses(&self) {
        let (Some(type_index), Some(sink)) = (self.type_index, self.type_uses) else {
            return;
        };
        let mut uses = sink.borrow_mut();
        for type_def in &self.ruleset.types {
            if !crate::references::is_tracked(self.ruleset, self.game, &type_def.name) {
                continue;
            }
            for (_, instance) in type_index.instances(&type_def.name) {
                uses.mark(&type_def.name, &instance.name);
            }
        }
    }

    /// Branches this file has evaluated. The memo test reads it; nothing in the
    /// validator branches on it beyond arming the memo.
    pub(crate) fn alias_branches_evaluated(&self) -> usize {
        self.alias_branch_budget.borrow().spent()
    }

    /// The memo identity of one alias usage, or `None` when this usage is not
    /// memoizable: the memo is still disarmed, or the usage is the node form,
    /// whose children slice the key does not identify.
    pub(crate) fn alias_memo_key(
        &self,
        category: &str,
        key: &str,
        leaf: Option<&Leaf>,
        clause_children: Option<&[cwtools_parser::ast::Child]>,
        scope_context: &Option<ScopeContext>,
    ) -> Option<AliasMemoKey> {
        if self.alias_branch_budget.borrow().spent() < ALIAS_MEMO_ARM_AFTER {
            return None;
        }
        let leaf = leaf.filter(|_| clause_children.is_none())?;
        Some(AliasMemoKey {
            category: category.into(),
            key: key.into(),
            span: (
                leaf.pos.start.line,
                leaf.pos.start.col,
                leaf.pos.end.line,
                leaf.pos.end.col,
            ),
            scope: scope_context.as_ref().map(ScopeKey::of),
            loop_vars: self
                .loop_vars
                .borrow()
                .iter()
                .map(|v| v.as_str().into())
                .collect(),
        })
    }

    /// Replay a memoized result onto `errors`. Returns false when nothing is
    /// memoized for `key`.
    pub(crate) fn alias_memo_replay(
        &self,
        key: &AliasMemoKey,
        errors: &mut Vec<crate::common::ValidationError>,
    ) -> bool {
        let memo = self.alias_memo.borrow();
        let Some(entry) = memo.entries.get(key) else {
            return false;
        };
        errors.extend(entry.errors.iter().cloned());
        true
    }

    /// Memoize one alias usage's result. Over either bound the result is dropped
    /// rather than stored, which only costs the revalidation the branch budget
    /// already bounds.
    pub(crate) fn alias_memo_store(
        &self,
        key: AliasMemoKey,
        errors: &[crate::common::ValidationError],
    ) {
        let mut memo = self.alias_memo.borrow_mut();
        if memo.entries.len() >= ALIAS_MEMO_ENTRIES || errors.len() > ALIAS_MEMO_ENTRY_ERRORS {
            return;
        }
        memo.entries.insert(
            key,
            AliasMemoEntry {
                errors: errors.to_vec(),
            },
        );
    }

    pub(crate) fn alias_branch_budget_exhaustion(&self) -> Option<AliasBranchBudgetExhaustion> {
        self.alias_branch_budget.borrow().exhaustion
    }

    pub(crate) fn alias_branch_budget_exhausted(&self) -> bool {
        self.alias_branch_budget_exhaustion().is_some()
    }

    /// Whether `name`, normalized the same way the variable index is, currently
    /// names a loop-local variable in scope. Normalizes into a reusable
    /// thread-local buffer (like `VarIndex::contains`) instead of allocating a
    /// fresh `String` on every checked variable read.
    pub(crate) fn is_loop_var(&self, name: &str) -> bool {
        thread_local! {
            static NORM_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
        }
        NORM_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            cwtools_index::VarIndex::normalize_into(name, &mut buf);
            self.loop_vars.borrow().iter().any(|v| v == buf.as_str())
        })
    }
}
