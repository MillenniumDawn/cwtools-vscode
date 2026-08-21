//! Runtime scope/link registry — the data-driven replacement for the hardcoded
//! per-game scope tables. Built from `scopes.cwt` + `links.cwt` via
//! [`ScopeRegistry::from_config`] and held by every
//! [`crate::scope_engine::ScopeContext`]. Construction lives here (not in the
//! validation crate) so the scope graph has ONE source of truth: the config
//! inputs merged over the hardcoded tables, both owned by this crate.

use crate::constants::Game;
use crate::scope_engine::{SCOPE_ANY, SCOPE_INVALID, ScopeId, ScopeLink};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

/// A scope definition parsed from `scopes.cwt` (`Country = { aliases = { country } }`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeInput {
    /// The formal scope name (the block key), e.g. `Country`, `Special Project`.
    pub name: String,
    /// Alternative names (`aliases = { country }`); the first is the canonical short form.
    pub aliases: Vec<String>,
    /// Parent scopes (`is_subscope_of = { country }`), for hierarchical matching.
    pub is_subscope_of: Vec<String>,
}

/// A scope/event-target link parsed from `links.cwt`. Mirrors the fields the F#
/// engine reads (`UtilityParser.parseLink`).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkInput {
    /// Link name / key (e.g. `owner`, `state`, `var`).
    pub name: String,
    /// Resulting scope name (`output_scope = country`); `None` = any.
    pub output_scope: Option<String>,
    /// Scopes the link is valid in (`input_scopes`); empty = any.
    pub input_scopes: Vec<String>,
    /// Data prefix for parameterised links (`prefix = var:` / `sp:` / `event_target:`).
    pub prefix: Option<String>,
    /// `from_data = yes` — the link takes a data argument (state id, tag, value, …).
    pub from_data: bool,
    /// `data_source` entries (`<state>`, `enum[country_tags]`, `value[variable]`), may repeat.
    pub data_source: Vec<String>,
}

/// An owned scope definition (config-driven equivalent of the const `ScopeDef`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeDefOwned {
    pub name: String,
    pub aliases: Vec<String>,
    pub subscope_of: Vec<ScopeId>,
}

/// The scopes and links available to scope resolution, built from config.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScopeRegistry {
    /// id -> definition (name, aliases, parents).
    pub by_id: FxHashMap<ScopeId, ScopeDefOwned>,
    /// lowercased name AND every alias -> id.
    pub by_name: FxHashMap<String, ScopeId>,
    /// Named links / iterators (`owner`, `every_state`, …) keyed by lowercase name.
    pub links: FxHashMap<String, ScopeLink>,
    /// Prefix links (`var:`, `sp:`, `event_target:`), matched by key prefix.
    pub prefix_links: Vec<(String, ScopeLink)>,
}

impl ScopeRegistry {
    /// Canonical short name for a scope id (first alias, else `scope_N`). The
    /// sentinels resolve to `any` / `invalid`.
    pub fn name_of(&self, id: ScopeId) -> String {
        if id == SCOPE_ANY {
            return "any".to_string();
        }
        if id == SCOPE_INVALID {
            return "invalid".to_string();
        }
        match self.by_id.get(&id) {
            Some(d) => d.aliases.first().cloned().unwrap_or_else(|| d.name.clone()),
            None => format!("scope_{}", id.0),
        }
    }

    /// Resolve a scope name/alias (`country`, `Special Project`, `any`, `none`) to an id.
    /// `none` maps to `SCOPE_ANY` (F# anyScope semantics: unrestricted).
    #[inline]
    pub fn id_of(&self, name: &str) -> Option<ScopeId> {
        let trimmed = name.trim();
        if trimmed.eq_ignore_ascii_case("any")
            || trimmed.eq_ignore_ascii_case("all")
            || trimmed.eq_ignore_ascii_case("none")
        {
            return Some(SCOPE_ANY);
        }
        if trimmed.eq_ignore_ascii_case("invalid") {
            return Some(SCOPE_INVALID);
        }
        // Fast path: try the key as-is (already lowercase in the common case),
        // only allocate a lowercase copy when the borrowed lookup misses.
        if let Some(id) = self.by_name.get(trimmed) {
            return Some(*id);
        }
        if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
            return self.by_name.get(&trimmed.to_ascii_lowercase()).copied();
        }
        None
    }

    /// True when no config scopes were loaded (callers then fall back / stay lenient).
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Whether `current` satisfies `target`: the same scope, the wildcard, or a
    /// subscope of it (`is_subscope_of`, walked transitively). E.g. a `character`
    /// scope satisfies a `country` requirement because `Character is_subscope_of
    /// { country }` in scopes.cwt.
    #[inline]
    pub fn is_subscope_or_eq(&self, current: ScopeId, target: ScopeId) -> bool {
        if current == target || current == SCOPE_ANY || target == SCOPE_ANY {
            return true;
        }
        let mut stack: SmallVec<[ScopeId; 8]> = SmallVec::new();
        stack.push(current);
        let mut seen: SmallVec<[ScopeId; 8]> = SmallVec::new();
        while let Some(c) = stack.pop() {
            if c == target {
                return true;
            }
            if seen.contains(&c) {
                continue;
            }
            seen.push(c);
            if let Some(def) = self.by_id.get(&c) {
                stack.extend(def.subscope_of.iter().copied());
            }
        }
        false
    }

    /// Build a registry from the hardcoded `const` scope tables for games that
    /// don't (yet) load their scopes from config — Stellaris/EU4/etc. and unit
    /// tests. HOI4 is config-driven and returns an empty registry here.
    pub fn from_hardcoded(game: Game) -> Self {
        let mut reg = ScopeRegistry::default();
        for def in game.scope_defs() {
            let id = def.id;
            reg.by_name.insert(def.name.to_ascii_lowercase(), id);
            for a in def.aliases {
                reg.by_name.insert(a.to_ascii_lowercase(), id);
            }
            reg.by_id.insert(
                id,
                ScopeDefOwned {
                    name: def.name.to_string(),
                    aliases: def.aliases.iter().map(|s| s.to_string()).collect(),
                    subscope_of: def.subscope_of.to_vec(),
                },
            );
        }
        crate::scope_engine::load_scope_links(game, &mut reg.links);
        reg
    }

    /// Build the runtime registry from a parsed config (`scopes.cwt` +
    /// `links.cwt`). When the config carries no scope defs (e.g. a game without
    /// a scopes.cwt), fall back to that game's hardcoded table. A non-empty but
    /// PARTIAL config is backfilled from the hardcoded table so missing scopes
    /// don't silently resolve to `None`. This is the bridge that makes the
    /// scope engine data-driven.
    pub fn from_config(scope_inputs: &[ScopeInput], link_inputs: &[LinkInput], game: Game) -> Self {
        if scope_inputs.is_empty() {
            return ScopeRegistry::from_hardcoded(game);
        }
        let mut reg = ScopeRegistry::default();
        let mut next_id = 100u32;

        // Pass 1: assign ids and names. `any`/`all` -> sentinel ANY, `invalid` -> INVALID.
        for si in scope_inputs {
            let is_invalid = si.name.eq_ignore_ascii_case("invalid")
                || si.aliases.iter().any(|a| a.eq_ignore_ascii_case("invalid"));
            let is_any = si.name.eq_ignore_ascii_case("any")
                || si.aliases.iter().any(|a| a.eq_ignore_ascii_case("any"));
            let id = if is_invalid {
                SCOPE_INVALID
            } else if is_any {
                SCOPE_ANY
            } else {
                let id = ScopeId(next_id);
                next_id += 1;
                id
            };
            reg.by_name.insert(si.name.to_ascii_lowercase(), id);
            for a in &si.aliases {
                reg.by_name.insert(a.to_ascii_lowercase(), id);
            }
            if id != SCOPE_ANY && id != SCOPE_INVALID {
                reg.by_id.insert(
                    id,
                    ScopeDefOwned {
                        name: si.name.clone(),
                        aliases: si.aliases.clone(),
                        subscope_of: Vec::new(),
                    },
                );
            }
        }

        // Pass 2: resolve subscope_of names -> ids (resolve first, then assign).
        for si in scope_inputs {
            let Some(id) = reg.id_of(&si.name) else {
                continue;
            };
            let parents: Vec<ScopeId> = si
                .is_subscope_of
                .iter()
                .filter_map(|n| reg.id_of(n))
                .collect();
            if let Some(def) = reg.by_id.get_mut(&id) {
                def.subscope_of = parents;
            }
        }

        // Links: resolve output/input scope names -> ids; prefix links go to a
        // separate list matched by key prefix.
        for li in link_inputs {
            let target = li.output_scope.as_deref().and_then(|n| reg.id_of(n));
            let valid: Vec<ScopeId> = li
                .input_scopes
                .iter()
                .map(|n| {
                    reg.id_of(n).unwrap_or_else(|| {
                        // Keep the permissive any-scope fallback, but surface a
                        // typo'd scope name in links.cwt instead of hiding it.
                        tracing::warn!(
                            "links.cwt: link `{}` lists unknown input scope `{n}`; treating as any",
                            li.name
                        );
                        SCOPE_ANY
                    })
                })
                .collect();
            let link = ScopeLink {
                valid_scopes: valid,
                target,
                ignore_keys: Vec::new(),
            };
            match &li.prefix {
                Some(p) => reg.prefix_links.push((p.to_ascii_lowercase(), link)),
                None => {
                    reg.links.insert(li.name.to_ascii_lowercase(), link);
                }
            }
        }

        // Synthesize the simple iterators (`every_/random_/any_/all_<scope>`), which
        // links.cwt doesn't list (F# synthesizes them too). Relation iterators
        // (`every_owned_state`, …) are handled by the alias `## push_scope` path.
        let scope_aliases: Vec<(String, ScopeId)> = reg
            .by_id
            .iter()
            .flat_map(|(id, def)| {
                std::iter::once(&def.name)
                    .chain(def.aliases.iter())
                    .filter(|a| a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                    .map(move |a| (a.to_ascii_lowercase(), *id))
            })
            .collect();
        for (alias, id) in scope_aliases {
            for pre in ["every_", "random_", "any_", "all_"] {
                reg.links
                    .entry(format!("{pre}{alias}"))
                    .or_insert(ScopeLink {
                        valid_scopes: Vec::new(),
                        target: Some(id),
                        ignore_keys: Vec::new(),
                    });
            }
        }

        // A non-empty config can still be missing scopes/links that the game's
        // hardcoded tables define (a partially-loaded scopes.cwt). Backfill those so
        // they don't silently resolve to None. No-op for HOI4 (empty scope_defs);
        // matters for games with hardcoded tables and an incomplete config.
        backfill_hardcoded(&mut reg, game, &mut next_id);

        reg
    }
}

/// Merge any hardcoded scope/link for `game` that `reg` (built from config) does
/// not already define. Hardcoded ids live in their own space, so backfilled
/// scopes get fresh ids in `reg`'s space and every referenced id (subscope_of,
/// link target/valid scopes) is remapped through `hc_to_reg`.
fn backfill_hardcoded(reg: &mut ScopeRegistry, game: Game, next_id: &mut u32) {
    let hc = ScopeRegistry::from_hardcoded(game);
    if hc.is_empty() {
        return;
    }

    // Map each hardcoded id to its id in the merged registry: an existing reg id
    // if the config already defines that scope by name, else a fresh id.
    let mut hc_to_reg: FxHashMap<ScopeId, ScopeId> = FxHashMap::default();
    for (hid, hdef) in &hc.by_id {
        let existing = std::iter::once(&hdef.name)
            .chain(hdef.aliases.iter())
            .find_map(|n| reg.id_of(n));
        let rid = match existing {
            Some(id) => id,
            None => {
                let id = ScopeId(*next_id);
                *next_id += 1;
                reg.by_name.insert(hdef.name.to_ascii_lowercase(), id);
                for a in &hdef.aliases {
                    reg.by_name.insert(a.to_ascii_lowercase(), id);
                }
                reg.by_id.insert(
                    id,
                    ScopeDefOwned {
                        name: hdef.name.clone(),
                        aliases: hdef.aliases.clone(),
                        subscope_of: Vec::new(), // resolved below
                    },
                );
                id
            }
        };
        hc_to_reg.insert(*hid, rid);
    }

    let remap = |id: ScopeId| -> ScopeId {
        if id == SCOPE_ANY || id == SCOPE_INVALID {
            id
        } else {
            hc_to_reg.get(&id).copied().unwrap_or(id)
        }
    };

    // Resolve the backfilled scopes' parents now that every id is mapped. Only
    // touch scopes we just added (existing config scopes keep their parents).
    for (hid, hdef) in &hc.by_id {
        let rid = hc_to_reg[hid];
        if hdef.subscope_of.is_empty() {
            continue;
        }
        if let Some(def) = reg.by_id.get_mut(&rid)
            && def.subscope_of.is_empty()
        {
            def.subscope_of = hdef.subscope_of.iter().map(|p| remap(*p)).collect();
        }
    }

    // Backfill links/prefix-links the config didn't define, remapping their ids.
    for (k, link) in &hc.links {
        if reg.links.contains_key(k) {
            continue;
        }
        reg.links.insert(
            k.clone(),
            ScopeLink {
                valid_scopes: link.valid_scopes.iter().map(|s| remap(*s)).collect(),
                target: link.target.map(remap),
                ignore_keys: link.ignore_keys.clone(),
            },
        );
    }
    for (p, link) in &hc.prefix_links {
        if reg.prefix_links.iter().any(|(ep, _)| ep == p) {
            continue;
        }
        reg.prefix_links.push((
            p.clone(),
            ScopeLink {
                valid_scopes: link.valid_scopes.iter().map(|s| remap(*s)).collect(),
                target: link.target.map(remap),
                ignore_keys: link.ignore_keys.clone(),
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{LinkInput, ScopeInput, ScopeRegistry};
    use crate::constants::Game;

    fn country_only() -> Vec<ScopeInput> {
        vec![ScopeInput {
            name: "Country".to_string(),
            aliases: vec!["country".to_string()],
            is_subscope_of: Vec::new(),
        }]
    }

    /// A partial config (only `Country`) for a game with hardcoded scopes must
    /// still resolve the rest of that game's scopes via the hardcoded backfill,
    /// instead of silently dropping them to None.
    #[test]
    fn partial_config_backfills_hardcoded_scopes() {
        let reg = ScopeRegistry::from_config(&country_only(), &[], Game::Stellaris);

        // The config-declared scope resolves to exactly one id (not duplicated).
        let country = reg.id_of("country").expect("country resolves");
        assert_eq!(reg.id_of("Country"), Some(country));

        // Hardcoded Stellaris scopes absent from the partial config are backfilled.
        assert!(reg.id_of("planet").is_some(), "planet backfilled");
        assert!(reg.id_of("ship").is_some(), "ship backfilled");
        assert!(reg.id_of("leader").is_some(), "leader backfilled");
        let planet = reg.id_of("planet").expect("planet backfilled");
        let owner = reg.links.get("owner").expect("owner link backfilled");
        assert_eq!(owner.target, Some(country));
        assert!(owner.valid_scopes.contains(&planet));
        // System has several aliases; any of them must resolve to one id.
        let system = reg.id_of("system").expect("system backfilled");
        assert_eq!(reg.id_of("galactic_object"), Some(system));
    }

    #[test]
    fn config_registry_resolves_names_links_and_subscopes() {
        let reg = ScopeRegistry::from_config(
            &[
                ScopeInput {
                    name: "Country".to_string(),
                    aliases: vec!["country".to_string()],
                    is_subscope_of: Vec::new(),
                },
                ScopeInput {
                    name: "Character".to_string(),
                    aliases: vec!["character".to_string()],
                    is_subscope_of: vec!["country".to_string()],
                },
            ],
            &[LinkInput {
                name: "owner".to_string(),
                output_scope: Some("country".to_string()),
                input_scopes: vec!["country".to_string()],
                prefix: None,
                from_data: false,
                data_source: Vec::new(),
            }],
            Game::Hoi4,
        );
        let country = reg.id_of("country").expect("country resolves");
        let character = reg.id_of("character").expect("character resolves");

        assert_eq!(reg.id_of("Character"), Some(character));
        assert!(reg.is_subscope_or_eq(character, country));
        let owner = reg.links.get("owner").expect("owner link resolves");
        assert_eq!(owner.target, Some(country));
        assert_eq!(owner.valid_scopes, vec![country]);
    }

    /// HOI4 has no hardcoded scope table, so the backfill is a no-op: a config
    /// scope resolves, an unrelated name does not get invented.
    #[test]
    fn hoi4_backfill_is_noop() {
        let reg = ScopeRegistry::from_config(&country_only(), &[], Game::Hoi4);
        assert!(reg.id_of("country").is_some());
        assert!(reg.id_of("planet").is_none(), "no hardcoded HOI4 backfill");
    }

    /// #184: the redundant mixed-case "Alliance" alias was dropped from the
    /// Stellaris Federation scope. It was redundant because `id_of` already
    /// lowercases on its uppercase-fallback path, so `Alliance` resolves through
    /// the `alliance` alias. This pins that all three forms still map to the one
    /// Federation id after the removal.
    #[test]
    fn stellaris_federation_resolves_via_alliance_alias() {
        let reg = ScopeRegistry::from_hardcoded(Game::Stellaris);
        let fed = reg.id_of("federation").expect("federation scope resolves");
        assert_eq!(
            reg.id_of("alliance"),
            Some(fed),
            "the lowercase `alliance` alias maps to Federation",
        );
        assert_eq!(
            reg.id_of("Alliance"),
            Some(fed),
            "mixed-case `Alliance` resolves via id_of's uppercase fallback, so the \
             explicit `Alliance` alias entry was redundant",
        );
        assert_eq!(
            reg.id_of("Federation"),
            Some(fed),
            "the mixed-case scope name resolves too",
        );
    }
}
