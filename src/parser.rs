use crate::error::{Error, Result};
use crate::lexer::{Token, TokenKind};

#[derive(Debug, Clone, PartialEq)]
pub enum StringInterpPart {
    Literal(String),
    Expr(Expr),
}

/// A pkl module (top-level file).
/// An annotation: `@Name { body }` or `@Name`
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub name: String,
    pub body: Vec<Entry>,
}

/// A pkl module (top-level file).
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub amends: Option<String>,
    pub extends: Option<String>,
    pub imports: Vec<Import>,
    pub annotations: Vec<Annotation>,
    pub body: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub uri: String,
    pub alias: Option<String>,
    /// `import*` glob import — result is a Mapping<String, Module>
    pub is_glob: bool,
}

/// A top-level or object entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// `key = expr` or `key: Type = expr`
    Property(Property),
    /// `["key"] = expr` (dynamic key)
    DynProperty(Expr, Expr),
    /// `for (k, v in collection) { ... }`
    ForGenerator(ForGenerator),
    /// `when (cond) { ... }`
    WhenGenerator(WhenGenerator),
    /// `...spread`
    Spread(Expr),
    /// Bare element expression (used in Listing bodies)
    Elem(Expr),
    /// Class definition: `class Name [extends Parent] { properties... }`
    /// Fields: (name, optional_parent, body)
    ClassDef(String, Option<String>, Vec<Entry>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub annotations: Vec<Annotation>,
    pub modifiers: Vec<Modifier>,
    pub name: String,
    pub type_ann: Option<TypeExpr>,
    pub value: Option<Expr>,
    /// Object body amendment: `foo { ... }` (no `=`)
    pub body: Option<Vec<Entry>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modifier {
    Local,
    Const,
    Fixed,
    Hidden,
    Abstract,
    Open,
    External,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    Named(String),
    Nullable(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    Generic(String, Vec<TypeExpr>),
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Ident(String),
    /// `new TypeName? { entries... }`
    New(Option<String>, Vec<Entry>),
    /// `expr.field`
    Field(Box<Expr>, String),
    /// `expr[key]`
    Index(Box<Expr>, Box<Expr>),
    /// `expr(args...)`
    Call(Box<Expr>, Vec<Expr>),
    /// `if (cond) then else`
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `let (name = val) body`
    Let(String, Box<Expr>, Box<Expr>),
    /// `expr is Type`
    Is(Box<Expr>, TypeExpr),
    /// `expr as Type`
    As(Box<Expr>, TypeExpr),
    /// Binary operation
    Binop(BinOp, Box<Expr>, Box<Expr>),
    /// Unary operation
    Unop(UnOp, Box<Expr>),
    /// Object/listing literal — anonymous `{ ... }`
    ObjectBody(Vec<Entry>),
    /// String interpolation: alternating literal strings and expressions
    StringInterpolation(Vec<StringInterpPart>),
    /// Null-safe field access: `expr?.field`
    NullSafeField(Box<Expr>, String),
    /// Lambda: `(params) -> body`
    Lambda(Vec<String>, Box<Expr>),
    /// `throw("msg")`
    Throw(Box<Expr>),
    /// `trace(expr)`
    Trace(Box<Expr>),
    /// `read("uri")`
    Read(Box<Expr>),
    /// `read?("uri")` — returns null on failure
    ReadOrNull(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    IntDiv,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    NullCoalesce,
    Pipe,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    NonNull,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForGenerator {
    pub key_var: Option<String>,
    pub val_var: String,
    pub collection: Expr,
    pub body: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhenGenerator {
    pub condition: Expr,
    pub body: Vec<Entry>,
    pub else_body: Option<Vec<Entry>>,
}

/// Collect all import URIs from a token stream (fast path, no full parse needed).
pub fn collect_imports(tokens: &[Token]) -> Vec<String> {
    let mut imports = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i].kind {
            TokenKind::KwAmends | TokenKind::KwImport | TokenKind::KwImportStar => {
                if let Some(TokenKind::StringLit(uri)) = tokens.get(i + 1).map(|t| &t.kind) {
                    imports.push(uri.clone());
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    imports
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
    p.parse_expr()
}

struct Parser<'a> {
    tokens: &'a [Token],
    source: String,
    name: String,
    pos: usize,
    /// Line of the last consumed token (used for newline-sensitive parsing).
    last_line: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token], source: &str, name: &str) -> Self {
        Self {
            tokens,
            source: source.to_string(),
            name: name.to_string(),
            pos: 0,
            last_line: 1,
        }
    }

    fn parse_error(&self, message: impl Into<String>) -> Error {
        let tok = self.peek_tok();
        Error::Parse {
            src: miette::NamedSource::new(&self.name, self.source.clone()),
            span: miette::SourceOffset::from(tok.offset),
            message: message.into(),
        }
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_tok(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        self.last_line = tok.line;
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, kind: &TokenKind) -> Result<&Token> {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            Ok(self.advance())
        } else {
            Err(self.parse_error(format!("expected {:?}, got {:?}", kind, self.peek())))
        }
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    /// Parse annotations: `@Name { body }` or `@Name`
    fn parse_annotations(&mut self) -> Result<Vec<Annotation>> {
        let mut annotations = Vec::new();
        while matches!(self.peek(), TokenKind::At) {
            self.advance(); // @
            // Parse annotation name (possibly dotted: @Foo.Bar)
            let mut name = self.expect_ident()?;
            while matches!(self.peek(), TokenKind::Dot) {
                self.advance();
                let part = self.expect_ident()?;
                name = format!("{name}.{part}");
            }
            // Parse annotation body if present
            let body = if matches!(self.peek(), TokenKind::LBrace) {
                self.advance();
                let entries = self.parse_entries()?;
                self.expect(&TokenKind::RBrace)?;
                entries
            } else if matches!(self.peek(), TokenKind::LParen) {
                // Annotation with parens: @Foo("arg") — parse as a single-element body
                self.advance();
                let mut entries = Vec::new();
                while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                    let expr = self.parse_expr()?;
                    entries.push(Entry::Elem(expr));
                    if matches!(self.peek(), TokenKind::Comma) {
                        self.advance();
                    }
                }
                self.expect(&TokenKind::RParen)?;
                entries
            } else {
                Vec::new()
            };
            annotations.push(Annotation { name, body });
        }
        Ok(annotations)
    }

    fn parse_module(&mut self) -> Result<Module> {
        let mut amends = None;
        let mut imports = Vec::new();

        // Parse module-level annotations (e.g. @ModuleInfo)
        let annotations = self.parse_annotations()?;

        // Parse header: module declaration, amends, imports
        // Skip `module <name>` declaration if present
        if matches!(self.peek(), TokenKind::KwModule) {
            self.advance();
            // Skip module name (may be dotted like `hk.Config`)
            while matches!(self.peek(), TokenKind::Ident(_)) {
                self.advance();
                if matches!(self.peek(), TokenKind::Dot) {
                    self.advance();
                } else {
                    break;
                }
            }
        }

        let mut extends = None;

        loop {
            match self.peek().clone() {
                TokenKind::KwAmends => {
                    self.advance();
                    let uri = self.expect_string()?;
                    amends = Some(uri);
                }
                TokenKind::KwExtends => {
                    self.advance();
                    let uri = self.expect_string()?;
                    extends = Some(uri);
                }
                TokenKind::KwImport | TokenKind::KwImportStar => {
                    let is_glob = matches!(self.peek(), TokenKind::KwImportStar);
                    self.advance();
                    let uri = self.expect_string()?;
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
                _ => break,
            }
        }

        let body = self.parse_entries()?;
        Ok(Module {
            amends,
            extends,
            imports,
            annotations,
            body,
        })
    }

    fn parse_entries(&mut self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        while !self.at_eof() && !matches!(self.peek(), TokenKind::RBrace) {
            let entry_annotations = self.parse_annotations()?;
            // Parse class definitions; skip typealias/function declarations
            if matches!(self.peek(), TokenKind::KwClass) {
                self.advance(); // consume 'class'
                let name = self.expect_ident()?;
                // Skip optional type params <...>
                if matches!(self.peek(), TokenKind::Lt) {
                    self.skip_generic_params()?;
                }
                // Parse optional extends clause
                let parent = if matches!(self.peek(), TokenKind::KwExtends) {
                    self.advance();
                    let mut parent_name = self.expect_ident()?;
                    // Handle dotted parent names: extends Foo.Bar
                    while matches!(self.peek(), TokenKind::Dot) {
                        self.advance();
                        let part = self.expect_ident()?;
                        parent_name.push('.');
                        parent_name.push_str(&part);
                    }
                    // Skip optional type params on parent
                    if matches!(self.peek(), TokenKind::Lt) {
                        self.skip_generic_params()?;
                    }
                    Some(parent_name)
                } else {
                    None
                };
                if matches!(self.peek(), TokenKind::LBrace) {
                    self.advance();
                    let body = self.parse_entries()?;
                    self.expect(&TokenKind::RBrace)?;
                    entries.push(Entry::ClassDef(name, parent, body));
                }
                continue;
            }
            if matches!(self.peek(), TokenKind::KwTypeAlias | TokenKind::KwFunction) {
                self.skip_declaration();
                continue;
            }
            // Also skip modifier-prefixed function/typealias declarations
            if self.peek_is_modifier() && self.peek_past_modifiers_is_decl() {
                self.skip_modifiers();
                self.skip_declaration();
                continue;
            }
            if self.at_eof() || matches!(self.peek(), TokenKind::RBrace) {
                break;
            }
            let mut entry = self.parse_entry()?;
            // Attach annotations to the parsed property
            if !entry_annotations.is_empty()
                && let Entry::Property(ref mut prop) = entry
            {
                prop.annotations = entry_annotations;
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Skip generic type parameters: `<Type, Type<Nested>, ...>`
    /// Assumes the current token is `<`. Consumes through the matching `>`.
    fn skip_generic_params(&mut self) -> Result<()> {
        let mut depth = 0;
        loop {
            match self.peek() {
                TokenKind::Lt => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::Gt => {
                    depth -= 1;
                    self.advance();
                    if depth == 0 {
                        break;
                    }
                }
                TokenKind::Eof => {
                    return Err(self.parse_error("unclosed generic type parameters"));
                }
                _ => {
                    self.advance();
                }
            }
        }
        Ok(())
    }

    fn peek_is_modifier(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::KwLocal
                | TokenKind::KwConst
                | TokenKind::KwFixed
                | TokenKind::KwHidden
                | TokenKind::KwAbstract
                | TokenKind::KwOpen
                | TokenKind::KwExternal
        )
    }

    fn peek_past_modifiers_is_decl(&self) -> bool {
        let mut i = self.pos;
        while i < self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::KwLocal
                | TokenKind::KwConst
                | TokenKind::KwFixed
                | TokenKind::KwHidden
                | TokenKind::KwAbstract
                | TokenKind::KwOpen
                | TokenKind::KwExternal => i += 1,
                TokenKind::KwFunction | TokenKind::KwTypeAlias => return true,
                _ => return false,
            }
        }
        false
    }

    fn skip_modifiers(&mut self) {
        while self.peek_is_modifier() {
            self.advance();
        }
    }

    /// Skip a class, typealias, or function declaration.
    fn skip_declaration(&mut self) {
        let start_line = self.peek_tok().line;
        self.advance(); // class/typealias/function keyword
        let mut brace_depth = 0;
        let mut paren_depth = 0;
        loop {
            if self.at_eof() {
                break;
            }
            match self.peek() {
                TokenKind::LParen => {
                    paren_depth += 1;
                    self.advance();
                }
                TokenKind::RParen => {
                    paren_depth -= 1;
                    self.advance();
                }
                TokenKind::LBrace => {
                    brace_depth += 1;
                    self.advance();
                }
                TokenKind::RBrace if brace_depth > 0 => {
                    brace_depth -= 1;
                    self.advance();
                    if brace_depth == 0 {
                        break;
                    }
                }
                TokenKind::RBrace => break,
                _ if brace_depth == 0 && paren_depth == 0 => {
                    let tok = self.peek_tok();
                    let on_new_line = tok.line > start_line && tok.line > self.last_line;
                    if on_new_line {
                        let next = self.peek().clone();
                        if matches!(
                            next,
                            TokenKind::At
                                | TokenKind::KwClass
                                | TokenKind::KwTypeAlias
                                | TokenKind::KwFunction
                                | TokenKind::KwLocal
                                | TokenKind::KwConst
                                | TokenKind::KwFixed
                                | TokenKind::KwHidden
                                | TokenKind::KwAbstract
                                | TokenKind::KwOpen
                                | TokenKind::KwExternal
                                | TokenKind::Ident(_)
                                | TokenKind::LBracket
                        ) {
                            break;
                        }
                    }
                    self.advance();
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn parse_entry(&mut self) -> Result<Entry> {
        match self.peek().clone() {
            TokenKind::LBracket => {
                // Dynamic property: ["key"] = expr  OR  ["key"] { body }
                self.advance();
                let key = self.parse_expr()?;
                self.expect(&TokenKind::RBracket)?;
                if matches!(self.peek(), TokenKind::LBrace) {
                    self.advance();
                    let entries = self.parse_entries()?;
                    self.expect(&TokenKind::RBrace)?;
                    Ok(Entry::DynProperty(key, Expr::ObjectBody(entries)))
                } else {
                    self.expect(&TokenKind::Equals)?;
                    let val = self.parse_expr()?;
                    Ok(Entry::DynProperty(key, val))
                }
            }
            TokenKind::KwFor => {
                self.advance(); // consume 'for'
                self.expect(&TokenKind::LParen)?;
                // for (k, v in collection) or (v in collection)
                let first = self.expect_ident()?;
                let (key_var, val_var, collection) = if matches!(self.peek(), TokenKind::Comma) {
                    self.advance();
                    let v = self.expect_ident()?;
                    self.expect(&TokenKind::KwIn)?;
                    let coll = self.parse_expr()?;
                    (Some(first), v, coll)
                } else {
                    self.expect(&TokenKind::KwIn)?;
                    let coll = self.parse_expr()?;
                    (None, first, coll)
                };
                self.expect(&TokenKind::RParen)?;
                self.expect(&TokenKind::LBrace)?;
                let body = self.parse_entries()?;
                self.expect(&TokenKind::RBrace)?;
                Ok(Entry::ForGenerator(ForGenerator {
                    key_var,
                    val_var,
                    collection,
                    body,
                }))
            }
            TokenKind::KwWhen => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                self.expect(&TokenKind::LBrace)?;
                let body = self.parse_entries()?;
                self.expect(&TokenKind::RBrace)?;
                let else_body = if matches!(self.peek(), TokenKind::KwElse) {
                    self.advance();
                    self.expect(&TokenKind::LBrace)?;
                    let eb = self.parse_entries()?;
                    self.expect(&TokenKind::RBrace)?;
                    Some(eb)
                } else {
                    None
                };
                Ok(Entry::WhenGenerator(WhenGenerator {
                    condition: cond,
                    body,
                    else_body,
                }))
            }
            TokenKind::DotDotDot => {
                self.advance();
                let expr = self.parse_expr()?;
                Ok(Entry::Spread(expr))
            }
            _ => {
                // Check for bare element (used in Listing bodies): literal values,
                // or identifiers not followed by = or { or : (which would be properties)
                let is_bare_literal = matches!(
                    self.peek(),
                    TokenKind::StringLit(_)
                        | TokenKind::InterpolatedString(_)
                        | TokenKind::IntLit(_)
                        | TokenKind::FloatLit(_)
                        | TokenKind::BoolLit(_)
                        | TokenKind::Null
                        | TokenKind::KwNew
                );
                let is_bare_ident = matches!(self.peek(), TokenKind::Ident(_))
                    && self.pos + 1 < self.tokens.len()
                    && !matches!(
                        self.tokens[self.pos + 1].kind,
                        TokenKind::Equals | TokenKind::LBrace | TokenKind::Colon
                    );
                if is_bare_literal || is_bare_ident {
                    let expr = self.parse_expr()?;
                    return Ok(Entry::Elem(expr));
                }

                // Property: [modifiers] name [: Type] [= expr | { body }]
                let mut modifiers = Vec::new();
                loop {
                    match self.peek() {
                        TokenKind::KwLocal => {
                            self.advance();
                            modifiers.push(Modifier::Local);
                        }
                        TokenKind::KwConst => {
                            self.advance();
                            modifiers.push(Modifier::Const);
                        }
                        TokenKind::KwFixed => {
                            self.advance();
                            modifiers.push(Modifier::Fixed);
                        }
                        TokenKind::KwHidden => {
                            self.advance();
                            modifiers.push(Modifier::Hidden);
                        }
                        TokenKind::KwAbstract => {
                            self.advance();
                            modifiers.push(Modifier::Abstract);
                        }
                        TokenKind::KwOpen => {
                            self.advance();
                            modifiers.push(Modifier::Open);
                        }
                        TokenKind::KwExternal => {
                            self.advance();
                            modifiers.push(Modifier::External);
                        }
                        _ => break,
                    }
                }

                let name = self.expect_ident()?;

                let type_ann = if matches!(self.peek(), TokenKind::Colon) {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };

                let (value, body) = if matches!(self.peek(), TokenKind::Equals) {
                    self.advance();
                    (Some(self.parse_expr()?), None)
                } else if matches!(self.peek(), TokenKind::LBrace) {
                    self.advance();
                    let entries = self.parse_entries()?;
                    self.expect(&TokenKind::RBrace)?;
                    (None, Some(entries))
                } else {
                    // Bare property with no value (type-only declaration)
                    (None, None)
                };

                Ok(Entry::Property(Property {
                    annotations: Vec::new(), // filled by parse_entries if present
                    modifiers,
                    name,
                    type_ann,
                    value,
                    body,
                }))
            }
        }
    }

    fn parse_type(&mut self) -> Result<TypeExpr> {
        let base = match self.peek().clone() {
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_type()?;
                self.expect(&TokenKind::RParen)?;
                inner
            }
            TokenKind::StringLit(s) => {
                self.advance();
                TypeExpr::Named(s)
            }
            TokenKind::Null => {
                self.advance();
                TypeExpr::Named("Null".to_string())
            }
            TokenKind::KwModule => {
                self.advance();
                TypeExpr::Named("module".to_string())
            }
            TokenKind::Ident(name) => {
                self.advance();
                if matches!(self.peek(), TokenKind::Lt) {
                    self.advance();
                    let mut args = vec![self.parse_type()?];
                    while matches!(self.peek(), TokenKind::Comma) {
                        self.advance();
                        args.push(self.parse_type()?);
                    }
                    self.expect(&TokenKind::Gt)?;
                    TypeExpr::Generic(name, args)
                } else {
                    TypeExpr::Named(name)
                }
            }
            tok => {
                return Err(self.parse_error(format!("expected type, got {:?}", tok)));
            }
        };

        // Nullable: Type?
        let t = if matches!(self.peek(), TokenKind::QuestionMark) {
            self.advance();
            TypeExpr::Nullable(Box::new(base))
        } else {
            base
        };

        // Union: Type | Type
        if matches!(self.peek(), TokenKind::Pipe) {
            let mut variants = vec![t];
            while matches!(self.peek(), TokenKind::Pipe) {
                self.advance();
                variants.push(self.parse_type()?);
            }
            Ok(TypeExpr::Union(variants))
        } else {
            Ok(t)
        }
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_pipe()
    }

    fn parse_pipe(&mut self) -> Result<Expr> {
        let mut left = self.parse_null_coalesce()?;
        while matches!(self.peek(), TokenKind::PipeGt) {
            self.advance();
            let right = self.parse_null_coalesce()?;
            left = Expr::Binop(BinOp::Pipe, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_null_coalesce(&mut self) -> Result<Expr> {
        let mut left = self.parse_or()?;
        while matches!(self.peek(), TokenKind::QuestionQuestion) {
            self.advance();
            let right = self.parse_or()?;
            left = Expr::Binop(BinOp::NullCoalesce, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), TokenKind::PipePipe) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Binop(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_compare()?;
        while matches!(self.peek(), TokenKind::AmpAmp) {
            self.advance();
            let right = self.parse_compare()?;
            left = Expr::Binop(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_compare(&mut self) -> Result<Expr> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.peek() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::BangEq => BinOp::Ne,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::LtEq => BinOp::Le,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::GtEq => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_add()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr> {
        let mut left = self.parse_exp()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                TokenKind::TildeSlash => BinOp::IntDiv,
                _ => break,
            };
            self.advance();
            let right = self.parse_exp()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_exp(&mut self) -> Result<Expr> {
        let base = self.parse_unary()?;
        if matches!(self.peek(), TokenKind::StarStar) {
            self.advance();
            // Right-associative: recurse into parse_exp
            let exp = self.parse_exp()?;
            Ok(Expr::Binop(BinOp::Pow, Box::new(base), Box::new(exp)))
        } else {
            Ok(base)
        }
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        match self.peek() {
            TokenKind::Minus => {
                self.advance();
                Ok(Expr::Unop(UnOp::Neg, Box::new(self.parse_postfix()?)))
            }
            TokenKind::Bang => {
                self.advance();
                Ok(Expr::Unop(UnOp::Not, Box::new(self.parse_postfix()?)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                TokenKind::Dot => {
                    self.advance();
                    let field = self.expect_ident()?;
                    expr = Expr::Field(Box::new(expr), field);
                }
                TokenKind::QuestionDot => {
                    self.advance();
                    let field = self.expect_ident()?;
                    expr = Expr::NullSafeField(Box::new(expr), field);
                }
                TokenKind::LBracket => {
                    // Only treat as indexing if on the same line as the expression.
                    // A `[` on a new line is a new dynamic entry, not indexing.
                    if self.peek_tok().line != self.last_line {
                        break;
                    }
                    self.advance();
                    let idx = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket)?;
                    expr = Expr::Index(Box::new(expr), Box::new(idx));
                }
                TokenKind::LParen => {
                    self.advance();
                    let mut args = Vec::new();
                    while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                        args.push(self.parse_expr()?);
                        if matches!(self.peek(), TokenKind::Comma) {
                            self.advance();
                        }
                    }
                    self.expect(&TokenKind::RParen)?;
                    expr = Expr::Call(Box::new(expr), args);
                }
                TokenKind::LBrace => {
                    // Object amendment: expr { ... }
                    self.advance();
                    let entries = self.parse_entries()?;
                    self.expect(&TokenKind::RBrace)?;
                    // Treat as: New with the base expr being amended
                    // For now represent as a field access + body
                    expr = Expr::Binop(
                        BinOp::Add,
                        Box::new(expr),
                        Box::new(Expr::ObjectBody(entries)),
                    );
                }
                TokenKind::KwIs => {
                    self.advance();
                    let ty = self.parse_type()?;
                    expr = Expr::Is(Box::new(expr), ty);
                }
                TokenKind::KwAs => {
                    self.advance();
                    let ty = self.parse_type()?;
                    expr = Expr::As(Box::new(expr), ty);
                }
                TokenKind::BangBang => {
                    self.advance();
                    expr = Expr::Unop(UnOp::NonNull, Box::new(expr));
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        match self.peek().clone() {
            TokenKind::Null => {
                self.advance();
                Ok(Expr::Null)
            }
            TokenKind::BoolLit(b) => {
                self.advance();
                Ok(Expr::Bool(b))
            }
            TokenKind::IntLit(n) => {
                self.advance();
                Ok(Expr::Int(n))
            }
            TokenKind::FloatLit(f) => {
                self.advance();
                Ok(Expr::Float(f))
            }
            TokenKind::StringLit(s) => {
                self.advance();
                Ok(Expr::String(s))
            }
            TokenKind::InterpolatedString(parts) => {
                self.advance();
                let mut interp_parts = Vec::new();
                for part in parts {
                    match part {
                        crate::lexer::StringPart::Literal(s) => {
                            interp_parts.push(StringInterpPart::Literal(s));
                        }
                        crate::lexer::StringPart::Tokens(tokens) => {
                            let expr = parse_expr_tokens(&tokens, &self.source, &self.name)?;
                            interp_parts.push(StringInterpPart::Expr(expr));
                        }
                    }
                }
                Ok(Expr::StringInterpolation(interp_parts))
            }
            TokenKind::LParen => {
                // Try to parse as lambda: (params) -> body
                let saved_pos = self.pos;
                let saved_last_line = self.last_line;
                self.advance(); // consume (
                if let Some(params) = self.try_parse_lambda_params()
                    && matches!(self.peek(), TokenKind::Arrow)
                {
                    self.advance(); // consume ->
                    let body = self.parse_expr()?;
                    return Ok(Expr::Lambda(params, Box::new(body)));
                }
                // Not a lambda — restore and parse as parenthesized expression
                self.pos = saved_pos;
                self.last_line = saved_last_line;
                self.advance(); // consume (
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(e)
            }
            TokenKind::LBrace => {
                self.advance();
                let entries = self.parse_entries()?;
                self.expect(&TokenKind::RBrace)?;
                Ok(Expr::ObjectBody(entries))
            }
            TokenKind::KwNew => {
                self.advance();
                let type_name = if let TokenKind::Ident(_) = self.peek() {
                    let mut name = self.expect_ident()?;
                    // Handle dotted type names: new Config.Step { ... }
                    while matches!(self.peek(), TokenKind::Dot) {
                        self.advance();
                        let part = self.expect_ident()?;
                        name.push('.');
                        name.push_str(&part);
                    }
                    Some(name)
                } else {
                    None
                };
                // Skip optional generic type params: <Type, Type, ...>
                if matches!(self.peek(), TokenKind::Lt) {
                    self.skip_generic_params()?;
                }
                self.expect(&TokenKind::LBrace)?;
                let entries = self.parse_entries()?;
                self.expect(&TokenKind::RBrace)?;
                Ok(Expr::New(type_name, entries))
            }
            TokenKind::KwIf => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                let then = self.parse_expr()?;
                self.expect(&TokenKind::KwElse)?;
                let else_ = self.parse_expr()?;
                Ok(Expr::If(Box::new(cond), Box::new(then), Box::new(else_)))
            }
            TokenKind::KwLet => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let name = self.expect_ident()?;
                self.expect(&TokenKind::Equals)?;
                let val = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                let body = self.parse_expr()?;
                Ok(Expr::Let(name, Box::new(val), Box::new(body)))
            }
            TokenKind::KwThrow => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let msg = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(Expr::Throw(Box::new(msg)))
            }
            TokenKind::KwTrace => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(Expr::Trace(Box::new(e)))
            }
            TokenKind::KwRead => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(Expr::Read(Box::new(e)))
            }
            TokenKind::KwReadOrNull => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(Expr::ReadOrNull(Box::new(e)))
            }
            TokenKind::Ident(name) => {
                self.advance();
                Ok(Expr::Ident(name))
            }
            TokenKind::KwThis => {
                self.advance();
                Ok(Expr::Ident("this".into()))
            }
            TokenKind::KwSuper => {
                self.advance();
                Ok(Expr::Ident("super".into()))
            }
            TokenKind::KwModule => {
                self.advance();
                Ok(Expr::Ident("module".into()))
            }
            tok => Err(self.parse_error(format!("unexpected token in expression: {:?}", tok))),
        }
    }

    /// Try to parse `ident, ident, ...) ` — returns None if not a valid lambda param list.
    fn try_parse_lambda_params(&mut self) -> Option<Vec<String>> {
        let mut params = Vec::new();
        // Handle () -> expr (no params)
        if matches!(self.peek(), TokenKind::RParen) {
            self.advance();
            return Some(params);
        }
        // First param
        if let TokenKind::Ident(name) = self.peek().clone() {
            self.advance();
            params.push(name);
        } else {
            return None;
        }
        // Remaining params
        while matches!(self.peek(), TokenKind::Comma) {
            self.advance();
            if let TokenKind::Ident(name) = self.peek().clone() {
                self.advance();
                params.push(name);
            } else {
                return None;
            }
        }
        if matches!(self.peek(), TokenKind::RParen) {
            self.advance();
            Some(params)
        } else {
            None
        }
    }

    fn expect_string(&mut self) -> Result<String> {
        let tok = self.advance();
        let offset = tok.offset;
        let kind = tok.kind.clone();
        if let TokenKind::StringLit(s) = kind {
            Ok(s)
        } else {
            Err(Error::Parse {
                src: miette::NamedSource::new(&self.name, self.source.clone()),
                span: miette::SourceOffset::from(offset),
                message: format!("expected string, got {:?}", kind),
            })
        }
    }

    fn expect_ident(&mut self) -> Result<String> {
        let tok = self.advance();
        let offset = tok.offset;
        let kind = tok.kind.clone();
        match &kind {
            TokenKind::Ident(s) => Ok(s.clone()),
            // Allow keywords as identifiers in property name position
            TokenKind::KwLocal => Ok("local".into()),
            TokenKind::KwFixed => Ok("fixed".into()),
            TokenKind::KwHidden => Ok("hidden".into()),
            TokenKind::KwNew => Ok("new".into()),
            TokenKind::KwModule => Ok("module".into()),
            other => Err(Error::Parse {
                src: miette::NamedSource::new(&self.name, self.source.clone()),
                span: miette::SourceOffset::from(offset),
                message: format!("expected identifier, got {:?}", other),
            }),
        }
    }
}
