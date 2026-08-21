use cwtools_string_table::string_table::StringTokens;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("{0}:{1}: {2}")]
    Pos(u32, u16, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equals,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
    NotEqual,
    EqualEqual,
    QuestionEqual,
}

impl Operator {
    pub fn as_str(&self) -> &'static str {
        match self {
            Operator::Equals => "=",
            Operator::GreaterThan => ">",
            Operator::LessThan => "<",
            Operator::GreaterThanOrEqual => ">=",
            Operator::LessThanOrEqual => "<=",
            Operator::NotEqual => "!=",
            Operator::EqualEqual => "==",
            Operator::QuestionEqual => "?=",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourcePos {
    pub line: u32,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    pub start: SourcePos,
    pub end: SourcePos,
}

// Arena indices
pub type LeafIdx = u32;
pub type LeafValueIdx = u32;
pub type CommentIdx = u32;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(StringTokens),
    QString(StringTokens),
    Float(f64),
    Int(i64),
    Bool(bool),
    Clause(Vec<Child>),
}

/// AST child reference. A keyed clause (`key = { ... }`) is a [`Leaf`] whose
/// value is [`Value::Clause`] — there is ONE clause representation (see
/// [`Arena::keyed_clause`]); the parser produces nothing else.
#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    Leaf(LeafIdx),
    LeafValue(LeafValueIdx),
    Comment(CommentIdx),
}

pub struct Leaf {
    pub key: StringTokens,
    pub value: Value,
    pub op: Operator,
    pub pos: SourceRange,
    pub value_pos: SourceRange,
}

pub struct LeafValue {
    pub value: Value,
    pub pos: SourceRange,
}

pub struct Comment {
    pub text: String,
    pub pos: SourceRange,
}

pub struct Arena {
    pub leaves: Vec<Leaf>,
    pub leaf_values: Vec<LeafValue>,
    pub comments: Vec<Comment>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Arena {
    pub fn new() -> Self {
        Self {
            leaves: Vec::new(),
            leaf_values: Vec::new(),
            comments: Vec::new(),
        }
    }

    pub fn push_leaf(&mut self, leaf: Leaf) -> LeafIdx {
        let idx = self.leaves.len() as u32;
        self.leaves.push(leaf);
        idx
    }

    pub fn push_leaf_value(&mut self, lv: LeafValue) -> LeafValueIdx {
        let idx = self.leaf_values.len() as u32;
        self.leaf_values.push(lv);
        idx
    }

    pub fn push_comment(&mut self, comment: Comment) -> CommentIdx {
        let idx = self.comments.len() as u32;
        self.comments.push(comment);
        idx
    }
}

/// View of a keyed clause (`key = { ... }`): a [`Leaf`] whose value is
/// [`Value::Clause`]. Prefer [`Arena::keyed_clause`] over matching by hand.
pub struct KeyedClause<'a> {
    pub key: StringTokens,
    pub children: &'a [Child],
    pub pos: SourceRange,
}

impl Arena {
    /// The keyed-clause view of `child`; `None` for anything that isn't a
    /// `Leaf` with a `Value::Clause` value.
    pub fn keyed_clause<'a>(&'a self, child: &Child) -> Option<KeyedClause<'a>> {
        match child {
            Child::Leaf(i) => {
                let l = &self.leaves[*i as usize];
                match &l.value {
                    Value::Clause(ch) => Some(KeyedClause {
                        key: l.key,
                        children: ch,
                        pos: l.pos,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

pub struct ParsedFile {
    pub arena: Arena,
    pub root_children: Vec<Child>,
    pub errors: Vec<ParseError>,
}
