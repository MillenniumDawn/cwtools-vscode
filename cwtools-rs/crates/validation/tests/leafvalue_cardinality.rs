use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::validate_ast;

// Regression: alternative leafvalue rules in one block must be counted
// independently (mirrors F# RuleValidationService.checkCardinality, which uses
// Seq.sumBy per rule). A value matching an earlier (permissive) alternative
// must not "starve" a later one. Here `destroyer`/`cruiser` are members of
// enum[ship_units], so the enum rule's count is 2 and it must NOT report
// "appears 0 time(s)".
#[test]
fn leafvalue_alternatives_counted_independently() {
    let cwt = r#"
ship_name = {
    type = scalar
    ## cardinality = 0..1
    ship_types = {
        ## cardinality = ~1..inf
        <unit.ship_unit>
        ## cardinality = ~1..inf
        enum[ship_units]
    }
}

types = {
    type[ship_name] = {
        path = "game/common/units/names_ships"
    }
    type[unit] = {
        path = "game/common/units"
        subtype[ship_unit] = {}
    }
}

enums = {
    enum[ship_units] = {
        destroyer cruiser
    }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let script = r#"
ship_name = {
    type = ship
    ship_types = {
        destroyer
        cruiser
    }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None);
    let enum_card_fp = errors
        .iter()
        .any(|e| e.message.contains("Enum(\"ship_units\")") && e.message.contains("appears 0"));
    assert!(
        !enum_card_fp,
        "false positive: enum[ship_units] reported 0 despite members present. Errors: {:?}",
        errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
}
