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
    // Acquire the read lock once for the whole conversion rather than per token.
    string_table.with_read(|table| CachedFile {
        root_children: children_to_cached(root_children),
        leaves: arena
            .leaves
            .iter()
            .map(|l| leaf_to_cached(l, &table))
            .collect(),
        leaf_values: arena
            .leaf_values
            .iter()
            .map(|lv| leaf_value_to_cached(lv, &table))
            .collect(),
        comments: arena.comments.iter().map(comment_to_cached).collect(),
    })
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
            key: next_token(&mut tokens),
            value: archived_value_to_value(&l.value, &mut tokens),
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
            value: archived_value_to_value(&lv.value, &mut tokens),
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
) -> Value {
    match v {
        ArchivedCachedValue::String(_) => Value::String(next_token(tokens)),
        ArchivedCachedValue::QString(_) => Value::QString(next_token(tokens)),
        ArchivedCachedValue::Float(f) => Value::Float(f.to_native()),
        ArchivedCachedValue::Int(i) => Value::Int(i.to_native()),
        ArchivedCachedValue::Bool(b) => Value::Bool(*b),
        ArchivedCachedValue::Clause(children) => Value::Clause(children_from_archived(children)),
    }
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

fn next_token(tokens: &mut impl Iterator<Item = StringTokens>) -> StringTokens {
    tokens.next().expect("interned token underrun")
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

fn leaf_to_cached(l: &Leaf, table: &StringResolver<'_>) -> CachedLeaf {
    let (sl, sc, el, ec) = range_to_cached(&l.pos);
    let (vsl, vsc, vel, vec_) = range_to_cached(&l.value_pos);
    CachedLeaf {
        key: string_token_to_owned(&l.key, table),
        value: value_to_cached(&l.value, table),
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

fn leaf_value_to_cached(lv: &LeafValue, table: &StringResolver<'_>) -> CachedLeafValue {
    let (sl, sc, el, ec) = range_to_cached(&lv.pos);
    CachedLeafValue {
        value: value_to_cached(&lv.value, table),
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

fn value_to_cached(v: &Value, table: &StringResolver<'_>) -> CachedValue {
    match v {
        Value::String(t) => CachedValue::String(string_token_to_owned(t, table)),
        Value::QString(t) => CachedValue::QString(string_token_to_owned(t, table)),
        Value::Float(f) => CachedValue::Float(*f),
        Value::Int(i) => CachedValue::Int(*i),
        Value::Bool(b) => CachedValue::Bool(*b),
        Value::Clause(children) => CachedValue::Clause(children_to_cached(children)),
    }
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
