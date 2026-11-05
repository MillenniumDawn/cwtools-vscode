//! A config that lists its core modifiers only in `logs/modifiers.log`, as the
//! Stellaris one does, still validates them in a modifier block (#1203).

use cwtools_file_manager::file_manager::ScanBudget;
use cwtools_index::TypeIndex;
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::build_modifier_keys;

use super::common::{Opts, validate_with};

const RULES: &str = r#"
types = {
    type[foo] = { path = "game/common/foo" }
    type[static_modifier] = { path = "game/common/static_modifiers" }
}
foo = {
    modifier = {
        alias_name[modifier] = alias_match_left[modifier]
    }
}
alias[modifier:<static_modifier>] = float
alias[modifier:<modifier>] = float
alias[modifier:show_if_not_potential] = yes
"#;

const LOG: &str = "[15:44:08][modifier.cpp:2200]: \n\
                   Printing Modifier Definitions:\n\
                   - core_modifier, Category: Pops\n\
                   - Mixed_Case_Modifier, Category: Planets\n";

#[test]
fn a_core_modifier_from_the_log_validates_and_an_unknown_one_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rules.cwt"), RULES).unwrap();
    std::fs::create_dir(dir.path().join("logs")).unwrap();
    std::fs::write(dir.path().join("logs").join("modifiers.log"), LOG).unwrap();

    let table = StringTable::new();
    let (ruleset, errors) = load_ruleset_from_dir(dir.path(), &table, ScanBudget::default());
    assert!(errors.is_empty(), "{errors:?}");
    let keys = build_modifier_keys(&ruleset, &TypeIndex::new());
    let opts = Opts {
        path: "game/common/foo/test.txt",
        modifier_keys: Some(&keys),
        ..Opts::DEFAULT
    };
    let script = "foo = { modifier = { core_modifier = 0.2 mixed_case_modifier = 0.1 \
                  not_a_modifier = 0.2 } }";

    let unexpected: Vec<String> = validate_with(&table, &ruleset, script, &opts)
        .into_iter()
        .filter(|e| matches!(e.code, Some("CW262" | "CW263")))
        .map(|e| e.message)
        .collect();

    assert_eq!(unexpected, ["Unexpected field 'not_a_modifier'"]);
}
