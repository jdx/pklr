//! Parsing a module and its declarations: members, classes, type aliases and
//! methods.

use super::*;

impl<'a> Parser<'a> {
    pub(super) fn parse_member_header(&mut self) -> Result<MemberHeader> {
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
    pub(super) fn blank_line_before(&self) -> bool {
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
    pub(super) fn parse_annotation(&mut self) -> Result<Annotation> {
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

    pub(super) fn parse_module(&mut self) -> Result<Module> {
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
            local_type_aliases: std::mem::take(&mut self.module_local_type_aliases),
        })
    }

    pub(super) fn parse_module_member(
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
                let is_local = header.has(Modifier::Local);
                let entry = self.parse_type_alias(header)?;
                if let (true, Entry::TypeAlias(name, _)) = (is_local, &entry) {
                    self.module_local_type_aliases.insert(name.clone());
                }
                entries.push(entry);
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

    pub(super) fn parse_type_alias(&mut self, header: MemberHeader) -> Result<Entry> {
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
    pub(super) fn parse_type_parameters(&mut self) -> Result<Vec<String>> {
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

    pub(super) fn type_parameters_error(&self, offset: usize) -> Error {
        self.error_at(
            offset,
            "Only standard library members can have type parameters.",
        )
    }

    pub(super) fn parse_class(&mut self, header: MemberHeader) -> Result<Entry> {
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
    pub(super) fn parse_class_property(
        &mut self,
        header: MemberHeader,
        module_level: bool,
    ) -> Result<Entry> {
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
            name: name.into(),
            type_ann,
            value,
            body: body.map(Into::into),
            is_method: false,
        })))
    }

    pub(super) fn local_amend_error(&self) -> Error {
        self.parse_error(
            "A local property definition cannot be amended.\n\n\
             Use definition syntax instead, for example `local person = new { ... }` \
             instead of `local person { ... }`.",
        )
    }

    /// Parse the object bodies of a property amendment `name { ... } { ... }`.
    /// Bodies with parameters amend a function: `name { x -> ... }` means
    /// `name = (x) -> super.name.apply(x) { ... }`.
    pub(super) fn parse_property_bodies(
        &mut self,
        name: &str,
    ) -> Result<(Option<Expr>, Option<Vec<Entry>>)> {
        let super_value = Expr::Field(Box::new(Expr::Ident("super".into())), name.to_string());
        match self.parse_body_list()? {
            BodyList::Plain(entries) => Ok((None, Some(entries))),
            BodyList::Amended(f) => Ok((Some(f(super_value)), None)),
        }
    }

    /// Parse one or more object bodies following a member. Several plain
    /// bodies amend in turn, which for a member is the same as one body
    /// holding all their members.
    pub(super) fn parse_body_list(&mut self) -> Result<BodyList> {
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
    pub(super) fn parse_method(
        &mut self,
        header: MemberHeader,
        kind: MemberKind,
    ) -> Result<Option<Entry>> {
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
            name: name.into(),
            type_ann: None,
            value: Some(Expr::Lambda(params.into(), std::sync::Arc::new(body))),
            body: None,
            modifiers: header.modifiers(),
            annotations: header.annotations.clone(),
            is_method: true,
        }))))
    }

    /// Parse `(a: Type, b, _)`; types are checked but not kept.
    pub(super) fn parse_parameter_list(&mut self) -> Result<Vec<String>> {
        self.expect(&TokenKind::LParen, "(")?;
        let params = self.parse_parameters(&TokenKind::RParen)?;
        self.expect2(&TokenKind::RParen, ")", ",")?;
        Ok(params)
    }

    /// Parse a comma-separated parameter list up to `terminator`, allowing a
    /// trailing comma.
    pub(super) fn parse_parameters(&mut self, terminator: &TokenKind) -> Result<Vec<String>> {
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
    pub(super) fn parse_parameter(&mut self) -> Result<String> {
        Ok(self.parse_typed_parameter()?.0)
    }

    /// Parse `name`, `name: Type` or `_`, keeping the type.
    pub(super) fn parse_typed_parameter(&mut self) -> Result<(String, Option<TypeExpr>)> {
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
}

pub(super) fn token_modifier(kind: &TokenKind) -> Option<Modifier> {
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

/// Object bodies following a member, either plain or amending a function.
pub(super) enum BodyList {
    Plain(Vec<Entry>),
    /// Builds the amended value from the parent value.
    Amended(Box<dyn FnOnce(Expr) -> Expr>),
}
