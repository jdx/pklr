//! Parsing a module and its declarations: members, classes, type aliases and
//! methods.

use super::*;

impl<'a> Parser<'a> {
    pub(super) fn parse_member_header(&mut self) -> Result<MemberHeader> {
        let mut header = MemberHeader::default();
        if matches!(self.peek(), TokenKind::DocComment) {
            header.doc_comment = Some(self.peek_tok().offset);
            let mut lines = vec![self.doc_comment_line()];
            self.advance();
            // Consecutive doc comment lines form one comment; a blank line
            // ends it, leaving a dangling doc comment.
            while matches!(self.peek(), TokenKind::DocComment) && !self.blank_line_before() {
                lines.push(self.doc_comment_line());
                self.advance();
            }
            header.doc = Some(lines.join("\n").into());
        }
        while matches!(self.peek(), TokenKind::At) {
            header
                .annotations_start
                .get_or_insert(self.peek_tok().offset);
            header.annotations.push(self.parse_annotation()?);
        }
        let at = self.peek_tok();
        header.at = SourcePos {
            line: at.line as u32,
            col: at.col as u32,
        };
        while let Some(modifier) = token_modifier(self.peek()) {
            header.modifiers.push((modifier, self.peek_tok().offset));
            self.advance();
        }
        Ok(header)
    }

    /// The text of the doc comment line at the cursor, without `///` and the
    /// space after it.
    fn doc_comment_line(&self) -> String {
        let offset = self.peek_tok().offset;
        let line = self
            .source
            .get(offset..)
            .and_then(|rest| rest.lines().next())
            .unwrap_or_default();
        let line = line.strip_prefix("///").unwrap_or(line);
        line.strip_prefix(' ')
            .unwrap_or(line)
            .trim_end()
            .to_string()
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
        let mut module_header: Option<MemberHeader> = None;

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
            module_header = Some(header.clone());
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
        let mut module_members = Vec::new();
        let mut header = header.filter(|header| !header.is_empty());
        loop {
            let header = match header.take() {
                Some(header) => header,
                None => self.parse_member_header()?,
            };
            if header.is_empty() && self.at_eof() {
                break;
            }
            let (member_at, before) = (header.at, body.len());
            self.parse_module_member(header, &mut body)?;
            if let Some(Entry::Property(prop)) = body.get(before) {
                module_members.push((prop.name.clone(), member_at));
            }
        }
        self.check_new_types(&body, &imports)?;
        self.check_member_duplicates(&body, &imports)?;
        self.check_type_alias_cycles(&body)?;
        if checks::has_constrained_reference(&body, &imports) {
            return Err(self.error_at(
                self.prev_end,
                "`Reference` referent type argument may not include type constraints.",
            ));
        }
        if let Some(name) =
            validate::find_called_lambda(&body, amends.is_some() || extends.is_some())
        {
            return Err(self.error_at(self.prev_end, format!("Cannot find method `{name}`.")));
        }
        let module_stem = std::path::Path::new(self.name)
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
        if let Some(violation) = validate::find_override_violation(&body, &module_stem) {
            return Err(self.error_at(self.prev_end, violation.message()));
        }
        let has_const_scope = self.tokens.iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::KwConst
                    | TokenKind::KwClass
                    | TokenKind::KwTypeAlias
                    | TokenKind::At
                    | TokenKind::InterpolatedString(_)
            )
        });
        let mut deferred_const_violations = Vec::new();
        if has_const_scope {
            let (violation, deferred) = validate::find_const_violation(
                &body,
                &self.declarations,
                amends.is_some() || extends.is_some(),
            );
            if let Some(violation) = violation {
                return Err(self.error_at(self.prev_end, violation.message()));
            }
            deferred_const_violations = deferred;
        }
        // The module itself, as a class named `module` (a keyword, so no class
        // is called that), for `pkl:reflect`.
        let module_header = module_header.unwrap_or_default();
        self.declarations.push(Declaration {
            is_class: true,
            modifiers: module_header.modifiers(),
            target: None,
            name: "module".into(),
            at: module_header.at,
            doc: module_header.doc.clone(),
            annotations: module_header.annotations.clone(),
            parent: extends.clone().or_else(|| amends.clone()),
            members: module_members,
            type_parameters: Vec::new(),
            bodyless_methods: std::mem::take(&mut self.bodyless_methods),
            signatures: std::mem::take(&mut self.method_signatures),
            nested: false,
        });
        Ok(Module {
            name,
            amends,
            extends,
            imports,
            import_exprs: std::mem::take(&mut self.import_exprs),
            annotations,
            body: body.into(),
            local_type_aliases: std::mem::take(&mut self.module_local_type_aliases),
            deferred_const_violations,
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
                if let (true, Entry::TypeAlias(name, ..)) = (is_local, &entry) {
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
        let nested = self.declaration_nesting > 0;
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
        self.type_alias_parameters
            .insert(name.clone(), parameters.clone());
        self.expect(&TokenKind::Equals, "=")?;
        let outer = self.in_alias_definition.replace(name.clone());
        let ty = self.parse_type();
        self.in_alias_definition = outer;
        let written = ty?;
        let generic = (!parameters.is_empty())
            .then(|| std::sync::Arc::new((parameters.clone(), written.clone())));
        let target = written.clone();
        let ty = erase_type_parameters(written, &parameters);
        self.declarations.push(Declaration {
            is_class: false,
            modifiers: header.modifiers(),
            target: Some(target),
            name: name.as_str().into(),
            at: header.at,
            doc: header.doc.clone(),
            annotations: header.annotations.clone(),
            parent: None,
            members: Vec::new(),
            type_parameters: parameters,
            bodyless_methods: Vec::new(),
            signatures: Vec::new(),
            nested,
        });
        Ok(Entry::TypeAlias(name, ty, generic))
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
        let nested = self.declaration_nesting > 0;
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
                | TypeExpr::Constrained(name, _, _) => name,
                other => type_expr_runtime_name(&other),
            })
        } else {
            None
        };
        let mut body = Vec::new();
        let mut members = Vec::new();
        let outer_methods = std::mem::take(&mut self.bodyless_methods);
        let outer_signatures = std::mem::take(&mut self.method_signatures);
        self.declaration_nesting += 1;
        if matches!(self.peek(), TokenKind::LBrace) {
            self.advance();
            while !matches!(self.peek(), TokenKind::RBrace) {
                if self.at_eof() {
                    return Err(self.error_at(self.prev_end, "Missing `}` delimiter."));
                }
                let header = self.parse_member_header()?;
                let (member_at, before) = (header.at, body.len());
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
                if let Some(Entry::Property(prop)) = body.get(before) {
                    members.push((prop.name.clone(), member_at));
                }
            }
            self.advance(); // }
        }
        self.declaration_nesting -= 1;
        let bodyless_methods = std::mem::replace(&mut self.bodyless_methods, outer_methods);
        let signatures = std::mem::replace(&mut self.method_signatures, outer_signatures);
        self.declarations.push(Declaration {
            is_class: true,
            modifiers: header.modifiers(),
            target: None,
            name: name.as_str().into(),
            at: header.at,
            doc: header.doc.clone(),
            annotations: header.annotations.clone(),
            parent: parent.clone(),
            members,
            type_parameters: Vec::new(),
            bodyless_methods,
            signatures,
            nested,
        });
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
        let (value, body) = match self.peek() {
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
        // A module's `local obj { ... }` defines the local from the default of
        // its (absent) type, which is `Dynamic`, as `local obj = new Dynamic { ... }`.
        let (mut value, body) = match (value, body) {
            (None, Some(entries)) if local && type_ann.is_none() => (
                Some(Expr::New(Some("Dynamic".into()), entries.into(), vec![])),
                None,
            ),
            other => other,
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
            doc: header.doc.clone(),
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
        // Several plain bodies amend in turn. That is one body holding all
        // their members, unless a later body redefines or selects what an
        // earlier one defined, which amends that member instead.
        let mut defined: Vec<&Entry> = Vec::new();
        let mut redefines = false;
        for body in &bodies {
            for entry in &body.entries {
                redefines |= match entry {
                    Entry::Predicate(..) => !defined.is_empty(),
                    Entry::Property(prop) => defined.iter().any(|earlier| {
                        matches!(earlier, Entry::Property(other) if other.name == prop.name)
                    }),
                    Entry::DynProperty(key, _) => defined.iter().any(|earlier| {
                        matches!(earlier, Entry::DynProperty(other, _) if other == key)
                    }),
                    _ => false,
                };
            }
            defined.extend(body.entries.iter());
        }
        let plain = bodies.iter().all(|body| body.params.is_empty());
        if redefines && plain {
            // Each body amends the inherited member in turn. Evaluating the
            // inherited value falls back to an empty object when nothing is
            // inherited.
            return Ok(BodyList::Amended(Box::new(move |parent| {
                bodies.into_iter().fold(parent, amend_expr)
            })));
        }
        if !redefines && plain {
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
        if !matches!(kind, MemberKind::ObjectMember) {
            self.method_signatures.push(MethodSignature {
                name: name.as_str().into(),
                parameters: params.clone(),
                return_type: return_type.clone(),
            });
        }
        if !matches!(self.peek(), TokenKind::Equals) {
            if matches!(kind, MemberKind::ObjectMember) {
                return Err(self.unexpected_token("="));
            }
            // Abstract or external method without a body
            self.bodyless_methods.push(BodylessMethod {
                name: name.as_str().into(),
                at: header.at,
                doc: header.doc.clone(),
                annotations: header.annotations.clone(),
                modifiers: header.modifiers(),
                parameters: params.into_iter().map(|(name, _)| name).collect(),
            });
            return Ok(None);
        }
        self.advance(); // =
        let mut body = self.parse_expr()?;
        if let Some(return_type) = &return_type {
            infer_method_return_new(&mut body, return_type);
        }
        let body = check_signature(&params, return_type.as_ref(), body);
        let params: Vec<String> = params.into_iter().map(|(name, _)| name).collect();
        Ok(Some(Entry::Property(std::sync::Arc::new(Property {
            name: name.into(),
            type_ann: None,
            value: Some(Expr::Lambda(params.into(), std::sync::Arc::new(body))),
            body: None,
            modifiers: header.modifiers(),
            annotations: header.annotations.clone(),
            doc: header.doc.clone(),
            is_method: true,
        }))))
    }

    /// Parse `(a: Type, b, _)`; the types are kept.
    pub(super) fn parse_parameter_list(&mut self) -> Result<Vec<(String, Option<TypeExpr>)>> {
        self.expect(&TokenKind::LParen, "(")?;
        let params = self.parse_typed_parameters(&TokenKind::RParen)?;
        self.expect2(&TokenKind::RParen, ")", ",")?;
        Ok(params)
    }

    /// Parse a comma-separated parameter list up to `terminator`, allowing a
    /// trailing comma, keeping the types.
    pub(super) fn parse_typed_parameters(
        &mut self,
        terminator: &TokenKind,
    ) -> Result<Vec<(String, Option<TypeExpr>)>> {
        let mut params = Vec::new();
        let at_terminator =
            |p: &Self| std::mem::discriminant(p.peek()) == std::mem::discriminant(terminator);
        if at_terminator(self) {
            return Ok(params);
        }
        loop {
            params.push(self.parse_typed_parameter()?);
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

    /// Parse `name`, `name: Type` or `_`, keeping the type.
    pub(super) fn parse_typed_parameter(&mut self) -> Result<(String, Option<TypeExpr>)> {
        if matches!(self.peek(), TokenKind::Ident(name) if name == "_")
            && !self.is_quoted(self.peek_tok())
        {
            // An ignored parameter names nothing: give it a name no
            // expression can read, so a backticked `_` still binds.
            let name = format!("\0pklr:ignored{}", self.peek_tok().offset);
            self.advance();
            return Ok((name, None));
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
