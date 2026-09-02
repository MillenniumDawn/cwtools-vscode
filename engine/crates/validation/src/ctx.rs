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
const INLINE_SCRIPT_EXPANSION_BUDGET: usize = 4_096;

const ALIAS_MEMO_ARM_AFTER: usize = ALIAS_BRANCH_BUDGET / 2;

const ALIAS_MEMO_ENTRIES: usize = 4_096;

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

    fn spent(&self) -> usize {
        ALIAS_BRANCH_BUDGET - self.remaining
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InlineScriptExpansionBudgetExhaustion {
    pub(crate) pos: SourcePos,
    pub(crate) end: Option<SourcePos>,
}

pub(crate) struct InlineScriptExpansionBudget {
    remaining: usize,
    exhaustion: Option<InlineScriptExpansionBudgetExhaustion>,
    origin: Option<(SourcePos, Option<SourcePos>)>,
}

impl Default for InlineScriptExpansionBudget {
    fn default() -> Self {
        Self {
            remaining: INLINE_SCRIPT_EXPANSION_BUDGET,
            exhaustion: None,
            origin: None,
        }
    }
}

impl InlineScriptExpansionBudget {
    fn reserve(&mut self, pos: SourcePos, end: Option<SourcePos>, nested: bool) -> bool {
        if !nested {
            self.origin = Some((pos, end));
        }
        if self.remaining > 0 {
            self.remaining -= 1;
            true
        } else {
            let (pos, end) = if nested {
                self.origin.unwrap_or((pos, end))
            } else {
                (pos, end)
            };
            self.exhaustion
                .get_or_insert(InlineScriptExpansionBudgetExhaustion { pos, end });
            false
        }
    }
}

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

#[derive(PartialEq, Eq, Hash)]
pub(crate) struct AliasMemoKey {
    category: Box<str>,
    key: Box<str>,
    span: (u32, u16, u32, u16),
    scope: Option<ScopeKey>,
    loop_vars: Box<[Box<str>]>,
}

struct AliasMemoEntry {
    errors: Vec<crate::common::ValidationError>,
}

#[derive(Default)]
pub(crate) struct AliasMemo {
    entries: FxHashMap<AliasMemoKey, AliasMemoEntry>,
}

pub(crate) struct ValidationCtx<'a> {
    pub(crate) ast: &'a ParsedFile,
    pub(crate) ruleset: &'a RuleSet,
    pub(crate) table: &'a StringTable,
    pub(crate) file_path: &'a crate::common::FilePath,
    pub(crate) game: Option<Game>,
    pub(crate) type_index: Option<&'a cwtools_index::TypeIndex>,
    pub(crate) modifier_keys: Option<&'a HashSet<String>>,
    pub(crate) loc_index: Option<&'a LocIndex>,
    pub(crate) extra_loc_keys: Option<&'a HashSet<String>>,
    pub(crate) inline_scripts: Option<&'a crate::inline_script::InlineScripts>,
    pub(crate) scope_checks: bool,
    pub(crate) var_checks: bool,
    pub(crate) loop_vars: RefCell<Vec<String>>,
    pub(crate) alias_branch_budget: &'a RefCell<AliasBranchBudget>,
    pub(crate) inline_script_expansion_budget: &'a RefCell<InlineScriptExpansionBudget>,
    pub(crate) inline_stack: &'a RefCell<Vec<String>>,
    pub(crate) alias_memo: RefCell<AliasMemo>,
    pub(crate) type_uses: Option<&'a RefCell<crate::references::UsedInstances>>,
}

impl<'a> ValidationCtx<'a> {
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
            inline_script_expansion_budget: self.inline_script_expansion_budget,
            inline_stack: self.inline_stack,
            alias_memo: RefCell::new(AliasMemo::default()),
            type_uses: self.type_uses,
        }
    }

    pub(crate) fn tracks_type_uses(&self, type_name: &str) -> bool {
        self.type_uses.is_some()
            && crate::references::is_tracked(self.ruleset, self.game, type_name)
    }

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

    pub(crate) fn alias_branches_evaluated(&self) -> usize {
        self.alias_branch_budget.borrow().spent()
    }

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

    pub(crate) fn reserve_inline_script_expansion(
        &self,
        pos: SourcePos,
        end: Option<SourcePos>,
    ) -> bool {
        let nested = !self.inline_stack.borrow().is_empty();
        self.inline_script_expansion_budget
            .borrow_mut()
            .reserve(pos, end, nested)
    }

    pub(crate) fn inline_script_expansion_budget_exhaustion(
        &self,
    ) -> Option<InlineScriptExpansionBudgetExhaustion> {
        self.inline_script_expansion_budget.borrow().exhaustion
    }

    pub(crate) fn inline_script_expansion_budget_exhausted(&self) -> bool {
        self.inline_script_expansion_budget_exhaustion().is_some()
    }

    pub(crate) fn alias_branch_budget_exhaustion(&self) -> Option<AliasBranchBudgetExhaustion> {
        self.alias_branch_budget.borrow().exhaustion
    }

    pub(crate) fn alias_branch_budget_exhausted(&self) -> bool {
        self.alias_branch_budget_exhaustion().is_some()
    }

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
