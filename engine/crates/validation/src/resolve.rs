use cwtools_index::dir_matches_pattern;
use cwtools_rules::rules_types::*;

/// paths are `/`-separated, so a Windows backslash path would make `rsplit('/')`
fn lookup_path(file_path: &str) -> String {
    file_path.to_lowercase().replace('\\', "/")
}

pub(crate) fn should_skip_root_key(key: &str, type_def: &TypeDefinition) -> bool {
    type_def
        .skip_root_key
        .first()
        .is_some_and(|sk| cwtools_index::skip_root_key_matches(sk, key))
}

pub(crate) fn skip_root_key_tail(
    type_def: &TypeDefinition,
) -> &[cwtools_rules::rules_types::SkipRootKey] {
    type_def.skip_root_key.get(1..).unwrap_or(&[])
}

pub(crate) fn find_type_and_rules<'a>(
    name: &str,
    ruleset: &'a RuleSet,
) -> Option<(&'a TypeDefinition, &'a [(RuleType, Options)])> {
    let type_def = ruleset
        .type_by_name()
        .get(name)
        .map(|&i| &ruleset.types[i])?;
    let rules = find_rules_by_name(name, ruleset);
    Some((type_def, rules))
}

pub(crate) fn type_extension_matches(file_path: &str, t: &TypeDefinition) -> bool {
    match &t.path_options.path_ext_lower {
        None => true,
        Some(ext) => {
            if ext.is_empty() {
                return true;
            }
            let path_lower = lookup_path(file_path);
            let basename = path_lower.rsplit('/').next().unwrap_or(&path_lower);
            basename
                .rsplit('.')
                .next()
                .is_some_and(|e| e == ext.as_str())
        }
    }
}

pub(crate) fn find_type_and_rules_for_file<'a>(
    name: &str,
    file_path: &str,
    ruleset: &'a RuleSet,
) -> Option<(&'a TypeDefinition, &'a [(RuleType, Options)])> {
    let by_name = find_type_and_rules(name, ruleset);
    if let Some((td, _)) = by_name {
        if type_extension_matches(file_path, td) {
            return by_name;
        }
        let file_path_lower = file_path.to_lowercase();
        if let Some(t) = find_type_by_path_and_key(&file_path_lower, Some(name), ruleset) {
            return Some((t, find_rules_by_name(&t.name, ruleset)));
        }
        return None;
    }
    by_name
}

pub(crate) fn find_rules_by_name<'a>(
    name: &str,
    ruleset: &'a RuleSet,
) -> &'a [(RuleType, Options)] {
    if let Some(&i) = ruleset.type_rules_idx().get(name)
        && let RootRule::TypeRule(_, (rule, _)) = &ruleset.root_rules[i]
        && let RuleType::NodeRule { rules, .. } = rule
    {
        return rules.as_ref();
    }
    &[]
}

pub(crate) fn find_type_rule_opts<'a>(name: &str, ruleset: &'a RuleSet) -> Option<&'a Options> {
    let i = *ruleset.type_rules_idx().get(name)?;
    if let RootRule::TypeRule(_, (_, opts)) = &ruleset.root_rules[i] {
        Some(opts)
    } else {
        None
    }
}

pub(crate) struct PathCandidate<'a> {
    pub type_def: &'a TypeDefinition,
    pub base_weight: usize,
}

pub(crate) fn path_candidates_for_file<'a>(
    file_path_lower: &str,
    ruleset: &'a RuleSet,
) -> Vec<PathCandidate<'a>> {
    // Logical paths are `/`-separated. A backslash path (Windows, if a caller
    let normalized = file_path_lower.replace('\\', "/");
    let file_path_lower = normalized.as_str();
    let basename = file_path_lower
        .rsplit('/')
        .next()
        .unwrap_or(file_path_lower);
    let dir = file_path_lower
        .strip_suffix(basename)
        .unwrap_or(file_path_lower)
        .trim_end_matches('/');
    let ext = basename.rsplit('.').next();

    let mut out = Vec::new();
    for t in &ruleset.types {
        if let Some(pf) = &t.path_options.path_file_lower
            && basename != pf.as_str()
        {
            continue;
        }
        if let Some(req_ext) = &t.path_options.path_ext_lower
            && ext.is_none_or(|e| e != req_ext.as_str())
        {
            continue;
        }
        let path_file_bonus = if t.path_options.path_file.is_some() {
            1000
        } else {
            0
        };
        let mut best_weight = 0usize;
        for p_lower in &t.path_options.paths_lower {
            if dir_matches_pattern(dir, p_lower, t.path_options.path_strict) {
                let w = p_lower.len() + path_file_bonus;
                if w > best_weight {
                    best_weight = w;
                }
            }
        }
        if best_weight > 0 {
            out.push(PathCandidate {
                type_def: t,
                base_weight: best_weight,
            });
        }
    }
    out
}

pub(crate) fn find_type_from_candidates<'a>(
    candidates: &[PathCandidate<'a>],
    root_key: Option<&str>,
) -> Option<&'a TypeDefinition> {
    let mut best: Option<&TypeDefinition> = None;
    let mut best_len = 0usize;

    for c in candidates {
        let t = c.type_def;
        let tkf_bonus = match (root_key, t.skip_root_key.is_empty(), &t.type_key_filter) {
            (Some(rk), true, Some((keys, negate))) => {
                let hit = keys.iter().any(|k| k.eq_ignore_ascii_case(rk));
                if hit != *negate {
                    5_000
                } else {
                    continue; // filter excludes this key: the type does not apply
                }
            }
            _ => 0,
        };
        let skip_key_bonus = match root_key {
            Some(rk) if should_skip_root_key(rk, t) => 10_000,
            _ => 0,
        };
        let weight = c.base_weight + skip_key_bonus + tkf_bonus;
        if weight > best_len {
            best = Some(t);
            best_len = weight;
        }
    }
    best
}

pub(crate) fn find_type_by_path_and_key<'a>(
    file_path_lower: &str,
    root_key: Option<&str>,
    ruleset: &'a RuleSet,
) -> Option<&'a TypeDefinition> {
    let candidates = path_candidates_for_file(file_path_lower, ruleset);
    find_type_from_candidates(&candidates, root_key)
}

pub(crate) fn grandchild_candidates_for_wrapper<'a>(
    path_candidates: &[PathCandidate<'a>],
    wrapper_root_key: &str,
) -> Vec<&'a TypeDefinition> {
    path_candidates
        .iter()
        .filter_map(|candidate| {
            should_skip_root_key(wrapper_root_key, candidate.type_def).then_some(candidate.type_def)
        })
        .collect()
}

pub(crate) fn find_grandchild_type<'a>(
    candidates: &[&'a TypeDefinition],
    gc_key: &str,
) -> Option<&'a TypeDefinition> {
    let mut generic: Option<&TypeDefinition> = None;
    for &t in candidates {
        match &t.type_key_filter {
            Some((keys, negative)) => {
                let in_list = keys.iter().any(|k| k.eq_ignore_ascii_case(gc_key));
                if in_list != *negative {
                    return Some(t);
                }
            }
            None => {
                if generic.is_none() {
                    generic = Some(t);
                }
            }
        }
    }
    generic
}

pub(crate) fn refine_grandchild_type<'a>(
    candidates: &[&'a TypeDefinition],
    gc_key: &str,
    type_def: &'a TypeDefinition,
    inner_rules: &'a [(RuleType, Options)],
    ruleset: &'a RuleSet,
) -> Option<(&'a TypeDefinition, &'a [(RuleType, Options)])> {
    match find_grandchild_type(candidates, gc_key) {
        Some(t) => {
            let r = find_rules_by_name(&t.name, ruleset);
            if !type_has_content(t, r) {
                return None;
            }
            Some((t, r))
        }
        None => {
            if let Some((keys, negate)) = &type_def.type_key_filter {
                let hit = keys.iter().any(|k| k.eq_ignore_ascii_case(gc_key));
                if hit == *negate {
                    return None;
                }
            }
            Some((type_def, inner_rules))
        }
    }
}

pub(crate) fn type_has_content(td: &TypeDefinition, rules: &[(RuleType, Options)]) -> bool {
    !rules.is_empty() || td.subtypes.iter().any(|st| !st.rules.is_empty())
}

pub(crate) fn best_content_type<'a>(
    candidates: &[PathCandidate<'a>],
    root_key: &str,
    ruleset: &'a RuleSet,
) -> Option<&'a TypeDefinition> {
    let filtered: Vec<PathCandidate<'a>> = candidates
        .iter()
        .filter(|c| type_has_content(c.type_def, find_rules_by_name(&c.type_def.name, ruleset)))
        .map(|c| PathCandidate {
            type_def: c.type_def,
            base_weight: c.base_weight,
        })
        .collect();
    find_type_from_candidates(&filtered, Some(root_key))
}

pub(crate) enum ResolvedType<'a> {
    Entity {
        type_def: &'a TypeDefinition,
        inner_rules: &'a [(RuleType, Options)],
    },
    Wrapper {
        type_def: &'a TypeDefinition,
        inner_rules: &'a [(RuleType, Options)],
        skip_tail: &'a [SkipRootKey],
    },
    None,
}

pub(crate) struct DispatchInput<'a> {
    pub ruleset: &'a RuleSet,
    pub file_path: &'a str,
    pub path_candidates: &'a [PathCandidate<'a>],
    pub allow_content_fallback: bool,
}

pub(crate) fn resolve_root_child<'a>(
    input: &DispatchInput<'a>,
    root_key: &str,
) -> ResolvedType<'a> {
    let ruleset = input.ruleset;

    if let Some((td, inner_rules)) =
        find_type_and_rules_for_file(root_key, input.file_path, ruleset)
    {
        let skips = should_skip_root_key(root_key, td);
        let skip_gate_ok = td.skip_root_key.is_empty() || skips;
        if type_has_content(td, inner_rules) && skip_gate_ok {
            return if skips {
                ResolvedType::Wrapper {
                    type_def: td,
                    inner_rules,
                    skip_tail: skip_root_key_tail(td),
                }
            } else {
                ResolvedType::Entity {
                    type_def: td,
                    inner_rules,
                }
            };
        }
    }

    let Some(mut td) = find_type_from_candidates(input.path_candidates, Some(root_key)) else {
        return ResolvedType::None;
    };
    let mut inner_rules = find_rules_by_name(&td.name, ruleset);
    if !type_has_content(td, inner_rules) {
        if !input.allow_content_fallback {
            return ResolvedType::None;
        }
        let Some(better) = best_content_type(input.path_candidates, root_key, ruleset) else {
            return ResolvedType::None;
        };
        td = better;
        inner_rules = find_rules_by_name(&td.name, ruleset);
    }
    if should_skip_root_key(root_key, td) {
        return ResolvedType::Wrapper {
            type_def: td,
            inner_rules,
            skip_tail: skip_root_key_tail(td),
        };
    }
    if !td.skip_root_key.is_empty() {
        return ResolvedType::None;
    }
    ResolvedType::Entity {
        type_def: td,
        inner_rules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    #[test]
    fn path_candidates_handle_backslash_paths() {
        let table = StringTable::new();
        let cwt = "types = { type[foo] = { path = \"common/foo\" } }";
        let parsed = parse_string(cwt, &table);
        let rs = ast_to_ruleset(&parsed, &table);

        assert!(
            !path_candidates_for_file("common/foo/x.txt", &rs).is_empty(),
            "forward-slash path should resolve type foo"
        );
        // A Windows backslash path must resolve the same type, not silently
        assert!(
            !path_candidates_for_file("common\\foo\\x.txt", &rs).is_empty(),
            "backslash path should resolve type foo too"
        );
    }

    #[test]
    fn grandchild_candidates_handle_backslash_paths() {
        let table = StringTable::new();
        let cwt = "types = { type[foo] = { path = \"common/foo\" skip_root_key = wrapper } }";
        let parsed = parse_string(cwt, &table);
        let rs = ast_to_ruleset(&parsed, &table);
        for path in ["common/foo/x.txt", "common\\foo\\x.txt"] {
            let candidates = path_candidates_for_file(path, &rs);
            let grandchild_candidates = grandchild_candidates_for_wrapper(&candidates, "wrapper");
            assert_eq!(
                grandchild_candidates.len(),
                1,
                "{path} should resolve the wrapper type"
            );
        }
    }
}
