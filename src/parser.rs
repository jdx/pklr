use crate::error::{Error, Result};
use crate::lexer::{StringPart, Token, TokenKind};

mod ast;
mod validate;

pub use ast::{
    Annotation, BinOp, Body, Entry, Expr, ForGenerator, Import, Modifier, Module, Property,
    StringInterpPart, TraceSite, TypeExpr, UnOp, WhenGenerator,
};
pub(crate) use ast::{has_untyped_result_new, infer_method_return_new, rewrite_untyped_result_new};
use ast::{infers_new, type_expr_runtime_name};

/// Collect all import URIs from a token stream (fast path, no full parse needed).
pub fn collect_imports(tokens: &[Token]) -> Vec<String> {
    collect_imports_with_kind(tokens)
        .into_iter()
        .map(|(uri, _)| uri)
        .collect()
}

/// Like [`collect_imports`], also telling whether each URI comes from a glob
/// import (`import*`), which evaluation expands rather than loads.
pub(crate) fn collect_imports_with_kind(tokens: &[Token]) -> Vec<(String, bool)> {
    let mut imports = Vec::new();
    collect_imports_into(tokens, &mut imports);
    imports
}

fn collect_imports_into(tokens: &[Token], imports: &mut Vec<(String, bool)>) {
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i].kind {
            TokenKind::KwAmends
            | TokenKind::KwExtends
            | TokenKind::KwImport
            | TokenKind::KwImportStar => {
                let is_glob = matches!(tokens[i].kind, TokenKind::KwImportStar);
                match tokens.get(i + 1).map(|t| &t.kind) {
                    // Declaration form: `import "uri"` / `import* "glob"` / `amends "uri"`
                    Some(TokenKind::StringLit(uri)) => {
                        imports.push((uri.clone(), is_glob));
                        i += 2;
                    }
                    // Expression form: `import("uri")` / `import*("glob")`
                    Some(TokenKind::LParen) => {
                        if let Some(TokenKind::StringLit(uri)) = tokens.get(i + 2).map(|t| &t.kind)
                        {
                            imports.push((uri.clone(), is_glob));
                        }
                        i += 2;
                    }
                    _ => i += 2,
                }
            }
            // An `import(...)` expression inside `"\(...)"` is lexed into the
            // interpolation's own token list, not the flat one scanned here.
            TokenKind::InterpolatedString(parts) => {
                for part in parts {
                    if let StringPart::Tokens(nested) = part {
                        collect_imports_into(nested, imports);
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
}

pub fn parse(tokens: &[Token]) -> Result<Module> {
    parse_named(tokens, "", "<input>")
}

pub fn parse_named(tokens: &[Token], source: &str, name: &str) -> Result<Module> {
    let mut p = Parser::new(tokens, source, name);
    p.parse_module()
}

pub fn parse_expr_tokens(tokens: &[Token], source: &str, name: &str) -> Result<Expr> {
    let mut p = Parser::new(tokens, source, name);
    let expr = p.parse_expr()?;
    if !p.at_eof() {
        return Err(p.unexpected_token("end of file"));
    }
    Ok(expr)
}

#[cfg(feature = "eval-core")]
pub(crate) fn parse_type_name(name: &str) -> Result<TypeExpr> {
    let tokens = crate::lexer::lex(name)?;
    Parser::new(&tokens, name, "<type>").parse_type()
}

/// The shared parser and lexer nesting limit.
pub const MAX_NESTING_DEPTH: usize = 128;

/// Words pkl reserves as keywords that pklr lexes as identifiers. They can
/// only be used as names when quoted with backticks.
const RESERVED_WORDS: &[&str] = &[
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

/// The doc comment, annotations and modifiers in front of a member.
#[derive(Default)]
struct MemberHeader {
    /// Offset of the first doc comment line
    doc_comment: Option<usize>,
    annotations: Vec<Annotation>,
    /// Offset of the first annotation
    annotations_start: Option<usize>,
    /// Modifiers and their offsets
    modifiers: Vec<(Modifier, usize)>,
}

impl MemberHeader {
    fn is_empty(&self) -> bool {
        self.doc_comment.is_none() && self.annotations.is_empty() && self.modifiers.is_empty()
    }

    fn start(&self) -> Option<usize> {
        self.doc_comment
            .or(self.annotations_start)
            .or(self.modifiers.first().map(|(_, offset)| *offset))
    }

    fn modifiers(&self) -> Vec<Modifier> {
        self.modifiers.iter().map(|(m, _)| m.clone()).collect()
    }

    fn has(&self, modifier: Modifier) -> bool {
        self.modifiers.iter().any(|(m, _)| *m == modifier)
    }
}

/// The kind of declaration a set of modifiers is attached to.
#[derive(Clone, Copy)]
enum MemberKind {
    Module,
    AmendingModule,
    Class,
    TypeAlias,
    Method,
    Property,
    ObjectMember,
}

impl MemberKind {
    fn allows(self, modifier: &Modifier) -> bool {
        use Modifier::*;
        match self {
            MemberKind::Module => matches!(modifier, Abstract | Open),
            MemberKind::AmendingModule => false,
            MemberKind::Class => matches!(modifier, Abstract | Open | Local | External),
            MemberKind::TypeAlias => matches!(modifier, Local | External),
            MemberKind::Method => matches!(modifier, Abstract | Local | External | Const),
            MemberKind::Property => {
                matches!(
                    modifier,
                    Abstract | Local | Hidden | External | Fixed | Const
                )
            }
            MemberKind::ObjectMember => matches!(modifier, Local | Const),
        }
    }

    fn description(self) -> &'static str {
        match self {
            MemberKind::Module => "modules",
            MemberKind::AmendingModule => "modules that amend another module",
            MemberKind::Class => "classes",
            MemberKind::TypeAlias => "type aliases",
            MemberKind::Method => "methods",
            MemberKind::Property => "properties",
            MemberKind::ObjectMember => "object members",
        }
    }
}

fn modifier_name(modifier: &Modifier) -> &'static str {
    match modifier {
        Modifier::Local => "local",
        Modifier::Const => "const",
        Modifier::Fixed => "fixed",
        Modifier::Hidden => "hidden",
        Modifier::Abstract => "abstract",
        Modifier::Open => "open",
        Modifier::External => "external",
    }
}

fn token_modifier(kind: &TokenKind) -> Option<Modifier> {
    Some(match kind {
        TokenKind::KwLocal => Modifier::Local,
        TokenKind::KwConst => Modifier::Const,
        TokenKind::KwFixed => Modifier::Fixed,
        TokenKind::KwHidden => Modifier::Hidden,
        TokenKind::KwAbstract => Modifier::Abstract,
        TokenKind::KwOpen => Modifier::Open,
        TokenKind::KwExternal => Modifier::External,
        _ => return None,
    })
}

fn keyword_text(kind: &TokenKind) -> Option<&'static str> {
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

/// Binary operators with their pkl precedence and associativity.
fn binary_operator(kind: &TokenKind) -> Option<(BinaryOp, u8, bool)> {
    use BinaryOp::*;
    Some(match kind {
        TokenKind::QuestionQuestion => (Bin(BinOp::NullCoalesce), 1, false),
        TokenKind::PipeGt => (Bin(BinOp::Pipe), 2, true),
        TokenKind::PipePipe => (Bin(BinOp::Or), 3, true),
        TokenKind::AmpAmp => (Bin(BinOp::And), 4, true),
        TokenKind::EqEq => (Bin(BinOp::Eq), 5, true),
        TokenKind::BangEq => (Bin(BinOp::Ne), 5, true),
        TokenKind::KwIs => (Is, 6, true),
        TokenKind::KwAs => (As, 6, true),
        TokenKind::Lt => (Bin(BinOp::Lt), 7, true),
        TokenKind::Gt => (Bin(BinOp::Gt), 7, true),
        TokenKind::LtEq => (Bin(BinOp::Le), 7, true),
        TokenKind::GtEq => (Bin(BinOp::Ge), 7, true),
        TokenKind::Plus => (Bin(BinOp::Add), 8, true),
        TokenKind::Minus => (Bin(BinOp::Sub), 8, true),
        TokenKind::Star => (Bin(BinOp::Mul), 9, true),
        TokenKind::Slash => (Bin(BinOp::Div), 9, true),
        TokenKind::TildeSlash => (Bin(BinOp::IntDiv), 9, true),
        TokenKind::Percent => (Bin(BinOp::Mod), 9, true),
        TokenKind::StarStar => (Bin(BinOp::Pow), 10, false),
        TokenKind::Dot => (Dot, 20, true),
        TokenKind::QuestionDot => (QDot, 20, true),
        _ => return None,
    })
}

#[derive(Clone, Copy)]
enum BinaryOp {
    Bin(BinOp),
    Is,
    As,
    Dot,
    QDot,
}

/// A parsed object body: optional parameters (`{ a, b -> ... }`) and members.
struct ObjectBody {
    params: Vec<String>,
    entries: Vec<Entry>,
}

struct Parser<'a> {
    tokens: &'a [Token],
    source: &'a str,
    name: &'a str,
    pos: usize,
    /// URIs of `import(...)` expressions parsed in this module.
    import_exprs: Vec<String>,
    /// End offset of the last consumed token.
    prev_end: usize,
    /// Line of the last consumed token, used when no source is available.
    prev_line: usize,
    /// Whether the module amends another module.
    amending: bool,
    /// Simple class names of `new` expressions and their offsets, checked
    /// against the module's declarations once it is parsed.
    new_types: Vec<(String, usize)>,
    /// Type parameter names for aliases currently being parsed.
    type_alias_parameters: std::collections::HashMap<String, Vec<String>>,
    /// Local aliases are resolved only if a value requires them, so their
    /// definitions are intentionally excluded from the module-level cycle
    /// check.
    local_type_aliases: std::collections::HashSet<String>,
    /// Expressions and type constraints currently nested around the cursor.
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token], source: &'a str, name: &'a str) -> Self {
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
            depth: 0,
        };
        p.skip_semicolons();
        p
    }

    // ---- token access ----

    fn skip_semicolons(&mut self) {
        while self.pos + 1 < self.tokens.len()
            && matches!(self.tokens[self.pos].kind, TokenKind::Semicolon)
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_tok(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// The kind of the `n`th token after the current one, skipping semicolons.
    fn peek_nth(&self, n: usize) -> &TokenKind {
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

    fn advance(&mut self) -> &'a Token {
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

    fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    fn preceded_by_semicolon(&self) -> bool {
        self.pos > 0 && matches!(self.tokens[self.pos - 1].kind, TokenKind::Semicolon)
    }

    /// Whether a newline separates the current token from the previous one.
    fn newline_before(&self) -> bool {
        let tok = self.peek_tok();
        match self.source.get(self.prev_end..tok.offset) {
            Some(gap) => gap.contains('\n'),
            None => tok.line != self.prev_line,
        }
    }

    /// Whether the current token continues the previous token's line: pkl
    /// only treats `(`, `[` and `-` as continuing an expression on the same
    /// line, without a semicolon in between.
    fn on_same_line(&self) -> bool {
        !self.newline_before() && !self.preceded_by_semicolon()
    }

    /// Source text of a token, for error messages.
    fn token_text(&self, tok: &Token) -> String {
        if matches!(tok.kind, TokenKind::Eof) {
            return "EOF".to_string();
        }
        match self.source.get(tok.offset..tok.end) {
            Some(text) if !text.is_empty() => text.to_string(),
            _ => format!("{:?}", tok.kind),
        }
    }

    /// Whether the current token was written as a backquoted identifier.
    fn is_quoted(&self, tok: &Token) -> bool {
        self.source.as_bytes().get(tok.offset) == Some(&b'`')
    }

    /// The keyword the current token spells, if it can't be used as a name.
    fn keyword_at(&self, tok: &Token) -> Option<String> {
        match &tok.kind {
            TokenKind::Ident(name) => (!self.is_quoted(tok)
                && RESERVED_WORDS.contains(&name.as_str()))
            .then(|| name.clone()),
            other => keyword_text(other).map(String::from),
        }
    }

    // ---- errors ----

    fn error_at(&self, offset: usize, message: impl Into<String>) -> Error {
        Error::parse(self.name, self.source, offset, message.into())
    }

    fn parse_error(&self, message: impl Into<String>) -> Error {
        self.error_at(self.peek_tok().offset, message)
    }

    /// Error offset for a missing token: the end of the previous token when
    /// the current one is on another line or is the end of the file.
    fn missing_offset(&self) -> usize {
        if self.at_eof() || self.newline_before() {
            self.prev_end
        } else {
            self.peek_tok().offset
        }
    }

    fn unexpected_token(&self, expected: &str) -> Error {
        let text = self.token_text(self.peek_tok());
        self.error_at(
            self.missing_offset(),
            format!("Unexpected token `{text}`. Expected `{expected}`."),
        )
    }

    fn unexpected_token2(&self, expected1: &str, expected2: &str) -> Error {
        let text = self.token_text(self.peek_tok());
        self.error_at(
            self.missing_offset(),
            format!("Unexpected token `{text}`. Expected `{expected1}` or `{expected2}`."),
        )
    }

    fn expect(&mut self, kind: &TokenKind, text: &str) -> Result<&'a Token> {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            Ok(self.advance())
        } else {
            Err(self.unexpected_token(text))
        }
    }

    fn expect2(&mut self, kind: &TokenKind, text: &str, alternative: &str) -> Result<&'a Token> {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            Ok(self.advance())
        } else {
            Err(self.unexpected_token2(alternative, text))
        }
    }

    fn keyword_error(&self, keyword: &str) -> Error {
        self.parse_error(format!(
            "Keyword `{keyword}` is not allowed here.\n\n\
             If you must use this name as identifier, enclose it in backticks."
        ))
    }

    fn expect_ident(&mut self) -> Result<String> {
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
    fn expect_field_ident(&mut self) -> Result<String> {
        if matches!(self.peek(), TokenKind::Ident(name) if name == "outer")
            && !self.is_quoted(self.peek_tok())
        {
            self.advance();
            return Ok("outer".to_string());
        }
        self.expect_ident()
    }

    fn expect_string_constant(&mut self) -> Result<String> {
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

    fn parse_qualified_ident(&mut self) -> Result<String> {
        let mut name = self.expect_ident()?;
        while matches!(self.peek(), TokenKind::Dot) {
            self.advance();
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        Ok(name)
    }

    // ---- declarations ----

    fn parse_member_header(&mut self) -> Result<MemberHeader> {
        let mut header = MemberHeader::default();
        if matches!(self.peek(), TokenKind::DocComment) {
            header.doc_comment = Some(self.peek_tok().offset);
            self.advance();
            // Consecutive doc comment lines form one comment; a blank line
            // ends it, leaving a dangling doc comment.
            while matches!(self.peek(), TokenKind::DocComment) && !self.blank_line_before() {
                self.advance();
            }
        }
        while matches!(self.peek(), TokenKind::At) {
            header
                .annotations_start
                .get_or_insert(self.peek_tok().offset);
            header.annotations.push(self.parse_annotation()?);
        }
        while let Some(modifier) = token_modifier(self.peek()) {
            header.modifiers.push((modifier, self.peek_tok().offset));
            self.advance();
        }
        Ok(header)
    }

    /// Whether an empty line separates the current token from the previous one.
    fn blank_line_before(&self) -> bool {
        let tok = self.peek_tok();
        match self.source.get(self.prev_end..tok.offset) {
            Some(gap) => gap
                .split('\n')
                .skip(1)
                .take(gap.matches('\n').count().saturating_sub(1))
                .any(|line| line.trim().is_empty()),
            None => tok.line > self.prev_line + 1,
        }
    }

    /// Parse an annotation: `@Type` or `@Type { body }`
    fn parse_annotation(&mut self) -> Result<Annotation> {
        self.advance(); // @
        let offset = self.peek_tok().offset;
        let name = match self.parse_type()? {
            TypeExpr::Named(name) | TypeExpr::Generic(name, _) => name,
            _ => return Err(self.error_at(offset, "Expected an annotation class.")),
        };
        let body = if matches!(self.peek(), TokenKind::LBrace) {
            self.parse_object_body(false)?.entries
        } else {
            Vec::new()
        };
        Ok(Annotation { name, body })
    }

    /// Check modifiers against the kind of member they are attached to.
    fn check_modifiers(&self, header: &MemberHeader, kind: MemberKind) -> Result<()> {
        for (modifier, offset) in &header.modifiers {
            if !kind.allows(modifier) {
                return Err(self.error_at(
                    *offset,
                    format!(
                        "Modifier `{}` is not applicable to {}.",
                        modifier_name(modifier),
                        kind.description()
                    ),
                ));
            }
        }
        let offset_of = |wanted: Modifier| {
            header
                .modifiers
                .iter()
                .find(|(m, _)| *m == wanted)
                .map_or(0, |(_, offset)| *offset)
        };
        if header.has(Modifier::External) {
            return Err(self.error_at(
                offset_of(Modifier::External),
                "External members can only be defined by standard library modules.",
            ));
        }
        if header.has(Modifier::Local) && header.has(Modifier::Hidden) {
            return Err(self.error_at(
                offset_of(Modifier::Hidden),
                "Modifier `hidden` is redundant here; just use `local`.",
            ));
        }
        if header.has(Modifier::Local) && header.has(Modifier::Fixed) {
            return Err(self.error_at(
                offset_of(Modifier::Fixed),
                "Modifier `fixed` is redundant here; just use `local`.",
            ));
        }
        if header.has(Modifier::Abstract) && header.has(Modifier::Open) {
            return Err(self.error_at(
                offset_of(Modifier::Open),
                "Modifier `open` is redundant here; just use `abstract`.",
            ));
        }
        if matches!(kind, MemberKind::ObjectMember)
            && header.has(Modifier::Const)
            && !header.has(Modifier::Local)
        {
            return Err(self.error_at(
                offset_of(Modifier::Const),
                "Modifier `const` can only be applied to object members that are also `local`.",
            ));
        }
        Ok(())
    }

    fn parse_module(&mut self) -> Result<Module> {
        let mut annotations = Vec::new();
        let mut amends = None;
        let mut extends = None;
        let mut header = Some(self.parse_member_header()?);

        let mut name = None;
        let has_module_decl = matches!(self.peek(), TokenKind::KwModule);
        if has_module_decl {
            self.advance();
            name = Some(self.parse_qualified_ident()?);
        }
        loop {
            match self.peek() {
                TokenKind::KwAmends if amends.is_none() => {
                    self.advance();
                    amends = Some(self.expect_string_constant()?);
                }
                TokenKind::KwExtends if extends.is_none() => {
                    self.advance();
                    extends = Some(self.expect_string_constant()?);
                }
                _ => break,
            }
        }
        self.amending = amends.is_some();
        if has_module_decl || amends.is_some() || extends.is_some() {
            let header = header.take().unwrap_or_default();
            self.check_modifiers(&header, MemberKind::Module)?;
            if self.amending {
                self.check_modifiers(&header, MemberKind::AmendingModule)?;
            }
            annotations = header.annotations.clone();
            for modifier in header.modifiers() {
                annotations.push(Annotation {
                    name: format!("pklr:module:{modifier:?}"),
                    body: Vec::new(),
                });
            }
        }
        self.check_min_pkl_version(&annotations, name.as_deref())?;
        let mut imports = Vec::new();
        while matches!(self.peek(), TokenKind::KwImport | TokenKind::KwImportStar) {
            if let Some(header) = &header
                && !header.is_empty()
            {
                return Err(self.error_at(
                    header.start().unwrap_or(self.peek_tok().offset),
                    "Imports cannot have doc comments, annotations or modifiers.",
                ));
            }
            let is_glob = matches!(self.peek(), TokenKind::KwImportStar);
            self.advance();
            let uri = self.expect_string_constant()?;
            let alias = if matches!(self.peek(), TokenKind::KwAs) {
                self.advance();
                Some(self.expect_ident()?)
            } else {
                None
            };
            imports.push(Import {
                uri,
                alias,
                is_glob,
            });
        }

        let mut body = Vec::new();
        let mut header = header.filter(|header| !header.is_empty());
        loop {
            let header = match header.take() {
                Some(header) => header,
                None => self.parse_member_header()?,
            };
            if header.is_empty() && self.at_eof() {
                break;
            }
            self.parse_module_member(header, &mut body)?;
        }
        self.check_new_types(&body, &imports)?;
        self.check_member_duplicates(&body, &imports)?;
        self.check_type_alias_cycles(&body)?;
        if let Some(violation) = validate::find_const_violation(&body) {
            return Err(self.error_at(self.prev_end, violation.message()));
        }
        Ok(Module {
            name,
            amends,
            extends,
            imports,
            import_exprs: std::mem::take(&mut self.import_exprs),
            annotations,
            body: body.into(),
            local_type_aliases: std::mem::take(&mut self.local_type_aliases),
        })
    }

    /// Reject `new X {}` where `X` names a module property rather than a
    /// class or type alias: types and values live in separate namespaces.
    fn check_new_types(&self, body: &[Entry], imports: &[Import]) -> Result<()> {
        if self.new_types.is_empty() {
            return Ok(());
        }
        let is_type = |name: &str| {
            entries_define_type(body, name)
                || imports.iter().any(|import| {
                    let inferred = import
                        .uri
                        .rsplit(['/', ':'])
                        .next()
                        .unwrap_or(&import.uri)
                        .trim_end_matches(".pkl");
                    import.alias.as_deref().unwrap_or(inferred) == name
                })
        };
        for (name, offset) in &self.new_types {
            let is_property = body
                .iter()
                .any(|entry| matches!(entry, Entry::Property(p) if p.name == *name));
            if is_property && !is_type(name) {
                return Err(self.error_at(
                    *offset,
                    format!("Expected `{name}` to be a type, but it is not."),
                ));
            }
        }
        Ok(())
    }

    fn check_min_pkl_version(
        &self,
        annotations: &[Annotation],
        module_name: Option<&str>,
    ) -> Result<()> {
        let required = annotations
            .iter()
            .filter(|annotation| annotation.name == "ModuleInfo")
            .flat_map(|annotation| &annotation.body)
            .find_map(|entry| match entry {
                Entry::Property(property) if property.name == "minPklVersion" => {
                    property.value.as_ref().and_then(constant_string)
                }
                _ => None,
            });
        let Some(required) = required else {
            return Ok(());
        };
        let (Some(required), Some(current)) = (version_triple(&required), version_triple("0.32.1"))
        else {
            return Ok(());
        };
        if required <= current {
            return Ok(());
        }
        let name = module_name.unwrap_or_else(|| {
            std::path::Path::new(self.name)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or(self.name)
        });
        Err(self.error_at(
            0,
            format!(
                "Module `{name}` requires Pkl version {} or higher, but your Pkl version is 0.32.1.",
                required_string(&required)
            ),
        ))
    }

    fn check_member_duplicates(&self, entries: &[Entry], imports: &[Import]) -> Result<()> {
        self.check_entry_duplicates(entries, Some(imports), false, false)
    }

    /// Validate duplicate names after parsing a body. Object values need the
    /// complete body before a duplicate can be rejected: Pkl merges a body
    /// containing a spread or generator at run time instead.
    fn check_entry_duplicates(
        &self,
        entries: &[Entry],
        imports: Option<&[Import]>,
        object_scope: bool,
        generated_scope: bool,
    ) -> Result<()> {
        let mut members = std::collections::HashSet::new();
        let mut local_members = std::collections::HashSet::new();
        let mut methods = std::collections::HashSet::new();
        let mut dynamic_entries = std::collections::HashSet::new();
        let has_runtime_members = entries.iter().any(|entry| {
            matches!(
                entry,
                Entry::Spread(_) | Entry::ForGenerator(_) | Entry::WhenGenerator(_)
            )
        });

        if let Some(imports) = imports {
            for import in imports {
                let name = import.alias.as_deref().or_else(|| {
                    (!import.is_glob).then(|| {
                        import
                            .uri
                            .rsplit(['/', ':'])
                            .next()
                            .unwrap_or(&import.uri)
                            .trim_end_matches(".pkl")
                    })
                });
                if let Some(name) = name
                    && !members.insert(name.to_string())
                {
                    return Err(self.error_at(
                        self.prev_end,
                        format!("Duplicate definition of member `{name}`."),
                    ));
                }
            }
        }
        for entry in entries {
            let duplicate = match entry {
                Entry::Property(property) => {
                    let names = if property.is_method {
                        &mut methods
                    } else if object_scope && property.modifiers.contains(&Modifier::Local) {
                        &mut local_members
                    } else {
                        &mut members
                    };
                    (!names.insert(property.name.clone())).then(|| property.name.clone())
                }
                Entry::ClassDef(name, ..) => (!members.insert(name.clone())).then(|| name.clone()),
                Entry::TypeAlias(name, _) => {
                    let names = if object_scope && self.local_type_aliases.contains(name) {
                        &mut local_members
                    } else {
                        &mut members
                    };
                    (!names.insert(name.clone())).then(|| name.clone())
                }
                Entry::DynProperty(key, _) => constant_entry_name(key)
                    .and_then(|name| (!dynamic_entries.insert(name.clone())).then_some(name)),
                _ => None,
            };
            if let Some(name) = duplicate
                && !generated_scope
                && !has_runtime_members
            {
                return Err(self.error_at(
                    self.prev_end,
                    format!("Duplicate definition of member `{name}`."),
                ));
            }
        }

        for entry in entries {
            self.check_entry_child_duplicates(entry)?;
        }
        Ok(())
    }

    fn check_entry_child_duplicates(&self, entry: &Entry) -> Result<()> {
        match entry {
            Entry::Property(property) => {
                for annotation in &property.annotations {
                    self.check_entry_duplicates(&annotation.body, None, true, false)?;
                }
                if let Some(value) = &property.value {
                    self.check_expr_duplicates(value)?;
                }
                if let Some(body) = &property.body {
                    self.check_entry_duplicates(body, None, true, false)?;
                }
            }
            Entry::DynProperty(key, value) | Entry::Predicate(key, value) => {
                self.check_expr_duplicates(key)?;
                self.check_expr_duplicates(value)?;
            }
            Entry::Spread(expr) | Entry::Elem(expr) => self.check_expr_duplicates(expr)?,
            Entry::ForGenerator(generator) => {
                self.check_expr_duplicates(&generator.collection)?;
                self.check_entry_duplicates(&generator.body, None, true, true)?;
            }
            Entry::WhenGenerator(generator) => {
                self.check_expr_duplicates(&generator.condition)?;
                self.check_entry_duplicates(&generator.body, None, true, true)?;
                if let Some(else_body) = &generator.else_body {
                    self.check_entry_duplicates(else_body, None, true, true)?;
                }
            }
            Entry::ClassDef(_, _, _, body) => {
                self.check_entry_duplicates(body, None, false, false)?
            }
            Entry::TypeAlias(_, _) => {}
        }
        Ok(())
    }

    fn check_expr_duplicates(&self, expr: &Expr) -> Result<()> {
        match expr {
            Expr::New(_, body, _) | Expr::ObjectBody(body) | Expr::InferredNew(_, body) => {
                self.check_entry_duplicates(body, None, true, false)?
            }
            Expr::Field(value, _)
            | Expr::NullSafeField(value, _)
            | Expr::Unop(_, value)
            | Expr::Throw(value)
            | Expr::Trace(value, _)
            | Expr::Read(value, _)
            | Expr::ReadOrNull(value, _)
            | Expr::ReadGlob(value, _) => self.check_expr_duplicates(value)?,
            Expr::Index(left, right) | Expr::Binop(_, left, right) => {
                self.check_expr_duplicates(left)?;
                self.check_expr_duplicates(right)?;
            }
            Expr::Call(callee, args) => {
                self.check_expr_duplicates(callee)?;
                for arg in args {
                    self.check_expr_duplicates(arg)?;
                }
            }
            Expr::If(condition, then_expr, else_expr) => {
                self.check_expr_duplicates(condition)?;
                self.check_expr_duplicates(then_expr)?;
                self.check_expr_duplicates(else_expr)?;
            }
            Expr::Let(_, value, body) => {
                self.check_expr_duplicates(value)?;
                self.check_expr_duplicates(body)?;
            }
            Expr::Lambda(_, body) => self.check_expr_duplicates(body)?,
            Expr::StringInterpolation(parts) => {
                for part in parts {
                    if let StringInterpPart::Expr(expr) = part {
                        self.check_expr_duplicates(expr)?;
                    }
                }
            }
            Expr::Is(value, _) | Expr::As(value, _) => self.check_expr_duplicates(value)?,
            Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Ident(_)
            | Expr::Import(_, _)
            | Expr::ImportGlob(_, _) => {}
        }
        Ok(())
    }

    fn check_type_alias_cycles(&self, entries: &[Entry]) -> Result<()> {
        let aliases: Vec<(&str, &TypeExpr)> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::TypeAlias(name, ty) if !self.local_type_aliases.contains(name) => {
                    Some((name.as_str(), ty))
                }
                _ => None,
            })
            .collect();
        let indexes: std::collections::HashMap<&str, usize> = aliases
            .iter()
            .enumerate()
            .map(|(index, (name, _))| (*name, index))
            .collect();
        let mut refs = Vec::with_capacity(aliases.len());
        for (alias, ty) in &aliases {
            let mut names = Vec::new();
            collect_type_names(ty, &mut names);
            if let Some(parameters) = self.type_alias_parameters.get(*alias) {
                names.retain(|name| !parameters.contains(name));
            }
            refs.push(
                names
                    .into_iter()
                    .filter_map(|name| indexes.get(name.as_str()).copied())
                    .collect::<Vec<_>>(),
            );
        }
        let mut state = vec![0_u8; aliases.len()];
        for root in 0..aliases.len() {
            if state[root] != 0 {
                continue;
            }
            let mut stack = vec![(root, 0_usize)];
            state[root] = 1;
            while let Some((index, next)) = stack.last_mut() {
                if *next == refs[*index].len() {
                    state[*index] = 2;
                    stack.pop();
                    continue;
                }
                let target = refs[*index][*next];
                *next += 1;
                match state[target] {
                    1 => {
                        return Err(self.error_at(
                            self.prev_end,
                            "Type alias definitions must not be cyclic.",
                        ));
                    }
                    0 => {
                        state[target] = 1;
                        stack.push((target, 0));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn parse_module_member(
        &mut self,
        header: MemberHeader,
        entries: &mut Vec<Entry>,
    ) -> Result<()> {
        if let Some(keyword) = self.keyword_at(self.peek_tok())
            && matches!(self.peek(), TokenKind::Ident(_))
        {
            return Err(self.keyword_error(&keyword));
        }
        match self.peek() {
            TokenKind::Ident(_) => {
                let entry = self.parse_class_property(header, true)?;
                entries.push(entry);
            }
            TokenKind::KwTypeAlias => {
                entries.push(self.parse_type_alias(header)?);
            }
            TokenKind::KwClass => {
                entries.push(self.parse_class(header)?);
            }
            TokenKind::KwFunction => {
                if self.amending && !header.has(Modifier::Local) {
                    return Err(self.parse_error(
                        "Method needs a `local` modifier because it is defined in an object, \
                         not a class.",
                    ));
                }
                if let Some(entry) = self.parse_method(header, MemberKind::Method)? {
                    entries.push(entry);
                }
            }
            TokenKind::Eof => return Err(self.parse_error("Unexpected end of file.")),
            TokenKind::DocComment => {
                return Err(self.parse_error(
                    "Dangling documentation comment.\n\n\
                     Documentation comments must be attached to modules, classes, typealiases, \
                     methods, or properties.",
                ));
            }
            other => {
                if let Some(keyword) = keyword_text(other) {
                    return Err(self.keyword_error(keyword));
                }
                return Err(self.parse_error(
                    "Invalid token at position. Expected a class, typealias, method, or property.",
                ));
            }
        }
        Ok(())
    }

    fn parse_type_alias(&mut self, header: MemberHeader) -> Result<Entry> {
        self.check_modifiers(&header, MemberKind::TypeAlias)?;
        if self.amending && !header.has(Modifier::Local) {
            return Err(self.parse_error(
                "Type alias needs a `local` modifier.\n\n\
                 To define a non-local type alias, extend rather than amend the parent module \
                 (which must be `open` for extension).",
            ));
        }
        self.advance(); // typealias
        let name = self.expect_ident()?;
        let parameters = if matches!(self.peek(), TokenKind::Lt) {
            self.parse_type_parameters()?
        } else {
            Vec::new()
        };
        if header.has(Modifier::Local) {
            self.local_type_aliases.insert(name.clone());
        }
        self.type_alias_parameters.insert(name.clone(), parameters);
        self.expect(&TokenKind::Equals, "=")?;
        let ty = self.parse_type()?;
        Ok(Entry::TypeAlias(name, ty))
    }

    /// Parse `<A, out B, in C>`, rejecting duplicate names.
    fn parse_type_parameters(&mut self) -> Result<Vec<String>> {
        self.expect(&TokenKind::Lt, "<")?;
        let mut names: Vec<String> = Vec::new();
        loop {
            match self.peek() {
                TokenKind::KwIn => {
                    self.advance();
                }
                TokenKind::Ident(name) if name == "out" && !self.is_quoted(self.peek_tok()) => {
                    self.advance();
                }
                _ => {}
            }
            let offset = self.peek_tok().offset;
            let name = self.expect_ident()?;
            if names.contains(&name) {
                return Err(self.error_at(offset, format!("Duplicate type parameter `{name}`.")));
            }
            names.push(name);
            if !matches!(self.peek(), TokenKind::Comma) {
                break;
            }
            self.advance();
            if matches!(self.peek(), TokenKind::Gt) {
                break;
            }
        }
        self.expect2(&TokenKind::Gt, ">", ",")?;
        Ok(names)
    }

    fn type_parameters_error(&self, offset: usize) -> Error {
        self.error_at(
            offset,
            "Only standard library members can have type parameters.",
        )
    }

    fn parse_class(&mut self, header: MemberHeader) -> Result<Entry> {
        self.check_modifiers(&header, MemberKind::Class)?;
        if self.amending && !header.has(Modifier::Local) {
            return Err(self.parse_error(
                "Class needs a `local` modifier.\n\n\
                 To define a non-local class, extend rather than amend the parent module \
                 (which must be `open` for extension).",
            ));
        }
        self.advance(); // class
        let name = self.expect_ident()?;
        if matches!(self.peek(), TokenKind::Lt) {
            let offset = self.peek_tok().offset;
            self.parse_type_parameters()?;
            return Err(self.type_parameters_error(offset));
        }
        let parent = if matches!(self.peek(), TokenKind::KwExtends) {
            self.advance();
            Some(match self.parse_type()? {
                TypeExpr::Named(name)
                | TypeExpr::Generic(name, _)
                | TypeExpr::Constrained(name, _) => name,
                other => type_expr_runtime_name(&other),
            })
        } else {
            None
        };
        let mut body = Vec::new();
        if matches!(self.peek(), TokenKind::LBrace) {
            self.advance();
            while !matches!(self.peek(), TokenKind::RBrace) {
                if self.at_eof() {
                    return Err(self.error_at(self.prev_end, "Missing `}` delimiter."));
                }
                let header = self.parse_member_header()?;
                match self.peek() {
                    TokenKind::KwFunction => {
                        if let Some(entry) = self.parse_method(header, MemberKind::Method)? {
                            body.push(entry);
                        }
                    }
                    TokenKind::KwClass => body.push(self.parse_class(header)?),
                    TokenKind::KwTypeAlias => body.push(self.parse_type_alias(header)?),
                    _ => body.push(self.parse_class_property(header, false)?),
                }
            }
            self.advance(); // }
        }
        Ok(Entry::ClassDef(
            name,
            header.modifiers(),
            parent,
            body.into(),
        ))
    }

    /// Parse a module or class property: `name [: Type] [= expr | { ... }]`.
    fn parse_class_property(&mut self, header: MemberHeader, module_level: bool) -> Result<Entry> {
        // Properties of amending modules are object members.
        let as_object_member = module_level && self.amending;
        self.check_modifiers(
            &header,
            if as_object_member {
                MemberKind::ObjectMember
            } else {
                MemberKind::Property
            },
        )?;
        let name_offset = self.peek_tok().offset;
        let name = self.expect_ident()?;
        let local = header.has(Modifier::Local);
        let type_ann = if matches!(self.peek(), TokenKind::Colon) {
            self.advance();
            let type_offset = self.peek_tok().offset;
            let ty = self.parse_type()?;
            if as_object_member && !local {
                return Err(self.error_at(
                    type_offset,
                    "A non-local object property cannot have a type annotation.",
                ));
            }
            Some(ty)
        } else {
            None
        };
        let (mut value, body) = match self.peek() {
            TokenKind::Equals => {
                self.advance();
                (Some(self.parse_expr()?), None)
            }
            TokenKind::LBrace => {
                if type_ann.is_some() {
                    return Err(self.parse_error(
                        "Properties with type annotations cannot have object bodies.\n\n\
                         To define both a type annotation and an object body, try using \
                         assignment instead.",
                    ));
                }
                if local && as_object_member {
                    return Err(self.local_amend_error());
                }
                self.parse_property_bodies(&name)?
            }
            _ => {
                if type_ann.is_none() {
                    return Err(self.error_at(
                        name_offset,
                        "Invalid property definition. Expected a type annotation, `=` or `{`.",
                    ));
                }
                if local {
                    return Err(self.error_at(name_offset, "Missing property value."));
                }
                (None, None)
            }
        };
        // A declared Mapping, Listing, class, or alias makes an untyped
        // `new { ... }` construct that declared type, so nested entry bodies
        // retain their value-type semantics.
        if let (Some(value), Some(ty)) = (&mut value, &type_ann)
            && infers_new(ty)
        {
            infer_method_return_new(value, ty);
        }
        Ok(Entry::Property(std::sync::Arc::new(Property {
            annotations: header.annotations.clone(),
            modifiers: header.modifiers(),
            name,
            type_ann,
            value,
            body: body.map(Into::into),
            is_method: false,
        })))
    }

    fn local_amend_error(&self) -> Error {
        self.parse_error(
            "A local property definition cannot be amended.\n\n\
             Use definition syntax instead, for example `local person = new { ... }` \
             instead of `local person { ... }`.",
        )
    }

    /// Parse the object bodies of a property amendment `name { ... } { ... }`.
    /// Bodies with parameters amend a function: `name { x -> ... }` means
    /// `name = (x) -> super.name.apply(x) { ... }`.
    fn parse_property_bodies(&mut self, name: &str) -> Result<(Option<Expr>, Option<Vec<Entry>>)> {
        let super_value = Expr::Field(Box::new(Expr::Ident("super".into())), name.to_string());
        match self.parse_body_list()? {
            BodyList::Plain(entries) => Ok((None, Some(entries))),
            BodyList::Amended(f) => Ok((Some(f(super_value)), None)),
        }
    }

    /// Parse one or more object bodies following a member. Several plain
    /// bodies amend in turn, which for a member is the same as one body
    /// holding all their members.
    fn parse_body_list(&mut self) -> Result<BodyList> {
        if !matches!(self.peek(), TokenKind::LBrace) {
            return Err(self.unexpected_token2("{", "="));
        }
        let mut bodies = Vec::new();
        while matches!(self.peek(), TokenKind::LBrace) {
            bodies.push(self.parse_object_body(false)?);
        }
        if bodies.iter().all(|body| body.params.is_empty()) {
            return Ok(BodyList::Plain(
                bodies.into_iter().flat_map(|body| body.entries).collect(),
            ));
        }
        Ok(BodyList::Amended(Box::new(move |parent| {
            bodies.into_iter().fold(parent, amend_expr)
        })))
    }

    /// Parse `function name(params): Type = body` into a Property with a
    /// Lambda value. Returns None for methods without a body.
    fn parse_method(&mut self, header: MemberHeader, kind: MemberKind) -> Result<Option<Entry>> {
        self.check_modifiers(&header, kind)?;
        self.advance(); // function
        let name = self.expect_ident()?;
        if matches!(self.peek(), TokenKind::Lt) {
            let offset = self.peek_tok().offset;
            self.parse_type_parameters()?;
            return Err(self.type_parameters_error(offset));
        }
        let params = self.parse_parameter_list()?;
        let return_type = if matches!(self.peek(), TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        if !matches!(self.peek(), TokenKind::Equals) {
            if matches!(kind, MemberKind::ObjectMember) {
                return Err(self.unexpected_token("="));
            }
            // Abstract or external method without a body
            return Ok(None);
        }
        self.advance(); // =
        let mut body = self.parse_expr()?;
        if let Some(return_type) = return_type {
            infer_method_return_new(&mut body, &return_type);
        }
        Ok(Some(Entry::Property(std::sync::Arc::new(Property {
            name,
            type_ann: None,
            value: Some(Expr::Lambda(params.into(), std::sync::Arc::new(body))),
            body: None,
            modifiers: header.modifiers(),
            annotations: header.annotations.clone(),
            is_method: true,
        }))))
    }

    /// Parse `(a: Type, b, _)`; types are checked but not kept.
    fn parse_parameter_list(&mut self) -> Result<Vec<String>> {
        self.expect(&TokenKind::LParen, "(")?;
        let params = self.parse_parameters(&TokenKind::RParen)?;
        self.expect2(&TokenKind::RParen, ")", ",")?;
        Ok(params)
    }

    /// Parse a comma-separated parameter list up to `terminator`, allowing a
    /// trailing comma.
    fn parse_parameters(&mut self, terminator: &TokenKind) -> Result<Vec<String>> {
        let mut params = Vec::new();
        let at_terminator =
            |p: &Self| std::mem::discriminant(p.peek()) == std::mem::discriminant(terminator);
        if at_terminator(self) {
            return Ok(params);
        }
        loop {
            params.push(self.parse_parameter()?);
            if !matches!(self.peek(), TokenKind::Comma) {
                break;
            }
            self.advance();
            if at_terminator(self) {
                break;
            }
        }
        Ok(params)
    }

    /// Parse `name`, `name: Type` or `_`.
    fn parse_parameter(&mut self) -> Result<String> {
        Ok(self.parse_typed_parameter()?.0)
    }

    /// Parse `name`, `name: Type` or `_`, keeping the type.
    fn parse_typed_parameter(&mut self) -> Result<(String, Option<TypeExpr>)> {
        if matches!(self.peek(), TokenKind::Ident(name) if name == "_")
            && !self.is_quoted(self.peek_tok())
        {
            self.advance();
            return Ok(("_".to_string(), None));
        }
        let name = self.expect_ident()?;
        let ty = if matches!(self.peek(), TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        Ok((name, ty))
    }

    // ---- object bodies ----

    /// Parse `{ [params ->] members }`. `in_for` is set inside the body of a
    /// for-generator, which cannot define properties or methods.
    fn parse_object_body(&mut self, in_for: bool) -> Result<ObjectBody> {
        self.expect(&TokenKind::LBrace, "{")?;
        let params = self.parse_body_params()?;
        let mut entries = Vec::new();
        let mut prev_end: Option<usize> = None;
        while !matches!(self.peek(), TokenKind::RBrace) {
            if self.at_eof() {
                return Err(self.error_at(self.prev_end, "Missing `}` delimiter."));
            }
            let start = self.peek_tok().offset;
            if prev_end == Some(start) {
                return Err(self.error_at(
                    start,
                    "Object members must be separated by whitespace, newline, or semicolon.\n\
                     Object entries must be separated by newline or semicolon.",
                ));
            }
            self.parse_object_member(in_for, &mut entries)?;
            prev_end = Some(self.prev_end);
        }
        self.advance(); // }
        Ok(ObjectBody { params, entries })
    }

    /// Parse the `a, b: Type ->` parameters at the start of an object body.
    fn parse_body_params(&mut self) -> Result<Vec<String>> {
        let is_param_start = |kind: &TokenKind| matches!(kind, TokenKind::Ident(_));
        if !is_param_start(self.peek()) {
            return Ok(Vec::new());
        }
        // Look ahead for `->` after a parameter list without consuming.
        let saved = (self.pos, self.prev_end, self.prev_line);
        let parsed = (|| -> Result<Option<Vec<String>>> {
            let params = self.parse_parameters(&TokenKind::Arrow)?;
            if matches!(self.peek(), TokenKind::Arrow) {
                self.advance();
                Ok(Some(params))
            } else {
                Ok(None)
            }
        })();
        match parsed {
            Ok(Some(params)) => Ok(params),
            _ => {
                (self.pos, self.prev_end, self.prev_line) = saved;
                Ok(Vec::new())
            }
        }
    }

    fn parse_object_member(&mut self, in_for: bool, entries: &mut Vec<Entry>) -> Result<()> {
        match self.peek() {
            TokenKind::Ident(_)
                if matches!(
                    self.peek_nth(1),
                    TokenKind::LBrace | TokenKind::Colon | TokenKind::Equals
                ) =>
            {
                let entry = self.parse_object_property(MemberHeader::default(), in_for)?;
                entries.push(entry);
            }
            TokenKind::KwFunction => {
                let entry = self.parse_object_method(MemberHeader::default(), in_for)?;
                entries.extend(entry);
            }
            TokenKind::LPred => {
                self.advance();
                let pred = self.parse_expr()?;
                self.expect(&TokenKind::RBracket, "]]")?;
                let first_end = self.prev_end;
                if !matches!(self.peek(), TokenKind::RBracket)
                    || self.peek_tok().offset != first_end
                {
                    return Err(self.unexpected_token("]]"));
                }
                self.advance();
                let value = self.parse_entry_value(&pred)?;
                entries.push(Entry::Predicate(pred, value));
            }
            TokenKind::LBracket => {
                self.advance();
                let key = self.parse_expr()?;
                self.expect(&TokenKind::RBracket, "]")?;
                let value = self.parse_entry_value(&key)?;
                entries.push(Entry::DynProperty(key, value));
            }
            TokenKind::DotDotDot | TokenKind::QuestionDotDotDot => {
                let nullable = matches!(self.peek(), TokenKind::QuestionDotDotDot);
                self.advance();
                let expr = self.parse_expr()?;
                entries.push(Entry::Spread(if nullable {
                    // `...?x` spreads nothing when `x` is null
                    Expr::Binop(
                        BinOp::NullCoalesce,
                        Box::new(expr),
                        Box::new(Expr::ObjectBody(Vec::new().into())),
                    )
                } else {
                    expr
                }));
            }
            TokenKind::KwWhen => {
                self.advance();
                self.expect(&TokenKind::LParen, "(")?;
                let condition = self.parse_expr()?;
                self.expect(&TokenKind::RParen, ")")?;
                let body = self.parse_generator_body(in_for)?;
                let else_body = if matches!(self.peek(), TokenKind::KwElse) {
                    self.advance();
                    Some(self.parse_generator_body(in_for)?.into())
                } else {
                    None
                };
                entries.push(Entry::WhenGenerator(WhenGenerator {
                    condition,
                    body: body.into(),
                    else_body,
                }));
            }
            TokenKind::KwFor => {
                self.advance();
                self.expect(&TokenKind::LParen, "(")?;
                let first = self.parse_typed_parameter()?;
                let (key_var, val_var) = if matches!(self.peek(), TokenKind::Comma) {
                    self.advance();
                    let second_offset = self.peek_tok().offset;
                    let second = self.parse_typed_parameter()?;
                    if first.0 == second.0 && first.0 != "_" {
                        return Err(self.error_at(
                            second_offset,
                            format!("Duplicate definition of member `{}`.", second.0),
                        ));
                    }
                    (Some(first), second)
                } else {
                    (None, first)
                };
                self.expect(&TokenKind::KwIn, "in")?;
                let collection = self.parse_expr()?;
                self.expect(&TokenKind::RParen, ")")?;
                let mut body = self.parse_generator_body(true)?;
                // Typed variables are checked for every iteration.
                let checks: Vec<Expr> = key_var
                    .iter()
                    .chain([&val_var])
                    .filter_map(|(name, ty)| {
                        let ty = ty.clone()?;
                        Some(Expr::Let(
                            "_".to_string(),
                            Box::new(Expr::As(Box::new(Expr::Ident(name.clone())), ty)),
                            Box::new(Expr::Bool(true)),
                        ))
                    })
                    .collect();
                if let Some(condition) = checks
                    .into_iter()
                    .reduce(|a, b| Expr::Binop(BinOp::And, Box::new(a), Box::new(b)))
                {
                    body = vec![Entry::WhenGenerator(WhenGenerator {
                        condition,
                        body: body.into(),
                        else_body: None,
                    })];
                }
                let key_var = key_var.map(|(name, _)| name);
                let val_var = val_var.0;
                entries.push(Entry::ForGenerator(ForGenerator {
                    key_var,
                    val_var,
                    collection,
                    body: body.into(),
                }));
            }
            TokenKind::KwTypeAlias => {
                entries.push(self.parse_type_alias(MemberHeader::default())?);
            }
            TokenKind::KwClass => {
                entries.push(self.parse_class(MemberHeader::default())?);
            }
            kind if matches!(kind, TokenKind::At) || token_modifier(kind).is_some() => {
                let header = self.parse_member_header()?;
                if matches!(self.peek(), TokenKind::KwFunction) {
                    let entry = self.parse_object_method(header, in_for)?;
                    entries.extend(entry);
                } else if matches!(self.peek(), TokenKind::KwTypeAlias) {
                    entries.push(self.parse_type_alias(header)?);
                } else if matches!(self.peek(), TokenKind::KwClass) {
                    entries.push(self.parse_class(header)?);
                } else {
                    let entry = self.parse_object_property(header, in_for)?;
                    entries.push(entry);
                }
            }
            _ => {
                let expr = self.parse_expr()?;
                entries.push(Entry::Elem(expr));
            }
        }
        Ok(())
    }

    /// Parse the body of a generator, which can't have parameters.
    fn parse_generator_body(&mut self, in_for: bool) -> Result<Vec<Entry>> {
        let body = self.parse_object_body(in_for)?;
        Ok(body.entries)
    }

    /// Parse `= expr` or `{ ... }` after an entry key or predicate.
    fn parse_entry_value(&mut self, key: &Expr) -> Result<Expr> {
        if matches!(self.peek(), TokenKind::Equals) {
            self.advance();
            return self.parse_expr();
        }
        match self.parse_body_list()? {
            BodyList::Plain(entries) => Ok(Expr::ObjectBody(entries.into())),
            BodyList::Amended(f) => Ok(f(Expr::Index(
                Box::new(Expr::Ident("super".into())),
                Box::new(key.clone()),
            ))),
        }
    }

    fn for_generator_error(&self, what: &str) -> Error {
        self.parse_error(format!(
            "A for-generator cannot generate object {what} (only entries and elements)."
        ))
    }

    fn parse_object_property(&mut self, header: MemberHeader, in_for: bool) -> Result<Entry> {
        if in_for {
            return Err(self.for_generator_error("properties"));
        }
        self.check_modifiers(&header, MemberKind::ObjectMember)?;
        let local = header.has(Modifier::Local);
        let name = self.expect_ident()?;
        let type_ann = if matches!(self.peek(), TokenKind::Colon) {
            self.advance();
            let type_offset = self.peek_tok().offset;
            let ty = self.parse_type()?;
            if !local {
                return Err(self.error_at(
                    type_offset,
                    "A non-local object property cannot have a type annotation.",
                ));
            }
            Some(ty)
        } else {
            None
        };
        let (mut value, body) = if type_ann.is_some() || matches!(self.peek(), TokenKind::Equals) {
            self.expect(&TokenKind::Equals, "=")?;
            (Some(self.parse_expr()?), None)
        } else {
            if local && matches!(self.peek(), TokenKind::LBrace) {
                return Err(self.local_amend_error());
            }
            self.parse_property_bodies(&name)?
        };
        // A declared Mapping, Listing, class, or alias makes an untyped
        // `new { ... }` construct that declared type, so nested entry bodies
        // retain their value-type semantics.
        if let (Some(value), Some(ty)) = (&mut value, &type_ann)
            && infers_new(ty)
        {
            infer_method_return_new(value, ty);
        }
        Ok(Entry::Property(std::sync::Arc::new(Property {
            modifiers: header.modifiers(),
            annotations: header.annotations,
            name,
            type_ann,
            value,
            body: body.map(Into::into),
            is_method: false,
        })))
    }

    fn parse_object_method(&mut self, header: MemberHeader, in_for: bool) -> Result<Option<Entry>> {
        if in_for {
            return Err(self.for_generator_error("methods"));
        }
        if !header.has(Modifier::Local) {
            return Err(self.parse_error(
                "Method needs a `local` modifier because it is defined in an object, not a class.",
            ));
        }
        self.parse_method(header, MemberKind::ObjectMember)
    }

    // ---- types ----

    fn parse_type(&mut self) -> Result<TypeExpr> {
        let depth = self.depth;
        self.deepen()?;
        let result = self.parse_type_inner();
        self.depth = depth;
        result
    }

    fn parse_type_inner(&mut self) -> Result<TypeExpr> {
        let start = self.peek_tok().offset;
        let mut default_index = None;
        if matches!(self.peek(), TokenKind::Star) {
            self.advance();
            default_index = Some(0);
        }
        let first = self.parse_type_atom()?;
        if !matches!(self.peek(), TokenKind::Pipe) {
            if default_index.is_some() {
                return Err(self.error_at(start, "Only type unions can have a default marker (*)."));
            }
            return Ok(first);
        }
        let mut variants = vec![first];
        while matches!(self.peek(), TokenKind::Pipe) {
            self.advance();
            if matches!(self.peek(), TokenKind::Star) {
                if default_index.is_some() {
                    return Err(
                        self.parse_error("A type union cannot have more than one default type.")
                    );
                }
                default_index = Some(variants.len());
                self.advance();
            }
            variants.push(self.parse_type_atom()?);
        }
        if let Some(index) = default_index {
            let ty = std::mem::replace(&mut variants[index], TypeExpr::Named(String::new()));
            variants[index] = mark_default_type(ty);
        }
        Ok(TypeExpr::Union(variants))
    }

    fn parse_type_atom(&mut self) -> Result<TypeExpr> {
        let ty = match self.peek().clone() {
            TokenKind::KwModule => {
                self.advance();
                let mut name = "module".to_string();
                while matches!(self.peek(), TokenKind::Dot) {
                    self.advance();
                    name.push('.');
                    name.push_str(&self.expect_ident()?);
                }
                TypeExpr::Named(name)
            }
            TokenKind::KwThis => {
                self.advance();
                let mut name = "this".to_string();
                while matches!(self.peek(), TokenKind::Dot) {
                    self.advance();
                    name.push('.');
                    name.push_str(&self.expect_ident()?);
                }
                TypeExpr::Named(name)
            }
            TokenKind::LParen => {
                self.advance();
                let mut params = Vec::new();
                if !matches!(self.peek(), TokenKind::RParen) {
                    loop {
                        params.push(self.parse_type()?);
                        if !matches!(self.peek(), TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                        if matches!(self.peek(), TokenKind::RParen) {
                            break;
                        }
                    }
                }
                let rparen_offset = self.peek_tok().offset;
                self.expect2(&TokenKind::RParen, ")", ",")?;
                if matches!(self.peek(), TokenKind::Arrow) || params.len() != 1 {
                    if params.is_empty() && !matches!(self.peek(), TokenKind::Arrow) {
                        return Err(
                            self.error_at(rparen_offset, "Unexpected token `)`. Expected a type.")
                        );
                    }
                    self.expect(&TokenKind::Arrow, "->")?;
                    // A function type `(A, B) -> R` is the class `Function2<A, B, R>`.
                    let ret = self.parse_type()?;
                    let name = format!("Function{}", params.len());
                    params.push(ret);
                    return Ok(TypeExpr::Generic(name, params));
                }
                params.pop().expect("one parameter")
            }
            TokenKind::StringLit(s) => {
                self.advance();
                TypeExpr::Named(format!("\"{s}\""))
            }
            TokenKind::InterpolatedString(_) => {
                return Err(self.parse_error("String constant cannot have interpolated values."));
            }
            TokenKind::Null => {
                self.advance();
                TypeExpr::Named("Null".to_string())
            }
            TokenKind::Ident(ref name)
                if matches!(name.as_str(), "unknown" | "nothing")
                    && !self.is_quoted(self.peek_tok()) =>
            {
                let name = name.clone();
                self.advance();
                TypeExpr::Named(name)
            }
            TokenKind::Ident(ref name) if name == "outer" && !self.is_quoted(self.peek_tok()) => {
                self.advance();
                let mut name = "outer".to_string();
                while matches!(self.peek(), TokenKind::Dot) {
                    self.advance();
                    name.push('.');
                    name.push_str(&self.expect_ident()?);
                }
                TypeExpr::Named(name)
            }
            TokenKind::Ident(_) => {
                let name = self.parse_qualified_ident()?;
                if matches!(self.peek(), TokenKind::Lt) {
                    self.advance();
                    let mut args = Vec::new();
                    loop {
                        args.push(self.parse_type()?);
                        if !matches!(self.peek(), TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                        if matches!(self.peek(), TokenKind::Gt) {
                            break;
                        }
                    }
                    self.expect2(&TokenKind::Gt, ">", ",")?;
                    TypeExpr::Generic(name, args)
                } else {
                    TypeExpr::Named(name)
                }
            }
            _ => {
                let text = self.token_text(self.peek_tok());
                return Err(
                    self.parse_error(format!("Unexpected token `{text}`. Expected a type."))
                );
            }
        };
        self.parse_type_end(ty)
    }

    /// Parse `?` and `(constraints)` suffixes of a type.
    fn parse_type_end(&mut self, mut ty: TypeExpr) -> Result<TypeExpr> {
        loop {
            if matches!(self.peek(), TokenKind::QuestionMark) {
                self.advance();
                ty = TypeExpr::Nullable(Box::new(ty));
            } else if matches!(self.peek(), TokenKind::LParen) && self.on_same_line() {
                self.advance();
                let mut constraint: Option<Expr> = None;
                loop {
                    let next = self.parse_expr()?;
                    constraint = Some(match constraint {
                        Some(prev) => Expr::Binop(BinOp::And, Box::new(prev), Box::new(next)),
                        None => next,
                    });
                    if !matches!(self.peek(), TokenKind::Comma) {
                        break;
                    }
                    self.advance();
                    if matches!(self.peek(), TokenKind::RParen) {
                        break;
                    }
                    self.deepen()?;
                }
                self.expect2(&TokenKind::RParen, ")", ",")?;
                ty = TypeExpr::Constrained(
                    type_expr_runtime_name(&ty),
                    Box::new(constraint.expect("at least one constraint")),
                );
            } else {
                return Ok(ty);
            }
        }
    }

    // ---- expressions ----

    fn parse_expr(&mut self) -> Result<Expr> {
        let depth = self.depth;
        self.deepen()?;
        let result = self.parse_binary(1);
        self.depth = depth;
        result
    }

    fn deepen(&mut self) -> Result<()> {
        if self.depth >= MAX_NESTING_DEPTH {
            return Err(self.parse_error(format!(
                "expressions cannot be nested more than {MAX_NESTING_DEPTH} levels deep"
            )));
        }
        self.depth += 1;
        Ok(())
    }

    /// Precedence climbing over pkl's binary operators.
    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr> {
        let mut expr = self.parse_unary()?;
        while let Some((op, prec, left_assoc)) = binary_operator(self.peek()) {
            if prec < min_prec {
                break;
            }
            // `-` on a new line (or after `;`) starts a new expression.
            if matches!(self.peek(), TokenKind::Minus) && !self.on_same_line() {
                break;
            }
            self.advance();
            self.deepen()?;
            expr = match op {
                BinaryOp::Is => Expr::Is(Box::new(expr), self.parse_type()?),
                BinaryOp::As => Expr::As(Box::new(expr), self.parse_type()?),
                BinaryOp::Dot | BinaryOp::QDot => {
                    let field = self.expect_field_ident()?;
                    let access = if matches!(op, BinaryOp::Dot) {
                        Expr::Field(Box::new(expr), field)
                    } else {
                        Expr::NullSafeField(Box::new(expr), field)
                    };
                    self.parse_call_args(access)?
                }
                BinaryOp::Bin(bin) => {
                    let next_min = if left_assoc { prec + 1 } else { prec };
                    let rhs = self.parse_binary(next_min)?;
                    Expr::Binop(bin, Box::new(expr), Box::new(rhs))
                }
            };
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        match self.peek() {
            TokenKind::Minus => {
                self.advance();
                if matches!(self.peek(), TokenKind::Minus | TokenKind::Bang) {
                    return Err(self.parse_error("unexpected token in prefix expression"));
                }
                // 2^63 is only a valid integer literal when this unary minus
                // supplies its sign. Keeping it distinct from `IntLit` makes
                // `1 - 9223372036854775808` reject instead of treating the
                // right-hand side as `i64::MIN`.
                if matches!(self.peek(), TokenKind::MinIntLit) {
                    self.advance();
                    return Ok(Expr::Int(i64::MIN));
                }
                let operand = self.parse_postfix()?;
                Ok(Expr::Unop(UnOp::Neg, Box::new(operand)))
            }
            TokenKind::Bang => {
                self.advance();
                if matches!(self.peek(), TokenKind::Minus | TokenKind::Bang) {
                    return Err(self.parse_error("unexpected token in prefix expression"));
                }
                Ok(Expr::Unop(UnOp::Not, Box::new(self.parse_postfix()?)))
            }
            _ => self.parse_postfix(),
        }
    }

    /// If the current token is `(` on the same line, parse call arguments
    /// and wrap `callee` in a call.
    fn parse_call_args(&mut self, callee: Expr) -> Result<Expr> {
        if !matches!(self.peek(), TokenKind::LParen) || !self.on_same_line() {
            return Ok(callee);
        }
        self.advance();
        let mut args = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if !matches!(self.peek(), TokenKind::Comma) {
                    break;
                }
                self.advance();
                if matches!(self.peek(), TokenKind::RParen) {
                    break;
                }
            }
        }
        self.expect2(&TokenKind::RParen, ")", ",")?;
        Ok(Expr::Call(Box::new(callee), args))
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let (mut expr, amendable) = self.parse_primary()?;
        let mut amendable = amendable;
        loop {
            match self.peek() {
                TokenKind::BangBang => {
                    self.deepen()?;
                    self.advance();
                    expr = Expr::Unop(UnOp::NonNull, Box::new(expr));
                    amendable = false;
                }
                TokenKind::LBrace => {
                    self.deepen()?;
                    if !amendable {
                        return Err(self.parse_error(
                            "Unexpected token: `'{'`.\n\n\
                             If you meant to write an amends expression, wrap the parent in \
                             parentheses.",
                        ));
                    }
                    let body = self.parse_object_body(false)?;
                    expr = amend_expr(expr, body);
                }
                TokenKind::Dot | TokenKind::QuestionDot => {
                    self.deepen()?;
                    let null_safe = matches!(self.peek(), TokenKind::QuestionDot);
                    self.advance();
                    let field = self.expect_field_ident()?;
                    let access = if null_safe {
                        Expr::NullSafeField(Box::new(expr), field)
                    } else {
                        Expr::Field(Box::new(expr), field)
                    };
                    expr = self.parse_call_args(access)?;
                    amendable = false;
                }
                TokenKind::LBracket if self.on_same_line() => {
                    self.deepen()?;
                    self.advance();
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "]")?;
                    expr = Expr::Index(Box::new(expr), Box::new(index));
                    amendable = false;
                }
                TokenKind::LParen if self.on_same_line() => {
                    self.deepen()?;
                    expr = self.parse_call_args(expr)?;
                    amendable = false;
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse a primary expression. The flag says whether an object body may
    /// follow to amend it: only parenthesized, `new` and amend expressions.
    fn parse_primary(&mut self) -> Result<(Expr, bool)> {
        let tok = self.peek_tok();
        let expr = match tok.kind.clone() {
            TokenKind::Null => {
                self.advance();
                Expr::Null
            }
            TokenKind::BoolLit(b) => {
                self.advance();
                Expr::Bool(b)
            }
            TokenKind::IntLit(n) => {
                self.advance();
                Expr::Int(n)
            }
            TokenKind::MinIntLit => {
                let text = self.token_text(tok);
                return Err(self.parse_error(format!("Int literal `{text}` is too large.")));
            }
            TokenKind::FloatLit(f) => {
                self.advance();
                Expr::Float(f)
            }
            TokenKind::StringLit(s) => {
                self.advance();
                Expr::String(s.into())
            }
            TokenKind::InterpolatedString(parts) => {
                self.advance();
                let mut interp_parts = Vec::new();
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            interp_parts.push(StringInterpPart::Literal(s));
                        }
                        StringPart::Tokens(tokens) => {
                            let mut nested = Parser::new(&tokens, self.source, self.name);
                            let expr = nested.parse_expr()?;
                            if !nested.at_eof() {
                                let text = nested.token_text(nested.peek_tok());
                                return Err(nested.parse_error(format!(
                                    "Unexpected token `{text}`. Expected `)`."
                                )));
                            }
                            self.import_exprs.append(&mut nested.import_exprs);
                            interp_parts.push(StringInterpPart::Expr(expr));
                        }
                    }
                }
                Expr::StringInterpolation(interp_parts)
            }
            TokenKind::LParen => return self.parse_paren_or_lambda(),
            TokenKind::KwNew => {
                self.advance();
                let (type_name, generic_params) = self.parse_new_type()?;
                let body = self.parse_object_body(false)?;
                if !body.params.is_empty() {
                    // `new Mixin { x -> ... }` is the function `(x) -> x { ... }`
                    let arg = Expr::Ident(body.params[0].clone());
                    let lambda_body = Expr::Binop(
                        BinOp::Add,
                        Box::new(arg),
                        Box::new(Expr::ObjectBody(body.entries.into())),
                    );
                    return Ok((
                        Expr::Lambda(body.params.into(), std::sync::Arc::new(lambda_body)),
                        true,
                    ));
                }
                return Ok((
                    Expr::New(type_name, body.entries.into(), generic_params),
                    true,
                ));
            }
            TokenKind::KwIf => {
                self.advance();
                self.expect(&TokenKind::LParen, "(")?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen, ")")?;
                let then = self.parse_expr()?;
                self.expect(&TokenKind::KwElse, "else")?;
                let else_ = self.parse_expr()?;
                Expr::If(Box::new(cond), Box::new(then), Box::new(else_))
            }
            TokenKind::KwLet => {
                self.advance();
                self.expect(&TokenKind::LParen, "(")?;
                let (name, ty) = self.parse_typed_parameter()?;
                self.expect(&TokenKind::Equals, "=")?;
                let mut val = self.parse_expr()?;
                if let Some(ty) = ty {
                    // `let (x: T = v)` checks `v` against `T`
                    val = Expr::As(Box::new(val), ty);
                }
                self.expect(&TokenKind::RParen, ")")?;
                let body = self.parse_expr()?;
                Expr::Let(name, Box::new(val), Box::new(body))
            }
            TokenKind::KwThrow => Expr::Throw(Box::new(self.parse_keyword_call()?)),
            TokenKind::KwTrace => self.parse_trace_call()?,
            TokenKind::KwRead => {
                Expr::Read(Box::new(self.parse_keyword_call()?), self.name.to_string())
            }
            TokenKind::KwReadOrNull => {
                Expr::ReadOrNull(Box::new(self.parse_keyword_call()?), self.name.to_string())
            }
            TokenKind::KwReadGlob => {
                Expr::ReadGlob(Box::new(self.parse_keyword_call()?), self.name.to_string())
            }
            TokenKind::KwImport => {
                self.advance();
                let uri = self.parse_import_expr_uri()?;
                self.import_exprs.push(uri.clone());
                Expr::Import(uri, self.name.to_string())
            }
            TokenKind::KwImportStar => {
                self.advance();
                let uri = self.parse_import_expr_uri()?;
                Expr::ImportGlob(uri, self.name.to_string())
            }
            TokenKind::Ident(name) => {
                if let Some(keyword) = self.keyword_at(tok)
                    && keyword != "outer"
                {
                    return Err(self.keyword_error(&keyword));
                }
                self.advance();
                return Ok((self.parse_call_args(Expr::Ident(name))?, false));
            }
            TokenKind::KwThis => {
                self.advance();
                Expr::Ident("this".into())
            }
            TokenKind::KwModule => {
                self.advance();
                Expr::Ident("module".into())
            }
            TokenKind::KwSuper => {
                self.advance();
                if matches!(self.peek(), TokenKind::Dot) {
                    self.advance();
                    let field = self.expect_field_ident()?;
                    let access = Expr::Field(Box::new(Expr::Ident("super".into())), field);
                    self.parse_call_args(access)?
                } else {
                    self.expect(&TokenKind::LBracket, "[")?;
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "]")?;
                    Expr::Index(Box::new(Expr::Ident("super".into())), Box::new(index))
                }
            }
            TokenKind::Eof => {
                return Err(self.error_at(self.prev_end, "Unexpected end of file."));
            }
            _ => {
                let text = self.token_text(tok);
                return Err(self.parse_error(format!("Unexpected token `{text}`.")));
            }
        };
        Ok((expr, false))
    }

    /// Parse `(expr)` after `throw` and `read`.
    fn parse_keyword_call(&mut self) -> Result<Expr> {
        self.advance(); // keyword
        self.expect(&TokenKind::LParen, "(")?;
        let expr = self.parse_expr()?;
        self.expect(&TokenKind::RParen, ")")?;
        Ok(expr)
    }

    fn parse_trace_call(&mut self) -> Result<Expr> {
        self.advance(); // trace
        self.expect(&TokenKind::LParen, "(")?;
        let start = self.pos;
        let expr = self.parse_expr()?;
        let end = self.pos;
        self.expect(&TokenKind::RParen, ")")?;
        let (start, end) = strip_trace_grouping_tokens(self.tokens, start, end);
        let source_start = self.tokens.get(start).map_or(0, |token| token.offset);
        let source_end = self
            .tokens
            .get(end.saturating_sub(1))
            .map_or(self.source.len(), |token| token.end);
        Ok(Expr::Trace(
            Box::new(expr),
            std::sync::Arc::new(TraceSite {
                source: self
                    .source
                    .get(source_start..source_end)
                    .unwrap_or_default()
                    .to_string(),
                module: self.name.to_string(),
                line: self.tokens.get(start).map_or(0, |token| token.line),
            }),
        ))
    }

    /// Parse the class of a `new` expression: its name and the simple names
    /// of its type arguments.
    fn parse_new_type(&mut self) -> Result<(Option<String>, Vec<String>)> {
        if matches!(self.peek(), TokenKind::LBrace) {
            return Ok((None, Vec::new()));
        }
        let offset = self.peek_tok().offset;
        let ty = self.parse_type()?;
        if let TypeExpr::Named(name) | TypeExpr::Generic(name, _) = &ty
            && name.matches('.').count() > 1
        {
            return Err(self.error_at(offset, format!("Invalid type name `{name}`.")));
        }
        if let TypeExpr::Named(name) | TypeExpr::Generic(name, _) = &ty
            && !name.contains('.')
        {
            self.new_types.push((name.clone(), offset));
        }
        Ok(match ty {
            TypeExpr::Generic(name, args) => {
                let mut params = Vec::new();
                for arg in &args {
                    generic_param_names(arg, &mut params);
                }
                (Some(name), params)
            }
            TypeExpr::Named(name) => (Some(name), Vec::new()),
            other => (Some(type_expr_runtime_name(&other)), Vec::new()),
        })
    }

    /// Parse a parenthesized expression or a function literal; `(` is next.
    fn parse_paren_or_lambda(&mut self) -> Result<(Expr, bool)> {
        self.advance(); // (
        let is_lambda = match self.peek() {
            TokenKind::RParen => true,
            TokenKind::Ident(name) if name == "_" && !self.is_quoted(self.peek_tok()) => true,
            TokenKind::Ident(_) => match self.peek_nth(1) {
                TokenKind::Comma | TokenKind::Colon => true,
                TokenKind::RParen => matches!(self.peek_nth(2), TokenKind::Arrow),
                _ => false,
            },
            _ => false,
        };
        if !is_lambda {
            let expr = self.parse_expr()?;
            self.expect(&TokenKind::RParen, ")")?;
            return Ok((expr, true));
        }
        let start = self.prev_end;
        let params = self.parse_parameters(&TokenKind::RParen)?;
        self.expect2(&TokenKind::RParen, ")", ",")?;
        self.expect(&TokenKind::Arrow, "->")?;
        if params.len() > 5 {
            return Err(self.error_at(
                start.saturating_sub(1),
                "Function literals can have at most five parameters.",
            ));
        }
        let body = self.parse_expr()?;
        Ok((
            Expr::Lambda(params.into(), std::sync::Arc::new(body)),
            false,
        ))
    }

    /// Parse the `("uri")` part of an `import(...)` / `import*(...)` expression.
    /// pkl requires the URI to be a constant string literal.
    fn parse_import_expr_uri(&mut self) -> Result<String> {
        self.expect(&TokenKind::LParen, "(")?;
        let uri = self.expect_string_constant()?;
        self.expect(&TokenKind::RParen, ")")?;
        Ok(uri)
    }
}

/// Object bodies following a member, either plain or amending a function.
enum BodyList {
    Plain(Vec<Entry>),
    /// Builds the amended value from the parent value.
    Amended(Box<dyn FnOnce(Expr) -> Expr>),
}

/// Amend `parent` with an object body. A body with parameters amends a
/// function: `f { x -> ... }` is `(x) -> f.apply(x) { ... }`.
fn amend_expr(parent: Expr, body: ObjectBody) -> Expr {
    let entries = Expr::ObjectBody(body.entries.into());
    if body.params.is_empty() {
        return Expr::Binop(BinOp::Add, Box::new(parent), Box::new(entries));
    }
    let args = body.params.iter().cloned().map(Expr::Ident).collect();
    let call = Expr::Call(
        Box::new(Expr::Field(Box::new(parent), "apply".to_string())),
        args,
    );
    Expr::Lambda(
        body.params.into(),
        std::sync::Arc::new(Expr::Binop(BinOp::Add, Box::new(call), Box::new(entries))),
    )
}

/// Collect the class names of a `new` expression's type arguments.
fn generic_param_names(ty: &TypeExpr, out: &mut Vec<String>) {
    match ty {
        TypeExpr::Named(name) | TypeExpr::Generic(name, _) => {
            // Retain `*` on a union's selected alternative. Mapping entry
            // bodies use it to distinguish the default type from another
            // merely permitted alternative.
            out.push(name.clone());
        }
        TypeExpr::Constrained(name, _) => {
            out.push(name.trim_end_matches('?').to_string());
        }
        TypeExpr::Nullable(inner) => generic_param_names(inner, out),
        TypeExpr::Union(variants) => {
            for variant in variants {
                generic_param_names(variant, out);
            }
        }
    }
}

fn collect_type_names(ty: &TypeExpr, out: &mut Vec<String>) {
    match ty {
        TypeExpr::Named(name) | TypeExpr::Constrained(name, _) => {
            out.push(name.trim_start_matches('*').to_string())
        }
        TypeExpr::Nullable(inner) => collect_type_names(inner, out),
        TypeExpr::Union(variants) => {
            for variant in variants {
                collect_type_names(variant, out);
            }
        }
        TypeExpr::Generic(name, args) => {
            out.push(name.clone());
            for arg in args {
                collect_type_names(arg, out);
            }
        }
    }
}

fn constant_entry_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::String(value) => Some(format!("\"{value}\"")),
        Expr::Int(value) => Some(value.to_string()),
        Expr::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn entries_define_type(entries: &[Entry], name: &str) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::ClassDef(entry_name, _, _, body) => {
            entry_name == name || entries_define_type(body, name)
        }
        Entry::TypeAlias(entry_name, _) => entry_name == name,
        Entry::Property(property) => {
            property
                .body
                .as_deref()
                .is_some_and(|body| entries_define_type(body, name))
                || property
                    .value
                    .as_ref()
                    .is_some_and(|value| expr_defines_type(value, name))
        }
        Entry::DynProperty(_, value)
        | Entry::Predicate(_, value)
        | Entry::Spread(value)
        | Entry::Elem(value) => expr_defines_type(value, name),
        Entry::ForGenerator(generator) => entries_define_type(&generator.body, name),
        Entry::WhenGenerator(generator) => {
            entries_define_type(&generator.body, name)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(|body| entries_define_type(body, name))
        }
    })
}

fn expr_defines_type(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::New(_, body, _) | Expr::ObjectBody(body) | Expr::InferredNew(_, body) => {
            entries_define_type(body, name)
        }
        Expr::Field(value, _)
        | Expr::NullSafeField(value, _)
        | Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::Trace(value, _)
        | Expr::Read(value, _)
        | Expr::ReadOrNull(value, _)
        | Expr::ReadGlob(value, _)
        | Expr::Is(value, _)
        | Expr::As(value, _) => expr_defines_type(value, name),
        Expr::Lambda(_, value) => expr_defines_type(value, name),
        Expr::Index(left, right) | Expr::Binop(_, left, right) => {
            expr_defines_type(left, name) || expr_defines_type(right, name)
        }
        Expr::Call(callee, args) => {
            expr_defines_type(callee, name) || args.iter().any(|arg| expr_defines_type(arg, name))
        }
        Expr::If(condition, then_expr, else_expr) => {
            expr_defines_type(condition, name)
                || expr_defines_type(then_expr, name)
                || expr_defines_type(else_expr, name)
        }
        Expr::Let(_, value, body) => {
            expr_defines_type(value, name) || expr_defines_type(body, name)
        }
        Expr::StringInterpolation(parts) => parts.iter().any(
            |part| matches!(part, StringInterpPart::Expr(value) if expr_defines_type(value, name)),
        ),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Ident(_)
        | Expr::Import(_, _)
        | Expr::ImportGlob(_, _) => false,
    }
}

fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

fn required_string(version: &(u64, u64, u64)) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

fn constant_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::String(value) => Some(value.to_string()),
        Expr::Binop(BinOp::Add, left, right) => {
            Some(constant_string(left)? + &constant_string(right)?)
        }
        Expr::StringInterpolation(parts) => parts
            .iter()
            .map(|part| match part {
                StringInterpPart::Literal(value) => Some(value.to_string()),
                StringInterpPart::Expr(expr) => constant_string(expr),
            })
            .collect(),
        _ => None,
    }
}

/// Drop grouping parentheses only when they enclose the complete trace
/// argument. Token offsets then give the argument's exact source section,
/// without mistaking comment-like text inside a string for a comment.
fn strip_trace_grouping_tokens(
    tokens: &[Token],
    mut start: usize,
    mut end: usize,
) -> (usize, usize) {
    while matches!(
        tokens.get(start).map(|token| &token.kind),
        Some(TokenKind::LParen)
    ) {
        let mut depth = 0usize;
        let mut close = None;
        for (index, token) in tokens.iter().enumerate().take(end).skip(start) {
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        if close != Some(end - 1) {
            break;
        }
        start += 1;
        end -= 1;
    }
    (start, end)
}

fn mark_default_type(ty: TypeExpr) -> TypeExpr {
    TypeExpr::Named(format!("*{}", type_expr_runtime_name(&ty)))
}
