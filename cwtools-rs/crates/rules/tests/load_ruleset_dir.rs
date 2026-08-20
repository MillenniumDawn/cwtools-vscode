use cwtools_error_codes::ErrorSeverity;
use cwtools_rules::rules_types::RootRule;
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ruleset")
}

/// The directory walk itself: every `.cwt` under the root, subdirectories
/// included, merged into one RuleSet, with `folders.cwt` taking its own path and
/// non-`.cwt` files left alone.
///
/// This is the always-on half of the pair below. It runs on a committed fixture
/// so CI asserts something real about `load_ruleset_from_dir` without a checkout
/// of the game config.
#[test]
fn load_ruleset_dir_merges_every_cwt_file() {
    let table = StringTable::new();
    let (ruleset, errors) = load_ruleset_from_dir(
        &fixture_dir(),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );

    assert!(
        errors.is_empty(),
        "fixture should parse clean, got {errors:?}"
    );

    // Types from the top level and from `nested/` both land in one RuleSet.
    let type_names: Vec<&str> = ruleset.types.iter().map(|t| t.name.as_str()).collect();
    assert!(
        type_names.contains(&"guard_idea"),
        "top-level types.cwt missing from {type_names:?}"
    );
    assert!(
        type_names.contains(&"guard_decision"),
        "nested/effects.cwt missing from {type_names:?}, the walk is not recursive"
    );
    assert!(
        !type_names.contains(&"should_never_load"),
        "loader parsed a non-.cwt file: {type_names:?}"
    );

    // A type's own fields survive the merge, not just its name.
    let idea = ruleset
        .types
        .iter()
        .find(|t| t.name == "guard_idea")
        .expect("guard_idea");
    assert_eq!(idea.name_field.as_deref(), Some("id"));
    // The loader strips the config's `game/` prefix off type paths.
    assert_eq!(idea.path_options.paths, vec!["common/ideas".to_string()]);

    let gender = ruleset
        .enums
        .iter()
        .find(|e| e.key == "guard_gender")
        .expect("enums.cwt did not contribute enum[guard_gender]");
    assert_eq!(
        gender.values,
        vec!["male".to_string(), "female".to_string()]
    );

    let alias_names: Vec<&str> = ruleset.aliases.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        alias_names.contains(&"effect:guard_set_flag") && alias_names.contains(&"effect:guard_if"),
        "aliases from nested/effects.cwt missing from {alias_names:?}"
    );
    assert!(
        ruleset
            .single_aliases
            .iter()
            .any(|(k, _)| k == "guard_limit"),
        "single_alias[guard_limit] missing"
    );

    // Root rules are keyed by type name, one per `<type> = { ... }` block.
    let root_types: Vec<&str> = ruleset
        .root_rules
        .iter()
        .filter_map(|r| match r {
            RootRule::TypeRule(name, _) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        root_types.contains(&"guard_idea") && root_types.contains(&"guard_decision"),
        "root rules missing from {root_types:?}"
    );

    // folders.cwt is a plain line list, not .cwt syntax, and has its own branch.
    assert_eq!(
        ruleset.folders,
        vec![
            "common".to_string(),
            "events".to_string(),
            "localisation".to_string()
        ]
    );
}

/// A directory walk failure must remain non-fatal but reach callers through
/// the structured rules-error channel. A regular file is a portable way to
/// make `read_dir` fail without relying on permissions or a race.
#[test]
fn load_ruleset_dir_reports_directory_read_error() {
    let path = fixture_dir().join("nested/notes.txt");
    let table = StringTable::new();
    let (ruleset, errors) = load_ruleset_from_dir(
        &path,
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );

    assert!(ruleset.types.is_empty());
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    let error = &errors[0];
    assert_eq!(error.file, path);
    assert_eq!((error.line, error.col), (1, 0));
    assert_eq!(
        (error.code, error.severity),
        ("CW600", ErrorSeverity::Error)
    );
    assert!(
        error.message.starts_with("read directory error:"),
        "error: {error}"
    );
}

/// Every rules problem carries a catalog code and a severity, so the CLI can
/// report it like any other diagnostic and fail CI on the errors alone. A
/// malformed `## cardinality` degrades one rule rather than breaking the
/// ruleset, so it stays a warning; an undefined reference does not.
#[test]
fn load_ruleset_dir_codes_and_grades_every_problem() {
    let tmp = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        tmp.path().join("broken.cwt"),
        "types = { type[foo] = { path = \"common/foo\" } }\n\
         ## cardinality = 0..x\n\
         r = { a = <undefined_type> }\n",
    )
    .expect("write broken.cwt");

    let table = StringTable::new();
    let (_, errors) = load_ruleset_from_dir(
        tmp.path(),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );

    let find = |code: &str| {
        errors
            .iter()
            .find(|e| e.code == code)
            .unwrap_or_else(|| panic!("no {code} in {errors:?}"))
    };
    assert_eq!(find("CW601").severity, ErrorSeverity::Error);
    assert!(find("CW601").message.contains("undefined_type"));
    assert_eq!(find("CW603").severity, ErrorSeverity::Warning);
}

#[test]
fn load_ruleset_dir_merges_cwt_files_in_sorted_order() {
    let tmp = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        tmp.path().join("zebra.cwt"),
        "types = { type[zebra_only] = { path = \"common/zebra\" } }\n",
    )
    .expect("write zebra.cwt");
    std::fs::write(
        tmp.path().join("alpha.cwt"),
        "types = { type[alpha_only] = { path = \"common/alpha\" } }\n",
    )
    .expect("write alpha.cwt");

    let table = StringTable::new();
    let (ruleset, errors) = load_ruleset_from_dir(
        tmp.path(),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    assert!(errors.is_empty(), "errors: {errors:?}");
    let names: Vec<&str> = ruleset.types.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["alpha_only", "zebra_only"], "got {names:?}");
}

/// End-to-end load of the real HOI4 config (`cwtools-hoi4-config`), which is
/// its own repo and not vendored here.
///
/// Ignored by default: without the checkout there is nothing to load, and a test
/// that quietly returns instead would report as passing while asserting nothing.
/// Run it with `cargo test -p cwtools_rules -- --ignored`, from a sibling
/// checkout (`<github-projects>/cwtools-hoi4-config/Config`) or with
/// `CWTOOLS_HOI4_CONFIG` pointing elsewhere. A missing directory is a hard
/// failure once you have asked for the test by name.
#[test]
#[ignore = "needs a cwtools-hoi4-config checkout; run with --ignored"]
fn load_hoi4_config_dir() {
    let config_dir = std::env::var_os("CWTOOLS_HOI4_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../cwtools-hoi4-config/Config")
        });

    assert!(
        config_dir.exists(),
        "hoi4-config not found at {}; clone it as a sibling or set CWTOOLS_HOI4_CONFIG",
        config_dir.display()
    );

    let table = StringTable::new();
    let (ruleset, errors) = load_ruleset_from_dir(
        &config_dir,
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );

    // Report parse errors but don't fail on them — some .cwt files may use
    // features the Rust loader doesn't implement yet.
    for err in &errors {
        eprintln!("warn: {}", err);
    }

    println!("  types:         {}", ruleset.types.len());
    println!("  enums:         {}", ruleset.enums.len());
    println!("  aliases:       {}", ruleset.aliases.len());
    println!("  single_aliases:{}", ruleset.single_aliases.len());
    println!("  complex_enums: {}", ruleset.complex_enums.len());
    println!("  root_rules:    {}", ruleset.root_rules.len());
    println!("  values:        {}", ruleset.values.len());

    assert!(
        !errors.iter().any(|e| e.message.contains("unexpanded")),
        "the real config must resolve every single_alias within the expansion budget: {errors:?}"
    );

    assert!(
        ruleset.types.len() > 20,
        "expected > 20 types, got {}",
        ruleset.types.len()
    );
    assert!(
        !ruleset.enums.is_empty(),
        "expected at least one enum, got {}",
        ruleset.enums.len()
    );
}
