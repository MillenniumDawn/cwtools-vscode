use crate::cache_format::*;
use crate::io::CacheError;
use cwtools_parser::ast::{
    Arena, Child, Comment, Leaf, LeafValue, Operator, ParseError, SourcePos, SourceRange, Value,
};
use cwtools_string_table::string_table::{StringResolver, StringTable, StringTokens};

/// Convert an arena AST (with StringTable IDs) into a self-contained CachedFile.
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

/// Convert recovered parse errors into their self-contained cache form.
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

/// Rebuild recovered parse errors from their cache form.
pub fn cached_errors_to_parse(cached: CachedErrors) -> Vec<ParseError> {
    cached
        .errors
        .into_iter()
        .map(|CachedParseError::Pos(line, col, message)| ParseError::Pos(line, col, message))
        .collect()
}

/// Rebuild an arena AST from the rkyv archived view, interning strings straight
/// out of the mapped buffer. Two-pass: collect every string in traversal order
/// (leaves, then leaf_values), batch-intern under a single write lock via
/// [`StringTable::intern_batch`](cwtools_string_table::string_table::StringTable::intern_batch),
/// then re-walk the nodes drawing the resulting tokens in the same order. The
/// token assignment is identical to per-string interning.
///
/// The rebuilt child references are bounds-checked against the arena vectors
/// once here (see [`validate_child_bounds`]); a corrupted or truncated cache
/// yields a [`CacheError`] so the caller's miss/re-parse fallback engages
/// rather than a panic deep inside a downstream consumer.
pub fn archived_to_arena(
    cached: &ArchivedCachedFile,
    string_table: &StringTable,
) -> Result<(Arena, Vec<Child>), CacheError> {
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
    validate_child_bounds(&arena, &root)?;
    Ok((arena, root))
}

/// Reject a cache whose child references fall outside the rebuilt arena vectors.
/// Downstream consumers index `arena.leaves[i]` (see [`Arena::keyed_clause`])
/// with no bounds checks, so a corrupted/truncated cache would panic far from
/// the load site. This runs once at the load boundary; every child list is
/// visited exactly once (`root` plus each node's `Value::Clause` children).
/// Indices are never followed, so a corrupt self-referential index can't loop.
fn validate_child_bounds(arena: &Arena, root: &[Child]) -> Result<(), CacheError> {
    check_child_list(arena, root)?;
    for l in &arena.leaves {
        if let Value::Clause(children) = &l.value {
            check_child_list(arena, children)?;
        }
    }
    for lv in &arena.leaf_values {
        if let Value::Clause(children) = &lv.value {
            check_child_list(arena, children)?;
        }
    }
    Ok(())
}

fn check_child_list(arena: &Arena, children: &[Child]) -> Result<(), CacheError> {
    for child in children {
        let in_bounds = match child {
            Child::Leaf(i) => (*i as usize) < arena.leaves.len(),
            Child::LeafValue(i) => (*i as usize) < arena.leaf_values.len(),
            Child::Comment(i) => (*i as usize) < arena.comments.len(),
        };
        if !in_bounds {
            return Err(CacheError::Deserialize {
                msg: "cache child index out of bounds",
                source: None,
            });
        }
    }
    Ok(())
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

// ---- helpers ----

fn string_token_to_owned(token: &StringTokens, table: &StringResolver<'_>) -> String {
    table.get(token.normal).unwrap_or_default().to_string()
}

/// Draw the next pre-interned token from the batch. The batch was collected in
/// the exact order these are consumed, so each call yields the token for the
/// string that would have been re-interned at this point.
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
