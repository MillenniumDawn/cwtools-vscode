//! "Object is missing its localisation" check (CW100).
//!
//! A type can declare which loc keys each of its instances must have via a
//! `localisation = { ## required name = "$" … }` block (the `$` is the instance
//! name, with an optional prefix/suffix). For every instance defined in a file
//! this flags any `## required` loc key that no loc file provides, so a modder
//! can see at a glance which objects lack localisation. Mirrors the old cwtools
//! "object has no localisation" warning.

use cwtools_index::{NormalizedPath, TypeInstance, check_path_dir_norm};
use cwtools_parser::fix::SuggestedFix;
use cwtools_rules::rules_types::{RuleSet, TypeDefinition};

use crate::ValidationError;
use cwtools_error_codes as error_codes;

/// Whether a type declares a loc key this check can flag: any `## required`
/// entry that isn't also `## optional`, whether its key comes from the instance
/// name (`prefix$suffix`) or from a child field's value (`explicit_field`).
fn has_required_loc(td: &TypeDefinition) -> bool {
    td.localisation
        .iter()
        .any(|loc| loc.is_required_name_derived() || loc.required_explicit_field().is_some())
}

/// Flag indexed instances whose `## required` localisation keys are not
/// provided by any loc file. `loc_exists(key_lower)` reports whether a
/// (lowercased) loc key exists across the indexed languages. Keys built from
/// the instance name (`prefix$suffix`) are derived here; `explicit_field` keys
/// were read off the child field at index time
/// ([`TypeInstance::required_loc_keys`]), so an instance that omits the field
/// contributes nothing (its absence is the rules' cardinality problem, not a
/// missing translation).
pub fn check_missing_localisation(
    instances: &[(&str, &TypeInstance)],
    logical_path: &str,
    file_path: &crate::FilePath,
    ruleset: &RuleSet,
    loc_exists: impl Fn(&str) -> bool,
) -> Vec<ValidationError> {
    // Only a type whose path covers this file can contribute instances here, so
    // unless one of those declares a `## required` name-derived loc key the whole
    // instance walk is dead work — which is most files (events, gfx, history, …).
    let np = NormalizedPath::new(logical_path);
    let relevant: Vec<&TypeDefinition> = ruleset
        .types
        .iter()
        .filter(|td| has_required_loc(td) && check_path_dir_norm(&td.path_options, &np))
        .collect();
    if relevant.is_empty() {
        return Vec::new();
    }

    let mut errors = Vec::new();

    for td in relevant {
        for &(type_name, inst) in instances {
            if type_name != td.name.as_str() {
                continue;
            }
            let derived = td
                .localisation
                .iter()
                .filter(|loc| loc.is_required_name_derived())
                .map(|loc| loc.derived_key(&inst.name));
            // The explicit-field keys were resolved at collection time, so they
            // are already the literal key the field named.
            let explicit = inst.required_loc_keys.iter().cloned();
            for expected in derived.chain(explicit) {
                if loc_exists(&expected.to_ascii_lowercase()) {
                    continue;
                }
                let fix = SuggestedFix::create_loc_key(
                    cwtools_i18n::format(cwtools_i18n::Key::ActionCreateLoc, &[&expected]),
                    &expected,
                );
                errors.push(
                    ValidationError::from_code(
                        &error_codes::CW100_MISSING_LOCALISATION,
                        file_path,
                        inst.location.line,
                        inst.location.col,
                        &[&expected, &inst.name],
                    )
                    .with_fix(fix),
                );
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_index::collect_type_instances;
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;

    /// A type whose `## required` loc key is taken from a child field's value
    /// rather than from the instance name (F#'s `explicit_field` form).
    const EXPLICIT_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            title = title_key
            ## optional
            flavour = flavour_key
        }
    }
}
thing = { title_key = scalar flavour_key = scalar }
"#;

    fn run_rules_at(
        rules: &str,
        logical_path: &str,
        script: &str,
        has: &[&str],
    ) -> Vec<ValidationError> {
        let table = StringTable::new();
        let parsed_cwt = parse_string(rules, &table);
        let ruleset = ast_to_ruleset(&parsed_cwt, &table);
        let parsed = parse_string(script, &table);
        let present: std::collections::HashSet<String> =
            has.iter().map(|s| s.to_ascii_lowercase()).collect();
        let per_type = collect_type_instances(&ruleset, &parsed, logical_path, &table);
        let instances: Vec<(&str, &TypeInstance)> = per_type
            .iter()
            .flat_map(|(type_name, values)| {
                values
                    .iter()
                    .map(move |instance| (type_name.as_str(), instance))
            })
            .collect();
        check_missing_localisation(
            &instances,
            logical_path,
            &logical_path.into(),
            &ruleset,
            |k| present.contains(k),
        )
    }

    fn run_at(logical_path: &str, script: &str, has: &[&str]) -> Vec<ValidationError> {
        run_rules_at(RULES, logical_path, script, has)
    }

    fn run(script: &str, has: &[&str]) -> Vec<ValidationError> {
        run_at("common/things/test.txt", script, has)
    }

    fn run_explicit(script: &str, has: &[&str]) -> Vec<ValidationError> {
        run_rules_at(EXPLICIT_RULES, "common/things/test.txt", script, has)
    }

    #[test]
    fn flags_instance_missing_required_loc() {
        // `my_thing` has its name loc but not `my_thing_desc`.
        let errs = run("my_thing = { x = yes }\n", &["my_thing"]);
        let msgs: Vec<&str> = errs.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            errs.len(),
            1,
            "expected one missing-loc warning, got: {:?}",
            msgs
        );
        assert!(errs[0].message.contains("my_thing_desc"), "got: {:?}", msgs);
        assert_eq!(errs[0].code, Some("CW100"));

        // The fix carries the missing key for the LSP's "create localisation
        // key" action, not a span edit — there's no existing text to replace.
        let fix = errs[0].fix.as_ref().expect("CW100 carries a fix");
        assert_eq!(fix.create_loc_key.as_deref(), Some("my_thing_desc"));
        assert!(fix.edits.is_empty());
        assert_eq!(fix.title, "Create localisation key my_thing_desc");
    }

    #[test]
    fn clean_when_no_loc_bearing_type_owns_the_path() {
        // No type declaring a required loc key covers `events/`, so the file is
        // skipped whole — same instance name, nothing flagged.
        let errs = run_at("events/test.txt", "my_thing = { x = yes }\n", &[]);
        assert!(
            errs.is_empty(),
            "got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn clean_when_all_required_loc_present() {
        let errs = run("my_thing = { x = yes }\n", &["my_thing", "my_thing_desc"]);
        assert!(
            errs.is_empty(),
            "got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn flags_explicit_field_required_loc() {
        let errs = run_explicit(
            "my_thing = { title_key = MY_TITLE flavour_key = MY_FLAVOUR }\n",
            &[],
        );
        let msgs: Vec<&str> = errs.iter().map(|e| e.message.as_str()).collect();
        // Only the `## required` entry is flagged; the `## optional` sibling and
        // the instance name itself are not loc keys of this type at all.
        assert_eq!(errs.len(), 1, "got: {:?}", msgs);
        assert!(errs[0].message.contains("MY_TITLE"), "got: {:?}", msgs);
        assert_eq!(errs[0].code, Some("CW100"));
        let fix = errs[0].fix.as_ref().expect("CW100 carries a fix");
        assert_eq!(fix.create_loc_key.as_deref(), Some("MY_TITLE"));
    }

    #[test]
    fn clean_when_explicit_field_loc_present() {
        let errs = run_explicit("my_thing = { title_key = MY_TITLE }\n", &["my_title"]);
        assert!(
            errs.is_empty(),
            "got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn clean_when_explicit_field_is_absent() {
        // Nothing names a key, so there is nothing to look up. A missing
        // `title_key` is the rules' cardinality complaint, not CW100's.
        let errs = run_explicit("my_thing = { flavour_key = MY_FLAVOUR }\n", &[]);
        assert!(
            errs.is_empty(),
            "got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}
