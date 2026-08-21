/// A depth-one `key = <type>` rule from a root rule. Built into a lookup map at
/// ruleset reindex time for reference indexing and navigation.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeReferenceRule {
    pub ref_type: String,
    /// `Some` for a `TypeRule`, whose path filter must apply. Alias roots apply
    /// regardless of the current file path.
    pub root_type: Option<String>,
}

/// Parsed result from a .cwt file or set of files.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleSet {
    pub types: Vec<TypeDefinition>,
    pub aliases: Vec<(String, NewRule)>,
    pub single_aliases: Vec<(String, NewRule)>,
    pub enums: Vec<EnumDefinition>,
    pub complex_enums: Vec<ComplexEnumDef>,
    pub root_rules: Vec<RootRule>,
    /// Parsed `values = { value[name] = { ... } }` blocks (item G).
    /// Keyed by name; sets from multiple .cwt files are unioned at merge.
    pub values: rustc_hash::FxHashMap<String, Vec<String>>,
    /// `(name, category)` pairs from a top-level `modifiers = { name = category ... }`
    /// block. The names are the valid keys for `alias_name[modifier]` slots (modifier
    /// contexts); the category resolves to a scope set via [`Self::modifier_categories`]
    /// for scope-aware completion.
    pub modifiers: Vec<(String, String)>,
    /// `category -> supported_scopes` from a top-level `modifier_categories = { cat =
    /// { supported_scopes = { ... } } }` block (modifier_categories.cwt). Lets
    /// completion rank/filter a modifier by whether the current scope is one its
    /// category supports.
    pub modifier_categories: rustc_hash::FxHashMap<String, Vec<String>>,
    /// Link names from a top-level `links = { name = { ... } }` block (links.cwt).
    /// A from-data scope link (e.g. `character`, `state`, `owner`) can appear as a
    /// scope-switching key, so these are the valid keys for an `[cat:scope_field]`
    /// slot alongside scope commands and type instances. See [`crate`] consumers.
    /// Derived from `link_inputs` (names + prefixes) during reindex.
    pub scope_links: rustc_hash::FxHashSet<String>,
    /// Scope definitions from a top-level `scopes = { Name = { aliases = {..} } }`
    /// block (scopes.cwt). Used to build the runtime scope registry. Empty when no
    /// scopes.cwt is loaded (the engine then falls back to the hardcoded table).
    pub scope_inputs: Vec<ScopeInput>,
    /// Full link definitions from `links = { name = { ... } }` (links.cwt), with
    /// every field the scope engine needs (output/input scopes, prefix, from_data).
    pub link_inputs: Vec<LinkInput>,
    /// Top-level script folder names from `folders.cwt` (one per line). Drives
    /// which subdirectories of a mod/vanilla root are discovered; empty when the
    /// config ships no folders.cwt (discovery then falls back to the engine's
    /// built-in folder list).
    pub folders: Vec<String>,
    /// Lowercased localisation command names from `localisation_commands = { ... }`
    /// (localisation.cwt). Terminal getters for loc validation (CW226/CW266).
    pub localisation_commands: rustc_hash::FxHashSet<String>,
    /// Lookup index over `aliases`, built by `reindex()`. Two-level map:
    /// `category → key → indices of every matching overload`. Lookups require
    /// only two borrowed-str probes with zero allocation on the hot path.
    alias_exact: rustc_hash::FxHashMap<String, rustc_hash::FxHashMap<String, Vec<usize>>>,
    /// Per-category alias metadata (the `<type>` patterns and `scope_field`),
    /// also built by `reindex()`.
    alias_categories: rustc_hash::FxHashMap<String, AliasCategoryIndex>,
    /// Lookup index over `types`, built by `reindex()`. Maps a type name to its
    /// index in `types`, so name lookups are O(1) instead of a linear scan.
    type_by_name: rustc_hash::FxHashMap<String, usize>,
    /// Lookup index over `enums`, built by `reindex()`. Maps an enum key to its
    /// index in `enums` for O(1) lookups.
    enum_by_name: rustc_hash::FxHashMap<String, usize>,
    /// Lookup index over `root_rules`, built by `reindex()`. Maps a type-rule
    /// name to its index in `root_rules`, so `find_rules_by_name` is O(1)
    /// instead of a linear scan per root child.
    type_rules_idx: rustc_hash::FxHashMap<String, usize>,
    /// Built by `reindex()`: lowercased leaf key -> each depth-one `<type>`
    /// reference rule. Shared by workspace reference indexing and open-document
    /// reference scans so neither rescans every root rule per candidate leaf.
    type_reference_rules: rustc_hash::FxHashMap<String, Vec<TypeReferenceRule>>,
    /// Built by `reindex()`: lowercased effect/trigger alias key -> the
    /// `value_set[...]` namespace its body declares (e.g. `set_country_flag` ->
    /// `country_flag`). Used to collect dynamically-defined set members (flags,
    /// tokens, …) for completion. Aliases declaring multiple namespaces keep
    /// the first found.
    value_set_effects: rustc_hash::FxHashMap<String, String>,
    /// Built by `reindex()`: lowercased effect/trigger alias key -> the
    /// `(binding_field_key, namespace)` pairs declared by a NESTED field in its
    /// block body (e.g. `generate_character` -> `[("token_base", "character_token")]`,
    /// `set_country_flag` -> `[("flag", "country_flag")]`). Lets the value-set
    /// collector capture the value of the exact field bound to `value_set[ns]`
    /// instead of guessing from a fixed key list, so members under non-obvious keys
    /// (`token_base`, `id`, `legacy_id`, `array`, …) are still collected.
    value_set_effect_fields: rustc_hash::FxHashMap<String, Vec<(String, String)>>,
    /// Built by `reindex()`, parallel to `enums`: each enum's values lowercased
    /// into a set for O(1) case-insensitive membership (matches the
    /// `eq_ignore_ascii_case` scans in the validator). Empty until reindex.
    enum_values_lower: Vec<rustc_hash::FxHashSet<String>>,
    /// Built by `reindex()`, parallel to `enums`: whether the enum has any
    /// `@`-prefixed scripted-constant member. Empty until reindex.
    enum_has_at: Vec<bool>,
    /// Built by `reindex()`, keyed like `values`: each `value[name]` set as a
    /// `FxHashSet` for O(1) exact membership. Empty until reindex.
    value_sets: rustc_hash::FxHashMap<String, rustc_hash::FxHashSet<String>>,
    /// Built by `reindex()`: the lowercased base names (before any `@` scope
    /// suffix) of `values["variable"]` — the config's built-in variable reads
    /// (`faction_leader`, `party_popularity@<ideology>`, …). Lets
    /// [`Self::is_builtin_variable_base`] answer a CW246 candidate in O(1)
    /// instead of scanning the list (~480 entries for HOI4) per checked read.
    /// Empty until reindex.
    builtin_variable_bases: rustc_hash::FxHashSet<String>,
    /// Built by `reindex()` from `alias[<scope>_pre_trigger:<name>] = bool`
    /// declarations: lowercased scope prefix -> lowercased trigger names. CW120 queries this.
    pretriggers: rustc_hash::FxHashMap<String, rustc_hash::FxHashSet<String>>,
    /// Source position of every `type[x]` / `enum[x]` / `complex_enum[x]` /
    /// `single_alias[x]` definition, filled by the directory loader for `.cwt`
    /// goto/hover. Empty for hand-built rulesets.
    pub def_positions: Vec<CwtDefPosition>,
}

/// What a `.cwt` construct under the cursor refers to. Mirrors the reference
/// kinds structural validation resolves (alias categories are out — see
/// `config_validation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwtDefKind {
    Type,
    Enum,
    SingleAlias,
}

/// Where one `.cwt` definition lives: 1-based line, 0-based char col of its
/// defining key.
#[derive(Debug, Clone, PartialEq)]
pub struct CwtDefPosition {
    pub kind: CwtDefKind,
    pub name: String,
    pub file: std::path::PathBuf,
    pub line: u32,
    pub col: u16,
}

/// Scope/link config inputs (`scopes.cwt` / `links.cwt`). The types live in the
/// game crate next to `ScopeRegistry::from_config` (the scope graph's single
/// source of truth); re-exported here because the converter produces them and
/// `RuleSet` carries them.
pub use cwtools_game::scope_registry::{LinkInput, ScopeInput};

/// What kind of placeholder a parsed alias pattern contains.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternKind {
    /// `<type>` or `<type.subtype>` — an instance of that type (subtype
    /// is advisory; only the base name is checked against the type index).
    Type,
    /// `enum[name]` or `complex_enum[name]` — a member of a named enum.
    Enum,
    /// `value[name]` or `value_set[name]` — a member of a named value set.
    Value,
}

/// Alias name pattern pre-parsed at ruleset build time.
///
/// An alias name like `modifier:production_speed_<building>_factor` or
/// `effect:set_country_flag_value[country_flag]` is split once into its
/// structural parts so the per-call `parsed_pattern_matches` can skip the
/// string scanning.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAliasPattern {
    /// Index into `RuleSet::aliases` for the corresponding rule.
    pub alias_idx: usize,
    /// Text before the placeholder (may be empty).
    pub prefix: String,
    /// Text after the placeholder (may be empty).
    pub suffix: String,
    /// What the placeholder represents.
    pub kind: PatternKind,
    /// The type/enum/value-set name inside the placeholder brackets.
    ///
    /// For `<type.subtype>` this stores the full `type.subtype` string; the
    /// base-type extraction (splitting on `.`) happens at match time.
    pub placeholder_name: String,
}

impl ParsedAliasPattern {
    /// Parse the `rest` portion of an alias name (the part after `category:`)
    /// into a `ParsedAliasPattern`. Returns `None` for patterns without a
    /// recognised placeholder (those go into the exact-match index instead).
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
        // Bracketed forms — check longer markers first so `enum[` does not
        // match inside `complex_enum[`. Pick the earliest match.
        // Store (open, inner, close, after) offsets only; resolve kind after
        // picking the earliest match so we never move the PatternKind during
        // the comparison loop.
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

/// Per-category alias index entry (see `RuleSet::alias_categories`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AliasCategoryIndex {
    /// Aliases in this category whose name embeds a placeholder pattern.
    /// Pre-parsed at `reindex()` time so match loops skip per-call string scanning.
    pub parsed_patterns: Vec<ParsedAliasPattern>,
    /// Index of this category's `scope_field` alias, if any.
    pub scope_field_idx: Option<usize>,
}

/// Normalize a `.cwt` path pattern to its lowercase lookup key:
/// `\` -> `/`, strip surrounding `/`, lowercase. Produces exactly the same string
/// as `p.replace('\\', "/").trim_matches('/').to_lowercase()` but skips the
/// `replace` allocation on the common (Linux) no-backslash case.
fn normalize_path_lower(p: &str) -> String {
    if p.contains('\\') {
        p.replace('\\', "/").trim_matches('/').to_ascii_lowercase()
    } else {
        p.trim_matches('/').to_ascii_lowercase()
    }
}

/// Fill in `paths_lower` / `path_file_lower` / `path_ext_lower` from `paths` /
/// `path_file` / `path_extension`. Shared by `RuleSet::reindex()` for both
/// `types` and `complex_enums`, which precompute the same lowercased lookup
/// keys on `PathOptions`.
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

    /// Build the alias lookup indexes from `aliases`. Call once after all aliases
    /// are loaded and post-processed (names/order are stable after that).
    pub fn reindex(&mut self) {
        self.alias_exact.clear();
        self.alias_categories.clear();
        self.pretriggers.clear();
        self.value_set_effects.clear();
        self.value_set_effect_fields.clear();
        // Which value_set namespace (if any) a rule tree declares.
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
        // Every `<specific_key> = value_set[ns]` binding reachable in a rule tree,
        // as `(key, ns)` pairs (see `value_set_effect_fields`).
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
                // `planet_pre_trigger:has_owner` -> pretriggers["planet"].insert("has_owner").
                if let Some(scope) = cat.strip_suffix("_pre_trigger") {
                    self.pretriggers
                        .entry(scope.to_ascii_lowercase())
                        .or_default()
                        .insert(key.to_ascii_lowercase());
                }
                // value_set namespace + binding-field extraction (effect/trigger only).
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
                // Store under the original category+key AND the all-lowercase variant
                // so that game-file keys like `instantTextboxType` (mixed case) match
                // rule alias keys like `instantTextBoxType` (camelCase). Paradox
                // script keys are case-insensitive; aliases are no different.
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
            self.type_by_name.insert(td.name.clone(), i);
        }
        self.enum_by_name.clear();
        for (i, e) in self.enums.iter().enumerate() {
            self.enum_by_name.insert(e.key.clone(), i);
        }
        self.enum_values_lower = self
            .enums
            .iter()
            .map(|e| e.values.iter().map(|v| v.to_ascii_lowercase()).collect())
            .collect();
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
                    // First writer wins — mirrors find_rules_by_name returning the
                    // first TypeRule with a given name.
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

    /// The cached depth-one type-reference rules for `key`, using the same
    /// case-insensitive lookup as script keys. Empty until [`Self::reindex`].
    pub fn type_reference_rules_for_key(&self, key: &str) -> Option<&[TypeReferenceRule]> {
        let rules = if key.bytes().any(|b| b.is_ascii_uppercase()) {
            self.type_reference_rules.get(&key.to_ascii_lowercase())
        } else {
            self.type_reference_rules.get(key)
        }?;
        Some(rules.as_slice())
    }

    /// Case-insensitive membership in enum `idx`'s values. Uses the precomputed
    /// lowercased set built by `reindex()`; falls back to a scan when the set
    /// isn't built yet (e.g. a ruleset assembled in a test without reindex).
    pub fn enum_values_contains_ci(&self, idx: usize, value: &str) -> bool {
        match self.enum_values_lower.get(idx) {
            Some(set) => {
                // The set stores lowercased values; probe directly when `value`
                // is already lowercase, else allocate the lowercased form once.
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

    /// Whether enum `idx` has any `@`-prefixed scripted-constant member. Uses the
    /// precomputed flag from `reindex()`, with a scan fallback.
    pub fn enum_has_at_constant(&self, idx: usize) -> bool {
        match self.enum_has_at.get(idx) {
            Some(&b) => b,
            None => self.enums[idx].values.iter().any(|v| v.starts_with('@')),
        }
    }

    /// Exact membership in the `value[name]` set, mirroring `values.get(name)`:
    /// `Some(is_member)` when the set exists and is non-empty, `None` when the
    /// set is absent or empty. Uses the precomputed `value_sets`, falling back to
    /// the source `values` Vec when it isn't built yet.
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

    /// Whether `base` (a variable token already split on its own `@` suffix)
    /// names a config-declared built-in variable: a member of the
    /// `value[variable]` set, matched by base name before its own `@` suffix
    /// (a declared `party_popularity@<ideology>` covers a read of
    /// `party_popularity@social_democrat`; see #92). Uses the precomputed
    /// `builtin_variable_bases` set built by `reindex()` with a reusable
    /// lowercase buffer, so a checked read pays no allocation. Falls back to
    /// scanning the source `values` list when the set isn't built yet (e.g. a
    /// hand-built ruleset that skipped `reindex()`).
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
            self.types.len() == self.type_by_name.len()
                && self.enums.len() == self.enum_by_name.len()
                && (self.aliases.is_empty()
                    || !self.alias_exact.is_empty()
                    || self.alias_categories.is_empty())
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

/// Builder that makes a stale index unrepresentable: mutate freely, then
/// `finish()` reindexes exactly once and returns the ready `RuleSet`.
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

    /// A builtin declared with a scope suffix (`party_popularity@<ideology>`)
    /// matches a base name split from a read carrying its own suffix
    /// (`party_popularity@social_democrat` -> `party_popularity`),
    /// case-insensitively, via the precomputed `builtin_variable_bases` set
    /// built by `reindex()` (#135, preserving the #92 base-name semantics).
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

    /// Same base-name matching, without calling `reindex()` first, to pin the
    /// scan fallback a hand-built ruleset relies on.
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

    /// `base_name` drops the subtype qualifier and nothing else, for both
    /// reference forms. An unqualified name has to come back untouched, or every
    /// ordinary `<type>` reference would resolve against a truncated name.
    #[test]
    fn base_name_strips_only_the_subtype_qualifier() {
        let simple = |n: &str| TypeType::Simple(n.to_string()).base_name().to_string();
        assert_eq!(simple("equipment"), "equipment");
        assert_eq!(simple("equipment.naval_equip"), "equipment");
        // Only the first `.` separates; anything after it stays part of the
        // qualifier rather than becoming a second base name.
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

/// A rule definition for a type (e.g. `ethos = { ... }`).
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
    /// `## graph_related_types = { ... }`: which other types may join a graph
    /// seeded on this one. Consumed by the LSP's `getGraphData` (`lsp::graph`);
    /// empty means any type.
    pub graph_related_types: Vec<String>,
    pub modifiers: Vec<TypeModifier>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PathOptions {
    pub paths: Vec<String>,
    pub path_strict: bool,
    pub path_file: Option<String>,
    pub path_extension: Option<String>,
    /// Pre-computed lowercased path patterns, built by `RuleSet::reindex()`.
    pub paths_lower: Vec<String>,
    /// Pre-computed lowercased `path_file`, built by `RuleSet::reindex()`.
    pub path_file_lower: Option<String>,
    /// Pre-computed lowercased `path_extension` with leading `.` stripped, built by `RuleSet::reindex()`.
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
    /// `## type_key_filter = X` (or `= { a b }`): the subtype is active when the
    /// instance's own node key is one of these values.
    pub type_key_filter: Vec<String>,
}

/// Whether a `SkipRootKey::MultipleKeys` rule matches when the root key IS one
/// of the listed keys (`Equals`, from a `==` directive) or is NOT (`NotEquals`,
/// from `<>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Equals,
    NotEquals,
}

impl MatchKind {
    /// Build from the old "should_match" bool: `==` (true) -> Equals, else NotEquals.
    pub fn from_equals(is_equals: bool) -> Self {
        if is_equals {
            MatchKind::Equals
        } else {
            MatchKind::NotEquals
        }
    }

    /// Whether this is the `Equals` (`==`) kind.
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
    /// Whether this declares a key CW100 can flag: `## required`, not
    /// `## optional`, and derived from the instance name rather than a child
    /// field. This is what "missing localisation" means; the loc-display paths
    /// (hover, graph labels) use a wider `primary || required` test instead.
    pub fn is_required_name_derived(&self) -> bool {
        self.required && !self.optional && self.explicit_field.is_none()
    }

    /// The child field a `## required` loc key is read from, for the
    /// explicit-field form (`## required title = title`). `None` when the entry
    /// is optional, not required, or derives its key from the instance name.
    pub fn required_explicit_field(&self) -> Option<&str> {
        if !self.required || self.optional {
            return None;
        }
        self.explicit_field.as_deref()
    }

    /// The key this definition derives for an instance called `name`. Only
    /// meaningful when `explicit_field` is unset, where the key comes from a
    /// child field's value instead. Case is left alone; the loc index is
    /// lowercased, so callers comparing against it lowercase the result.
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

/// A rule is a (RuleType, Options) pair.
pub type NewRule = (RuleType, Options);

/// The child rules of a clause-shaped rule.
///
/// Shared rather than owned so cloning a rule is O(1) in its subtree: single
/// alias inlining substitutes the same body at every reference site, and the
/// editor paths (`rules_at_pos`, `value_rules_for_key`, semantic tokens) copy
/// matched rules out per request. With an owned `Vec` both duplicate the whole
/// tree. Rebuild through `Arc::make_mut` or by assigning a fresh `Vec`; the
/// post-processing passes in `post_process` are the only writers.
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
    // SingleAliasClauseField removed: never constructed by the converter.
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
    // JominiGuiField removed: never constructed.
    IgnoreMarkerField,
    IgnoreField(Box<NewField>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValueType {
    Enum(String),
    Float {
        min: f64,
        max: f64,
    },
    Bool,
    Int {
        min: i32,
        max: i32,
    },
    Percent,
    Date,
    DateTime,
    Ck2Dna,
    Ck2DnaProperty,
    IrFamilyName,
    StlNameFormat(String),
    /// A recursive math-expression operand (HOI4 `set_variable` math blocks).
    /// As a leaf it is a number or variable reference; as a `{block}` it is a
    /// `value` base plus `mathexpr` operator keys, validated strictly so a
    /// mis-typed operator is flagged rather than silently treated as a new
    /// variable assignment. See `rule_core::validate_math_clause`.
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
    /// The type a `<type>` / `<type.subtype>` reference resolves against, with
    /// any subtype qualifier dropped (`equipment.naval_equip` -> `equipment`).
    /// The qualifier constrains which instances match, but the definition is
    /// keyed by the base type, so anything looking the type up in a ruleset or
    /// an index wants this rather than the name as written.
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

/// The label and direction of a reference declared via `## outgoingReferenceLabel`
/// (`Outgoing`) or `## incomingReferenceLabel` (`Incoming`).
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
    /// `## outgoingReferenceLabel`/`## incomingReferenceLabel`: parsed for .cwt
    /// spec compatibility; not consumed.
    pub reference_details: Option<Box<ReferenceDetail>>,
    // key_required_quotes, value_required_quotes, type_hint removed:
    // always default-valued, no readers (quoted-key enforcement unimplemented).
    pub error_if_only_match: Option<String>,
    /// `## default_bool = yes|no`: the field's engine default. When the field is
    /// set to this value an info-level hint (CW282) notes it can be omitted.
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

/// Represents the `name = { ... }` subtree inside a complex_enum definition.
/// This captures the key-path structure used to extract enum member names from files.
#[derive(Debug, Clone, PartialEq)]
pub enum ComplexEnumNameTree {
    /// No name block was present.
    Empty,
    /// A list of leaf/node entries describing the name-extraction path.
    Entries(Vec<ComplexEnumNameTreeEntry>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComplexEnumNameTreeEntry {
    /// A leaf entry: the key under which the enum name lives.
    /// `is_name` is true when the value is `enum_name`/`this`.
    Leaf { key: String, is_name: bool },
    /// A nested node entry: descend into `key` then recurse.
    Node {
        key: String,
        children: ComplexEnumNameTree,
    },
    /// A bare `enum_name` value inside a block (`stats = { enum_name }`):
    /// every bare value at this level of the target file is an enum member.
    BareName,
}

/// Root-level rule from a .cwt file.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum RootRule {
    AliasRule(String, NewRule),
    SingleAliasRule(String, NewRule),
    TypeRule(String, NewRule),
}
