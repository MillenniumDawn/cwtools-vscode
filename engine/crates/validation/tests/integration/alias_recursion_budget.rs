use cwtools_validation::ErrorSeverity;

use super::common::{Opts, validate};

const FOO_FILE: Opts<'static> = Opts {
    path: "game/common/foo/test.txt",
    ..Opts::DEFAULT
};

const UNITS_FILE: Opts<'static> = Opts {
    path: "history/units/test.txt",
    ..Opts::DEFAULT
};

const RECURSIVE_ALIAS_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

const RECURSIVE_DISTINCT_ALIAS_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

fn recursive_alias_script(depth: usize) -> String {
    let mut script = String::from("foo = {\n");
    for _ in 0..depth {
        script.push_str("recurse = {\n");
    }
    script.push_str("bad = nope\n");
    for _ in 0..=depth {
        script.push_str("}\n");
    }
    script
}

fn sibling_alias_script(count: usize) -> String {
    let mut script = String::from("foo = {\n");
    for _ in 0..count {
        script.push_str("recurse = { }\n");
    }
    script.push_str("}\n");
    script
}

#[test]
fn duplicate_recursive_aliases_reuse_one_candidate() {
    let errors = validate(
        RECURSIVE_ALIAS_RULES,
        &recursive_alias_script(20),
        &FOO_FILE,
    );
    assert!(
        errors.iter().any(|error| error.code == Some("CW263")),
        "expected the deepest invalid field, got: {errors:?}"
    );
    assert!(
        errors.iter().all(|error| error.code != Some("CW277")),
        "equivalent candidates must not exhaust the branch budget: {errors:?}"
    );
}

#[test]
fn alias_branch_budget_accepts_exact_capacity() {
    // 32,768 two-overload usages reserve exactly the 65,536-branch budget.
    let errors = validate(
        RECURSIVE_DISTINCT_ALIAS_RULES,
        &sibling_alias_script(32_768),
        &FOO_FILE,
    );
    assert!(
        errors.is_empty(),
        "the last fully funded alias usage must remain valid: {errors:?}"
    );
}

/// The budget is the fallback guarantee: distinct usages have nothing to reuse,
/// so 32,769 of them still stop the file one usage past capacity.
#[test]
fn alias_branch_budget_rejects_the_next_usage() {
    let errors = validate(
        RECURSIVE_DISTINCT_ALIAS_RULES,
        &sibling_alias_script(32_769),
        &FOO_FILE,
    );
    assert_eq!(errors.len(), 1, "expected one limit diagnostic: {errors:?}");
    assert_eq!(errors[0].code, Some("CW277"));
    assert_eq!(errors[0].severity, ErrorSeverity::Warning);
    // The usage that could not be funded, squiggling its key.
    assert_eq!((errors[0].line, errors[0].col), (32_770, 0));
    assert_eq!(errors[0].end, Some((32_770, 7)));
}

/// Distinct overloads (only the severity differs, so the equivalent-candidate
/// coalescing cannot collapse them) recurse into the same subtree in the same
/// state at every level. The memo answers the repeat, so a depth that used to
/// spend the whole budget validates to the bottom instead.
#[test]
fn distinct_recursive_aliases_are_memoized_not_capped() {
    let errors = validate(
        RECURSIVE_DISTINCT_ALIAS_RULES,
        &recursive_alias_script(20),
        &FOO_FILE,
    );
    assert!(
        errors.iter().all(|error| error.code != Some("CW277")),
        "an equivalent repeated subtree must not exhaust the budget: {errors:?}"
    );
    assert!(
        errors.iter().any(|error| error.code == Some("CW263")),
        "the deepest invalid field should be reported: {errors:?}"
    );
}

/// The disjunction accepts on the first clean candidate, so a file that is
/// actually valid never opens the second overload and never spends the budget.
/// That is what keeps the cap off legitimate script: depth alone is not what
/// exhausts it, unresolvable depth is.
///
/// Kept to 40 deliberately. The deepest nesting in real content is 24, the
/// parser refuses to descend past `MAX_CLAUSE_DEPTH`, and a debug-build walk
/// this deep is already spending real stack on a 1-2 MiB test thread.
#[test]
fn valid_deep_recursion_is_never_capped() {
    let mut script = String::from("foo = {\n");
    for _ in 0..40 {
        script.push_str("recurse = {\n");
    }
    for _ in 0..=40 {
        script.push_str("}\n");
    }
    let errors = validate(RECURSIVE_DISTINCT_ALIAS_RULES, &script, &FOO_FILE);
    assert!(
        errors.is_empty(),
        "40 levels of valid recursion must validate clean: {errors:?}"
    );
}

const RECURSIVE_PER_FILE_ALIAS_RULES: &str = r#"
types = {
    type[oob] = {
        path = "game/history/units"
        type_per_file = yes
    }
}
oob = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

/// A `type_per_file` file is validated and returned from its own branch, before
/// the root-child dispatch every other cap test goes through. That branch has to
/// report the limit too, or an OOB-shaped file stops validating in silence.
#[test]
fn type_per_file_recursion_stops_at_the_branch_budget() {
    // One usage past capacity, all of them distinct, so there is nothing for the
    // memo to reuse and the budget is what stops the file.
    let mut script = String::new();
    for _ in 0..32_769 {
        script.push_str("recurse = { }\n");
    }
    let errors = validate(RECURSIVE_PER_FILE_ALIAS_RULES, &script, &UNITS_FILE);
    let capped: Vec<_> = errors
        .iter()
        .filter(|error| error.code == Some("CW277"))
        .collect();
    assert_eq!(capped.len(), 1, "expected one limit diagnostic: {errors:?}");
    assert_eq!(capped[0].severity, ErrorSeverity::Warning);
}

/// The same file under a single overload never branches, so it validates to the
/// bottom and reports the invalid field instead of the limit. Without this the
/// test above could pass on a rule set that caps everything.
#[test]
fn type_per_file_recursion_without_overloads_is_not_capped() {
    const SINGLE: &str = r#"
types = {
    type[oob] = {
        path = "game/history/units"
        type_per_file = yes
    }
}
oob = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;
    let mut script = String::new();
    for _ in 0..20 {
        script.push_str("recurse = {\n");
    }
    script.push_str("bad = nope\n");
    for _ in 0..20 {
        script.push_str("}\n");
    }
    let errors = validate(SINGLE, &script, &UNITS_FILE);
    assert!(
        errors.iter().all(|error| error.code != Some("CW277")),
        "a single overload never reserves a branch: {errors:?}"
    );
    assert!(
        errors.iter().any(|error| error.code == Some("CW263")),
        "the deepest invalid field should still be reported: {errors:?}"
    );
}
