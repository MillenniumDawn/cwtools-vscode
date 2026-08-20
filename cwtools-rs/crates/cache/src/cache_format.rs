/// Self-contained cached AST for a single file.
/// All strings are stored inline (not as StringTable IDs) so the cache
/// can be loaded without an external string table.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedFile {
    pub root_children: Vec<CachedChild>,
    pub leaves: Vec<CachedLeaf>,
    pub leaf_values: Vec<CachedLeafValue>,
    pub comments: Vec<CachedComment>,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedErrors {
    pub errors: Vec<CachedParseError>,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum CachedParseError {
    Pos(u32, u16, String),
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum CachedChild {
    Leaf(u32),
    LeafValue(u32),
    Comment(u32),
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedLeaf {
    pub key: String,
    pub value: CachedValue,
    pub op: CachedOperator,
    pub start_line: u32,
    pub start_col: u16,
    pub end_line: u32,
    pub end_col: u16,
    pub value_start_line: u32,
    pub value_start_col: u16,
    pub value_end_line: u32,
    pub value_end_col: u16,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedLeafValue {
    pub value: CachedValue,
    pub start_line: u32,
    pub start_col: u16,
    pub end_line: u32,
    pub end_col: u16,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedComment {
    pub text: String,
    pub start_line: u32,
    pub start_col: u16,
    pub end_line: u32,
    pub end_col: u16,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum CachedValue {
    String(String),
    QString(String),
    Float(f64),
    Int(i64),
    Bool(bool),
    Clause(Vec<CachedChild>),
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum CachedOperator {
    Equals,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
    NotEqual,
    EqualEqual,
    QuestionEqual,
}
