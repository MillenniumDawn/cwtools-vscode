use std::collections::HashSet;
use std::path::PathBuf;

use cwtools_driver::{RulesInput, Session, SessionConfig};
use cwtools_game::constants::Game;
use cwtools_test_support::{capped_alias_workspace, unused_workspace};

// ── CW239 unused-instance pass ───────────────────────────────────────────────

fn unused_session(workspace: &std::path::Path) -> cwtools_driver::SessionWithFiles {
    Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(workspace.join("rules")),
        directory: workspace.join("mod"),
        parent_roots: Vec::new(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    })
}

fn cw239_rows(workspace: &std::path::Path) -> Vec<(PathBuf, String)> {
    unused_session(workspace)
        .validate_all()
        .into_iter()
        .flat_map(|(path, errs)| {
            errs.into_iter()
                .filter(|e| e.code == Some("CW239"))
                .map(move |e| (path.clone(), e.message))
        })
        .collect()
}

/// The batch path's two-phase pass runs end to end: uses recorded across every
/// file, merged, and the definition nothing referenced reported against the
/// file that defines it. A per-file check could not tell these two apart, since
/// the reference lives in a different file from both definitions.
#[test]
fn validate_all_reports_the_unreferenced_instance() {
    let tmp = unused_workspace(true);
    let rows = cw239_rows(tmp.path());
    assert_eq!(
        rows.len(),
        1,
        "exactly one instance is unreferenced: {rows:?}"
    );
    let (path, message) = &rows[0];
    assert!(
        message.contains("lone_thing"),
        "the unreferenced instance should be named: {rows:?}"
    );
    assert!(
        path.ends_with("common/things/x.txt"),
        "CW239 belongs to the file that defines the instance, got {}",
        path.display()
    );
}

#[test]
fn validate_all_reports_capped_alias_without_unused_errors() {
    let tmp = capped_alias_workspace();
    let results = unused_session(tmp.path()).validate_all();
    let capped: Vec<_> = results
        .iter()
        .flat_map(|(path, errors)| {
            errors
                .iter()
                .filter(|error| error.code == Some("CW277"))
                .map(move |error| (path, error))
        })
        .collect();
    assert_eq!(capped.len(), 1, "expected one CW277: {results:?}");
    assert!(
        capped[0].0.ends_with("common/users/u.txt"),
        "the capped file should carry CW277, got {}",
        capped[0].0.display()
    );
    assert!(
        results
            .iter()
            .flat_map(|(_, errors)| errors)
            .all(|error| error.code != Some("CW239")),
        "a capped file must not create false unused-instance errors: {results:?}"
    );
    let neighbour = results
        .iter()
        .find(|(path, _)| path.ends_with("common/users/v.txt"))
        .expect("v.txt should be validated");
    assert_eq!(
        neighbour.1.len(),
        1,
        "the budget is per file; the neighbour keeps its own diagnostic: {:?}",
        neighbour.1
    );
    // Files are validated in parallel. A budget shared across them would move
    // the cap (and everything downstream of it) from run to run.
    assert_eq!(
        results,
        unused_session(tmp.path()).validate_all(),
        "a capped run must be repeatable"
    );
}

/// The same mod under a config that marks no type `should_be_used` reports
/// nothing. This pins the output, not the `needs_use_tracking` short-circuit:
/// that gate only saves work, and `is_tracked` keeps the result empty either
/// way, so forcing the gate on is not observable here.
#[test]
fn validate_all_reports_nothing_without_should_be_used() {
    let tmp = unused_workspace(false);
    assert!(
        cw239_rows(tmp.path()).is_empty(),
        "no type asks to be referenced, so nothing should report"
    );
}

/// `validate_selected` skips the files outside the selection, so a caller
/// reporting on a subset doesn't pay to validate the rest.
#[test]
fn validate_selected_skips_the_unselected_files() {
    let tmp = unused_workspace(false);
    let session = unused_session(tmp.path());
    let only = HashSet::from([tmp
        .path()
        .join("mod")
        .join("common")
        .join("things")
        .join("x.txt")]);

    let paths: Vec<PathBuf> = session
        .validate_selected(Some(&only))
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(paths.len(), 1, "only the selected file runs: {paths:?}");
    assert!(paths[0].ends_with("common/things/x.txt"));
    assert_eq!(
        session.validate_all().len(),
        2,
        "and the unrestricted pass still covers both"
    );
}

/// The selection is an optimization, never a change of answer. With the
/// cross-file use pass armed, CW239 judges every definition against the
/// references collected from every file — so honouring a selection that leaves
/// out the only file holding a reference would report `used_thing` as unused.
/// `validate_selected` validates the whole set instead, leaving the caller to
/// filter its own report.
#[test]
fn validate_selected_is_ignored_while_use_tracking_is_on() {
    let tmp = unused_workspace(true);
    let session = unused_session(tmp.path());
    // The definitions, without the file that references one of them.
    let only = HashSet::from([tmp
        .path()
        .join("mod")
        .join("common")
        .join("things")
        .join("x.txt")]);

    let results = session.validate_selected(Some(&only));
    let unused: Vec<&str> = results
        .iter()
        .flat_map(|(_, errors)| errors)
        .filter(|e| e.code == Some("CW239"))
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(
        unused.len(),
        1,
        "only lone_thing is unreferenced; a partial pass would add used_thing: {unused:?}"
    );
    assert!(unused[0].contains("lone_thing"), "got {unused:?}");
    assert_eq!(
        results.len(),
        2,
        "the whole set is validated when the selection can't be honoured"
    );
}

/// The pass is part of `validate_all`'s result, so it must be as repeatable as
/// the rest of it. The per-file uses are merged out of a rayon collect, and a
/// set iteration leaking into the output would show up here.
#[test]
fn validate_all_unused_rows_are_deterministic() {
    let tmp = unused_workspace(true);
    assert_eq!(cw239_rows(tmp.path()), cw239_rows(tmp.path()));
}
