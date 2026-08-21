//! End-to-end test for the cwtools-stellaris-config integration.
//!
//! The fixture under `testfiles/stellaris-config/` mirrors the layout of the
//! external `cwtools-stellaris-config` repo (its `scopes.cwt` and `links.cwt`).
//! We load it through the real ruleset loader, build the runtime
//! `ScopeRegistry`, and assert that scope/link resolution matches what the
//! config declares. This is the regression net for issue #8's Stellaris slice:
//! once the hardcoded `STELLARIS_SCOPES` table is gone, the config has to be
//! the source of truth and this test pins that.

use std::path::PathBuf;

use cwtools_game::constants::Game;
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testfiles")
        .join("stellaris-config")
}

fn load_stellaris_ruleset() -> cwtools_rules::rules_types::RuleSet {
    let table = StringTable::new();
    let (ruleset, _errors) = load_ruleset_from_dir(
        &fixture_dir(),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    // The fixture only carries scopes.cwt, links.cwt, pre_triggers.cwt; other
    // cross-file type references surface as config-validation warnings about
    // unrelated type definitions. The scope and link inputs we test against
    // are populated regardless.
    ruleset
}

fn stellaris_registry() -> ScopeRegistry {
    let ruleset = load_stellaris_ruleset();
    ScopeRegistry::from_config(&ruleset.scope_inputs, &ruleset.link_inputs, Game::Stellaris)
}

/// Sanity: the fixture must contain a scopes.cwt and a links.cwt, otherwise
/// the rest of the assertions in this file are meaningless. Catches the case
/// where someone deletes the fixture but leaves the tests.
#[test]
fn fixture_files_exist() {
    let dir = fixture_dir();
    assert!(
        dir.join("scopes.cwt").is_file(),
        "scopes.cwt missing under {}",
        dir.display()
    );
    assert!(
        dir.join("links.cwt").is_file(),
        "links.cwt missing under {}",
        dir.display()
    );
}

/// All scopes the cwtools-stellaris-config declares must resolve through the
/// registry. Pins that `from_config` is wired up for Stellaris.
#[test]
fn config_scopes_resolve() {
    let ruleset = load_stellaris_ruleset();
    let reg = stellaris_registry();

    // Every scope name declared in the config must resolve.
    for si in &ruleset.scope_inputs {
        assert!(
            reg.id_of(&si.name).is_some(),
            "config-declared scope `{}` did not resolve",
            si.name
        );
        for alias in &si.aliases {
            assert!(
                reg.id_of(alias).is_some(),
                "config-declared scope alias `{alias}` (of `{}`) did not resolve",
                si.name
            );
        }
    }

    // Spot-check the named scopes the rest of the engine (and the existing
    // Stellaris tests) refer to. If any of these resolve to None, scope checks
    // for those names will be silently lenient.
    for scope_name in [
        "Country",
        "Leader",
        "System",
        "Planet",
        "Ship",
        "Fleet",
        "Pop",
        "Army",
        "Species",
        "Pop Faction",
        "Sector",
        "War",
        "Megastructure",
        "Design",
        "Starbase",
        "Star",
        "Deposit",
        "Federation",
        "Alliance",
        "Trait",
        "Situation",
        "Agreement",
    ] {
        assert!(
            reg.id_of(scope_name).is_some(),
            "expected scope `{scope_name}` to resolve (config or synthesized)"
        );
    }
}

/// `from_config` treats Alliance and Federation as DIFFERENT scopes (per the
/// config), unlike the legacy hardcoded table which merged them. This pins the
/// new behaviour: each name resolves to its own id.
#[test]
fn config_alliance_and_federation_are_separate_scopes() {
    let reg = stellaris_registry();

    let alliance = reg
        .id_of("Alliance")
        .expect("Alliance scope resolves from config");
    let federation = reg
        .id_of("Federation")
        .expect("Federation scope resolves from config");

    assert_ne!(
        alliance, federation,
        "the cwtools-stellaris-config declares Alliance and Federation as \
         separate scopes; the engine must not merge them"
    );

    assert_eq!(reg.id_of("alliance"), Some(alliance));
    assert_eq!(reg.id_of("federation"), Some(federation));
}

/// All links the cwtools-stellaris-config declares must resolve. The link's
/// target scope (if any) must also resolve.
#[test]
fn config_links_resolve() {
    let reg = stellaris_registry();

    for li in &load_stellaris_ruleset().link_inputs {
        let registered = match &li.prefix {
            // Prefix links live in `prefix_links`, not `links`.
            Some(p) => reg
                .prefix_links
                .iter()
                .any(|(prefix, _)| prefix == p.to_ascii_lowercase().as_str()),
            None => reg.links.contains_key(&li.name.to_ascii_lowercase()),
        };
        assert!(
            registered,
            "config-declared link `{}` did not register",
            li.name
        );

        if let Some(target) = &li.output_scope {
            assert!(
                reg.id_of(target).is_some(),
                "link `{}` targets unknown scope `{target}` (the config's \
                 scope list must declare every link target)",
                li.name
            );
        }
    }
}

/// Pins the production config -> reindex() -> CW120 wiring (validator unit tests hand-build the map).
#[test]
fn config_pretriggers_populate_per_scope() {
    let ruleset = load_stellaris_ruleset();

    for (scope, member) in [
        ("planet", "has_owner"),
        ("planet", "is_ai"),
        ("pop", "is_enslaved"),
        ("pop", "is_being_purged"),
        ("system", "is_capital"),
        ("starbase", "is_occupied_flag"),
        ("leader", "is_idle"),
        ("situation", "has_owner"),
        ("country", "is_ai"),
    ] {
        assert!(
            ruleset
                .pretriggers()
                .get(scope)
                .is_some_and(|set| set.contains(member)),
            "expected pretriggers[{scope}] to contain `{member}`, got: {:?}",
            ruleset.pretriggers().keys().collect::<Vec<_>>()
        );
    }

    // The map is per-scope, not a flat union: `is_idle` is leader-only.
    assert!(
        !ruleset
            .pretriggers()
            .get("planet")
            .is_some_and(|set| set.contains("is_idle")),
        "leader-only pretrigger must not leak into the planet set"
    );
}

/// End-to-end: the loaded config drives CW120 for planet_event but not fleet_event.
#[test]
fn config_driven_cw120_fires_for_planet_event() {
    use cwtools_string_table::string_table::StringTable;
    use cwtools_validation::per_game::stellaris::validate_stellaris;

    let ruleset = load_stellaris_ruleset();
    let table = StringTable::new();

    let run = |script: &str| {
        let ast = cwtools_parser::parser::parse_string(script, &table);
        let mut errors = Vec::new();
        validate_stellaris(
            &ast,
            &ruleset,
            &table,
            &"events/test.txt".into(),
            None,
            &mut errors,
        );
        errors
    };

    let flagged = run("planet_event = {\n\
         is_triggered_only = yes\n\
         trigger = { has_owner = yes }\n\
         }\n");
    assert!(
        flagged.iter().any(|e| e.code == Some("CW120")),
        "planet pretrigger inside trigger should emit CW120, got: {flagged:?}"
    );

    let quiet = run("fleet_event = {\n\
         is_triggered_only = yes\n\
         trigger = { has_owner = yes }\n\
         }\n");
    assert!(
        !quiet.iter().any(|e| e.code == Some("CW120")),
        "fleet_event has no pretrigger category, got: {quiet:?}"
    );
}

/// Synthesized iterators (`every_/random_/any_/all_<scope>`) must be
/// generated for every scope alias the config declares.
#[test]
fn synthesized_iterators_present_for_every_scope_alias() {
    let ruleset = load_stellaris_ruleset();
    let reg = stellaris_registry();

    for si in &ruleset.scope_inputs {
        for alias in std::iter::once(&si.name).chain(si.aliases.iter()) {
            // Compound names like `planet_army` are not synthesized — only
            // simple ascii-alphanumeric/underscore aliases are.
            if !alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let alias_lc = alias.to_ascii_lowercase();
            for prefix in ["every_", "random_", "any_", "all_"] {
                let key = format!("{prefix}{alias_lc}");
                assert!(
                    reg.links.contains_key(&key),
                    "expected synthesized iterator `{key}` (config scope `{alias}`)"
                );
            }
        }
    }
}
