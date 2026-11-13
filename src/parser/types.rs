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
            match ty {
                // `*(A|*B)` defaults to the inner default: the union is spliced
                // in, keeping its own marker.
                TypeExpr::Union(inner) if inner.iter().any(is_marked) => {
                    variants.splice(index..=index, inner);
                }
                ty => variants[index] = mark_default_type(ty),
            }
        }
        Ok(TypeExpr::Union(variants))
    }

    pub(super) fn parse_type_atom(&mut self) -> Result<TypeExpr> {
        // Whether the type is one name written in backticks.
        let mut quoted_name = false;
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
                quoted_name = self.is_quoted(self.peek_tok());
                let name = self.parse_qualified_ident()?;
                match &self.in_alias_definition {
                    Some(alias) => {
                        self.alias_definition_uses
                            .entry(alias.clone())
                            .or_default()
                            .insert(name.clone());
                    }
                    None => {
                        self.type_uses.insert(name.clone());
                    }
                }
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
        self.parse_type_end_of(ty, quoted_name)
    }

    /// Parse `?` and `(constraints)` suffixes of a type, which is
    /// `quoted_name` when it is one name in backticks.
    fn parse_type_end_of(&mut self, mut ty: TypeExpr, quoted_name: bool) -> Result<TypeExpr> {
        loop {
            if matches!(self.peek(), TokenKind::QuestionMark) {
                self.advance();
                ty = TypeExpr::Nullable(Box::new(ty));
            } else if matches!(self.peek(), TokenKind::LParen) && self.on_same_line() {
                self.advance();
                let mut constraint: Option<Expr> = None;
                loop {
                    let start = self.peek_tok().offset;
                    let body = self.parse_expr()?;
                    let text = self.source_since(start);
                    let next = Expr::Constraint(Box::new(body), unparenthesized(&text));
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
                    match &ty {
                        TypeExpr::Generic(..) => type_expr_constrained_name(&ty),
                        TypeExpr::Nullable(inner) if matches!(**inner, TypeExpr::Generic(..)) => {
                            type_expr_constrained_name(&ty)
                        }
                        _ => type_expr_runtime_name(&ty),
                    },
                    Box::new(constraint.expect("at least one constraint")),
                    if quoted_name && matches!(ty, TypeExpr::Named(_)) {
                        BaseForm::Quoted
                    } else {
                        BaseForm::Text
                    },
                );
            } else {
                return Ok(ty);
            }
        }
    }
}

/// Mark a union alternative as the default. A constrained alternative keeps
/// its predicate: the marker goes on the base name, as for a plain name.
pub(super) fn mark_default_type(ty: TypeExpr) -> TypeExpr {
    match ty {
        TypeExpr::Constrained(base, constraint, quoted) => {
            TypeExpr::Constrained(format!("*{base}"), constraint, quoted)
        }
        TypeExpr::Nullable(inner)
            if matches!(*inner, TypeExpr::Constrained(..) | TypeExpr::Nullable(_)) =>
        {
            TypeExpr::Nullable(Box::new(mark_default_type(*inner)))
        }
        ty => TypeExpr::Named(format!("*{}", type_expr_runtime_name(&ty))),
    }
}

/// Whether a union alternative carries a `*` default marker.
fn is_marked(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Named(name) | TypeExpr::Constrained(name, _, _) => name.starts_with('*'),
        TypeExpr::Nullable(inner) => is_marked(inner),
        _ => false,
    }
}

/// The source of a constraint as pkl shows it in an error: without the
/// parentheses that wrap all of it, as in `List((this[0] is Int))`.
fn unparenthesized(text: &str) -> String {
    let text = text.trim();
    let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) else {
        return text.to_string();
    };
    // The first `(` must close at the very end, not earlier as in `(a) + (b)`.
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for c in inner.chars() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' if depth == 0 => return text.to_string(),
            ')' => depth -= 1,
            _ => {}
        }
    }
    if depth == 0 {
        inner.trim().to_string()
    } else {
        text.to_string()
    }
}
