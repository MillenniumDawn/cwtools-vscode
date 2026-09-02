use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tower_lsp::lsp_types::*;

use cwtools_game::scope_engine::{SCOPE_ANY, ScopeId};
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_info::InfoService;
use cwtools_rules::rules_types::{
    NewField, ParsedAliasPattern, PatternKind, RootRule, RuleSet, RuleType, SubTypeDefinition,
    TypeType, ValueType,
};
use cwtools_validation::scope_matches_required;

use super::generate_node_snippet;
use super::scope_names::scope_completion_names;
use super::snippets::{alias_completion_snippet, choice_list, quote_if_needed};
use super::sort_for_kind;

type ScopeCtx<'a> = Option<(ScopeId, &'a ScopeRegistry)>;

fn is_specific_requirement(required: &[String]) -> bool {
    !required.is_empty()
        && !required
            .iter()
            .any(|s| s.eq_ignore_ascii_case("any") || s.eq_ignore_ascii_case("all"))
}

#[derive(Clone, Copy)]
enum ScopeRank {
    SpecificMatch,
    Neutral,
    Mismatch,
}

impl ScopeRank {
    fn sort_text(self, kind: Option<CompletionItemKind>, label: &str) -> Option<String> {
        match self {
            ScopeRank::SpecificMatch => Some(format!("0_{}", label)),
            ScopeRank::Neutral => sort_for_kind(kind, label),
            ScopeRank::Mismatch => Some(format!("z_{}", label)),
        }
    }
}

pub(crate) struct BuildFilter<'a> {
    token: &'a str,
    budget: usize,
    dropped: usize,
}

impl<'a> BuildFilter<'a> {
    const BUDGET: usize = 4 * super::CONTEXT_CAP;

    pub(crate) fn new(token: &'a str) -> Self {
        Self {
            token,
            budget: Self::BUDGET,
            dropped: 0,
        }
    }

    pub(crate) fn dropped(&self) -> usize {
        self.dropped
    }

    fn admit(&mut self, label: &str) -> bool {
        let ok = self.budget > 0
            && (self.token.is_empty() || super::subsequence_match(label, self.token));
        if ok {
            self.budget -= 1;
        } else {
            self.dropped += 1;
        }
        ok
    }
}

fn recurses_into_category(rule: &RuleType, cat: &str) -> bool {
    let is_cat = |f: &NewField| matches!(f, NewField::AliasField(c) if c == cat);
    match rule {
        RuleType::LeafRule { left, right } => is_cat(left) || is_cat(right),
        RuleType::LeafValueRule { right } => is_cat(right),
        RuleType::NodeRule { left, rules } => {
            is_cat(left) || rules.iter().any(|(r, _)| recurses_into_category(r, cat))
        }
        RuleType::ValueClauseRule { rules } | RuleType::SubtypeRule { rules, .. } => {
            rules.iter().any(|(r, _)| recurses_into_category(r, cat))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResolveData {
    Alias { cat: String, name: String },
    Type { t: String },
    Enum { id: String },
}

impl ResolveData {
    fn into_value(self) -> Option<Value> {
        let s = match self {
            ResolveData::Alias { cat, name } => format!("alias:{cat}:{name}"),
            ResolveData::Type { t } => format!("type:{t}"),
            ResolveData::Enum { id } => format!("enum:{id}"),
        };
        Some(Value::String(s))
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        let (kind, rest) = s.split_once(':')?;
        match kind {
            "alias" => {
                let (cat, name) = rest.split_once(':')?;
                Some(ResolveData::Alias {
                    cat: cat.to_string(),
                    name: name.to_string(),
                })
            }
            "type" => Some(ResolveData::Type {
                t: rest.to_string(),
            }),
            "enum" => Some(ResolveData::Enum {
                id: rest.to_string(),
            }),
            _ => None,
        }
    }
}

pub(crate) fn apply_label_details(items: &mut [CompletionItem]) {
    for item in items {
        let Some(data) = item.data.as_ref().and_then(|d| d.as_str()) else {
            continue;
        };
        let description = match ResolveData::parse(data) {
            Some(ResolveData::Type { t }) => t,
            Some(ResolveData::Enum { id }) => format!("enum {id}"),
            Some(ResolveData::Alias { cat, .. }) => cat,
            None => continue,
        };
        item.label_details = Some(CompletionItemLabelDetails {
            detail: None,
            description: Some(description),
        });
    }
}

#[tracing::instrument(skip_all, fields(rules = rules.len()))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn completions_from_rules(
    rules: &[(RuleType, cwtools_rules::rules_types::Options)],
    ruleset: &RuleSet,
    info: &InfoService,
    language: &str,
    modifier_keys: &HashSet<String>,
    modifier_scopes: &HashMap<String, Vec<String>>,
    registry: Option<&cwtools_game::scope_registry::ScopeRegistry>,
    current_scope: Option<ScopeId>,
    token: &str,
) -> (Vec<CompletionItem>, usize) {
    let mut flt = BuildFilter::new(token);
    let mut items: Vec<CompletionItem> = Vec::new();
    // Per-request memo so a repeated enum is only collected/sorted once (#46).
    let mut enum_cache: HashMap<String, Vec<String>> = HashMap::new();
    // appear in this block (#44).
    let mut scope_names: Option<Vec<String>> = None;
    let scope_ctx: ScopeCtx = match (current_scope, registry) {
        (Some(s), Some(reg)) if s != SCOPE_ANY => Some((s, reg)),
        _ => None,
    };
    // category triggered them, so emit them at most once per block (#76).
    let mut scope_links_emitted = false;
    let mut seen_alias_cats: HashSet<&str> = HashSet::new();

    for (rule_type, opts) in rules {
        match rule_type {
            RuleType::LeafRule {
                left: NewField::SpecificField(k),
                right,
            } => push_specific_leaf_key(&mut items, k, right, opts, ruleset),
            RuleType::NodeRule {
                left: NewField::SpecificField(k),
                rules: inner,
            } => push_specific_node_key(&mut items, k, inner, opts, ruleset),
            RuleType::LeafRule {
                left: NewField::ValueField(ValueType::Enum(e)),
                right,
            } => push_enum_keyed_leaf(
                &mut items,
                &mut enum_cache,
                ruleset,
                info,
                e,
                right,
                &mut flt,
            ),
            RuleType::NodeRule {
                left: NewField::ValueField(ValueType::Enum(e)),
                ..
            } => push_enum_keyed_node(&mut items, &mut enum_cache, ruleset, info, e, &mut flt),
            RuleType::LeafRule {
                left: NewField::TypeField(TypeType::Simple(t)),
                right: NewField::AliasField(_) | NewField::ValueField(_) | NewField::ScalarField,
            }
            | RuleType::NodeRule {
                left: NewField::TypeField(TypeType::Simple(t)),
                ..
            } => {
                let style = if matches!(rule_type, RuleType::NodeRule { .. }) {
                    TypeInstanceStyle::NodeKey
                } else {
                    TypeInstanceStyle::LeafKey
                };
                push_type_instances(&mut items, info, t, style, &mut flt);
            }
            RuleType::LeafValueRule {
                right: NewField::ValueField(ValueType::Enum(e)),
            } => push_enum_leaf_values(&mut items, &mut enum_cache, ruleset, info, e, &mut flt),
            RuleType::LeafValueRule {
                right: NewField::TypeField(TypeType::Simple(t)),
            }
            | RuleType::LeafRule {
                right: NewField::TypeField(TypeType::Simple(t)),
                ..
            } => push_type_instances(&mut items, info, t, TypeInstanceStyle::Reference, &mut flt),
            RuleType::LeafRule {
                right: NewField::AliasField(cat),
                ..
            }
            | RuleType::LeafValueRule {
                right: NewField::AliasField(cat),
            }
            | RuleType::NodeRule {
                left: NewField::AliasField(cat),
                ..
            } => {
                if seen_alias_cats.insert(cat.as_str()) {
                    push_alias_keys(
                        &mut items,
                        ruleset,
                        info,
                        modifier_keys,
                        modifier_scopes,
                        cat,
                        scope_ctx,
                        &mut flt,
                    );
                }
                // (#76). The resolution machinery already backs goto/hover.
                if !scope_links_emitted
                    && ruleset
                        .alias_categories()
                        .get(cat)
                        .is_some_and(|c| c.scope_field_idx.is_some())
                {
                    push_scope_link_keys(&mut items, ruleset, info, &mut flt);
                    scope_links_emitted = true;
                }
            }
            // (cwtools-vscode#65).
            RuleType::LeafRule {
                left: NewField::AliasValueKeysField(cat),
                ..
            } if seen_alias_cats.insert(cat.as_str()) => push_alias_keys(
                &mut items,
                ruleset,
                info,
                modifier_keys,
                modifier_scopes,
                cat,
                scope_ctx,
                &mut flt,
            ),
            RuleType::LeafRule {
                right: NewField::ScopeField(_),
                ..
            }
            | RuleType::LeafValueRule {
                right: NewField::ScopeField(_),
            } => {
                let names =
                    scope_names.get_or_insert_with(|| scope_completion_names(language, registry));
                push_scope_names(&mut items, names);
            }
            RuleType::LeafValueRule {
                right: NewField::ValueField(ValueType::Bool),
            } => push_bool_leaf_values(&mut items),
            _ => {}
        }
    }

    // the most specific snippet) (cwtools-vscode#66).
    let mut seen_labels: HashSet<String> = HashSet::new();
    items.retain(|item| seen_labels.insert(item.label.clone()));

    (items, flt.dropped())
}

/// for short enums, a bare tab stop otherwise — cwtools-vscode#16).
fn push_specific_leaf_key(
    items: &mut Vec<CompletionItem>,
    k: &str,
    right: &NewField,
    opts: &cwtools_rules::rules_types::Options,
    ruleset: &RuleSet,
) {
    let snippet = match right {
        NewField::ValueField(ValueType::Bool) => Some(format!("{} = ${{1|yes,no|}}", k)),
        NewField::ValueField(ValueType::Enum(e)) => {
            let vals = enum_values_for(ruleset, e);
            if !vals.is_empty() && vals.len() <= 20 {
                Some(format!("{} = ${{1|{}|}}", k, choice_list(vals)))
            } else {
                Some(format!("{} = $0", k))
            }
        }
        // a bare `key` with no operator (cwtools-vscode#16).
        _ => Some(format!("{} = $0", k)),
    };
    items.push(CompletionItem {
        label: k.to_string(),
        kind: Some(CompletionItemKind::FIELD),
        detail: opts.description.clone(),
        insert_text: snippet.clone(),
        insert_text_format: if snippet.is_some() {
            Some(InsertTextFormat::SNIPPET)
        } else {
            None
        },
        sort_text: sort_for_kind(Some(CompletionItemKind::FIELD), k),
        ..Default::default()
    });
}

fn push_specific_node_key(
    items: &mut Vec<CompletionItem>,
    k: &str,
    inner: &[(RuleType, cwtools_rules::rules_types::Options)],
    opts: &cwtools_rules::rules_types::Options,
    ruleset: &RuleSet,
) {
    let snippet = generate_node_snippet(k, inner, ruleset);
    let sort = if !opts.required_scopes.is_empty() {
        format!("0_{}", k)
    } else {
        format!("1_{}", k)
    };
    items.push(CompletionItem {
        label: k.to_string(),
        kind: Some(CompletionItemKind::STRUCT),
        detail: opts.description.clone(),
        insert_text: Some(snippet),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        sort_text: Some(sort),
        ..Default::default()
    });
}

fn push_enum_keyed_leaf(
    items: &mut Vec<CompletionItem>,
    enum_cache: &mut HashMap<String, Vec<String>>,
    ruleset: &RuleSet,
    info: &InfoService,
    e: &str,
    right: &NewField,
    flt: &mut BuildFilter,
) {
    let snippet_value = match right {
        NewField::ValueField(ValueType::Bool) => "${1|yes,no|}".to_string(),
        _ => "${1}".to_string(),
    };
    for v in all_enum_values_cached(enum_cache, ruleset, info, e) {
        if !flt.admit(v) {
            continue;
        }
        items.push(CompletionItem {
            label: v.clone(),
            kind: Some(CompletionItemKind::FIELD),
            data: ResolveData::Enum { id: e.to_string() }.into_value(),
            insert_text: Some(format!("{} = {}", v, snippet_value)),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            sort_text: sort_for_kind(Some(CompletionItemKind::FIELD), v),
            ..Default::default()
        });
    }
}

fn push_enum_keyed_node(
    items: &mut Vec<CompletionItem>,
    enum_cache: &mut HashMap<String, Vec<String>>,
    ruleset: &RuleSet,
    info: &InfoService,
    e: &str,
    flt: &mut BuildFilter,
) {
    for v in all_enum_values_cached(enum_cache, ruleset, info, e) {
        if !flt.admit(v) {
            continue;
        }
        items.push(CompletionItem {
            label: v.clone(),
            kind: Some(CompletionItemKind::STRUCT),
            data: ResolveData::Enum { id: e.to_string() }.into_value(),
            insert_text: Some(format!("{} = {{\n\t$0\n}}", v)),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            sort_text: sort_for_kind(Some(CompletionItemKind::STRUCT), v),
            ..Default::default()
        });
    }
}

fn push_enum_leaf_values(
    items: &mut Vec<CompletionItem>,
    enum_cache: &mut HashMap<String, Vec<String>>,
    ruleset: &RuleSet,
    info: &InfoService,
    e: &str,
    flt: &mut BuildFilter,
) {
    for v in all_enum_values_cached(enum_cache, ruleset, info, e) {
        if !flt.admit(v) {
            continue;
        }
        items.push(CompletionItem {
            label: v.clone(),
            kind: Some(CompletionItemKind::ENUM_MEMBER),
            data: ResolveData::Enum { id: e.to_string() }.into_value(),
            sort_text: sort_for_kind(Some(CompletionItemKind::ENUM_MEMBER), v),
            ..Default::default()
        });
    }
}

enum TypeInstanceStyle {
    NodeKey,
    LeafKey,
    Reference,
}

fn find_subtype<'a>(ruleset: &'a RuleSet, t: &str) -> Option<&'a SubTypeDefinition> {
    let (base, sub) = t.split_once('.')?;
    let &i = ruleset.type_by_name().get(base)?;
    ruleset.types[i].subtypes.iter().find(|st| st.name == sub)
}

#[tracing::instrument(skip_all, fields(type_name = %t))]
fn push_type_instances(
    items: &mut Vec<CompletionItem>,
    info: &InfoService,
    t: &str,
    style: TypeInstanceStyle,
    flt: &mut BuildFilter,
) {
    for (_, inst) in info.type_index.instances(t) {
        if !flt.admit(&inst.name) {
            continue;
        }
        let (kind, insert_text) = match style {
            TypeInstanceStyle::NodeKey => (
                CompletionItemKind::STRUCT,
                Some(format!("{} = {{\n\t$0\n}}", inst.name)),
            ),
            TypeInstanceStyle::LeafKey => (
                CompletionItemKind::STRUCT,
                Some(format!("{} = ${{1}}", inst.name)),
            ),
            TypeInstanceStyle::Reference => (CompletionItemKind::REFERENCE, None),
        };
        items.push(CompletionItem {
            label: inst.name.clone(),
            kind: Some(kind),
            data: ResolveData::Type { t: t.to_string() }.into_value(),
            insert_text_format: insert_text.as_ref().map(|_| InsertTextFormat::SNIPPET),
            insert_text,
            sort_text: sort_for_kind(Some(kind), &inst.name),
            ..Default::default()
        });
    }
}

pub(crate) fn type_instance_detail(
    ruleset: Option<&RuleSet>,
    info: &InfoService,
    t: &str,
    name: &str,
) -> Option<String> {
    info.type_index
        .instances(t)
        .iter()
        .any(|(_, inst)| inst.name == name)
        .then(|| {
            let display_name = ruleset
                .and_then(|rs| find_subtype(rs, t))
                .and_then(|st| st.display_name.as_deref());
            match display_name {
                Some(d) => {
                    let base = t.split_once('.').map_or(t, |(base, _)| base);
                    format!("{}.{} instance", base, d)
                }
                None => format!("{} instance", t),
            }
        })
}

/// KEYWORD item (cwtools-vscode#64).
/// scope-specific requirement is ranked into the top bucket (#78). The scope test
#[allow(clippy::too_many_arguments)]
fn push_alias_keys(
    items: &mut Vec<CompletionItem>,
    ruleset: &RuleSet,
    info: &InfoService,
    modifier_keys: &HashSet<String>,
    modifier_scopes: &HashMap<String, Vec<String>>,
    cat: &str,
    scope: ScopeCtx,
    flt: &mut BuildFilter,
) {
    let prefix = format!("{}:", cat);
    let verdicts: Option<HashMap<&str, (bool, bool)>> = scope.map(|(current, reg)| {
        let mut m: HashMap<&str, (bool, bool)> = HashMap::new();
        for (alias_name, (_, opts)) in &ruleset.aliases {
            let Some(k) = alias_name.strip_prefix(&prefix) else {
                continue;
            };
            if k == "scope_field" || ParsedAliasPattern::parse(k, 0).is_some() {
                continue;
            }
            let matches = scope_matches_required(current, reg, &opts.required_scopes);
            let specific = matches && is_specific_requirement(&opts.required_scopes);
            let e = m.entry(k).or_insert((false, false));
            e.0 |= matches;
            e.1 |= specific;
        }
        m
    });
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (alias_name, (rule, _)) in &ruleset.aliases {
        let Some(k) = alias_name.strip_prefix(&prefix) else {
            continue;
        };
        if k == "scope_field" {
            continue;
        }
        if ParsedAliasPattern::parse(k, 0).is_some() {
            continue;
        }
        // contexts), so a valid key must never silently vanish (#78).
        let mut scope_rank = match verdicts.as_ref().and_then(|v| v.get(k)) {
            Some(&(false, _)) => ScopeRank::Mismatch,
            Some(&(true, true)) => ScopeRank::SpecificMatch,
            _ => ScopeRank::Neutral,
        };
        // below every scope-matched plain effect (#94).
        if !matches!(scope_rank, ScopeRank::Mismatch) && recurses_into_category(rule, cat) {
            scope_rank = ScopeRank::SpecificMatch;
        }
        if let Some(&idx) = seen.get(k) {
            let item: &mut CompletionItem = &mut items[idx];
            if item.insert_text.is_none()
                && let Some(snip) = alias_completion_snippet(k, rule, ruleset)
            {
                item.insert_text = Some(snip);
                item.insert_text_format = Some(InsertTextFormat::SNIPPET);
            }
            continue;
        }
        seen.insert(k.to_string(), items.len());
        let snippet = alias_completion_snippet(k, rule, ruleset);
        let sort_text = scope_rank.sort_text(Some(CompletionItemKind::KEYWORD), k);
        let data = if alias_has_description(ruleset, cat, k) {
            ResolveData::Alias {
                cat: cat.to_string(),
                name: k.to_string(),
            }
            .into_value()
        } else {
            None
        };
        items.push(CompletionItem {
            label: k.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(cat.to_string()),
            data,
            insert_text_format: snippet.as_ref().map(|_| InsertTextFormat::SNIPPET),
            insert_text: snippet,
            sort_text,
            ..Default::default()
        });
    }

    if let Some(cat_idx) = ruleset.alias_categories().get(cat) {
        for pattern in &cat_idx.parsed_patterns {
            if !pattern.prefix.is_empty() || !pattern.suffix.is_empty() {
                continue;
            }
            let (_, (rule, _)) = &ruleset.aliases[pattern.alias_idx];
            match pattern.kind {
                PatternKind::Type => {
                    for (_, inst) in info.type_index.instances(&pattern.placeholder_name) {
                        if seen.contains_key(&inst.name) {
                            continue;
                        }
                        if !flt.admit(&inst.name) {
                            continue;
                        }
                        let snippet = alias_completion_snippet(&inst.name, rule, ruleset);
                        items.push(CompletionItem {
                            label: inst.name.clone(),
                            kind: Some(CompletionItemKind::KEYWORD),
                            detail: Some(cat.to_string()),
                            insert_text_format: snippet.as_ref().map(|_| InsertTextFormat::SNIPPET),
                            insert_text: snippet,
                            sort_text: sort_for_kind(Some(CompletionItemKind::KEYWORD), &inst.name),
                            ..Default::default()
                        });
                    }
                }
                PatternKind::Enum => {
                    for v in all_enum_values(ruleset, info, &pattern.placeholder_name) {
                        if seen.contains_key(&v) {
                            continue;
                        }
                        if !flt.admit(&v) {
                            continue;
                        }
                        let snippet = alias_completion_snippet(&v, rule, ruleset);
                        items.push(CompletionItem {
                            label: v.clone(),
                            kind: Some(CompletionItemKind::KEYWORD),
                            detail: Some(cat.to_string()),
                            insert_text_format: snippet.as_ref().map(|_| InsertTextFormat::SNIPPET),
                            insert_text: snippet,
                            sort_text: sort_for_kind(Some(CompletionItemKind::KEYWORD), &v),
                            ..Default::default()
                        });
                    }
                }
                PatternKind::Value => {
                    for v in info
                        .type_index
                        .value_set_values
                        .values(&pattern.placeholder_name)
                    {
                        if seen.contains_key(v) {
                            continue;
                        }
                        if !flt.admit(v) {
                            continue;
                        }
                        let snippet = alias_completion_snippet(v, rule, ruleset);
                        items.push(CompletionItem {
                            label: v.to_string(),
                            kind: Some(CompletionItemKind::KEYWORD),
                            detail: Some(cat.to_string()),
                            insert_text_format: snippet.as_ref().map(|_| InsertTextFormat::SNIPPET),
                            insert_text: snippet,
                            sort_text: sort_for_kind(Some(CompletionItemKind::KEYWORD), v),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }

    if cat == "modifier" {
        // scope is unknown (#78).
        for m in modifier_keys {
            if !flt.admit(m) {
                continue;
            }
            let scope_rank = match scope {
                Some((current, reg)) => {
                    let scopes = modifier_scopes.get(m).map(Vec::as_slice).unwrap_or(&[]);
                    if !scope_matches_required(current, reg, scopes) {
                        ScopeRank::Mismatch
                    } else if is_specific_requirement(scopes) {
                        ScopeRank::SpecificMatch
                    } else {
                        ScopeRank::Neutral
                    }
                }
                None => ScopeRank::Neutral,
            };
            let sort_text = scope_rank.sort_text(Some(CompletionItemKind::FIELD), m);
            items.push(CompletionItem {
                label: m.clone(),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some("modifier".to_string()),
                insert_text: Some(format!("{} = $0", m)),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                sort_text,
                ..Default::default()
            });
        }
    }
}

pub(crate) fn expanded_modifier_scopes(
    ruleset: &RuleSet,
    type_index: &cwtools_info::TypeIndex,
) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, category) in &ruleset.modifiers {
        let scopes = ruleset
            .modifier_categories
            .get(category)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        match (name.find('<'), name.find('>')) {
            (Some(open), Some(close)) if open < close => {
                let tn = &name[open + 1..close];
                let pre = &name[..open];
                let suf = &name[close + 1..];
                for (_, inst) in type_index.instances(tn) {
                    map.insert(
                        format!("{}{}{}", pre, inst.name, suf).to_lowercase(),
                        scopes.to_vec(),
                    );
                }
            }
            _ => {
                map.insert(name.to_lowercase(), scopes.to_vec());
            }
        }
    }
    map
}

/// item per type instance the link's `data_source` names (#76). Only prefixed
fn push_scope_link_keys(
    items: &mut Vec<CompletionItem>,
    ruleset: &RuleSet,
    info: &InfoService,
    flt: &mut BuildFilter,
) {
    const SCOPE_LINK_KEY_CAP: usize = 2000;
    let mut count = 0usize;
    for li in &ruleset.link_inputs {
        if !li.from_data {
            continue;
        }
        let Some(prefix) = li.prefix.as_deref().filter(|p| !p.is_empty()) else {
            continue;
        };
        for ds in &li.data_source {
            let Some(t) = ds.strip_prefix('<').and_then(|s| s.strip_suffix('>')) else {
                continue;
            };
            for (_, inst) in info.type_index.instances(t) {
                if count >= SCOPE_LINK_KEY_CAP {
                    flt.dropped += 1;
                    return;
                }
                let label = format!("{}{}", prefix, inst.name);
                if !flt.admit(&label) {
                    continue;
                }
                items.push(CompletionItem {
                    label: label.clone(),
                    kind: Some(CompletionItemKind::REFERENCE),
                    detail: Some(format!("{} scope", li.name)),
                    insert_text: Some(format!("{} = {{\n\t$0\n}}", label)),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    sort_text: sort_for_kind(Some(CompletionItemKind::REFERENCE), &label),
                    ..Default::default()
                });
                count += 1;
            }
        }
    }
}

pub(crate) fn alias_documentation(ruleset: &RuleSet, cat: &str, name: &str) -> Option<String> {
    let indices = ruleset.alias_exact().get(cat)?.get(name)?;
    indices
        .iter()
        .find_map(|&i| ruleset.aliases[i].1.1.description.clone())
}

fn alias_has_description(ruleset: &RuleSet, cat: &str, name: &str) -> bool {
    ruleset
        .alias_exact()
        .get(cat)
        .and_then(|m| m.get(name))
        .is_some_and(|indices| {
            indices
                .iter()
                .any(|&i| ruleset.aliases[i].1.1.description.is_some())
        })
}

fn push_scope_names(items: &mut Vec<CompletionItem>, names: &[String]) {
    for name in names {
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::VALUE),
            detail: Some("scope".to_string()),
            sort_text: sort_for_kind(Some(CompletionItemKind::VALUE), name),
            ..Default::default()
        });
    }
}

fn push_bool_leaf_values(items: &mut Vec<CompletionItem>) {
    for v in &["yes", "no"] {
        items.push(CompletionItem {
            label: v.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("bool".to_string()),
            sort_text: sort_for_kind(Some(CompletionItemKind::KEYWORD), v),
            ..Default::default()
        });
    }
}

pub(crate) fn enum_values_for<'a>(ruleset: &'a RuleSet, enum_name: &str) -> &'a [String] {
    if let Some(&idx) = ruleset.enum_by_name().get(enum_name) {
        return &ruleset.enums[idx].values;
    }
    &[]
}

pub(crate) fn all_enum_values(
    ruleset: &RuleSet,
    info: &InfoService,
    enum_name: &str,
) -> Vec<String> {
    let mut vals = enum_values_for(ruleset, enum_name).to_vec();
    vals.extend(
        info.type_index
            .complex_enum_values
            .values(enum_name)
            .map(str::to_string),
    );
    vals.sort_unstable();
    vals.dedup();
    vals
}

fn all_enum_values_cached<'c>(
    cache: &'c mut HashMap<String, Vec<String>>,
    ruleset: &RuleSet,
    info: &InfoService,
    enum_name: &str,
) -> &'c [String] {
    cache
        .entry(enum_name.to_string())
        .or_insert_with(|| all_enum_values(ruleset, info, enum_name))
}

pub(crate) fn enum_member_detail(
    ruleset: &RuleSet,
    info: &InfoService,
    id: &str,
    v: &str,
) -> Option<String> {
    let in_static = enum_values_for(ruleset, id).iter().any(|s| s == v);
    let in_dynamic = info.type_index.complex_enum_values.contains(id, v);
    (in_static || in_dynamic).then(|| format!("enum {}", id))
}

fn normalized_path_part(value: &str) -> String {
    value
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

fn filepath_values(
    index: &cwtools_info::FileIndex,
    prefix: Option<&str>,
    extension: Option<&str>,
) -> Vec<String> {
    let prefix = prefix
        .map(normalized_path_part)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    let extension = extension
        .filter(|extension| !extension.is_empty())
        .map(|extension| {
            format!(
                ".{}",
                extension.trim_start_matches('.').to_ascii_lowercase()
            )
        })
        .unwrap_or_default();
    let mut values: Vec<String> = index
        .paths()
        .filter_map(|path| {
            let mut value = path.as_str();
            if !prefix.is_empty() {
                value = value.strip_prefix(&prefix)?;
                if !value.starts_with('/') {
                    return None;
                }
            }
            if !extension.is_empty() {
                value = value.strip_suffix(&extension)?;
            }
            let value = value.trim_start_matches('/');
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect();
    values.sort_unstable();
    values.dedup();
    values
}

fn icon_values(index: &cwtools_info::FileIndex, folder: &str) -> Vec<String> {
    let prefix = normalized_path_part(folder)
        .trim_end_matches('/')
        .to_string();
    let mut values: Vec<String> = index
        .paths()
        .filter_map(|path| {
            let mut value = path.as_str();
            if !prefix.is_empty() {
                value = value.strip_prefix(&prefix)?;
                if !value.starts_with('/') {
                    return None;
                }
                value = value.trim_start_matches('/');
            }
            let extension = [".dds", ".tga", ".png"]
                .into_iter()
                .find(|extension| value.ends_with(extension))?;
            let value = value.strip_suffix(extension)?;
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect();
    values.sort_unstable();
    values.dedup();
    values
}

#[derive(Clone, Copy)]
pub(crate) struct ValueCompletionSets<'a> {
    pub modifier_keys: &'a HashSet<String>,
    pub modifier_scopes: &'a HashMap<String, Vec<String>>,
    pub loc_keys: &'a HashSet<String>,
}

pub(crate) fn value_rules_need_loc_keys(
    value_rules: &[(RuleType, cwtools_rules::rules_types::Options)],
) -> bool {
    fn field_needs_loc_keys(field: &NewField) -> bool {
        match field {
            NewField::LocalisationField { .. } => true,
            NewField::IgnoreField(inner) => field_needs_loc_keys(inner),
            _ => false,
        }
    }

    value_rules.iter().any(|(rule, _)| match rule {
        RuleType::LeafRule { right, .. } | RuleType::LeafValueRule { right } => {
            field_needs_loc_keys(right)
        }
        _ => false,
    })
}

#[tracing::instrument(skip_all, fields(value_rules = value_rules.len()))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn value_completions(
    value_rules: &[(RuleType, cwtools_rules::rules_types::Options)],
    ruleset: &RuleSet,
    info: &InfoService,
    registry: Option<&cwtools_game::scope_registry::ScopeRegistry>,
    language: &str,
    sets: ValueCompletionSets<'_>,
    current_scope: Option<ScopeId>,
    token: &str,
) -> (Vec<CompletionItem>, usize) {
    let mut flt = BuildFilter::new(token);
    let mut items: Vec<CompletionItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_alias_cats: HashSet<&str> = HashSet::new();
    // Per-request memo so a repeated enum is only collected/sorted once (#46).
    let mut enum_cache: HashMap<String, Vec<String>> = HashMap::new();
    // value rules arrive here (#44).
    let mut scope_names: Option<Vec<String>> = None;
    let mut push = |label: String,
                    kind: CompletionItemKind,
                    detail: Option<String>,
                    data: Option<Value>,
                    insert_text: Option<String>,
                    items: &mut Vec<CompletionItem>| {
        if seen.insert(label.clone()) {
            let sort_label = label.clone();
            items.push(CompletionItem {
                label,
                kind: Some(kind),
                detail,
                data,
                insert_text,
                sort_text: sort_for_kind(Some(kind), &sort_label),
                ..Default::default()
            });
        }
    };

    for (rule_type, _opts) in value_rules {
        let right = match rule_type {
            RuleType::LeafRule { right, .. } => right,
            RuleType::LeafValueRule { right } => right,
            _ => continue,
        };
        match right {
            NewField::SpecificField(value) => {
                let quoted = quote_if_needed(value);
                push(
                    value.clone(),
                    CompletionItemKind::VALUE,
                    Some("literal".to_string()),
                    None,
                    (quoted != *value).then_some(quoted),
                    &mut items,
                );
            }
            NewField::TypeField(TypeType::Simple(t)) => {
                for (_, inst) in info.type_index.instances(t) {
                    if !flt.admit(&inst.name) {
                        continue;
                    }
                    push(
                        inst.name.clone(),
                        CompletionItemKind::REFERENCE,
                        None,
                        ResolveData::Type { t: t.to_string() }.into_value(),
                        None,
                        &mut items,
                    );
                }
            }
            NewField::TypeField(TypeType::Complex {
                prefix,
                name,
                suffix,
            }) => {
                for (_, inst) in info.type_index.instances(name) {
                    let label = format!("{}{}{}", prefix, inst.name, suffix);
                    if !flt.admit(&label) {
                        continue;
                    }
                    push(
                        label,
                        CompletionItemKind::REFERENCE,
                        Some(format!("{} instance", name)),
                        None,
                        None,
                        &mut items,
                    );
                }
            }
            NewField::ValueField(ValueType::Enum(e)) => {
                for v in all_enum_values_cached(&mut enum_cache, ruleset, info, e) {
                    if !flt.admit(v) {
                        continue;
                    }
                    let quoted = quote_if_needed(v);
                    let insert_text = (quoted != *v).then_some(quoted);
                    push(
                        v.clone(),
                        CompletionItemKind::ENUM_MEMBER,
                        None,
                        ResolveData::Enum { id: e.to_string() }.into_value(),
                        insert_text,
                        &mut items,
                    );
                }
            }
            // variable dump (cwtools-vscode#74).
            NewField::LocalisationField { .. } => {
                for k in sets.loc_keys {
                    push(
                        k.clone(),
                        CompletionItemKind::TEXT,
                        Some("loc key".to_string()),
                        None,
                        None,
                        &mut items,
                    );
                }
            }
            NewField::FilepathField { prefix, extension } => {
                for value in filepath_values(
                    &info.type_index.file_index,
                    prefix.as_deref(),
                    extension.as_deref(),
                ) {
                    if !flt.admit(&value) {
                        continue;
                    }
                    let quoted = quote_if_needed(&value);
                    push(
                        value.clone(),
                        CompletionItemKind::FILE,
                        Some("file path".to_string()),
                        None,
                        (quoted != value).then_some(quoted),
                        &mut items,
                    );
                }
            }
            NewField::IconField(folder) => {
                for value in icon_values(&info.type_index.file_index, folder) {
                    if !flt.admit(&value) {
                        continue;
                    }
                    let quoted = quote_if_needed(&value);
                    push(
                        value.clone(),
                        CompletionItemKind::FILE,
                        Some("icon".to_string()),
                        None,
                        (quoted != value).then_some(quoted),
                        &mut items,
                    );
                }
            }
            NewField::ValueField(ValueType::Bool) => {
                for v in ["yes", "no"] {
                    push(
                        v.to_string(),
                        CompletionItemKind::KEYWORD,
                        Some("bool".to_string()),
                        None,
                        None,
                        &mut items,
                    );
                }
            }
            NewField::ScopeField(_)
            | NewField::ValueScopeField { .. }
            | NewField::ValueScopeMarkerField { .. } => {
                let names =
                    scope_names.get_or_insert_with(|| scope_completion_names(language, registry));
                for name in names.iter() {
                    push(
                        name.clone(),
                        CompletionItemKind::VALUE,
                        Some("scope".to_string()),
                        None,
                        None,
                        &mut items,
                    );
                }
            }
            NewField::VariableGetField(ns) | NewField::VariableSetField(ns) => {
                let member_iter: Box<dyn Iterator<Item = &str>> = match ns.as_str() {
                    "event_target" => Box::new(info.event_target_counts.keys().map(String::as_str)),
                    "variable" => Box::new(info.variable_counts.keys().map(String::as_str)),
                    other => Box::new(
                        ruleset
                            .values
                            .get(other)
                            .into_iter()
                            .flatten()
                            .map(String::as_str)
                            .chain(info.type_index.value_set_values.values(other)),
                    ),
                };
                for v in member_iter {
                    if !flt.admit(v) {
                        continue;
                    }
                    push(
                        v.to_string(),
                        CompletionItemKind::CONSTANT,
                        Some(format!("value[{}]", ns)),
                        None,
                        None,
                        &mut items,
                    );
                }
            }
            NewField::AliasField(cat) | NewField::AliasValueKeysField(cat) => {
                if !seen_alias_cats.insert(cat.as_str()) {
                    continue;
                }
                let scope_ctx = match (current_scope, registry) {
                    (Some(scope), Some(reg)) if scope != SCOPE_ANY => Some((scope, reg)),
                    _ => None,
                };
                let mut aliases = Vec::new();
                push_alias_keys(
                    &mut aliases,
                    ruleset,
                    info,
                    sets.modifier_keys,
                    sets.modifier_scopes,
                    cat,
                    scope_ctx,
                    &mut flt,
                );
                for item in &mut aliases {
                    let quoted = quote_if_needed(&item.label);
                    item.insert_text = (quoted != item.label).then_some(quoted);
                    item.insert_text_format = None;
                }
                items.extend(aliases);
            }
            NewField::VariableField { .. } => {
                for v in info.variable_counts.keys() {
                    if !flt.admit(v) {
                        continue;
                    }
                    push(
                        v.clone(),
                        CompletionItemKind::CONSTANT,
                        Some("variable".to_string()),
                        None,
                        None,
                        &mut items,
                    );
                }
            }
            NewField::ValueField(ValueType::MathExpr) => {
                for v in info.variable_counts.keys() {
                    if !flt.admit(v) {
                        continue;
                    }
                    push(
                        v.clone(),
                        CompletionItemKind::CONSTANT,
                        Some("variable".to_string()),
                        None,
                        None,
                        &mut items,
                    );
                }
                for et in info.event_target_counts.keys() {
                    let label = format!("event_target:{}", et);
                    if !flt.admit(&label) {
                        continue;
                    }
                    push(
                        label,
                        CompletionItemKind::VARIABLE,
                        Some("event target".to_string()),
                        None,
                        None,
                        &mut items,
                    );
                }
            }
            NewField::IgnoreField(inner) => {
                let nested_rules = vec![(
                    RuleType::LeafValueRule {
                        right: (**inner).clone(),
                    },
                    cwtools_rules::rules_types::Options::default(),
                )];
                let (nested, nested_dropped) = value_completions(
                    &nested_rules,
                    ruleset,
                    info,
                    registry,
                    language,
                    sets,
                    current_scope,
                    token,
                );
                flt.dropped += nested_dropped;
                items.extend(nested);
            }
            NewField::SingleAliasField(_)
            | NewField::ScalarField
            | NewField::ValueField(
                ValueType::Float { .. }
                | ValueType::Int { .. }
                | ValueType::Percent
                | ValueType::Date
                | ValueType::DateTime
                | ValueType::Ck2Dna
                | ValueType::Ck2DnaProperty
                | ValueType::IrFamilyName
                | ValueType::StlNameFormat(_),
            )
            | NewField::MarkerField(_)
            | NewField::IgnoreMarkerField => {}
        }
    }

    let mut final_seen = HashSet::new();
    items.retain(|item| final_seen.insert(item.label.clone()));
    (items, flt.dropped())
}

#[tracing::instrument(skip_all, fields(logical_path = %logical_path))]
pub(crate) fn root_type_snippets(ruleset: &RuleSet, logical_path: &str) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    for td in &ruleset.types {
        if !cwtools_info::check_path_dir(&td.path_options, logical_path) {
            continue;
        }

        let mut openers: Vec<String> = match &td.type_key_filter {
            Some((keys, false)) if !keys.is_empty() => keys.clone(),
            _ => vec![td.name.clone()],
        };

        for st in &td.subtypes {
            if let Some(tkf) = &st.type_key_field
                && !openers.contains(tkf)
            {
                openers.push(tkf.clone());
            }
        }

        let child_rules: Option<&[(RuleType, cwtools_rules::rules_types::Options)]> =
            ruleset.root_rules.iter().find_map(|r| {
                if let RootRule::TypeRule(name, (RuleType::NodeRule { rules, .. }, _)) = r {
                    if name == &td.name {
                        Some(rules.as_ref())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

        for opener in openers {
            let snippet = if let Some(cr) = child_rules {
                generate_node_snippet(&opener, cr, ruleset)
            } else {
                format!("{} = {{\n\t$0\n}}", opener)
            };
            items.push(CompletionItem {
                label: opener.clone(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some(format!("type {} instance", td.name)),
                insert_text: Some(snippet),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                sort_text: Some(format!("0_{}", opener)),
                ..Default::default()
            });
        }
    }

    items
}

#[cfg(test)]
mod resolve_data_tests {

    use std::sync::Arc;

    use cwtools_rules::rules_types::{EnumDefinition, Options};

    use super::*;

    #[test]
    fn label_details_describe_origin_from_resolve_data() {
        let mut items = vec![
            CompletionItem {
                label: "MY_FOCUS".to_string(),
                data: ResolveData::Type {
                    t: "focus".to_string(),
                }
                .into_value(),
                ..Default::default()
            },
            CompletionItem {
                label: "army".to_string(),
                data: ResolveData::Enum {
                    id: "stat".to_string(),
                }
                .into_value(),
                ..Default::default()
            },
            CompletionItem {
                label: "add_political_power".to_string(),
                data: ResolveData::Alias {
                    cat: "effect".to_string(),
                    name: "add_political_power".to_string(),
                }
                .into_value(),
                ..Default::default()
            },
            CompletionItem {
                label: "plain".to_string(),
                ..Default::default()
            },
        ];
        apply_label_details(&mut items);
        let desc = |i: usize| {
            items[i]
                .label_details
                .as_ref()
                .and_then(|d| d.description.as_deref())
        };
        assert_eq!(desc(0), Some("focus"));
        assert_eq!(desc(1), Some("enum stat"));
        assert_eq!(desc(2), Some("effect"));
        assert!(items[3].label_details.is_none());
    }

    #[test]
    fn alias_item_defers_documentation() {
        let mut rs = RuleSet::new();
        rs.aliases.push((
            "effect:add_political_power".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[effect:add_political_power]".to_string()),
                    right: NewField::ScalarField,
                },
                Options {
                    description: Some("Adds political power to the country.".to_string()),
                    ..Options::default()
                },
            ),
        ));
        rs.reindex();
        let info = InfoService::new();
        let rules = vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("effect".to_string()),
                right: NewField::AliasField("effect".to_string()),
            },
            Options::default(),
        )];
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;
        let item = items
            .iter()
            .find(|i| i.label == "add_political_power")
            .expect("'add_political_power' item");
        assert!(
            item.documentation.is_none(),
            "documentation must be deferred out of the built item, got: {:?}",
            item.documentation
        );
        assert_eq!(
            item.data,
            Some(serde_json::Value::String(
                "alias:effect:add_political_power".to_string()
            ))
        );
        assert_eq!(
            alias_documentation(&rs, "effect", "add_political_power").as_deref(),
            Some("Adds political power to the country.")
        );
    }

    #[test]
    fn alias_item_without_description_carries_no_data() {
        let mut rs = RuleSet::new();
        rs.aliases.push((
            "effect:add_political_power".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[effect:add_political_power]".to_string()),
                    right: NewField::ScalarField,
                },
                Options::default(),
            ),
        ));
        rs.reindex();
        let info = InfoService::new();
        let rules = vec![(
            RuleType::LeafRule {
                left: NewField::AliasField("effect".to_string()),
                right: NewField::AliasField("effect".to_string()),
            },
            Options::default(),
        )];
        let items = completions_from_rules(
            &rules,
            &rs,
            &info,
            "hoi4",
            &HashSet::new(),
            &Default::default(),
            None,
            None,
            "",
        )
        .0;
        let item = items
            .iter()
            .find(|i| i.label == "add_political_power")
            .expect("'add_political_power' item");
        assert!(
            item.data.is_none(),
            "a doc-less alias must carry no data, got: {:?}",
            item.data
        );
    }

    #[test]
    fn alias_value_items_insert_bare_values() {
        let mut rs = RuleSet::new();
        rs.aliases.push((
            "modifier:build cost".to_string(),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField("alias[modifier:build cost]".to_string()),
                    right: NewField::ScalarField,
                },
                Options::default(),
            ),
        ));
        rs.reindex();
        let info = InfoService::new();
        for right in [
            NewField::AliasField("modifier".to_string()),
            NewField::AliasValueKeysField("modifier".to_string()),
        ] {
            let rules = vec![(RuleType::LeafValueRule { right }, Options::default())];
            let items = value_completions(
                &rules,
                &rs,
                &info,
                None,
                "hoi4",
                ValueCompletionSets {
                    modifier_keys: &HashSet::new(),
                    modifier_scopes: &Default::default(),
                    loc_keys: &HashSet::new(),
                },
                None,
                "",
            )
            .0;
            let item = items
                .iter()
                .find(|item| item.label == "build cost")
                .expect("alias value");
            assert_eq!(item.insert_text.as_deref(), Some("\"build cost\""));
            assert_eq!(item.insert_text_format, None);
        }
    }

    #[test]
    fn localisation_values_use_the_localisation_index() {
        let rs = RuleSet::new();
        let info = InfoService::new();
        let rules = vec![(
            RuleType::LeafValueRule {
                right: NewField::LocalisationField {
                    synced: false,
                    is_inline: false,
                },
            },
            Options::default(),
        )];
        let loc_keys = HashSet::from(["known_key".to_string()]);
        let items = value_completions(
            &rules,
            &rs,
            &info,
            None,
            "hoi4",
            ValueCompletionSets {
                modifier_keys: &HashSet::new(),
                modifier_scopes: &Default::default(),
                loc_keys: &loc_keys,
            },
            None,
            "",
        )
        .0;
        assert!(items.iter().any(|item| item.label == "known_key"));
        assert!(value_rules_need_loc_keys(&rules));
    }

    #[test]
    fn filepath_and_icon_values_follow_validation_shapes() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "common/ships/alpha.txt",
            "common/ships/nested/beta.TXT",
            "common/ships/with space.txt",
            "common/shipyard/not-a-ship.txt",
            "gfx/interface/icons/alpha.dds",
            "gfx/interface/icons/alpha.png",
            "gfx/interface/icons/nested/beta.tga",
            "gfx/interface/iconography/wrong.dds",
        ] {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let mut index = cwtools_info::FileIndex::new();
        index.add_root(root.path());

        assert_eq!(
            filepath_values(&index, Some("common/ships/"), Some(".txt")),
            vec!["alpha", "nested/beta", "with space"]
        );
        assert_eq!(
            icon_values(&index, "gfx\\interface\\icons\\"),
            vec!["alpha", "nested/beta"]
        );
    }

    #[test]
    fn type_instance_item_defers_detail() {
        let mut info = InfoService::new();
        let mut per_type: HashMap<String, Vec<cwtools_info::TypeInstance>> = HashMap::new();
        per_type.insert(
            "state".to_string(),
            vec![cwtools_info::TypeInstance {
                name: "STATE_1".to_string(),
                location: cwtools_info::SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        Arc::make_mut(&mut info.type_index).merge("file:///states/s.txt", per_type);

        let mut items = Vec::new();
        push_type_instances(
            &mut items,
            &info,
            "state",
            TypeInstanceStyle::Reference,
            &mut BuildFilter::new(""),
        );
        let item = items.first().expect("one item");
        assert!(
            item.detail.is_none(),
            "detail must be deferred out of the built item, got: {:?}",
            item.detail
        );
        assert_eq!(
            item.data,
            Some(serde_json::Value::String("type:state".to_string()))
        );
        assert_eq!(
            type_instance_detail(None, &info, "state", "STATE_1").as_deref(),
            Some("state instance")
        );
    }

    fn ruleset_with_subtype(
        type_name: &str,
        subtype_name: &str,
        display_name: Option<&str>,
    ) -> RuleSet {
        let mut rs = RuleSet::new();
        rs.types.push(cwtools_rules::rules_types::TypeDefinition {
            name: type_name.to_string(),
            name_field: None,
            path_options: cwtools_rules::rules_types::PathOptions::default(),
            subtypes: vec![SubTypeDefinition {
                name: subtype_name.to_string(),
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

    fn merge_one_instance(info: &mut InfoService, key: &str, name: &str) {
        let mut per_type: HashMap<String, Vec<cwtools_info::TypeInstance>> = HashMap::new();
        per_type.insert(
            key.to_string(),
            vec![cwtools_info::TypeInstance {
                name: name.to_string(),
                location: cwtools_info::SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        Arc::make_mut(&mut info.type_index).merge("file:///x.txt", per_type);
    }

    #[test]
    fn type_instance_detail_prefers_subtype_display_name() {
        let rs = ruleset_with_subtype("event", "country", Some("Country Event"));
        let mut info = InfoService::new();
        merge_one_instance(&mut info, "event.country", "my_event");

        assert_eq!(
            type_instance_detail(Some(&rs), &info, "event.country", "my_event").as_deref(),
            Some("event.Country Event instance")
        );
    }

    #[test]
    fn type_instance_detail_unchanged_without_display_name() {
        let rs = ruleset_with_subtype("event", "country", None);
        let mut info = InfoService::new();
        merge_one_instance(&mut info, "event.country", "my_event");

        assert_eq!(
            type_instance_detail(Some(&rs), &info, "event.country", "my_event").as_deref(),
            Some("event.country instance")
        );
    }

    #[test]
    fn enum_member_item_defers_detail() {
        let mut rs = RuleSet::new();
        rs.enums.push(EnumDefinition {
            key: "my_enum".to_string(),
            description: String::new(),
            values: vec!["alpha".to_string()],
        });
        rs.reindex();
        let info = InfoService::new();

        let mut items = Vec::new();
        let mut cache = HashMap::new();
        push_enum_leaf_values(
            &mut items,
            &mut cache,
            &rs,
            &info,
            "my_enum",
            &mut BuildFilter::new(""),
        );
        let item = items.first().expect("one item");
        assert!(
            item.detail.is_none(),
            "detail must be deferred out of the built item, got: {:?}",
            item.detail
        );
        assert_eq!(
            item.data,
            Some(serde_json::Value::String("enum:my_enum".to_string()))
        );
        assert_eq!(
            enum_member_detail(&rs, &info, "my_enum", "alpha").as_deref(),
            Some("enum my_enum")
        );
    }
}
