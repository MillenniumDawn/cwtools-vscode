pub mod constants;
pub mod scope;
pub mod scope_engine;
pub mod scope_registry;

// Re-export the most-used public types at the crate root for convenience.
pub use constants::Game;
pub use scope::ScopeDef;
pub use scope_engine::{SCOPE_ANY, SCOPE_INVALID, ScopeId, ScopeLink};
pub use scope_registry::{ScopeDefOwned, ScopeRegistry};
