use crate::scope::ScopeDef;
use crate::scope_engine::ScopeId;

/// Game identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Game {
    Hoi4,
    Stellaris,
    Eu4,
    Ck2,
    Ck3,
    Vic2,
    Vic3,
    Ir,
    Eu5,
    Custom,
}

impl std::fmt::Display for Game {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Game::Hoi4 => write!(f, "hoi4"),
            Game::Stellaris => write!(f, "stellaris"),
            Game::Eu4 => write!(f, "eu4"),
            Game::Ck2 => write!(f, "ck2"),
            Game::Ck3 => write!(f, "ck3"),
            Game::Vic2 => write!(f, "vic2"),
            Game::Vic3 => write!(f, "vic3"),
            Game::Ir => write!(f, "ir"),
            Game::Eu5 => write!(f, "eu5"),
            Game::Custom => write!(f, "custom"),
        }
    }
}

impl Game {
    // Returns Option (no parse error type), so it can't be the Result-returning
    // std::str::FromStr; keeping the conventional `from_str` name.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "hoi4" => Some(Game::Hoi4),
            "stellaris" | "stl" => Some(Game::Stellaris),
            "eu4" => Some(Game::Eu4),
            "ck2" => Some(Game::Ck2),
            "ck3" => Some(Game::Ck3),
            "vic2" => Some(Game::Vic2),
            "vic3" => Some(Game::Vic3),
            "ir" | "imperator" => Some(Game::Ir),
            "eu5" => Some(Game::Eu5),
            "custom" => Some(Game::Custom),
            _ => None,
        }
    }

    /// Scope definitions for this game (name, aliases, numeric id, subscope_of).
    pub fn scope_defs(&self) -> &'static [ScopeDef] {
        match self {
            // HOI4 scopes are loaded from `scopes.cwt` into the runtime
            // ScopeRegistry; there is no hardcoded table.
            Game::Hoi4 => &[],
            Game::Stellaris => STELLARIS_SCOPES,
            Game::Eu4 => EU4_SCOPES,
            Game::Ck2 => CK2_SCOPES,
            Game::Ck3 => CK3_SCOPES,
            Game::Vic2 => VIC2_SCOPES,
            Game::Vic3 => VIC3_SCOPES,
            Game::Ir => IR_SCOPES,
            Game::Eu5 => EU5_SCOPES,
            Game::Custom => CUSTOM_SCOPES,
        }
    }
}

// ── HOI4 ─────────────────────────────────────────────────────────────────────
// IDs: Country=100, State=101, Unit Leader=102, Air=103

// HOI4 scopes are now loaded from `scopes.cwt` into the runtime ScopeRegistry
// (see `scope_registry.rs`); the hardcoded `HOI4_SCOPES` table was removed.

// ── Stellaris ─────────────────────────────────────────────────────────────────
// IDs: Country=200, Leader=201, System=202, Planet=203, Ship=204, Fleet=205,
//      Pop=206, Army=207, Species=208, Pop Faction=209, Sector=210,
//      Federation=211, War=212, Megastructure=213, Design=214, Starbase=215,
//      Star=216, Deposit=217, Archaeological Site=218, Ambient Object=219

const STELLARIS_SCOPES: &[ScopeDef] = &[
    ScopeDef {
        name: "Country",
        aliases: &["country"],
        id: ScopeId(200),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Leader",
        aliases: &["leader"],
        id: ScopeId(201),
        subscope_of: &[],
    },
    ScopeDef {
        name: "System",
        aliases: &["galacticobject", "system", "galactic_object"],
        id: ScopeId(202),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Planet",
        aliases: &["planet"],
        id: ScopeId(203),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Ship",
        aliases: &["ship"],
        id: ScopeId(204),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Fleet",
        aliases: &["fleet"],
        id: ScopeId(205),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Pop",
        aliases: &["pop"],
        id: ScopeId(206),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Army",
        aliases: &["army"],
        id: ScopeId(207),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Species",
        aliases: &["species"],
        id: ScopeId(208),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Pop Faction",
        aliases: &["popfaction", "pop_faction"],
        id: ScopeId(209),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Sector",
        aliases: &["sector"],
        id: ScopeId(210),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Federation",
        aliases: &["alliance", "federation"],
        id: ScopeId(211),
        subscope_of: &[],
    },
    ScopeDef {
        name: "War",
        aliases: &["war"],
        id: ScopeId(212),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Megastructure",
        aliases: &["megastructure"],
        id: ScopeId(213),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Design",
        aliases: &["design"],
        id: ScopeId(214),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Starbase",
        aliases: &["starbase"],
        id: ScopeId(215),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Star",
        aliases: &["star"],
        id: ScopeId(216),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Deposit",
        aliases: &["deposit"],
        id: ScopeId(217),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Archaeological Site",
        aliases: &["archaeologicalsite", "archaeological_site"],
        id: ScopeId(218),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Ambient Object",
        aliases: &["ambientobject", "ambient_object"],
        id: ScopeId(219),
        subscope_of: &[],
    },
];

// ── EU4 ───────────────────────────────────────────────────────────────────────
// IDs: Country=300, Province=301, Trade Node=302 (subscope of Province),
//      Unit=303, Monarch=304, Heir=305, Consort=306, Rebel Faction=307,
//      Religion=308, Culture=309, Advisor=310
// F# source: CWTools/Common/EU4Constants.fs defaultScopes

const EU4_SCOPES: &[ScopeDef] = &[
    ScopeDef {
        name: "Country",
        aliases: &["country"],
        id: ScopeId(300),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Province",
        aliases: &["province"],
        id: ScopeId(301),
        subscope_of: &[],
    },
    // Trade Node isSubscopeOf Province (F# EU4Constants.fs line 9: [ "province" ])
    ScopeDef {
        name: "Trade Node",
        aliases: &["trade_node", "tradenode"],
        id: ScopeId(302),
        subscope_of: &[ScopeId(301)],
    },
    ScopeDef {
        name: "Unit",
        aliases: &["unit"],
        id: ScopeId(303),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Monarch",
        aliases: &["monarch"],
        id: ScopeId(304),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Heir",
        aliases: &["heir"],
        id: ScopeId(305),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Consort",
        aliases: &["consort"],
        id: ScopeId(306),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Rebel Faction",
        aliases: &["rebel_faction"],
        id: ScopeId(307),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Religion",
        aliases: &["religion"],
        id: ScopeId(308),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Culture",
        aliases: &["culture"],
        id: ScopeId(309),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Advisor",
        aliases: &["advisor"],
        id: ScopeId(310),
        subscope_of: &[],
    },
];

// ── CK2 ───────────────────────────────────────────────────────────────────────
// IDs: Character=400, Title=401, Province=402, Offmap=403, War=404,
//      Siege=405, Unit=406, Religion=407, Culture=408, Society=409,
//      Artifact=410, Bloodline=411, Wonder=412
// F# source: CWTools/Common/CK2Constants.fs defaultScopes

const CK2_SCOPES: &[ScopeDef] = &[
    ScopeDef {
        name: "Character",
        aliases: &["character"],
        id: ScopeId(400),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Title",
        aliases: &["title"],
        id: ScopeId(401),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Province",
        aliases: &["province"],
        id: ScopeId(402),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Offmap",
        aliases: &["offmap"],
        id: ScopeId(403),
        subscope_of: &[],
    },
    ScopeDef {
        name: "War",
        aliases: &["war"],
        id: ScopeId(404),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Siege",
        aliases: &["siege"],
        id: ScopeId(405),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Unit",
        aliases: &["unit"],
        id: ScopeId(406),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Religion",
        aliases: &["religion"],
        id: ScopeId(407),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Culture",
        aliases: &["culture"],
        id: ScopeId(408),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Society",
        aliases: &["society"],
        id: ScopeId(409),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Artifact",
        aliases: &["artifact"],
        id: ScopeId(410),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Bloodline",
        aliases: &["bloodline"],
        id: ScopeId(411),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Wonder",
        aliases: &["wonder"],
        id: ScopeId(412),
        subscope_of: &[],
    },
];

// ── CK3 ───────────────────────────────────────────────────────────────────────
// IDs: Value=500, Bool=501, Flag=502, Color=503, Country=504, Character=505,
//      Province=506, Combat=507, Unit=508, Pop=509, Family=510, Party=511,
//      Religion=512, Culture=513, Job=514, CultureGroup=515, Area=516,
//      State=517, Subunit=518, Governorship=519, Region=520
// F# source: CWTools/Common/CK3Constants.fs defaultScopes

const CK3_SCOPES: &[ScopeDef] = &[
    ScopeDef {
        name: "Value",
        aliases: &["value"],
        id: ScopeId(500),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Bool",
        aliases: &["bool"],
        id: ScopeId(501),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Flag",
        aliases: &["flag"],
        id: ScopeId(502),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Color",
        aliases: &["color"],
        id: ScopeId(503),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Country",
        aliases: &["country"],
        id: ScopeId(504),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Character",
        aliases: &["character"],
        id: ScopeId(505),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Province",
        aliases: &["province"],
        id: ScopeId(506),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Combat",
        aliases: &["combat"],
        id: ScopeId(507),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Unit",
        aliases: &["unit"],
        id: ScopeId(508),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Pop",
        aliases: &["pop"],
        id: ScopeId(509),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Family",
        aliases: &["family"],
        id: ScopeId(510),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Party",
        aliases: &["party"],
        id: ScopeId(511),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Religion",
        aliases: &["religion"],
        id: ScopeId(512),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Culture",
        aliases: &["culture"],
        id: ScopeId(513),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Job",
        aliases: &["job"],
        id: ScopeId(514),
        subscope_of: &[],
    },
    ScopeDef {
        name: "CultureGroup",
        aliases: &["culture group"],
        id: ScopeId(515),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Area",
        aliases: &["area"],
        id: ScopeId(516),
        subscope_of: &[],
    },
    ScopeDef {
        name: "State",
        aliases: &["state"],
        id: ScopeId(517),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Subunit",
        aliases: &["subunit"],
        id: ScopeId(518),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Governorship",
        aliases: &["governorship"],
        id: ScopeId(519),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Region",
        aliases: &["region"],
        id: ScopeId(520),
        subscope_of: &[],
    },
];

// ── VIC2 ──────────────────────────────────────────────────────────────────────
// F# VIC2Constants.fs has identical scope list to CK3 / IR.
// Using IDs 600-620 to avoid collision.

const VIC2_SCOPES: &[ScopeDef] = &[
    ScopeDef {
        name: "Value",
        aliases: &["value"],
        id: ScopeId(600),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Bool",
        aliases: &["bool"],
        id: ScopeId(601),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Flag",
        aliases: &["flag"],
        id: ScopeId(602),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Color",
        aliases: &["color"],
        id: ScopeId(603),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Country",
        aliases: &["country"],
        id: ScopeId(604),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Character",
        aliases: &["character"],
        id: ScopeId(605),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Province",
        aliases: &["province"],
        id: ScopeId(606),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Combat",
        aliases: &["combat"],
        id: ScopeId(607),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Unit",
        aliases: &["unit"],
        id: ScopeId(608),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Pop",
        aliases: &["pop"],
        id: ScopeId(609),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Family",
        aliases: &["family"],
        id: ScopeId(610),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Party",
        aliases: &["party"],
        id: ScopeId(611),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Religion",
        aliases: &["religion"],
        id: ScopeId(612),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Culture",
        aliases: &["culture"],
        id: ScopeId(613),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Job",
        aliases: &["job"],
        id: ScopeId(614),
        subscope_of: &[],
    },
    ScopeDef {
        name: "CultureGroup",
        aliases: &["culture group"],
        id: ScopeId(615),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Area",
        aliases: &["area"],
        id: ScopeId(616),
        subscope_of: &[],
    },
    ScopeDef {
        name: "State",
        aliases: &["state"],
        id: ScopeId(617),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Subunit",
        aliases: &["subunit"],
        id: ScopeId(618),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Governorship",
        aliases: &["governorship"],
        id: ScopeId(619),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Region",
        aliases: &["region"],
        id: ScopeId(620),
        subscope_of: &[],
    },
];

// ── VIC3 ──────────────────────────────────────────────────────────────────────
// No scope list. Filling it in by hand is the wrong direction: the scope tables
// are meant to come from the game's own .cwt config (#8, #13).

const VIC3_SCOPES: &[ScopeDef] = &[];

// ── IR (Imperator: Rome) ──────────────────────────────────────────────────────
// F# IRConstants.fs — same scope list as CK3 / VIC2.
// Using IDs 700-720.

const IR_SCOPES: &[ScopeDef] = &[
    ScopeDef {
        name: "Value",
        aliases: &["value"],
        id: ScopeId(700),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Bool",
        aliases: &["bool"],
        id: ScopeId(701),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Flag",
        aliases: &["flag"],
        id: ScopeId(702),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Color",
        aliases: &["color"],
        id: ScopeId(703),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Country",
        aliases: &["country"],
        id: ScopeId(704),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Character",
        aliases: &["character"],
        id: ScopeId(705),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Province",
        aliases: &["province"],
        id: ScopeId(706),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Combat",
        aliases: &["combat"],
        id: ScopeId(707),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Unit",
        aliases: &["unit"],
        id: ScopeId(708),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Pop",
        aliases: &["pop"],
        id: ScopeId(709),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Family",
        aliases: &["family"],
        id: ScopeId(710),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Party",
        aliases: &["party"],
        id: ScopeId(711),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Religion",
        aliases: &["religion"],
        id: ScopeId(712),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Culture",
        aliases: &["culture"],
        id: ScopeId(713),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Job",
        aliases: &["job"],
        id: ScopeId(714),
        subscope_of: &[],
    },
    ScopeDef {
        name: "CultureGroup",
        aliases: &["culture group"],
        id: ScopeId(715),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Area",
        aliases: &["area"],
        id: ScopeId(716),
        subscope_of: &[],
    },
    ScopeDef {
        name: "State",
        aliases: &["state"],
        id: ScopeId(717),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Subunit",
        aliases: &["subunit"],
        id: ScopeId(718),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Governorship",
        aliases: &["governorship"],
        id: ScopeId(719),
        subscope_of: &[],
    },
    ScopeDef {
        name: "Region",
        aliases: &["region"],
        id: ScopeId(720),
        subscope_of: &[],
    },
];

// ── EU5 ───────────────────────────────────────────────────────────────────────
// No scope list, same reason as VIC3 above (#8, #13).

const EU5_SCOPES: &[ScopeDef] = &[];

// ── Custom ────────────────────────────────────────────────────────────────────

const CUSTOM_SCOPES: &[ScopeDef] = &[];
