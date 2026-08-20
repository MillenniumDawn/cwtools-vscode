use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::validate_ast;

// Regression: several types can share a path AND `skip_root_key = objectTypes`
// (in HOI4 `.gfx` files, `pdxmesh` and `pdxparticle` both live under
// `objectTypes`), distinguished only by `## type_key_filter`. Each grandchild of
// the wrapper must be validated against the type its OWN key selects — not against
// whichever type happened to win the path lookup (pdxparticle here, since its path
// `gfx/entities` is longer than pdxmesh's `gfx`). Before the fix, every `pdxmesh`
// body was validated against the `pdxparticle` rule, so `file`/`animation` read as
// "unexpected" and `type` (required by pdxparticle) read as missing.
const CWT: &str = r#"
types = {
    ## type_key_filter = pdxmesh
    type[pdxmesh] = {
        skip_root_key = objectTypes
        path = "game/gfx"
        path_extension = .gfx
        name_field = "name"
    }
    ## type_key_filter = pdxparticle
    type[pdxparticle] = {
        skip_root_key = objectTypes
        path = "game/gfx/entities"
        path_extension = .gfx
        name_field = "name"
    }
}

pdxmesh = {
    name = scalar
    file = scalar
    ## cardinality = 0..inf
    animation = {
        id = scalar
        type = scalar
    }
}

pdxparticle = {
    name = scalar
    type = scalar
}
"#;

fn ruleset_and_table() -> (cwtools_rules::rules_types::RuleSet, StringTable) {
    let table = StringTable::new();
    let parsed_cwt = parse_string(CWT, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    (ruleset, table)
}

#[test]
fn objecttypes_grandchildren_validate_against_their_own_type() {
    let (ruleset, table) = ruleset_and_table();
    // Both a pdxmesh and a pdxparticle under the same objectTypes wrapper; every
    // field is valid for its respective type.
    let script = r#"
objectTypes = {
    pdxmesh = {
        name = "m"
        file = "gfx/models/x.mesh"
        animation = { id = "idle" type = "x_anim" }
    }
    pdxparticle = {
        name = "p"
        type = "some_particle"
    }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "gfx/entities/test.gfx",
        None,
        None,
        None,
    );
    assert!(
        errors.is_empty(),
        "expected no diagnostics, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn objecttypes_disambiguation_still_flags_real_errors() {
    // Not suppression: a field that belongs to neither type's rule is still caught,
    // and it's caught against the CORRECT type (so the pdxparticle bogus field is
    // flagged in the pdxparticle body, the pdxmesh bogus field in the pdxmesh body).
    let (ruleset, table) = ruleset_and_table();
    let script = r#"
objectTypes = {
    pdxmesh = {
        name = "m"
        file = "gfx/models/x.mesh"
        totally_bogus_mesh_field = 5
    }
    pdxparticle = {
        name = "p"
        type = "some_particle"
        totally_bogus_particle_field = yes
    }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "gfx/entities/test.gfx",
        None,
        None,
        None,
    );
    let msgs: Vec<&String> = errors.iter().map(|e| &e.message).collect();
    assert!(
        msgs.iter().any(|m| m.contains("totally_bogus_mesh_field")),
        "expected the bogus pdxmesh field to be flagged, got: {:?}",
        msgs
    );
    assert!(
        msgs.iter()
            .any(|m| m.contains("totally_bogus_particle_field")),
        "expected the bogus pdxparticle field to be flagged, got: {:?}",
        msgs
    );
}
