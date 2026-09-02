use crate::rules_types::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasExpansionError {
    pub name: String,
    pub message: String,
}

fn for_each_root_rule_mut(ruleset: &mut RuleSet, mut f: impl FnMut(&mut NewRule)) {
    for root in ruleset.root_rules.iter_mut() {
        match root {
            RootRule::TypeRule(_, rule) => f(rule),
            RootRule::AliasRule(_, rule) => f(rule),
            RootRule::SingleAliasRule(_, rule) => f(rule),
        }
    }
    for (_, rule) in ruleset.aliases.iter_mut() {
        f(rule);
    }
    for (_, rule) in ruleset.single_aliases.iter_mut() {
        f(rule);
    }
}

fn body_of(rt: &RuleType) -> Option<&RuleBody> {
    match rt {
        RuleType::NodeRule { rules, .. }
        | RuleType::ValueClauseRule { rules }
        | RuleType::SubtypeRule { rules, .. } => Some(rules),
        _ => None,
    }
}

fn body_mut(rt: &mut RuleType) -> Option<&mut RuleBody> {
    match rt {
        RuleType::NodeRule { rules, .. }
        | RuleType::ValueClauseRule { rules }
        | RuleType::SubtypeRule { rules, .. } => Some(rules),
        _ => None,
    }
}

#[derive(Default)]
struct CleanCache {
    seen: FxHashSet<*const NewRule>,
    keep: Vec<RuleBody>,
}

impl CleanCache {
    fn any_rule(&mut self, rule: &NewRule, pred: &dyn Fn(&RuleType) -> bool) -> bool {
        pred(&rule.0) || body_of(&rule.0).is_some_and(|b| self.any_in_body(b, pred))
    }

    fn any_in_body(&mut self, body: &RuleBody, pred: &dyn Fn(&RuleType) -> bool) -> bool {
        if body.is_empty() {
            return false;
        }
        let key = body.as_ptr();
        if self.seen.contains(&key) {
            return false;
        }
        if body.iter().any(|r| self.any_rule(r, pred)) {
            return true;
        }
        self.seen.insert(key);
        self.keep.push(body.clone());
        false
    }
}

#[tracing::instrument(skip_all)]
pub fn post_process(ruleset: &mut RuleSet) -> Vec<AliasExpansionError> {
    post_process_with_budget(ruleset, MAX_EXPANDED_NODES)
}

fn post_process_with_budget(ruleset: &mut RuleSet, max_nodes: usize) -> Vec<AliasExpansionError> {
    replace_value_marker_fields(ruleset);
    let errors = replace_single_aliases(ruleset, max_nodes);
    replace_colour_field(ruleset);
    replace_ignore_marker_fields(ruleset);
    errors
}

const MAX_EXPANDED_NODES: usize = 1_000_000;

const MAX_ALIAS_DEPTH: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Resolution {
    Pending,
    InProgress,
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Refusal {
    Cycle,
    Depth,
    Budget,
}

struct Expander {
    by_name: FxHashMap<String, usize>,
    bodies: Vec<Option<NewRule>>,
    state: Vec<Resolution>,
    size: Vec<usize>,
    limit: usize,
    remaining: usize,
    refused: FxHashMap<(String, Refusal), usize>,
    clean: CleanCache,
}

fn replace_single_aliases(ruleset: &mut RuleSet, max_nodes: usize) -> Vec<AliasExpansionError> {
    let defs = std::mem::take(&mut ruleset.single_aliases);
    let mut expander = Expander::new(&defs, max_nodes);
    for idx in 0..defs.len() {
        expander.resolve(idx, 0);
    }
    ruleset.single_aliases = defs
        .into_iter()
        .enumerate()
        .map(|(idx, (name, rule))| (name, expander.bodies[idx].clone().unwrap_or(rule)))
        .collect();

    for_each_root_rule_mut(ruleset, |rule| expander.inline_rule(rule));

    let errors = expander.into_errors();
    if !errors.is_empty() {
        tracing::warn!(
            count = errors.len(),
            "single_alias expansion left references unexpanded"
        );
    }
    errors
}

impl Expander {
    fn new(defs: &[(String, NewRule)], max_nodes: usize) -> Self {
        let mut by_name = FxHashMap::default();
        for (idx, (name, _)) in defs.iter().enumerate() {
            by_name.entry(name.clone()).or_insert(idx);
        }
        Self {
            by_name,
            bodies: defs.iter().map(|(_, rule)| Some(rule.clone())).collect(),
            state: vec![Resolution::Pending; defs.len()],
            size: vec![0; defs.len()],
            limit: max_nodes,
            remaining: max_nodes,
            refused: FxHashMap::default(),
            clean: CleanCache::default(),
        }
    }

    fn resolve(&mut self, idx: usize, depth: usize) {
        if self.state[idx] != Resolution::Pending {
            return;
        }
        if depth >= MAX_ALIAS_DEPTH {
            return;
        }
        self.state[idx] = Resolution::InProgress;
        let mut deps = Vec::new();
        collect_refs(self.bodies[idx].as_ref().expect("pending body"), &mut deps);
        for dep in deps {
            if let Some(&dep_idx) = self.by_name.get(&dep) {
                self.resolve(dep_idx, depth + 1);
            }
        }
        let mut body = self.bodies[idx].take().expect("pending body");
        self.inline_rule(&mut body);
        self.size[idx] = logical_size(&body);
        self.bodies[idx] = Some(body);
        self.state[idx] = Resolution::Done;
    }

    fn take_alias(&mut self, name: &str) -> Option<NewRule> {
        let idx = *self.by_name.get(name)?;
        match self.state[idx] {
            Resolution::Done => {}
            Resolution::InProgress => {
                self.refuse(name, Refusal::Cycle);
                return None;
            }
            Resolution::Pending => {
                self.refuse(name, Refusal::Depth);
                return None;
            }
        }
        let size = self.size[idx];
        if size > self.remaining {
            self.refuse(name, Refusal::Budget);
            return None;
        }
        self.remaining -= size;
        self.bodies[idx].clone()
    }

    fn refuse(&mut self, name: &str, reason: Refusal) {
        *self.refused.entry((name.to_string(), reason)).or_default() += 1;
    }

    fn inline_rule(&mut self, rule: &mut NewRule) {
        if let RuleType::LeafRule {
            right: NewField::SingleAliasField(name),
            ..
        } = &rule.0
        {
            let name = name.clone();
            if let Some(resolved) = self.take_alias(&name) {
                let left = extract_leaf_left(&rule.0);
                match resolved.0 {
                    RuleType::LeafRule { right: ar, .. } => {
                        rule.0 = RuleType::LeafRule { left, right: ar }
                    }
                    RuleType::NodeRule { rules: ar, .. } => {
                        rule.0 = RuleType::NodeRule { left, rules: ar }
                    }
                    _ => {}
                }
            }
            return;
        }
        if let Some(rules) = body_mut(&mut rule.0) {
            self.inline_list(rules);
        }
    }

    fn inline_list(&mut self, rules: &mut RuleBody) {
        if !self.clean.any_in_body(rules, &is_single_alias_ref) {
            return;
        }
        if !rules.iter().any(|r| is_single_alias_ref(&r.0)) {
            for rule in Arc::make_mut(rules) {
                self.inline_rule(rule);
            }
            return;
        }
        let mut out: Vec<NewRule> = Vec::with_capacity(rules.len());
        for rule in rules.iter() {
            let mut rule = rule.clone();
            match &rule.0 {
                RuleType::LeafRule {
                    left: _,
                    right: NewField::SingleAliasField(name),
                } => {
                    let name = name.clone();
                    let opts = rule.1.clone();
                    match self.take_alias(&name).map(|resolved| resolved.0) {
                        Some(RuleType::LeafRule { right: ar, .. }) => {
                            out.push((
                                RuleType::LeafRule {
                                    left: extract_leaf_left(&rule.0),
                                    right: ar,
                                },
                                opts,
                            ));
                        }
                        Some(RuleType::NodeRule { rules: ar, .. }) => {
                            out.push((
                                RuleType::NodeRule {
                                    left: extract_leaf_left(&rule.0),
                                    rules: ar,
                                },
                                opts,
                            ));
                        }
                        Some(other) => out.push((other, opts)),
                        None => out.push(rule),
                    }
                }
                RuleType::NodeRule { .. }
                | RuleType::ValueClauseRule { .. }
                | RuleType::SubtypeRule { .. } => {
                    self.inline_rule(&mut rule);
                    out.push(rule);
                }
                _ => {
                    out.push(rule);
                }
            }
        }
        *rules = out.into();
    }

    fn into_errors(self) -> Vec<AliasExpansionError> {
        let limit = self.limit;
        let mut errors: Vec<AliasExpansionError> = self
            .refused
            .into_iter()
            .map(|((name, reason), count)| {
                let detail = match reason {
                    Refusal::Cycle => "is part of a reference cycle".to_string(),
                    Refusal::Depth => {
                        format!("nests deeper than the {MAX_ALIAS_DEPTH}-level chain limit")
                    }
                    Refusal::Budget => {
                        format!("would take the rules past the {limit}-node expansion budget")
                    }
                };
                AliasExpansionError {
                    message: format!(
                        "single_alias[{name}] {detail}; {count} reference(s) left unexpanded"
                    ),
                    name,
                }
            })
            .collect();
        errors.sort_by(|a, b| (&a.name, &a.message).cmp(&(&b.name, &b.message)));
        errors
    }
}

fn is_single_alias_ref(rt: &RuleType) -> bool {
    matches!(
        rt,
        RuleType::LeafRule {
            right: NewField::SingleAliasField(..),
            ..
        }
    )
}

fn collect_refs(rule: &NewRule, out: &mut Vec<String>) {
    match &rule.0 {
        RuleType::LeafRule {
            right: NewField::SingleAliasField(name),
            ..
        } => out.push(name.clone()),
        RuleType::NodeRule { rules, .. }
        | RuleType::ValueClauseRule { rules }
        | RuleType::SubtypeRule { rules, .. } => rules.iter().for_each(|r| collect_refs(r, out)),
        _ => {}
    }
}

fn logical_size(rule: &NewRule) -> usize {
    1 + body_of(&rule.0).map_or(0, |b| b.iter().map(logical_size).sum::<usize>())
}

fn extract_leaf_left(rt: &RuleType) -> NewField {
    match rt {
        RuleType::LeafRule { left, .. } => left.clone(),
        _ => NewField::ScalarField,
    }
}

fn replace_colour_field(ruleset: &mut RuleSet) {
    let mut clean = CleanCache::default();
    for_each_root_rule_mut(ruleset, |rule| expand_colour_in_rule(rule, &mut clean));
}

fn expand_colour_in_rule(rule: &mut NewRule, clean: &mut CleanCache) {
    if let Some(rules) = body_mut(&mut rule.0) {
        expand_colour_in_list(rules, clean);
    }
}

fn is_colour_marker(rt: &RuleType) -> bool {
    matches!(
        rt,
        RuleType::LeafRule {
            right: NewField::MarkerField(Marker::ColourField),
            ..
        } | RuleType::LeafRule {
            left: NewField::MarkerField(Marker::IrCountryTag),
            ..
        } | RuleType::LeafRule {
            right: NewField::MarkerField(Marker::IrCountryTag),
            ..
        } | RuleType::NodeRule {
            left: NewField::MarkerField(Marker::IrCountryTag),
            ..
        }
    )
}

fn expand_colour_in_list(rules: &mut RuleBody, clean: &mut CleanCache) {
    if !clean.any_in_body(rules, &is_colour_marker) {
        return;
    }
    if !rules.iter().any(|r| is_colour_marker(&r.0)) {
        for rule in Arc::make_mut(rules) {
            expand_colour_in_rule(rule, clean);
        }
        return;
    }
    let mut out: Vec<NewRule> = Vec::with_capacity(rules.len());
    for rule in rules.iter() {
        out.extend(expand_colour_rule(rule.clone(), clean));
    }
    *rules = out.into();
}

fn expand_colour_rule(mut rule: NewRule, clean: &mut CleanCache) -> Vec<NewRule> {
    match &rule.0 {
        RuleType::LeafRule {
            right: NewField::MarkerField(Marker::ColourField),
            ..
        } => {
            let left = extract_leaf_left(&rule.0);
            let opts = rule.1.clone();
            let inner_rule = (
                RuleType::LeafValueRule {
                    right: NewField::ValueField(ValueType::Float {
                        min: -256.0,
                        max: 256.0,
                    }),
                },
                Options {
                    min: 3,
                    max: 3,
                    strict_min: true,
                    leafvalue: true,
                    ..Options::default()
                },
            );
            vec![(
                RuleType::NodeRule {
                    left,
                    rules: [inner_rule].into(),
                },
                opts,
            )]
        }
        RuleType::LeafRule {
            right: NewField::MarkerField(Marker::IrCountryTag),
            ..
        } => {
            let left = extract_leaf_left(&rule.0);
            let opts = rule.1.clone();
            vec![
                (
                    RuleType::LeafRule {
                        left: left.clone(),
                        right: NewField::ValueField(ValueType::Enum("country_tags".to_string())),
                    },
                    opts.clone(),
                ),
                (
                    RuleType::LeafRule {
                        left,
                        right: NewField::VariableGetField("dynamic_country_tag".to_string()),
                    },
                    opts,
                ),
            ]
        }
        RuleType::LeafRule {
            left: NewField::MarkerField(Marker::IrCountryTag),
            ..
        } => {
            let right = extract_leaf_right(&rule.0);
            let opts = rule.1.clone();
            vec![
                (
                    RuleType::LeafRule {
                        left: NewField::ValueField(ValueType::Enum("country_tags".to_string())),
                        right: right.clone(),
                    },
                    opts.clone(),
                ),
                (
                    RuleType::LeafRule {
                        left: NewField::VariableGetField("dynamic_country_tag".to_string()),
                        right,
                    },
                    opts,
                ),
            ]
        }
        RuleType::NodeRule {
            left: NewField::MarkerField(Marker::IrCountryTag),
            ..
        } => {
            if let RuleType::NodeRule { rules, .. } = rule.0 {
                let opts = rule.1.clone();
                let mut rules_a = rules.clone();
                expand_colour_in_list(&mut rules_a, clean);
                let mut rules_b = rules;
                expand_colour_in_list(&mut rules_b, clean);
                vec![
                    (
                        RuleType::NodeRule {
                            left: NewField::ValueField(ValueType::Enum("country_tags".to_string())),
                            rules: rules_a,
                        },
                        opts.clone(),
                    ),
                    (
                        RuleType::NodeRule {
                            left: NewField::VariableGetField("dynamic_country_tag".to_string()),
                            rules: rules_b,
                        },
                        opts,
                    ),
                ]
            } else {
                vec![rule]
            }
        }
        RuleType::NodeRule { .. }
        | RuleType::ValueClauseRule { .. }
        | RuleType::SubtypeRule { .. } => {
            expand_colour_in_rule(&mut rule, clean);
            vec![rule]
        }
        _ => vec![rule],
    }
}

fn extract_leaf_right(rt: &RuleType) -> NewField {
    match rt {
        RuleType::LeafRule { right, .. } => right.clone(),
        _ => NewField::ScalarField,
    }
}

fn replace_value_marker_fields(ruleset: &mut RuleSet) {
    let mut clean = CleanCache::default();
    for_each_root_rule_mut(ruleset, |rule| rewrite_vsm_in_rule(rule, &mut clean));
}

fn rewrite_vsm_in_rule(rule: &mut NewRule, clean: &mut CleanCache) {
    let (rt, _) = rule;
    match rt {
        RuleType::LeafRule { left, right } => {
            rewrite_vsm_field(left);
            rewrite_vsm_field(right);
        }
        RuleType::LeafValueRule { right } => {
            rewrite_vsm_field(right);
        }
        RuleType::NodeRule { left, rules } => {
            rewrite_vsm_field(left);
            rewrite_vsm_in_list(rules, clean);
        }
        RuleType::ValueClauseRule { rules } => {
            rewrite_vsm_in_list(rules, clean);
        }
        RuleType::SubtypeRule { rules, .. } => {
            rewrite_vsm_in_list(rules, clean);
        }
    }
}

fn has_vsm_field(rt: &RuleType) -> bool {
    let is_vsm = |f: &NewField| matches!(f, NewField::ValueScopeMarkerField { .. });
    match rt {
        RuleType::LeafRule { left, right } => is_vsm(left) || is_vsm(right),
        RuleType::LeafValueRule { right } => is_vsm(right),
        RuleType::NodeRule { left, .. } => is_vsm(left),
        _ => false,
    }
}

fn rewrite_vsm_in_list(rules: &mut RuleBody, clean: &mut CleanCache) {
    if !clean.any_in_body(rules, &has_vsm_field) {
        return;
    }
    for rule in Arc::make_mut(rules) {
        rewrite_vsm_in_rule(rule, clean);
    }
}

fn rewrite_vsm_field(field: &mut NewField) {
    if let NewField::ValueScopeMarkerField { is_int, min, max } = field {
        *field = NewField::ValueScopeField {
            is_int: *is_int,
            min: *min,
            max: *max,
        };
    }
}

fn replace_ignore_marker_fields(ruleset: &mut RuleSet) {
    let mut clean = CleanCache::default();
    for_each_root_rule_mut(ruleset, |rule| expand_ignore_in_rule(rule, &mut clean));
}

fn expand_ignore_in_rule(rule: &mut NewRule, clean: &mut CleanCache) {
    if let Some(rules) = body_mut(&mut rule.0) {
        expand_ignore_in_list(rules, clean);
    }
}

fn is_ignore_marker(rt: &RuleType) -> bool {
    matches!(
        rt,
        RuleType::LeafRule {
            right: NewField::IgnoreMarkerField,
            ..
        }
    )
}

fn expand_ignore_in_list(rules: &mut RuleBody, clean: &mut CleanCache) {
    if !clean.any_in_body(rules, &is_ignore_marker) {
        return;
    }
    for rule in Arc::make_mut(rules) {
        if is_ignore_marker(&rule.0) {
            let left = extract_leaf_left(&rule.0);
            let opts = rule.1.clone();
            *rule = (
                RuleType::NodeRule {
                    left: NewField::IgnoreField(Box::new(left)),
                    rules: RuleBody::default(),
                },
                opts,
            );
        } else {
            expand_ignore_in_rule(rule, clean);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules_converter::ast_to_ruleset;
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;

    fn parse_and_post(input: &str) -> RuleSet {
        let (ruleset, errors) = parse_and_post_with_budget(input, MAX_EXPANDED_NODES);
        assert!(errors.is_empty(), "expansion refused: {errors:?}");
        ruleset
    }

    fn parse_and_post_with_budget(
        input: &str,
        max_nodes: usize,
    ) -> (RuleSet, Vec<AliasExpansionError>) {
        let table = StringTable::new();
        let parsed = parse_string(input, &table);
        let mut ruleset = ast_to_ruleset(&parsed, &table);
        let errors = post_process_with_budget(&mut ruleset, max_nodes);
        (ruleset, errors)
    }

    fn alias_body<'a>(ruleset: &'a RuleSet, name: &str) -> &'a NewRule {
        ruleset
            .aliases
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, rule)| rule)
            .unwrap_or_else(|| panic!("no alias[{name}]"))
    }

    fn distinct_nodes(rule: &NewRule, seen: &mut FxHashSet<*const NewRule>) -> usize {
        1 + body_of(&rule.0).map_or(0, |body| {
            if body.is_empty() || !seen.insert(body.as_ptr()) {
                return 0;
            }
            body.iter().map(|r| distinct_nodes(r, seen)).sum::<usize>()
        })
    }

    fn fan_out_config(fan_out: usize) -> String {
        let mut config = String::from("single_alias[level_c] = {\n");
        for i in 0..fan_out {
            config.push_str(&format!("    c_{i} = scalar\n"));
        }
        config.push_str("}\nsingle_alias[level_b] = {\n");
        for i in 0..fan_out {
            config.push_str(&format!("    b_{i} = single_alias_right[level_c]\n"));
        }
        config.push_str("}\nalias[effect:top] = {\n");
        for i in 0..fan_out {
            config.push_str(&format!("    a_{i} = single_alias_right[level_b]\n"));
        }
        config.push_str("}\n");
        config
    }

    fn body_at<'a>(rule: &'a NewRule, path: &[&str]) -> &'a [NewRule] {
        let mut rules: &[NewRule] = match &rule.0 {
            RuleType::NodeRule { rules, .. } => rules,
            other => panic!("expected a NodeRule, got {other:?}"),
        };
        for key in path {
            let next = rules
                .iter()
                .find(|(rt, _)| {
                    matches!(rt, RuleType::NodeRule { left: NewField::SpecificField(s), .. } if s == key)
                })
                .unwrap_or_else(|| panic!("no `{key}` block in {rules:?}"));
            rules = match &next.0 {
                RuleType::NodeRule { rules, .. } => rules,
                _ => unreachable!(),
            };
        }
        rules
    }

    fn rule_named<'a>(rules: &'a [NewRule], key: &str) -> &'a NewRule {
        rules
            .iter()
            .find(|(rt, _)| match rt {
                RuleType::LeafRule {
                    left: NewField::SpecificField(s),
                    ..
                }
                | RuleType::NodeRule {
                    left: NewField::SpecificField(s),
                    ..
                } => s == key,
                _ => false,
            })
            .unwrap_or_else(|| panic!("no `{key}` rule in {rules:?}"))
    }

    #[test]
    fn a_marker_nested_below_the_top_level_still_expands() {
        let input = r#"
single_alias[deep_sa] = {
    ## cardinality = 0..1
    from_alias = scalar
}

alias[effect:nested] = {
    outer = {
        inner = {
            colour = colour_field
            amount = value_field
            skipped = ignore_field
            aliased = single_alias_right[deep_sa]
        }
    }
}
"#;
        let rs = parse_and_post(input);
        let (_, rule) = rs
            .aliases
            .iter()
            .find(|(n, _)| n == "effect:nested")
            .unwrap();
        let inner = body_at(rule, &["outer", "inner"]);

        match &rule_named(inner, "amount").0 {
            RuleType::LeafRule { right, .. } => assert!(
                matches!(right, NewField::ValueScopeField { .. }),
                "value_field not rewritten: {right:?}"
            ),
            other => panic!("expected a LeafRule for `amount`, got {other:?}"),
        }

        let aliased = body_at(rule_named(inner, "aliased"), &[]);
        assert!(
            aliased
                .iter()
                .any(|(rt, _)| matches!(rt, RuleType::LeafRule { left: NewField::SpecificField(s), .. } if s == "from_alias")),
            "single_alias not inlined: {aliased:?}"
        );

        let colour = body_at(rule_named(inner, "colour"), &[]);
        assert_eq!(colour.len(), 1, "colour body: {colour:?}");
        assert!(
            matches!(
                &colour[0].0,
                RuleType::LeafValueRule {
                    right: NewField::ValueField(ValueType::Float { .. })
                }
            ) && colour[0].1.min == 3,
            "colour_field not expanded: {colour:?}"
        );

        assert!(
            inner.iter().any(|(rt, _)| matches!(
                rt,
                RuleType::NodeRule {
                    left: NewField::IgnoreField(_),
                    ..
                }
            )),
            "ignore_field not expanded: {inner:?}"
        );
    }

    #[test]
    fn test_single_alias_inline_leaf() {
        let input = r#"
single_alias[my_sa] = scalar

alias[effect:test] = {
    ## cardinality = 0..inf
    my_field = single_alias_right[my_sa]
}
"#;
        let rs = parse_and_post(input);
        let (_, (rule, _)) = rs.aliases.iter().find(|(n, _)| n == "effect:test").unwrap();
        if let RuleType::NodeRule { rules, .. } = rule {
            let (inner_rule, _) = &rules[0];
            match inner_rule {
                RuleType::LeafRule { right, .. } => {
                    assert!(
                        matches!(right, NewField::ScalarField),
                        "expected ScalarField after single_alias inline, got {:?}",
                        right
                    );
                }
                other => panic!("expected LeafRule, got {:?}", other),
            }
        } else {
            panic!("expected NodeRule");
        }
    }

    #[test]
    fn test_single_alias_inline_node() {
        let input = r#"
single_alias[my_node_sa] = {
    ## cardinality = 0..1
    inner_key = scalar
}

alias[effect:node_test] = {
    ## cardinality = 0..inf
    block_ref = single_alias_right[my_node_sa]
}
"#;
        let rs = parse_and_post(input);
        let (_, (rule, _)) = rs
            .aliases
            .iter()
            .find(|(n, _)| n == "effect:node_test")
            .unwrap();
        if let RuleType::NodeRule { rules, .. } = rule {
            let (inner_rule, _) = &rules[0];
            match inner_rule {
                RuleType::NodeRule {
                    rules: inner_rules, ..
                } => {
                    assert!(
                        !inner_rules.is_empty(),
                        "inlined node alias should have inner rules"
                    );
                }
                other => panic!("expected NodeRule after node-alias inline, got {:?}", other),
            }
        } else {
            panic!("expected outer NodeRule");
        }
    }

    #[test]
    fn test_single_alias_inline_whole_body() {
        let input = r#"
single_alias[every_clause] = {
    ## cardinality = 0..1
    limit = scalar
}

alias[effect:every_country] = single_alias_right[every_clause]
"#;
        let rs = parse_and_post(input);
        let (_, (rule, _)) = rs
            .aliases
            .iter()
            .find(|(n, _)| n == "effect:every_country")
            .unwrap();
        match rule {
            RuleType::NodeRule { rules, .. } => {
                assert!(
                    rules.iter().any(|(rt, _)| matches!(rt,
                        RuleType::LeafRule { left: NewField::SpecificField(s), .. } if s == "limit")),
                    "expected 'limit' rule from inlined every_clause, got {:?}", rules
                );
            }
            other => panic!(
                "expected NodeRule after whole-body single_alias inline, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn fan_out_expands_by_sharing_not_copying() {
        let measure = |fan_out: usize| {
            let ruleset = parse_and_post(&fan_out_config(fan_out));
            let top = alias_body(&ruleset, "effect:top");
            let mut seen = FxHashSet::default();
            (distinct_nodes(top, &mut seen), logical_size(top))
        };
        let (small_distinct, small_logical) = measure(16);
        let (large_distinct, large_logical) = measure(32);

        assert!(
            large_logical > 7 * small_logical,
            "doubling the fan-out should cube the described tree: {small_logical} -> {large_logical}"
        );
        assert!(
            large_distinct < 3 * small_distinct,
            "doubling the fan-out must not square what is materialized: {small_distinct} -> {large_distinct}"
        );
    }

    #[test]
    fn expansion_past_the_budget_keeps_the_reference_and_reports_it() {
        let input = r#"
single_alias[block] = {
    a = scalar
    b = scalar
    c = scalar
}

alias[effect:uses_block] = {
    one = single_alias_right[block]
    two = single_alias_right[block]
    three = single_alias_right[block]
    four = single_alias_right[block]
}
"#;
        let (ruleset, errors) = parse_and_post_with_budget(input, 9);

        assert_eq!(errors.len(), 1, "one diagnostic per definition: {errors:?}");
        assert_eq!(errors[0].name, "block");
        assert!(
            errors[0].message.contains("9-node expansion budget")
                && errors[0].message.contains("2 reference(s)"),
            "message should name the budget and the sites left: {}",
            errors[0].message
        );

        let body = body_at(alias_body(&ruleset, "effect:uses_block"), &[]);
        let unexpanded = body
            .iter()
            .filter(|(rt, _)| is_single_alias_ref(rt))
            .count();
        assert_eq!(unexpanded, 2, "over-budget references must stay: {body:?}");
    }

    #[test]
    fn a_reference_cycle_is_reported_and_stops_expanding() {
        let input = r#"
single_alias[loop_a] = {
    x = single_alias_right[loop_b]
}

single_alias[loop_b] = {
    y = single_alias_right[loop_a]
}

alias[effect:uses_loop] = {
    z = single_alias_right[loop_a]
}
"#;
        let (ruleset, errors) = parse_and_post_with_budget(input, MAX_EXPANDED_NODES);

        assert_eq!(errors.len(), 1, "one diagnostic per cycle: {errors:?}");
        assert_eq!(errors[0].name, "loop_a");
        assert!(
            errors[0].message.contains("reference cycle"),
            "message should say why: {}",
            errors[0].message
        );
        assert!(
            logical_size(alias_body(&ruleset, "effect:uses_loop")) < 32,
            "a cycle must not grow the tree it is in"
        );
    }

    #[test]
    fn test_colour_field_expands_to_node_rule() {
        let input = r#"
alias[effect:colour_test] = {
    ## cardinality = 0..1
    colour = colour_field
}
"#;
        let rs = parse_and_post(input);
        let (_, (rule, _)) = rs
            .aliases
            .iter()
            .find(|(n, _)| n == "effect:colour_test")
            .unwrap();
        if let RuleType::NodeRule { rules, .. } = rule {
            let (inner, _) = &rules[0];
            match inner {
                RuleType::NodeRule {
                    rules: colour_inner,
                    ..
                } => {
                    assert_eq!(
                        colour_inner.len(),
                        1,
                        "colour NodeRule should have 1 LeafValue child"
                    );
                    let (lv, lv_opts) = &colour_inner[0];
                    match lv {
                        RuleType::LeafValueRule {
                            right: NewField::ValueField(ValueType::Float { min, max }),
                        } => {
                            assert_eq!(*min, -256.0, "colour float min");
                            assert_eq!(*max, 256.0, "colour float max");
                        }
                        other => panic!("colour child should be Float(-256..256), got {:?}", other),
                    }
                    assert_eq!(lv_opts.min, 3);
                    assert_eq!(lv_opts.max, 3);
                }
                other => panic!(
                    "expected NodeRule from colour_field expansion, got {:?}",
                    other
                ),
            }
        } else {
            panic!("expected outer NodeRule");
        }
    }

    #[test]
    fn test_value_scope_marker_rewrite() {
        let input = r#"
alias[trigger:val_test] = {
    ## cardinality = 0..inf
    amount = value_field
}
"#;
        let rs = parse_and_post(input);
        let (_, (rule, _)) = rs
            .aliases
            .iter()
            .find(|(n, _)| n == "trigger:val_test")
            .unwrap();
        if let RuleType::NodeRule { rules, .. } = rule {
            let (inner, _) = &rules[0];
            match inner {
                RuleType::LeafRule { right, .. } => {
                    assert!(
                        matches!(right, NewField::ValueScopeField { is_int: false, .. }),
                        "expected ValueScopeField, got {:?}",
                        right
                    );
                }
                other => panic!("expected LeafRule, got {:?}", other),
            }
        } else {
            panic!("expected outer NodeRule");
        }
    }

    #[test]
    fn test_ignore_marker_expands() {
        let input = r#"
alias[effect:ignore_test] = {
    ## cardinality = 0..inf
    some_key = ignore_field
}
"#;
        let rs = parse_and_post(input);
        let (_, (rule, _)) = rs
            .aliases
            .iter()
            .find(|(n, _)| n == "effect:ignore_test")
            .unwrap();
        if let RuleType::NodeRule { rules, .. } = rule {
            let (inner, _) = &rules[0];
            match inner {
                RuleType::NodeRule {
                    left: NewField::IgnoreField(boxed),
                    rules: inner_rules,
                } => {
                    assert!(
                        matches!(boxed.as_ref(), NewField::SpecificField(_)),
                        "IgnoreField should wrap the original key field"
                    );
                    assert!(
                        inner_rules.is_empty(),
                        "IgnoreField NodeRule should have no children"
                    );
                }
                other => panic!("expected NodeRule(IgnoreField(..), []), got {:?}", other),
            }
        } else {
            panic!("expected outer NodeRule");
        }
    }
}
