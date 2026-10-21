//! HOI4 equipment names the game derives instead of declaring: a variant that
//! is `naval_equip` only through `archetype = <equipment.naval_equip>`, and the
//! renamed copies `duplicate_archetypes` makes of every variant of an archetype.
//! With a complete index, a type-keyed field rejects names the index lacks, so
//! these must resolve or every OOB that uses them reports CW262.

use cwtools_index::TypeIndex;
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{Game, subtype_membership_for_instance};

use super::common::{Opts, error_codes, ruleset, validate_with};

const CWT: &str = r#"
types = {
    type[equipment] = {
        skip_root_key = equipments
        skip_root_key = duplicate_archetypes
        path = "game/common/units/equipment"
        subtype[archetype_equip] = {
            ## cardinality = 0..1
            is_archetype = yes
        }
        subtype[regular_equip] = {
            ## cardinality = 0..1
            is_archetype = no
        }
        subtype[naval_equip] = {
            ## cardinality = 0..1
            type = enum[ship_units]
            ## cardinality = 0..1
            archetype = <equipment.naval_equip>
        }
    }
    type[oob] = {
        path = "game/history/units"
        type_per_file = yes
    }
}

equipment = {
    ## cardinality = 0..1
    is_archetype = bool
    ## cardinality = 0..1
    archetype = <equipment>
    ## cardinality = 0..1
    type = enum[ship_units]
}

oob = {
    ## cardinality = 0..1
    ships = {
        ## cardinality = 0..inf
        <equipment.naval_equip> = { amount = int }
    }
    ## cardinality = 0..1
    variants = {
        ## cardinality = 0..inf
        <equipment.regular_equip> = { amount = int }
    }
}

enums = {
    enum[ship_units] = {
        screen_ship
    }
}
"#;

const EQUIPMENT: &str = r#"
equipments = {
    corvette = {
        is_archetype = yes
        type = screen_ship
    }
    corvette_hull_1 = {
        archetype = corvette
    }
    medium_tank_chassis = {
        is_archetype = yes
    }
    medium_tank_chassis_1 = {
        archetype = medium_tank_chassis
    }
}
"#;

const DUPLICATES: &str = r#"
duplicate_archetypes = {
    medium_tank_artillery_chassis = {
        archetype = medium_tank_chassis
    }
}
"#;

const EQUIPMENT_URI: &str = "file://equipment.txt";

fn build_index(ruleset: &cwtools_rules::rules_types::RuleSet, table: &StringTable) -> TypeIndex {
    let mut idx = TypeIndex::new();
    for (uri, logical, script) in [
        (
            EQUIPMENT_URI,
            "common/units/equipment/equipment.txt",
            EQUIPMENT,
        ),
        (
            "file://x_chassis.txt",
            "common/units/equipment/x_chassis.txt",
            DUPLICATES,
        ),
    ] {
        let parsed = parse_string(script, table);
        let collected = cwtools_index::collect_type_instances_with_subtypes(
            ruleset,
            &parsed,
            logical,
            table,
            subtype_membership_for_instance,
        );
        idx.merge(uri, collected.instances);
        idx.merge(uri, collected.subtype_instances);
        idx.merge_derived(uri, collected.derived);
    }
    idx.complete = true;
    idx
}

fn oob_codes(oob: &str, idx: &TypeIndex, table: &StringTable) -> Vec<String> {
    let (_, ruleset) = ruleset(CWT);
    let errors = validate_with(
        table,
        &ruleset,
        oob,
        &Opts {
            path: "history/units/BEL.txt",
            game: Some(Game::Hoi4),
            type_index: Some(idx),
            ..Opts::DEFAULT
        },
    );
    error_codes(&errors)
}

#[test]
fn variant_inherits_naval_equip_from_its_archetype() {
    let (table, ruleset) = ruleset(CWT);
    let idx = build_index(&ruleset, &table);
    assert!(idx.contains("equipment.naval_equip", "corvette_hull_1"));
    assert!(!idx.contains("equipment.naval_equip", "medium_tank_chassis_1"));
}

#[test]
fn duplicate_archetype_derives_renamed_variants() {
    let (table, ruleset) = ruleset(CWT);
    let idx = build_index(&ruleset, &table);
    assert!(idx.contains("equipment", "medium_tank_artillery_chassis_1"));
    assert!(idx.contains("equipment.regular_equip", "medium_tank_artillery_chassis_1"));
    assert!(!idx.contains("equipment", "medium_tank_artillery_chassis_2"));
}

#[test]
fn derived_names_resolve_as_type_keys() {
    let (table, ruleset) = ruleset(CWT);
    let idx = build_index(&ruleset, &table);
    let oob = r#"
ships = {
    corvette_hull_1 = { amount = 1 }
}
variants = {
    medium_tank_chassis_1 = { amount = 1 }
    medium_tank_artillery_chassis_1 = { amount = 1 }
}
"#;
    assert_eq!(oob_codes(oob, &idx, &table), Vec::<String>::new());
}

#[test]
fn names_the_game_does_not_derive_still_report_cw262() {
    let (table, ruleset) = ruleset(CWT);
    let idx = build_index(&ruleset, &table);
    let oob = r#"
ships = {
    medium_tank_chassis_1 = { amount = 1 }
}
variants = {
    medium_tank_artillery_chassis_9 = { amount = 1 }
}
"#;
    assert_eq!(oob_codes(oob, &idx, &table), vec!["CW262", "CW262"]);
}

#[test]
fn materialized_names_survive_as_plain_instances() {
    let (table, ruleset) = ruleset(CWT);
    let mut idx = build_index(&ruleset, &table);
    idx.materialize_derived();
    let map = idx.into_map();
    let names = |type_name: &str| -> Vec<String> {
        let mut names: Vec<String> = map
            .get(type_name)
            .into_iter()
            .flatten()
            .map(|(_, instance)| instance.name.clone())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names("equipment.naval_equip"),
        ["corvette", "corvette_hull_1"]
    );
    assert!(names("equipment").contains(&"medium_tank_artillery_chassis_1".to_string()));
    assert!(
        names("equipment.regular_equip").contains(&"medium_tank_artillery_chassis_1".to_string())
    );
    assert!(
        !names("equipment.naval_equip").contains(&"medium_tank_artillery_chassis_1".to_string())
    );
}

#[test]
fn removing_the_archetype_file_drops_derived_names() {
    let (table, ruleset) = ruleset(CWT);
    let mut idx = build_index(&ruleset, &table);
    idx.remove_file(EQUIPMENT_URI);
    assert!(!idx.contains("equipment.naval_equip", "corvette_hull_1"));
    assert!(!idx.contains("equipment", "medium_tank_artillery_chassis_1"));
}
