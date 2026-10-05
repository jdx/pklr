#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StringPart {
    Literal(String),
    Tokens(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TokenKind {
    // Literals
    Ident(String),
    StringLit(String),
    /// Interpolated string: alternating literal parts and expression-token groups.
    /// Parts[0] is always a literal (possibly empty), Parts[1] is tokens for first \(...), etc.
    InterpolatedString(Vec<StringPart>),
    /// A `///` doc comment line; its span covers the comment text.
    DocComment,
    IntLit(i64),
    /// The magnitude `9223372036854775808`, which is valid only as the
    /// operand of a unary minus.
    MinIntLit,
    FloatLit(f64),
    BoolLit(bool),
    Null,

    // Punctuation
    LBrace,            // {
    RBrace,            // }
    LParen,            // (
    RParen,            // )
    LBracket,          // [
    LPred,             // [[ (member predicate)
    RBracket,          // ]
    Comma,             // ,
    Semicolon,         // ;
    Dot,               // .
    DotDotDot,         // ...
    QuestionDotDotDot, // ...?
    Equals,            // =
    Colon,             // :
    QuestionMark,      // ?
    QuestionQuestion,  // ??
    QuestionDot,       // ?.
    Bang,              // !
    BangBang,          // !!
    Pipe,              // |
    PipeGt,            // |>
    Caret,             // ^
    At,                // @

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    BangEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    TildeSlash, // ~/
    StarStar,   // **
    AmpAmp,
    PipePipe,
    Arrow,     // ->
    ThinArrow, // =>

    // Keywords
    KwAmends,
    KwImport,
    KwAs,
    KwLocal,
    KwConst,
    KwFixed,
    KwHidden,
    KwNew,
    KwExtends,
    KwAbstract,
    KwOpen,
    KwExternal,
    KwClass,
    KwTypeAlias,
    KwFunction,
    KwThis,
    KwSuper,
    KwModule,
    KwImportStar,
    KwIf,
    KwElse,
    KwWhen,
    KwIs,
    KwLet,
    KwThrow,
    KwTrace,
    KwRead,
    KwReadOrNull,
    KwReadGlob,
    KwFor,
    KwIn,

    // End of file
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub col: usize,
    /// Byte offset in the source string where this token starts.
    pub offset: usize,
    /// Byte offset immediately after this token.
    pub end: usize,
}
