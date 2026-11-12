//! Parsing object bodies and their members.

use super::*;

impl<'a> Parser<'a> {
    /// Parse `{ [params ->] members }`. `in_for` is set inside the body of a
    /// for-generator, which cannot define properties or methods.
    pub(super) fn parse_object_body(&mut self, in_for: bool) -> Result<ObjectBody> {
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
    pub(super) fn parse_body_params(&mut self) -> Result<Vec<String>> {
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

    pub(super) fn parse_object_member(
        &mut self,
        in_for: bool,
        entries: &mut Vec<Entry>,
    ) -> Result<()> {
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
    pub(super) fn parse_generator_body(&mut self, in_for: bool) -> Result<Vec<Entry>> {
        let body = self.parse_object_body(in_for)?;
        Ok(body.entries)
    }

    /// Parse `= expr` or `{ ... }` after an entry key or predicate.
    pub(super) fn parse_entry_value(&mut self, key: &Expr) -> Result<Expr> {
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

    pub(super) fn for_generator_error(&self, what: &str) -> Error {
        self.parse_error(format!(
            "A for-generator cannot generate object {what} (only entries and elements)."
        ))
    }

    pub(super) fn parse_object_property(
        &mut self,
        header: MemberHeader,
        in_for: bool,
    ) -> Result<Entry> {
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
            name: name.into(),
            type_ann,
            value,
            body: body.map(Into::into),
            is_method: false,
        })))
    }

    pub(super) fn parse_object_method(
        &mut self,
        header: MemberHeader,
        in_for: bool,
    ) -> Result<Option<Entry>> {
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
}

/// A parsed object body: optional parameters (`{ a, b -> ... }`) and members.
pub(super) struct ObjectBody {
    pub(super) params: Vec<String>,
    pub(super) entries: Vec<Entry>,
}

/// Amend `parent` with an object body. A body with parameters amends a
/// function: `f { x -> ... }` is `(x) -> f.apply(x) { ... }`.
pub(super) fn amend_expr(parent: Expr, body: ObjectBody) -> Expr {
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
