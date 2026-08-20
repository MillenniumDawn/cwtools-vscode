use crate::constants::Game;
use rustc_hash::FxHashMap;

use super::{ScopeId, ScopeLink};

// ── Scope link loading ────────────────────────────────────────────────────────

/// Populate the hardcoded scope-link table for a game. HOI4 is config-driven
/// (its links come from `links.cwt` via the scope registry), so it adds nothing
/// here. Used only by [`crate::scope_registry::ScopeRegistry::from_hardcoded`].
pub fn load_scope_links(game: Game, links: &mut FxHashMap<String, ScopeLink>) {
    use crate::constants::Game::*;
    match game {
        Hoi4 => {}
        Stellaris => load_stellaris_links(links),
        Eu4 => load_eu4_links(links),
        Ck2 => load_ck2_links(links),
        Ck3 => load_ck3_links(links),
        Vic2 => load_vic2_links(links),
        Ir => load_ir_links(links),
        Vic3 => load_vic3_links(links),
        Eu5 => load_eu5_links(links),
        Custom => {}
    }
}

/// Build a scope-change link: valid in `valid_scopes`, produces `target`.
fn sc(valid: &[u32], target: u32) -> ScopeLink {
    ScopeLink {
        valid_scopes: valid.iter().copied().map(ScopeId).collect(),
        target: Some(ScopeId(target)),
        ignore_keys: vec![],
    }
}

/// Insert a link under multiple alias keys.
fn insert_aliases(links: &mut FxHashMap<String, ScopeLink>, names: &[&str], link: ScopeLink) {
    for name in names {
        links.insert(name.to_string(), link.clone());
    }
}

/// Register every `(aliases, valid_scopes, target)` entry as a scope-change
/// link. Shared by the per-game `load_*_links` tables, whose loop bodies were
/// byte-identical.
fn load_entries(links: &mut FxHashMap<String, ScopeLink>, entries: &[(&[&str], &[u32], u32)]) {
    for (aliases, valid, target) in entries {
        insert_aliases(links, aliases, sc(valid, *target));
    }
}

// ── Stellaris ────────────────────────────────────────────────────────────────

// Scope IDs:
// Country=200, Leader=201, System=202, Planet=203, Ship=204, Fleet=205,
// Pop=206, Army=207, Species=208, PopFaction=209, Sector=210,
// Federation=211, War=212, Megastructure=213, Design=214, Starbase=215,
// Star=216, Deposit=217, ArchaeologicalSite=218, AmbientObject=219
//
// Source: STLScopes.fs (oneToOneScopes) + Stellaris CWT config rules.
// STLScopes.fs has no scopedEffects list; links derive from CWT and docs.
fn load_stellaris_links(links: &mut FxHashMap<String, ScopeLink>) {
    const COUNTRY: u32 = 200;
    const LEADER: u32 = 201;
    const SYSTEM: u32 = 202;
    const PLANET: u32 = 203;
    const SHIP: u32 = 204;
    const FLEET: u32 = 205;
    const POP: u32 = 206;
    const ARMY: u32 = 207;
    const SPECIES: u32 = 208;
    const POP_FACTION: u32 = 209;
    const SECTOR: u32 = 210;
    const FEDERATION: u32 = 211;
    const WAR: u32 = 212;
    const MEGASTRUCTURE: u32 = 213;
    const DESIGN: u32 = 214;
    const STARBASE: u32 = 215;
    const STAR: u32 = 216;
    const DEPOSIT: u32 = 217;
    const ARCHAEOLOGICAL_SITE: u32 = 218;
    const AMBIENT_OBJECT: u32 = 219;

    let entries: &[(&[&str], &[u32], u32)] = &[
        // ── Global iterators ─────────────────────────────────────────────
        (
            &["every_country", "random_country", "any_country", "country"],
            &[],
            COUNTRY,
        ),
        (
            &["every_planet", "random_planet", "any_planet", "planet"],
            &[],
            PLANET,
        ),
        (
            &["every_ship", "random_ship", "any_ship", "ship"],
            &[],
            SHIP,
        ),
        (
            &["every_fleet", "random_fleet", "any_fleet", "fleet"],
            &[],
            FLEET,
        ),
        (&["every_pop", "random_pop", "any_pop", "pop"], &[], POP),
        (
            &["every_army", "random_army", "any_army", "army"],
            &[],
            ARMY,
        ),
        (
            &[
                "every_system",
                "random_system",
                "any_system",
                "galactic_object",
                "system",
                "galacticobject",
            ],
            &[],
            SYSTEM,
        ),
        (
            &["every_leader", "random_leader", "any_leader", "leader"],
            &[],
            LEADER,
        ),
        (
            &["every_species", "random_species", "any_species", "species"],
            &[],
            SPECIES,
        ),
        (
            &[
                "every_pop_faction",
                "random_pop_faction",
                "any_pop_faction",
                "pop_faction",
            ],
            &[],
            POP_FACTION,
        ),
        (
            &[
                "every_megastructure",
                "random_megastructure",
                "any_megastructure",
                "megastructure",
            ],
            &[],
            MEGASTRUCTURE,
        ),
        (
            &["every_deposit", "random_deposit", "any_deposit", "deposit"],
            &[],
            DEPOSIT,
        ),
        (&["every_war", "random_war", "any_war", "war"], &[], WAR),
        (
            &[
                "every_federation",
                "random_federation",
                "any_federation",
                "federation",
            ],
            &[],
            FEDERATION,
        ),
        (
            &[
                "every_archaeological_site",
                "random_archaeological_site",
                "any_archaeological_site",
            ],
            &[],
            ARCHAEOLOGICAL_SITE,
        ),
        (
            &[
                "every_ambient_object",
                "random_ambient_object",
                "any_ambient_object",
            ],
            &[],
            AMBIENT_OBJECT,
        ),
        // ── Country-scoped iterators ─────────────────────────────────────
        (
            &[
                "every_owned_planet",
                "random_owned_planet",
                "any_owned_planet",
            ],
            &[COUNTRY],
            PLANET,
        ),
        (
            &[
                "every_controlled_planet",
                "random_controlled_planet",
                "any_controlled_planet",
            ],
            &[COUNTRY],
            PLANET,
        ),
        (
            &["every_subject", "random_subject", "any_subject"],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &[
                "every_playable_country",
                "random_playable_country",
                "any_playable_country",
            ],
            &[],
            COUNTRY,
        ),
        (
            &["every_owned_ship", "random_owned_ship", "any_owned_ship"],
            &[COUNTRY, FLEET],
            SHIP,
        ),
        (
            &["every_owned_fleet", "random_owned_fleet", "any_owned_fleet"],
            &[COUNTRY],
            FLEET,
        ),
        (
            &[
                "every_owned_leader",
                "random_owned_leader",
                "any_owned_leader",
            ],
            &[COUNTRY],
            LEADER,
        ),
        (
            &[
                "every_owned_species",
                "random_owned_species",
                "any_owned_species",
            ],
            &[COUNTRY],
            SPECIES,
        ),
        (
            &["every_owned_pop", "random_owned_pop", "any_owned_pop"],
            &[COUNTRY, PLANET, SECTOR],
            POP,
        ),
        (
            &[
                "every_owned_starbase",
                "random_owned_starbase",
                "any_owned_starbase",
            ],
            &[COUNTRY],
            STARBASE,
        ),
        (
            &[
                "every_neighbor_country",
                "random_neighbor_country",
                "any_neighbor_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &["every_sector", "random_sector", "any_sector"],
            &[COUNTRY],
            SECTOR,
        ),
        // ── System-scoped iterators ──────────────────────────────────────
        (
            &[
                "every_system_planet",
                "random_system_planet",
                "any_system_planet",
            ],
            &[SYSTEM],
            PLANET,
        ),
        (
            &[
                "every_system_fleet",
                "random_system_fleet",
                "any_system_fleet",
            ],
            &[SYSTEM],
            FLEET,
        ),
        (
            &[
                "every_fleet_in_system",
                "random_fleet_in_system",
                "any_fleet_in_system",
            ],
            &[SYSTEM],
            FLEET,
        ),
        (
            &[
                "every_system_ambient_object",
                "random_system_ambient_object",
            ],
            &[SYSTEM],
            AMBIENT_OBJECT,
        ),
        (
            &[
                "every_system_deposit",
                "random_system_deposit",
                "any_system_deposit",
            ],
            &[SYSTEM],
            DEPOSIT,
        ),
        (
            &["every_system_archaeological_site"],
            &[SYSTEM],
            ARCHAEOLOGICAL_SITE,
        ),
        // ── Planet-scoped iterators ──────────────────────────────────────
        (
            &["every_planet_pop", "random_planet_pop", "any_planet_pop"],
            &[PLANET],
            POP,
        ),
        (
            &["every_planet_army", "random_planet_army", "any_planet_army"],
            &[PLANET],
            ARMY,
        ),
        (
            &[
                "every_planet_deposit",
                "random_planet_deposit",
                "any_planet_deposit",
            ],
            &[PLANET],
            DEPOSIT,
        ),
        // ── Sector-scoped iterators ──────────────────────────────────────
        (
            &[
                "every_sector_system",
                "random_sector_system",
                "any_sector_system",
            ],
            &[SECTOR],
            SYSTEM,
        ),
        (
            &[
                "every_sector_planet",
                "random_sector_planet",
                "any_sector_planet",
            ],
            &[SECTOR],
            PLANET,
        ),
        // ── Country named links ──────────────────────────────────────────
        (&["overlord"], &[COUNTRY], COUNTRY),
        (&["federation_leader"], &[COUNTRY, FEDERATION], COUNTRY),
        (&["capital"], &[COUNTRY], PLANET),
        (&["capital_scope"], &[COUNTRY], PLANET),
        (&["capital_star"], &[COUNTRY], SYSTEM),
        (&["starbase"], &[SYSTEM], STARBASE),
        // ── Planet/system links ──────────────────────────────────────────
        (&["star"], &[PLANET], STAR),
        (
            &["solar_system"],
            &[PLANET, SHIP, FLEET, STARBASE, ARMY, POP],
            SYSTEM,
        ),
        (&["sector"], &[PLANET, SYSTEM], SECTOR),
        (
            &["owner"],
            &[
                PLANET,
                SHIP,
                FLEET,
                ARMY,
                POP,
                POP_FACTION,
                STARBASE,
                MEGASTRUCTURE,
                DEPOSIT,
                LEADER,
            ],
            COUNTRY,
        ),
        (&["controller"], &[PLANET], COUNTRY),
        // ── Ship/fleet links ─────────────────────────────────────────────
        (&["fleet"], &[SHIP], FLEET),
        (&["leader"], &[SHIP, FLEET, COUNTRY, ARMY], LEADER),
        (&["design"], &[SHIP], DESIGN),
        // ── Species links ────────────────────────────────────────────────
        (&["species"], &[POP, LEADER], SPECIES),
        // ── Pop-faction link ─────────────────────────────────────────────
        (&["pop_faction"], &[POP], POP_FACTION),
    ];

    load_entries(links, entries);
}

// ── EU4 ──────────────────────────────────────────────────────────────────────

// Scope IDs: Country=300, Province=301, TradeNode=302, Unit=303,
//            Monarch=304, Heir=305, Consort=306, RebelFaction=307,
//            Religion=308, Culture=309, Advisor=310
//
// Source: EU4Scopes.fs (oneToOneScopes + scopedEffects).
// scopedEffects: only "owner" (Province→Country) is active; others commented out.
// Additional links from EU4 CWT rules and modding docs.
fn load_eu4_links(links: &mut FxHashMap<String, ScopeLink>) {
    const COUNTRY: u32 = 300;
    const PROVINCE: u32 = 301;
    const TRADE_NODE: u32 = 302;
    const UNIT: u32 = 303;
    const MONARCH: u32 = 304;
    const HEIR: u32 = 305;
    const CONSORT: u32 = 306;
    const REBEL_FACTION: u32 = 307;
    const RELIGION: u32 = 308;
    const CULTURE: u32 = 309;
    const ADVISOR: u32 = 310;

    let entries: &[(&[&str], &[u32], u32)] = &[
        // ── Global iterators ─────────────────────────────────────────────
        (
            &["every_country", "random_country", "any_country", "country"],
            &[],
            COUNTRY,
        ),
        (
            &[
                "every_province",
                "random_province",
                "any_province",
                "province",
            ],
            &[],
            PROVINCE,
        ),
        (
            &[
                "every_subject_country",
                "random_subject_country",
                "any_subject_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &[
                "every_neighbor_country",
                "random_neighbor_country",
                "any_neighbor_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &["every_ally", "random_ally", "any_ally"],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &[
                "every_enemy_country",
                "random_enemy_country",
                "any_enemy_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &[
                "every_core_province",
                "random_core_province",
                "any_core_province",
            ],
            &[COUNTRY],
            PROVINCE,
        ),
        (
            &[
                "every_owned_province",
                "random_owned_province",
                "any_owned_province",
            ],
            &[COUNTRY],
            PROVINCE,
        ),
        (
            &[
                "every_controlled_province",
                "random_controlled_province",
                "any_controlled_province",
            ],
            &[COUNTRY],
            PROVINCE,
        ),
        (
            &[
                "every_province_in_state",
                "random_province_in_state",
                "any_province_in_state",
            ],
            &[PROVINCE],
            PROVINCE,
        ),
        (
            &[
                "every_neighbor_province",
                "random_neighbor_province",
                "any_neighbor_province",
            ],
            &[PROVINCE],
            PROVINCE,
        ),
        // ── Country named links ──────────────────────────────────────────
        // From EU4Scopes.fs scopedEffects: "owner" Province→Country
        (&["owner"], &[PROVINCE, TRADE_NODE], COUNTRY),
        (&["controller"], &[PROVINCE], COUNTRY),
        (&["capital", "capital_scope"], &[COUNTRY], PROVINCE),
        (&["overlord"], &[COUNTRY], COUNTRY),
        (&["emperor"], &[], COUNTRY),
        (&["trade_node", "tradenode"], &[], TRADE_NODE),
        (&["monarch"], &[COUNTRY], MONARCH),
        (&["heir"], &[COUNTRY], HEIR),
        (&["consort"], &[COUNTRY], CONSORT),
        (&["unit"], &[], UNIT),
        // ── Rebel faction iterators ──────────────────────────────────────
        (
            &[
                "every_rebel_faction",
                "random_rebel_faction",
                "any_rebel_faction",
            ],
            &[COUNTRY, PROVINCE],
            REBEL_FACTION,
        ),
        // ── Advisor iterators ────────────────────────────────────────────
        (
            &["every_advisor", "random_advisor", "any_advisor"],
            &[COUNTRY],
            ADVISOR,
        ),
        // ── Religion / Culture iterators ─────────────────────────────────
        (
            &[
                "every_known_country",
                "random_known_country",
                "any_known_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        // ── Province stubs ───────────────────────────────────────────────
        (&["religion"], &[PROVINCE, COUNTRY], RELIGION),
        (&["culture"], &[PROVINCE, COUNTRY], CULTURE),
    ];

    load_entries(links, entries);
}

// ── CK2 ──────────────────────────────────────────────────────────────────────

// Scope IDs: Character=400, Title=401, Province=402, Offmap=403, War=404,
//            Siege=405, Unit=406, Religion=407, Culture=408, Society=409,
//            Artifact=410, Bloodline=411, Wonder=412
//
// Source: CK2Scopes.fs (oneToOneScopes + scopedEffects).
// scopedEffects has: primary_title, mother, mother_even_if_dead, father,
// father_even_if_dead, killer, liege, liege_before_war, top_liege,
// capital_scope, owner.  Additional iterators from CK2 CWT rules.
fn load_ck2_links(links: &mut FxHashMap<String, ScopeLink>) {
    const CHARACTER: u32 = 400;
    const TITLE: u32 = 401;
    const PROVINCE: u32 = 402;
    const OFFMAP: u32 = 403;
    const WAR: u32 = 404;
    const UNIT: u32 = 406;
    const RELIGION: u32 = 407;
    const CULTURE: u32 = 408;
    const SOCIETY: u32 = 409;
    const ARTIFACT: u32 = 410;

    let entries: &[(&[&str], &[u32], u32)] = &[
        // ── Global iterators ─────────────────────────────────────────────
        (
            &[
                "every_character",
                "random_character",
                "any_character",
                "character",
            ],
            &[],
            CHARACTER,
        ),
        (
            &[
                "every_province",
                "random_province",
                "any_province",
                "province",
            ],
            &[],
            PROVINCE,
        ),
        (
            &[
                "every_playable_ruler",
                "random_playable_ruler",
                "any_playable_ruler",
            ],
            &[],
            CHARACTER,
        ),
        // ── Character iterators ──────────────────────────────────────────
        (
            &["every_vassal", "random_vassal", "any_vassal"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_ward", "random_ward", "any_ward"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_child", "random_child", "any_child"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_sibling", "random_sibling", "any_sibling"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_spouse", "random_spouse", "any_spouse"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_courtier", "random_courtier", "any_courtier"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &[
                "every_realm_character",
                "random_realm_character",
                "any_realm_character",
            ],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &[
                "every_realm_province",
                "random_realm_province",
                "any_realm_province",
            ],
            &[CHARACTER],
            PROVINCE,
        ),
        (
            &[
                "every_demesne_province",
                "random_demesne_province",
                "any_demesne_province",
            ],
            &[CHARACTER],
            PROVINCE,
        ),
        (
            &[
                "every_demesne_title",
                "random_demesne_title",
                "any_demesne_title",
            ],
            &[CHARACTER],
            TITLE,
        ),
        (
            &["every_realm_title", "random_realm_title", "any_realm_title"],
            &[CHARACTER],
            TITLE,
        ),
        (
            &["every_claim", "random_claim", "any_claim"],
            &[CHARACTER],
            TITLE,
        ),
        (
            &["every_heir_title", "random_heir_title"],
            &[CHARACTER],
            TITLE,
        ),
        (
            &["every_artifact", "random_artifact", "any_artifact"],
            &[CHARACTER],
            ARTIFACT,
        ),
        // ── Province iterators ───────────────────────────────────────────
        (
            &[
                "every_neighbor_province",
                "random_neighbor_province",
                "any_neighbor_province",
            ],
            &[PROVINCE],
            PROVINCE,
        ),
        (
            &["every_de_jure_vassal", "random_de_jure_vassal"],
            &[TITLE],
            CHARACTER,
        ),
        // ── Character named links (from CK2Scopes.fs scopedEffects) ──────
        (&["primary_title"], &[CHARACTER], TITLE),
        (&["mother"], &[CHARACTER], CHARACTER),
        (&["mother_even_if_dead"], &[CHARACTER], CHARACTER),
        (&["father"], &[CHARACTER], CHARACTER),
        (&["father_even_if_dead"], &[CHARACTER], CHARACTER),
        (&["killer"], &[CHARACTER], CHARACTER),
        (&["liege"], &[CHARACTER], CHARACTER),
        (&["liege_before_war"], &[CHARACTER], CHARACTER),
        (&["top_liege"], &[CHARACTER], CHARACTER),
        (&["employer"], &[CHARACTER], CHARACTER),
        (&["host"], &[CHARACTER], CHARACTER),
        (&["spouse"], &[CHARACTER], CHARACTER),
        (&["guardian"], &[CHARACTER], CHARACTER),
        (&["betrothed"], &[CHARACTER], CHARACTER),
        (&["regent"], &[CHARACTER], CHARACTER),
        // ── Province links ───────────────────────────────────────────────
        // From CK2Scopes.fs: capital_scope Character/Title → Province
        (&["capital_scope"], &[CHARACTER, TITLE], PROVINCE),
        // Province → Character (owner)
        (&["owner"], &[PROVINCE], CHARACTER),
        (&["location"], &[CHARACTER, UNIT], PROVINCE),
        (&["realm_capital"], &[CHARACTER], PROVINCE),
        // ── Title links ──────────────────────────────────────────────────
        (&["holder_scope"], &[TITLE], CHARACTER),
        (&["de_jure_liege_title"], &[TITLE], TITLE),
        (&["de_facto_liege"], &[TITLE], TITLE),
        (&["independent_ruler"], &[TITLE], CHARACTER),
        // ── War links ────────────────────────────────────────────────────
        (&["war"], &[CHARACTER], WAR),
        (&["attacker"], &[WAR], CHARACTER),
        (&["defender"], &[WAR], CHARACTER),
        // ── Religion / culture links ─────────────────────────────────────
        (&["religion"], &[CHARACTER, PROVINCE], RELIGION),
        (&["culture"], &[CHARACTER, PROVINCE], CULTURE),
        // ── Offmap / society stubs ───────────────────────────────────────
        (&["offmap_ruler"], &[OFFMAP], CHARACTER),
        (
            &[
                "any_society_member",
                "every_society_member",
                "random_society_member",
            ],
            &[SOCIETY],
            CHARACTER,
        ),
    ];

    load_entries(links, entries);
}

// ── CK3 ──────────────────────────────────────────────────────────────────────

// Scope IDs (same set as VIC2/IR, block 500–520):
// Value=500, Bool=501, Flag=502, Color=503, Country=504, Character=505,
// Province=506, Combat=507, Unit=508, Pop=509, Family=510, Party=511,
// Religion=512, Culture=513, Job=514, CultureGroup=515, Area=516,
// State=517, Subunit=518, Governorship=519, Region=520
//
// Source: CK3Scopes.fs.  scopedEffects are all commented out upstream;
// links are derived from CK3 CWT config rules and CK3 modding docs.
fn load_ck3_links(links: &mut FxHashMap<String, ScopeLink>) {
    const CHARACTER: u32 = 505;
    const PROVINCE: u32 = 506;
    const COMBAT: u32 = 507;
    const UNIT: u32 = 508;
    const POP: u32 = 509;
    const FAMILY: u32 = 510;
    const RELIGION: u32 = 512;
    const CULTURE: u32 = 513;
    const CULTURE_GROUP: u32 = 515;
    const STATE: u32 = 517;
    const GOVERNORSHIP: u32 = 519;

    let entries: &[(&[&str], &[u32], u32)] = &[
        // ── Global iterators ─────────────────────────────────────────────
        (
            &[
                "every_character",
                "random_character",
                "any_character",
                "character",
            ],
            &[],
            CHARACTER,
        ),
        (
            &[
                "every_province",
                "random_province",
                "any_province",
                "province",
            ],
            &[],
            PROVINCE,
        ),
        (
            &["every_ruler", "random_ruler", "any_ruler"],
            &[],
            CHARACTER,
        ),
        // ── Character iterators ──────────────────────────────────────────
        (
            &["every_vassal", "random_vassal", "any_vassal"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &[
                "every_direct_vassal_character",
                "random_direct_vassal_character",
            ],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_child", "random_child", "any_child"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_sibling", "random_sibling", "any_sibling"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_spouse", "random_spouse", "any_spouse"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_courtier", "random_courtier", "any_courtier"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &[
                "every_close_family_member",
                "random_close_family_member",
                "any_close_family_member",
            ],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &[
                "every_extended_family_member",
                "random_extended_family_member",
            ],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &[
                "every_realm_province",
                "random_realm_province",
                "any_realm_province",
            ],
            &[CHARACTER],
            PROVINCE,
        ),
        (
            &["every_held_title", "random_held_title", "any_held_title"],
            &[CHARACTER],
            PROVINCE,
        ),
        (
            &["every_claim", "random_claim", "any_claim"],
            &[CHARACTER],
            PROVINCE,
        ),
        // ── Province iterators ───────────────────────────────────────────
        (
            &[
                "every_neighbor_province",
                "random_neighbor_province",
                "any_neighbor_province",
            ],
            &[PROVINCE],
            PROVINCE,
        ),
        (
            &["every_county_province", "random_county_province"],
            &[PROVINCE],
            PROVINCE,
        ),
        // ── Character named links (CK3 equivalents of CK2 scopedEffects) ─
        (&["liege"], &[CHARACTER], CHARACTER),
        (&["top_liege"], &[CHARACTER], CHARACTER),
        (&["father"], &[CHARACTER], CHARACTER),
        (&["mother"], &[CHARACTER], CHARACTER),
        (&["spouse"], &[CHARACTER], CHARACTER),
        (&["betrothed"], &[CHARACTER], CHARACTER),
        (&["guardian"], &[CHARACTER], CHARACTER),
        (&["employer"], &[CHARACTER], CHARACTER),
        (&["host"], &[CHARACTER], CHARACTER),
        // ── Province/character links ─────────────────────────────────────
        (&["capital_province"], &[CHARACTER], PROVINCE),
        (&["holder"], &[PROVINCE], CHARACTER),
        (&["owner", "controller"], &[PROVINCE], CHARACTER),
        (&["location"], &[CHARACTER], PROVINCE),
        // ── Family ───────────────────────────────────────────────────────
        (&["family"], &[CHARACTER], FAMILY),
        (
            &[
                "every_family_member",
                "random_family_member",
                "any_family_member",
            ],
            &[FAMILY],
            CHARACTER,
        ),
        // ── Governorship/state ───────────────────────────────────────────
        (&["governor"], &[GOVERNORSHIP, STATE], CHARACTER),
        (
            &[
                "every_governorship",
                "random_governorship",
                "any_governorship",
            ],
            &[CHARACTER],
            GOVERNORSHIP,
        ),
        // ── Religion/culture stubs ────────────────────────────────────────
        (&["religion"], &[CHARACTER, PROVINCE], RELIGION),
        (&["culture"], &[CHARACTER, PROVINCE], CULTURE),
        (&["culture_group"], &[CULTURE], CULTURE_GROUP),
        // ── Combat ───────────────────────────────────────────────────────
        (
            &["every_combat_side", "random_combat_side", "any_combat_side"],
            &[COMBAT],
            CHARACTER,
        ),
        (&["commander"], &[UNIT], CHARACTER),
        // ── Pop iterators ─────────────────────────────────────────────────
        (
            &["every_pop", "random_pop", "any_pop"],
            &[PROVINCE, STATE],
            POP,
        ),
    ];

    load_entries(links, entries);
}

// ── VIC2 ─────────────────────────────────────────────────────────────────────

// IDs 600–620 (same scope set as CK3/IR).
//
// Source: VIC2Scopes.fs.  scopedEffects all commented out; links from CWT
// rules and VIC2 modding docs.
fn load_vic2_links(links: &mut FxHashMap<String, ScopeLink>) {
    const COUNTRY: u32 = 604;
    const CHARACTER: u32 = 605;
    const PROVINCE: u32 = 606;
    const UNIT: u32 = 608;
    const POP: u32 = 609;
    const PARTY: u32 = 611;
    const RELIGION: u32 = 612;
    const CULTURE: u32 = 613;
    const STATE: u32 = 617;

    let entries: &[(&[&str], &[u32], u32)] = &[
        // ── Global iterators ─────────────────────────────────────────────
        (
            &["every_country", "random_country", "any_country", "country"],
            &[],
            COUNTRY,
        ),
        (
            &[
                "every_province",
                "random_province",
                "any_province",
                "province",
            ],
            &[],
            PROVINCE,
        ),
        // ── Country-scoped iterators ─────────────────────────────────────
        (
            &[
                "every_owned_province",
                "random_owned_province",
                "any_owned_province",
            ],
            &[COUNTRY],
            PROVINCE,
        ),
        (
            &[
                "every_core_province",
                "random_core_province",
                "any_core_province",
            ],
            &[COUNTRY],
            PROVINCE,
        ),
        (
            &["every_controlled_province", "random_controlled_province"],
            &[COUNTRY],
            PROVINCE,
        ),
        (
            &["every_state", "random_state", "any_state"],
            &[COUNTRY],
            STATE,
        ),
        (
            &[
                "every_neighbor_country",
                "random_neighbor_country",
                "any_neighbor_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &["every_sphere_member", "random_sphere_member"],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &["every_vassal", "random_vassal", "any_vassal"],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &["every_pop", "random_pop", "any_pop"],
            &[COUNTRY, PROVINCE],
            POP,
        ),
        (&["every_party", "random_party"], &[COUNTRY], PARTY),
        // ── Province-scoped iterators ────────────────────────────────────
        (
            &["every_neighbor_province", "random_neighbor_province"],
            &[PROVINCE],
            PROVINCE,
        ),
        // ── Named links ──────────────────────────────────────────────────
        (&["owner"], &[PROVINCE], COUNTRY),
        (&["controller"], &[PROVINCE], COUNTRY),
        (&["capital"], &[COUNTRY], PROVINCE),
        (&["overlord"], &[COUNTRY], COUNTRY),
        (&["sphere_owner"], &[COUNTRY], COUNTRY),
        (&["union"], &[COUNTRY], COUNTRY),
        (&["ruling_party"], &[COUNTRY], PARTY),
        (&["primary_culture"], &[COUNTRY], CULTURE),
        (&["national_focus"], &[COUNTRY], PROVINCE),
        (&["religion"], &[COUNTRY, PROVINCE, POP], RELIGION),
        (&["culture"], &[COUNTRY, PROVINCE, POP], CULTURE),
        (&["location"], &[CHARACTER, UNIT], PROVINCE),
    ];

    load_entries(links, entries);
}

// ── IR (Imperator: Rome) ─────────────────────────────────────────────────────

// IDs 700–720 (same scope set as CK3/VIC2).
//
// Source: IRScopes.fs.  scopedEffects all commented out; additional links from
// IR CWT rules, modding docs, and the IR effects/triggers .log test files.
fn load_ir_links(links: &mut FxHashMap<String, ScopeLink>) {
    const COUNTRY: u32 = 704;
    const CHARACTER: u32 = 705;
    const PROVINCE: u32 = 706;
    const UNIT: u32 = 708;
    const POP: u32 = 709;
    const FAMILY: u32 = 710;
    const PARTY: u32 = 711;
    const RELIGION: u32 = 712;
    const CULTURE: u32 = 713;
    const STATE: u32 = 717;
    const SUBUNIT: u32 = 718;
    const GOVERNORSHIP: u32 = 719;
    const REGION: u32 = 720;

    let entries: &[(&[&str], &[u32], u32)] = &[
        // ── Global iterators ─────────────────────────────────────────────
        (
            &["every_country", "random_country", "any_country", "country"],
            &[],
            COUNTRY,
        ),
        (
            &[
                "every_province",
                "random_province",
                "any_province",
                "province",
            ],
            &[],
            PROVINCE,
        ),
        (
            &[
                "every_character",
                "random_character",
                "any_character",
                "character",
            ],
            &[],
            CHARACTER,
        ),
        (
            &[
                "every_ownable_province",
                "random_ownable_province",
                "any_ownable_province",
            ],
            &[],
            PROVINCE,
        ),
        // ── Country-scoped iterators (from IR effects.log / CWT rules) ───
        (
            &[
                "every_owned_province",
                "random_owned_province",
                "any_owned_province",
            ],
            &[COUNTRY],
            PROVINCE,
        ),
        (
            &[
                "every_allied_country",
                "random_allied_country",
                "any_allied_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &["every_subject", "random_subject", "any_subject"],
            &[COUNTRY],
            COUNTRY,
        ),
        (
            &[
                "every_neighbor_country",
                "random_neighbour_country",
                "any_neighbor_country",
                "random_neighbor_country",
            ],
            &[COUNTRY],
            COUNTRY,
        ),
        (&["every_army", "random_army", "any_army"], &[COUNTRY], UNIT),
        (&["every_navy", "random_navy", "any_navy"], &[COUNTRY], UNIT),
        (
            &[
                "every_country_state",
                "random_country_state",
                "any_country_state",
            ],
            &[COUNTRY],
            STATE,
        ),
        (
            &["every_governor_state", "random_governor_state"],
            &[CHARACTER],
            STATE,
        ),
        (
            &["every_successor", "random_successor"],
            &[COUNTRY],
            CHARACTER,
        ),
        // ── Province-scoped iterators ────────────────────────────────────
        (
            &[
                "every_neighbor_province",
                "random_neighbor_province",
                "any_neighbor_province",
            ],
            &[PROVINCE],
            PROVINCE,
        ),
        // ── Character-scoped iterators ───────────────────────────────────
        (
            &["every_child", "random_child", "any_child"],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &[
                "every_friend",
                "random_friend",
                "any_friend",
                "ordered_friend",
            ],
            &[CHARACTER],
            CHARACTER,
        ),
        (
            &["every_support_as_heir", "any_support_as_heir"],
            &[CHARACTER],
            CHARACTER,
        ),
        // ── Region/area iterators ────────────────────────────────────────
        (&["every_area", "random_area", "any_area"], &[], STATE),
        (
            &["every_region", "random_region", "any_region"],
            &[],
            REGION,
        ),
        (
            &["every_region_state", "random_region_state"],
            &[REGION],
            STATE,
        ),
        (
            &["every_area_province", "random_area_province"],
            &[STATE],
            PROVINCE,
        ),
        // ── Named links (from IRScopes.fs active entries) ─────────────────
        (&["owner"], &[PROVINCE], COUNTRY),
        (&["controller"], &[PROVINCE], COUNTRY),
        (&["capital"], &[COUNTRY], PROVINCE),
        (&["liege"], &[CHARACTER], CHARACTER),
        (&["employer"], &[CHARACTER], CHARACTER),
        (&["spouse"], &[CHARACTER], CHARACTER),
        (&["father"], &[CHARACTER], CHARACTER),
        (&["mother"], &[CHARACTER], CHARACTER),
        (&["top_liege"], &[CHARACTER], CHARACTER),
        (&["family"], &[CHARACTER], FAMILY),
        (&["location"], &[CHARACTER, UNIT, SUBUNIT], PROVINCE),
        (&["overlord"], &[COUNTRY], COUNTRY),
        // ── Governor/party/pop ────────────────────────────────────────────
        (&["governor"], &[GOVERNORSHIP, STATE], CHARACTER),
        (&["governorship"], &[CHARACTER], GOVERNORSHIP),
        (&["ruling_party"], &[COUNTRY], PARTY),
        (
            &["every_pop", "random_pop", "any_pop"],
            &[PROVINCE, COUNTRY, STATE],
            POP,
        ),
        (
            &[
                "every_family_member",
                "random_family_member",
                "any_family_member",
            ],
            &[FAMILY],
            CHARACTER,
        ),
        // ── Religion/culture stubs ────────────────────────────────────────
        (&["religion"], &[CHARACTER, PROVINCE, COUNTRY], RELIGION),
        (&["culture"], &[CHARACTER, PROVINCE, COUNTRY], CULTURE),
    ];

    load_entries(links, entries);
}

// ── VIC3 / EU5 ────────────────────────────────────────────────────────────────

// VIC3Scopes.fs and EU5Scopes.fs have all scopedEffects commented out and only
// the standard oneToOneScopes (THIS/ROOT/FROM/PREV chains).  Scope IDs are not
// assigned in constants.rs (empty arrays), so no links to register here.
// These stubs are kept so load_scope_links can dispatch cleanly.
fn load_vic3_links(_links: &mut FxHashMap<String, ScopeLink>) {
    // No game-specific scope data yet — VIC3 uses only the generic THIS/ROOT/etc.
}

fn load_eu5_links(_links: &mut FxHashMap<String, ScopeLink>) {
    // No game-specific scope data yet — EU5 uses only the generic THIS/ROOT/etc.
}

// validate_scope_field deleted: no callers and the implementation was incorrect.
