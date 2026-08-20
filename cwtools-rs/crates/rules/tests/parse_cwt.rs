use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::RootRule;
use cwtools_string_table::string_table::StringTable;

#[test]
fn test_parse_test_cwt() {
    let input = r#"
alias[effect:create_starbase] = {
    ## cardinality = 1..1
    owner = scalar

    ## cardinality = 1..1
    size = scalar
}

types = {
    type[ship_size] = {
        path = "game/common/ship_sizes"
        subtype[starbase] = {
            class = shipclass_starbase
        }
    }
}

enums = {
    enum[shipsize_class] = {
        shipclass_military
        shipclass_starbase
    }
}
"#;

    let table = StringTable::new();
    let parsed = parse_string(input, &table);
    let ruleset = ast_to_ruleset(&parsed, &table);

    assert_eq!(ruleset.aliases.len(), 1); // alias[effect:create_starbase] extracted
    assert_eq!(ruleset.root_rules.len(), 0); // no top-level type rules in this snippet
    assert_eq!(ruleset.types.len(), 1);
    assert!(ruleset.root_rules.is_empty());
    assert_eq!(ruleset.types[0].name, "ship_size");
    assert_eq!(
        ruleset.types[0].path_options.paths,
        vec!["common/ship_sizes"]
    );
    assert_eq!(ruleset.types[0].subtypes.len(), 1);
    assert_eq!(ruleset.types[0].subtypes[0].name, "starbase");

    assert_eq!(ruleset.enums.len(), 1);
    assert_eq!(ruleset.enums[0].key, "shipsize_class");
    assert_eq!(
        ruleset.enums[0].values,
        vec!["shipclass_military", "shipclass_starbase"]
    );
}

#[test]
fn test_parse_real_cwt() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testfiles/configtests/test.cwt"
    );
    let input = std::fs::read_to_string(path).unwrap();
    let table = StringTable::new();
    let parsed = parse_string(&input, &table);
    let ruleset = ast_to_ruleset(&parsed, &table);

    assert!(!ruleset.types.is_empty(), "expected at least one type");
    assert!(!ruleset.enums.is_empty(), "expected at least one enum");

    // Check that we got the ship_size type with the expected path options.
    let ship_size = ruleset
        .types
        .iter()
        .find(|t| t.name == "ship_size")
        .expect("ship_size type should be present");
    assert_eq!(
        ship_size.path_options.paths,
        vec!["common/ship_sizes"],
        "ship_size path should have game/ prefix stripped during conversion"
    );
    assert!(
        !ship_size.path_options.path_strict,
        "ship_size should not be path_strict in this ruleset"
    );

    // Check that we got the shipsize_class enum with the expected members.
    let shipsize_class = ruleset
        .enums
        .iter()
        .find(|e| e.key == "shipsize_class")
        .expect("shipsize_class enum should be present");
    assert!(
        shipsize_class
            .values
            .contains(&"shipclass_military".to_string())
    );
    assert!(
        shipsize_class
            .values
            .contains(&"shipclass_starbase".to_string())
    );
    assert!(
        shipsize_class
            .values
            .contains(&"shipclass_military_station".to_string())
    );

    // Check that several aliases were extracted, including scalar and block forms.
    assert!(
        ruleset
            .aliases
            .iter()
            .any(|(n, _)| n == "effect:create_starbase"),
        "effect:create_starbase alias missing"
    );
    assert!(
        ruleset.aliases.iter().any(|(n, _)| n == "effect:set_name"),
        "effect:set_name alias missing"
    );

    // The ruleset index should have been built, so name lookups work.
    assert!(
        ruleset.type_by_name().contains_key("ship_size"),
        "type_by_name index missing ship_size"
    );
    assert!(
        ruleset.enum_by_name().contains_key("shipsize_class"),
        "enum_by_name index missing shipsize_class"
    );
}

#[test]
fn test_parse_stellaris_ethics() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testfiles/stellarisconfig/ethics.cwt"
    );
    let input = std::fs::read_to_string(path).unwrap();
    let table = StringTable::new();
    let parsed = parse_string(&input, &table);
    let ruleset = ast_to_ruleset(&parsed, &table);

    assert!(!ruleset.types.is_empty(), "expected at least one type");
    let ethos = ruleset
        .types
        .iter()
        .find(|t| t.name == "ethos")
        .expect("ethos type should be present");

    assert_eq!(
        ethos.path_options.paths,
        vec!["common/ethics"],
        "ethos type path should be common/ethics"
    );
    assert_eq!(
        ethos.subtypes.len(),
        2,
        "ethos should have ethic_categories and actual_ethics subtypes"
    );
    assert!(
        ethos.subtypes.iter().any(|s| s.name == "ethic_categories"),
        "ethic_categories subtype missing"
    );
    assert!(
        ethos.subtypes.iter().any(|s| s.name == "actual_ethics"),
        "actual_ethics subtype missing"
    );

    // The ethic_categories subtype should carry a type_key_filter pointing at ethic_categories.
    let ethic_categories = ethos
        .subtypes
        .iter()
        .find(|s| s.name == "ethic_categories")
        .expect("ethic_categories subtype");
    assert_eq!(
        ethic_categories.type_key_filter,
        vec!["ethic_categories".to_string()],
        "ethic_categories subtype should filter on the ethic_categories node key"
    );

    // The type and enum indexes should have been populated.
    assert!(
        ruleset.type_by_name().contains_key("ethos"),
        "type_by_name index missing ethos"
    );

    // The root rule for ethos should have been extracted.
    assert!(
        ruleset
            .root_rules
            .iter()
            .any(|rr| matches!(rr, RootRule::TypeRule(n, _) if n == "ethos")),
        "ethos root rule missing"
    );
}
