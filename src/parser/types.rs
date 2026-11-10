//! Parsing type annotations.

use super::*;

impl<'a> Parser<'a> {
    pub(super) fn parse_type(&mut self) -> Result<TypeExpr> {
        let depth = self.depth;
        self.deepen()?;
        let result = self.parse_type_inner();
        self.depth = depth;
        result
    }

    pub(super) fn parse_type_inner(&mut self) -> Result<TypeExpr> {
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

    pub(super) fn parse_type_atom(&mut self) -> Result<TypeExpr> {
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
    pub(super) fn parse_type_end(&mut self, mut ty: TypeExpr) -> Result<TypeExpr> {
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
}

pub(super) fn mark_default_type(ty: TypeExpr) -> TypeExpr {
    TypeExpr::Named(format!("*{}", type_expr_runtime_name(&ty)))
}
