#[derive(Debug, Clone, PartialEq)]
pub struct TypeReferenceRule {
    pub ref_type: String,
    pub root_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuleSet {
    pub types: Vec<TypeDefinition>,
    pub aliases: Vec<(String, NewRule)>,
    pub single_aliases: Vec<(String, NewRule)>,
    pub enums: Vec<EnumDefinition>,
    pub complex_enums: Vec<ComplexEnumDef>,
    pub root_rules: Vec<RootRule>,
    pub values: rustc_hash::FxHashMap<String, Vec<String>>,
    pub modifiers: Vec<(String, String)>,
    pub modifier_categories: rustc_hash::FxHashMap<String, Vec<String>>,
    pub scope_links: rustc_hash::FxHashSet<String>,
    pub scope_inputs: Vec<ScopeInput>,
    pub link_inputs: Vec<LinkInput>,
    pub folders: Vec<String>,
    pub localisation_commands: rustc_hash::FxHashSet<String>,
    alias_exact: rustc_hash::FxHashMap<String, rustc_hash::FxHashMap<String, Vec<usize>>>,
    alias_categories: rustc_hash::FxHashMap<String, AliasCategoryIndex>,
    type_by_name: rustc_hash::FxHashMap<String, usize>,
    enum_by_name: rustc_hash::FxHashMap<String, usize>,
    type_rules_idx: rustc_hash::FxHashMap<String, usize>,
    type_reference_rules: rustc_hash::FxHashMap<String, Vec<TypeReferenceRule>>,
    value_set_effects: rustc_hash::FxHashMap<String, String>,
    value_set_effect_fields: rustc_hash::FxHashMap<String, Vec<(String, String)>>,
    enum_values_lower: Vec<rustc_hash::FxHashSet<String>>,
    enum_has_at: Vec<bool>,
    value_sets: rustc_hash::FxHashMap<String, rustc_hash::FxHashSet<String>>,
    builtin_variable_bases: rustc_hash::FxHashSet<String>,
    pretriggers: rustc_hash::FxHashMap<String, rustc_hash::FxHashSet<String>>,
    pub def_positions: Vec<CwtDefPosition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwtDefKind {
    Type,
    Enum,
    SingleAlias,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CwtDefPosition {
    pub kind: CwtDefKind,
    pub name: String,
    pub file: std::path::PathBuf,
    pub line: u32,
    pub col: u16,
}

pub use cwtools_game::scope_registry::{LinkInput, ScopeInput};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternKind {
    Type,
    Enum,
    Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAliasPattern {
    pub alias_idx: usize,
    pub prefix: String,
    pub suffix: String,
    pub kind: PatternKind,
    pub placeholder_name: String,
}

impl ParsedAliasPattern {
    pub fn parse(rest: &str, alias_idx: usize) -> Option<Self> {
        if let Some(open) = rest.find('<') {
            let close = open + rest[open..].find('>')?;
            return Some(ParsedAliasPattern {
                alias_idx,
                prefix: rest[..open].to_string(),
                suffix: rest[close + 1..].to_string(),
                kind: PatternKind::Type,
                placeholder_name: rest[open + 1..close].to_string(),
            });
        }
        let markers: &[(&str, PatternKind)] = &[
            ("value_set[", PatternKind::Value),
            ("complex_enum[", PatternKind::Enum),
            ("value[", PatternKind::Value),
            ("enum[", PatternKind::Enum),
        ];
        let mut found: Option<(usize, usize, usize, usize, PatternKind)> = None;
        for (marker, kind) in markers {
            if let Some(open) = rest.find(marker) {
                let inner = open + marker.len();
                let close = inner + rest[inner..].find(']')?;
                let earlier = found.as_ref().is_none_or(|&(o, ..)| open < o);
                if earlier {
                    found = Some((open, inner, close, close + 1, *kind));
                }
            }
        }
        let (open, inner, close, after, kind) = found?;
        Some(ParsedAliasPattern {
            alias_idx,
            prefix: rest[..open].to_string(),
            suffix: rest[after..].to_string(),
            kind,
            placeholder_name: rest[inner..close].to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AliasCategoryIndex {
    pub parsed_patterns: Vec<ParsedAliasPattern>,
    pub scope_field_idx: Option<usize>,
}

fn normalize_path_lower(p: &str) -> String {
    if p.contains('\\') {
        p.replace('\\', "/").trim_matches('/').to_ascii_lowercase()
    } else {
        p.trim_matches('/').to_ascii_lowercase()
    }
}

fn normalize_path_options(opts: &mut PathOptions) {
    opts.paths_lower = opts.paths.iter().map(|p| normalize_path_lower(p)).collect();
    opts.path_file_lower = opts.path_file.as_deref().map(|s| s.to_ascii_lowercase());
    opts.path_ext_lower = opts.path_extension.as_deref().map(|s| {
        let s = s.to_ascii_lowercase();
        s.strip_prefix('.').map(|t| t.to_string()).unwrap_or(s)
    });
}

impl Default for RuleSet {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleSet {
    pub fn new() -> Self {
        Self {
            types: Vec::new(),
            aliases: Vec::new(),
            single_aliases: Vec::new(),
            enums: Vec::new(),
            complex_enums: Vec::new(),
            root_rules: Vec::new(),
            values: rustc_hash::FxHashMap::default(),
            modifiers: Vec::new(),
            modifier_categories: rustc_hash::FxHashMap::default(),
            scope_links: rustc_hash::FxHashSet::default(),
            scope_inputs: Vec::new(),
            link_inputs: Vec::new(),
            folders: Vec::new(),
            localisation_commands: rustc_hash::FxHashSet::default(),
            alias_exact: rustc_hash::FxHashMap::default(),
            alias_categories: rustc_hash::FxHashMap::default(),
            type_by_name: rustc_hash::FxHashMap::default(),
            enum_by_name: rustc_hash::FxHashMap::default(),
            type_rules_idx: rustc_hash::FxHashMap::default(),
            type_reference_rules: rustc_hash::FxHashMap::default(),
            value_set_effects: rustc_hash::FxHashMap::default(),
            value_set_effect_fields: rustc_hash::FxHashMap::default(),
            enum_values_lower: Vec::new(),
            enum_has_at: Vec::new(),
            value_sets: rustc_hash::FxHashMap::default(),
            builtin_variable_bases: rustc_hash::FxHashSet::default(),
            pretriggers: rustc_hash::FxHashMap::default(),
            def_positions: Vec::new(),
        }
    }

    pub fn reindex(&mut self) {
        let mut normalized_enums = Vec::with_capacity(self.enums.len());
        let mut enum_by_name = rustc_hash::FxHashMap::default();
        let mut enum_values_lower = Vec::with_capacity(self.enums.len());
        for EnumDefinition {
            key,
            description,
            values,
        } in self.enums.drain(..)
        {
            let idx = if let Some(&idx) = enum_by_name.get(&key) {
                idx
            } else {
                let idx = normalized_enums.len();
                enum_by_name.insert(key.clone(), idx);
                normalized_enums.push(EnumDefinition {
                    key,
                    description,
                    values: Vec::new(),
                });
                enum_values_lower.push(rustc_hash::FxHashSet::default());
                idx
            };
            for value in values {
                if enum_values_lower[idx].insert(value.to_ascii_lowercase()) {
                    normalized_enums[idx].values.push(value);
                }
            }
        }
        self.enums = normalized_enums;
        self.enum_by_name = enum_by_name;
        self.enum_values_lower = enum_values_lower;

        self.alias_exact.clear();
        self.alias_categories.clear();
        self.pretriggers.clear();
        self.value_set_effects.clear();
        self.value_set_effect_fields.clear();
        fn first_value_set_ns(rule: &RuleType) -> Option<&str> {
            fn of_field(f: &NewField) -> Option<&str> {
                match f {
                    NewField::VariableSetField(ns) => Some(ns.as_str()),
                    _ => None,
                }
            }
            match rule {
                RuleType::LeafRule { left, right } => of_field(left).or_else(|| of_field(right)),
                RuleType::LeafValueRule { right } => of_field(right),
                RuleType::NodeRule { left, rules } => of_field(left)
                    .or_else(|| rules.iter().find_map(|(rt, _)| first_value_set_ns(rt))),
                RuleType::ValueClauseRule { rules } | RuleType::SubtypeRule { rules, .. } => {
                    rules.iter().find_map(|(rt, _)| first_value_set_ns(rt))
                }
            }
        }
        fn collect_binding_fields(rule: &RuleType, out: &mut Vec<(String, String)>) {
            match rule {
                RuleType::LeafRule {
                    left: NewField::SpecificField(key),
                    right: NewField::VariableSetField(ns),
                } => out.push((key.to_ascii_lowercase(), ns.clone())),
                RuleType::NodeRule { left, rules } => {
                    if let NewField::SpecificField(key) = left {
                        for (rt, _) in rules.iter() {
                            if let RuleType::LeafValueRule {
                                right: NewField::VariableSetField(ns),
                            } = rt
                            {
                                out.push((key.to_ascii_lowercase(), ns.clone()));
                            }
                        }
                    }
                    for (rt, _) in rules.iter() {
                        collect_binding_fields(rt, out);
                    }
                }
                RuleType::ValueClauseRule { rules } | RuleType::SubtypeRule { rules, .. } => {
                    for (rt, _) in rules.iter() {
                        collect_binding_fields(rt, out);
                    }
                }
                _ => {}
            }
        }
        for (i, (name, (rule, _))) in self.aliases.iter().enumerate() {
            if let Some((cat, key)) = name.split_once(':') {
                if let Some(scope) = cat.strip_suffix("_pre_trigger") {
                    self.pretriggers
                        .entry(scope.to_ascii_lowercase())
                        .or_default()
                        .insert(key.to_ascii_lowercase());
                }
                if cat == "effect" || cat == "trigger" {
                    if let Some(ns) = first_value_set_ns(rule) {
                        self.value_set_effects
                            .entry(key.to_ascii_lowercase())
                            .or_insert_with(|| ns.to_string());
                    }
                    let mut fields = Vec::new();
                    collect_binding_fields(rule, &mut fields);
                    if !fields.is_empty() {
                        self.value_set_effect_fields
                            .entry(key.to_ascii_lowercase())
                            .or_default()
                            .extend(fields);
                    }
                }
                self.alias_exact
                    .entry(cat.to_string())
                    .or_default()
                    .entry(key.to_string())
                    .or_default()
                    .push(i);
                let lower_cat = cat.to_ascii_lowercase();
                let lower_key = key.to_ascii_lowercase();
                if lower_cat != cat || lower_key != key {
                    self.alias_exact
                        .entry(lower_cat)
                        .or_default()
                        .entry(lower_key)
                        .or_default()
                        .push(i);
                }
            }
            if let Some((cat, rest)) = name.split_once(':') {
                let entry = self.alias_categories.entry(cat.to_string()).or_default();
                if rest == "scope_field" {
                    entry.scope_field_idx = Some(i);
                } else if let Some(parsed) = ParsedAliasPattern::parse(rest, i) {
                    entry.parsed_patterns.push(parsed);
                }
            }
        }
        for td in &mut self.types {
            normalize_path_options(&mut td.path_options);
        }
        for ce in &mut self.complex_enums {
            normalize_path_options(&mut ce.path_options);
        }
        self.type_by_name.clear();
        for (i, td) in self.types.iter().enumerate() {
            self.type_by_name.entry(td.name.clone()).or_insert(i);
        }
        self.enum_has_at = self
            .enums
            .iter()
            .map(|e| e.values.iter().any(|v| v.starts_with('@')))
            .collect();
        self.value_sets = self
            .values
            .iter()
            .map(|(k, vs)| (k.clone(), vs.iter().cloned().collect()))
            .collect();
        self.builtin_variable_bases = self
            .values
            .get("variable")
            .map(|members| {
                members
                    .iter()
                    .map(|m| {
                        let base = m.split('@').next().unwrap_or(m);
                        base.to_ascii_lowercase()
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.type_rules_idx.clear();
        self.type_reference_rules.clear();
        for (i, root_rule) in self.root_rules.iter().enumerate() {
            let (root_type, rule) = match root_rule {
                RootRule::TypeRule(name, rule) => {
                    self.type_rules_idx.entry(name.clone()).or_insert(i);
                    (Some(name.as_str()), rule)
                }
                RootRule::AliasRule(_, rule) | RootRule::SingleAliasRule(_, rule) => (None, rule),
            };
            let (rule_type, _) = rule;
            let RuleType::NodeRule { rules, .. } = rule_type else {
                continue;
            };
            for (inner, _) in rules.iter() {
                if let RuleType::LeafRule {
                    left: NewField::SpecificField(key),
                    right: NewField::TypeField(TypeType::Simple(ref_type)),
                } = inner
                {
                    self.type_reference_rules
                        .entry(key.to_ascii_lowercase())
                        .or_default()
                        .push(TypeReferenceRule {
                            ref_type: ref_type.clone(),
                            root_type: root_type.map(str::to_string),
                        });
                }
            }
        }
    }

    pub fn type_reference_rules_for_key(&self, key: &str) -> Option<&[TypeReferenceRule]> {
        let rules = if key.bytes().any(|b| b.is_ascii_uppercase()) {
            self.type_reference_rules.get(&key.to_ascii_lowercase())
        } else {
            self.type_reference_rules.get(key)
        }?;
        Some(rules.as_slice())
    }

    pub fn enum_values_contains_ci(&self, idx: usize, value: &str) -> bool {
        match self.enum_values_lower.get(idx) {
            Some(set) => {
                if value.bytes().any(|b| b.is_ascii_uppercase()) {
                    set.contains(&value.to_ascii_lowercase() as &str)
                } else {
                    set.contains(value)
                }
            }
            None => self.enums[idx]
                .values
                .iter()
                .any(|v| v.eq_ignore_ascii_case(value)),
        }
    }

    pub fn enum_has_at_constant(&self, idx: usize) -> bool {
        match self.enum_has_at.get(idx) {
            Some(&b) => b,
            None => self.enums[idx].values.iter().any(|v| v.starts_with('@')),
        }
    }

    pub fn value_set_lookup(&self, name: &str, value: &str) -> Option<bool> {
        if let Some(set) = self.value_sets.get(name)
            && !set.is_empty()
        {
            return Some(set.contains(value));
        }
        match self.values.get(name) {
            Some(vs) if !vs.is_empty() => Some(vs.iter().any(|v| v == value)),
            _ => None,
        }
    }

    pub fn is_builtin_variable_base(&self, base: &str) -> bool {
        if self.builtin_variable_bases.is_empty() {
            return match self.values.get("variable") {
                Some(members) => members.iter().any(|m| {
                    let b = m.split('@').next().unwrap_or(m);
                    b.eq_ignore_ascii_case(base)
                }),
                None => false,
            };
        }
        if !base.bytes().any(|b| b.is_ascii_uppercase()) {
            return self.builtin_variable_bases.contains(base);
        }
        thread_local! {
            static LOWER_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
        }
        LOWER_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            buf.clear();
            buf.extend(base.chars().map(|c| c.to_ascii_lowercase()));
            self.builtin_variable_bases.contains(buf.as_str())
        })
    }

    fn assert_reindexed(&self) {
        debug_assert!(
            self.type_by_name.iter().all(|(name, &idx)| {
                self.types
                    .get(idx)
                    .is_some_and(|td| td.name.as_str() == name.as_str())
            }) && self
                .types
                .iter()
                .enumerate()
                .all(|(i, td)| { self.type_by_name.get(&td.name).is_some_and(|&idx| idx <= i) })
                && self.enums.len() == self.enum_by_name.len()
                && (self.aliases.is_empty() || !self.alias_exact.is_empty())
                && self.enums.len() == self.enum_values_lower.len()
                && self.enums.len() == self.enum_has_at.len(),
            "RuleSet used without reindex: derived indexes are stale"
        );
    }

    pub fn alias_exact(
        &self,
    ) -> &rustc_hash::FxHashMap<String, rustc_hash::FxHashMap<String, Vec<usize>>> {
        self.assert_reindexed();
        &self.alias_exact
    }

    pub fn alias_categories(&self) -> &rustc_hash::FxHashMap<String, AliasCategoryIndex> {
        self.assert_reindexed();
        &self.alias_categories
    }

    pub fn type_by_name(&self) -> &rustc_hash::FxHashMap<String, usize> {
        self.assert_reindexed();
        &self.type_by_name
    }

    pub fn enum_by_name(&self) -> &rustc_hash::FxHashMap<String, usize> {
        self.assert_reindexed();
        &self.enum_by_name
    }

    pub fn type_rules_idx(&self) -> &rustc_hash::FxHashMap<String, usize> {
        self.assert_reindexed();
        &self.type_rules_idx
    }

    pub fn type_reference_rules(&self) -> &rustc_hash::FxHashMap<String, Vec<TypeReferenceRule>> {
        self.assert_reindexed();
        &self.type_reference_rules
    }

    pub fn value_set_effects(&self) -> &rustc_hash::FxHashMap<String, String> {
        self.assert_reindexed();
        &self.value_set_effects
    }

    pub fn value_set_effect_fields(&self) -> &rustc_hash::FxHashMap<String, Vec<(String, String)>> {
        self.assert_reindexed();
        &self.value_set_effect_fields
    }

    pub fn enum_values_lower(&self) -> &[rustc_hash::FxHashSet<String>] {
        self.assert_reindexed();
        &self.enum_values_lower
    }

    pub fn enum_has_at(&self) -> &[bool] {
        self.assert_reindexed();
        &self.enum_has_at
    }

    pub fn value_sets(&self) -> &rustc_hash::FxHashMap<String, rustc_hash::FxHashSet<String>> {
        self.assert_reindexed();
        &self.value_sets
    }

    pub fn builtin_variable_bases(&self) -> &rustc_hash::FxHashSet<String> {
        self.assert_reindexed();
        &self.builtin_variable_bases
    }

    pub fn pretriggers(&self) -> &rustc_hash::FxHashMap<String, rustc_hash::FxHashSet<String>> {
        self.assert_reindexed();
        &self.pretriggers
    }

    pub fn alias_exact_for(&self, category: &str, key: &str) -> Option<&[usize]> {
        self.assert_reindexed();
        self.alias_exact
            .get(category)
            .and_then(|m| m.get(key))
            .map(|v| v.as_slice())
    }

    pub fn alias_category(&self, category: &str) -> Option<&AliasCategoryIndex> {
        self.assert_reindexed();
        self.alias_categories.get(category)
    }
}

#[derive(Debug, Default)]
pub struct RuleSetBuilder {
    inner: RuleSet,
}

impl RuleSetBuilder {
    pub fn new() -> Self {
        Self {
            inner: RuleSet::new(),
        }
    }

    pub fn from_ruleset(ruleset: RuleSet) -> Self {
        Self { inner: ruleset }
    }

    pub fn ruleset_mut(&mut self) -> &mut RuleSet {
        &mut self.inner
    }

    pub fn finish(mut self) -> RuleSet {
        self.inner.reindex();
        self.inner
    }
}

impl From<RuleSet> for RuleSetBuilder {
    fn from(rs: RuleSet) -> Self {
        Self::from_ruleset(rs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_variable_base_matches_across_suffixes() {
        let mut ruleset = RuleSet::new();
        ruleset.values.insert(
            "variable".to_string(),
            vec![
                "party_popularity@<ideology>".to_string(),
                "Faction_Leader".to_string(),
            ],
        );
        ruleset.reindex();

        assert!(ruleset.is_builtin_variable_base("party_popularity"));
        assert!(ruleset.is_builtin_variable_base("faction_leader"));
        assert!(ruleset.is_builtin_variable_base("FACTION_LEADER"));
        assert!(!ruleset.is_builtin_variable_base("party_popularity@social_democrat"));
        assert!(!ruleset.is_builtin_variable_base("unrelated_var"));
    }

    #[test]
    fn builtin_variable_base_fallback_without_reindex() {
        let mut ruleset = RuleSet::new();
        ruleset.values.insert(
            "variable".to_string(),
            vec!["party_popularity@<ideology>".to_string()],
        );
        assert!(ruleset.is_builtin_variable_base("party_popularity"));
        assert!(!ruleset.is_builtin_variable_base("unrelated_var"));
    }

    #[test]
    #[should_panic(expected = "RuleSet used without reindex")]
    fn assert_reindexed_rejects_partially_built_alias_indexes() {
        let mut ruleset = RuleSet::new();
        ruleset.aliases.push((
            "effect:test".to_string(),
            (
                RuleType::LeafValueRule {
                    right: NewField::ScalarField,
                },
                Options::default(),
            ),
        ));
        let _ = ruleset.alias_exact();
    }

    #[test]
    fn base_name_strips_only_the_subtype_qualifier() {
        let simple = |n: &str| TypeType::Simple(n.to_string()).base_name().to_string();
        assert_eq!(simple("equipment"), "equipment");
        assert_eq!(simple("equipment.naval_equip"), "equipment");
        assert_eq!(simple("a.b.c"), "a");

        let complex = |n: &str| {
            TypeType::Complex {
                prefix: "GFX_".to_string(),
                name: n.to_string(),
                suffix: "_icon".to_string(),
            }
            .base_name()
            .to_string()
        };
        assert_eq!(complex("thing"), "thing");
        assert_eq!(complex("thing.fancy"), "thing");
    }
}

impl From<&Severity> for cwtools_error_codes::ErrorSeverity {
    fn from(sev: &Severity) -> Self {
        match sev {
            Severity::Error => Self::Error,
            Severity::Warning => Self::Warning,
            Severity::Information => Self::Information,
            Severity::Hint => Self::Hint,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDefinition {
    pub name: String,
    pub name_field: Option<String>,
    pub path_options: PathOptions,
    pub subtypes: Vec<SubTypeDefinition>,
    pub type_key_filter: Option<(Vec<String>, bool)>,
    pub skip_root_key: Vec<SkipRootKey>,
    pub starts_with: Option<String>,
    pub type_per_file: bool,
    pub key_prefix: Option<String>,
    pub warning_only: bool,
    pub unique: bool,
    pub should_be_referenced: bool,
    pub localisation: Vec<TypeLocalisation>,
    pub graph_related_types: Vec<String>,
    pub modifiers: Vec<TypeModifier>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PathOptions {
    pub paths: Vec<String>,
    pub path_strict: bool,
    pub path_file: Option<String>,
    pub path_extension: Option<String>,
    pub paths_lower: Vec<String>,
    pub path_file_lower: Option<String>,
    pub path_ext_lower: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubTypeDefinition {
    pub name: String,
    pub display_name: Option<String>,
    pub abbreviation: Option<String>,
    pub rules: Vec<NewRule>,
    pub type_key_field: Option<String>,
    pub starts_with: Option<String>,
    pub push_scope: Option<String>,
    pub localisation: Vec<TypeLocalisation>,
    pub only_if_not: Vec<String>,
    pub modifiers: Vec<TypeModifier>,
    pub type_key_filter: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Equals,
    NotEquals,
}

impl MatchKind {
    pub fn from_equals(is_equals: bool) -> Self {
        if is_equals {
            MatchKind::Equals
        } else {
            MatchKind::NotEquals
        }
    }

    pub fn is_equals(self) -> bool {
        matches!(self, MatchKind::Equals)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkipRootKey {
    SpecificKey(String),
    AnyKey,
    MultipleKeys(Vec<String>, MatchKind),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeLocalisation {
    pub name: String,
    pub prefix: String,
    pub suffix: String,
    pub required: bool,
    pub optional: bool,
    pub explicit_field: Option<String>,
    pub replace_scopes: Option<ReplaceScopes>,
    pub primary: bool,
}

impl TypeLocalisation {
    pub fn is_required_name_derived(&self) -> bool {
        self.required && !self.optional && self.explicit_field.is_none()
    }

    pub fn required_explicit_field(&self) -> Option<&str> {
        if !self.required || self.optional {
            return None;
        }
        self.explicit_field.as_deref()
    }

    pub fn derived_key(&self, name: &str) -> String {
        format!("{}{}{}", self.prefix, name, self.suffix)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeModifier {
    pub prefix: String,
    pub suffix: String,
    pub category: String, // ModifierCategory simplified
    pub documentation: Option<String>,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReplaceScopes {
    pub root: Option<String>,
    pub this: Option<String>,
    pub froms: Vec<String>,
    pub prevs: Vec<String>,
}

pub type NewRule = (RuleType, Options);

pub type RuleBody = std::sync::Arc<[NewRule]>;

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum RuleType {
    NodeRule {
        left: NewField,
        rules: RuleBody,
    },
    LeafRule {
        left: NewField,
        right: NewField,
    },
    LeafValueRule {
        right: NewField,
    },
    ValueClauseRule {
        rules: RuleBody,
    },
    SubtypeRule {
        name: String,
        positive: bool,
        rules: RuleBody,
    },
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum NewField {
    ValueField(ValueType),
    SpecificField(String),
    ScalarField,
    TypeField(TypeType),
    ScopeField(Vec<String>),
    LocalisationField {
        synced: bool,
        is_inline: bool,
    },
    FilepathField {
        prefix: Option<String>,
        extension: Option<String>,
    },
    IconField(String),
    AliasValueKeysField(String),
    AliasField(String),
    SingleAliasField(String),
    VariableSetField(String),
    VariableGetField(String),
    VariableField {
        is_int: bool,
        is_32bit: bool,
        min: f64,
        max: f64,
    },
    ValueScopeMarkerField {
        is_int: bool,
        min: f64,
        max: f64,
    },
    ValueScopeField {
        is_int: bool,
        min: f64,
        max: f64,
    },
    MarkerField(Marker),
    IgnoreMarkerField,
    IgnoreField(Box<NewField>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValueType {
    Enum(String),
    Float { min: f64, max: f64 },
    Bool,
    Int { min: i32, max: i32 },
    Percent,
    Date,
    DateTime,
    Ck2Dna,
    Ck2DnaProperty,
    IrFamilyName,
    StlNameFormat(String),
    MathExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeType {
    Simple(String),
    Complex {
        prefix: String,
        name: String,
        suffix: String,
    },
}

impl TypeType {
    pub fn base_name(&self) -> &str {
        let name = match self {
            TypeType::Simple(n) => n.as_str(),
            TypeType::Complex { name, .. } => name.as_str(),
        };
        name.split_once('.').map_or(name, |(base, _)| base)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Marker {
    ColourField,
    IrCountryTag,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReferenceDetail {
    Outgoing(String),
    Incoming(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub min: i32,
    pub max: i32,
    pub strict_min: bool,
    pub leafvalue: bool,
    pub description: Option<String>,
    pub push_scope: Option<String>,
    pub replace_scopes: Option<Box<ReplaceScopes>>,
    pub severity: Option<Severity>,
    pub required_scopes: Vec<String>,
    pub comparison: bool,
    pub reference_details: Option<Box<ReferenceDetail>>,
    pub error_if_only_match: Option<String>,
    pub default_bool: Option<bool>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min: 0,
            max: 1000,
            strict_min: true,
            leafvalue: false,
            description: None,
            push_scope: None,
            replace_scopes: None,
            severity: None,
            required_scopes: Vec::new(),
            comparison: false,
            reference_details: None,
            error_if_only_match: None,
            default_bool: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDefinition {
    pub key: String,
    pub description: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComplexEnumDef {
    pub name: String,
    pub description: String,
    pub path_options: PathOptions,
    pub name_tree: ComplexEnumNameTree,
    pub start_from_root: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComplexEnumNameTree {
    Empty,
    Entries(Vec<ComplexEnumNameTreeEntry>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComplexEnumNameTreeEntry {
    Leaf {
        key: String,
        is_name: bool,
    },
    Node {
        key: String,
        children: ComplexEnumNameTree,
    },
    BareName,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum RootRule {
    AliasRule(String, NewRule),
    SingleAliasRule(String, NewRule),
    TypeRule(String, NewRule),
}
