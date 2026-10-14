//! Workspaces the driver and `cwtools_lsp` tests both build: the CW239
//! unused-instance scenario and the alias branch-limit (CW277) scenario.

use std::path::Path;

use tempfile::TempDir;

/// Two types: `thing`, whose instances are expected to be referenced, and
/// `user`, which references one. `{SHOULD_BE_USED}` is filled by [`unused_rules`].
const UNUSED_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        {SHOULD_BE_USED}
    }
    type[user] = { path = "game/common/users" }
}
thing = { x = scalar }
user = { uses = <thing> }
"#;

/// Two `thing` instances, only `used_thing` of which [`UNUSED_USERS`] references.
pub const UNUSED_THINGS: &str = "used_thing = { x = a }\nlone_thing = { x = b }\n";

/// A `user` that references `used_thing` and nothing else.
pub const UNUSED_USERS: &str = "a_user = { uses = used_thing }\n";

/// The rules for the unused-instance scenario. `armed` makes `thing` a
/// `should_be_used` type; disarmed, the same mod must report nothing.
pub fn unused_rules(armed: bool) -> String {
    UNUSED_RULES.replace(
        "{SHOULD_BE_USED}",
        if armed { "should_be_used = yes" } else { "" },
    )
}

/// A temp workspace holding [`UNUSED_THINGS`] and [`UNUSED_USERS`] under `mod/`
/// and the [`unused_rules`] under `rules/`.
pub fn unused_workspace(armed: bool) -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write_file(&tmp.path().join("rules/things.cwt"), &unused_rules(armed));
    write_file(&tmp.path().join("mod/common/things/x.txt"), UNUSED_THINGS);
    write_file(&tmp.path().join("mod/common/users/u.txt"), UNUSED_USERS);
    tmp
}

/// A `user` whose alias usages can exhaust the branch budget, and nothing else.
pub const ALIAS_BRANCH_LIMIT_RULES: &str = r#"
types = {
    type[user] = { path = "game/common/users" }
}
user = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:needs_int] = int
"#;

/// [`ALIAS_BRANCH_LIMIT_RULES`] plus a `should_be_used` type, so the same capped
/// file also decides whether the unused-instance check can run.
pub const CAPPED_UNUSED_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = { path = "game/common/users" }
}
thing = { x = scalar }
user = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:needs_int] = int
"#;

/// A `common/users` file whose alias usages exhaust the branch budget, preceded
/// by `noise` single-overload value errors. Those cost no budget, so the caller
/// can push the file past the server's per-file diagnostic cap without changing
/// where the cap falls.
///
/// Every usage is its own, so there is nothing for the alias memo to reuse and
/// the budget is what stops the file: one two-overload usage past capacity.
pub fn capped_alias_file(noise: usize) -> String {
    let mut text = String::from("a_user = {\n");
    for _ in 0..noise {
        text.push_str("needs_int = nope\n");
    }
    for _ in 0..32_769 {
        text.push_str("recurse = { }\n");
    }
    text.push_str("}\n");
    text
}

/// A temp workspace under [`CAPPED_UNUSED_RULES`] with one capped file and a
/// neighbour carrying one ordinary error, so a test can check the budget is per
/// file.
pub fn capped_alias_workspace() -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write_file(&tmp.path().join("rules/aliases.cwt"), CAPPED_UNUSED_RULES);
    write_file(
        &tmp.path().join("mod/common/things/x.txt"),
        "my_thing = { x = a }\n",
    );
    write_file(
        &tmp.path().join("mod/common/users/u.txt"),
        &capped_alias_file(0),
    );
    write_file(
        &tmp.path().join("mod/common/users/v.txt"),
        "b_user = { needs_int = nope }\n",
    );
    tmp
}

fn write_file(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
