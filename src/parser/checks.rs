//! Checks on parsed declarations that pkl reports as syntax errors: modifiers,
//! duplicate members, type alias cycles and the module's minimum pkl version.

use super::*;

impl<'a> Parser<'a> {
    /// Check modifiers against the kind of member they are attached to.
    pub(super) fn check_modifiers(&self, header: &MemberHeader, kind: MemberKind) -> Result<()> {
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

    /// Reject `new X {}` where `X` names a module property rather than a
    /// class or type alias: types and values live in separate namespaces.
    pub(super) fn check_new_types(&self, body: &[Entry], imports: &[Import]) -> Result<()> {
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
                .any(|entry| matches!(entry, Entry::Property(p) if *p.name == **name));
            if is_property && !is_type(name) {
                return Err(self.error_at(
                    *offset,
                    format!("Expected `{name}` to be a type, but it is not."),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn check_min_pkl_version(
        &self,
        annotations: &[Annotation],
        module_name: Option<&str>,
    ) -> Result<()> {
        let required = annotations
            .iter()
            .filter(|annotation| annotation.name == "ModuleInfo")
            .flat_map(|annotation| &annotation.body)
            .find_map(|entry| match entry {
                Entry::Property(property) if &*property.name == "minPklVersion" => {
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

    pub(super) fn check_member_duplicates(
        &self,
        entries: &[Entry],
        imports: &[Import],
    ) -> Result<()> {
        self.check_entry_duplicates(entries, Some(imports), false, false)
    }

    /// Validate duplicate names after parsing a body. Object values need the
    /// complete body before a duplicate can be rejected: Pkl merges a body
    /// containing a spread or generator at run time instead.
    pub(super) fn check_entry_duplicates(
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
                    (!names.insert(property.name.to_string())).then(|| property.name.to_string())
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

    pub(super) fn check_entry_child_duplicates(&self, entry: &Entry) -> Result<()> {
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

    pub(super) fn check_expr_duplicates(&self, expr: &Expr) -> Result<()> {
        match expr {
            Expr::New(_, body, _) | Expr::ObjectBody(body) | Expr::InferredNew(_, body) => {
                self.check_entry_duplicates(body, None, true, false)?
            }
            Expr::Field(value, _)
            | Expr::NullSafeField(value, _)
            | Expr::Unop(_, value)
            | Expr::Throw(value)
            | Expr::TemplateLocals(_, value)
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

    pub(super) fn check_type_alias_cycles(&self, entries: &[Entry]) -> Result<()> {
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
}

pub(super) fn modifier_name(modifier: &Modifier) -> &'static str {
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

/// Collect the class names of a `new` expression's type arguments.
pub(super) fn generic_param_names(ty: &TypeExpr, out: &mut Vec<String>) {
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

pub(super) fn collect_type_names(ty: &TypeExpr, out: &mut Vec<String>) {
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

pub(super) fn constant_entry_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::String(value) => Some(format!("\"{value}\"")),
        Expr::Int(value) => Some(value.to_string()),
        Expr::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn entries_define_type(entries: &[Entry], name: &str) -> bool {
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

pub(super) fn expr_defines_type(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::New(_, body, _) | Expr::ObjectBody(body) | Expr::InferredNew(_, body) => {
            entries_define_type(body, name)
        }
        Expr::Field(value, _)
        | Expr::NullSafeField(value, _)
        | Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::TemplateLocals(_, value)
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

pub(super) fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub(super) fn required_string(version: &(u64, u64, u64)) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

pub(super) fn constant_string(expr: &Expr) -> Option<String> {
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
