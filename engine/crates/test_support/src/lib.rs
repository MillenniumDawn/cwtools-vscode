//! Helpers the engine's tests and benches share, so no crate keeps its own copy.
//! A dev-dependency only, on no engine crate.
//!
//! - `rules`: the pinned upstream Stellaris and HOI4 rules, fetched on first use.
//! - `paths`: the Windows-safe absolute path, the `testfiles` fixture root, and
//!   the sibling checkouts the benches read.
//! - `scratch`: the environment that keeps a spawned binary out of the
//!   developer's home.
//! - `fixtures`: the CW239 and alias branch-limit workspaces.

mod fixtures;
mod paths;
mod rules;
mod scratch;

pub use fixtures::{
    ALIAS_BRANCH_LIMIT_RULES, CAPPED_UNUSED_RULES, UNUSED_THINGS, UNUSED_USERS, capped_alias_file,
    capped_alias_workspace, unused_rules, unused_workspace,
};
pub use paths::{abs, abs_path, projects_dir, rules_dir, testfiles};
pub use rules::{hoi4_config, stellaris_config};
pub use scratch::scratch_home_env;
