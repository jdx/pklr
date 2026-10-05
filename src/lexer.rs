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
    /// String interpolations currently open around the lexer's position.
    interpolation_depth: usize,
}

/// The most string interpolations that may nest inside each other. The lexer
/// recurses for each one, so the limit keeps hostile input from overflowing
/// the stack.
const MAX_INTERPOLATION_DEPTH: usize = crate::parser::MAX_NESTING_DEPTH;

/// One piece of a multi-line string before indentation is stripped.
enum Piece<'a> {
    Newline,
    /// Raw characters of (part of) a line, and their byte offset.
    Text(&'a str, usize),
    /// A decoded escape sequence, and its byte offset.
    Escape(String, usize),
    /// `\` followed by a newline: joins two lines.
    Continuation(usize),
    Interp(Vec<Token>, usize),
}

/// Accumulates string literal parts, keeping `StringPart`s alternating
/// between literals and interpolations.
#[derive(Default)]
struct StringBuilder {
    parts: Vec<StringPart>,
    current: String,
}

impl StringBuilder {
    fn push_interp(&mut self, tokens: Vec<Token>) {
        self.parts
            .push(StringPart::Literal(std::mem::take(&mut self.current)));
        self.parts.push(StringPart::Tokens(tokens));
    }

    fn finish(mut self) -> TokenKind {
        if self.parts.is_empty() {
            TokenKind::StringLit(self.current)
        } else {
            self.parts.push(StringPart::Literal(self.current));
            TokenKind::InterpolatedString(self.parts)
        }
    }
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, name: &str) -> Self {
        Self {
            source,
            name: name.to_string(),
            pos: 0,
            line: 1,
            col: 1,
            interpolation_depth: 0,
        }
    }

    fn lex_error(&self, message: impl Into<String>) -> Error {
        self.lex_error_at(self.pos, message)
    }

    fn lex_error_at(&self, offset: usize, message: impl Into<String>) -> Error {
        Error::lex(&self.name, self.source, offset, message.into())
    }

    fn rest(&self) -> &'a str {
        &self.source[self.pos..]
    }

    #[cold]
    #[inline(never)]
    fn interpolation_depth_error(&self) -> Error {
        self.lex_error(format!(
            "string interpolations nest more than {MAX_INTERPOLATION_DEPTH} levels deep"
        ))
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
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

    /// Advance past `n` bytes of ASCII text known not to contain a newline.
    fn advance_ascii(&mut self, n: usize) {
        self.pos += n;
        self.col += n;
    }

    /// Skip whitespace and comments other than doc comments.
    fn skip_whitespace_and_comments(&mut self) -> Result<()> {
        loop {
            // Skip whitespace. ASCII whitespace is one byte per column, so
            // scan bytes rather than decoding a char at a time.
            let bytes = self.source.as_bytes();
            while let Some(&byte) = bytes.get(self.pos)
                && matches!(byte, b' ' | b'\n' | b'\t' | b'\r' | b'\x0c')
            {
                self.pos += 1;
                if byte == b'\n' {
                    self.line += 1;
                    self.col = 1;
                } else {
                    self.col += 1;
                }
            }
            let rest = self.rest();
            // Skip line comments, up to (not including) the newline.
            // `///` starts a doc comment, which is a token.
            if rest.starts_with("//") && !rest.starts_with("///") {
                let len = rest.find(['\n', '\r']).unwrap_or(rest.len());
                self.col += rest[..len].chars().count();
                self.pos += len;
                continue;
            }
            if rest.starts_with("/*") {
                let start = self.pos;
                self.advance_ascii(2);
                loop {
                    if self.rest().starts_with("*/") {
                        self.advance_ascii(2);
                        break;
                    }
                    if self.advance().is_none() {
                        return Err(self.lex_error_at(start, "Unexpected end of file."));
                    }
                }
                continue;
            }
            return Ok(());
        }
    }

    fn tokenize(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        if self.rest().starts_with("#!") {
            // Shebang line
            let len = self.rest().find('\n').unwrap_or(self.rest().len());
            self.col += self.rest()[..len].chars().count();
            self.pos += len;
        }
        loop {
            let token = self.next_token()?;
            let eof = matches!(token.kind, TokenKind::Eof);
            tokens.push(token);
            if eof {
                break;
            }
        }
        Ok(tokens)
    }

    /// Lex the next token, skipping whitespace and comments first.
    fn next_token(&mut self) -> Result<Token> {
        self.skip_whitespace_and_comments()?;
        let line = self.line;
        let col = self.col;
        let offset = self.pos;
        let kind = match self.peek() {
            None => TokenKind::Eof,
            Some(ch) => self.read_token_from(ch)?,
        };
        Ok(Token {
            kind,
            line,
            col,
            offset,
            end: self.pos,
        })
    }

    fn read_token_from(&mut self, ch: char) -> Result<TokenKind> {
        // Only compared with ASCII punctuation, so a byte is enough.
        let next = self
            .source
            .as_bytes()
            .get(self.pos + ch.len_utf8())
            .map(|&b| b as char);
        // Fixed-width punctuation: (token, byte length)
        let (kind, len) = match (ch, next) {
            ('{', _) => (TokenKind::LBrace, 1),
            ('}', _) => (TokenKind::RBrace, 1),
            ('(', _) => (TokenKind::LParen, 1),
            (')', _) => (TokenKind::RParen, 1),
            ('[', Some('[')) => (TokenKind::LPred, 2),
            ('[', _) => (TokenKind::LBracket, 1),
            (']', _) => (TokenKind::RBracket, 1),
            (',', _) => (TokenKind::Comma, 1),
            (';', _) => (TokenKind::Semicolon, 1),
            ('=', Some('=')) => (TokenKind::EqEq, 2),
            ('=', Some('>')) => (TokenKind::ThinArrow, 2),
            ('=', _) => (TokenKind::Equals, 1),
            (':', _) => (TokenKind::Colon, 1),
            ('?', Some('?')) => (TokenKind::QuestionQuestion, 2),
            ('?', Some('.')) => (TokenKind::QuestionDot, 2),
            ('?', _) => (TokenKind::QuestionMark, 1),
            ('!', Some('=')) => (TokenKind::BangEq, 2),
            ('!', Some('!')) => (TokenKind::BangBang, 2),
            ('!', _) => (TokenKind::Bang, 1),
            ('|', Some('|')) => (TokenKind::PipePipe, 2),
            ('|', Some('>')) => (TokenKind::PipeGt, 2),
            ('|', _) => (TokenKind::Pipe, 1),
            ('^', _) => (TokenKind::Caret, 1),
            ('@', _) => (TokenKind::At, 1),
            ('+', _) => (TokenKind::Plus, 1),
            ('-', Some('>')) => (TokenKind::Arrow, 2),
            ('-', _) => (TokenKind::Minus, 1),
            ('*', Some('*')) => (TokenKind::StarStar, 2),
            ('*', _) => (TokenKind::Star, 1),
            ('/', Some('/')) => {
                // Doc comment (`///`); plain comments were skipped already.
                // The token's span covers the comment text.
                let rest = self.rest();
                let len = rest.find(['\n', '\r']).unwrap_or(rest.len());
                self.col += rest[..len].chars().count();
                self.pos += len;
                return Ok(TokenKind::DocComment);
            }
            ('/', _) => (TokenKind::Slash, 1),
            ('%', _) => (TokenKind::Percent, 1),
            ('<', Some('=')) => (TokenKind::LtEq, 2),
            ('<', _) => (TokenKind::Lt, 1),
            ('>', Some('=')) => (TokenKind::GtEq, 2),
            ('>', _) => (TokenKind::Gt, 1),
            ('&', Some('&')) => (TokenKind::AmpAmp, 2),
            ('&', _) => {
                return Err(self.lex_error("Unexpected character `&`. Did you mean `&&`?"));
            }
            ('~', Some('/')) => (TokenKind::TildeSlash, 2),
            ('~', _) => {
                return Err(self.lex_error("Unexpected character `~`. Did you mean `~/`?"));
            }
            ('.', Some('.')) => {
                if self.rest().starts_with("...?") {
                    (TokenKind::QuestionDotDotDot, 4)
                } else if self.rest().starts_with("...") {
                    (TokenKind::DotDotDot, 3)
                } else {
                    return Err(self.lex_error(
                        "Unexpected character `..`. Did you mean `.`, `...` or `...?`?",
                    ));
                }
            }
            ('.', Some(c)) if c.is_ascii_digit() => {
                return self.read_number();
            }
            ('.', _) => (TokenKind::Dot, 1),
            ('"', _) => {
                self.advance_ascii(1);
                return self.read_string(0);
            }
            ('#', _) => {
                let start = self.pos;
                let mut pounds = 0;
                while self.peek() == Some('#') {
                    self.advance_ascii(1);
                    pounds += 1;
                }
                if self.peek() != Some('"') {
                    return Err(self.lex_error_at(
                        start,
                        format!(
                            "Unexpected character `{}`. Did you mean `\"`?",
                            self.peek().map(String::from).unwrap_or("EOF".into())
                        ),
                    ));
                }
                self.advance_ascii(1);
                return self.read_string(pounds);
            }
            ('`', _) => return self.read_quoted_ident(),
            (c, _) if c.is_ascii_digit() => return self.read_number(),
            (c, _) if is_identifier_start(c) => return Ok(self.read_ident()),
            (c, _) => {
                return Err(self.lex_error(format!("Invalid character `{c}`.")));
            }
        };
        self.advance_ascii(len);
        Ok(kind)
    }

    fn read_ident(&mut self) -> TokenKind {
        let start = self.pos;
        self.advance();
        while self.peek().is_some_and(is_identifier_part) {
            self.advance();
        }
        let ident = &self.source[start..self.pos];
        // `import*`, `read*` and `read?` are single tokens
        match (ident, self.peek()) {
            ("import", Some('*')) => {
                self.advance_ascii(1);
                TokenKind::KwImportStar
            }
            ("read", Some('*')) => {
                self.advance_ascii(1);
                TokenKind::KwReadGlob
            }
            ("read", Some('?')) => {
                self.advance_ascii(1);
                TokenKind::KwReadOrNull
            }
            _ => keyword_or_ident(ident),
        }
    }

    fn read_quoted_ident(&mut self) -> Result<TokenKind> {
        let start = self.pos;
        self.advance_ascii(1);
        let ident_start = self.pos;
        while self.peek().is_some_and(|c| !matches!(c, '`' | '\n' | '\r')) {
            self.advance();
        }
        if self.peek() != Some('`') {
            return Err(self.lex_error_at(start, "Unterminated quoted identifier."));
        }
        let ident = self.source[ident_start..self.pos].to_string();
        self.advance_ascii(1);
        if ident.is_empty() {
            return Err(self.lex_error_at(start, "Empty quoted identifier."));
        }
        Ok(TokenKind::Ident(ident))
    }

    fn consume_digits(&mut self, is_digit: impl Fn(char) -> bool) {
        while self.peek().is_some_and(|c| is_digit(c) || c == '_') {
            self.advance_ascii(1);
        }
    }

    fn separator_error(&self) -> Error {
        self.lex_error(
            "Unexpected separator character.\n\nThe separator character (`_`) cannot follow \
             `0x`, `0b`, `.`, `e`, or 'E' in a number literal.",
        )
    }

    /// Lex the digits of a radix-prefixed literal after `0x`/`0b`/`0o`.
    fn read_radix_digits(&mut self, radix: u32, what: &str) -> Result<()> {
        match self.peek() {
            Some('_') => Err(self.separator_error()),
            Some(c) if c.is_digit(radix) => {
                self.consume_digits(|c| c.is_digit(radix));
                Ok(())
            }
            other => Err(self.lex_error(format!(
                "Unexpected character `{}`. Did you mean {what}?",
                other.map(String::from).unwrap_or("EOF".into())
            ))),
        }
    }

    /// Lex the exponent of a float literal; `e`/`E` is already consumed.
    fn read_exponent(&mut self) -> Result<()> {
        if matches!(self.peek(), Some('+' | '-')) {
            self.advance_ascii(1);
        }
        match self.peek() {
            Some('_') => Err(self.separator_error()),
            Some(c) if c.is_ascii_digit() => {
                self.consume_digits(|c| c.is_ascii_digit());
                Ok(())
            }
            other => Err(self.lex_error(format!(
                "Unexpected character `{}`. Did you mean number?",
                other.map(String::from).unwrap_or("EOF".into())
            ))),
        }
    }

    /// Lex the fraction (and optional exponent) of a float literal; `.` is
    /// already consumed.
    fn read_fraction(&mut self) -> Result<()> {
        if self.peek() == Some('_') {
            return Err(self.separator_error());
        }
        self.consume_digits(|c| c.is_ascii_digit());
        if matches!(self.peek(), Some('e' | 'E')) {
            self.advance_ascii(1);
            self.read_exponent()?;
        }
        Ok(())
    }

    fn read_number(&mut self) -> Result<TokenKind> {
        let start = self.pos;
        let first = self.advance().expect("caller saw a digit or `.`");
        let mut is_float = false;
        let mut radix = 10;
        if first == '.' {
            self.read_fraction()?;
            is_float = true;
        } else if first == '0' && matches!(self.peek(), Some('x' | 'X')) {
            self.advance_ascii(1);
            self.read_radix_digits(16, "hexadecimal number")?;
            radix = 16;
        } else if first == '0' && matches!(self.peek(), Some('b' | 'B')) {
            self.advance_ascii(1);
            self.read_radix_digits(2, "binary number")?;
            radix = 2;
        } else if first == '0' && matches!(self.peek(), Some('o' | 'O')) {
            self.advance_ascii(1);
            self.read_radix_digits(8, "octal number")?;
            radix = 8;
        } else {
            self.consume_digits(|c| c.is_ascii_digit());
            if matches!(self.peek(), Some('e' | 'E')) {
                self.advance_ascii(1);
                self.read_exponent()?;
                is_float = true;
            } else if self.peek() == Some('.') {
                match self.peek_nth(1) {
                    Some('_') => {
                        self.advance_ascii(1);
                        return Err(self.separator_error());
                    }
                    Some(c) if c.is_ascii_digit() => {
                        self.advance_ascii(1);
                        self.read_fraction()?;
                        is_float = true;
                    }
                    // `1.foo` is a member access on an Int
                    _ => {}
                }
            }
        }
        let raw = &self.source[start..self.pos];
        let cleaned = raw.replace('_', "");
        if is_float {
            let v: f64 = cleaned
                .parse()
                .map_err(|_| self.lex_error_at(start, format!("Invalid float literal `{raw}`.")))?;
            return Ok(TokenKind::FloatLit(v));
        }
        let digits = if radix == 10 {
            &cleaned[..]
        } else {
            &cleaned[2..]
        };
        // Parse as u64 so that `-9223372036854775808` can be lexed. Preserve
        // 2^63 as a distinct token so the parser accepts it only as the
        // operand of a unary minus, never after binary subtraction.
        match u64::from_str_radix(digits, radix) {
            Ok(v) if v <= i64::MAX as u64 => Ok(TokenKind::IntLit(v as i64)),
            Ok(v) if v == 1 << 63 => Ok(TokenKind::MinIntLit),
            _ => Err(self.lex_error_at(start, format!("Integer literal `{raw}` is too large."))),
        }
    }

    /// Lex a string literal. The opening pounds and first quote are consumed.
    fn read_string(&mut self, pounds: usize) -> Result<TokenKind> {
        let start = self.pos - 1 - pounds;
        if self.rest().starts_with("\"\"") {
            self.advance_ascii(2);
            return self.read_multiline_string(pounds, start);
        }
        let closing = delimiter("\"", pounds);
        let escape = delimiter("\\", pounds);
        let mut builder = StringBuilder::default();
        loop {
            let chunk = self.take_plain_chunk();
            if !chunk.is_empty() {
                builder.current.push_str(chunk);
                continue;
            }
            let rest = self.rest();
            if rest.starts_with(&*closing) {
                self.advance_ascii(closing.len());
                return Ok(builder.finish());
            }
            if rest.starts_with(&*escape) {
                let escape_start = self.pos;
                self.advance_ascii(escape.len());
                match self.read_escape(pounds, escape_start)? {
                    Escaped::Text(s) => builder.current.push_str(&s),
                    Escaped::Interp(tokens) => builder.push_interp(tokens),
                    Escaped::Continuation => {
                        return Err(self.lex_error_at(
                            escape_start,
                            "Invalid line continuation escape sequence.\n\n\
                             Line continuations are only allowed in multi-line strings.",
                        ));
                    }
                }
                continue;
            }
            match self.peek() {
                None | Some('\n' | '\r') => {
                    return Err(self.lex_error_at(start, format!("Missing `{closing}` delimiter.")));
                }
                Some(c) => {
                    self.advance();
                    builder.current.push(c);
                }
            }
        }
    }

    /// Consume string characters up to the next quote, backslash or line
    /// break, none of which can end the text or start an escape.
    fn take_plain_chunk(&mut self) -> &'a str {
        let rest = self.rest();
        let len = rest.find(['"', '\\', '\n', '\r']).unwrap_or(rest.len());
        let chunk = &rest[..len];
        self.col += chunk.chars().count();
        self.pos += len;
        chunk
    }

    fn read_multiline_string(&mut self, pounds: usize, start: usize) -> Result<TokenKind> {
        let closing = delimiter("\"\"\"", pounds);
        let escape = delimiter("\\", pounds);
        let source = self.source;
        let mut pieces = Vec::new();
        let mut text_start = self.pos;
        // Text is a contiguous run of source between escapes and newlines.
        let flush = |text_start: usize, end: usize, pieces: &mut Vec<Piece<'a>>| {
            if end > text_start {
                pieces.push(Piece::Text(&source[text_start..end], text_start));
            }
        };
        loop {
            if !self.take_plain_chunk().is_empty() {
                continue;
            }
            let rest = self.rest();
            if rest.starts_with(&*closing) {
                flush(text_start, self.pos, &mut pieces);
                self.advance_ascii(closing.len());
                break;
            }
            if rest.starts_with(&*escape) {
                flush(text_start, self.pos, &mut pieces);
                let escape_start = self.pos;
                self.advance_ascii(escape.len());
                pieces.push(match self.read_escape(pounds, escape_start)? {
                    Escaped::Text(s) => Piece::Escape(s, escape_start),
                    Escaped::Interp(tokens) => Piece::Interp(tokens, escape_start),
                    Escaped::Continuation => Piece::Continuation(escape_start),
                });
                text_start = self.pos;
                continue;
            }
            match self.peek() {
                None => {
                    return Err(self.lex_error_at(start, format!("Missing `{closing}` delimiter.")));
                }
                Some('\n' | '\r') => {
                    flush(text_start, self.pos, &mut pieces);
                    if self.advance() == Some('\r') && self.peek() == Some('\n') {
                        self.advance();
                    }
                    pieces.push(Piece::Newline);
                    text_start = self.pos;
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
        self.render_multiline_string(pieces, start)
    }

    /// Strip the closing delimiter's indentation from every line of a
    /// multi-line string, as pkl's parser does.
    fn render_multiline_string(&self, pieces: Vec<Piece<'a>>, start: usize) -> Result<TokenKind> {
        if !matches!(pieces.first(), Some(Piece::Newline)) {
            return Err(self.lex_error_at(
                start,
                "The content of a multi-line string must begin on a new line.",
            ));
        }
        if pieces.len() == 1 {
            return Ok(TokenKind::StringLit(String::new()));
        }
        let closing_error = || {
            self.lex_error_at(
                start,
                "The closing delimiter of a multi-line string must begin on a new line.",
            )
        };
        let (indent, end) = match &pieces[pieces.len() - 1] {
            Piece::Newline => ("", pieces.len() - 1),
            // A line continuation consumed the newline immediately before
            // the closing delimiter, but that delimiter is still on its own
            // source line.
            Piece::Continuation(_) => ("", pieces.len() - 1),
            Piece::Text(text, _)
                if matches!(
                    pieces[pieces.len() - 2],
                    Piece::Newline | Piece::Continuation(_)
                ) && text.chars().all(|c| c == ' ' || c == '\t') =>
            {
                (*text, pieces.len() - 2)
            }
            _ => return Err(closing_error()),
        };
        let indent_error = |offset: usize| {
            self.lex_error_at(
                offset,
                "Line must match or exceed indentation of the String's last line.",
            )
        };
        let mut builder = StringBuilder::default();
        let mut at_line_start = true;
        for piece in pieces.into_iter().take(end).skip(1) {
            match piece {
                Piece::Newline => {
                    builder.current.push('\n');
                    at_line_start = true;
                }
                // An escape can't start a line that must be indented.
                Piece::Continuation(offset)
                | Piece::Interp(_, offset)
                | Piece::Escape(_, offset)
                    if at_line_start && !indent.is_empty() =>
                {
                    return Err(indent_error(offset));
                }
                Piece::Continuation(_) => at_line_start = true,
                Piece::Text(text, offset) => {
                    if at_line_start {
                        let Some(stripped) = text.strip_prefix(indent) else {
                            let leading = text.len() - text.trim_start_matches([' ', '\t']).len();
                            return Err(indent_error(offset + leading));
                        };
                        builder.current.push_str(stripped);
                    } else {
                        builder.current.push_str(text);
                    }
                    at_line_start = false;
                }
                Piece::Escape(text, _) => {
                    builder.current.push_str(&text);
                    at_line_start = false;
                }
                Piece::Interp(tokens, _) => {
                    builder.push_interp(tokens);
                    at_line_start = false;
                }
            }
        }
        Ok(builder.finish())
    }

    /// Lex an escape sequence; the backslash and pounds are consumed.
    fn read_escape(&mut self, pounds: usize, escape_start: usize) -> Result<Escaped> {
        let Some(ch) = self.advance() else {
            return Err(self.lex_error("Unexpected end of file."));
        };
        let text = match ch {
            'n' => "\n",
            't' => "\t",
            'r' => "\r",
            '"' => "\"",
            '\\' => "\\",
            '(' => return self.read_interpolation().map(Escaped::Interp),
            'u' => {
                return self
                    .read_unicode_escape(pounds, escape_start)
                    .map(Escaped::Text);
            }
            '\n' => return Ok(Escaped::Continuation),
            '\r' => {
                if self.peek() == Some('\n') {
                    self.advance();
                }
                return Ok(Escaped::Continuation);
            }
            ' ' | '\t' => {
                let rest = self.rest();
                let trailing = rest.trim_start_matches([' ', '\t']);
                if trailing.starts_with(['\n', '\r']) {
                    return Err(self.lex_error_at(
                        escape_start,
                        "Invalid line continuation escape sequence.\n\n\
                         Whitespace between the continuation escape and following newline \
                         is not allowed.",
                    ));
                }
                return Err(self.lex_error_at(
                    escape_start,
                    format!("Invalid character escape sequence `\\{ch}`."),
                ));
            }
            other => {
                return Err(self.lex_error_at(
                    escape_start,
                    format!("Invalid character escape sequence `\\{other}`."),
                ));
            }
        };
        Ok(Escaped::Text(text.to_string()))
    }

    /// Lex `{XXXX}` after `\u`, returning the decoded text.
    fn read_unicode_escape(&mut self, pounds: usize, escape_start: usize) -> Result<String> {
        let code_point = self.read_code_point(escape_start)?;
        // pkl strings are UTF-16: a surrogate pair written as two escapes is
        // one character, and a lone surrogate renders as `?`.
        if (0xD800..0xDC00).contains(&code_point) {
            let next_escape = format!("\\{}u{{", "#".repeat(pounds));
            if self.rest().starts_with(&next_escape) {
                let saved = (self.pos, self.line, self.col);
                let low_start = self.pos;
                self.advance_ascii(next_escape.len() - 1);
                if let Ok(low) = self.read_code_point(low_start)
                    && (0xDC00..0xE000).contains(&low)
                {
                    let combined = 0x10000 + ((code_point - 0xD800) << 10) + (low - 0xDC00);
                    return Ok(char::from_u32(combined).unwrap_or('?').to_string());
                }
                (self.pos, self.line, self.col) = saved;
            }
        }
        if (0xD800..0xE000).contains(&code_point) {
            return Ok("?".to_string());
        }
        char::from_u32(code_point).map(String::from).ok_or_else(|| {
            self.lex_error_at(
                escape_start,
                format!(
                    "Invalid Unicode escape sequence `{}`.",
                    &self.source[escape_start..self.pos]
                ),
            )
        })
    }

    fn read_code_point(&mut self, escape_start: usize) -> Result<u32> {
        if self.peek() != Some('{') {
            return Err(self.lex_error(format!(
                "Unexpected character `{}`. Did you mean `{{`?",
                self.peek().map(String::from).unwrap_or("EOF".into())
            )));
        }
        self.advance_ascii(1);
        let digits_start = self.pos;
        while self.peek().is_some_and(|c| c.is_alphanumeric()) {
            self.advance();
        }
        if self.peek() != Some('}') {
            return Err(self.lex_error_at(
                escape_start,
                format!(
                    "Unterminated Unicode escape sequence `{}`.",
                    &self.source[escape_start..self.pos]
                ),
            ));
        }
        let digits = &self.source[digits_start..self.pos];
        self.advance_ascii(1);
        u32::from_str_radix(digits, 16).map_err(|_| {
            self.lex_error_at(
                escape_start,
                format!(
                    "Invalid Unicode escape sequence `{}`.",
                    &self.source[escape_start..self.pos]
                ),
            )
        })
    }

    /// Lex the tokens of a `\(...)` interpolation; `\(` is consumed. The
    /// returned tokens end with `Eof` in place of the closing `)`.
    fn read_interpolation(&mut self) -> Result<Vec<Token>> {
        if self.interpolation_depth >= MAX_INTERPOLATION_DEPTH {
            return Err(self.interpolation_depth_error());
        }
        self.interpolation_depth += 1;
        let result = (|| {
            let mut depth = 1;
            let mut tokens = Vec::new();
            loop {
                let token = self.next_token()?;
                match token.kind {
                    TokenKind::Eof => {
                        return Err(self.lex_error("Unexpected end of file."));
                    }
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            tokens.push(Token {
                                kind: TokenKind::Eof,
                                ..token
                            });
                            return Ok(tokens);
                        }
                    }
                    _ => {}
                }
                tokens.push(token);
            }
        })();
        self.interpolation_depth -= 1;
        result
    }
}

/// `prefix` followed by `pounds` `#` characters, without allocating for
/// the common case of no pounds.
fn delimiter(prefix: &'static str, pounds: usize) -> std::borrow::Cow<'static, str> {
    if pounds == 0 {
        prefix.into()
    } else {
        format!("{prefix}{}", "#".repeat(pounds)).into()
    }
}

enum Escaped {
    Text(String),
    Interp(Vec<Token>),
    Continuation,
}

fn is_identifier_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

fn is_identifier_part(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    fn string(src: &str) -> TokenKind {
        kinds(src).into_iter().next().unwrap()
    }

    fn lex_err(src: &str) -> String {
        lex(src).unwrap_err().to_string()
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
        let toks = kinds("42 1.23 0xFF .5 1e3 1_000 0b1_0 0o17");
        assert_eq!(
            toks,
            vec![
                TokenKind::IntLit(42),
                TokenKind::FloatLit(1.23),
                TokenKind::IntLit(255),
                TokenKind::FloatLit(0.5),
                TokenKind::FloatLit(1000.0),
                TokenKind::IntLit(1000),
                TokenKind::IntLit(2),
                TokenKind::IntLit(15),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_int_member_access() {
        assert_eq!(
            kinds("1.abs"),
            vec![
                TokenKind::IntLit(1),
                TokenKind::Dot,
                TokenKind::Ident("abs".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_number_errors() {
        assert!(lex_err("0x_01").contains("separator"));
        assert!(lex_err("0b_01").contains("separator"));
        assert!(lex_err("1._5").contains("separator"));
        assert!(lex_err("1e_5").contains("separator"));
        assert!(lex_err("9223372036854775809").contains("too large"));
        assert_eq!(kinds("9223372036854775808")[0], TokenKind::MinIntLit);
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

    #[test]
    fn test_doc_comment() {
        let toks = kinds("/// doc\nfoo");
        assert_eq!(
            toks,
            vec![
                TokenKind::DocComment,
                TokenKind::Ident("foo".into()),
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn test_token_end() {
        let toks = lex("foo = \"bar\"").unwrap();
        assert_eq!((toks[0].offset, toks[0].end), (0, 3));
        assert_eq!((toks[2].offset, toks[2].end), (6, 11));
    }

    #[test]
    fn test_spread_and_predicate_tokens() {
        assert_eq!(
            kinds("...?x [[y]]"),
            vec![
                TokenKind::QuestionDotDotDot,
                TokenKind::Ident("x".into()),
                TokenKind::LPred,
                TokenKind::Ident("y".into()),
                TokenKind::RBracket,
                TokenKind::RBracket,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn test_identifiers() {
        assert_eq!(string("$foo"), TokenKind::Ident("$foo".into()));
        assert_eq!(string("日本語"), TokenKind::Ident("日本語".into()));
        assert_eq!(string("`a b`"), TokenKind::Ident("a b".into()));
        assert_eq!(string("read*"), TokenKind::KwReadGlob);
    }

    #[test]
    fn test_string_escapes() {
        assert_eq!(
            string(r#""a\n\t\"\\\u{41}""#),
            TokenKind::StringLit("a\n\t\"\\A".into())
        );
        assert_eq!(
            string(r#""\u{D83D}\u{DE00}""#),
            TokenKind::StringLit("\u{1F600}".into())
        );
        assert_eq!(string(r#""\u{D800}h""#), TokenKind::StringLit("?h".into()));
        assert!(lex_err(r#""\a""#).contains("Invalid character escape"));
        assert!(lex_err("\"abc\n\"").contains("Missing"));
        assert!(lex_err("\"abc \\\ndef\"").contains("continuation"));
    }

    #[test]
    fn test_raw_strings() {
        assert_eq!(
            string(r##"#"a\n"b\#n"#"##),
            TokenKind::StringLit("a\\n\"b\n".into())
        );
        assert_eq!(
            string(r###"##"\#(x)"##"###),
            TokenKind::StringLit("\\#(x)".into())
        );
        let TokenKind::InterpolatedString(parts) = string(r##"#"a\#(x)b"#"##) else {
            panic!("expected interpolation");
        };
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], StringPart::Literal("a".into()));
        assert_eq!(parts[2], StringPart::Literal("b".into()));
    }

    #[test]
    fn test_multiline_strings() {
        assert_eq!(
            string("\"\"\"\n  a\n    b\n  \"\"\""),
            TokenKind::StringLit("a\n  b".into())
        );
        assert_eq!(
            string("\"\"\"\n  a \\\n  b\\t\n  \"\"\""),
            TokenKind::StringLit("a b\t".into())
        );
        assert_eq!(string("\"\"\"\n\"\"\""), TokenKind::StringLit("".into()));
        assert!(matches!(
            string("\"\"\"\n  a \\(x)\n  \"\"\""),
            TokenKind::InterpolatedString(_)
        ));
        assert!(lex_err("\"\"\"\n a\n  \"\"\"").contains("indentation"));
        assert!(lex_err("\"\"\"a\n\"\"\"").contains("new line"));
        assert!(lex_err("\"\"\"\na\"\"\"").contains("new line"));
        assert!(lex_err("\"\"\"\n a \\  \n \"\"\"").contains("Whitespace"));
    }
}
