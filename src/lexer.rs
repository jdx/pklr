use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Ident(String),
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    Null,

    // Punctuation
    LBrace,       // {
    RBrace,       // }
    LParen,       // (
    RParen,       // )
    LBracket,     // [
    RBracket,     // ]
    Comma,        // ,
    Dot,          // .
    DotDotDot,    // ...
    Equals,       // =
    Colon,        // :
    QuestionMark, // ?
    Bang,         // !
    Pipe,         // |
    Caret,        // ^
    At,           // @

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
    KwFor,
    KwIn,

    // End of file
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub col: usize,
}

pub fn lex(source: &str) -> Result<Vec<Token>> {
    let mut lexer = Lexer::new(source);
    lexer.tokenize()
}

struct Lexer<'a> {
    source: &'a str,
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while self
                .peek()
                .map(|c| c.is_ascii_whitespace())
                .unwrap_or(false)
            {
                self.advance();
            }
            // Skip line comments
            if self.source[self.pos..].starts_with("//") {
                while self.peek().map(|c| c != '\n').unwrap_or(false) {
                    self.advance();
                }
                continue;
            }
            // Skip block comments /* ... */
            if self.source[self.pos..].starts_with("/*") {
                self.advance();
                self.advance(); // consume /*
                loop {
                    if self.source[self.pos..].starts_with("*/") {
                        self.advance();
                        self.advance();
                        break;
                    }
                    if self.advance().is_none() {
                        break;
                    }
                }
                continue;
            }
            break;
        }
    }

    fn read_string(&mut self) -> Result<String> {
        // Assumes opening quote already consumed
        let mut s = String::new();
        loop {
            match self.advance() {
                None => {
                    return Err(Error::Lex {
                        line: self.line,
                        col: self.col,
                        message: "unterminated string".into(),
                    });
                }
                Some('"') => break,
                Some('\\') => {
                    match self.advance() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('"') => s.push('"'),
                        Some('\\') => s.push('\\'),
                        Some('(') => {
                            // String interpolation \( ... ) - not fully supported yet
                            s.push_str("\\(");
                        }
                        Some(c) => {
                            s.push('\\');
                            s.push(c);
                        }
                        None => {
                            return Err(Error::Lex {
                                line: self.line,
                                col: self.col,
                                message: "unterminated escape".into(),
                            });
                        }
                    }
                }
                Some(c) => s.push(c),
            }
        }
        Ok(s)
    }

    fn read_multiline_string(&mut self) -> Result<String> {
        // Already consumed the first three `"`
        // Read until closing `"""`
        let mut s = String::new();
        loop {
            if self.source[self.pos..].starts_with("\"\"\"") {
                self.advance();
                self.advance();
                self.advance();
                break;
            }
            match self.advance() {
                None => {
                    return Err(Error::Lex {
                        line: self.line,
                        col: self.col,
                        message: "unterminated multiline string".into(),
                    });
                }
                Some(c) => s.push(c),
            }
        }
        // Strip leading/trailing newlines per pkl convention
        let s = s.trim_start_matches('\n');
        // Dedent by removing common leading whitespace
        dedent(s)
    }

    fn read_number(&mut self, first: char) -> Result<TokenKind> {
        // self.pos is already PAST first (caller called advance() before us)
        let start = self.pos - first.len_utf8();

        // Handle 0x / 0b / 0o prefixes immediately after '0'
        if first == '0' {
            match self.peek() {
                Some('x') | Some('X') => {
                    self.advance(); // consume 'x'
                    while self
                        .peek()
                        .map(|c| c.is_ascii_hexdigit() || c == '_')
                        .unwrap_or(false)
                    {
                        self.advance();
                    }
                    let raw = self.source[start..self.pos].replace('_', "");
                    let v = i64::from_str_radix(&raw[2..], 16).map_err(|_| Error::Lex {
                        line: self.line,
                        col: self.col,
                        message: format!("invalid hex literal: {raw}"),
                    })?;
                    return Ok(TokenKind::IntLit(v));
                }
                Some('b') | Some('B') => {
                    self.advance();
                    while self
                        .peek()
                        .map(|c| c == '0' || c == '1' || c == '_')
                        .unwrap_or(false)
                    {
                        self.advance();
                    }
                    let raw = self.source[start..self.pos].replace('_', "");
                    let v = i64::from_str_radix(&raw[2..], 2).map_err(|_| Error::Lex {
                        line: self.line,
                        col: self.col,
                        message: format!("invalid binary literal: {raw}"),
                    })?;
                    return Ok(TokenKind::IntLit(v));
                }
                Some('o') | Some('O') => {
                    self.advance();
                    while self
                        .peek()
                        .map(|c| matches!(c, '0'..='7') || c == '_')
                        .unwrap_or(false)
                    {
                        self.advance();
                    }
                    let raw = self.source[start..self.pos].replace('_', "");
                    let v = i64::from_str_radix(&raw[2..], 8).map_err(|_| Error::Lex {
                        line: self.line,
                        col: self.col,
                        message: format!("invalid octal literal: {raw}"),
                    })?;
                    return Ok(TokenKind::IntLit(v));
                }
                _ => {}
            }
        }

        // Consume remaining decimal digits
        while self
            .peek()
            .map(|c| c.is_ascii_digit() || c == '_')
            .unwrap_or(false)
        {
            self.advance();
        }
        let is_float = self.peek() == Some('.');
        if is_float {
            self.advance(); // consume '.'
            while self
                .peek()
                .map(|c| c.is_ascii_digit() || c == '_')
                .unwrap_or(false)
            {
                self.advance();
            }
        }
        // Exponent
        if self.peek().map(|c| c == 'e' || c == 'E').unwrap_or(false) {
            self.advance();
            if self.peek().map(|c| c == '+' || c == '-').unwrap_or(false) {
                self.advance();
            }
            while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                self.advance();
            }
        }
        let raw = &self.source[start..self.pos];
        let cleaned = raw.replace('_', "");
        if is_float || cleaned.contains('e') || cleaned.contains('E') {
            let v: f64 = cleaned.parse().map_err(|_| Error::Lex {
                line: self.line,
                col: self.col,
                message: format!("invalid float: {raw}"),
            })?;
            Ok(TokenKind::FloatLit(v))
        } else {
            let v = cleaned.parse::<i64>().map_err(|_| Error::Lex {
                line: self.line,
                col: self.col,
                message: format!("invalid integer: {raw}"),
            })?;
            Ok(TokenKind::IntLit(v))
        }
    }

    fn tokenize(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            let line = self.line;
            let col = self.col;

            let ch = match self.peek() {
                None => {
                    tokens.push(Token {
                        kind: TokenKind::Eof,
                        line,
                        col,
                    });
                    break;
                }
                Some(c) => c,
            };

            let kind = match ch {
                '{' => {
                    self.advance();
                    TokenKind::LBrace
                }
                '}' => {
                    self.advance();
                    TokenKind::RBrace
                }
                '(' => {
                    self.advance();
                    TokenKind::LParen
                }
                ')' => {
                    self.advance();
                    TokenKind::RParen
                }
                '[' => {
                    self.advance();
                    TokenKind::LBracket
                }
                ']' => {
                    self.advance();
                    TokenKind::RBracket
                }
                ',' => {
                    self.advance();
                    TokenKind::Comma
                }
                '.' => {
                    self.advance();
                    if self.source[self.pos..].starts_with("..") {
                        self.advance();
                        self.advance();
                        TokenKind::DotDotDot
                    } else {
                        TokenKind::Dot
                    }
                }
                '=' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::EqEq
                    } else if self.peek() == Some('>') {
                        self.advance();
                        TokenKind::ThinArrow
                    } else {
                        TokenKind::Equals
                    }
                }
                ':' => {
                    self.advance();
                    TokenKind::Colon
                }
                '?' => {
                    self.advance();
                    TokenKind::QuestionMark
                }
                '!' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::BangEq
                    } else {
                        TokenKind::Bang
                    }
                }
                '|' => {
                    self.advance();
                    if self.peek() == Some('|') {
                        self.advance();
                        TokenKind::PipePipe
                    } else {
                        TokenKind::Pipe
                    }
                }
                '^' => {
                    self.advance();
                    TokenKind::Caret
                }
                '@' => {
                    self.advance();
                    TokenKind::At
                }
                '+' => {
                    self.advance();
                    TokenKind::Plus
                }
                '-' => {
                    self.advance();
                    if self.peek() == Some('>') {
                        self.advance();
                        TokenKind::Arrow
                    } else {
                        TokenKind::Minus
                    }
                }
                '*' => {
                    self.advance();
                    TokenKind::Star
                }
                '/' => {
                    self.advance();
                    TokenKind::Slash
                }
                '%' => {
                    self.advance();
                    TokenKind::Percent
                }
                '<' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::LtEq
                    } else {
                        TokenKind::Lt
                    }
                }
                '>' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::GtEq
                    } else {
                        TokenKind::Gt
                    }
                }
                '&' => {
                    self.advance();
                    if self.peek() == Some('&') {
                        self.advance();
                        TokenKind::AmpAmp
                    } else {
                        return Err(Error::Lex {
                            line,
                            col,
                            message: "unexpected '&'".into(),
                        });
                    }
                }
                '"' => {
                    self.advance();
                    // Check for multiline string `"""`
                    if self.source[self.pos..].starts_with("\"\"") {
                        self.advance();
                        self.advance();
                        let s = self.read_multiline_string()?;
                        TokenKind::StringLit(s)
                    } else {
                        let s = self.read_string()?;
                        TokenKind::StringLit(s)
                    }
                }
                '#' => {
                    // #"..."# raw strings
                    self.advance();
                    if self.peek() == Some('"') {
                        self.advance();
                        let s = self.read_raw_string('#')?;
                        TokenKind::StringLit(s)
                    } else {
                        // Could be a shebang line or annotation
                        while self.peek().map(|c| c != '\n').unwrap_or(false) {
                            self.advance();
                        }
                        continue;
                    }
                }
                c if c.is_ascii_digit() => {
                    self.advance();
                    self.read_number(c)?
                }
                c if c.is_alphabetic() || c == '_' => {
                    let start = self.pos; // pos points TO current char before advance
                    self.advance();
                    while self
                        .peek()
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false)
                    {
                        self.advance();
                    }
                    let ident = &self.source[start..self.pos];
                    keyword_or_ident(ident)
                }
                c => {
                    return Err(Error::Lex {
                        line,
                        col,
                        message: format!("unexpected character: {c:?}"),
                    });
                }
            };

            tokens.push(Token { kind, line, col });
        }
        Ok(tokens)
    }

    fn read_raw_string(&mut self, _delimiter: char) -> Result<String> {
        // Read until `"#`
        let mut s = String::new();
        loop {
            if self.source[self.pos..].starts_with("\"#") {
                self.advance();
                self.advance();
                break;
            }
            match self.advance() {
                None => {
                    return Err(Error::Lex {
                        line: self.line,
                        col: self.col,
                        message: "unterminated raw string".into(),
                    });
                }
                Some(c) => s.push(c),
            }
        }
        Ok(s)
    }
}

fn keyword_or_ident(s: &str) -> TokenKind {
    match s {
        "amends" => TokenKind::KwAmends,
        "import" => TokenKind::KwImport,
        "as" => TokenKind::KwAs,
        "local" => TokenKind::KwLocal,
        "const" => TokenKind::KwConst,
        "fixed" => TokenKind::KwFixed,
        "hidden" => TokenKind::KwHidden,
        "new" => TokenKind::KwNew,
        "extends" => TokenKind::KwExtends,
        "abstract" => TokenKind::KwAbstract,
        "open" => TokenKind::KwOpen,
        "external" => TokenKind::KwExternal,
        "class" => TokenKind::KwClass,
        "typealias" => TokenKind::KwTypeAlias,
        "function" => TokenKind::KwFunction,
        "this" => TokenKind::KwThis,
        "module" => TokenKind::KwModule,
        "import*" => TokenKind::KwImportStar,
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "when" => TokenKind::KwWhen,
        "is" => TokenKind::KwIs,
        "let" => TokenKind::KwLet,
        "throw" => TokenKind::KwThrow,
        "trace" => TokenKind::KwTrace,
        "read" => TokenKind::KwRead,
        "read?" => TokenKind::KwReadOrNull,
        "for" => TokenKind::KwFor,
        "in" => TokenKind::KwIn,
        "true" => TokenKind::BoolLit(true),
        "false" => TokenKind::BoolLit(false),
        "null" => TokenKind::Null,
        _ => TokenKind::Ident(s.to_string()),
    }
}

fn dedent(s: &str) -> Result<String> {
    let lines: Vec<&str> = s.lines().collect();
    if lines.is_empty() {
        return Ok(String::new());
    }
    // Find minimum indentation (ignoring empty lines)
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let dedented: Vec<&str> = lines
        .iter()
        .map(|l| {
            if l.len() >= min_indent {
                &l[min_indent..]
            } else {
                l.trim_start()
            }
        })
        .collect();
    Ok(dedented.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn test_basic() {
        let toks = kinds(r#"foo = "hello""#);
        assert_eq!(
            toks,
            vec![
                TokenKind::Ident("foo".into()),
                TokenKind::Equals,
                TokenKind::StringLit("hello".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_amends() {
        let toks = kinds(r#"amends "pkl/Config.pkl""#);
        assert_eq!(
            toks,
            vec![
                TokenKind::KwAmends,
                TokenKind::StringLit("pkl/Config.pkl".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_numbers() {
        let toks = kinds("42 1.23 0xFF");
        assert_eq!(
            toks,
            vec![
                TokenKind::IntLit(42),
                TokenKind::FloatLit(1.23),
                TokenKind::IntLit(255),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_booleans() {
        let toks = kinds("true false null");
        assert_eq!(
            toks,
            vec![
                TokenKind::BoolLit(true),
                TokenKind::BoolLit(false),
                TokenKind::Null,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_line_comment() {
        let toks = kinds("// comment\nfoo");
        assert_eq!(toks, vec![TokenKind::Ident("foo".into()), TokenKind::Eof]);
    }
}
