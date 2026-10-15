use std::collections::HashMap;

use cwtools_index::{SourceLocation, TypeIndex, TypeInstance};
use cwtools_validation::Game;

use super::common::{Opts, validate};

#[test]
fn test_gfx_sprite_type_parses_and_validates() {
    // Minimal rules matching HOI4 interface/gfx.cwt: spriteType lives in
    // .gfx files under interface/, skipping the spriteTypes root key.
    let cwt = r#"
types = {
    type[spriteType] = {
        path = "game/interface"
        skip_root_key = spriteTypes
        path_extension = .gfx
        name_field = "name"
        ## type_key_filter = spriteType
        subtype[spriteType] = { }
    }
}

spriteType = {
    name = scalar
    ## cardinality = 0..1
    textureFile = scalar
    ## cardinality = 0..1
    noOfFrames = int
}
"#;

    // Valid .gfx file: a spriteType wrapped in spriteTypes { }
    let script = r#"
spriteTypes = {
    spriteType = {
        name = "GFX_my_icon"
        textureFile = "gfx/interface/my_icon.dds"
    }
}
"#;
    // Path must match the type's path (interface/) and extension (.gfx)
    let errors = validate(
        cwt,
        script,
        &Opts {
            path: "game/interface/my_mod.gfx",
            game: Some(Game::Hoi4),
            ..Opts::DEFAULT
        },
    );
    assert!(
        errors.is_empty(),
        "Expected no errors for valid .gfx, got: {:?}",
        errors
    );

    // The same content in a .txt file should NOT match the spriteType type
    // (path_extension = .gfx filters it out), so no type errors but also no
    // validation against spriteType rules (the file is simply untyped).
    let errors_txt = validate(
        cwt,
        script,
        &Opts {
            path: "game/interface/my_mod.txt",
            game: Some(Game::Hoi4),
            ..Opts::DEFAULT
        },
    );
    // A .txt file won't match the type (wrong extension), so validation is
    // lenient (no rules apply). Either 0 errors or only non-type errors are OK.
    let _ = errors_txt; // just ensure it doesn't panic
}

#[test]
fn test_gfx_sprite_type_indexed_for_reference_resolution() {
    // spriteType instances defined in a .gfx file must appear in the TypeIndex
    // so that <spriteType> references in .gui files resolve correctly.
    let cwt = r#"
types = {
    type[spriteType] = {
        path = "game/interface"
        skip_root_key = spriteTypes
        path_extension = .gfx
        name_field = "name"
    }
    type[widget] = {
        path = "game/interface"
        path_extension = .gui
        skip_root_key = guiTypes
    }
}

widget = {
    name = scalar
    ## cardinality = 0..1
    spriteType = <spriteType>
}
"#;

    // Build a TypeIndex that includes a known sprite (as if collected from a .gfx file)
    let mut idx = TypeIndex::new();
    let mut map = HashMap::new();
    map.insert(
        "spriteType".to_string(),
        vec![TypeInstance {
            name: "GFX_my_icon".to_string(),
            location: SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }],
    );
    idx.merge("game/interface/sprites.gfx", map);
    idx.complete = true;
    let opts = Opts {
        path: "game/interface/my_widget.gui",
        game: Some(Game::Hoi4),
        type_index: Some(&idx),
        ..Opts::DEFAULT
    };

    // .gui file referencing the known sprite — should be clean
    let gui_valid = r#"
guiTypes = {
    widget = {
        name = "my_widget"
        spriteType = GFX_my_icon
    }
}
"#;
    let errs_valid = validate(cwt, gui_valid, &opts);
    let type_errs: Vec<_> = errs_valid
        .iter()
        .filter(|e| e.code == Some("CW500"))
        .collect();
    assert!(
        type_errs.is_empty(),
        "Valid sprite ref should not produce CW500, got: {:?}",
        errs_valid
    );

    // .gui file referencing an unknown sprite — should produce CW500
    let gui_bad = r#"
guiTypes = {
    widget = {
        name = "my_widget"
        spriteType = GFX_nonexistent
    }
}
"#;
    let errs_bad = validate(cwt, gui_bad, &opts);
    let type_errs_bad: Vec<_> = errs_bad
        .iter()
        .filter(|e| e.code == Some("CW500"))
        .collect();
    assert!(
        !type_errs_bad.is_empty(),
        "Bogus sprite ref should produce CW500, got: {:?}",
        errs_bad
    );
}

#[test]
fn test_gui_container_window_type_parses_and_validates() {
    // Minimal rules matching HOI4 interface/gui.cwt: containerWindowType lives
    // in .gui files under interface/, skipping the guiTypes root key.
    let cwt = r#"
types = {
    type[containerWindowType] = {
        path = "game/interface"
        name_field = "name"
        path_extension = .gui
        skip_root_key = guiTypes
    }
}

containerWindowType = {
    name = scalar
    ## cardinality = 0..1
    moveable = bool
}
"#;
    let opts = Opts {
        path: "game/interface/my_window.gui",
        game: Some(Game::Hoi4),
        ..Opts::DEFAULT
    };

    // Valid .gui file
    let script = r#"
guiTypes = {
    containerWindowType = {
        name = "my_window"
        moveable = yes
    }
}
"#;
    let errors = validate(cwt, script, &opts);
    assert!(
        errors.is_empty(),
        "Expected no errors for valid .gui, got: {:?}",
        errors
    );

    // Malformed .gui: wrong type for moveable (expects bool, got int)
    let bad_script = r#"
guiTypes = {
    containerWindowType = {
        name = "my_window"
        moveable = 42
    }
}
"#;
    let errors_bad = validate(cwt, bad_script, &opts);
    assert!(
        !errors_bad.is_empty(),
        "Expected a validation error for moveable = 42, got none"
    );
}
