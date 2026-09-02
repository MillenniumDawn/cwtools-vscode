use cwtools_game::constants::Game;
use cwtools_index::TypeInstance;
use cwtools_parser::ast::SourcePos;
use cwtools_rules::rules_types::{RuleSet, TypeType};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::ValidationCtx;
use crate::{FilePath, ValidationError};
use cwtools_error_codes as error_codes;

pub(crate) const TECHNOLOGY: &str = "technology";

#[derive(Debug, Default, PartialEq)]
pub struct UsedInstances(FxHashMap<String, FxHashSet<String>>);

impl UsedInstances {
    pub fn mark(&mut self, type_name: &str, instance: &str) {
        let instance = instance.trim().trim_matches('"').trim();
        if instance.is_empty() {
            return;
        }
        let lower = instance.to_ascii_lowercase();
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

    pub fn absorb(&mut self, other: Self) {
        for (type_name, names) in other.0 {
            self.0.entry(type_name).or_default().extend(names);
        }
    }

    pub fn merge_from(&mut self, other: &Self) {
        for (type_name, names) in &other.0 {
            self.0
                .entry(type_name.clone())
                .or_default()
                .extend(names.iter().cloned());
        }
    }

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

pub fn needs_use_tracking(ruleset: &RuleSet, game: Option<Game>) -> bool {
    game == Some(Game::Stellaris) || ruleset.types.iter().any(|td| td.should_be_referenced)
}

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
        let found = unused_techs(
            "tech_a = { modifier = { } }\n\
             tech_b = { prereqfor_desc = { } }\n\
             tech_c = { feature_flags = { x } }\n\
             tech_d = { weight = 0 }\n\
             tech_e = { weight_modifier = { factor = 0 } }\n",
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

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

    #[test]
    fn technology_float_zero_weight_factor_is_exempt() {
        let found = unused_techs("tech_f = { weight_modifier = { factor = 0.0 } }\n");
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn technology_with_a_nonzero_weight_is_not_exempt() {
        let found = unused_techs("tech_g = { weight = 5 weight_modifier = { factor = 2 } }\n");
        assert_eq!(found.len(), 1, "got: {found:?}");
        assert_eq!(found[0].0, "CW231");
    }

    #[test]
    fn technology_is_only_checked_for_stellaris() {
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
        assert!(needs_use_tracking(&RuleSet::new(), Some(Game::Stellaris)));
    }
}
