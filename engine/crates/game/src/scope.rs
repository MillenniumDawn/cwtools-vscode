use crate::scope_engine::ScopeId;

/// A game-specific scope definition (name + aliases + subscope relations).
/// `id`/`subscope_of` use [`ScopeId`] — the same scope-id newtype the live
/// engine uses; the const tables in `constants.rs` build these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScopeDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub id: ScopeId,
    /// IDs of scopes this scope is a sub-scope of.
    pub subscope_of: &'static [ScopeId],
}
