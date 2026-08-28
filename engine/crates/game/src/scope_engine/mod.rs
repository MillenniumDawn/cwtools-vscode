//! Scope engine: the live scope-transition machine ([`engine`]).

mod engine;

pub use engine::{
    SCOPE_ANY, SCOPE_INVALID, SavedContext, ScopeContext, ScopeId, ScopeLink, ScopeResult,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::Game;
    use crate::scope_registry::{LinkInput, ScopeInput, ScopeRegistry};
    use std::sync::Arc;

    fn stl_ctx() -> ScopeContext {
        // Every game's hardcoded scope table is gone (#373), so `from_hardcoded`
        // returns an empty registry regardless of `Game::Stellaris` here — these
        // ids are opaque stack markers exercising PREV/FROM/ROOT mechanics, not
        // real scope resolution.
        ScopeContext::new(Game::Stellaris, ScopeId(200))
    }

    /// Small EU4-shaped config fixture: Country/Province scopes plus `owner`
    /// (Province -> Country) and `capital` (Country -> Province) links, enough
    /// to exercise the link-resolution tests below without the deleted
    /// hardcoded EU4 table.
    fn eu4_registry() -> Arc<ScopeRegistry> {
        let scopes = vec![
            ScopeInput {
                name: "Country".to_string(),
                aliases: vec!["country".to_string()],
                is_subscope_of: Vec::new(),
            },
            ScopeInput {
                name: "Province".to_string(),
                aliases: vec!["province".to_string()],
                is_subscope_of: Vec::new(),
            },
        ];
        let links = vec![
            LinkInput {
                name: "owner".to_string(),
                output_scope: Some("country".to_string()),
                input_scopes: vec!["province".to_string()],
                prefix: None,
                from_data: false,
                data_source: Vec::new(),
            },
            LinkInput {
                name: "capital".to_string(),
                output_scope: Some("province".to_string()),
                input_scopes: vec!["country".to_string()],
                prefix: None,
                from_data: false,
                data_source: Vec::new(),
            },
        ];
        Arc::new(ScopeRegistry::from_config(&scopes, &links, Game::Eu4))
    }

    // ── PREV chain tests ──────────────────────────────────────────────────────

    #[test]
    fn prev_single() {
        let mut ctx = stl_ctx();
        ctx.push_scope(ScopeId(203)); // now: [200, 203]
        let res = ctx.change_scope("prev");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(200),
                ignore_keys: vec![]
            }
        );
        // Stack after PREV: [200, 200] (hopped back to 200)
        assert_eq!(ctx.current(), ScopeId(200));
    }

    #[test]
    fn prevprev() {
        let mut ctx = stl_ctx();
        ctx.push_scope(ScopeId(203)); // [200, 203]
        ctx.push_scope(ScopeId(202)); // [200, 203, 202]
        let res = ctx.change_scope("prevprev");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(200),
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn prevprevprev() {
        let mut ctx = stl_ctx();
        ctx.push_scope(ScopeId(203));
        ctx.push_scope(ScopeId(202));
        ctx.push_scope(ScopeId(204));
        let res = ctx.change_scope("prevprevprev");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(200),
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn prevprevprevprev() {
        let mut ctx = stl_ctx();
        ctx.push_scope(ScopeId(203));
        ctx.push_scope(ScopeId(202));
        ctx.push_scope(ScopeId(204));
        ctx.push_scope(ScopeId(205));
        let res = ctx.change_scope("prevprevprevprev");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(200),
                ignore_keys: vec![]
            }
        );
    }

    // ── FROM chain tests ──────────────────────────────────────────────────────

    #[test]
    fn from_basic() {
        let mut ctx = stl_ctx();
        ctx.from.push(ScopeId(203)); // FROM = Planet
        let res = ctx.change_scope("from");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(203),
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn fromfrom() {
        let mut ctx = stl_ctx();
        ctx.from.push(ScopeId(203));
        ctx.from.push(ScopeId(202)); // FROMFROM = System
        let res = ctx.change_scope("fromfrom");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(202),
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn fromfromfrom() {
        let mut ctx = stl_ctx();
        ctx.from.push(ScopeId(203));
        ctx.from.push(ScopeId(202));
        ctx.from.push(ScopeId(204)); // FROMFROMFROM = Ship
        let res = ctx.change_scope("fromfromfrom");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(204),
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn fromfromfromfrom() {
        let mut ctx = stl_ctx();
        ctx.from.push(ScopeId(203));
        ctx.from.push(ScopeId(202));
        ctx.from.push(ScopeId(204));
        ctx.from.push(ScopeId(205)); // FROMFROMFROMFROM = Fleet
        let res = ctx.change_scope("fromfromfromfrom");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(205),
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn from_missing_returns_anyscope() {
        let mut ctx = stl_ctx();
        // No FROM set — should fall back to SCOPE_ANY
        let res = ctx.change_scope("from");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: SCOPE_ANY,
                ignore_keys: vec![]
            }
        );
    }

    // ── Dotted key tests ──────────────────────────────────────────────────────

    #[test]
    fn dotted_owner_capital() {
        // EU4: Province → owner (Country) → capital (Province)
        let reg = eu4_registry();
        let province = reg.id_of("province").expect("province resolves");
        let mut ctx = ScopeContext::from_registry(reg, province); // start in Province
        let res = ctx.change_scope("owner.capital");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: province,
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn dotted_single_segment_same_as_plain() {
        let reg = eu4_registry();
        let country = reg.id_of("country").expect("country resolves");
        let mut ctx_dot = ScopeContext::from_registry(reg.clone(), country);
        let mut ctx_plain = ScopeContext::from_registry(reg, country);
        let r1 = ctx_dot.change_scope("owner");
        let r2 = ctx_plain.change_scope("owner");
        assert_eq!(r1, r2);
    }

    // ── Prefix tests ──────────────────────────────────────────────────────────

    #[test]
    fn event_target_prefix_anyscope() {
        let mut ctx = stl_ctx();
        let res = ctx.change_scope("event_target:my_target");
        assert_eq!(res, ScopeResult::AnyScope);
    }

    #[test]
    fn parameter_prefix_anyscope() {
        let mut ctx = stl_ctx();
        let res = ctx.change_scope("parameter:x");
        assert_eq!(res, ScopeResult::AnyScope);
    }

    #[test]
    fn scope_prefix_anyscope() {
        let mut ctx = stl_ctx();
        let res = ctx.change_scope("scope:my_scope");
        assert_eq!(res, ScopeResult::AnyScope);
    }

    #[test]
    fn hidden_prefix_stripped() {
        // hidden:owner in EU4 Province should resolve like plain owner
        let reg = eu4_registry();
        let province = reg.id_of("province").expect("province resolves");
        let country = reg.id_of("country").expect("country resolves");
        let mut ctx = ScopeContext::from_registry(reg, province);
        let res = ctx.change_scope("hidden:owner");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: country,
                ignore_keys: vec![]
            }
        );
    }

    // ── Meta scope tests ──────────────────────────────────────────────────────

    #[test]
    fn root_returns_root() {
        let mut ctx = stl_ctx();
        ctx.push_scope(ScopeId(203));
        let res = ctx.change_scope("root");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(200),
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn this_returns_current() {
        let mut ctx = stl_ctx();
        ctx.push_scope(ScopeId(203));
        let res = ctx.change_scope("this");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(203),
                ignore_keys: vec![]
            }
        );
    }

    // ── Save / restore tests ──────────────────────────────────────────────────

    #[test]
    fn save_restore_roundtrip() {
        let mut ctx = stl_ctx();
        ctx.push_scope(ScopeId(203));
        ctx.from.push(ScopeId(202));
        let saved = ctx.save();
        ctx.push_scope(ScopeId(204));
        ctx.restore(saved);
        assert_eq!(ctx.current(), ScopeId(203));
        assert_eq!(ctx.from, vec![ScopeId(202)]);
    }

    // ── Game-specific link tests ──────────────────────────────────────────────

    #[test]
    fn hoi4_state_owner() {
        // HOI4 is config-driven: build a minimal registry (state -> owner ->
        // country) instead of the removed hardcoded table.
        let mut reg = ScopeRegistry::default();
        reg.links.insert(
            "owner".to_string(),
            ScopeLink {
                valid_scopes: vec![ScopeId(101)],
                target: Some(ScopeId(100)),
                ignore_keys: vec![],
            },
        );
        let mut ctx = ScopeContext::from_registry(Arc::new(reg), ScopeId(101)); // State
        let res = ctx.change_scope("owner");
        assert!(matches!(
            res,
            ScopeResult::NewScope {
                scope: ScopeId(100),
                ..
            }
        ));
    }

    #[test]
    fn stellaris_planet_owner() {
        // Small Stellaris-shaped fixture: Planet -> owner -> Country.
        let scopes = vec![
            ScopeInput {
                name: "Country".to_string(),
                aliases: vec!["country".to_string()],
                is_subscope_of: Vec::new(),
            },
            ScopeInput {
                name: "Planet".to_string(),
                aliases: vec!["planet".to_string()],
                is_subscope_of: Vec::new(),
            },
        ];
        let links = vec![LinkInput {
            name: "owner".to_string(),
            output_scope: Some("country".to_string()),
            input_scopes: vec!["planet".to_string()],
            prefix: None,
            from_data: false,
            data_source: Vec::new(),
        }];
        let reg = Arc::new(ScopeRegistry::from_config(&scopes, &links, Game::Stellaris));
        let planet = reg.id_of("planet").expect("planet resolves");
        let country = reg.id_of("country").expect("country resolves");
        let mut ctx = ScopeContext::from_registry(reg, planet);
        let res = ctx.change_scope("owner");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: country,
                ignore_keys: vec![]
            }
        );
    }

    #[test]
    fn eu4_province_owner_gives_country() {
        let reg = eu4_registry();
        let province = reg.id_of("province").expect("province resolves");
        let country = reg.id_of("country").expect("country resolves");
        let mut ctx = ScopeContext::from_registry(reg, province);
        let res = ctx.change_scope("owner");
        assert_eq!(
            res,
            ScopeResult::NewScope {
                scope: country,
                ignore_keys: vec![]
            }
        );
    }
}
