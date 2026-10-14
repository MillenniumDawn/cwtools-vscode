use cwtools_driver::Session;
use cwtools_localization::Lang;

use crate::common::{THING_RULES, loc_gate_workspace, workspace_config, write_tree};

// ── CW100 loc gate ───────────────────────────────────────────────────────────

fn cw100_count(workspace: &std::path::Path) -> usize {
    let session = Session::load(workspace_config(workspace));
    session
        .validate_all()
        .iter()
        .flat_map(|(_, errs)| errs.iter())
        .filter(|e| e.code == Some("CW100"))
        .count()
}

/// A mod with no `localisation/` at all must not report CW100 for every object:
/// an empty loc index means "loc not loaded", not "nothing is localised". Same
/// gate the LSP applies in `append_missing_loc_errors`.
#[test]
fn cw100_is_suppressed_when_the_loc_index_is_empty() {
    let tmp = loc_gate_workspace(None);
    assert_eq!(
        cw100_count(tmp.path()),
        0,
        "CW100 must not fire when no loc files were loaded"
    );
}

/// With loc data present the gate is open: an object whose required key is
/// missing from a non-empty loc index still reports CW100.
#[test]
fn cw100_still_fires_when_loc_data_exists() {
    let tmp = loc_gate_workspace(Some("l_english:\n unrelated_key:0 \"text\"\n"));
    assert_eq!(
        cw100_count(tmp.path()),
        1,
        "CW100 must still fire for a missing key once loc data is loaded"
    );
}

// ── loc language scoping ─────────────────────────────────────────────────────

/// A temp workspace whose `mod/localisation/` holds one english file, one french
/// file, and one file with an unrecognised language header.
fn loc_language_workspace() -> tempfile::TempDir {
    write_tree(&[
        ("rules/things.cwt", THING_RULES),
        ("mod/common/things/x.txt", "my_thing = { }\n"),
        (
            "mod/localisation/a_l_english.yml",
            "l_english:\n english_key:0 \"e\"\n",
        ),
        (
            "mod/localisation/b_l_french.yml",
            "l_french:\n french_key:0 \"f\"\n",
        ),
        (
            "mod/localisation/c_l_klingon.yml",
            "l_klingon:\n other_key:0 \"k\"\n",
        ),
    ])
}

fn load_scoped(
    workspace: &std::path::Path,
    langs: Option<Vec<Lang>>,
) -> cwtools_driver::SessionWithFiles {
    let mut config = workspace_config(workspace);
    config.loc_languages = langs;
    Session::load(config)
}

#[test]
fn unscoped_loc_load_keeps_every_language() {
    let ws = loc_language_workspace();
    let session = load_scoped(ws.path(), None);
    assert!(session.loc_index().exists_any("english_key"));
    assert!(
        session.loc_index().exists_any("french_key"),
        "the default must keep loading every language"
    );
}

#[test]
fn scoped_loc_load_skips_other_languages() {
    let ws = loc_language_workspace();
    let session = load_scoped(ws.path(), Some(vec![Lang::English]));
    assert!(session.loc_index().exists_any("english_key"));
    assert!(
        !session.loc_index().exists_any("french_key"),
        "a language outside --loc-language should never be parsed"
    );
    assert_eq!(session.loc_index().languages_with_data(), &[Lang::English]);
}

#[test]
fn session_loc_load_filters_the_mod_but_not_vanilla() {
    let ws = loc_language_workspace();
    let mod_loc = ws.path().join("mod/localisation");
    std::fs::write(
        mod_loc.join("skip_mod_l_english.yml"),
        "l_english:\n ignored_mod_key:0 \"m\"\n",
    )
    .unwrap();
    let vanilla = ws.path().join("vanilla");
    let vanilla_loc = vanilla.join("localisation");
    std::fs::create_dir_all(&vanilla_loc).unwrap();
    std::fs::write(
        vanilla_loc.join("skip_vanilla_l_english.yml"),
        "l_english:\n vanilla_key:0 \"v\"\n",
    )
    .unwrap();
    std::fs::write(
        vanilla_loc.join("names.csv"),
        "key;english;french\nvanilla_csv_key;;Nom\n",
    )
    .unwrap();
    let ignore_files = ["skip_*".to_string()];

    let mut config = workspace_config(ws.path());
    config.vanilla = Some(vanilla);
    config.ignore_files = &ignore_files;
    config.loc_languages = Some(vec![Lang::English]);
    let session = Session::load(config);

    assert!(!session.loc_index().exists_any("ignored_mod_key"));
    assert!(session.loc_index().exists_any("vanilla_key"));
    assert!(session.loc_index().exists_any("vanilla_csv_key"));
}

#[test]
fn scoped_loc_load_still_validates_unrecognised_headers() {
    // A file whose header language can't be read isn't scoped out by the loc
    // validator, so the parse-time filter must not drop it either (CW256).
    let ws = loc_language_workspace();
    let session = load_scoped(ws.path(), Some(vec![Lang::English]));
    let diagnostics = session.loc_project_diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.file.ends_with("c_l_klingon.yml")),
        "unrecognised-header files must still be parsed and linted, got {:?}",
        diagnostics.iter().map(|d| &d.file).collect::<Vec<_>>()
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.file.ends_with("b_l_french.yml")),
        "a scoped-out language reports nothing, same as before"
    );
}
