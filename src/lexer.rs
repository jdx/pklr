use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    Literal(String),
    Tokens(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Ident(String),
    StringLit(String),
    /// Interpolated string: alternating literal parts and expression-token groups.
    /// Parts[0] is always a literal (possibly empty), Parts[1] is tokens for first \(...), etc.
    InterpolatedString(Vec<StringPart>),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    Null,

    // Punctuation
    LBrace,           // {
    RBrace,           // }
    LParen,           // (
    RParen,           // )
    LBracket,         // [
    RBracket,         // ]
    Comma,            // ,
    Dot,              // .
    DotDotDot,        // ...
    Equals,           // =
    Colon,            // :
    QuestionMark,     // ?
    QuestionQuestion, // ??
    QuestionDot,      // ?.
    Bang,             // !
    BangBang,         // !!
    Pipe,             // |
    PipeGt,           // |>
    Caret,            // ^
    At,               // @

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
}

pub fn lex(source: &str) -> Result<Vec<Token>> {
    lex_named(source, "<input>")
}

pub fn lex_named(source: &str, name: &str) -> Result<Vec<Token>> {
    let mut lexer = Lexer::new(source, name);
    lexer.tokenize()
}

struct Lexer<'a> {
    source: &'a str,
    name: String,
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, name: &str) -> Self {
        Self {
            source,
            name: name.to_string(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn lex_error(&self, message: impl Into<String>) -> Error {
        Error::Lex {
            src: miette::NamedSource::new(&self.name, self.source.to_string()),
            span: miette::SourceOffset::from(self.pos),
            message: message.into(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.source[self.pos..].chars().nth(n)
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
            // Skip whitespace and semicolons (Pkl allows `;` as a property separator)
            while self
                .peek()
                .map(|c| c.is_ascii_whitespace() || c == ';')
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

    fn read_string_token(&mut self) -> Result<TokenKind> {
        // Assumes opening quote already consumed
        let mut current = String::new();
        let mut parts: Vec<StringPart> = Vec::new();
        let mut has_interpolation = false;
        loop {
            match self.advance() {
                None => {
                    return Err(self.lex_error("unterminated string"));
                }
                Some('"') => break,
                Some('\\') => {
                    match self.advance() {
                        Some('n') => current.push('\n'),
                        Some('t') => current.push('\t'),
                        Some('r') => current.push('\r'),
                        Some('"') => current.push('"'),
                        Some('\\') => current.push('\\'),
                        Some('u') => {
                            // Unicode escape: \u{XXXX}
                            if self.peek() == Some('{') {
                                self.advance(); // consume '{'
                                let mut hex = String::new();
                                loop {
                                    match self.peek() {
                                        Some('}') => {
                                            self.advance();
                                            break;
                                        }
                                        Some(c) if c.is_ascii_hexdigit() => {
                                            hex.push(c);
                                            self.advance();
                                        }
                                        _ => {
                                            return Err(
                                                self.lex_error("invalid unicode escape sequence")
                                            );
                                        }
                                    }
                                }
                                if hex.is_empty() {
                                    return Err(self.lex_error(
                                        "unicode escape must have at least one hex digit: \\u{XXXX}",
                                    ));
                                }
                                let code_point = u32::from_str_radix(&hex, 16).map_err(|_| {
                                    self.lex_error(format!(
                                        "invalid unicode code point: \\u{{{hex}}}"
                                    ))
                                })?;
                                let ch = char::from_u32(code_point).ok_or_else(|| {
                                    self.lex_error(format!(
                                        "invalid unicode code point: \\u{{{hex}}}"
                                    ))
                                })?;
                                current.push(ch);
                            } else {
                                return Err(
                                    self.lex_error("invalid unicode escape: expected \\u{XXXX}")
                                );
                            }
                        }
                        Some('(') => {
                            has_interpolation = true;
                            parts.push(StringPart::Literal(std::mem::take(&mut current)));
                            // Lex tokens until matching ')'
                            let mut depth = 1;
                            let mut expr_tokens = Vec::new();
                            loop {
                                self.skip_whitespace_and_comments();
                                if self.peek().is_none() {
                                    return Err(self.lex_error("unterminated string interpolation"));
                                }
                                if self.peek() == Some(')') && depth == 1 {
                                    self.advance();
                                    break;
                                }
                                // Use the main tokenizer to get one token
                                let line = self.line;
                                let col = self.col;
                                let offset = self.pos;
                                let kind = self.read_one_token()?;
                                if matches!(kind, TokenKind::LParen) {
                                    depth += 1;
                                } else if matches!(kind, TokenKind::RParen) {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                expr_tokens.push(Token {
                                    kind,
                                    line,
                                    col,
                                    offset,
                                });
                            }
                            // Add Eof token so the parser knows when to stop
                            expr_tokens.push(Token {
                                kind: TokenKind::Eof,
                                line: self.line,
                                col: self.col,
                                offset: self.pos,
                            });
                            parts.push(StringPart::Tokens(expr_tokens));
                        }
                        Some(c) => {
                            current.push('\\');
                            current.push(c);
                        }
                        None => {
                            return Err(self.lex_error("unterminated escape"));
                        }
                    }
                }
                Some(c) => current.push(c),
            }
        }
        if has_interpolation {
            parts.push(StringPart::Literal(current));
            Ok(TokenKind::InterpolatedString(parts))
        } else {
            Ok(TokenKind::StringLit(current))
        }
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
                    return Err(self.lex_error("unterminated multiline string"));
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
                    let v = i64::from_str_radix(&raw[2..], 16)
                        .map_err(|_| self.lex_error(format!("invalid hex literal: {raw}")))?;
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
                    let v = i64::from_str_radix(&raw[2..], 2)
                        .map_err(|_| self.lex_error(format!("invalid binary literal: {raw}")))?;
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
                    let v = i64::from_str_radix(&raw[2..], 8)
                        .map_err(|_| self.lex_error(format!("invalid octal literal: {raw}")))?;
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
        // Only treat '.' as decimal point if followed by a digit
        let is_float = self.peek() == Some('.')
            && self
                .peek_nth(1)
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
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
            let v: f64 = cleaned
                .parse()
                .map_err(|_| self.lex_error(format!("invalid float: {raw}")))?;
            Ok(TokenKind::FloatLit(v))
        } else {
            let v = cleaned
                .parse::<i64>()
                .map_err(|_| self.lex_error(format!("invalid integer: {raw}")))?;
            Ok(TokenKind::IntLit(v))
        }
    }

    fn tokenize(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            let line = self.line;
            let col = self.col;
            let offset = self.pos;

            let ch = match self.peek() {
                None => {
                    tokens.push(Token {
                        kind: TokenKind::Eof,
                        line,
                        col,
                        offset,
                    });
                    break;
                }
                Some(c) => c,
            };

            let kind = self.read_one_token_from(ch)?;

            tokens.push(Token {
                kind,
                line,
                col,
                offset,
            });
        }
        Ok(tokens)
    }

    fn read_one_token(&mut self) -> Result<TokenKind> {
        self.skip_whitespace_and_comments();
        let ch = self
            .peek()
            .ok_or_else(|| self.lex_error("unexpected end of input"))?;
        self.read_one_token_from(ch)
    }

    fn read_one_token_from(&mut self, ch: char) -> Result<TokenKind> {
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
                if self.peek() == Some('?') {
                    self.advance();
                    TokenKind::QuestionQuestion
                } else if self.peek() == Some('.') {
                    self.advance();
                    TokenKind::QuestionDot
                } else {
                    TokenKind::QuestionMark
                }
            }
            '!' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::BangEq
                } else if self.peek() == Some('!') {
                    self.advance();
                    TokenKind::BangBang
                } else {
                    TokenKind::Bang
                }
            }
            '|' => {
                self.advance();
                if self.peek() == Some('|') {
                    self.advance();
                    TokenKind::PipePipe
                } else if self.peek() == Some('>') {
                    self.advance();
                    TokenKind::PipeGt
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
                if self.peek() == Some('*') {
                    self.advance();
                    TokenKind::StarStar
                } else {
                    TokenKind::Star
                }
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
                    return Err(self.lex_error("unexpected '&'"));
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
                    self.read_string_token()?
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
                    // Could be a shebang line or annotation — skip line
                    while self.peek().map(|c| c != '\n').unwrap_or(false) {
                        self.advance();
                    }
                    return self.read_one_token();
                }
            }
            '~' => {
                self.advance();
                if self.peek() == Some('/') {
                    self.advance();
                    TokenKind::TildeSlash
                } else {
                    return Err(self.lex_error("unexpected '~'"));
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
                // Handle `import*` and `read?` as single tokens
                if ident == "import" && self.peek() == Some('*') {
                    self.advance();
                    TokenKind::KwImportStar
                } else if ident == "read" && self.peek() == Some('?') {
                    self.advance();
                    TokenKind::KwReadOrNull
                } else {
                    keyword_or_ident(ident)
                }
            }
            c => {
                return Err(self.lex_error(format!("unexpected character: {c:?}")));
            }
        };
        Ok(kind)
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
                    return Err(self.lex_error("unterminated raw string"));
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
        "super" => TokenKind::KwSuper,
        "module" => TokenKind::KwModule,
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
        "NaN" => TokenKind::FloatLit(f64::NAN),
        "Infinity" => TokenKind::FloatLit(f64::INFINITY),
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
