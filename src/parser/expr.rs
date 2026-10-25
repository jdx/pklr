//! Parsing expressions.

use super::*;

impl<'a> Parser<'a> {
    pub(super) fn parse_expr(&mut self) -> Result<Expr> {
        let depth = self.depth;
        self.deepen()?;
        let result = self.parse_binary(1);
        self.depth = depth;
        result
    }

    pub(super) fn deepen(&mut self) -> Result<()> {
        if self.depth >= MAX_NESTING_DEPTH {
            return Err(self.parse_error(format!(
                "expressions cannot be nested more than {MAX_NESTING_DEPTH} levels deep"
            )));
        }
        self.depth += 1;
        Ok(())
    }

    /// Precedence climbing over pkl's binary operators.
    pub(super) fn parse_binary(&mut self, min_prec: u8) -> Result<Expr> {
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

    pub(super) fn parse_unary(&mut self) -> Result<Expr> {
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
    pub(super) fn parse_call_args(&mut self, callee: Expr) -> Result<Expr> {
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

    pub(super) fn parse_postfix(&mut self) -> Result<Expr> {
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
    pub(super) fn parse_primary(&mut self) -> Result<(Expr, bool)> {
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
    pub(super) fn parse_keyword_call(&mut self) -> Result<Expr> {
        self.advance(); // keyword
        self.expect(&TokenKind::LParen, "(")?;
        let expr = self.parse_expr()?;
        self.expect(&TokenKind::RParen, ")")?;
        Ok(expr)
    }

    pub(super) fn parse_trace_call(&mut self) -> Result<Expr> {
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
    pub(super) fn parse_new_type(&mut self) -> Result<(Option<String>, Vec<String>)> {
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
    pub(super) fn parse_paren_or_lambda(&mut self) -> Result<(Expr, bool)> {
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
    pub(super) fn parse_import_expr_uri(&mut self) -> Result<String> {
        self.expect(&TokenKind::LParen, "(")?;
        let uri = self.expect_string_constant()?;
        self.expect(&TokenKind::RParen, ")")?;
        Ok(uri)
    }
}

/// Binary operators with their pkl precedence and associativity.
pub(super) fn binary_operator(kind: &TokenKind) -> Option<(BinaryOp, u8, bool)> {
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
pub(super) enum BinaryOp {
    Bin(BinOp),
    Is,
    As,
    Dot,
    QDot,
}

/// Drop grouping parentheses only when they enclose the complete trace
/// argument. Token offsets then give the argument's exact source section,
/// without mistaking comment-like text inside a string for a comment.
pub(super) fn strip_trace_grouping_tokens(
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
