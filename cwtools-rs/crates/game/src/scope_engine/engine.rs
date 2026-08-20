use crate::constants::Game;
use smallvec::SmallVec;

/// Opaque scope id — a thin `u32` newtype identifying a scope (country, state,
/// character, …). Used by both the live engine and the const scope tables in
/// `constants.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub u32);

/// ANY scope sentinel — matches any scope check.
pub const SCOPE_ANY: ScopeId = ScopeId(0);
/// INVALID scope sentinel — used for `none` contexts.
pub const SCOPE_INVALID: ScopeId = ScopeId(1);

// ── ScopeResult ──────────────────────────────────────────────────────────────

/// Result of resolving a scope-change command against a `ScopeContext`.
///
/// Mirrors F# `ScopeResult` (Scopes.fs:82-88) with the addition of
/// richer payload fields the Rust validation layer needs.
#[derive(Debug, Clone, PartialEq)]
pub enum ScopeResult {
    /// The command is a valid scope-change and has already been applied to the
    /// context.  `ignore_keys` lists child keys that should not be validated
    /// inside the resulting block (matches F# `ignoreKeys`).
    NewScope {
        scope: ScopeId,
        ignore_keys: Vec<String>,
    },
    /// Command exists but the current scope does not satisfy it.
    WrongScope {
        command: String,
        current: ScopeId,
        expected: Vec<ScopeId>,
    },
    /// Variable / scripted-var reference found (key starts with `@` or
    /// matches a var-prefix).
    VarFound,
    /// Variable reference not found.
    VarNotFound(String),
    /// Value-only trigger (not a scope-changer) was matched at the final
    /// segment of a dotted key — e.g. `has_technology`.
    ValueFound,
    /// Nothing matched — caller should treat the key as an unknown command.
    NotFound,
    /// `event_target:`, `parameter:`, `scope:` or similar prefix that always
    /// produces ANY scope (valid in any context).
    AnyScope,
}

// ── Saved state ──────────────────────────────────────────────────────────────

/// Snapshot of a `ScopeContext` that can be restored after recursing into a
/// child block.  Returned by `save()` / accepted by `restore()`.
///
/// Scope stacks are typically shallow (1–8 entries); SmallVec avoids a heap
/// allocation in the common case.
#[derive(Debug, Clone)]
pub struct SavedContext {
    pub root: ScopeId,
    pub scopes: SmallVec<[ScopeId; 8]>,
    pub from: SmallVec<[ScopeId; 4]>,
}

// ── ScopeContext ─────────────────────────────────────────────────────────────

/// The scope context that is threaded through the AST traversal.
///
/// Mirrors F# `ScopeContext` (Scopes.fs:61-81) but with the richer stack
/// operations needed by the Rust validator.
///
/// * `root`   – scope of the outermost block (e.g. Country for a country event).
/// * `scopes` – stack; `scopes.last()` is the current scope.
/// * `from`   – FROM stack: `from[0]` = FROM, `from[1]` = FROMFROM, etc.
///
/// Scope zero (`SCOPE_ANY`) is the wildcard that passes all checks.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeContext {
    pub root: ScopeId,
    /// Current-scope stack (most-recent last, like F# `Scopes` list head).
    pub scopes: Vec<ScopeId>,
    /// FROM chain: index 0 = FROM, 1 = FROMFROM, 2 = FROMFROMFROM, 3 = FROMFROMFROMFROM.
    pub from: Vec<ScopeId>,
    /// Config-driven scope/link registry (named links, prefixes, scope names).
    /// Shared (cheap to clone for `save`/recursion).
    pub registry: std::sync::Arc<crate::scope_registry::ScopeRegistry>,
}

/// A single named scope-link definition (a scoped effect / one-to-one link).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeLink {
    /// Scopes the command is valid in (empty = valid in all).
    pub valid_scopes: Vec<ScopeId>,
    /// Scope produced by the link.  None = value-only (no scope change), so
    /// `target.is_some()` is exactly "does this link change scope".
    pub target: Option<ScopeId>,
    /// Keys to ignore inside the child block.
    pub ignore_keys: Vec<String>,
}

impl ScopeContext {
    // ── Constructors ────────────────────────────────────────────────────────

    /// Create a fresh context rooted at `root` for the given `game`, using the
    /// hardcoded scope/link tables (Stellaris/EU4/tests; HOI4 is config-driven
    /// via [`Self::from_registry`]).
    pub fn new(game: Game, root: ScopeId) -> Self {
        Self::from_registry(
            std::sync::Arc::new(crate::scope_registry::ScopeRegistry::from_hardcoded(game)),
            root,
        )
    }

    /// Create a context backed by a prebuilt (config-driven) registry.
    pub fn from_registry(
        registry: std::sync::Arc<crate::scope_registry::ScopeRegistry>,
        root: ScopeId,
    ) -> Self {
        Self {
            root,
            scopes: vec![root],
            from: Vec::new(),
            registry,
        }
    }

    // ── Stack accessors ──────────────────────────────────────────────────────

    /// Current active scope (top of stack). The stack is invariantly non-empty
    /// (seeded with `root`; `apply_prev` never pops the last entry), so this
    /// always returns a real scope — `root` is only a defensive fallback.
    pub fn current(&self) -> ScopeId {
        debug_assert!(!self.scopes.is_empty(), "scope stack must never be empty");
        self.scopes.last().copied().unwrap_or(self.root)
    }

    /// Depth of the scope stack. Used by callers to detect whether a
    /// `change_scope` call actually pushed a new scope.
    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Push a new scope onto the stack (used by callers that already resolved
    /// a `NewScope` result and want to record the change).
    pub fn push_scope(&mut self, scope: ScopeId) {
        self.scopes.push(scope);
    }

    /// Return GET_FROM(i) (1-based, matching F# `GetFrom`).
    pub fn get_from(&self, i: usize) -> ScopeId {
        if i >= 1 && self.from.len() >= i {
            self.from[i - 1]
        } else {
            SCOPE_ANY
        }
    }

    // ── Save / restore ───────────────────────────────────────────────────────

    /// Snapshot the mutable parts of the context.
    pub fn save(&self) -> SavedContext {
        SavedContext {
            root: self.root,
            scopes: self.scopes.iter().copied().collect(),
            from: self.from.iter().copied().collect(),
        }
    }

    /// Restore the mutable parts from a snapshot.
    pub fn restore(&mut self, saved: SavedContext) {
        self.root = saved.root;
        self.scopes.clear();
        self.scopes.extend_from_slice(&saved.scopes);
        self.from.clear();
        self.from.extend_from_slice(&saved.from);
    }

    // ── replace_scope ────────────────────────────────────────────────────────

    /// Apply a `replace_scope` block (§ replace_scope in CWT rules).
    ///
    /// `root`, `this` and `from`/`prev` slots are set from the provided
    /// scope name strings, resolved through the game's scope catalog.  Unknown
    /// or absent names are left unchanged.
    pub fn apply_replace_scope(
        &mut self,
        root: Option<&str>,
        this: Option<&str>,
        froms: &[String],
        prevs: &[String],
    ) {
        // Resolve all names up front through the registry (clone the Arc so the
        // closure doesn't borrow `self` while we mutate the scope stack below).
        // The integer fallback supports id literals used in some tests.
        let reg = self.registry.clone();
        let resolve = |name: &str| -> Option<ScopeId> {
            reg.id_of(name)
                .or_else(|| name.trim().parse::<u32>().ok().map(ScopeId))
        };
        let root_id = root.and_then(&resolve);
        let this_id = this.and_then(&resolve);
        let from_ids: Vec<ScopeId> = froms.iter().filter_map(|n| resolve(n)).collect();
        let prev_ids: Vec<ScopeId> = prevs.iter().filter_map(|n| resolve(n)).collect();

        if let Some(r) = root_id {
            self.root = r;
        }
        if let Some(t) = this_id {
            // "this" replaces the current scope (top of stack). The stack is
            // invariantly non-empty, so `last_mut` always succeeds.
            debug_assert!(!self.scopes.is_empty(), "scope stack must never be empty");
            if let Some(last) = self.scopes.last_mut() {
                *last = t;
            }
        }
        if !from_ids.is_empty() {
            self.from = from_ids;
        }
        if !prev_ids.is_empty() {
            // Replace the bottom of the scope stack with the prev chain.
            // Keep the current scope on top.
            let current = self.scopes.last().copied().unwrap_or(self.root);
            let mut new_scopes = prev_ids;
            new_scopes.push(current);
            self.scopes = new_scopes;
        }
    }

    // ── change_scope ─────────────────────────────────────────────────────────

    /// Resolve a single key against the current context.
    ///
    /// Handles:
    /// * Prefixes: `hidden:` (stripped), `event_target:`, `parameter:`,
    ///   `scope:` (Jomini named scope) → `AnyScope`.
    /// * Meta keywords: `this`/`self`, `root`, `prev`/`prevprev`/…,
    ///   `from`/`fromfrom`/…, `root_from`/`root_fromfrom`/….
    /// * Dotted chains: `owner.capital.controller` split and folded.
    /// * Game-specific named links looked up in `scope_links`.
    #[inline]
    pub fn change_scope(&mut self, key: &str) -> ScopeResult {
        // Strip leading `hidden:` prefix (F# Scopes.fs:148-149). Compare
        // case-insensitively without allocating in the common (unprefixed) case.
        let key = match key.get(..7) {
            Some(p) if p.eq_ignore_ascii_case("hidden:") => &key[7..],
            _ => key,
        };

        // Only allocate a lowercase copy when the key actually has uppercase
        // bytes; the common case (already lowercase) borrows `key` directly.
        let lower_owned;
        let lower: &str = if key.bytes().any(|b| b.is_ascii_uppercase()) {
            lower_owned = key.to_ascii_lowercase();
            &lower_owned
        } else {
            key
        };

        // Config-driven prefix links (`var:`, `sp:`, `mio:`, `event_target:`, …).
        // A scope-changing prefix (`sp:` → special_project) pushes its target; a
        // value/data prefix (`var:`, `event_target:`) opens ANY. Every prefix
        // carries its `:` separator, so a key with no `:` can't match any of
        // them — skip the ordered scan entirely in that (common) case.
        if lower.contains(':') {
            for (prefix, link) in &self.registry.prefix_links {
                if lower.starts_with(prefix.as_str()) {
                    if let Some(target) = link.target {
                        self.scopes.push(target);
                        return ScopeResult::NewScope {
                            scope: target,
                            ignore_keys: link.ignore_keys.clone(),
                        };
                    }
                    self.scopes.push(SCOPE_ANY);
                    return ScopeResult::AnyScope;
                }
            }
        }

        // Hardcoded fallback prefixes that always yield AnyScope (F# Scopes.fs:153-164),
        // used when the registry carries no matching prefix link.
        if lower.starts_with("event_target:")
            || lower.starts_with("parameter:")
            || lower.starts_with("scope:")
            || lower.starts_with('@')
        {
            // Push ANY so subsequent dotted segments see an open scope.
            self.scopes.push(SCOPE_ANY);
            return ScopeResult::AnyScope;
        }

        // Dotted key: fold through each segment.
        if key.contains('.') {
            return self.change_scope_dotted(key);
        }

        self.resolve_single_with_lower(key, lower)
    }

    /// Fold a dotted key like `owner.capital.controller` left-to-right.
    fn change_scope_dotted(&mut self, key: &str) -> ScopeResult {
        let mut segments = key.split('.').peekable();
        let mut last_result = ScopeResult::NotFound;

        while let Some(seg) = segments.next() {
            let is_last = segments.peek().is_none();
            let result = self.resolve_single(seg);
            match &result {
                ScopeResult::NewScope { .. } | ScopeResult::AnyScope => {
                    // scope was pushed — continue to next segment
                    last_result = result;
                }
                ScopeResult::VarFound | ScopeResult::ValueFound if is_last => {
                    last_result = result;
                    break;
                }
                _ => {
                    // Any failure short-circuits
                    return result;
                }
            }
        }
        last_result
    }

    /// Resolve a single (non-dotted) key.
    fn resolve_single(&mut self, key: &str) -> ScopeResult {
        let lower_owned;
        let lower: &str = if key.bytes().any(|b| b.is_ascii_uppercase()) {
            lower_owned = key.to_ascii_lowercase();
            &lower_owned
        } else {
            key
        };
        self.resolve_single_with_lower(key, lower)
    }

    #[inline]
    fn resolve_single_with_lower(&mut self, key: &str, lower: &str) -> ScopeResult {
        // Variable / scripted prefix
        if lower.starts_with('@') {
            self.scopes.push(SCOPE_ANY);
            return ScopeResult::VarFound;
        }

        if let Some(result) = self.resolve_meta_keyword(lower) {
            return result;
        }

        // Game-specific named link lookup — borrow rather than clone the whole
        // ScopeLink struct; only clone the sub-Vecs we actually need.
        if let Some(link) = self.registry.links.get(lower) {
            let current = self.scopes.last().copied().unwrap_or(self.root);

            // ANY scope always passes; also check config-driven subscope
            // relations (e.g. character is_subscope_of country) via the
            // registry: "is current a subscope of the required scope?".
            let valid = current == SCOPE_ANY
                || link.valid_scopes.is_empty()
                || link
                    .valid_scopes
                    .iter()
                    .any(|s| self.registry.is_subscope_or_eq(current, *s));

            if valid {
                if let Some(target) = link.target {
                    let ignore_keys = link.ignore_keys.clone();
                    self.scopes.push(target);
                    return ScopeResult::NewScope {
                        scope: target,
                        ignore_keys,
                    };
                } else {
                    // Value-only trigger
                    return ScopeResult::ValueFound;
                }
            } else {
                let expected = link.valid_scopes.clone();
                return ScopeResult::WrongScope {
                    command: key.to_string(),
                    current,
                    expected,
                };
            }
        }

        ScopeResult::NotFound
    }

    /// Resolve a meta keyword (`this`/`self`, `root`, the `prev`/`from` chains,
    /// their `root_from` composites, and the logical/boolean pass-through
    /// keywords). Returns `None` when `lower` is not a meta keyword, leaving the
    /// caller to fall through to the named-link lookup.
    fn resolve_meta_keyword(&mut self, lower: &str) -> Option<ScopeResult> {
        let result = match lower {
            // ── this / self ──────────────────────────────────────────────
            "this" | "self" => {
                let cur = self.scopes.last().copied().unwrap_or(self.root);
                self.scopes.push(cur);
                ScopeResult::NewScope {
                    scope: cur,
                    ignore_keys: vec![],
                }
            }
            // ── root ─────────────────────────────────────────────────────
            "root" => {
                let r = self.root;
                self.scopes.push(r);
                ScopeResult::NewScope {
                    scope: r,
                    ignore_keys: vec![],
                }
            }
            // ── prev chain ───────────────────────────────────────────────
            "prev" => self.apply_prev(1),
            "prevprev" | "prev_prev" => self.apply_prev(2),
            "prevprevprev" | "prev_prev_prev" => self.apply_prev(3),
            "prevprevprevprev" | "prev_prev_prev_prev" => self.apply_prev(4),
            // ── from chain ───────────────────────────────────────────────
            "from" => self.apply_from(1),
            "fromfrom" => self.apply_from(2),
            "fromfromfrom" => self.apply_from(3),
            "fromfromfromfrom" => self.apply_from(4),
            // ── root_from composites ─────────────────────────────────────
            "root_from" => {
                let r = self.root;
                self.scopes.push(r);
                self.apply_from(1)
            }
            "root_fromfrom" => {
                let r = self.root;
                self.scopes.push(r);
                self.apply_from(2)
            }
            "root_fromfromfrom" => {
                let r = self.root;
                self.scopes.push(r);
                self.apply_from(3)
            }
            "root_fromfromfromfrom" => {
                let r = self.root;
                self.scopes.push(r);
                self.apply_from(4)
            }
            // ── logical/boolean keywords (pass-through) ──────────────────
            "and" | "or" | "not" | "nor" | "nand" | "if" | "else" | "else_if" | "hidden_effect"
            | "hidden_trigger" | "limit" | "trigger_if" | "trigger_else" | "trigger_else_if" => {
                let cur = self.scopes.last().copied().unwrap_or(self.root);
                ScopeResult::NewScope {
                    scope: cur,
                    ignore_keys: vec![],
                }
            }
            _ => return None,
        };
        Some(result)
    }

    // ── prev / from helpers ───────────────────────────────────────────────────

    fn apply_prev(&mut self, hops: usize) -> ScopeResult {
        // Pop `hops` levels in place.  The resulting top of stack is the PREV
        // scope; never pop the last entry so the stack stays non-empty.
        for _ in 0..hops {
            if self.scopes.len() > 1 {
                self.scopes.pop();
            }
        }
        let scope = self.scopes.last().copied().unwrap_or(self.root);
        ScopeResult::NewScope {
            scope,
            ignore_keys: vec![],
        }
    }

    fn apply_from(&mut self, i: usize) -> ScopeResult {
        let scope = self.get_from(i);
        self.scopes.push(scope);
        ScopeResult::NewScope {
            scope,
            ignore_keys: vec![],
        }
    }
}
