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
    /// Every [`CachedComment`] ranges into this one pooled buffer, mirroring
    /// the arena's comment pool (#575).
    pub comment_text_pool: String,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedErrors {
    pub errors: Vec<CachedParseError>,
    pub source: Option<CachedSource>,
}

/// What a scan needs from a source's text besides its parse, so a metadata hit
/// can skip reading the file. Ignore lines and codes are sorted.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedSource {
    pub content_hash: u64,
    pub inline_ignored: Vec<(u32, Vec<String>)>,
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
    pub color_prefix: Option<CachedColorPrefix>,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedLeafValue {
    pub value: CachedValue,
    pub start_line: u32,
    pub start_col: u16,
    pub end_line: u32,
    pub end_col: u16,
    pub color_prefix: Option<CachedColorPrefix>,
}

#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum CachedColorPrefix {
    Rgb,
    Hsv,
    Hsv360,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct CachedComment {
    /// Byte range into [`CachedFile::comment_text_pool`], the pooled comment
    /// text of this entry (#575).
    pub text_start: u32,
    pub text_end: u32,
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
