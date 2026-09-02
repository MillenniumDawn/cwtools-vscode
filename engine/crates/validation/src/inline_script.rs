use std::collections::HashMap;
use std::fmt;

use cwtools_parser::ast::{Arena, Child, Comment, Leaf, LeafValue, ParsedFile, Value};
use cwtools_string_table::string_table::StringTable;

use crate::common::leaf_value_to_string;

const SCRIPT_DIR: &str = "common/inline_scripts/";

const MAX_DEPTH: usize = 5;

#[derive(Default)]
pub struct InlineScripts {
    scripts: HashMap<String, InlineScript>,
}

struct InlineScript {
    logical_path: String,
    ast: ParsedFile,
}

impl InlineScripts {
    pub fn insert(&mut self, logical_path: &str, ast: ParsedFile) -> Option<String> {
        let name = script_name(logical_path)?;
        self.scripts.insert(
            name.clone(),
            InlineScript {
                logical_path: logical_path.to_string(),
                ast,
            },
        );
        Some(name)
    }

    pub fn remove(&mut self, logical_path: &str) -> Option<String> {
        let name = script_name(logical_path)?;
        self.scripts.remove(&name).map(|_| name)
    }

    pub fn is_script_path(logical_path: &str) -> bool {
        script_name(logical_path).is_some()
    }

    pub fn len(&self) -> usize {
        self.scripts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }

    fn get(&self, name: &str) -> Option<&InlineScript> {
        self.scripts.get(&normalize(name))
    }
}

fn script_name(logical_path: &str) -> Option<String> {
    let normalized = normalize(logical_path);
    let rest = normalized.split_once(SCRIPT_DIR).map(|(_, rest)| rest)?;
    let stem = rest.strip_suffix(".txt").unwrap_or(rest);
    (!stem.is_empty()).then(|| stem.to_string())
}

fn normalize(s: &str) -> String {
    s.replace('\\', "/").to_ascii_lowercase()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExpandError {
    NotAClause,
    MissingScript,
    Unknown(String),
    Cycle(String),
    TooDeep(String),
    BudgetExceeded,
}

impl fmt::Display for ExpandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAClause => write!(
                f,
                "An inline_script call must be a {{ script = ... }} block"
            ),
            Self::MissingScript => write!(f, "This inline_script call has no 'script' field"),
            Self::Unknown(name) => write!(
                f,
                "Inline script '{name}' does not exist in {}",
                SCRIPT_DIR.trim_end_matches('/')
            ),
            Self::Cycle(chain) => write!(f, "Inline script '{chain}' calls itself"),
            Self::TooDeep(name) => write!(
                f,
                "Inline script '{name}' nests more than {MAX_DEPTH} levels deep"
            ),
            Self::BudgetExceeded => write!(f, "Inline script expansion budget exceeded"),
        }
    }
}

pub(crate) struct Expanded {
    pub(crate) name: String,
    pub(crate) logical_path: String,
    pub(crate) ast: ParsedFile,
}

pub(crate) fn expand(
    call: &Leaf,
    arena: &Arena,
    table: &StringTable,
    scripts: &InlineScripts,
    stack: &[String],
) -> Result<Expanded, ExpandError> {
    let Value::Clause(call_children) = &call.value else {
        return Err(ExpandError::NotAClause);
    };

    let mut name = String::new();
    let mut args: HashMap<String, String> = HashMap::new();
    for child in call_children {
        let Child::Leaf(idx) = child else {
            continue;
        };
        let arg = &arena.leaves[*idx as usize];
        let key = table.get_string(arg.key.normal).unwrap_or_default();
        let value = leaf_value_to_string(&arg.value, table);
        if key.eq_ignore_ascii_case("script") {
            name = value;
        } else {
            args.insert(normalize(&key), value);
        }
    }

    if name.is_empty() {
        return Err(ExpandError::MissingScript);
    }
    let lookup = normalize(&name);
    if stack.contains(&lookup) {
        return Err(ExpandError::Cycle(name));
    }
    if stack.len() >= MAX_DEPTH {
        return Err(ExpandError::TooDeep(name));
    }
    let Some(script) = scripts.get(&lookup) else {
        return Err(ExpandError::Unknown(name));
    };

    let mut out = Arena::new();
    let root_children = clone_children(
        &script.ast.root_children,
        &script.ast.arena,
        table,
        &args,
        &mut out,
    );
    Ok(Expanded {
        name: lookup,
        logical_path: script.logical_path.clone(),
        ast: ParsedFile {
            arena: out,
            root_children,
            errors: Vec::new(),
        },
    })
}

fn clone_children(
    children: &[Child],
    src: &Arena,
    table: &StringTable,
    args: &HashMap<String, String>,
    out: &mut Arena,
) -> Vec<Child> {
    children
        .iter()
        .map(|child| match child {
            Child::Leaf(idx) => {
                let leaf = &src.leaves[*idx as usize];
                let key = substitute_tokens(leaf.key, table, args);
                let value = clone_value(&leaf.value, src, table, args, out);
                Child::Leaf(out.push_leaf(Leaf {
                    key,
                    value,
                    op: leaf.op,
                    pos: leaf.pos,
                    value_pos: leaf.value_pos,
                }))
            }
            Child::LeafValue(idx) => {
                let lv = &src.leaf_values[*idx as usize];
                let value = clone_value(&lv.value, src, table, args, out);
                Child::LeafValue(out.push_leaf_value(LeafValue { value, pos: lv.pos }))
            }
            Child::Comment(idx) => {
                let comment = &src.comments[*idx as usize];
                Child::Comment(out.push_comment(Comment {
                    text: comment.text.clone(),
                    pos: comment.pos,
                }))
            }
        })
        .collect()
}

fn clone_value(
    value: &Value,
    src: &Arena,
    table: &StringTable,
    args: &HashMap<String, String>,
    out: &mut Arena,
) -> Value {
    match value {
        Value::String(t) => Value::String(substitute_tokens(*t, table, args)),
        Value::QString(t) => Value::QString(substitute_tokens(*t, table, args)),
        Value::Clause(children) => Value::Clause(clone_children(children, src, table, args, out)),
        Value::Float(f) => Value::Float(*f),
        Value::Int(i) => Value::Int(*i),
        Value::Bool(b) => Value::Bool(*b),
    }
}

fn substitute_tokens(
    tokens: cwtools_string_table::string_table::StringTokens,
    table: &StringTable,
    args: &HashMap<String, String>,
) -> cwtools_string_table::string_table::StringTokens {
    let substituted = table
        .with_string(tokens.normal, |s| substitute(s, args))
        .flatten();
    match substituted {
        Some(text) => table.intern(&text),
        None => tokens,
    }
}

fn substitute(text: &str, args: &HashMap<String, String>) -> Option<String> {
    if !text.contains('$') {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut replaced = false;
    while let Some(open) = rest.find('$') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('$') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let name = &after[..close];
        match args.get(&normalize(name)) {
            Some(value) => {
                out.push_str(value);
                replaced = true;
            }
            None => {
                out.push('$');
                out.push_str(name);
                out.push('$');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    replaced.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::parser::parse_string;

    fn args(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (normalize(k), (*v).to_string()))
            .collect()
    }

    fn registry(entries: &[(&str, &str)], table: &StringTable) -> InlineScripts {
        let mut scripts = InlineScripts::default();
        for (path, body) in entries {
            assert!(scripts.insert(path, parse_string(body, table)).is_some());
        }
        scripts
    }

    fn call_leaf(source: &str, table: &StringTable) -> (ParsedFile, u32) {
        let parsed = parse_string(source, table);
        let Child::Leaf(idx) = parsed.root_children[0] else {
            panic!("expected a leaf");
        };
        (parsed, idx)
    }

    #[test]
    fn script_name_is_the_path_below_the_inline_scripts_dir() {
        assert_eq!(
            script_name("common/inline_scripts/foo/bar.txt").as_deref(),
            Some("foo/bar")
        );
        assert_eq!(
            script_name("Common\\Inline_Scripts\\Foo.txt").as_deref(),
            Some("foo")
        );
        assert_eq!(script_name("common/scripted_effects/foo.txt"), None);
        assert_eq!(script_name("common/inline_scripts/"), None);
    }

    #[test]
    fn substitute_replaces_named_arguments() {
        assert_eq!(
            substitute("add_$WHAT$_bonus", &args(&[("WHAT", "air")])).as_deref(),
            Some("add_air_bonus")
        );
        assert_eq!(
            substitute("$what$", &args(&[("WHAT", "air")])).as_deref(),
            Some("air")
        );
    }

    #[test]
    fn substitute_leaves_untouched_text_alone() {
        assert_eq!(substitute("plain_key", &args(&[("A", "b")])), None);
        assert_eq!(substitute("$MISSING$", &args(&[("A", "b")])), None);
        assert_eq!(substitute("cost_$", &args(&[("A", "b")])), None);
    }

    #[test]
    fn substitute_keeps_unpassed_markers_verbatim() {
        assert_eq!(
            substitute("$A$_$B$", &args(&[("A", "x")])).as_deref(),
            Some("x_$B$")
        );
    }

    #[test]
    fn expand_substitutes_into_keys_and_values() {
        let table = StringTable::new();
        let scripts = registry(
            &[("common/inline_scripts/bonus.txt", "$WHICH$ = $HOW_MUCH$")],
            &table,
        );
        let (parsed, idx) = call_leaf(
            "inline_script = { script = bonus WHICH = air_bonus HOW_MUCH = 5 }",
            &table,
        );
        let leaf = &parsed.arena.leaves[idx as usize];
        let expanded = expand(leaf, &parsed.arena, &table, &scripts, &[]).expect("expands");

        assert_eq!(expanded.logical_path, "common/inline_scripts/bonus.txt");
        let Child::Leaf(body_idx) = expanded.ast.root_children[0] else {
            panic!("expected a leaf");
        };
        let body = &expanded.ast.arena.leaves[body_idx as usize];
        assert_eq!(
            table.get_string(body.key.normal).as_deref(),
            Some("air_bonus")
        );
        assert_eq!(leaf_value_to_string(&body.value, &table), "5");
    }

    #[test]
    fn expand_reports_a_missing_script() {
        let table = StringTable::new();
        let scripts = registry(&[], &table);
        let (parsed, idx) = call_leaf("inline_script = { script = nope }", &table);
        let leaf = &parsed.arena.leaves[idx as usize];
        assert_eq!(
            expand(leaf, &parsed.arena, &table, &scripts, &[]).err(),
            Some(ExpandError::Unknown("nope".to_string()))
        );
    }

    #[test]
    fn expand_reports_a_call_with_no_script_field() {
        let table = StringTable::new();
        let scripts = registry(&[], &table);
        let (parsed, idx) = call_leaf("inline_script = { WHICH = air }", &table);
        let leaf = &parsed.arena.leaves[idx as usize];
        assert_eq!(
            expand(leaf, &parsed.arena, &table, &scripts, &[]).err(),
            Some(ExpandError::MissingScript)
        );
    }

    #[test]
    fn expand_reports_a_scalar_call() {
        let table = StringTable::new();
        let scripts = registry(&[], &table);
        let (parsed, idx) = call_leaf("inline_script = bonus", &table);
        let leaf = &parsed.arena.leaves[idx as usize];
        assert_eq!(
            expand(leaf, &parsed.arena, &table, &scripts, &[]).err(),
            Some(ExpandError::NotAClause)
        );
    }

    #[test]
    fn expand_reports_a_script_already_on_the_stack() {
        let table = StringTable::new();
        let scripts = registry(&[("common/inline_scripts/loop.txt", "a = 1")], &table);
        let (parsed, idx) = call_leaf("inline_script = { script = loop }", &table);
        let leaf = &parsed.arena.leaves[idx as usize];
        let stack = vec!["loop".to_string()];
        assert_eq!(
            expand(leaf, &parsed.arena, &table, &scripts, &stack).err(),
            Some(ExpandError::Cycle("loop".to_string()))
        );
    }

    #[test]
    fn expand_reports_a_chain_past_the_depth_limit() {
        let table = StringTable::new();
        let scripts = registry(&[("common/inline_scripts/deep.txt", "a = 1")], &table);
        let (parsed, idx) = call_leaf("inline_script = { script = deep }", &table);
        let leaf = &parsed.arena.leaves[idx as usize];
        let stack: Vec<String> = (0..MAX_DEPTH).map(|i| format!("s{i}")).collect();
        assert_eq!(
            expand(leaf, &parsed.arena, &table, &scripts, &stack).err(),
            Some(ExpandError::TooDeep("deep".to_string()))
        );
    }
}
