use cwtools_index::TypeIndex;
use cwtools_validation::Game;

use super::common::{Opts, validate};

const GFX_FILE: Opts<'static> = Opts {
    path: "game/interface/test.gfx",
    game: Some(Game::Hoi4),
    ..Opts::DEFAULT
};

const TEXTURE_CWT: &str = r#"
spriteType = {
    texturefile = filepath
    secondfile = filepath
}
types = {
    type[spriteType] = {
        path = "game/interface"
    }
}
"#;

#[test]
fn texture_reference_resolves_via_sibling_extension() {
    // The engine resolves textures by stem: a `.tga` reference is satisfied by a
    // shipped `.dds` and vice versa (vanilla `core.gfx` points at `.tga` files
    // while only the `.dds` ships). CW113 must not fire when the sibling exists.
    let mut idx = TypeIndex::new();
    idx.file_index.add_paths([
        "gfx/test/button.dds".to_string(),
        "gfx/test/icon.tga".to_string(),
    ]);

    let script = r#"
spriteType = {
    texturefile = "gfx/test/button.tga"
    secondfile = "gfx/test/icon.dds"
}
"#;
    let errs = validate(
        TEXTURE_CWT,
        script,
        &Opts {
            type_index: Some(&idx),
            ..GFX_FILE
        },
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert!(
        cw113.is_empty(),
        "texture references should resolve via their sibling extension, got: {:?}",
        cw113
    );
}

#[test]
fn case_mismatched_file_reference_relates_the_on_disk_spelling() {
    // On a case-sensitive run the file is there, just under another spelling.
    // CW113 says which one, so the reader doesn't go hunting for the file.
    let mut idx = TypeIndex::new();
    idx.file_index.set_case_sensitive(true);
    idx.file_index
        .add_paths(["gfx/test/button.dds".to_string()]);

    let script = "spriteType = {\n    texturefile = \"gfx/test/Button.dds\"\n}\n";
    let errs = validate(
        TEXTURE_CWT,
        script,
        &Opts {
            type_index: Some(&idx),
            ..GFX_FILE
        },
    );
    let cw113 = errs
        .iter()
        .find(|e| e.code == Some("CW113"))
        .expect("case mismatch is flagged in case-sensitive mode");
    assert_eq!(cw113.related.len(), 1, "got: {:?}", cw113.related);
    assert!(
        cw113.related[0].message.contains("gfx/test/button.dds"),
        "got: {}",
        cw113.related[0].message
    );
}

#[test]
fn missing_texture_with_no_sibling_still_flagged() {
    // Regression guard against blanket-suppressing texture CW113: a reference with
    // neither extension present on disk (genuinely missing asset) must still flag.
    let mut idx = TypeIndex::new();
    idx.file_index
        .add_paths(["gfx/test/button.dds".to_string()]);

    let script = r#"
spriteType = {
    texturefile = "gfx/test/ghost.tga"
}
"#;
    let errs = validate(
        TEXTURE_CWT,
        script,
        &Opts {
            type_index: Some(&idx),
            ..GFX_FILE
        },
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert_eq!(
        cw113.len(),
        1,
        "a texture missing in both extensions must still be flagged, got: {:?}",
        errs
    );
}

#[test]
fn sound_asset_file_resolves_beside_the_asset() {
    // A sound `.asset` `file =` resolves relative to the .asset's own directory,
    // not the field's `sound/` root prefix (CW113 false positive).
    let cwt = r#"
sound = {
    name = scalar
    file = filepath[sound/]
}
types = {
    type[sound] = {
        path = "game/sound"
    }
}
"#;
    let mut idx = TypeIndex::new();
    // The .asset itself is indexed (so its root-relative dir can be recovered)
    // and the referenced .wav lives beside it — but NOT under bare `sound/`.
    idx.file_index.add_paths([
        "sound/zom/zom_vo.asset".to_string(),
        "sound/zom/zom_idle_001.wav".to_string(),
    ]);

    let script = r#"
sound = {
    name = "zom_idle_001"
    file = "zom_idle_001.wav"
}
"#;
    let errs = validate(
        cwt,
        script,
        &Opts {
            path: "/game/root/sound/zom/zom_vo.asset",
            type_index: Some(&idx),
            ..GFX_FILE
        },
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert!(
        cw113.is_empty(),
        "sound file beside the .asset should resolve, got: {:?}",
        cw113
    );
}

/// `filepath[gfx/,.dds]` declares both a root prefix and an extension. A
/// reference that already carries either one — in any case — must not have it
/// appended or prepended a second time.
const PREFIX_EXT_CWT: &str = r#"
spriteType = {
    texturefile = filepath[gfx/,.dds]
}
types = {
    type[spriteType] = {
        path = "game/interface"
    }
}
"#;

fn filepath_errors(script: &str, indexed: &[&str]) -> Vec<(Option<&'static str>, String)> {
    let mut idx = TypeIndex::new();
    idx.file_index
        .add_paths(indexed.iter().map(|s| s.to_string()));
    validate(
        PREFIX_EXT_CWT,
        script,
        &Opts {
            type_index: Some(&idx),
            ..GFX_FILE
        },
    )
    .into_iter()
    .filter(|e| e.code == Some("CW113"))
    .map(|e| (e.code, e.message))
    .collect()
}

#[test]
fn extension_already_present_in_mixed_case_is_not_appended_twice() {
    // `.DDS` is the configured `.dds` in another case. Appending would look for
    // `gfx/x.DDS.dds`, which is not what the engine resolves.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"gfx/x.DDS\"\n}\n",
        &["gfx/x.dds"],
    );
    assert!(
        errs.is_empty(),
        "a mixed-case extension must count as already present, got: {errs:?}"
    );
}

#[test]
fn prefix_already_present_in_mixed_case_is_not_prepended_twice() {
    // `GFX/` is the configured `gfx/` in another case. Prepending would look for
    // `gfx/GFX/x.dds`.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"GFX/x.dds\"\n}\n",
        &["gfx/x.dds"],
    );
    assert!(
        errs.is_empty(),
        "a mixed-case prefix must count as already present, got: {errs:?}"
    );
}

#[test]
fn missing_prefix_and_extension_are_both_supplied() {
    // The complement of the two above: a bare reference gets both, and the
    // resulting candidate is what resolves against the index.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"x\"\n}\n",
        &["gfx/x.dds"],
    );
    assert!(
        errs.is_empty(),
        "a bare reference must resolve as gfx/x.dds, got: {errs:?}"
    );
}

#[test]
fn a_genuinely_missing_prefixed_reference_still_reports_the_built_candidate() {
    // Guard against blanket suppression, and pin the message: the candidate the
    // diagnostic names is the prefixed-and-extended form actually looked up.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"ghost\"\n}\n",
        &["gfx/x.dds"],
    );
    assert_eq!(
        errs.len(),
        1,
        "a missing reference must report, got: {errs:?}"
    );
    assert!(
        errs[0].1.contains("gfx/ghost.dds"),
        "the diagnostic should name the built candidate, got: {:?}",
        errs[0].1
    );
}

#[test]
fn texture_sibling_lookup_handles_a_mixed_case_non_ascii_path() {
    // The sibling swap rewrites the last four bytes of the candidate. A
    // non-ASCII stem makes that a byte offset that must still land on a
    // character boundary, and the extension is matched case-insensitively.
    // No configured extension here: with one, the field appends it before the
    // sibling swap is ever reached.
    let mut idx = TypeIndex::new();
    idx.file_index.add_paths(["gfx/café.dds".to_string()]);

    let errs = validate(
        TEXTURE_CWT,
        "spriteType = {\n    texturefile = \"gfx/café.TGA\"\n}\n",
        &Opts {
            type_index: Some(&idx),
            ..GFX_FILE
        },
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert!(
        cw113.is_empty(),
        "a .TGA reference should resolve via its .dds sibling, got: {cw113:?}"
    );
}
