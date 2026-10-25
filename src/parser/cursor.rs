//! Moving through the token stream, and the errors the parser reports.

use super::*;

impl<'a> Parser<'a> {
    pub(super) fn new(tokens: &'a [Token], source: &'a str, name: &'a str) -> Self {
        let first = tokens.first();
        let mut p = Self {
            tokens,
            source,
            name,
            pos: 0,
            import_exprs: Vec::new(),
            prev_end: first.map_or(0, |t| t.offset),
            prev_line: first.map_or(1, |t| t.line),
            amending: false,
            new_types: Vec::new(),
            type_alias_parameters: std::collections::HashMap::new(),
            local_type_aliases: std::collections::HashSet::new(),
            module_local_type_aliases: std::collections::HashSet::new(),
            depth: 0,
        };
        p.skip_semicolons();
        p
    }

    pub(super) fn skip_semicolons(&mut self) {
        while self.pos + 1 < self.tokens.len()
            && matches!(self.tokens[self.pos].kind, TokenKind::Semicolon)
        {
            self.pos += 1;
        }
    }

    pub(super) fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    pub(super) fn peek_tok(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// The kind of the `n`th token after the current one, skipping semicolons.
    pub(super) fn peek_nth(&self, n: usize) -> &TokenKind {
        let mut i = self.pos;
        let mut remaining = n;
        while remaining > 0 && i + 1 < self.tokens.len() {
            i += 1;
            if !matches!(self.tokens[i].kind, TokenKind::Semicolon) {
                remaining -= 1;
            }
        }
        &self.tokens[i].kind
    }

    pub(super) fn advance(&mut self) -> &'a Token {
        let tokens = self.tokens;
        let tok = &tokens[self.pos];
        self.prev_end = tok.end;
        self.prev_line = tok.line;
        if self.pos + 1 < tokens.len() {
            self.pos += 1;
            self.skip_semicolons();
        }
        tok
    }

    pub(super) fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    pub(super) fn preceded_by_semicolon(&self) -> bool {
        self.pos > 0 && matches!(self.tokens[self.pos - 1].kind, TokenKind::Semicolon)
    }

    /// Whether a newline separates the current token from the previous one.
    pub(super) fn newline_before(&self) -> bool {
        let tok = self.peek_tok();
        match self.source.get(self.prev_end..tok.offset) {
            Some(gap) => gap.contains('\n'),
            None => tok.line != self.prev_line,
        }
    }

    /// Whether the current token continues the previous token's line: pkl
    /// only treats `(`, `[` and `-` as continuing an expression on the same
    /// line, without a semicolon in between.
    pub(super) fn on_same_line(&self) -> bool {
        !self.newline_before() && !self.preceded_by_semicolon()
    }

    /// Source text of a token, for error messages.
    pub(super) fn token_text(&self, tok: &Token) -> String {
        if matches!(tok.kind, TokenKind::Eof) {
            return "EOF".to_string();
        }
        match self.source.get(tok.offset..tok.end) {
            Some(text) if !text.is_empty() => text.to_string(),
            _ => format!("{:?}", tok.kind),
        }
    }

    /// Whether the current token was written as a backquoted identifier.
    pub(super) fn is_quoted(&self, tok: &Token) -> bool {
        self.source.as_bytes().get(tok.offset) == Some(&b'`')
    }

    /// The keyword the current token spells, if it can't be used as a name.
    pub(super) fn keyword_at(&self, tok: &Token) -> Option<String> {
        match &tok.kind {
            TokenKind::Ident(name) => (!self.is_quoted(tok)
                && RESERVED_WORDS.contains(&name.as_str()))
            .then(|| name.clone()),
            other => keyword_text(other).map(String::from),
        }
    }

    pub(super) fn error_at(&self, offset: usize, message: impl Into<String>) -> Error {
        Error::parse(self.name, self.source, offset, message.into())
    }

    pub(super) fn parse_error(&self, message: impl Into<String>) -> Error {
        self.error_at(self.peek_tok().offset, message)
    }

    /// Error offset for a missing token: the end of the previous token when
    /// the current one is on another line or is the end of the file.
    pub(super) fn missing_offset(&self) -> usize {
        if self.at_eof() || self.newline_before() {
            self.prev_end
        } else {
            self.peek_tok().offset
        }
    }

    pub(super) fn unexpected_token(&self, expected: &str) -> Error {
        let text = self.token_text(self.peek_tok());
        self.error_at(
            self.missing_offset(),
            format!("Unexpected token `{text}`. Expected `{expected}`."),
        )
    }

    pub(super) fn unexpected_token2(&self, expected1: &str, expected2: &str) -> Error {
        let text = self.token_text(self.peek_tok());
        self.error_at(
            self.missing_offset(),
            format!("Unexpected token `{text}`. Expected `{expected1}` or `{expected2}`."),
        )
    }

    pub(super) fn expect(&mut self, kind: &TokenKind, text: &str) -> Result<&'a Token> {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            Ok(self.advance())
        } else {
            Err(self.unexpected_token(text))
        }
    }

    pub(super) fn expect2(
        &mut self,
        kind: &TokenKind,
        text: &str,
        alternative: &str,
    ) -> Result<&'a Token> {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            Ok(self.advance())
        } else {
            Err(self.unexpected_token2(alternative, text))
        }
    }

    pub(super) fn keyword_error(&self, keyword: &str) -> Error {
        self.parse_error(format!(
            "Keyword `{keyword}` is not allowed here.\n\n\
             If you must use this name as identifier, enclose it in backticks."
        ))
    }

    pub(super) fn expect_ident(&mut self) -> Result<String> {
        let tok = self.peek_tok();
        if let Some(keyword) = self.keyword_at(tok) {
            return Err(self.keyword_error(&keyword));
        }
        match &tok.kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(self.unexpected_token("identifier")),
        }
    }

    /// `outer` is reserved as a declaration name, but remains valid as a
    /// member selected from an outer receiver (`outer.outer`).
    pub(super) fn expect_field_ident(&mut self) -> Result<String> {
        if matches!(self.peek(), TokenKind::Ident(name) if name == "outer")
            && !self.is_quoted(self.peek_tok())
        {
            self.advance();
            return Ok("outer".to_string());
        }
        self.expect_ident()
    }

    pub(super) fn expect_string_constant(&mut self) -> Result<String> {
        match self.peek() {
            TokenKind::StringLit(s) => {
                let s = s.clone();
                self.advance();
                Ok(s)
            }
            TokenKind::InterpolatedString(_) => {
                Err(self.parse_error("String constant cannot have interpolated values."))
            }
            _ => Err(self.unexpected_token("\"")),
        }
    }

    pub(super) fn parse_qualified_ident(&mut self) -> Result<String> {
        let mut name = self.expect_ident()?;
        while matches!(self.peek(), TokenKind::Dot) {
            self.advance();
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        Ok(name)
    }
}

/// Words pkl reserves as keywords that pklr lexes as identifiers. They can
/// only be used as names when quoted with backticks.
pub(super) const RESERVED_WORDS: &[&str] = &[
    "_",
    "case",
    "delete",
    "nothing",
    "out",
    "outer",
    "override",
    "protected",
    "record",
    "switch",
    "unknown",
    "vararg",
];

pub(super) fn keyword_text(kind: &TokenKind) -> Option<&'static str> {
    Some(match kind {
        TokenKind::KwAmends => "amends",
        TokenKind::KwImport => "import",
        TokenKind::KwAs => "as",
        TokenKind::KwLocal => "local",
        TokenKind::KwConst => "const",
        TokenKind::KwFixed => "fixed",
        TokenKind::KwHidden => "hidden",
        TokenKind::KwNew => "new",
        TokenKind::KwExtends => "extends",
        TokenKind::KwAbstract => "abstract",
        TokenKind::KwOpen => "open",
        TokenKind::KwExternal => "external",
        TokenKind::KwClass => "class",
        TokenKind::KwTypeAlias => "typealias",
        TokenKind::KwFunction => "function",
        TokenKind::KwThis => "this",
        TokenKind::KwSuper => "super",
        TokenKind::KwModule => "module",
        TokenKind::KwImportStar => "import*",
        TokenKind::KwIf => "if",
        TokenKind::KwElse => "else",
        TokenKind::KwWhen => "when",
        TokenKind::KwIs => "is",
        TokenKind::KwLet => "let",
        TokenKind::KwThrow => "throw",
        TokenKind::KwTrace => "trace",
        TokenKind::KwRead => "read",
        TokenKind::KwReadOrNull => "read?",
        TokenKind::KwReadGlob => "read*",
        TokenKind::KwFor => "for",
        TokenKind::KwIn => "in",
        TokenKind::BoolLit(true) => "true",
        TokenKind::BoolLit(false) => "false",
        TokenKind::Null => "null",
        _ => return None,
    })
}
