use crate::cache_format::*;
use crate::io::CacheError;
use cwtools_parser::ast::{
    Arena, Child, Comment, Leaf, LeafValue, Operator, ParseError, SourcePos, SourceRange, Value,
};
use cwtools_parser::parser::MAX_CLAUSE_DEPTH;
use cwtools_string_table::string_table::{StringResolver, StringTable, StringTokens};

/// [`MAX_CLAUSE_DEPTH`] levels it descends into, plus the empty clause it leaves
const MAX_CACHED_CLAUSE_DEPTH: u32 = MAX_CLAUSE_DEPTH + 1;

const UNRESOLVED_DEPTH: u32 = u32::MAX;

pub fn arena_to_cached(
    arena: &Arena,
    root_children: &[Child],
    string_table: &StringTable,
) -> CachedFile {
    let mut string_tokens = Vec::new();
    for leaf in &arena.leaves {
        string_tokens.push(leaf.key);
        collect_value_tokens(&leaf.value, &mut string_tokens);
    }
    for leaf_value in &arena.leaf_values {
        collect_value_tokens(&leaf_value.value, &mut string_tokens);
    }

    let resolved_strings = string_table.with_read(|table| {
        string_tokens
            .into_iter()
            .map(|token| string_token_to_owned(&token, &table))
            .collect::<Vec<_>>()
    });
    let mut strings = resolved_strings.into_iter();

    let cached = CachedFile {
        root_children: children_to_cached(root_children),
        leaves: arena
            .leaves
            .iter()
            .map(|l| leaf_to_cached(l, &mut strings))
            .collect(),
        leaf_values: arena
            .leaf_values
            .iter()
            .map(|lv| leaf_value_to_cached(lv, &mut strings))
            .collect(),
        comments: arena.comments.iter().map(comment_to_cached).collect(),
    };
    debug_assert!(strings.next().is_none(), "resolved string count mismatch");
    cached
}

pub fn errors_to_cached(errors: &[ParseError]) -> CachedErrors {
    CachedErrors {
        errors: errors
            .iter()
            .map(|ParseError::Pos(line, col, message)| {
                CachedParseError::Pos(*line, *col, message.clone())
            })
            .collect(),
    }
}

pub fn cached_errors_to_parse(cached: CachedErrors) -> Vec<ParseError> {
    cached
        .errors
        .into_iter()
        .map(|CachedParseError::Pos(line, col, message)| ParseError::Pos(line, col, message))
        .collect()
}

/// Rebuild an arena AST from the rkyv archived view, interning strings straight
/// the per-string shared-lock probe via
pub fn archived_to_arena(
    cached: &ArchivedCachedFile,
    string_table: &StringTable,
) -> Result<(Arena, Vec<Child>), CacheError> {
    validate_archived_child_bounds(cached)?;
    validate_archived_clause_depth(cached)?;

    let mut to_intern: Vec<&str> = Vec::new();
    for l in cached.leaves.iter() {
        to_intern.push(l.key.as_str());
        collect_archived_value_strings(&l.value, &mut to_intern);
    }
    for lv in cached.leaf_values.iter() {
        collect_archived_value_strings(&lv.value, &mut to_intern);
    }

    let tokens = string_table.intern_batch(to_intern.iter().copied());
    let mut tokens = tokens.into_iter();

    let mut arena = Arena::new();
    for l in cached.leaves.iter() {
        arena.push_leaf(Leaf {
            key: next_token(&mut tokens)?,
            value: archived_value_to_value(&l.value, &mut tokens)?,
            op: archived_op_to_op(&l.op),
            pos: cached_to_range(
                l.start_line.to_native(),
                l.start_col.to_native(),
                l.end_line.to_native(),
                l.end_col.to_native(),
            ),
            value_pos: cached_to_range(
                l.value_start_line.to_native(),
                l.value_start_col.to_native(),
                l.value_end_line.to_native(),
                l.value_end_col.to_native(),
            ),
        });
    }
    for lv in cached.leaf_values.iter() {
        arena.push_leaf_value(LeafValue {
            value: archived_value_to_value(&lv.value, &mut tokens)?,
            pos: cached_to_range(
                lv.start_line.to_native(),
                lv.start_col.to_native(),
                lv.end_line.to_native(),
                lv.end_col.to_native(),
            ),
        });
    }
    for c in cached.comments.iter() {
        arena.push_comment(Comment {
            text: c.text.as_str().to_string(),
            pos: cached_to_range(
                c.start_line.to_native(),
                c.start_col.to_native(),
                c.end_line.to_native(),
                c.end_col.to_native(),
            ),
        });
    }
    debug_assert!(tokens.next().is_none(), "interned token count mismatch");

    let root = children_from_archived(&cached.root_children);
    Ok((arena, root))
}

fn validate_archived_child_bounds(cached: &ArchivedCachedFile) -> Result<(), CacheError> {
    let leaf_count = cached.leaves.len();
    let leaf_value_count = cached.leaf_values.len();
    let comment_count = cached.comments.len();

    check_archived_child_list(
        &cached.root_children,
        leaf_count,
        leaf_value_count,
        comment_count,
        ClauseOwner::Root,
    )?;
    for (index, leaf) in cached.leaves.iter().enumerate() {
        if let ArchivedCachedValue::Clause(children) = &leaf.value {
            check_archived_child_list(
                children,
                leaf_count,
                leaf_value_count,
                comment_count,
                ClauseOwner::Leaf(index),
            )?;
        }
    }
    for (index, leaf_value) in cached.leaf_values.iter().enumerate() {
        if let ArchivedCachedValue::Clause(children) = &leaf_value.value {
            check_archived_child_list(
                children,
                leaf_count,
                leaf_value_count,
                comment_count,
                ClauseOwner::LeafValue(index),
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ClauseOwner {
    Root,
    Leaf(usize),
    LeafValue(usize),
}

fn check_archived_child_list(
    children: &rkyv::vec::ArchivedVec<ArchivedCachedChild>,
    leaf_count: usize,
    leaf_value_count: usize,
    comment_count: usize,
    owner: ClauseOwner,
) -> Result<(), CacheError> {
    for child in children.iter() {
        let (index, count, owner_index) = match child {
            ArchivedCachedChild::Leaf(i) => (
                i.to_native() as usize,
                leaf_count,
                match owner {
                    ClauseOwner::Leaf(index) => Some(index),
                    ClauseOwner::Root | ClauseOwner::LeafValue(_) => None,
                },
            ),
            ArchivedCachedChild::LeafValue(i) => (
                i.to_native() as usize,
                leaf_value_count,
                match owner {
                    ClauseOwner::LeafValue(index) => Some(index),
                    ClauseOwner::Root | ClauseOwner::Leaf(_) => None,
                },
            ),
            ArchivedCachedChild::Comment(i) => (i.to_native() as usize, comment_count, None),
        };
        if index >= count {
            return Err(cache_rejected("cache child index out of bounds"));
        }
        if owner_index.is_some_and(|slot| index >= slot) {
            return Err(cache_rejected("cache child index out of parse order"));
        }
    }
    Ok(())
}

fn validate_archived_clause_depth(cached: &ArchivedCachedFile) -> Result<(), CacheError> {
    let mut depths = ClauseDepths {
        leaves: vec![UNRESOLVED_DEPTH; cached.leaves.len()],
        leaf_values: vec![UNRESOLVED_DEPTH; cached.leaf_values.len()],
    };
    let mut stack: Vec<DepthFrame> = Vec::new();
    let nodes = (0..cached.leaves.len())
        .map(ArchivedNode::Leaf)
        .chain((0..cached.leaf_values.len()).map(ArchivedNode::LeafValue));

    for node in nodes {
        if depths.get(node) != UNRESOLVED_DEPTH {
            continue;
        }
        stack.push(DepthFrame::new(node));
        while let Some(mut frame) = stack.pop() {
            let children = archived_clause_children(cached, frame.node);
            let Some(child) = children.and_then(|list| list.get(frame.next_child)) else {
                let depth = match children {
                    Some(_) => frame.deepest_child + 1,
                    None => 0,
                };
                if depth > MAX_CACHED_CLAUSE_DEPTH {
                    return Err(cache_rejected("cache clause nesting too deep"));
                }
                depths.set(frame.node, depth);
                if let Some(owner) = stack.last_mut() {
                    owner.deepest_child = owner.deepest_child.max(depth);
                }
                continue;
            };
            frame.next_child += 1;
            let child_node = match child {
                ArchivedCachedChild::Leaf(i) => ArchivedNode::Leaf(i.to_native() as usize),
                ArchivedCachedChild::LeafValue(i) => {
                    ArchivedNode::LeafValue(i.to_native() as usize)
                }
                ArchivedCachedChild::Comment(_) => {
                    stack.push(frame);
                    continue;
                }
            };
            let settled = depths.get(child_node);
            let descend = settled == UNRESOLVED_DEPTH
                && archived_clause_children(cached, child_node).is_some();
            if settled != UNRESOLVED_DEPTH {
                frame.deepest_child = frame.deepest_child.max(settled);
            }
            stack.push(frame);
            if descend {
                stack.push(DepthFrame::new(child_node));
                if stack.len() > MAX_CACHED_CLAUSE_DEPTH as usize {
                    return Err(cache_rejected("cache clause nesting too deep"));
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ArchivedNode {
    Leaf(usize),
    LeafValue(usize),
}

struct ClauseDepths {
    leaves: Vec<u32>,
    leaf_values: Vec<u32>,
}

impl ClauseDepths {
    fn get(&self, node: ArchivedNode) -> u32 {
        match node {
            ArchivedNode::Leaf(i) => self.leaves[i],
            ArchivedNode::LeafValue(i) => self.leaf_values[i],
        }
    }

    fn set(&mut self, node: ArchivedNode, depth: u32) {
        match node {
            ArchivedNode::Leaf(i) => self.leaves[i] = depth,
            ArchivedNode::LeafValue(i) => self.leaf_values[i] = depth,
        }
    }
}

struct DepthFrame {
    node: ArchivedNode,
    next_child: usize,
    deepest_child: u32,
}

impl DepthFrame {
    fn new(node: ArchivedNode) -> Self {
        Self {
            node,
            next_child: 0,
            deepest_child: 0,
        }
    }
}

fn archived_clause_children(
    cached: &ArchivedCachedFile,
    node: ArchivedNode,
) -> Option<&rkyv::vec::ArchivedVec<ArchivedCachedChild>> {
    let value = match node {
        ArchivedNode::Leaf(i) => &cached.leaves[i].value,
        ArchivedNode::LeafValue(i) => &cached.leaf_values[i].value,
    };
    match value {
        ArchivedCachedValue::Clause(children) => Some(children),
        ArchivedCachedValue::String(_)
        | ArchivedCachedValue::QString(_)
        | ArchivedCachedValue::Float(_)
        | ArchivedCachedValue::Int(_)
        | ArchivedCachedValue::Bool(_) => None,
    }
}

fn cache_rejected(msg: &'static str) -> CacheError {
    CacheError::Deserialize { msg, source: None }
}

fn collect_archived_value_strings<'a>(v: &'a ArchivedCachedValue, out: &mut Vec<&'a str>) {
    match v {
        ArchivedCachedValue::String(s) | ArchivedCachedValue::QString(s) => out.push(s.as_str()),
        ArchivedCachedValue::Float(_)
        | ArchivedCachedValue::Int(_)
        | ArchivedCachedValue::Bool(_)
        | ArchivedCachedValue::Clause(_) => {}
    }
}

fn children_from_archived(children: &rkyv::vec::ArchivedVec<ArchivedCachedChild>) -> Vec<Child> {
    children
        .iter()
        .map(|c| match c {
            ArchivedCachedChild::Leaf(i) => Child::Leaf(i.to_native()),
            ArchivedCachedChild::LeafValue(i) => Child::LeafValue(i.to_native()),
            ArchivedCachedChild::Comment(i) => Child::Comment(i.to_native()),
        })
        .collect()
}

fn archived_value_to_value(
    v: &ArchivedCachedValue,
    tokens: &mut impl Iterator<Item = StringTokens>,
) -> Result<Value, CacheError> {
    Ok(match v {
        ArchivedCachedValue::String(_) => Value::String(next_token(tokens)?),
        ArchivedCachedValue::QString(_) => Value::QString(next_token(tokens)?),
        ArchivedCachedValue::Float(f) => Value::Float(f.to_native()),
        ArchivedCachedValue::Int(i) => Value::Int(i.to_native()),
        ArchivedCachedValue::Bool(b) => Value::Bool(*b),
        ArchivedCachedValue::Clause(children) => Value::Clause(children_from_archived(children)),
    })
}

fn archived_op_to_op(op: &ArchivedCachedOperator) -> Operator {
    match op {
        ArchivedCachedOperator::Equals => Operator::Equals,
        ArchivedCachedOperator::GreaterThan => Operator::GreaterThan,
        ArchivedCachedOperator::LessThan => Operator::LessThan,
        ArchivedCachedOperator::GreaterThanOrEqual => Operator::GreaterThanOrEqual,
        ArchivedCachedOperator::LessThanOrEqual => Operator::LessThanOrEqual,
        ArchivedCachedOperator::NotEqual => Operator::NotEqual,
        ArchivedCachedOperator::EqualEqual => Operator::EqualEqual,
        ArchivedCachedOperator::QuestionEqual => Operator::QuestionEqual,
    }
}

fn string_token_to_owned(token: &StringTokens, table: &StringResolver<'_>) -> String {
    table.get(token.normal).unwrap_or_default().to_string()
}

fn next_token(tokens: &mut impl Iterator<Item = StringTokens>) -> Result<StringTokens, CacheError> {
    tokens.next().ok_or(CacheError::Deserialize {
        msg: "interned token underrun",
        source: None,
    })
}

fn range_to_cached(r: &SourceRange) -> (u32, u16, u32, u16) {
    (r.start.line, r.start.col, r.end.line, r.end.col)
}

fn cached_to_range(start_line: u32, start_col: u16, end_line: u32, end_col: u16) -> SourceRange {
    SourceRange {
        start: SourcePos {
            line: start_line,
            col: start_col,
        },
        end: SourcePos {
            line: end_line,
            col: end_col,
        },
    }
}

fn children_to_cached(children: &[Child]) -> Vec<CachedChild> {
    children
        .iter()
        .map(|c| match c {
            Child::Leaf(i) => CachedChild::Leaf(*i),
            Child::LeafValue(i) => CachedChild::LeafValue(*i),
            Child::Comment(i) => CachedChild::Comment(*i),
        })
        .collect()
}

fn leaf_to_cached(l: &Leaf, strings: &mut impl Iterator<Item = String>) -> CachedLeaf {
    let (sl, sc, el, ec) = range_to_cached(&l.pos);
    let (vsl, vsc, vel, vec_) = range_to_cached(&l.value_pos);
    CachedLeaf {
        key: next_owned_string(strings),
        value: value_to_cached(&l.value, strings),
        op: op_to_cached(&l.op),
        start_line: sl,
        start_col: sc,
        end_line: el,
        end_col: ec,
        value_start_line: vsl,
        value_start_col: vsc,
        value_end_line: vel,
        value_end_col: vec_,
    }
}

fn leaf_value_to_cached(
    lv: &LeafValue,
    strings: &mut impl Iterator<Item = String>,
) -> CachedLeafValue {
    let (sl, sc, el, ec) = range_to_cached(&lv.pos);
    CachedLeafValue {
        value: value_to_cached(&lv.value, strings),
        start_line: sl,
        start_col: sc,
        end_line: el,
        end_col: ec,
    }
}

fn comment_to_cached(c: &Comment) -> CachedComment {
    let (sl, sc, el, ec) = range_to_cached(&c.pos);
    CachedComment {
        text: c.text.clone(),
        start_line: sl,
        start_col: sc,
        end_line: el,
        end_col: ec,
    }
}

fn value_to_cached(v: &Value, strings: &mut impl Iterator<Item = String>) -> CachedValue {
    match v {
        Value::String(_) => CachedValue::String(next_owned_string(strings)),
        Value::QString(_) => CachedValue::QString(next_owned_string(strings)),
        Value::Float(f) => CachedValue::Float(*f),
        Value::Int(i) => CachedValue::Int(*i),
        Value::Bool(b) => CachedValue::Bool(*b),
        Value::Clause(children) => CachedValue::Clause(children_to_cached(children)),
    }
}

fn collect_value_tokens(value: &Value, out: &mut Vec<StringTokens>) {
    match value {
        Value::String(tokens) | Value::QString(tokens) => out.push(*tokens),
        Value::Float(_) | Value::Int(_) | Value::Bool(_) | Value::Clause(_) => {}
    }
}

fn next_owned_string(strings: &mut impl Iterator<Item = String>) -> String {
    strings
        .next()
        .expect("all arena string tokens were resolved before cache conversion")
}

fn op_to_cached(op: &Operator) -> CachedOperator {
    match op {
        Operator::Equals => CachedOperator::Equals,
        Operator::GreaterThan => CachedOperator::GreaterThan,
        Operator::LessThan => CachedOperator::LessThan,
        Operator::GreaterThanOrEqual => CachedOperator::GreaterThanOrEqual,
        Operator::LessThanOrEqual => CachedOperator::LessThanOrEqual,
        Operator::NotEqual => CachedOperator::NotEqual,
        Operator::EqualEqual => CachedOperator::EqualEqual,
        Operator::QuestionEqual => CachedOperator::QuestionEqual,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CachedFile, arena_to_cached, children_to_cached, comment_to_cached, next_token,
        op_to_cached, range_to_cached, string_token_to_owned,
    };
    use crate::cache_format::{CachedLeaf, CachedLeafValue, CachedValue};
    use crate::io::CacheError;
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::{StringResolver, StringTable, StringTokens};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn arena_to_cached_preserves_spelling_with_overlay_strings() {
        let table = StringTable::new();
        let overlay = table.with_overlay();
        let parsed = parse_string(
            "MixedKey = MiXeDValue\nQuoted = \"Quoted Value\"\nouter = { ChildKey = ChildValue }\n",
            &overlay,
        );

        let cached = arena_to_cached(&parsed.arena, &parsed.root_children, &overlay);

        let leaf = |key: &str| {
            cached
                .leaves
                .iter()
                .find(|leaf| leaf.key == key)
                .unwrap_or_else(|| panic!("missing cached leaf {key}"))
        };
        assert!(matches!(
            &leaf("MixedKey").value,
            CachedValue::String(value) if value == "MiXeDValue"
        ));
        assert!(matches!(
            &leaf("Quoted").value,
            CachedValue::QString(value) if value == "\"Quoted Value\""
        ));
        assert!(matches!(&leaf("outer").value, CachedValue::Clause(_)));
        assert!(matches!(
            &leaf("ChildKey").value,
            CachedValue::String(value) if value == "ChildValue"
        ));
    }

    // Pre-#494 baseline: resolve tokens on demand while the resolver remains
    // held through CachedFile construction.
    fn arena_to_cached_holding_read_lock(
        arena: &cwtools_parser::ast::Arena,
        root_children: &[cwtools_parser::ast::Child],
        string_table: &StringTable,
    ) -> CachedFile {
        string_table.with_read(|table| CachedFile {
            root_children: children_to_cached(root_children),
            leaves: arena
                .leaves
                .iter()
                .map(|leaf| baseline_leaf_to_cached(leaf, &table))
                .collect(),
            leaf_values: arena
                .leaf_values
                .iter()
                .map(|leaf_value| baseline_leaf_value_to_cached(leaf_value, &table))
                .collect(),
            comments: arena.comments.iter().map(comment_to_cached).collect(),
        })
    }

    fn baseline_leaf_to_cached(
        leaf: &cwtools_parser::ast::Leaf,
        table: &StringResolver<'_>,
    ) -> CachedLeaf {
        let (sl, sc, el, ec) = range_to_cached(&leaf.pos);
        let (vsl, vsc, vel, vec_) = range_to_cached(&leaf.value_pos);
        CachedLeaf {
            key: string_token_to_owned(&leaf.key, table),
            value: baseline_value_to_cached(&leaf.value, table),
            op: op_to_cached(&leaf.op),
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
            value_start_line: vsl,
            value_start_col: vsc,
            value_end_line: vel,
            value_end_col: vec_,
        }
    }

    fn baseline_leaf_value_to_cached(
        leaf_value: &cwtools_parser::ast::LeafValue,
        table: &StringResolver<'_>,
    ) -> CachedLeafValue {
        let (sl, sc, el, ec) = range_to_cached(&leaf_value.pos);
        CachedLeafValue {
            value: baseline_value_to_cached(&leaf_value.value, table),
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
        }
    }

    fn baseline_value_to_cached(
        value: &cwtools_parser::ast::Value,
        table: &StringResolver<'_>,
    ) -> CachedValue {
        match value {
            cwtools_parser::ast::Value::String(token) => {
                CachedValue::String(string_token_to_owned(token, table))
            }
            cwtools_parser::ast::Value::QString(token) => {
                CachedValue::QString(string_token_to_owned(token, table))
            }
            cwtools_parser::ast::Value::Float(value) => CachedValue::Float(*value),
            cwtools_parser::ast::Value::Int(value) => CachedValue::Int(*value),
            cwtools_parser::ast::Value::Bool(value) => CachedValue::Bool(*value),
            cwtools_parser::ast::Value::Clause(children) => {
                CachedValue::Clause(children_to_cached(children))
            }
        }
    }

    /// Interleaved comparison on a checked-in ~165 KiB event file, with a
    /// concurrent interner to make resolver lock duration observable.
    /// Run with: cargo test -p cwtools_cache -- --ignored --nocapture bench_arena_to_cached_lock_scope
    #[test]
    #[ignore]
    fn bench_arena_to_cached_lock_scope() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testfiles/performancetest2/events/cc_colony_events.txt");
        let source = std::fs::read_to_string(path).expect("read cache benchmark fixture");
        let table = StringTable::new();
        let parsed = parse_string(&source, &table);
        assert!(
            !parsed.arena.leaves.is_empty(),
            "benchmark fixture did not parse"
        );

        // Warm both paths before collecting timings.
        let _ = arena_to_cached_holding_read_lock(&parsed.arena, &parsed.root_children, &table);
        let _ = arena_to_cached(&parsed.arena, &parsed.root_children, &table);

        let mut isolated_held_lock = Vec::with_capacity(100);
        let mut isolated_released_lock = Vec::with_capacity(100);
        for round in 0..100 {
            let run_held = || {
                let start = Instant::now();
                let _ =
                    arena_to_cached_holding_read_lock(&parsed.arena, &parsed.root_children, &table);
                start.elapsed()
            };
            let run_released = || {
                let start = Instant::now();
                let _ = arena_to_cached(&parsed.arena, &parsed.root_children, &table);
                start.elapsed()
            };
            if round % 2 == 0 {
                isolated_held_lock.push(run_held());
                isolated_released_lock.push(run_released());
            } else {
                isolated_released_lock.push(run_released());
                isolated_held_lock.push(run_held());
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let writes = Arc::new(AtomicUsize::new(0));
        let writer_table = table.clone();
        let writer_stop = Arc::clone(&stop);
        let writer_writes = Arc::clone(&writes);
        let writer = thread::spawn(move || {
            let mut sequence = 0usize;
            while !writer_stop.load(Ordering::Relaxed) {
                let value = format!("cache_bench_writer_{sequence}");
                writer_table.intern(&value);
                sequence += 1;
                writer_writes.fetch_add(1, Ordering::Relaxed);
            }
        });
        thread::sleep(Duration::from_millis(100));

        let mut held_lock = Vec::with_capacity(100);
        let mut released_lock = Vec::with_capacity(100);
        let mut held_writer_progress = Vec::with_capacity(100);
        let mut released_writer_progress = Vec::with_capacity(100);
        for round in 0..100 {
            let run_held = || {
                let writes_before = writes.load(Ordering::Relaxed);
                let start = Instant::now();
                let _ =
                    arena_to_cached_holding_read_lock(&parsed.arena, &parsed.root_children, &table);
                (
                    start.elapsed(),
                    writes.load(Ordering::Relaxed) - writes_before,
                )
            };
            let run_released = || {
                let writes_before = writes.load(Ordering::Relaxed);
                let start = Instant::now();
                let _ = arena_to_cached(&parsed.arena, &parsed.root_children, &table);
                (
                    start.elapsed(),
                    writes.load(Ordering::Relaxed) - writes_before,
                )
            };
            if round % 2 == 0 {
                let (duration, progress) = run_held();
                held_lock.push(duration);
                held_writer_progress.push(progress);
                let (duration, progress) = run_released();
                released_lock.push(duration);
                released_writer_progress.push(progress);
            } else {
                let (duration, progress) = run_released();
                released_lock.push(duration);
                released_writer_progress.push(progress);
                let (duration, progress) = run_held();
                held_lock.push(duration);
                held_writer_progress.push(progress);
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().expect("join interleaved benchmark writer");

        fn percentile<T: Copy + Ord>(samples: &mut [T], percentile: usize) -> T {
            samples.sort_unstable();
            samples[(samples.len() * percentile).div_ceil(100).saturating_sub(1)]
        }
        let held_median = percentile(&mut held_lock, 50);
        let held_p95 = percentile(&mut held_lock, 95);
        let released_median = percentile(&mut released_lock, 50);
        let released_p95 = percentile(&mut released_lock, 95);
        let held_writer_median = percentile(&mut held_writer_progress, 50);
        let released_writer_median = percentile(&mut released_writer_progress, 50);
        let isolated_held_median = percentile(&mut isolated_held_lock, 50);
        let isolated_held_p95 = percentile(&mut isolated_held_lock, 95);
        let isolated_released_median = percentile(&mut isolated_released_lock, 50);
        let isolated_released_p95 = percentile(&mut isolated_released_lock, 95);
        assert!(
            released_writer_progress.iter().sum::<usize>() > 0,
            "benchmark writer made no progress during released-lock samples"
        );
        eprintln!(
            "fixture_bytes={} interleaved_rounds={} writer_interns={}\nuncontended held-lock median={:?} p95={:?}\nuncontended released-lock median={:?} p95={:?}\ncontended held-lock median={:?} p95={:?} writer-ops/convert median={}\ncontended released-lock median={:?} p95={:?} writer-ops/convert median={}",
            source.len(),
            held_lock.len(),
            writes.load(Ordering::Relaxed),
            isolated_held_median,
            isolated_held_p95,
            isolated_released_median,
            isolated_released_p95,
            held_median,
            held_p95,
            held_writer_median,
            released_median,
            released_p95,
            released_writer_median,
        );
    }

    #[test]
    fn missing_interned_token_is_a_cache_error() {
        let mut tokens = std::iter::empty::<StringTokens>();
        assert!(matches!(
            next_token(&mut tokens),
            Err(CacheError::Deserialize {
                msg: "interned token underrun",
                source: None,
            })
        ));
    }
}
