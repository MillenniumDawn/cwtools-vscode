use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    Hover, HoverContents, HoverParams, MarkupContent, MarkupKind, Position, Range,
};

use cwtools_info::{PositionElement, ReferenceHint};

use crate::RuleCursorInfo;
use crate::paths::{lang_display_name, logical_path_from_uri};
use crate::{Backend, LocTextMap};

impl Backend {
    pub(crate) async fn hover_impl(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let pos = params.text_document_position_params.position;

        if crate::paths::is_loc_file(&uri) {
            return Ok(self.loc_ref_hover(&uri, pos));
        }

        if crate::paths::is_cwt_file(&uri) {
            return Ok(self.cwt_hover(&uri, pos).await);
        }

        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);

        if let Some(RuleCursorInfo {
            element,
            hint,
            category,
            description: desc,
            required_scopes: scopes,
            current_scope,
            root_scope,
            prev_scope,
            from_scopes,
            resolved_scope,
        }) = self.rule_info_at_cursor(&uri, pos, &logical_path)
        {
            let debug = self
                .state
                .hover_debug
                .load(std::sync::atomic::Ordering::Relaxed);
            // Lock order: rules -> info_service -> loc_text.
            let rules_guard = self.state.rules.read();
            let mut md = build_hover_markdown(
                &element,
                &hint,
                category.as_deref(),
                desc.as_deref(),
                &scopes,
                ScopeTable {
                    current: current_scope.as_deref(),
                    root: root_scope.as_deref(),
                    prev: prev_scope.as_deref(),
                    from: &from_scopes,
                    resolved: resolved_scope.as_deref(),
                },
                debug,
                rules_guard.ruleset.as_deref(),
            );
            if let ReferenceHint::Variable { name, .. } = &hint {
                let info_guard = self.state.info_service.read();
                let (values, more) = info_guard.variable_values(name, 5);
                if !values.is_empty() {
                    let joined = values
                        .iter()
                        .map(|v| format!("`{}`", v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let suffix = if more { ", +more" } else { "" };
                    md.push_str(&format!("\n\nSet to: {}{}", joined, suffix));
                }
            }
            append_localisation(&mut md, &element, &self.state.loc_text.read());
            // explicit-field key or a name-derived key. (#40)
            if let ReferenceHint::TypeRef { type_name, value } = &hint {
                let info_guard = self.state.info_service.read();
                let loc_text = self.state.loc_text.read();
                if let Some(ruleset) = rules_guard.ruleset.as_ref() {
                    append_type_localisation(
                        &mut md,
                        type_name,
                        value,
                        &info_guard.type_index,
                        ruleset,
                        &loc_text,
                    );
                }
            }
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: md,
                }),
                range: None,
            }));
        }

        if let Some(element) = self.element_at_cursor(&uri, pos) {
            let debug = self
                .state
                .hover_debug
                .load(std::sync::atomic::Ordering::Relaxed);
            let mut contents = if debug {
                match &element {
                    PositionElement::Leaf { key, value } => {
                        format!("**Field**: `{} = {}`", key, value)
                    }
                    PositionElement::LeafValue { value } => {
                        format!("**Value**: `{}`", value)
                    }
                }
            } else {
                String::new()
            };
            append_localisation(&mut contents, &element, &self.state.loc_text.read());
            if !contents.trim().is_empty() {
                return Ok(Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: contents,
                    }),
                    range: None,
                }));
            }
        }
        Ok(None)
    }

    async fn cwt_hover(&self, uri: &str, pos: Position) -> Option<Hover> {
        use cwtools_rules::rules_types::CwtDefKind;
        let (kind, name) = self.cwt_ref_at_cursor(uri, pos).await?;
        let rules = self.state.rules.read();
        let rs = rules.ruleset.as_ref()?;
        let mut md = match kind {
            CwtDefKind::Type => {
                let mut s = format!("**type** `{}`", name);
                if let Some(&idx) = rs.type_by_name().get(&name) {
                    let paths = rs.types[idx].path_options.paths.join(", ");
                    if !paths.is_empty() {
                        s.push_str(&format!("\n\npath: {}", paths));
                    }
                }
                s
            }
            CwtDefKind::Enum => {
                let mut s = format!("**enum** `{}`", name);
                if let Some(&idx) = rs.enum_by_name().get(&name) {
                    let values = &rs.enums[idx].values;
                    let shown = values.iter().take(5).cloned().collect::<Vec<_>>();
                    let more = if values.len() > 5 { ", …" } else { "" };
                    s.push_str(&format!(
                        "\n\n{} values: {}{}",
                        values.len(),
                        shown.join(", "),
                        more
                    ));
                }
                s
            }
            CwtDefKind::SingleAlias => format!("**single_alias** `{}`", name),
        };
        if let Some(def) = rs
            .def_positions
            .iter()
            .find(|d| d.kind == kind && d.name == name)
        {
            md.push_str(&format!("\n\ndefined in `{}`", def.file.display()));
        }
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: None,
        })
    }

    fn loc_ref_hover(&self, uri: &str, pos: Position) -> Option<Hover> {
        let (key, start, end) = self.loc_ref_at_cursor_doc(uri, pos)?;
        let loc_text = self.state.loc_text.read();
        let translations = if key.bytes().any(|b| b.is_ascii_uppercase()) {
            let lower = key.to_lowercase();
            loc_text.get(lower.as_str())?
        } else {
            loc_text.get(key.as_str())?
        };
        let mut md = format!("**Localisation key** `{}`", key);
        for (lang, text) in translations {
            md.push_str(&format!("\n- {}: {}", lang_display_name(*lang), text));
        }
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: Some(Range {
                start: Position {
                    line: pos.line,
                    character: start,
                },
                end: Position {
                    line: pos.line,
                    character: end,
                },
            }),
        })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ScopeTable<'a> {
    pub current: Option<&'a str>,
    pub root: Option<&'a str>,
    pub prev: Option<&'a str>,
    pub from: &'a [String],
    /// Shown as a `Resolves to` line when set and different from `current`. (#37)
    pub resolved: Option<&'a str>,
}

fn display_type_name(
    ruleset: Option<&cwtools_rules::rules_types::RuleSet>,
    type_name: &str,
) -> String {
    let Some((base, sub)) = type_name.split_once('.') else {
        return type_name.to_string();
    };
    let display = ruleset.and_then(|rs| {
        let &i = rs.type_by_name().get(base)?;
        rs.types[i]
            .subtypes
            .iter()
            .find(|st| st.name == sub)?
            .display_name
            .as_deref()
    });
    match display {
        Some(d) => format!("{}.{}", base, d),
        None => type_name.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_hover_markdown(
    element: &PositionElement,
    hint: &ReferenceHint,
    category: Option<&str>,
    rule_desc: Option<&str>,
    rule_scopes: &[String],
    scopes: ScopeTable<'_>,
    debug: bool,
    ruleset: Option<&cwtools_rules::rules_types::RuleSet>,
) -> String {
    let mut info: Vec<String> = Vec::new();
    if let (Some(cat), PositionElement::Leaf { key, .. }) = (category, element) {
        let label = match cat {
            "trigger" => "Trigger",
            "effect" => "Effect",
            "modifier" => "Modifier",
            other => other,
        };
        info.push(format!("**{}** `{}`", label, key));
    }
    if debug {
        let line = match hint {
            ReferenceHint::TypeRef { type_name, value } => {
                format!(
                    "**Type reference** — `{}` (`{}`)",
                    value,
                    display_type_name(ruleset, type_name)
                )
            }
            ReferenceHint::EnumRef { enum_name, value } => {
                format!("**Enum value** — `{}` (member of `{}`)", value, enum_name)
            }
            ReferenceHint::LocRef { key } => format!("**Localisation key** — `{}`", key),
            ReferenceHint::FileRef { path } => format!("**File path** — `{}`", path),
            ReferenceHint::ScopeName { name } => format!("**Scope** — `{}`", name),
            ReferenceHint::Variable { name, namespace } => {
                format!("**Variable** — `{}` (namespace `{}`)", name, namespace)
            }
            ReferenceHint::Unknown => match element {
                PositionElement::Leaf { key, value } => {
                    format!("**Field** — `{} = {}`", key, value)
                }
                PositionElement::LeafValue { value } => format!("**Value** — `{}`", value),
            },
        };
        info.push(line);
    }
    if let Some(desc) = rule_desc {
        info.push(desc.to_string());
    }

    let required = (!rule_scopes.is_empty()).then(|| {
        format!(
            "**{}**: {}",
            cwtools_i18n::t(cwtools_i18n::Key::HoverRequiredScopes),
            rule_scopes.join(", ")
        )
    });

    let scope_table = scopes.current.map(|scope| {
        let mut scope_lines = vec![format!(
            "**{}**: {}",
            cwtools_i18n::t(cwtools_i18n::Key::HoverScope),
            scope
        )];
        // for it and it actually differs from the ambient scope. (#37)
        if let Some(resolved) = scopes.resolved.filter(|r| Some(*r) != scopes.current) {
            scope_lines.push(format!(
                "**{}**: {}",
                cwtools_i18n::t(cwtools_i18n::Key::HoverResolvesTo),
                resolved
            ));
        }
        if let Some(root) = scopes.root.filter(|r| Some(*r) != scopes.current) {
            scope_lines.push(format!("**Root**: {}", root));
        }
        if let Some(prev) = scopes.prev.filter(|p| Some(*p) != scopes.current) {
            scope_lines.push(format!("**Prev**: {}", prev));
        }
        if let Some(from) = scopes.from.first() {
            scope_lines.push(format!("**From**: {}", from));
        }
        if let Some(fromfrom) = scopes.from.get(1) {
            scope_lines.push(format!("**From.From**: {}", fromfrom));
        }
        scope_lines.join("  \n")
    });

    // paragraph (matches the F#/Tboby hover layout). (#38)
    let mut sections: Vec<String> = Vec::new();
    if !info.is_empty() {
        sections.push(info.join("\n\n"));
    }
    sections.extend(required);
    sections.extend(scope_table);
    sections.join("\n\n---\n\n")
}

/// `"my_war_flag"` is not a key in the loc map (#317).
pub(crate) fn append_localisation(
    md: &mut String,
    element: &PositionElement,
    loc_text: &LocTextMap,
) {
    let loc_key = |s: &str| {
        let s = s.trim();
        if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
            s[1..s.len() - 1].trim().to_lowercase()
        } else {
            s.to_lowercase()
        }
    };
    let (name_key, desc_key): (Option<String>, Option<String>) = match element {
        PositionElement::Leaf { key, value } if value.is_empty() => {
            let k = loc_key(key);
            (Some(k.clone()), Some(format!("{k}_desc")))
        }
        PositionElement::Leaf { value, .. } => (Some(loc_key(value)), None),
        PositionElement::LeafValue { value } => (Some(loc_key(value)), None),
    };
    let mut emit = |loc_key: &str, label: &str| {
        if let Some(translations) = loc_text.get(loc_key) {
            md.push_str(label);
            for (lang, text) in translations {
                md.push_str(&format!("\n- {}: {}", lang_display_name(*lang), text));
            }
        }
    };
    if let Some(nk) = name_key {
        emit(
            &nk,
            &format!(
                "\n\n---\n\n**{}**:",
                cwtools_i18n::t(cwtools_i18n::Key::HoverLocalisation)
            ),
        );
    }
    if let Some(dk) = desc_key {
        emit(
            &dk,
            &format!(
                "\n\n**{}**:",
                cwtools_i18n::t(cwtools_i18n::Key::HoverDescription)
            ),
        );
    }
}

/// the F# build showed (#40).
pub(crate) fn append_type_localisation(
    md: &mut String,
    type_name: &str,
    value: &str,
    type_index: &cwtools_info::TypeIndex,
    ruleset: &cwtools_rules::rules_types::RuleSet,
    loc_text: &LocTextMap,
) {
    let value = value.trim_matches('"');
    let mut keys: Vec<String> = Vec::new();
    if let Some(k) = type_index.primary_loc_key(type_name, value) {
        keys.push(k.to_ascii_lowercase());
    }
    if let Some(&i) = ruleset.type_by_name().get(type_name) {
        for loc in &ruleset.types[i].localisation {
            if loc.explicit_field.is_none() && (loc.primary || loc.required) {
                keys.push(loc.derived_key(value).to_ascii_lowercase());
            }
        }
    }
    for key in keys {
        if let Some(translations) = loc_text.get(key.as_str()) {
            md.push_str("\n\n---\n\n**Localisation**:");
            for (lang, text) in translations {
                md.push_str(&format!("\n- {}: {}", lang_display_name(*lang), text));
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hover_type_ref() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "ethos".to_string(),
                value: "my_ethos".to_string(),
            },
            &ReferenceHint::TypeRef {
                type_name: "ethoses".to_string(),
                value: "my_ethos".to_string(),
            },
            None,
            None,
            &[],
            ScopeTable::default(),
            true,
            None,
        );
        assert!(md.contains("Type reference"), "got: {}", md);
        assert!(md.contains("my_ethos"), "got: {}", md);
        assert!(md.contains("ethoses"), "got: {}", md);
    }

    fn ruleset_with_subtype(display_name: Option<&str>) -> cwtools_rules::rules_types::RuleSet {
        use cwtools_rules::rules_types::{PathOptions, RuleSet, SubTypeDefinition, TypeDefinition};

        let mut rs = RuleSet::new();
        rs.types.push(TypeDefinition {
            name: "event".to_string(),
            name_field: None,
            path_options: PathOptions::default(),
            subtypes: vec![SubTypeDefinition {
                name: "country".to_string(),
                display_name: display_name.map(str::to_string),
                abbreviation: None,
                rules: Vec::new(),
                type_key_field: None,
                starts_with: None,
                push_scope: None,
                localisation: Vec::new(),
                only_if_not: Vec::new(),
                modifiers: Vec::new(),
                type_key_filter: Vec::new(),
            }],
            type_key_filter: None,
            skip_root_key: Vec::new(),
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: false,
            localisation: Vec::new(),
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        rs.reindex();
        rs
    }

    #[test]
    fn test_hover_type_ref_prefers_subtype_display_name() {
        let rs = ruleset_with_subtype(Some("Country Event"));
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "add_ideas".to_string(),
                value: "my_event".to_string(),
            },
            &ReferenceHint::TypeRef {
                type_name: "event.country".to_string(),
                value: "my_event".to_string(),
            },
            None,
            None,
            &[],
            ScopeTable::default(),
            true,
            Some(&rs),
        );
        assert!(md.contains("event.Country Event"), "got: {}", md);
        assert!(!md.contains("`event.country`"), "got: {}", md);
    }

    #[test]
    fn test_hover_type_ref_unchanged_without_display_name() {
        let rs = ruleset_with_subtype(None);
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "add_ideas".to_string(),
                value: "my_event".to_string(),
            },
            &ReferenceHint::TypeRef {
                type_name: "event.country".to_string(),
                value: "my_event".to_string(),
            },
            None,
            None,
            &[],
            ScopeTable::default(),
            true,
            Some(&rs),
        );
        assert!(md.contains("`event.country`"), "got: {}", md);
    }

    #[test]
    fn test_hover_default_hides_classification() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "ethos".to_string(),
                value: "my_ethos".to_string(),
            },
            &ReferenceHint::TypeRef {
                type_name: "ethoses".to_string(),
                value: "my_ethos".to_string(),
            },
            None,
            Some("Pick an ethos"),
            &["country".to_string()],
            ScopeTable::default(),
            false,
            None,
        );
        assert!(!md.contains("Type reference"), "should hide debug: {}", md);
        assert!(md.contains("Pick an ethos"), "got: {}", md);
        assert!(md.contains("Required scopes"), "got: {}", md);
    }

    #[test]
    fn test_hover_enum_ref() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "kind".to_string(),
                value: "alpha".to_string(),
            },
            &ReferenceHint::EnumRef {
                enum_name: "my_enum".to_string(),
                value: "alpha".to_string(),
            },
            None,
            None,
            &[],
            ScopeTable::default(),
            true,
            None,
        );
        assert!(md.contains("Enum value"), "got: {}", md);
        assert!(md.contains("alpha"), "got: {}", md);
        assert!(md.contains("my_enum"), "got: {}", md);
    }

    #[test]
    fn test_hover_unknown_falls_back_to_raw() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "foo".to_string(),
                value: "bar".to_string(),
            },
            &ReferenceHint::Unknown,
            None,
            None,
            &[],
            ScopeTable::default(),
            true,
            None,
        );
        assert!(md.contains("foo") && md.contains("bar"), "got: {}", md);
    }

    #[test]
    fn test_hover_with_rule_description() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "kind".to_string(),
                value: "alpha".to_string(),
            },
            &ReferenceHint::EnumRef {
                enum_name: "my_enum".to_string(),
                value: "alpha".to_string(),
            },
            None,
            Some("The kind of this thing"),
            &["country".to_string()],
            ScopeTable::default(),
            false,
            None,
        );
        assert!(md.contains("The kind of this thing"), "got: {}", md);
        assert!(md.contains("Required scopes"), "got: {}", md);
    }

    #[test]
    fn test_hover_shows_current_scope() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "set_country_flag".to_string(),
                value: "my_flag".to_string(),
            },
            &ReferenceHint::Unknown,
            Some("effect"),
            None,
            &[],
            ScopeTable {
                current: Some("country"),
                ..Default::default()
            },
            false,
            None,
        );
        assert!(md.contains("**Scope**: country"), "got: {}", md);
    }

    #[test]
    fn test_hover_shows_related_scopes() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "set_country_flag".to_string(),
                value: "my_flag".to_string(),
            },
            &ReferenceHint::Unknown,
            Some("effect"),
            None,
            &[],
            ScopeTable {
                current: Some("state"),
                root: Some("country"),
                prev: Some("unit_leader"),
                from: &["combat".to_string(), "operation".to_string()],
                resolved: None,
            },
            false,
            None,
        );
        assert!(md.contains("**Scope**: state"), "got: {}", md);
        assert!(md.contains("**Root**: country"), "got: {}", md);
        assert!(md.contains("**Prev**: unit_leader"), "got: {}", md);
        assert!(md.contains("**From**: combat"), "got: {}", md);
        assert!(md.contains("**From.From**: operation"), "got: {}", md);
    }

    #[test]
    fn test_hover_separates_sections() {
        // #38: a horizontal rule divides description, required scope, and the
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "set_country_flag".to_string(),
                value: "my_flag".to_string(),
            },
            &ReferenceHint::Unknown,
            Some("effect"),
            Some("Sets a flag"),
            &["country".to_string()],
            ScopeTable {
                current: Some("country"),
                root: None,
                prev: None,
                from: &[],
                resolved: None,
            },
            false,
            None,
        );
        assert!(
            md.contains("---"),
            "expected a section separator, got: {}",
            md
        );
    }

    #[test]
    fn test_hover_resolved_scope_line() {
        // #37: when a resolved/target scope is supplied and differs from the
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "owner".to_string(),
                value: String::new(),
            },
            &ReferenceHint::Unknown,
            None,
            None,
            &[],
            ScopeTable {
                current: Some("state"),
                root: None,
                prev: None,
                from: &[],
                resolved: Some("country"),
            },
            false,
            None,
        );
        assert!(md.contains("**Resolves to**: country"), "got: {}", md);
    }

    #[test]
    fn test_hover_omits_absent_and_duplicate_scopes() {
        let md = build_hover_markdown(
            &PositionElement::Leaf {
                key: "set_country_flag".to_string(),
                value: "my_flag".to_string(),
            },
            &ReferenceHint::Unknown,
            Some("effect"),
            None,
            &[],
            ScopeTable {
                current: Some("country"),
                root: Some("country"), // Root == Scope, should be omitted
                prev: None,            // Prev absent, should be omitted
                from: &["country".to_string()], // From == Scope, but FROM always shown
                resolved: None,
            },
            false,
            None,
        );
        assert!(md.contains("**Scope**: country"), "got: {}", md);
        assert!(!md.contains("**Root**"), "got: {}", md);
        assert!(!md.contains("**Prev**"), "got: {}", md);
        assert!(md.contains("**From**: country"), "got: {}", md);
        assert!(!md.contains("**From.From**"), "got: {}", md);
    }

    fn loc_map_with(pairs: &[(&str, &str)]) -> LocTextMap {
        let mut m = LocTextMap::default();
        for (k, v) in pairs {
            m.insert(
                std::sync::Arc::<str>::from(*k),
                vec![(cwtools_localization::Lang::English, (*v).to_string())],
            );
        }
        m
    }

    #[test]
    fn test_append_localisation_strips_quoted_value() {
        // #317: a quoted leaf value (`"my_war_flag"`) carries the surrounding
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "has_country_flag".to_string(),
                value: "\"my_war_flag\"".to_string(),
            },
            &loc_map_with(&[("my_war_flag", "War Flag")]),
        );
        assert!(md.contains("War Flag"), "got: {}", md);
        assert!(md.contains("**Localisation**"), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_unquoted_still_works() {
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "name".to_string(),
                value: "my_idea".to_string(),
            },
            &loc_map_with(&[("my_idea", "My Idea")]),
        );
        assert!(md.contains("My Idea"), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_strips_quoted_definition_key() {
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "\"my_great_idea\"".to_string(),
                value: String::new(),
            },
            &loc_map_with(&[
                ("my_great_idea", "Great Idea"),
                ("my_great_idea_desc", "It is great."),
            ]),
        );
        assert!(md.contains("Great Idea"), "got: {}", md);
        assert!(md.contains("It is great."), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_strips_quoted_leaf_value_variant() {
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::LeafValue {
                value: "\"my_war_flag\"".to_string(),
            },
            &loc_map_with(&[("my_war_flag", "War Flag")]),
        );
        assert!(md.contains("War Flag"), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_quoted_case_insensitive() {
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "has_country_flag".to_string(),
                value: "\"MY_WAR_FLAG\"".to_string(),
            },
            &loc_map_with(&[("my_war_flag", "War Flag")]),
        );
        assert!(md.contains("War Flag"), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_outer_whitespace_with_quotes() {
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "has_country_flag".to_string(),
                value: " \"my_war_flag\" ".to_string(),
            },
            &loc_map_with(&[("my_war_flag", "War Flag")]),
        );
        assert!(md.contains("War Flag"), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_inner_whitespace_with_quotes() {
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "has_country_flag".to_string(),
                value: "\" my_war_flag \"".to_string(),
            },
            &loc_map_with(&[("my_war_flag", "War Flag")]),
        );
        assert!(md.contains("War Flag"), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_mismatched_quote_not_stripped() {
        // single leading or trailing quote stays verbatim and must not
        let loc = loc_map_with(&[("my_war_flag", "War Flag")]);
        for bad in [
            "\"my_war_flag",
            "my_war_flag\"",
            "\"my_war_flag'",
            "'my_war_flag\"",
        ] {
            let mut md = String::new();
            append_localisation(
                &mut md,
                &PositionElement::Leaf {
                    key: "has_country_flag".to_string(),
                    value: bad.to_string(),
                },
                &loc,
            );
            assert!(
                !md.contains("War Flag"),
                "mismatched quotes {bad:?} should not match, got: {}",
                md
            );
        }
    }

    #[test]
    fn test_append_localisation_empty_quoted_no_panic() {
        let mut md = String::new();
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "has_country_flag".to_string(),
                value: "\"\"".to_string(),
            },
            &loc_map_with(&[("my_war_flag", "War Flag")]),
        );
        assert!(!md.contains("War Flag"), "got: {}", md);
        assert!(!md.contains("**Localisation**"), "got: {}", md);
    }

    #[test]
    fn test_append_localisation_missing_key_no_section() {
        let mut md = String::from("prefix");
        append_localisation(
            &mut md,
            &PositionElement::Leaf {
                key: "has_country_flag".to_string(),
                value: "\"unknown_flag\"".to_string(),
            },
            &loc_map_with(&[("my_war_flag", "War Flag")]),
        );
        assert_eq!(md, "prefix", "got: {}", md);
    }
}
