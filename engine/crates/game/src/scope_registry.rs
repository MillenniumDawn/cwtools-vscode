use crate::constants::Game;
use crate::scope_engine::{SCOPE_ANY, SCOPE_INVALID, ScopeId, ScopeLink};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeInput {
    pub name: String,
    pub aliases: Vec<String>,
    pub is_subscope_of: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkInput {
    pub name: String,
    pub output_scope: Option<String>,
    pub input_scopes: Vec<String>,
    pub prefix: Option<String>,
    pub from_data: bool,
    pub data_source: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeDefOwned {
    pub name: String,
    pub aliases: Vec<String>,
    pub subscope_of: Vec<ScopeId>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScopeRegistry {
    pub by_id: FxHashMap<ScopeId, ScopeDefOwned>,
    pub by_name: FxHashMap<String, ScopeId>,
    pub links: FxHashMap<String, ScopeLink>,
    pub prefix_links: Vec<(String, ScopeLink)>,
}

impl ScopeRegistry {
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
        if let Some(id) = self.by_name.get(trimmed) {
            return Some(*id);
        }
        if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
            return self.by_name.get(&trimmed.to_ascii_lowercase()).copied();
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

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

    pub fn from_config(
        scope_inputs: &[ScopeInput],
        link_inputs: &[LinkInput],
        _game: Game,
    ) -> Self {
        if scope_inputs.is_empty() {
            if !link_inputs.is_empty() {
                tracing::warn!(
                    "config declares {} link(s) but no scopes; dropping the links and disabling scope checks",
                    link_inputs.len()
                );
            }
            return ScopeRegistry::default();
        }
        let mut reg = ScopeRegistry::default();
        let mut next_id = 100u32;

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

        for li in link_inputs {
            let target = li.output_scope.as_deref().and_then(|n| reg.id_of(n));
            let valid: Vec<ScopeId> = li
                .input_scopes
                .iter()
                .map(|n| {
                    reg.id_of(n).unwrap_or_else(|| {
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

        reg
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

    #[test]
    fn config_is_the_only_scope_source() {
        let reg = ScopeRegistry::from_config(&country_only(), &[], Game::Stellaris);
        assert!(reg.id_of("country").is_some());
        assert!(reg.id_of("planet").is_none(), "no hardcoded backfill");
    }

    #[test]
    fn no_config_yields_empty_registry() {
        let reg = ScopeRegistry::from_config(&[], &[], Game::Stellaris);
        assert!(reg.is_empty());
        assert!(reg.id_of("country").is_none());
    }

    #[test]
    fn links_without_scopes_yield_empty_registry() {
        let reg = ScopeRegistry::from_config(
            &[],
            &[LinkInput {
                name: "owner".to_string(),
                output_scope: Some("country".to_string()),
                input_scopes: vec!["state".to_string()],
                prefix: None,
                from_data: false,
                data_source: Vec::new(),
            }],
            Game::Stellaris,
        );
        assert!(reg.is_empty());
        assert!(reg.links.is_empty());
    }

    #[test]
    fn every_alias_resolves_to_one_id() {
        let reg = ScopeRegistry::from_config(
            &[ScopeInput {
                name: "System".to_string(),
                aliases: vec!["system".to_string(), "galactic_object".to_string()],
                is_subscope_of: Vec::new(),
            }],
            &[],
            Game::Stellaris,
        );
        let id = reg.id_of("system").expect("system resolves");

        assert_eq!(reg.id_of("System"), Some(id));
        assert_eq!(reg.id_of("galactic_object"), Some(id));
        assert_eq!(reg.name_of(id), "system", "first alias is the short name");
        for link in ["every_system", "random_galactic_object"] {
            let l = reg.links.get(link).expect("iterator synthesized");
            assert_eq!(l.target, Some(id));
        }
    }
}
