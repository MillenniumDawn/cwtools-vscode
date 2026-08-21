//! Project-wide "is this definition ever used?" tracking (CW239, CW231).
//!
//! A type the config marks `should_be_used = yes` expects every instance to be
//! referenced somewhere, and so does a Stellaris technology. Neither can be
//! decided from one file, so the rule engine records every `<type>` reference it
//! resolves into a per-file [`UsedInstances`], the batch driver merges those
//! across the run, and [`check_unused_instances`] flags the definitions nothing
//! used.
//!
//! Recording is off unless the caller asks for it
//! ([`crate::validate_prepared_tracking_uses`]), so the ordinary single-file
//! path pays one branch per `<type>` reference and nothing else.

use cwtools_game::constants::Game;
use cwtools_index::TypeInstance;
use cwtools_parser::ast::SourcePos;
use cwtools_rules::rules_types::{RuleSet, TypeType};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::ValidationCtx;
use crate::{FilePath, ValidationError};
use cwtools_error_codes as error_codes;

/// The Stellaris type whose unused check is CW231 (`Technology {} is not used`)
/// rather than the generic CW239.
pub(crate) const TECHNOLOGY: &str = "technology";

/// Instance names seen used, per type name. Built per file by the validator and
/// merged into one run-wide set by the driver (or, in the LSP, into a per-file
/// store the editor keeps current across edits).
#[derive(Debug, Default, PartialEq)]
pub struct UsedInstances(FxHashMap<String, FxHashSet<String>>);

impl UsedInstances {
    /// Record `instance` as a use of a `type_name` instance. Names are stored
    /// unquoted and lowercased, matching how the type index compares them.
    pub fn mark(&mut self, type_name: &str, instance: &str) {
        let instance = instance.trim().trim_matches('"').trim();
        if instance.is_empty() {
            return;
        }
        let lower = instance.to_ascii_lowercase();
        // Only the first use of a type name pays for an owned key.
        match self.0.get_mut(type_name) {
            Some(names) => {
                names.insert(lower);
            }
            None => {
                self.0
                    .entry(type_name.to_string())
                    .or_default()
                    .insert(lower);
            }
        }
    }

    /// Fold one file's uses into this set.
    pub fn absorb(&mut self, other: Self) {
        for (type_name, names) in other.0 {
            self.0.entry(type_name).or_default().extend(names);
        }
    }

    /// [`Self::absorb`] without consuming the source, for callers merging out of
    /// a store they keep (the LSP's per-file map).
    pub fn merge_from(&mut self, other: &Self) {
        for (type_name, names) in &other.0 {
            self.0
                .entry(type_name.clone())
                .or_default()
                .extend(names.iter().cloned());
        }
    }

    /// The instance names recorded in exactly one of the two sets, across all
    /// types. This is the set of names whose "is it used?" answer may have
    /// changed between two versions of a file, so the LSP scopes its dependent
    /// sweep to the open docs that mention one of them.
    pub fn changed_names(&self, other: &Self) -> FxHashSet<String> {
        let mut out = FxHashSet::default();
        for (a, b) in [(self, other), (other, self)] {
            for (type_name, names) in &a.0 {
                let b_names = b.0.get(type_name);
                for name in names {
                    if !b_names.is_some_and(|n| n.contains(name)) {
                        out.insert(name.clone());
                    }
                }
            }
        }
        out
    }

    fn contains(&self, type_name: &str, instance_lower: &str) -> bool {
        self.0
            .get(type_name)
            .is_some_and(|names| names.contains(instance_lower))
    }
}

/// Whether uses of `type_name`'s instances are recorded. Two sources: a type the
/// config marks `should_be_used` (reported as CW239), and Stellaris technologies
/// (CW231).
pub(crate) fn is_tracked(ruleset: &RuleSet, game: Option<Game>, type_name: &str) -> bool {
    is_tracked_technology(game, type_name)
        || ruleset
            .type_by_name()
            .get(type_name)
            .is_some_and(|&i| ruleset.types[i].should_be_referenced)
}

fn is_tracked_technology(game: Option<Game>, type_name: &str) -> bool {
    game == Some(Game::Stellaris) && type_name == TECHNOLOGY
}

/// Whether this run has anything to track at all. The driver calls this once and
/// skips both the per-file recording and the project-wide pass when it is false,
/// which is every config that declares no `should_be_used` type.
pub fn needs_use_tracking(ruleset: &RuleSet, game: Option<Game>) -> bool {
    game == Some(Game::Stellaris) || ruleset.types.iter().any(|td| td.should_be_referenced)
}

/// Record `value` as a use of the instance a `<type>` field names.
///
/// For a complex `prefix<type>suffix` field the written value and the instance
/// name differ, so all three forms the CW500 lookup accepts are marked — the
/// unused check only needs one of them to match a definition. F# records these
/// as `TypeDefFuzzy` and then ignores them, which reports an instance referenced
/// only through an affixed field as unused; resolving the affixes here is what
/// the rest of the engine already does for CW500.
///
/// A subtype-qualified field (`<equipment.naval_equip>`) is recorded against its
/// base type. The qualifier narrows which instances the reference accepts, but
/// the definition it points at is a plain `equipment`, which is how both the
/// tracking gate and the unused check key their lookups.
pub(crate) fn mark_type_field_use(ctx: &ValidationCtx, field: &TypeType, value: &str) {
    let type_name = field.base_name();
    if !ctx.tracks_type_uses(type_name) {
        return;
    }
    match field {
        TypeType::Simple(_) => ctx.mark_type_use(type_name, value),
        TypeType::Complex { prefix, suffix, .. } => {
            let mut stripped = value;
            if !prefix.is_empty() {
                stripped = stripped.strip_prefix(prefix.as_str()).unwrap_or(stripped);
            }
            if !suffix.is_empty() {
                stripped = stripped.strip_suffix(suffix.as_str()).unwrap_or(stripped);
            }
            ctx.mark_type_use(type_name, stripped);
            ctx.mark_type_use(type_name, value);
            ctx.mark_type_use(type_name, &format!("{prefix}{value}{suffix}"));
        }
    }
}

/// Flag the instances defined in one file that no file in the run used.
///
/// `instances` is that file's own definitions ([`cwtools_index::TypeIndex::instances_in_file`])
/// and `used` the merged run-wide set. Only the mod's files are checked: the run
/// validates nothing from the base game, so its definitions never reach here.
pub fn check_unused_instances(
    ruleset: &RuleSet,
    game: Option<Game>,
    instances: &[(&str, &TypeInstance)],
    used: &UsedInstances,
    file_path: &FilePath,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    for &(type_name, inst) in instances {
        if !is_tracked(ruleset, game, type_name) {
            continue;
        }
        if used.contains(type_name, &inst.name.to_ascii_lowercase()) {
            continue;
        }
        // The whole definition is the complaint (a cleanup deletes all of it),
        // so the squiggle spans it rather than just the key.
        let end = SourcePos {
            line: inst.location.end.0,
            col: inst.location.end.1,
        };
        let error = if is_tracked_technology(game, type_name) {
            ValidationError::from_code(
                &error_codes::CW231_UNUSED_TECH,
                file_path,
                inst.location.line,
                inst.location.col,
                &[&inst.name],
            )
        } else {
            ValidationError::from_code(
                &error_codes::CW239_UNUSED_TYPE,
                file_path,
                inst.location.line,
                inst.location.col,
                &[&inst.name, type_name],
            )
        };
        errors.push(error.with_end(end));
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Prepared, build_scope_registry_arc, validate_prepared_tracking_uses};
    use cwtools_index::{TypeIndex, collect_type_instances};
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = { uses = <thing> }
"#;

    /// Validate `files` as one run and return the CW239/CW231 diagnostics in
    /// file order. Mirrors what the driver does: track uses per file, merge,
    /// then check each file's own definitions.
    fn unused_errors_for(
        game: Option<Game>,
        rules_src: &str,
        files: &[(&str, &str)],
    ) -> Vec<ValidationError> {
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(rules_src, &table), &table);
        let registry = build_scope_registry_arc(&ruleset, game);

        let parsed: Vec<_> = files
            .iter()
            .map(|(path, src)| (*path, parse_string(src, &table)))
            .collect();

        let mut index = TypeIndex::new();
        for (path, ast) in &parsed {
            index.merge(path, collect_type_instances(&ruleset, ast, path, &table));
        }
        index.complete = true;

        let prepared = Prepared {
            ruleset: &ruleset,
            table: &table,
            game,
            type_index: Some(&index),
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        };

        let mut used = UsedInstances::default();
        for (path, ast) in &parsed {
            let (_, file_used) = validate_prepared_tracking_uses(ast, path, &prepared);
            used.absorb(file_used);
        }

        let mut out = Vec::new();
        for (path, _) in &parsed {
            let file_path: FilePath = (*path).into();
            let instances = index.instances_in_file(path);
            out.extend(check_unused_instances(
                &ruleset, game, &instances, &used, &file_path,
            ));
        }
        out
    }

    fn unused_for(
        game: Option<Game>,
        rules_src: &str,
        files: &[(&str, &str)],
    ) -> Vec<(&'static str, String)> {
        unused_errors_for(game, rules_src, files)
            .into_iter()
            .map(|e| (e.code.unwrap(), e.message))
            .collect()
    }

    fn unused_in(rules_src: &str, files: &[(&str, &str)]) -> Vec<(&'static str, String)> {
        unused_for(None, rules_src, files)
    }

    #[test]
    fn unreferenced_should_be_used_instance_is_cw239() {
        let found = unused_in(
            RULES,
            &[
                (
                    "common/things/test.txt",
                    "used_thing = { x = a }\nlone_thing = { x = b }\n",
                ),
                ("common/users/test.txt", "a_user = { uses = used_thing }\n"),
            ],
        );
        assert_eq!(
            found.len(),
            1,
            "only the unreferenced thing should flag, got: {found:?}"
        );
        assert_eq!(found[0].0, "CW239");
        assert!(found[0].1.contains("lone_thing"), "got: {found:?}");
    }

    #[test]
    fn reference_is_case_insensitive() {
        let found = unused_in(
            RULES,
            &[
                ("common/things/test.txt", "MyThing = { x = a }\n"),
                ("common/users/test.txt", "a_user = { uses = mything }\n"),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn type_without_should_be_used_is_never_flagged() {
        // `user` carries no `should_be_used`, and nothing references it.
        let found = unused_in(RULES, &[("common/users/test.txt", "a_user = { }\n")]);
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn affixed_reference_counts_as_a_use() {
        const AFFIX_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = { uses = GFX_<thing>_icon }
"#;
        let found = unused_in(
            AFFIX_RULES,
            &[
                ("common/things/test.txt", "my_thing = { x = a }\n"),
                (
                    "common/users/test.txt",
                    "a_user = { uses = GFX_my_thing_icon }\n",
                ),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// A subtype-qualified reference (`<thing.fancy>`) still points at a plain
    /// `thing` definition, so it has to count the same as a bare `<thing>` one.
    /// The qualifier is not a type of its own: nothing is ever defined under
    /// `thing.fancy`, and recording the use there reported every instance
    /// referenced only this way as unused.
    #[test]
    fn subtype_qualified_reference_counts_as_a_use() {
        const SUBTYPE_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
        subtype[fancy] = {
            fancy = yes
        }
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = {
    ## cardinality = 0..1
    fancy = bool
}
user = { uses = <thing.fancy> }
"#;
        let found = unused_in(
            SUBTYPE_RULES,
            &[
                ("common/things/test.txt", "my_thing = { fancy = yes }\n"),
                ("common/users/test.txt", "a_user = { uses = my_thing }\n"),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// The affixed form carrying a qualifier too (`GFX_<thing.fancy>_icon`).
    /// This is the other arm of the strip: the qualifier comes off the type name
    /// while the affixes still come off the value, and the two must not
    /// interfere.
    #[test]
    fn affixed_subtype_qualified_reference_counts_as_a_use() {
        const AFFIX_SUBTYPE_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
        subtype[fancy] = {
            fancy = yes
        }
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = {
    ## cardinality = 0..1
    fancy = bool
}
user = { uses = GFX_<thing.fancy>_icon }
"#;
        let found = unused_in(
            AFFIX_SUBTYPE_RULES,
            &[
                ("common/things/test.txt", "my_thing = { fancy = yes }\n"),
                (
                    "common/users/test.txt",
                    "a_user = { uses = GFX_my_thing_icon }\n",
                ),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// A qualifier on a rule keyed by `<type>` (`<thing.fancy> = int`). This
    /// records from `rule_core/children.rs` rather than `leaf.rs`, so it is a
    /// separate route to the same recording call.
    #[test]
    fn subtype_qualified_keyed_rule_records_its_key_as_a_use() {
        const KEYED_SUBTYPE_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
        subtype[fancy] = {
            fancy = yes
        }
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = {
    ## cardinality = 0..1
    fancy = bool
}
user = {
    <thing.fancy> = int
}
"#;
        let found = unused_in(
            KEYED_SUBTYPE_RULES,
            &[
                ("common/things/test.txt", "my_thing = { fancy = yes }\n"),
                ("common/users/test.txt", "a_user = { my_thing = 3 }\n"),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// A reference written inside an effect/trigger alias body still counts.
    /// Those leaves reach `validate_leaf` through the alias overload loop
    /// (`rule_core/alias.rs`) rather than the plain child walk, and most real
    /// references live there: if that route stopped recording, everything
    /// referenced only from script would report unused.
    #[test]
    fn alias_routed_reference_counts_as_a_use() {
        const ALIAS_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = {
    alias_name[trigger] = alias_match_left[trigger]
}
alias[trigger:has_thing] = <thing>
"#;
        let found = unused_in(
            ALIAS_RULES,
            &[
                ("common/things/test.txt", "my_thing = { x = a }\n"),
                (
                    "common/users/test.txt",
                    "a_user = { has_thing = my_thing }\n",
                ),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn capped_alias_validation_does_not_emit_unused_errors() {
        const CAPPED_ALIAS_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;
        // One two-overload usage past the 65,536-branch budget. Every usage is
        // its own, so there is nothing to memoize and the budget is what stops
        // the file.
        let mut user = String::from("a_user = {\n");
        for _ in 0..32_769 {
            user.push_str("recurse = { }\n");
        }
        user.push_str("}\n");
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(CAPPED_ALIAS_RULES, &table), &table);
        let parsed = parse_string(&user, &table);
        let errors = crate::validate_ast(
            &parsed,
            &ruleset,
            &table,
            "common/users/test.txt",
            None,
            None,
            None,
        );
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.code == Some("CW277"))
                .count(),
            1,
            "the fixture must reach the alias branch cap: {errors:?}"
        );

        let found = unused_in(
            CAPPED_ALIAS_RULES,
            &[
                ("common/things/test.txt", "my_thing = { x = a }\n"),
                ("common/users/test.txt", &user),
            ],
        );
        assert!(
            found.is_empty(),
            "capped validation must not emit CW239: {found:?}"
        );
    }

    /// A memoized alias subtree still establishes what it references. The memo
    /// replays diagnostics, not type uses, because a use recorded during the walk
    /// that filled an entry is already in the file's sink. If that ever stopped
    /// holding, this file would report its reference as unused.
    #[test]
    fn memoized_alias_validation_still_records_its_uses() {
        const MEMOIZED_ALIAS_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:uses] = <thing>
"#;
        // Twenty levels of two-way recursion around a field nothing accepts: no
        // candidate comes back clean, so every level branches and the memo takes
        // over partway down. The reference sits at the bottom, inside the part
        // of the walk the memo answers for.
        let mut user = String::from("a_user = {\n");
        for _ in 0..20 {
            user.push_str("recurse = {\n");
        }
        user.push_str("uses = my_thing\nbad = nope\n");
        for _ in 0..=20 {
            user.push_str("}\n");
        }
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(MEMOIZED_ALIAS_RULES, &table), &table);
        let parsed = parse_string(&user, &table);
        let errors = crate::validate_ast(
            &parsed,
            &ruleset,
            &table,
            "common/users/test.txt",
            None,
            None,
            None,
        );
        assert!(
            errors.iter().all(|error| error.code != Some("CW277")),
            "the fixture must finish rather than cap, or the blanket suppression \
             would hide a lost use: {errors:?}"
        );

        let found = unused_in(
            MEMOIZED_ALIAS_RULES,
            &[
                ("common/things/test.txt", "my_thing = { x = a }\n"),
                ("common/users/test.txt", &user),
            ],
        );
        assert!(
            found.is_empty(),
            "the memoized walk referenced my_thing: {found:?}"
        );
    }

    /// A rule keyed by `<type>` makes the KEY the reference, not the value.
    #[test]
    fn type_keyed_rule_records_its_key_as_a_use() {
        const KEYED_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = {
    <thing> = int
}
"#;
        let found = unused_in(
            KEYED_RULES,
            &[
                ("common/things/test.txt", "my_thing = { x = a }\n"),
                ("common/users/test.txt", "a_user = { my_thing = 3 }\n"),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn quoted_reference_counts_as_a_use() {
        let found = unused_in(
            RULES,
            &[
                ("common/things/test.txt", "my_thing = { x = a }\n"),
                (
                    "common/users/test.txt",
                    "a_user = { uses = \"my_thing\" }\n",
                ),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// The other direction of the affix case: the instance carries the affixes
    /// and the reference is written bare, so only the prepended form matches.
    #[test]
    fn bare_reference_to_an_affixed_instance_counts_as_a_use() {
        const AFFIX_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = { uses = GFX_<thing>_icon }
"#;
        let found = unused_in(
            AFFIX_RULES,
            &[
                ("common/things/test.txt", "GFX_my_thing_icon = { x = a }\n"),
                ("common/users/test.txt", "a_user = { uses = my_thing }\n"),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// A reference from the same file that holds the definition counts. The
    /// merge is not "every other file", it is every file.
    #[test]
    fn reference_from_the_defining_file_counts_as_a_use() {
        const SELF_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
}
thing = {
    ## cardinality = 0..1
    uses = <thing>
}
"#;
        let found = unused_in(
            SELF_RULES,
            &[(
                "common/things/test.txt",
                "referrer = { uses = target }\ntarget = { }\n",
            )],
        );
        assert_eq!(
            found.len(),
            1,
            "only the referrer itself is unused, got: {found:?}"
        );
        assert!(found[0].1.contains("referrer"), "got: {found:?}");
    }

    /// A `type_per_file` file returns from a separate branch of
    /// `validate_prepared_inner` (the whole file is one instance, so there is no
    /// root-child dispatch). References written inside one still have to record.
    #[test]
    fn reference_inside_a_type_per_file_entity_counts_as_a_use() {
        const PER_FILE_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[oob] = {
        path = "game/history/units"
        type_per_file = yes
    }
}
thing = { }
oob = { uses = <thing> }
"#;
        let found = unused_in(
            PER_FILE_RULES,
            &[
                ("common/things/test.txt", "my_thing = { }\n"),
                ("history/units/my_oob.txt", "uses = my_thing\n"),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// The diagnostic lands on the definition and covers it, rather than
    /// defaulting to a whole-line squiggle at 0,0.
    #[test]
    fn cw239_anchors_at_the_definition_and_spans_it() {
        let errors = unused_errors_for(
            None,
            RULES,
            &[(
                "common/things/test.txt",
                "first = { x = a }\nlone = {\n    x = b\n}\n",
            )],
        );
        let lone = errors
            .iter()
            .find(|e| e.message.contains("lone"))
            .expect("lone should be reported");
        assert_eq!(lone.line, 2, "should anchor at the definition's own line");
        let end = lone.end.expect("the definition's extent should be set");
        assert!(
            end.0 > lone.line,
            "a multi-line definition should span past its first line, got {end:?}"
        );
    }

    // ── Stellaris technology (CW231) ──────────────────────────────────────────

    const TECH_RULES: &str = r#"
types = {
    type[technology] = {
        path = "game/common/technology"
    }
}
technology = {
    ## cardinality = 0..1
    category = { scalar }
    ## cardinality = 0..1
    modifier = { }
    ## cardinality = 0..1
    weight = int
    ## cardinality = 0..1
    prerequisites = {
        ## cardinality = 0..100
        <technology>
    }
}
"#;

    fn unused_techs(script: &str) -> Vec<(&'static str, String)> {
        unused_for(
            Some(Game::Stellaris),
            TECH_RULES,
            &[("common/technology/test.txt", script)],
        )
    }

    #[test]
    fn technology_no_one_requires_is_cw231() {
        let found = unused_techs(
            "tech_root = { weight = 10 }\ntech_leaf = { weight = 10 prerequisites = { tech_root } }\n",
        );
        assert_eq!(found.len(), 1, "got: {found:?}");
        assert_eq!(found[0].0, "CW231");
        assert!(found[0].1.contains("tech_leaf"), "got: {found:?}");
    }

    #[test]
    fn technology_exemptions_keep_cw231_quiet() {
        // F# `validateTechnologies` exempts each of these before reporting.
        let found = unused_techs(
            "tech_a = { modifier = { } }\n\
             tech_b = { prereqfor_desc = { } }\n\
             tech_c = { feature_flags = { x } }\n\
             tech_d = { weight = 0 }\n\
             tech_e = { weight_modifier = { factor = 0 } }\n",
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// Most things that require a technology are not technologies. A reference
    /// from any other file has to count, or every leaf tech reports unused.
    #[test]
    fn technology_referenced_from_another_folder_counts_as_a_use() {
        const CROSS_RULES: &str = r#"
types = {
    type[technology] = {
        path = "game/common/technology"
    }
    type[building] = {
        path = "game/common/buildings"
    }
}
technology = {
    ## cardinality = 0..1
    weight = int
}
building = {
    ## cardinality = 0..1
    prerequisites = {
        ## cardinality = 0..100
        <technology>
    }
}
"#;
        let found = unused_for(
            Some(Game::Stellaris),
            CROSS_RULES,
            &[
                ("common/technology/test.txt", "tech_x = { weight = 10 }\n"),
                (
                    "common/buildings/test.txt",
                    "bld = { prerequisites = { tech_x } }\n",
                ),
            ],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// CW231's half of the subtype-qualifier fix. `is_tracked_technology`
    /// compares against the bare type name, so a prerequisite written
    /// `<technology.tier_one>` was not tracked at all and every technology
    /// required only that way reported unused. The tech nothing requires still
    /// reports, so the fix cannot be silencing the check wholesale.
    #[test]
    fn subtype_qualified_technology_reference_counts_as_a_use() {
        const SUBTYPE_TECH_RULES: &str = r#"
types = {
    type[technology] = {
        path = "game/common/technology"
        subtype[tier_one] = {
            tier = 1
        }
    }
}
technology = {
    ## cardinality = 0..1
    tier = int
    ## cardinality = 0..1
    weight = int
    ## cardinality = 0..1
    prerequisites = {
        ## cardinality = 0..100
        <technology.tier_one>
    }
}
"#;
        let found = unused_for(
            Some(Game::Stellaris),
            SUBTYPE_TECH_RULES,
            &[(
                "common/technology/test.txt",
                "tech_root = { tier = 1 weight = 10 }\n\
                 tech_leaf = { weight = 10 prerequisites = { tech_root } }\n",
            )],
        );
        assert_eq!(found.len(), 1, "only tech_leaf is unused, got: {found:?}");
        assert_eq!(found[0].0, "CW231");
        assert!(found[0].1.contains("tech_leaf"), "got: {found:?}");
    }

    /// F# matches `weight_modifier`'s factor against a decimal zero, so the
    /// float spelling has to exempt the same as the integer one.
    #[test]
    fn technology_float_zero_weight_factor_is_exempt() {
        let found = unused_techs("tech_f = { weight_modifier = { factor = 0.0 } }\n");
        assert!(found.is_empty(), "got: {found:?}");
    }

    /// The exemptions are a "the game uses this itself" list, not a blanket
    /// silence: a technology carrying none of them still reports.
    #[test]
    fn technology_with_a_nonzero_weight_is_not_exempt() {
        let found = unused_techs("tech_g = { weight = 5 weight_modifier = { factor = 2 } }\n");
        assert_eq!(found.len(), 1, "got: {found:?}");
        assert_eq!(found[0].0, "CW231");
    }

    #[test]
    fn technology_is_only_checked_for_stellaris() {
        // The same file under a config that never marks `technology`
        // `should_be_used` reports nothing outside Stellaris.
        let found = unused_for(
            Some(Game::Hoi4),
            TECH_RULES,
            &[(
                "common/technology/test.txt",
                "tech_root = { weight = 10 }\n",
            )],
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn merge_from_matches_absorb() {
        let mut a = UsedInstances::default();
        a.mark("thing", "one");
        let mut b = UsedInstances::default();
        b.mark("thing", "two");
        b.mark("other", "three");

        let mut by_ref = UsedInstances::default();
        by_ref.merge_from(&a);
        by_ref.merge_from(&b);
        let mut by_value = UsedInstances::default();
        by_value.absorb(a);
        by_value.absorb(b);
        assert_eq!(by_ref, by_value);
    }

    #[test]
    fn changed_names_is_the_symmetric_difference() {
        let mut before = UsedInstances::default();
        before.mark("thing", "kept");
        before.mark("thing", "removed");
        let mut after = UsedInstances::default();
        after.mark("thing", "kept");
        after.mark("other", "added");

        let changed = before.changed_names(&after);
        assert_eq!(
            changed,
            FxHashSet::from_iter(["removed".to_string(), "added".to_string()])
        );
        assert!(before.changed_names(&before).is_empty());
    }

    #[test]
    fn needs_use_tracking_follows_the_config() {
        let table = StringTable::new();
        let tracked = ast_to_ruleset(&parse_string(RULES, &table), &table);
        assert!(needs_use_tracking(&tracked, None));
        assert!(!needs_use_tracking(&RuleSet::new(), None));
        // Stellaris always tracks `technology` for CW231, config or not.
        assert!(needs_use_tracking(&RuleSet::new(), Some(Game::Stellaris)));
    }
}
