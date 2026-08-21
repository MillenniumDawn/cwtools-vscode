use cwtools_parser::ast::Value;
use cwtools_string_table::string_table::{StringId, StringTable};
use std::collections::HashMap;

pub mod dynamic_values;
pub mod vanilla_cache;

mod collect;
mod path_match;
mod type_index;
mod variables;

pub use collect::{
    CollectedTypeInstances, InstanceNode, SubtypeCollector, collect_scripted_gui_callback_names,
    collect_scripted_loc_names, collect_type_instances, collect_type_instances_with_subtypes,
    for_each_instance_node, hash_instance_exports, index_discovered_files, mix_export_symbol,
    skip_root_key_matches,
};
pub use path_match::{
    NormalizedPath, check_path_dir, check_path_dir_norm, dir_matches_pattern, path_contains_segment,
};
pub use type_index::{
    FileIndex, ScriptedGuiIndex, ScriptedLocIndex, TypeIndex, TypeInstance, VarIndex,
};
pub use variables::{
    DefinedVariable, collect_defined_variables_from_rules, collect_set_variable_defs,
    collect_set_variable_names, variable_defining_effects,
};

/// Strip one layer of surrounding double-quotes, if present. A lone `"` (no
/// matching pair) is left untouched, since `strip_suffix` finds no closing
/// quote in the already-stripped remainder.
pub(crate) fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s)
}

/// Decrement a refcount entry in `map`, removing it when the count reaches 0.
/// Does nothing if the key is absent. Shared by every refcounted name/value
/// index so re-indexing a file drops only its last contribution.
pub(crate) fn dec_ref<K, Q, S>(map: &mut HashMap<K, usize, S>, key: &Q)
where
    K: std::hash::Hash + Eq + std::borrow::Borrow<Q>,
    Q: std::hash::Hash + Eq + ?Sized,
    S: std::hash::BuildHasher,
{
    if let Some(count) = map.get_mut(key) {
        *count -= 1;
        if *count == 0 {
            map.remove(key);
        }
    }
}

/// Resolve a `StringId` to its owned text, returning `""` when interning lost
/// it. For a `StringId` taken from a parsed AST this should never miss, so a
/// debug build trips an assertion to surface the bug; release behaves exactly
/// like the prior `get_string(id).unwrap_or_default()`.
#[inline]
pub(crate) fn get_string_or_empty(table: &StringTable, id: StringId) -> String {
    match table.get_string(id) {
        Some(s) => s,
        None => {
            debug_assert!(
                false,
                "get_string returned None for a StringId from a parsed AST"
            );
            String::new()
        }
    }
}

/// Extract a plain string from a leaf value.
pub fn leaf_value_string(value: &Value, table: &StringTable) -> String {
    match value {
        Value::String(t) | Value::QString(t) => get_string_or_empty(table, t.normal),
        Value::Float(f) => f.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Clause(_) => String::new(),
    }
}

/// Call `f` with the leaf value as a `&str`. String-typed values borrow straight
/// from the string table (no allocation); numeric/bool values are formatted into
/// a scratch buffer that is owned by this call. Clauses yield `""`. Internal
/// allocation-free counterpart to [`leaf_value_string`] for the index collectors.
pub(crate) fn with_leaf_value_str<R>(
    value: &Value,
    table: &StringTable,
    f: impl FnOnce(&str) -> R,
) -> R {
    match value {
        Value::String(t) | Value::QString(t) => {
            // `Some` already invoked `f`; a `None` (out-of-range id) maps to `""`,
            // matching `get_string(..).unwrap_or_default()`.
            let mut f = Some(f);
            match table.with_string(t.normal, |s| (f.take().unwrap())(s)) {
                Some(r) => r,
                None => (f.take().unwrap())(""),
            }
        }
        Value::Float(n) => f(&n.to_string()),
        Value::Int(i) => f(&i.to_string()),
        Value::Bool(b) => f(&b.to_string()),
        Value::Clause(_) => f(""),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SourceLocation {
    pub line: u32,
    pub col: u16,
    /// End position `(line, col)` of the construct. For a keyed clause it is the
    /// spot just past the closing brace (the parser's `SourceRange.end`), so a
    /// definition's full extent is `(line, col)..end`. Cleanup features (rename a
    /// definition, delete an unreferenced instance) need the whole span, not just
    /// the start. Synthesized locations with no real range use `end == (line,
    /// col)`. Non-optional because the range is always in hand at collection.
    pub end: (u32, u16),
}

/// Whether an index key is a subtype-qualified membership key (`"type.subtype"`,
/// produced by [`SubtypeCollector`]) rather than a plain `type` key. The `.`
/// separator is the discriminator: CWT `type[...]` names are bare identifiers and
/// never contain a dot, so any key with one is a subtype membership entry. Such
/// keys feed `contains` (so `<type.subtype>` references resolve) but are kept out
/// of `name_counts` / document-symbol output. This invariant is relied on in
/// `merge`, `remove_file`, `instances_in_file`, and the validator's CW500 check,
/// so it lives here in one place.
pub fn is_subtype_key(type_name: &str) -> bool {
    type_name.contains('.')
}

/// A saved event target and where it was defined.
#[derive(Debug, Clone)]
pub struct SavedEventTarget {
    pub name: String,
    pub location: SourceLocation,
    /// true = global (save_global_event_target_as)
    pub is_global: bool,
}
