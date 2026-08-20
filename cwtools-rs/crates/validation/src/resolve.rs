//! Type/path resolution: pick which `TypeDefinition` (and its rules) a root key
//! or file path resolves to.

use cwtools_index::dir_matches_pattern;
use cwtools_rules::rules_types::*;

/// Lowercased, forward-slashed copy of `file_path` for type-path lookup. Logical
/// paths are `/`-separated, so a Windows backslash path would make `rsplit('/')`
/// treat the whole path as the basename and match no type.
fn lookup_path(file_path: &str) -> String {
    file_path.to_lowercase().replace('\\', "/")
}

/// Check if `key` is a level-1 skip_root_key wrapper for this type.
///
/// Only the FIRST entry of the stack is tested: each element in
/// `skip_root_key` is a distinct nesting level (block form `{ A B }`
/// produces `[SpecificKey("A"), AnyKey]` — the first entry is the root
/// wrapper, the rest are deeper levels).  Using `.any()` over the whole
/// Vec would incorrectly treat every key as a wrapper for types that have
/// `[SpecificKey("ideas"), AnyKey]`.
pub(crate) fn should_skip_root_key(key: &str, type_def: &TypeDefinition) -> bool {
    type_def
        .skip_root_key
        .first()
        .is_some_and(|sk| cwtools_index::skip_root_key_matches(sk, key))
}

/// Return the remaining skip levels after the first one has been consumed
/// (i.e. the tail of the skip stack).  Empty when there is at most one level.
pub(crate) fn skip_root_key_tail(
    type_def: &TypeDefinition,
) -> &[cwtools_rules::rules_types::SkipRootKey] {
    type_def.skip_root_key.get(1..).unwrap_or(&[])
}

/// Look up both the TypeDefinition and the actual validation rules for a given type name.
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

/// True if `t` has no `path_extension` constraint, or `file_path` satisfies it.
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

/// Resolve a top-level entity's type by its root key, honoring `path_extension`.
///
/// The fast path matches the key against type NAMES (`find_type_and_rules`).
/// But several types can share a `## type_key_filter` + path and differ only by
/// `path_extension`: `music` is the `.txt` song lists while `musicasset` is the
/// `.asset` definitions, both keyed `music`. The by-name lookup always returns
/// `music`, so `.asset` bodies (name/file/volume) wrongly flag as unexpected and
/// `song` reads as missing. When the by-name type is gated to an extension the
/// file lacks, defer to the path/extension-aware resolver instead.
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
        // Extension mismatch: try the path-aware lookup first.
        let file_path_lower = file_path.to_lowercase();
        if let Some(t) = find_type_by_path_and_key(&file_path_lower, Some(name), ruleset) {
            return Some((t, find_rules_by_name(&t.name, ruleset)));
        }
        // No path match either: the by-name hit was for a different extension;
        // returning it would validate the wrong rule body.
        return None;
    }
    by_name
}

/// Find the actual validation rules for a type by looking in root_rules.
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

/// The `Options` of a type's root rule (carries `## replace_scope` / `## push_scope`
/// that seed the instance's scope, e.g. the state-history `state` object).
pub(crate) fn find_type_rule_opts<'a>(name: &str, ruleset: &'a RuleSet) -> Option<&'a Options> {
    let i = *ruleset.type_rules_idx().get(name)?;
    if let RootRule::TypeRule(_, (_, opts)) = &ruleset.root_rules[i] {
        Some(opts)
    } else {
        None
    }
}

/// A path-matched type with its base weight (path length + path_file bonus).
/// The key-dependent bonuses (`skip_key_bonus`, `tkf_bonus`) are added later
/// by [`find_type_from_candidates`] so that path filtering is done once per
/// file while key scoring is done once per root child.
pub(crate) struct PathCandidate<'a> {
    pub type_def: &'a TypeDefinition,
    /// Largest `p_lower.len() + path_file_bonus` over all matching paths.
    pub base_weight: usize,
}

/// Pre-filter types to those whose path options match `file_path_lower`.
/// Returns one entry per matching type (the highest base weight across all
/// matching paths_lower entries).  Call once per file, then reuse the slice
/// across all root children via [`find_type_from_candidates`].
pub(crate) fn path_candidates_for_file<'a>(
    file_path_lower: &str,
    ruleset: &'a RuleSet,
) -> Vec<PathCandidate<'a>> {
    // Logical paths are `/`-separated. A backslash path (Windows, if a caller
    // didn't normalize) would make `rsplit('/')` treat the whole path as the
    // basename and match no type — silently breaking hover/goto/validation
    // (e.g. trigger doc tooltips) for that file.
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

/// Pick the best-matching type from path-prefiltered candidates, applying
/// key-dependent bonuses (`skip_root_key` and `type_key_filter`).
pub(crate) fn find_type_from_candidates<'a>(
    candidates: &[PathCandidate<'a>],
    root_key: Option<&str>,
) -> Option<&'a TypeDefinition> {
    let mut best: Option<&TypeDefinition> = None;
    let mut best_len = 0usize;

    for c in candidates {
        let t = c.type_def;
        // `## type_key_filter` gates a NON-wrapper type to nodes whose own key
        // satisfies the filter. A matching filter also earns a bonus so the
        // filtered type beats an unfiltered one on the same path.
        // (For skip_root_key wrappers the filter applies to GRANDCHILDREN,
        // handled in validate_wrapper_grandchildren, so it is not gated here.)
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

/// Find a type whose path_options match the given file path, also considering
/// the root key of the child being validated. Types whose `skip_root_key`
/// matches `root_key` are given a large bonus, so they beat a longer-path type
/// that has no skip_root_key and would otherwise win on path length alone.
///
/// `file_path_lower` must already be lowercased (ASCII) by the caller so
/// that per-child calls in a hot loop share a single allocation.
///
/// For hot loops processing many root children of the same file, prefer
/// calling [`path_candidates_for_file`] once and [`find_type_from_candidates`]
/// per child.
///
/// This mirrors F# behaviour where `type[pdxmesh] { skip_root_key = objectTypes }`
/// correctly wins over `type[light] { path = "gfx/entities" }` for
/// a `objectTypes = { pdxmesh = { ... } }` root node in a `.gfx` file.
pub(crate) fn find_type_by_path_and_key<'a>(
    file_path_lower: &str,
    root_key: Option<&str>,
    ruleset: &'a RuleSet,
) -> Option<&'a TypeDefinition> {
    let candidates = path_candidates_for_file(file_path_lower, ruleset);
    find_type_from_candidates(&candidates, root_key)
}

/// Path-matched types whose first skip-root level selects `wrapper_root_key`.
/// Compute this once for a wrapper, then reuse it for every grandchild instead of
/// rechecking every ruleset type and normalizing the same path per grandchild.
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

/// Resolve which type a `skip_root_key` wrapper's grandchild belongs to, by the
/// grandchild's own key. Several types can share a path AND `skip_root_key`
/// (e.g. `pdxmesh`, `pdxparticle`, `entity` all sit under `objectTypes` in `.gfx`
/// files); `## type_key_filter` is what disambiguates them. Prefer a candidate
/// whose filter selects `gc_key`; otherwise fall back to a wrapper type that has
/// no filter. Returns `None` when nothing fits, in which case the caller keeps
/// the type that won the path lookup (so single-type wrappers are unaffected).
pub(crate) fn find_grandchild_type<'a>(
    candidates: &[&'a TypeDefinition],
    gc_key: &str,
) -> Option<&'a TypeDefinition> {
    let mut generic: Option<&TypeDefinition> = None;
    for &t in candidates {
        match &t.type_key_filter {
            Some((keys, negative)) => {
                let in_list = keys.iter().any(|k| k.eq_ignore_ascii_case(gc_key));
                // `negative` = `## type_key_filter <> ...` (exclude); otherwise include.
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

/// Refine a `skip_root_key` wrapper's resolved type for one grandchild, shared
/// by the validator's `validate_wrapper_grandchildren` and the navigator's
/// `descend_wrapper`. Prefers [`find_grandchild_type`]'s `## type_key_filter`
/// match; when that finds nothing, falls back to the wrapper's already-resolved
/// `type_def`/`inner_rules`, but only when `type_def`'s own `type_key_filter`
/// (if any) actually applies to `gc_key`. `None` means the caller should skip
/// this grandchild (continue the loop / stop descending) rather than validate
/// or enter it.
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
            // Resolved to an index-only type (no rule body): its fields
            // are not content-validated, so don't flag them.
            if !type_has_content(t, r) {
                return None;
            }
            Some((t, r))
        }
        // No better match. Only fall back to the wrapper's resolved type
        // when that type actually applies to THIS grandchild's key. A type
        // with `## type_key_filter = containerWindowType` must not validate
        // a sibling `scrollbarType`/`guiButtonType` (top-level widgets under
        // `guiTypes`) against the containerWindowType schema — F# excludes
        // them via the filter and leaves them unvalidated. Without this the
        // widgets' own fields (slider/track/priority/...) flag as CW201.
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

/// Whether a type has its own validation rules, rather than being an index-only
/// `type[x]` declaration (path/name_field with no rule body) that exists solely
/// to register instances.
pub(crate) fn type_has_content(td: &TypeDefinition, rules: &[(RuleType, Options)]) -> bool {
    !rules.is_empty() || td.subtypes.iter().any(|st| !st.rules.is_empty())
}

/// The best path candidate that carries an actual rule body, ignoring index-only
/// types. Used as a navigation fallback when the top path+key match is a
/// rule-less skip wrapper whose content is validated by a sibling base type
/// (e.g. the `on_action` base owns the rules that `on_weekly`/`on_daily`
/// instances under `on_actions = { }` are checked against).
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

/// What a root key resolves to, shared by the validator (`validate_prepared`)
/// and the editor navigator (`rules_at_pos`). The two callers must agree on
/// which `TypeDefinition` owns a node — see [`resolve_root_child`].
pub(crate) enum ResolvedType<'a> {
    /// Validate / enter the node itself as an instance of `type_def`.
    Entity {
        type_def: &'a TypeDefinition,
        inner_rules: &'a [(RuleType, Options)],
    },
    /// The node is a `skip_root_key` wrapper; its children are the instances.
    /// Descend through it rather than treating the node as content.
    Wrapper {
        type_def: &'a TypeDefinition,
        inner_rules: &'a [(RuleType, Options)],
        skip_tail: &'a [SkipRootKey],
    },
    /// No type applies, or the match is index-only (no rule body) — skip.
    None,
}

/// Inputs shared across per-root-child resolution. `path_candidates` is computed
/// once per file (it depends only on the path, not the key) and reused for every
/// child.
pub(crate) struct DispatchInput<'a> {
    pub ruleset: &'a RuleSet,
    pub file_path: &'a str,
    pub path_candidates: &'a [PathCandidate<'a>],
    /// When true (navigation), a path match that is an index-only skip wrapper
    /// falls back to the best content-bearing sibling type so the cursor can
    /// still descend (e.g. `on_actions` -> `on_action`). The validator passes
    /// false: it skips such roots rather than content-validating them.
    pub allow_content_fallback: bool,
}

/// Resolve which type owns a root node, given its key. The dispatch tree that
/// `validate_prepared` and `rules_at_pos` previously each carried a copy of:
/// exact root-key match first, then path-based fallback. The only behavioral
/// difference between the two callers is `allow_content_fallback` (see
/// [`DispatchInput`]); keeping one copy here removes the drift risk that the two
/// trees fall out of step.
pub(crate) fn resolve_root_child<'a>(
    input: &DispatchInput<'a>,
    root_key: &str,
) -> ResolvedType<'a> {
    let ruleset = input.ruleset;

    // 1. Exact root-key match (e.g. `ai_strategy_plan = { ... }`).
    if let Some((td, inner_rules)) =
        find_type_and_rules_for_file(root_key, input.file_path, ruleset)
    {
        // A type gated by skip_root_key only applies when the matched key is one
        // of its skip keys (the key IS the wrapper). If it declares skip_root_key
        // but this key matches none, the name-match is spurious — fall through to
        // path matching so another type whose skip_root_key IS this key can own it.
        let skips = should_skip_root_key(root_key, td);
        let skip_gate_ok = td.skip_root_key.is_empty() || skips;
        // Only content-validate when the matched type actually has rules; a
        // type[x] declared solely for instance indexing must not flag its fields.
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
        // matched by name but instance-only / skip-gate mismatch: fall through.
    }

    // 2. Path-based fallback. Re-query with the actual root key so a type with a
    // matching skip_root_key can beat a longer-path type that lacks one.
    let Some(mut td) = find_type_from_candidates(input.path_candidates, Some(root_key)) else {
        return ResolvedType::None;
    };
    let mut inner_rules = find_rules_by_name(&td.name, ruleset);
    if !type_has_content(td, inner_rules) {
        // Index-only `type[x]` (path/name_field, no rule body): its instances are
        // not content-validated. The validator skips; navigation falls back to a
        // content-bearing sibling type so the cursor can still descend.
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
        // skip_root_key = ...: the node is a WRAPPER — descend into its children.
        return ResolvedType::Wrapper {
            type_def: td,
            inner_rules,
            skip_tail: skip_root_key_tail(td),
        };
    }
    if !td.skip_root_key.is_empty() {
        // skip_root_key gate: the type declares skip keys but this root matches
        // none, so the type does not apply to this root.
        return ResolvedType::None;
    }
    // No skip_root_key — the root node itself is the instance.
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

        // Forward-slash resolves the type.
        assert!(
            !path_candidates_for_file("common/foo/x.txt", &rs).is_empty(),
            "forward-slash path should resolve type foo"
        );
        // A Windows backslash path must resolve the same type, not silently
        // fail (which would break hover/goto/validation for the file).
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
