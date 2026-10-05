use crate::error::{Error, Result};

mod token;

pub use token::{StringPart, Token, TokenKind};

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
    token_start_line: usize,
    token_start_col: usize,
    token_start_offset: usize,
    /// String interpolations currently open around the lexer's position.
    interpolation_depth: usize,
}

/// The most string interpolations that may nest inside each other. The lexer
/// recurses for each one, so the limit keeps hostile input from overflowing
/// the stack.
const MAX_INTERPOLATION_DEPTH: usize = crate::parser::MAX_NESTING_DEPTH;

impl<'a> Lexer<'a> {
    fn new(source: &'a str, name: &str) -> Self {
        Self {
            source,
            name: name.to_string(),
            pos: 0,
            line: 1,
            col: 1,
            token_start_line: 1,
            token_start_col: 1,
            token_start_offset: 0,
            interpolation_depth: 0,
        }
    }

    fn lex_error(&self, message: impl Into<String>) -> Error {
        Error::lex(&self.name, self.source, self.pos, message.into())
    }

    #[cold]
    #[inline(never)]
    fn interpolation_depth_error(&self) -> Error {
        self.lex_error(format!(
            "string interpolations nest more than {MAX_INTERPOLATION_DEPTH} levels deep"
        ))
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

    fn mark_token_start(&mut self) {
        self.token_start_line = self.line;
        self.token_start_col = self.col;
        self.token_start_offset = self.pos;
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace. ASCII whitespace is one byte per column, so
            // scan bytes rather than decoding a char at a time.
            let bytes = self.source.as_bytes();
            while let Some(&byte) = bytes.get(self.pos)
                && byte.is_ascii_whitespace()
            {
                self.pos += 1;
                if byte == b'\n' {
                    self.line += 1;
                    self.col = 1;
                } else {
                    self.col += 1;
                }
            }
            // Skip line comments, up to (not including) the newline
            if self.source[self.pos..].starts_with("//") {
                let rest = &self.source[self.pos..];
                let len = rest.find('\n').unwrap_or(rest.len());
                self.col += rest[..len].chars().count();
                self.pos += len;
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
        // Recursive interpolation lexing updates the shared token-start fields.
        // Restore this string's span before returning to its enclosing tokenizer.
        let token_start = (
            self.token_start_line,
            self.token_start_col,
            self.token_start_offset,
        );
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
                            if self.interpolation_depth >= MAX_INTERPOLATION_DEPTH {
                                return Err(self.interpolation_depth_error());
                            }
                            self.interpolation_depth += 1;
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
                                    line: self.token_start_line,
                                    col: self.token_start_col,
                                    offset: self.token_start_offset,
                                    end: self.pos,
                                });
                            }
                            // Add Eof token so the parser knows when to stop
                            expr_tokens.push(Token {
                                kind: TokenKind::Eof,
                                line: self.line,
                                col: self.col,
                                offset: self.pos,
                                end: self.pos,
                            });
                            self.interpolation_depth -= 1;
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
        let token = if has_interpolation {
            parts.push(StringPart::Literal(current));
            TokenKind::InterpolatedString(parts)
        } else {
            TokenKind::StringLit(current)
        };
        (
            self.token_start_line,
            self.token_start_col,
            self.token_start_offset,
        ) = token_start;
        Ok(token)
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
        normalize_multiline_string(&s)
    }

    fn read_raw_multiline_string(&mut self, hash_count: usize) -> Result<String> {
        // Already consumed the opening hashes and `"""`
        // Read until the matching closing `"""` plus the same number of hashes.
        let closing = format!("\"\"\"{}", "#".repeat(hash_count));
        let mut s = String::new();
        loop {
            if self.source[self.pos..].starts_with(&closing) {
                for _ in 0..closing.chars().count() {
                    self.advance();
                }
                break;
            }
            match self.advance() {
                None => {
                    return Err(self.lex_error("unterminated raw multiline string"));
                }
                Some(c) => s.push(c),
            }
        }
        normalize_multiline_string(&s)
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
            self.mark_token_start();

            let ch = match self.peek() {
                None => {
                    tokens.push(Token {
                        kind: TokenKind::Eof,
                        line: self.token_start_line,
                        col: self.token_start_col,
                        offset: self.token_start_offset,
                        end: self.token_start_offset,
                    });
                    break;
                }
                Some(c) => c,
            };

            let kind = self.read_one_token_from(ch)?;

            tokens.push(Token {
                kind,
                line: self.token_start_line,
                col: self.token_start_col,
                offset: self.token_start_offset,
                end: self.pos,
            });
        }
        Ok(tokens)
    }

    fn read_one_token(&mut self) -> Result<TokenKind> {
        self.skip_whitespace_and_comments();
        self.mark_token_start();
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
            ';' => {
                self.advance();
                TokenKind::Semicolon
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
                // #"..."# and #"""..."""# raw strings. Pkl allows multiple hashes.
                let mut hash_count = 0;
                while self.peek() == Some('#') {
                    self.advance();
                    hash_count += 1;
                }
                if self.source[self.pos..].starts_with("\"\"\"") {
                    self.advance();
                    self.advance();
                    self.advance();
                    let s = self.read_raw_multiline_string(hash_count)?;
                    TokenKind::StringLit(s)
                } else if self.peek() == Some('"') {
                    self.advance();
                    let s = self.read_raw_string(hash_count)?;
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
                } else if ident == "read" && self.peek() == Some('*') {
                    self.advance();
                    TokenKind::KwReadGlob
                } else {
                    keyword_or_ident(ident)
                }
            }
            '`' => {
                self.advance();
                let start = self.pos;
                while self.peek().is_some_and(|c| c != '`') {
                    self.advance();
                }
                if self.peek() != Some('`') {
                    return Err(self.lex_error("unterminated quoted identifier"));
                }
                let ident = self.source[start..self.pos].to_string();
                self.advance();
                if ident.is_empty() {
                    return Err(self.lex_error("empty quoted identifier"));
                }
                TokenKind::Ident(ident)
            }
            c => {
                return Err(self.lex_error(format!("unexpected character: {c:?}")));
            }
        };
        Ok(kind)
    }

    fn read_raw_string(&mut self, hash_count: usize) -> Result<String> {
        // Read until the closing quote plus the same number of hashes.
        let closing = format!("\"{}", "#".repeat(hash_count));
        let mut s = String::new();
        loop {
            if self.source[self.pos..].starts_with(&closing) {
                for _ in 0..closing.chars().count() {
                    self.advance();
                }
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

fn normalize_multiline_string(s: &str) -> Result<String> {
    let s = s
        .strip_prefix("\r\n")
        .or_else(|| s.strip_prefix('\n'))
        .unwrap_or(s);
    dedent(s)
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
        "read*" => TokenKind::KwReadGlob,
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
