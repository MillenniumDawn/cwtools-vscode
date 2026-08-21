//! Regression tests for HOI4 false positives reported against the Millennium
//! Dawn mod: enum case-sensitivity, top-level GUI widgets validated against the
//! wrong type, `.asset` music routing, and soft (`~`) cardinality minimums.

use cwtools_index::{SourceLocation, TypeIndex, TypeInstance};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::validate_ast;
use std::collections::HashMap;

fn messages(errors: &[cwtools_validation::ValidationError]) -> Vec<String> {
    errors.iter().map(|e| e.message.clone()).collect()
}

// Fix #1: enum membership is case-insensitive (F# lowercases both sides). The
// `containerOrientations` enum is authored UPPER_LEFT/CENTER but vanilla and mod
// files use upper_left/center.
#[test]
fn enum_value_match_is_case_insensitive() {
    let cwt = r#"
container = {
    orientation = enum[orientations]
}
types = {
    type[container] = { path = "gfx" }
}
enums = {
    enum[orientations] = { UPPER_LEFT CENTER LOWER_LEFT }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let script = r#"
container = {
    orientation = upper_left
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "gfx/test.txt", None, None, None);
    assert!(
        !errors.iter().any(|e| e.message.contains("orientation")),
        "lowercase enum value wrongly flagged. Errors: {:?}",
        messages(&errors)
    );

    // A value in neither case is still rejected.
    let bad = parse_string("container = { orientation = sideways }", &table);
    let bad_errors = validate_ast(&bad, &ruleset, &table, "gfx/test.txt", None, None, None);
    assert!(
        bad_errors.iter().any(|e| e.message.contains("orientation")),
        "genuinely invalid enum value should still be flagged"
    );
}

// Fix #2: a `skip_root_key` wrapper type with a `type_key_filter` must not
// validate sibling grandchildren whose key the filter excludes. Top-level
// `scrollbarType` under `guiTypes` is not a `containerWindowType`, so its fields
// (slider/track/...) must not flag as unexpected.
#[test]
fn wrapper_skips_grandchildren_excluded_by_type_key_filter() {
    let cwt = r#"
containerWindowType = {
    name = scalar
}
types = {
    ## type_key_filter = containerWindowType
    type[containerWindowType] = {
        path = "interface"
        name_field = name
        skip_root_key = guiTypes
    }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let script = r#"
guiTypes = {
    containerWindowType = { name = "ok" }
    scrollbarType = {
        slider = "x"
        track = "y"
    }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "interface/test.gui",
        None,
        None,
        None,
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("slider") || e.message.contains("track")),
        "top-level scrollbarType validated against containerWindowType. Errors: {:?}",
        messages(&errors)
    );
}

// Fix #3: two types sharing a `type_key_filter` and path, differing only by
// `path_extension`, must be disambiguated by the file's extension. A `.asset`
// `music = {}` is a `musicasset` (name/file), not the `.txt` `music` (song).
#[test]
fn asset_extension_routes_to_correct_type() {
    let cwt = r#"
music = {
    song = scalar
}
musicasset = {
    name = scalar
    file = scalar
}
types = {
    ## type_key_filter = music
    type[music] = {
        path = "music"
        path_extension = .txt
        name_field = song
    }
    ## type_key_filter = music
    type[musicasset] = {
        path = "music"
        path_extension = .asset
        name_field = name
    }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let asset = r#"
music = {
    name = "maintheme"
    file = "main.ogg"
}
"#;
    let parsed = parse_string(asset, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "music/test.asset",
        None,
        None,
        None,
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("Unexpected field 'name'")
                || e.message.contains("Unexpected field 'file'")
                || e.message.contains("song")),
        ".asset music routed to the .txt `music` type. Errors: {:?}",
        messages(&errors)
    );

    // The .txt form still routes to `music` (song required, name unexpected).
    let txt = "music = { name = \"x\" }";
    let parsed_txt = parse_string(txt, &table);
    let txt_errors = validate_ast(
        &parsed_txt,
        &ruleset,
        &table,
        "music/test.txt",
        None,
        None,
        None,
    );
    assert!(
        txt_errors
            .iter()
            .any(|e| e.message.contains("Unexpected field 'name'")),
        ".txt music should still validate against the `music` type"
    );
}

// A `@name = value` leaf is a Paradox read-time variable definition, valid
// anywhere in a block. It must not flag as an unexpected field (F# skips keys
// whose first char is '@'). A genuinely unknown key still flags.
#[test]
fn at_prefixed_read_time_variable_is_not_unexpected() {
    let cwt = r#"
container = {
    name = scalar
}
types = {
    type[container] = { path = "interface" }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let script = r#"
container = {
    name = "ok"
    @col_0 = 0
    @col_1 = 73
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "interface/test.gui",
        None,
        None,
        None,
    );
    assert!(
        !errors.iter().any(|e| e.message.contains('@')),
        "@-prefixed read-time variable wrongly flagged. Errors: {:?}",
        messages(&errors)
    );

    // A non-@ unknown key is still flagged.
    let bad = parse_string("container = { name = \"ok\"  bogus = 1 }", &table);
    let bad_errors = validate_ast(
        &bad,
        &ruleset,
        &table,
        "interface/test.gui",
        None,
        None,
        None,
    );
    assert!(
        bad_errors
            .iter()
            .any(|e| e.message.contains("Unexpected field 'bogus'")),
        "genuinely unknown field should still be flagged"
    );
}

// Fix #4: a `~` (soft) cardinality minimum must not flag an under-count. In a
// disjunction of overlapping leafvalue rules (each `~1..inf`), a value matching
// one alternative leaves the others at 0, which is not an error. Genuinely
// invalid values are still caught by the per-value "Unexpected bare value" check.
#[test]
fn soft_min_disjunction_no_false_positive_but_catches_invalid() {
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
    type[ship_name] = { path = "names_ships" }
    type[unit] = {
        path = "units"
        subtype[ship_unit] = {}
    }
}
enums = {
    enum[ship_units] = { destroyer battleship }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // `cruiser` matches <unit.ship_unit> (indexed elsewhere) but is NOT in
    // enum[ship_units]; the enum rule must not report "appears 0 time(s)".
    let script = "ship_name = { type = ship  ship_types = { cruiser } }";
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "names_ships/FRA.txt",
        None,
        None,
        None,
    );
    assert!(
        !errors.iter().any(|e| e.message.contains("appears 0")),
        "soft (~) minimum wrongly flagged an under-count. Errors: {:?}",
        messages(&errors)
    );
}

// A type whose name equals a root key but which is gated by `skip_root_key`
// must NOT own that root node by name. `type[terrain] { skip_root_key =
// categories }` has a `color`-requiring rule, but the top-level `terrain = {}`
// block is the wrapper for `type[graphical_terrain] { skip_root_key = terrain }`.
// The terrain entries must validate against graphical_terrain (type/color/
// texture), not flag as unexpected with `color` missing on the wrapper.
#[test]
fn skip_root_key_type_does_not_own_root_matching_its_name() {
    let cwt = r#"
terrain = {
    color = { int int int }
    movement_cost = float
}
graphical_terrain = {
    type = scalar
    color = { int }
    texture = int
}
types = {
    type[terrain] = {
        path = "common/terrain"
        skip_root_key = categories
    }
    type[graphical_terrain] = {
        path = "common/terrain"
        skip_root_key = terrain
    }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let script = r#"
categories = {
    plains = { movement_cost = 1.0 color = { 0 1 2 } }
}
terrain = {
    terrain_0 = { type = plains color = { 0 } texture = 1 }
    desert = { type = desert color = { 3 } texture = 9 }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "common/terrain/00_terrain.txt",
        None,
        None,
        None,
    );
    assert!(
        !errors.iter().any(|e| e.message.contains("terrain_0")
            || e.message.contains("Unexpected field 'desert'")
            || e.message.contains("color")),
        "graphical terrain entries wrongly validated against type[terrain]. Errors: {:?}",
        messages(&errors)
    );

    // A genuinely unknown field inside a graphical_terrain entry still flags.
    let bad = parse_string(
        "terrain = { terrain_0 = { type = plains color = { 0 } texture = 1 bogus = 1 } }",
        &table,
    );
    let bad_errors = validate_ast(
        &bad,
        &ruleset,
        &table,
        "common/terrain/00_terrain.txt",
        None,
        None,
        None,
    );
    assert!(
        bad_errors
            .iter()
            .any(|e| e.message.contains("Unexpected field 'bogus'")),
        "unknown field in a graphical_terrain entry should still flag. Errors: {:?}",
        messages(&bad_errors)
    );
}

// Fix: a prefixed Complex sprite reference (`GFX_idea_<spriteType>`) must resolve
// against ALL real-world forms the game accepts: a bare value (`picture = x` ->
// sprite `GFX_idea_x`) AND an already-prefixed value (`picture = GFX_idea_x`). A
// value with no matching sprite under any form still flags, so genuinely-missing
// idea pictures are surfaced.
#[test]
fn prefixed_sprite_reference_resolves_bare_and_full_forms() {
    let cwt = r#"
idea = {
    picture = GFX_idea_<spriteType>
}
types = {
    type[idea] = { path = "common/ideas" }
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let mut index = TypeIndex::new();
    index.complete = true;
    let mut per_type: HashMap<String, Vec<TypeInstance>> = HashMap::new();
    per_type.insert(
        "spriteType".to_string(),
        vec![TypeInstance {
            name: "GFX_idea_research_bonus".to_string(),
            location: SourceLocation {
                line: 1,
                col: 1,
                end: (1, 1),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }],
    );
    index.merge("interface/ideas.gfx", per_type);

    // Both the bare form and the already-prefixed form point at the same sprite.
    let ok = parse_string(
        "bare = { picture = research_bonus } full = { picture = GFX_idea_research_bonus }",
        &table,
    );
    let ok_errors = validate_ast(
        &ok,
        &ruleset,
        &table,
        "common/ideas/test.txt",
        None,
        Some(&index),
        None,
    );
    assert!(
        !ok_errors.iter().any(|e| e.message.contains("picture")),
        "bare and prefixed idea pictures should both resolve. Errors: {:?}",
        messages(&ok_errors)
    );

    // A picture with no matching sprite under any form is a genuine miss.
    let bad = parse_string("x = { picture = nonexistent_art }", &table);
    let bad_errors = validate_ast(
        &bad,
        &ruleset,
        &table,
        "common/ideas/test.txt",
        None,
        Some(&index),
        None,
    );
    assert!(
        bad_errors
            .iter()
            .any(|e| e.message.contains("picture") && e.message.contains("nonexistent_art")),
        "a missing idea picture should still flag. Errors: {:?}",
        messages(&bad_errors)
    );
}
