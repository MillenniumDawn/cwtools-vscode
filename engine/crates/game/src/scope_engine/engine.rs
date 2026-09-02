// stripped to WHY-only — stack invariants and Windows 8.3/private/var kept in code
use crate::constants::Game;
use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub u32);

pub const SCOPE_ANY: ScopeId = ScopeId(0);
pub const SCOPE_INVALID: ScopeId = ScopeId(1);

#[derive(Debug, Clone, PartialEq)]
pub enum ScopeResult {
    NewScope {
        scope: ScopeId,
        ignore_keys: Vec<String>,
    },
    WrongScope {
        command: String,
        current: ScopeId,
        expected: Vec<ScopeId>,
    },
    VarFound,
    VarNotFound(String),
    ValueFound,
    NotFound,
    AnyScope,
}

#[derive(Debug, Clone)]
pub struct SavedContext {
    pub root: ScopeId,
    pub scopes: SmallVec<[ScopeId; 8]>,
    pub from: SmallVec<[ScopeId; 4]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeContext {
    pub root: ScopeId,
    pub scopes: Vec<ScopeId>,
    pub from: Vec<ScopeId>,
    pub registry: std::sync::Arc<crate::scope_registry::ScopeRegistry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeLink {
    pub valid_scopes: Vec<ScopeId>,
    pub target: Option<ScopeId>,
    pub ignore_keys: Vec<String>,
}

impl ScopeContext {
    pub fn new(_game: Game, root: ScopeId) -> Self {
        Self::from_registry(
            std::sync::Arc::new(crate::scope_registry::ScopeRegistry::default()),
            root,
        )
    }

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

    pub fn current(&self) -> ScopeId {
        debug_assert!(!self.scopes.is_empty(), "scope stack must never be empty");
        self.scopes.last().copied().unwrap_or(self.root)
    }

    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    pub fn push_scope(&mut self, scope: ScopeId) {
        self.scopes.push(scope);
    }

    pub fn get_from(&self, i: usize) -> ScopeId {
        if i >= 1 && self.from.len() >= i {
            self.from[i - 1]
        } else {
            SCOPE_ANY
        }
    }

    pub fn save(&self) -> SavedContext {
        SavedContext {
            root: self.root,
            scopes: self.scopes.iter().copied().collect(),
            from: self.from.iter().copied().collect(),
        }
    }

    pub fn restore(&mut self, saved: SavedContext) {
        self.root = saved.root;
        self.scopes.clear();
        self.scopes.extend_from_slice(&saved.scopes);
        self.from.clear();
        self.from.extend_from_slice(&saved.from);
    }

    pub fn apply_replace_scope(
        &mut self,
        root: Option<&str>,
        this: Option<&str>,
        froms: &[String],
        prevs: &[String],
    ) {
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
            debug_assert!(!self.scopes.is_empty(), "scope stack must never be empty");
            if let Some(last) = self.scopes.last_mut() {
                *last = t;
            }
        }
        if !from_ids.is_empty() {
            self.from = from_ids;
        }
        if !prev_ids.is_empty() {
            let current = self.scopes.last().copied().unwrap_or(self.root);
            let mut new_scopes = prev_ids;
            new_scopes.push(current);
            self.scopes = new_scopes;
        }
    }

    #[inline]
    pub fn change_scope(&mut self, key: &str) -> ScopeResult {
        let key = match key.get(..7) {
            Some(p) if p.eq_ignore_ascii_case("hidden:") => &key[7..],
            _ => key,
        };

        let lower_owned;
        let lower: &str = if key.bytes().any(|b| b.is_ascii_uppercase()) {
            lower_owned = key.to_ascii_lowercase();
            &lower_owned
        } else {
            key
        };

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

        if lower.starts_with("event_target:")
            || lower.starts_with("parameter:")
            || lower.starts_with("scope:")
            || lower.starts_with('@')
        {
            self.scopes.push(SCOPE_ANY);
            return ScopeResult::AnyScope;
        }

        if key.contains('.') {
            return self.change_scope_dotted(key);
        }

        self.resolve_single_with_lower(key, lower)
    }

    fn change_scope_dotted(&mut self, key: &str) -> ScopeResult {
        let mut segments = key.split('.').peekable();
        let mut last_result = ScopeResult::NotFound;

        while let Some(seg) = segments.next() {
            let is_last = segments.peek().is_none();
            let result = self.resolve_single(seg);
            match &result {
                ScopeResult::NewScope { .. } | ScopeResult::AnyScope => {
                    last_result = result;
                }
                ScopeResult::VarFound | ScopeResult::ValueFound if is_last => {
                    last_result = result;
                    break;
                }
                _ => {
                    return result;
                }
            }
        }
        last_result
    }

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
        if lower.starts_with('@') {
            self.scopes.push(SCOPE_ANY);
            return ScopeResult::VarFound;
        }

        if let Some(result) = self.resolve_meta_keyword(lower) {
            return result;
        }

        if let Some(link) = self.registry.links.get(lower) {
            let current = self.scopes.last().copied().unwrap_or(self.root);

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

    fn resolve_meta_keyword(&mut self, lower: &str) -> Option<ScopeResult> {
        let result = match lower {
            "this" | "self" => {
                let cur = self.scopes.last().copied().unwrap_or(self.root);
                self.scopes.push(cur);
                ScopeResult::NewScope {
                    scope: cur,
                    ignore_keys: vec![],
                }
            }
            "root" => {
                let r = self.root;
                self.scopes.push(r);
                ScopeResult::NewScope {
                    scope: r,
                    ignore_keys: vec![],
                }
            }
            "prev" => self.apply_prev(1),
            "prevprev" | "prev_prev" => self.apply_prev(2),
            "prevprevprev" | "prev_prev_prev" => self.apply_prev(3),
            "prevprevprevprev" | "prev_prev_prev_prev" => self.apply_prev(4),
            "from" => self.apply_from(1),
            "fromfrom" => self.apply_from(2),
            "fromfromfrom" => self.apply_from(3),
            "fromfromfromfrom" => self.apply_from(4),
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

    fn apply_prev(&mut self, hops: usize) -> ScopeResult {
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
