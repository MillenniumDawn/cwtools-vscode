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

        // Localisation file: a `$KEY$` under the cursor is a nested loc-key
        // reference. .yml isn't a game AST, so resolve it directly to the
        // referenced entry's text instead of the rule walk below.
        if crate::paths::is_loc_file(&uri) {
            return Ok(self.loc_ref_hover(&uri, pos));
        }

        // `.cwt` rule file: describe the type/enum/single_alias the construct
        // under the cursor references (no game rule walk).
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
            // `resolved_scope` is already `None` unless the resolved setting
            // is on (computed in rule_info_at_cursor).
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
            // For a variable read, append the known assigned value(s) so the
            // user sees what it resolves to without chasing the definition.
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
            // Append localisation translations. A reference resolves by leaf
            // value; a definition key (idea/decision) resolves by its key.
            append_localisation(&mut md, &element, &self.state.loc_text.read());
            // A reference to a type with a primary localisation (an event id,
            // …) shows the localised title, resolved from the captured
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

        // Fallback: no-rule position finder. With debug off this shows only
        // localisation (the raw `Field`/`Value` line is developer detail); if
        // there's nothing to show, return no hover rather than an empty box.
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
            // Append localisation translations for the hovered element.
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

    /// Hover inside a `.cwt` rule file: describe the type/enum/single_alias
    /// the construct under the cursor references, plus where it is defined.
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

    /// Hover for a `$KEY$` reference in a `.yml` loc file: show the referenced
    /// entry's translations. `None` when the cursor isn't on a known loc-key
    /// reference (e.g. a bare runtime variable with no loc entry).
    fn loc_ref_hover(&self, uri: &str, pos: Position) -> Option<Hover> {
        let (key, start, end) = self.loc_ref_at_cursor_doc(uri, pos)?;
        let loc_text = self.state.loc_text.read();
        // The map is keyed by ASCII-lowercased loc keys. Avoid the temp String
        // when the key is already lowercase (the common case).
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

/// The scope context at the cursor, rendered as the hover scope table. Mirrors
/// the small ROOT/PREV/FROM table the F# build showed. Names are already
/// resolved and placeholder-filtered by `rule_info_at_cursor`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ScopeTable<'a> {
    /// The scope the containing block evaluates in (current scope).
    pub current: Option<&'a str>,
    /// The outermost block's scope.
    pub root: Option<&'a str>,
    /// The enclosing scope, one level out from current.
    pub prev: Option<&'a str>,
    /// The FROM chain: `[0]` = FROM, `[1]` = FROM.FROM, ….
    pub from: &'a [String],
    /// The scope the hovered key resolves to (`hover.scopeDisplay = "resolved"`).
    /// Shown as a `Resolves to` line when set and different from `current`. (#37)
    pub resolved: Option<&'a str>,
}

/// If `type_name` is a subtype-qualified reference (`"type.subtype"`, from a
/// `<type.subtype>` back-reference) and the ruleset's subtype declares a
/// `## display_name`, swap it in for the raw subtype name. Otherwise returns
/// `type_name` unchanged. Mirrors `completion::builders::find_subtype`, kept
/// local since hover isn't a descendant of the completion module.
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

/// Build a Markdown hover string from the classified element + the matched
/// rule's category, description, and required scopes (from
/// `rule_info_at_cursor`).
///
/// By default this shows only the information a modder needs: the alias category
/// header (Trigger/Effect/Modifier), the rule description, and the required
/// scopes (plus localisation, appended by the caller). When `debug` is set the
/// raw rule classification (`Type reference` / `Field` / `Scope` / …) is added —
/// useful for extension developers, noise for everyone else.
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
    // Section 1 — identity + documentation ("what it is"): the alias-category
    // header (so a trigger reads as a trigger), the raw classification when
    // debug is on, and the rule description.
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

    // Section 2 — required scopes ("where it's allowed").
    let required = (!rule_scopes.is_empty()).then(|| {
        format!(
            "**{}**: {}",
            cwtools_i18n::t(cwtools_i18n::Key::HoverRequiredScopes),
            rule_scopes.join(", ")
        )
    });

    // Section 3 — the current scope at the cursor ("where you are"), independent
    // of the rule's required scope. Related scopes (ROOT/PREV and the FROM chain)
    // follow on consecutive hard-break lines, restoring the small scope table the
    // F# build showed. Root/Prev are suppressed when identical to the current
    // scope (noise); FROM/FROM.FROM are always shown when present.
    let scope_table = scopes.current.map(|scope| {
        let mut scope_lines = vec![format!(
            "**{}**: {}",
            cwtools_i18n::t(cwtools_i18n::Key::HoverScope),
            scope
        )];
        // The scope the hovered link/keyword resolves to, when the setting asks
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
        // Markdown collapses single newlines, so join the rows with a hard break.
        scope_lines.join("  \n")
    });

    // Join the logical sections with a horizontal rule so documentation, required
    // scope, and current scope read as distinct blocks instead of one run-on
    // paragraph (matches the F#/Tboby hover layout). (#38)
    let mut sections: Vec<String> = Vec::new();
    if !info.is_empty() {
        sections.push(info.join("\n\n"));
    }
    sections.extend(required);
    sections.extend(scope_table);
    sections.join("\n\n---\n\n")
}

/// Append localisation translations for the hovered element to `md`.
///
/// A reference (`add_ideas = DEN_Maersk1`) looks up the leaf value. A definition
/// key (`DEN_Maersk1 = { ... }`, value empty) looks up the key itself: for ideas,
/// decisions and similar entities the token IS the loc key, and the `<key>_desc`
/// entry holds the description tooltip. The cwt config doesn't model this, so it
/// can't be resolved through the rule walk.
///
/// Quoted leaf values (`"DEN_Maersk1"`) carry the surrounding `"…"` through
/// `leaf_value_to_string`; the loc index stores bare keys, so strip the quotes
/// before lookup. Without this a `value[...]` reference like
/// `has_country_flag = "my_war_flag"` fails to preview its localisation, since
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

/// Append the localised primary text (e.g. an event's title) for a reference to
/// a type that declares a primary localisation. Two resolution paths, matching
/// how the config models loc keys: an explicit-field binding (`title = title`)
/// uses the key captured from the instance's field at index time, while a
/// name-derived binding (`title = "$.t"`) builds `prefix + id + suffix`. The
/// first key that resolves to loc text is shown. Fixes the missing event title
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
    // Explicit-field key captured per instance (events: the `title` value).
    if let Some(k) = type_index.primary_loc_key(type_name, value) {
        keys.push(k.to_ascii_lowercase());
    }
    // Name-derived primary/required keys from the type's localisation config.
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

    /// A `RuleSet` with one type declaring one subtype, matching the
    /// `"type.subtype"` shape a `<type.subtype>` back-reference resolves to.
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
        // No `## display_name` declared: the raw "type.subtype" string shows,
        // same as before this field was wired in.
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
        // Default (debug off): the raw "Type reference" line is suppressed, but
        // the description and required scopes still show.
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
        // The current scope at the cursor renders even when the rule declares no
        // required scope, so a hover always shows where you are.
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
        // Root, Prev and the FROM chain render after the current scope so the
        // modder sees the whole scope table the F# build used to show.
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
        // current-scope table so they don't read as one run-on block.
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
        // current scope, a `Resolves to` line is shown.
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
        // Root/Prev are suppressed when missing or identical to the current scope
        // (noise), but FROM is always shown when the chain has it.
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
        // quotes through `leaf_value_to_string`. The loc index stores bare
        // keys, so the lookup must strip them; otherwise `value[...]`-shaped
        // references like `has_country_flag = "my_war_flag"` never preview
        // their loc.
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
        // The unquoted path keeps working: `name = my_idea` (LocalisationField
        // cases already passed, but assert the bare-key path explicitly so the
        // quote-stripping fix doesn't regress it).
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
        // A quoted key on a definition-side leaf (the key IS the loc key for
        // ideas/decisions) must also be unquoted before lookup.
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
        // Bare values (`LeafValue`) take the same path. A quoted leaf value
        // there must also be unquoted before the loc lookup.
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
        // Loc keys are lowercased. Quoted upper-case must still resolve.
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
        // Parser-adjacent whitespace is trimmed before quote detection. A value
        // that arrives as ` "my_war_flag" ` should still preview.
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
        // Inner whitespace inside the quotes is also trimmed. Defensive:
        // a value that arrives as `" my_war_flag "` (e.g. from a forgiving
        // formatter) should still preview, and `"   "` collapses to empty.
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
        // Only a balanced pair of surrounding double-quotes is stripped. A
        // single leading or trailing quote stays verbatim and must not
        // spuriously match the bare loc key.
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
        // `""` strips to "" and looks up the empty key. Must not panic and
        // must not produce a localisation section.
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
        // Unknown loc key leaves md untouched. Guards against false positives
        // where any non-empty hover would be mistaken for a localisation preview.
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
