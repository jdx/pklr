use crate::error::{Error, Result};
use crate::lexer::{StringPart, Token, TokenKind};

mod ast;
mod checks;

pub use ast::{
    Annotation, BinOp, Body, Entry, Expr, ForGenerator, Import, Modifier, Module, Property,
    StringInterpPart, TypeExpr, UnOp, WhenGenerator,
};
use ast::{infer_method_return_new, type_expr_runtime_name};
use checks::{BodyKind, BodyScope};

/// Collect all import URIs from a token stream (fast path, no full parse needed).
pub fn collect_imports(tokens: &[Token]) -> Vec<String> {
    let mut imports = Vec::new();
    collect_imports_into(tokens, &mut imports);
    imports
}

fn collect_imports_into(tokens: &[Token], imports: &mut Vec<String>) {
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i].kind {
            TokenKind::KwAmends | TokenKind::KwImport | TokenKind::KwImportStar => {
                match tokens.get(i + 1).map(|t| &t.kind) {
                    // Declaration form: `import "uri"` / `import* "glob"` / `amends "uri"`
                    Some(TokenKind::StringLit(uri)) => {
                        imports.push(uri.clone());
                        i += 2;
                    }
                    // Expression form: `import("uri")` / `import*("glob")`
                    Some(TokenKind::LParen) => {
                        if let Some(TokenKind::StringLit(uri)) = tokens.get(i + 2).map(|t| &t.kind)
                        {
                            imports.push(uri.clone());
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
    p.parse_expr()
}

#[cfg(feature = "eval-core")]
pub(crate) fn parse_type_name(name: &str) -> Result<TypeExpr> {
    let tokens = crate::lexer::lex(name)?;
    Parser::new(&tokens, name, "<type>").parse_type()
}

struct Parser<'a> {
    tokens: &'a [Token],
    source: &'a str,
    name: &'a str,
    pos: usize,
    /// Line of the last consumed token (used for newline-sensitive parsing).
    last_line: usize,
    /// Members of the body being parsed, for static checks.
    body: BodyScope,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token], source: &'a str, name: &'a str) -> Self {
        Self {
            tokens,
            source,
            name,
            pos: 0,
            last_line: 1,
            body: BodyScope::new(BodyKind::Object, None),
        }
    }

    fn parse_error(&self, message: impl Into<String>) -> Error {
        let tok = self.peek_tok();
        Error::parse(self.name, self.source, tok.offset, message.into())
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
        let mut annotations = self.parse_annotations()?;

        // Parse header: module declaration, amends, imports
        let module_modifiers_offset = self.peek_tok().offset;
        let mut module_modifiers = Vec::new();
        if self.peek_is_modifier() && self.peek_past_modifiers_is(TokenKind::KwModule) {
            module_modifiers = self.collect_modifiers();
            for modifier in &module_modifiers {
                annotations.push(Annotation {
                    name: format!("pklr:module:{modifier:?}"),
                    body: Vec::new(),
                });
            }
        }
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
                TokenKind::Comma | TokenKind::Semicolon => {
                    self.advance();
                }
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

        self.check_module_modifiers(module_modifiers_offset, &module_modifiers, amends.is_some())?;
        let kind = if amends.is_some() {
            BodyKind::AmendingModule
        } else {
            BodyKind::Module
        };
        let mut scope = BodyScope::new(kind, None);
        if let Some(name) = scope.declare_imports(&imports) {
            return Err(self.duplicate_error(self.peek_tok().offset, &name));
        }
        let body = self.parse_body(scope)?;
        Ok(Module {
            amends,
            extends,
            imports,
            annotations,
            body: body.into(),
        })
    }

    fn parse_entries(&mut self) -> Result<Vec<Entry>> {
        self.parse_body(BodyScope::new(BodyKind::Object, None))
    }

    /// Parse the members of a body, checking them against the rules for the
    /// kind of body `scope` describes.
    fn parse_body(&mut self, scope: BodyScope) -> Result<Vec<Entry>> {
        let saved = std::mem::replace(&mut self.body, scope);
        let entries = self.parse_body_entries();
        let scope = std::mem::replace(&mut self.body, saved);
        let entries = entries?;
        self.finish_body(&scope)?;
        Ok(entries)
    }

    fn parse_body_entries(&mut self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        while !self.at_eof() && !matches!(self.peek(), TokenKind::RBrace) {
            if matches!(self.peek(), TokenKind::Comma | TokenKind::Semicolon) {
                self.advance();
                continue;
            }
            let entry_annotations = self.parse_annotations()?;
            let member_offset = self.peek_tok().offset;
            // Parse class definitions (with optional modifiers); skip typealias/function declarations
            let class_modifiers =
                if self.peek_is_modifier() && self.peek_past_modifiers_is(TokenKind::KwClass) {
                    self.collect_modifiers()
                } else {
                    Vec::new()
                };
            if matches!(self.peek(), TokenKind::KwClass) {
                self.advance(); // consume 'class'
                let name = self.expect_ident()?;
                self.check_class(member_offset, &class_modifiers, &name)?;
                if matches!(self.peek(), TokenKind::Lt) {
                    return Err(self.type_parameters_error());
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
                    let body = self.parse_body(BodyScope::new(BodyKind::Class, None))?;
                    self.expect(&TokenKind::RBrace)?;
                    entries.push(Entry::ClassDef(name, class_modifiers, parent, body.into()));
                }
                continue;
            }
            if matches!(self.peek(), TokenKind::KwTypeAlias) {
                self.advance(); // consume 'typealias'
                let name = self.expect_ident()?;
                self.check_type_alias(member_offset, &[], &name)?;
                if matches!(self.peek(), TokenKind::Lt) {
                    self.check_type_parameters(self.pos)?;
                    self.skip_generic_params()?;
                }
                self.expect(&TokenKind::Equals)?;
                let ty = self.parse_type()?;
                entries.push(Entry::TypeAlias(name, ty));
                continue;
            }
            if matches!(self.peek(), TokenKind::KwFunction) {
                if let Some(entry) = self.try_parse_function_def(member_offset, Vec::new())? {
                    entries.push(entry);
                }
                continue;
            }
            // Also handle modifier-prefixed function/typealias declarations
            if self.peek_is_modifier() && self.peek_past_modifiers_is_decl() {
                let mods = self.collect_modifiers();
                if matches!(self.peek(), TokenKind::KwFunction) {
                    if let Some(entry) = self.try_parse_function_def(member_offset, mods)? {
                        entries.push(entry);
                    }
                } else {
                    // typealias — skip as before
                    if let Some(TokenKind::Ident(name)) =
                        self.tokens.get(self.pos + 1).map(|tok| &tok.kind)
                    {
                        self.check_type_alias(member_offset, &mods, name)?;
                    }
                    if matches!(
                        self.tokens.get(self.pos + 2).map(|tok| &tok.kind),
                        Some(TokenKind::Lt)
                    ) {
                        self.check_type_parameters(self.pos + 2)?;
                    }
                    self.skip_declaration();
                }
                continue;
            }
            if self.at_eof() || matches!(self.peek(), TokenKind::RBrace) {
                break;
            }
            let mut entry = self.parse_entry()?;
            match &entry {
                Entry::Property(prop) => self.check_property(member_offset, prop)?,
                Entry::DynProperty(key, _) => self.check_entry_key(member_offset, key),
                Entry::ForGenerator(_) | Entry::WhenGenerator(_) | Entry::Spread(_) => {
                    self.body.set_has_generator();
                }
                _ => {}
            }
            // Attach annotations to the parsed property
            if !entry_annotations.is_empty()
                && let Entry::Property(ref mut prop) = entry
            {
                std::sync::Arc::make_mut(prop).annotations = entry_annotations;
            }
            entries.push(entry);
            while matches!(self.peek(), TokenKind::Comma | TokenKind::Semicolon) {
                self.advance();
            }
        }
        Ok(entries)
    }

    /// Collect top-level generic type parameter names: `<Type, Type<Nested>, ...>`
    /// Returns the simple names of each top-level param (e.g., `["String", "Step"]`).
    fn collect_generic_params(&mut self) -> Result<Vec<String>> {
        let mut params = Vec::new();
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
                TokenKind::Comma if depth == 1 => {
                    self.advance();
                }
                TokenKind::Ident(name) if depth == 1 => {
                    let mut name = name.clone();
                    self.advance();
                    while depth == 1 && matches!(self.peek(), TokenKind::Dot) {
                        self.advance();
                        let part = self.expect_ident()?;
                        name.push('.');
                        name.push_str(&part);
                    }
                    params.push(name);
                }
                TokenKind::Eof => {
                    return Err(self.parse_error("unclosed generic type parameters"));
                }
                _ => {
                    self.advance();
                }
            }
        }
        Ok(params)
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

    fn peek_past_modifiers_is(&self, target: TokenKind) -> bool {
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
                tok if std::mem::discriminant(tok) == std::mem::discriminant(&target) => {
                    return true;
                }
                _ => return false,
            }
        }
        false
    }

    fn collect_modifiers(&mut self) -> Vec<Modifier> {
        let mut mods = Vec::new();
        loop {
            match self.peek() {
                TokenKind::KwLocal => {
                    self.advance();
                    mods.push(Modifier::Local);
                }
                TokenKind::KwConst => {
                    self.advance();
                    mods.push(Modifier::Const);
                }
                TokenKind::KwFixed => {
                    self.advance();
                    mods.push(Modifier::Fixed);
                }
                TokenKind::KwHidden => {
                    self.advance();
                    mods.push(Modifier::Hidden);
                }
                TokenKind::KwAbstract => {
                    self.advance();
                    mods.push(Modifier::Abstract);
                }
                TokenKind::KwOpen => {
                    self.advance();
                    mods.push(Modifier::Open);
                }
                TokenKind::KwExternal => {
                    self.advance();
                    mods.push(Modifier::External);
                }
                _ => break,
            }
        }
        mods
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

    /// Parse `function name(params...): ReturnType = body` into a Property with Lambda value.
    /// Returns None if the function body can't be parsed (falls back to skip).
    fn try_parse_function_def(
        &mut self,
        offset: usize,
        modifiers: Vec<Modifier>,
    ) -> Result<Option<Entry>> {
        let saved_pos = self.pos;
        let saved_last_line = self.last_line;
        self.advance(); // consume `function`
        let name = match self.peek() {
            TokenKind::Ident(_) => self.expect_ident()?,
            _ => {
                // Not a named function — restore and skip
                self.pos = saved_pos;
                self.last_line = saved_last_line;
                self.skip_declaration();
                return Ok(None);
            }
        };
        self.check_method(offset, &modifiers, &name)?;
        if matches!(self.peek(), TokenKind::Lt) {
            return Err(self.type_parameters_error());
        }
        // Parse parameter list: (param1: Type, param2: Type, ...)
        if !matches!(self.peek(), TokenKind::LParen) {
            self.pos = saved_pos;
            self.last_line = saved_last_line;
            self.skip_declaration();
            return Ok(None);
        }
        self.advance(); // consume (
        let mut params = Vec::new();
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            let param_name = self.expect_ident()?;
            if matches!(self.peek(), TokenKind::Colon) {
                self.advance();
                self.parse_type()?;
            }
            params.push(param_name);
            if matches!(self.peek(), TokenKind::Comma) {
                self.advance();
            }
        }
        self.expect(&TokenKind::RParen)?;
        let return_type = if matches!(self.peek(), TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        // Parse body: `= expr`
        if !matches!(self.peek(), TokenKind::Equals) {
            // No body — skip rest
            self.pos = saved_pos;
            self.last_line = saved_last_line;
            self.skip_declaration();
            return Ok(None);
        }
        self.advance(); // consume =
        let mut body = self.parse_expr()?;
        if let Some(return_type) = return_type {
            infer_method_return_new(&mut body, &return_type);
        }
        Ok(Some(Entry::Property(std::sync::Arc::new(Property {
            name,
            type_ann: None,
            value: Some(Expr::Lambda(params.into(), std::sync::Arc::new(body))),
            body: None,
            modifiers,
            annotations: Vec::new(),
        }))))
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
                    Ok(Entry::DynProperty(key, Expr::ObjectBody(entries.into())))
                } else {
                    self.expect(&TokenKind::Equals)?;
                    let val = self.parse_expr()?;
                    Ok(Entry::DynProperty(key, val))
                }
            }
            TokenKind::KwFor => {
                let for_offset = self.peek_tok().offset;
                self.advance(); // consume 'for'
                self.expect(&TokenKind::LParen)?;
                // for (k, v in collection) or (v in collection)
                let first = self.expect_ident()?;
                let (key_var, val_var, collection) = if matches!(self.peek(), TokenKind::Comma) {
                    self.advance();
                    let v_offset = self.peek_tok().offset;
                    let v = self.expect_ident()?;
                    if v == first {
                        return Err(self.duplicate_error(v_offset, &v));
                    }
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
                let body = self.parse_body(self.for_scope(for_offset))?;
                self.expect(&TokenKind::RBrace)?;
                Ok(Entry::ForGenerator(ForGenerator {
                    key_var,
                    val_var,
                    collection,
                    body: body.into(),
                }))
            }
            TokenKind::KwWhen => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                self.expect(&TokenKind::LBrace)?;
                let body = self.parse_body(self.when_scope())?;
                self.expect(&TokenKind::RBrace)?;
                let else_body = if matches!(self.peek(), TokenKind::KwElse) {
                    self.advance();
                    self.expect(&TokenKind::LBrace)?;
                    let eb = self.parse_body(self.when_scope())?;
                    self.expect(&TokenKind::RBrace)?;
                    Some(eb)
                } else {
                    None
                };
                Ok(Entry::WhenGenerator(WhenGenerator {
                    condition: cond,
                    body: body.into(),
                    else_body: else_body.map(Into::into),
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
                        | TokenKind::KwSuper
                        | TokenKind::KwImport
                        | TokenKind::KwImportStar
                        | TokenKind::LParen
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

                Ok(Entry::Property(std::sync::Arc::new(Property {
                    annotations: Vec::new(), // filled by parse_entries if present
                    modifiers,
                    name,
                    type_ann,
                    value,
                    body: body.map(Into::into),
                })))
            }
        }
    }

    fn parse_type(&mut self) -> Result<TypeExpr> {
        let (first, first_is_default) = self.parse_type_member()?;
        let mut variants = vec![(first, first_is_default)];
        while matches!(self.peek(), TokenKind::Pipe) {
            self.advance();
            variants.push(self.parse_type_member()?);
        }
        if variants.len() == 1 {
            let (ty, is_default) = variants.pop().unwrap();
            Ok(if is_default {
                mark_default_type(ty)
            } else {
                ty
            })
        } else {
            Ok(TypeExpr::Union(
                variants
                    .into_iter()
                    .map(|(ty, is_default)| {
                        if is_default {
                            mark_default_type(ty)
                        } else {
                            ty
                        }
                    })
                    .collect(),
            ))
        }
    }

    fn parse_type_member(&mut self) -> Result<(TypeExpr, bool)> {
        let is_default = if matches!(self.peek(), TokenKind::Star) {
            self.advance();
            true
        } else {
            false
        };
        let base = match self.peek().clone() {
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_type()?;
                self.expect(&TokenKind::RParen)?;
                inner
            }
            TokenKind::StringLit(s) => {
                self.advance();
                TypeExpr::Named(format!("\"{s}\""))
            }
            TokenKind::Null => {
                self.advance();
                TypeExpr::Named("Null".to_string())
            }
            TokenKind::KwModule => {
                self.advance();
                TypeExpr::Named("module".to_string())
            }
            TokenKind::Ident(mut name) => {
                self.advance();
                // Handle dotted type names: e.g., Config.StepTest
                while matches!(self.peek(), TokenKind::Dot) {
                    self.advance();
                    let part = self.expect_ident()?;
                    name.push('.');
                    name.push_str(&part);
                }
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

        let mut t = self.parse_type_constraints(base)?;
        if matches!(self.peek(), TokenKind::QuestionMark) {
            self.advance();
            t = TypeExpr::Nullable(Box::new(t));
            t = self.parse_type_constraints(t)?;
        }
        Ok((t, is_default))
    }

    fn parse_type_constraints(&mut self, base: TypeExpr) -> Result<TypeExpr> {
        if !matches!(self.peek(), TokenKind::LParen) || self.peek_tok().line != self.last_line {
            return Ok(base);
        }
        self.advance();
        let mut constraint = self.parse_expr()?;
        while matches!(self.peek(), TokenKind::Comma) {
            self.advance();
            let next = self.parse_expr()?;
            constraint = Expr::Binop(BinOp::And, Box::new(constraint), Box::new(next));
        }
        self.expect(&TokenKind::RParen)?;
        Ok(TypeExpr::Constrained(
            type_expr_runtime_name(&base),
            Box::new(constraint),
        ))
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
                    if self.peek_tok().line != self.last_line {
                        break;
                    }
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
                        Box::new(Expr::ObjectBody(entries.into())),
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
                Ok(Expr::String(s.into()))
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
                            let expr = parse_expr_tokens(&tokens, self.source, self.name)?;
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
                    return Ok(Expr::Lambda(params.into(), std::sync::Arc::new(body)));
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
                Ok(Expr::ObjectBody(entries.into()))
            }
            TokenKind::KwNew => {
                self.advance();
                // `new module.C {}` and `new this.C {}` name a class through
                // the module object.
                let qualifier = match self.peek() {
                    TokenKind::KwModule => Some("module"),
                    TokenKind::KwThis => Some("this"),
                    _ => None,
                }
                .filter(|_| {
                    matches!(
                        self.tokens.get(self.pos + 1).map(|tok| &tok.kind),
                        Some(TokenKind::Dot)
                    )
                });
                let type_name = if qualifier.is_some() || matches!(self.peek(), TokenKind::Ident(_))
                {
                    let mut name = match qualifier {
                        Some(qualifier) => {
                            self.advance();
                            qualifier.to_string()
                        }
                        None => self.expect_ident()?,
                    };
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
                // Collect optional generic type params: <Type, Type, ...>
                let generic_params = if matches!(self.peek(), TokenKind::Lt) {
                    self.collect_generic_params()?
                } else {
                    Vec::new()
                };
                self.expect(&TokenKind::LBrace)?;
                let entries = self.parse_entries()?;
                self.expect(&TokenKind::RBrace)?;
                Ok(Expr::New(type_name, entries.into(), generic_params))
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
            TokenKind::KwImport => {
                self.advance();
                let uri = self.parse_import_expr_uri("import")?;
                Ok(Expr::Import(uri, self.name.to_string()))
            }
            TokenKind::KwImportStar => {
                self.advance();
                let uri = self.parse_import_expr_uri("import*")?;
                Ok(Expr::ImportGlob(uri, self.name.to_string()))
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

    /// Parse the `("uri")` part of an `import(...)` / `import*(...)` expression.
    /// pkl requires the URI to be a constant string literal.
    fn parse_import_expr_uri(&mut self, keyword: &str) -> Result<String> {
        self.expect(&TokenKind::LParen)?;
        if !matches!(self.peek(), TokenKind::StringLit(_)) {
            let tok = self.peek().clone();
            return Err(self.parse_error(format!(
                "{keyword}() requires a string literal URI, got {tok:?}"
            )));
        }
        let uri = self.expect_string()?;
        self.expect(&TokenKind::RParen)?;
        Ok(uri)
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
            if matches!(self.peek(), TokenKind::Colon) {
                self.advance();
                self.parse_type().ok()?;
            }
            params.push(name);
        } else {
            return None;
        }
        // Remaining params
        while matches!(self.peek(), TokenKind::Comma) {
            self.advance();
            if let TokenKind::Ident(name) = self.peek().clone() {
                self.advance();
                if matches!(self.peek(), TokenKind::Colon) {
                    self.advance();
                    self.parse_type().ok()?;
                }
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
            Err(Error::parse(
                self.name,
                self.source,
                offset,
                format!("expected string, got {:?}", kind),
            ))
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
            other => Err(Error::parse(
                self.name,
                self.source,
                offset,
                format!("expected identifier, got {:?}", other),
            )),
        }
    }
}

fn mark_default_type(ty: TypeExpr) -> TypeExpr {
    TypeExpr::Named(format!("*{}", type_expr_runtime_name(&ty)))
}
