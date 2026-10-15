use super::common::{Opts, validate};

const GFX_FILE: Opts<'static> = Opts {
    path: "gfx/entities/test.gfx",
    ..Opts::DEFAULT
};

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

#[test]
fn objecttypes_grandchildren_validate_against_their_own_type() {
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
    let errors = validate(CWT, script, &GFX_FILE);
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
    let errors = validate(CWT, script, &GFX_FILE);
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
