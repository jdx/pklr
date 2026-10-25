//! Evaluating a module body, including the late-binding passes that settle
//! properties which read members a later module overrides.

use super::*;

impl Evaluator {
    pub(super) fn eval_module(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
    ) -> Result<Value> {
        self.eval_module_with_scope(module, path, depth, None, None)
    }

    pub(super) fn layer_evaluated_module_scope(&self, path: &Path, scope: &mut Scope) {
        let Some(inherited) = self.module_scopes.get(path) else {
            return;
        };
        for (name, value) in &inherited.values {
            if &**name != "this" && &**name != "module" {
                scope.set_name(name.clone(), value.clone());
            }
        }
        for (name, ty) in &inherited.type_aliases {
            scope.set_type_alias(name.clone(), ty.clone());
        }
    }

    pub(super) fn inherited_late_properties(&self, path: &Path) -> Vec<Arc<Property>> {
        self.module_scopes
            .get(path)
            .map(|snapshot| snapshot.late_properties.clone())
            .unwrap_or_default()
    }

    /// The member view a derived module exposes through `super`.
    ///
    /// A module's rendered value omits hidden properties and methods, but Pkl
    /// makes hidden properties available through `super`. Use the completed
    /// module scope and its declared properties instead of its rendered object
    /// so classes, locals, and output declarations remain absent. A module
    /// being amended cannot invoke base supermethods, unlike one extending it.
    pub(super) fn module_super_members(
        &self,
        path: &Path,
        fallback: &ObjectMap,
        include_methods: bool,
    ) -> ObjectMap {
        let Some(snapshot) = self.module_scopes.get(path) else {
            return fallback.clone();
        };

        let mut members = ObjectMap::default();
        for prop in &snapshot.late_properties {
            if prop.is_method && !include_methods {
                continue;
            }
            if let Some(value) = snapshot.values.get(prop.name.as_ref()) {
                members.insert(prop.name.clone(), value.clone());
            }
        }
        members
    }

    pub(super) fn eval_module_with_scope(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
        requested_fields: Option<HashSet<Name>>,
    ) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval(format!(
                "max import depth {} exceeded",
                self.max_depth
            )));
        }
        self.check_cancelled()?;
        self.prefetch_remote_imports(module, path);
        if let Some(name) = &module.name {
            let identity = self.module_type_namespace(path);
            self.module_names.insert(identity, name.clone());
        }
        if module
            .body
            .iter()
            .any(|entry| matches!(entry, Entry::Elem(_)))
        {
            return Err(Error::Eval("Invalid property definition".into()));
        }
        let type_namespace = self.module_type_namespace(path);
        let mut scope = Scope {
            type_namespace: Some(type_namespace),
            ..Scope::default()
        };
        seed_builtins(&mut scope);
        // Evaluated as the base of an amending or extending module, whose
        // reads of this module's bindings are not analyzed here.
        let evaluated_as_base = inherited_scope.is_some();
        if let Some(inherited_scope) = inherited_scope {
            for (key, value) in inherited_scope.flatten() {
                scope.set_name(key, value);
            }
            for (key, ty) in inherited_scope.flatten_type_aliases() {
                scope.set_type_alias(key, ty);
            }
            for (key, identity) in inherited_scope.flatten_module_identities() {
                scope.set_module_identity(key, identity);
            }
        }
        let mut inherited_type_names = HashSet::default();
        let inherited_references =
            self.inherited_reference_roots(module, path, depth + 1, &mut inherited_type_names)?;
        // Checks here also resolve the type aliases of the modules this one
        // amends or extends, which may redefine a built-in.
        let inherited_builtins: Vec<&str> = BINDING_BUILTIN_TYPES
            .iter()
            .copied()
            .filter(|name| inherited_type_names.contains(*name))
            .collect();
        let requested_output_fields = requested_fields
            .as_ref()
            .map(|fields| expand_requested_fields(&module.body, fields, &inherited_builtins));
        let analysis_entries =
            analysis_entries_for_requested_fields(&module.body, requested_output_fields.as_ref());
        let mut referenced_imports = referenced_roots(&analysis_entries);
        referenced_imports.extend(inherited_references);
        let import_field_uses = import_field_uses(&analysis_entries);

        let inherited_local_paths: Vec<_> = module
            .amends
            .iter()
            .chain(module.extends.iter())
            .filter(|uri| local_module_path(path, uri).is_some())
            .map(|uri| self.local_file_path(path, uri))
            .collect::<Result<_>>()?;
        let mut deferred_inherited_imports = Vec::new();

        // Process imports
        for import in &module.imports {
            // A relative import inside a remote module resolves against that URL.
            let resolved_uri = resolve_remote_relative(path, &import.uri);
            let uri: &str = resolved_uri.as_deref().unwrap_or(&import.uri);

            // Handle glob imports: import* "dir/*.pkl" as Alias
            if import.is_glob {
                let alias = import
                    .alias
                    .clone()
                    .ok_or_else(|| Error::Eval("import* requires an alias".into()))?;
                if !referenced_imports.contains(alias.as_str()) {
                    continue;
                }

                // Code in other modules can read the alias without being analyzed
                // here: an amended or extended base reads it from this module's
                // scope, and a module amending or extending this one inherits it.
                let requested =
                    if module.amends.is_none() && module.extends.is_none() && !evaluated_as_base {
                        glob_index_keys(&import_field_uses, &alias)
                    } else {
                        None
                    };
                let mapping = self.eval_glob_import(uri, path, depth, requested.as_ref())?;
                scope.declare(alias, mapping);
                continue;
            }

            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP import
                let alias = import.alias.clone().unwrap_or_else(|| {
                    uri.rsplit('/')
                        .next()
                        .unwrap_or(uri)
                        .strip_suffix(".pkl")
                        .unwrap_or(uri)
                        .to_string()
                });
                if !referenced_imports.contains(alias.as_str()) {
                    continue;
                }
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                let source = self.fetch_source(uri)?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, uri)?;
                    let imp_module = parser::parse_named(&tokens, &source, uri)?;
                    self.eval_module_with_scope(
                        &imp_module,
                        Path::new(uri),
                        depth + 1,
                        None,
                        requested,
                    )?
                };
                scope.declare(&alias, imported_val);
                scope.set_module_identity(alias, canonical_remote_module_identity(uri));
                continue;
            }

            if uri.starts_with("package://") {
                let pkg = self.resolve_package(uri)?;
                let fragment = uri.split_once('#').map(|(_, f)| f).unwrap_or("");
                let file_path = fragment.strip_prefix('/').unwrap_or(fragment);
                let alias = import.alias.clone().unwrap_or_else(|| {
                    file_path
                        .rsplit('/')
                        .next()
                        .unwrap_or(file_path)
                        .strip_suffix(".pkl")
                        .unwrap_or(file_path)
                        .to_string()
                });
                if !referenced_imports.contains(alias.as_str()) {
                    continue;
                }
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                // For zip packages, extract to temp dir and eval as local file
                if let PackageSource::Zip(zip_url, _) = &pkg {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(file_path);
                        let imported_val = self.eval_file_with_requested_fields(
                            &local_path,
                            depth + 1,
                            requested,
                        )?;
                        let identity = self.module_type_namespace(&local_path);
                        scope.declare(&alias, imported_val);
                        scope.set_module_identity(alias, identity);
                        continue;
                    }
                    #[cfg(not(feature = "package-zip"))]
                    {
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                }
                let url = match &pkg {
                    PackageSource::Direct { url, .. } => url.clone(),
                    PackageSource::Zip(..) => unreachable!(),
                };
                let root = match &pkg {
                    PackageSource::Direct { root, .. } => root,
                    PackageSource::Zip(..) => unreachable!(),
                };
                let source = self.fetch_direct_package_source(&url, root)?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, &url)?;
                    let imp_module = parser::parse_named(&tokens, &source, &url)?;
                    self.eval_module_with_scope(
                        &imp_module,
                        Path::new(&url),
                        depth + 1,
                        None,
                        requested,
                    )?
                };
                scope.declare(&alias, imported_val);
                scope.set_module_identity(alias, url);
                continue;
            }

            // Handle pkl: standard library imports
            if let Some(module_name) = uri.strip_prefix("pkl:") {
                let stdlib_val = self.stdlib_module(module_name, depth)?;
                let alias = import
                    .alias
                    .clone()
                    .unwrap_or_else(|| module_name.to_string());
                if !referenced_imports.contains(alias.as_str()) {
                    continue;
                }
                scope.declare(alias, stdlib_val);
                continue;
            }

            // Skip other non-local imports
            if uri.contains("://") && !uri.starts_with("file://") {
                continue;
            }

            let import_path = self.local_file_path(path, uri)?;
            let alias = import.alias.clone().unwrap_or_else(|| {
                import_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });
            if !referenced_imports.contains(alias.as_str()) {
                // Unused imports are intentionally lazy: missing local paths are
                // reported only if the imported binding is actually referenced.
                continue;
            }
            if !self.path_exists_io(&import_path)? {
                return Err(Error::ImportNotFound(import_path.display().to_string()));
            }
            let mut inherited_path = None;
            for candidate in &inherited_local_paths {
                if self.same_local_path(candidate, &import_path)? {
                    inherited_path = Some(candidate);
                    break;
                }
            }
            if let Some(inherited_path) = inherited_path {
                deferred_inherited_imports.push((alias, inherited_path.clone()));
                continue;
            }
            {
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                let imported_val =
                    self.eval_file_with_requested_fields(&import_path, depth + 1, requested)?;
                let identity = self.module_type_namespace(&import_path);
                scope.declare(&alias, imported_val);
                scope.set_module_identity(alias, identity);
            }
        }

        // The base module's members, which `super` reads.
        let mut super_members: Option<ObjectMap> = None;
        // Process amends: load base module as starting values
        let mut base_obj = ObjectMap::default();
        let mut late_inherited_properties = Vec::new();
        if let Some(amends_uri) = &module.amends {
            // A relative amends inside a remote module resolves against that URL.
            let resolved_amends = resolve_remote_relative(path, amends_uri);
            let uri: &str = resolved_amends.as_deref().unwrap_or(amends_uri);
            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP amends
                self.check_not_self(module, path, Path::new(uri), "amend")?;
                let source = self.fetch_source(uri)?;
                let tokens = lexer::lex_named(&source, uri)?;
                let base_module = parser::parse_named(&tokens, &source, uri)?;
                let base_val = self.eval_module_with_scope(
                    &base_module,
                    Path::new(uri),
                    depth + 1,
                    Some(scope.clone()),
                    None,
                )?;
                self.layer_evaluated_module_scope(Path::new(uri), &mut scope);
                late_inherited_properties.extend(self.inherited_late_properties(Path::new(uri)));
                if let Value::Object(m, _) = base_val {
                    base_obj = (*m).clone();
                }
                super_members = Some(self.module_super_members(Path::new(uri), &base_obj, false));
            } else if uri.starts_with("package://") {
                let pkg = self.resolve_package(uri)?;
                if let PackageSource::Zip(zip_url, entry) = &pkg {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(entry);
                        self.check_not_self(module, path, &local_path, "amend")?;
                        let source = self.read_to_string_io(&local_path)?;
                        let name = local_path.display().to_string();
                        let tokens = lexer::lex_named(&source, &name)?;
                        let base_module = parser::parse_named(&tokens, &source, &name)?;
                        let base_val = self.eval_module_with_scope(
                            &base_module,
                            &local_path,
                            depth + 1,
                            Some(scope.clone()),
                            None,
                        )?;
                        self.layer_evaluated_module_scope(&local_path, &mut scope);
                        late_inherited_properties
                            .extend(self.inherited_late_properties(&local_path));
                        if let Value::Object(m, _) = base_val {
                            base_obj = (*m).clone();
                        }
                        super_members =
                            Some(self.module_super_members(&local_path, &base_obj, false));
                    }
                    #[cfg(not(feature = "package-zip"))]
                    {
                        let _ = entry;
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                } else if let PackageSource::Direct { url, root } = &pkg {
                    self.check_not_self(module, path, Path::new(url.as_str()), "amend")?;
                    let source = self.fetch_direct_package_source(url, root)?;
                    let tokens = lexer::lex_named(&source, url)?;
                    let base_module = parser::parse_named(&tokens, &source, url)?;
                    let base_val = self.eval_module_with_scope(
                        &base_module,
                        Path::new(url.as_str()),
                        depth + 1,
                        Some(scope.clone()),
                        None,
                    )?;
                    self.layer_evaluated_module_scope(Path::new(url.as_str()), &mut scope);
                    late_inherited_properties
                        .extend(self.inherited_late_properties(Path::new(url.as_str())));
                    if let Value::Object(m, _) = base_val {
                        base_obj = (*m).clone();
                    }
                    super_members =
                        Some(self.module_super_members(Path::new(url.as_str()), &base_obj, false));
                }
            } else if uri == "pkl:test" {
                inherit_stdlib_module("test", &mut base_obj, &mut scope);
            } else if !uri.starts_with("pkl:")
                && (!uri.contains("://") || uri.starts_with("file://"))
            {
                let amends_path = self.local_file_path(path, uri)?;
                if self.path_exists_io(&amends_path)? {
                    self.check_not_self(module, path, &amends_path, "amend")?;
                    let base_val =
                        self.eval_file_with_scope(&amends_path, depth + 1, Some(scope.clone()))?;
                    self.layer_evaluated_module_scope(&amends_path, &mut scope);
                    late_inherited_properties.extend(self.inherited_late_properties(&amends_path));
                    if let Value::Object(m, _) = &base_val {
                        self.bind_deferred_inherited_imports(
                            &deferred_inherited_imports,
                            &amends_path,
                            &base_val,
                            &mut scope,
                        )?;
                        base_obj = (**m).clone();
                    }
                    super_members = Some(self.module_super_members(&amends_path, &base_obj, false));
                }
            }
        }

        // Inject class definitions from amends base into scope so the amending
        // module can reference them (e.g., `new Step { ... }`).
        // We re-parse the base module to find ClassDef entries, then evaluate
        // them directly (similar to extends handling), because eval_module strips
        // class definitions from its return value.
        // Also remove inherited class definitions from base_obj so they don't
        // appear in the amending module's data output.
        if let Some(amends_uri) = &module.amends {
            let resolved_amends = resolve_remote_relative(path, amends_uri);
            let uri: &str = resolved_amends.as_deref().unwrap_or(amends_uri);
            if let Some((base_module, source_path)) = self.load_parsed_module(uri, path)? {
                let mut base_scope = scope.clone();
                base_scope.type_namespace =
                    Some(self.module_type_namespace(Path::new(&source_path)));
                for entry in base_module.body.iter() {
                    if let Entry::ClassDef(name, class_mods, parent, body) = entry {
                        let defaults = self.eval_class_def(
                            name,
                            class_mods,
                            parent.as_deref(),
                            body,
                            &base_scope,
                            depth,
                        )?;
                        scope.set(name, defaults.clone());
                        if depth == 0 {
                            base_obj.shift_remove(name.as_str());
                        } else {
                            base_obj.insert(name.as_str().into(), defaults.clone());
                        }
                    }
                    // Extract converters from the base module's output block
                    // (the amending module inherits them; child overrides if present).
                    if let Entry::Property(prop) = entry
                        && &*prop.name == "output"
                        && depth == 0
                    {
                        self.extract_converters_from_ast(prop, &scope, depth);
                    }
                    if let Entry::Property(prop) = entry
                        && !has_modifier(&prop.modifiers, Modifier::Local)
                        && &*prop.name != "output"
                        && !self
                            .inherited_late_properties(Path::new(&source_path))
                            .iter()
                            .any(|known| known.name == prop.name)
                    {
                        push_base_property(&mut late_inherited_properties, prop);
                    }
                }
            }
            // Functions inherited from the base stay members, so `module.f`
            // and `this.f` reach them. They are dropped when the module is
            // rendered (see the end of this function).
        }

        // Process extends: load base module, inherit all members and scope
        if let Some(extends_uri) = &module.extends {
            // A relative extends inside a remote module resolves against that URL.
            let resolved_extends = resolve_remote_relative(path, extends_uri);
            let uri: &str = resolved_extends.as_deref().unwrap_or(extends_uri);
            if uri == "pkl:test" {
                inherit_stdlib_module("test", &mut base_obj, &mut scope);
                super_members = Some(base_obj.clone());
            }
            if !uri.contains("://") || uri.starts_with("file://") {
                let extends_path = self.local_file_path(path, uri)?;
                if self.path_exists_io(&extends_path)? {
                    self.check_not_self(module, path, &extends_path, "extend")?;
                    let base_module = self.parse_file(&extends_path)?;
                    if base_module.amends.is_some() {
                        return Err(Error::Eval(format!(
                            "Module `{}` cannot be extended or used as type because it amends another module.",
                            self.amended_module_name(&base_module, &extends_path)
                        )));
                    }
                    let ext_val =
                        self.eval_file_with_scope(&extends_path, depth + 1, Some(scope.clone()))?;
                    self.layer_evaluated_module_scope(&extends_path, &mut scope);
                    late_inherited_properties.extend(self.inherited_late_properties(&extends_path));
                    let name = extends_path.display().to_string();
                    let source = self.read_to_string_io(&extends_path)?;
                    let tokens = lexer::lex_named(&source, &name)?;
                    let ext_module = parser::parse_named(&tokens, &source, &name)?;
                    if !ext_module.annotations.iter().any(|annotation| {
                        matches!(
                            annotation.name.as_str(),
                            "pklr:module:Open" | "pklr:module:Abstract"
                        )
                    }) {
                        return Err(Error::Eval(format!(
                            "Cannot extend non-open module `{}`.",
                            extends_path
                                .file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                        )));
                    }
                    if let Value::Object(m, _) = &ext_val {
                        self.bind_deferred_inherited_imports(
                            &deferred_inherited_imports,
                            &extends_path,
                            &ext_val,
                            &mut scope,
                        )?;
                        base_obj = (**m).clone();
                    }
                    let mut base_scope = scope.clone();
                    base_scope.type_namespace = Some(self.module_type_namespace(&extends_path));
                    // Also evaluate the base module's scope (classes, locals) into our scope
                    // by re-processing its body entries
                    for entry in ext_module.body.iter() {
                        match entry {
                            Entry::ClassDef(cls_name, cls_mods, parent, body) => {
                                let defaults = self.eval_class_def(
                                    cls_name,
                                    cls_mods,
                                    parent.as_deref(),
                                    body,
                                    &base_scope,
                                    depth,
                                )?;
                                scope.set(cls_name, defaults.clone());
                                if depth == 0 {
                                    base_obj.shift_remove(cls_name.as_str());
                                } else {
                                    base_obj.insert(cls_name.as_str().into(), defaults.clone());
                                }
                            }
                            Entry::TypeAlias(name, ty) => {
                                self.eval_type_alias(name, ty, &mut scope);
                            }
                            Entry::Property(prop) if &*prop.name == "output" && depth == 0 => {
                                self.extract_converters_from_ast(prop, &scope, depth);
                            }
                            Entry::Property(prop)
                                if !has_modifier(&prop.modifiers, Modifier::Local)
                                    && &*prop.name != "output" =>
                            {
                                push_base_property(&mut late_inherited_properties, prop);
                                if let Ok(Some(value)) = self.eval_property(prop, &scope, depth) {
                                    scope.set(&prop.name, value);
                                }
                            }
                            _ => {}
                        }
                    }
                    super_members = Some(self.module_super_members(&extends_path, &base_obj, true));
                }
            } else if uri.starts_with("https://") || uri.starts_with("http://") {
                self.check_not_self(module, path, Path::new(uri), "extend")?;
                let source = self.fetch_source(uri)?;
                let tokens = lexer::lex_named(&source, uri)?;
                let ext_module = parser::parse_named(&tokens, &source, uri)?;
                if ext_module.amends.is_some() {
                    return Err(Error::Eval(format!(
                        "Module `{}` cannot be extended or used as type because it amends another module.",
                        self.amended_module_name(&ext_module, Path::new(uri))
                    )));
                }
                if !ext_module.annotations.iter().any(|annotation| {
                    matches!(
                        annotation.name.as_str(),
                        "pklr:module:Open" | "pklr:module:Abstract"
                    )
                }) {
                    return Err(Error::Eval(format!(
                        "Cannot extend non-open module `{}`.",
                        Path::new(uri)
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                    )));
                }
                let ext_val = self.eval_module_with_scope(
                    &ext_module,
                    Path::new(uri),
                    depth + 1,
                    Some(scope.clone()),
                    None,
                )?;
                self.layer_evaluated_module_scope(Path::new(uri), &mut scope);
                late_inherited_properties.extend(self.inherited_late_properties(Path::new(uri)));
                if let Value::Object(m, _) = ext_val {
                    base_obj = (*m).clone();
                }
                let mut base_scope = scope.clone();
                base_scope.type_namespace = Some(self.module_type_namespace(Path::new(uri)));
                // Inject class definitions from HTTP base into scope
                for entry in ext_module.body.iter() {
                    if let Entry::ClassDef(cls_name, cls_mods, parent, body) = entry {
                        let defaults = self.eval_class_def(
                            cls_name,
                            cls_mods,
                            parent.as_deref(),
                            body,
                            &base_scope,
                            depth,
                        )?;
                        scope.set(cls_name, defaults.clone());
                        if depth == 0 {
                            base_obj.shift_remove(cls_name.as_str());
                        } else {
                            base_obj.insert(cls_name.as_str().into(), defaults.clone());
                        }
                    }
                    if let Entry::Property(prop) = entry
                        && !has_modifier(&prop.modifiers, Modifier::Local)
                        && &*prop.name != "output"
                    {
                        push_base_property(&mut late_inherited_properties, prop);
                        if let Ok(Some(value)) = self.eval_property(prop, &scope, depth) {
                            scope.set(&prop.name, value);
                        }
                    }
                }
                super_members = Some(self.module_super_members(Path::new(uri), &base_obj, true));
            }
        }

        if (module.amends.is_some() || module.extends.is_some()) && super_members.is_none() {
            super_members = Some(base_obj.clone());
        }
        let super_bound = super_members.is_some();
        if let Some(members) = super_members {
            scope.set("super", Value::Object(Arc::new(members), None));
        }
        // Properties of an amending module are object members. Their modifier
        // applicability is a declaration error, so it must not depend on
        // whether a narrowed import evaluates that property successfully.
        if module.amends.is_some() {
            for entry in module.body.iter() {
                let Entry::Property(prop) = entry else {
                    continue;
                };
                if has_modifier(&prop.modifiers, Modifier::Local) {
                    continue;
                }
                if has_modifier(&prop.modifiers, Modifier::Fixed) {
                    return Err(Error::Eval(
                        "Modifier `fixed` is not applicable to object members.".into(),
                    ));
                }
                if has_modifier(&prop.modifiers, Modifier::Const) {
                    return Err(Error::Eval(
                        "Modifier `const` can only be applied to object members that are also `local`."
                            .into(),
                    ));
                }
            }
        }
        // An amending module cannot repeat a non-local property's type
        // annotation. Keep the inherited declaration on its replacement so
        // an untyped `new` continues to amend the base property's default
        // rather than constructing a Dynamic object. This is the module
        // counterpart of `inherit_property_types` for classes.
        let typed_body = module.amends.as_ref().and_then(|_| {
            self.inherit_module_property_types(&module.body, &late_inherited_properties, &mut scope)
        });
        let body = typed_body.as_ref().unwrap_or(&module.body);
        // These annotations were reconstructed from the amended module, not
        // written in this body. They guide `new` and body evaluation above.
        // A failed check remains attached to its member rather than eagerly
        // rejecting an import that reads a different property, matching Pkl's
        // lazy evaluation of inherited property values. A constraint that
        // cannot yet be evaluated stays lazy too.
        let synthesized_inherited_types: HashSet<&str> = typed_body
            .as_ref()
            .map(|typed| {
                module
                    .body
                    .iter()
                    .zip(typed.iter())
                    .filter_map(|(original, typed)| match (original, typed) {
                        (Entry::Property(original), Entry::Property(typed))
                            if original.type_ann.is_none() && typed.type_ann.is_some() =>
                        {
                            Some(typed.name.as_ref())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let requested_eval_fields = requested_output_fields.as_ref().map(|fields| {
            let mut dependency_entries = late_inherited_properties
                .iter()
                .cloned()
                .map(Entry::Property)
                .collect::<Vec<_>>();
            dependency_entries.extend(body.iter().cloned());
            expand_requested_fields(&dependency_entries, fields, &inherited_builtins)
        });

        // Inherited members are visible by name and through `this`/`module`
        // from the start; this module's members join them as they evaluate.
        let mut all_props = Arc::new(base_obj.clone());
        for (k, v) in &base_obj {
            scope.set(k, v.clone());
        }
        scope.set("this", Value::Object(Arc::clone(&all_props), None));
        scope.set("module", Value::Object(Arc::clone(&all_props), None));

        // Evaluate locals, classes, type aliases and properties in dependency
        // order, so a member can read one declared after it. A member that
        // fails is poisoned rather than failing the module: as in Pkl, the
        // error surfaces only where the member is used (or rendered, below).
        let mut class_names: std::collections::HashSet<Name> = std::collections::HashSet::default();
        let mut evaluated: HashMap<&str, Value> = HashMap::default();
        let mut failed: HashMap<&str, String> = HashMap::default();
        let mut failed_indices: HashSet<usize> = HashSet::default();
        let plan = Arc::clone(
            &self
                .plan_cache
                .entry(Arc::as_ptr(body) as usize)
                .or_insert_with(|| (Arc::clone(body), Arc::new(module_evaluation_plan(body))))
                .1,
        );
        let mut todo = plan.order.clone();
        // A member can fail because it read another before that one could be
        // evaluated: through a dynamic `module[key]`, which the order can't
        // account for. Members that failed are evaluated again, along with
        // the members that read them, until a pass changes nothing.
        let mut previous_failures: Option<HashMap<&str, String>> = None;
        for _ in 0..=plan.order.len() {
            for index in todo {
                let entry = &body[index];
                if let Some(name) = module_member_name(entry) {
                    failed.remove(name);
                    failed_indices.remove(&index);
                }
                match entry {
                    Entry::Property(prop)
                        if has_modifier(&prop.modifiers, Modifier::Local)
                            && prop.value.is_some() =>
                    {
                        match self.eval_expr(prop.value.as_ref().unwrap(), &scope, depth) {
                            Ok(val) => scope.declare(&prop.name, val),
                            Err(Error::Eval(message)) => {
                                scope.declare_poisoned(prop.name.clone(), message.clone());
                                failed.insert(&prop.name, message);
                                failed_indices.insert(index);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Entry::ClassDef(name, class_mods, parent, body) => {
                        class_names.insert(nm(name));
                        // Reject invalid inheritance eagerly using the same
                        // structural validation as `eval_class_def`. Other
                        // evaluation errors can depend on members declared
                        // later in the module and remain retryable.
                        if let Some(parent_name) = parent.as_deref() {
                            let parent_val = resolve_dotted(&scope, parent_name);
                            check_supertype(&scope, name, parent_name, parent_val.as_ref())?;
                        }
                        match self.eval_class_def(
                            name,
                            class_mods,
                            parent.as_deref(),
                            body,
                            &scope,
                            depth,
                        ) {
                            Ok(defaults) => {
                                scope.set_member_poison(name, None);
                                scope.declare(name, defaults.clone());
                                module_props_insert(
                                    &mut scope,
                                    &mut all_props,
                                    name.clone(),
                                    defaults,
                                );
                                let snapshot = Value::Object(Arc::clone(&all_props), None);
                                scope.set("this", snapshot.clone());
                                scope.set("module", snapshot);
                            }
                            Err(Error::Eval(message)) => {
                                scope.set_member_poison(name, Some(message.clone()));
                                scope.declare_poisoned(name.clone(), message.clone());
                                failed.insert(name, message);
                                failed_indices.insert(index);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Entry::TypeAlias(name, ty) => {
                        self.eval_type_alias(name, ty, &mut scope);
                        // A non-local alias to a class is a module member, like
                        // the class, and its value is schema, not data. An
                        // inherited property of the same name stays data.
                        if !module.local_type_aliases.contains(name)
                            && !base_obj.contains_key(name.as_str())
                            && matches!(
                                scope.get(name),
                                Some(Value::Object(_, Some(source)))
                                    if source.kind == ObjectKind::Class
                            )
                        {
                            class_names.insert(nm(name));
                        }
                    }
                    Entry::Property(prop) => {
                        let mods = &prop.modifiers;
                        if has_modifier(mods, Modifier::Local) {
                            continue;
                        }
                        // Extract renderer converters from the `output` block AST,
                        // then skip it (it's not included in the output).
                        // Clear any base-inherited converters so child overrides take precedence.
                        if &*prop.name == "output" {
                            if depth == 0 {
                                self.output_props.push(Arc::clone(prop));
                                self.extract_converters_from_ast(prop, &scope, depth);
                            }
                            continue;
                        }
                        if let Some(fields) = &requested_eval_fields
                            && !fields.contains(&prop.name)
                        {
                            continue;
                        }
                        // abstract/external properties must have a value (or be overridden)
                        if (has_modifier(mods, Modifier::Abstract)
                            || has_modifier(mods, Modifier::External))
                            && prop.value.is_none()
                            && prop.body.is_none()
                        {
                            if let Some(v) = base_obj.get(prop.name.as_ref()) {
                                // Satisfied by base — add to scope so other properties can reference it
                                scope.set(&prop.name, v.clone());
                            } else if has_modifier(mods, Modifier::External) {
                                return Err(Error::Eval(format!(
                                    "external property '{}' must be assigned a value in {}",
                                    prop.name,
                                    path.display()
                                )));
                            } else if depth == 0 && !module_is_abstract(module) {
                                return Err(Error::Eval(format!(
                                    "abstract property '{}' must be assigned a value in {}",
                                    prop.name,
                                    path.display()
                                )));
                            }
                            continue;
                        }
                        let property_result = self
                            .eval_property_with(prop, &scope, depth, true)
                            .and_then(|value| {
                                if synthesized_inherited_types.contains(prop.name.as_ref())
                                    && let (Some(value), Some(ty)) = (&value, &prop.type_ann)
                                {
                                    self.check_inherited_mapping_value_types(
                                        value, ty, &scope, depth,
                                    )?;
                                }
                                Ok(value)
                            });
                        let val = match property_result {
                            Ok(value) => value,
                            // Module properties are late-bound. Keep an unresolved
                            // template expression deferred until a consumer actually
                            // requires it; poisoned locals still surface that error.
                            Err(Error::Eval(message))
                                if is_unresolved_template_error(&message)
                                    && (module_is_abstract(module)
                                        || property_reference_names(prop).iter().any(|name| {
                                            late_inherited_properties
                                                .iter()
                                                .any(|inherited| inherited.name == *name)
                                        })) =>
                            {
                                None
                            }
                            Err(Error::Eval(message)) => {
                                scope.declare_poisoned(prop.name.clone(), message.clone());
                                failed.insert(&prop.name, message);
                                failed_indices.insert(index);
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        if let Some(v) = val {
                            if let Some(message) = late_inherited_properties
                                .iter()
                                .filter(|inherited| inherited.name == prop.name)
                                .find_map(|inherited| immutable_member_error(prop, inherited))
                            {
                                return Err(Error::Eval(message));
                            }
                            // Always add to scope so other properties can reference it
                            scope.declare(&prop.name, v.clone());
                            // Track in all_props (including hidden) for `this`/`module`
                            module_props_insert(
                                &mut scope,
                                &mut all_props,
                                prop.name.clone(),
                                v.clone(),
                            );
                            // Update `this` and `module` with all properties (including hidden)
                            let snapshot = Value::Object(Arc::clone(&all_props), None);
                            scope.set("this", snapshot.clone());
                            scope.set("module", snapshot);
                            evaluated.insert(&prop.name, v);
                        }
                    }
                    _ => {}
                }
            }
            if failed_indices.is_empty() || previous_failures.as_ref() == Some(&failed) {
                break;
            }
            previous_failures = Some(failed.clone());
            todo = plan.affected_by(&failed_indices);
        }

        // The planner may evaluate an annotation class or alias after the
        // annotated property. Validate once the final module scope is ready.
        check_module_annotations(module, &scope)?;

        // Assemble the module object in declaration order: inherited members,
        // then classes (exported so they're accessible via dotted paths, e.g.
        // `import "helpers.pkl"` → `helpers.ClassName`), then properties. A
        // failed property that would be rendered fails the module.
        let mut out = base_obj;
        let mut poisoned_members = IndexMap::new();
        for entry in body.iter() {
            // A typealias to a class is a module member too, so an importer
            // can write `new Alias.Name {}`.
            let exported = match entry {
                Entry::ClassDef(name, ..) => Some(name),
                Entry::TypeAlias(name, _) if class_names.contains(name.as_str()) => Some(name),
                _ => None,
            };
            if let Some(name) = exported {
                if let Some(value) = scope.get(name) {
                    out.insert(name.as_str().into(), value.clone());
                } else if let Some(message) = failed.get(name.as_str()) {
                    poisoned_members.insert(
                        nm(name),
                        PoisonedMember {
                            message: message.clone(),
                            rendered: false,
                        },
                    );
                }
            }
        }
        for entry in body.iter() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            // Locals participate in failure convergence but are never module
            // members or rendered output.
            if has_modifier(&prop.modifiers, Modifier::Local) {
                continue;
            }
            let rendered = !has_modifier(&prop.modifiers, Modifier::Hidden)
                && requested_output_fields
                    .as_ref()
                    .is_none_or(|fields| fields.contains(&prop.name));
            if let Some(message) = failed.get(prop.name.as_ref()) {
                if rendered && depth == 0 {
                    return Err(Error::Eval(message.clone()));
                }
                self.rendered_member_failed |= rendered;
                poisoned_members.insert(
                    prop.name.clone(),
                    PoisonedMember {
                        message: message.clone(),
                        rendered,
                    },
                );
                continue;
            }
            if let Some(value) = evaluated.remove(prop.name.as_ref())
                && rendered
                && (depth > 0 || should_render_property_value(prop, &value))
            {
                out.insert(prop.name.as_ref().into(), value);
            }
        }

        // Pkl properties are late-bound. Re-evaluate inherited expressions after
        // child overrides have populated the scope (for example `uses` derived
        // from an action module's overridden `action` and `version`).
        let child_property_names: HashSet<&str> = body
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some(prop.name.as_ref()),
                _ => None,
            })
            .collect();
        if depth == 0
            && !module_is_abstract(module)
            && let Some(prop) = late_inherited_properties.iter().find(|prop| {
                has_modifier(&prop.modifiers, Modifier::Abstract)
                    && prop.value.is_none()
                    && prop.body.is_none()
                    && !child_property_names.contains(prop.name.as_ref())
            })
        {
            return Err(Error::Eval(format!(
                "abstract property '{}' must be assigned a value in {}",
                prop.name,
                path.display()
            )));
        }
        let child_candidates = body
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop)
                    if &*prop.name != "output" && (prop.value.is_some() || prop.body.is_some()) =>
                {
                    Some(prop)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut late_bound_names = late_inherited_properties
            .iter()
            .map(|prop| prop.name.clone())
            .collect::<HashSet<_>>();
        // A body amending an inherited object is late-bound like the inherited
        // expression it amends: the object is rebuilt once overrides settle.
        let composed_amendments: Vec<Arc<Property>> = child_candidates
            .iter()
            .filter(|prop| prop.body.is_some() && prop.value.is_none())
            .filter_map(|prop| {
                let inherited = late_inherited_properties
                    .iter()
                    .rfind(|inherited| inherited.name == prop.name && inherited.value.is_some())?;
                let composed = compose_amended_object(inherited, prop, None);
                composed.value.is_some().then_some(composed)
            })
            .collect();
        let mut late_child_properties: Vec<&Property> = Vec::new();
        for composed in &composed_amendments {
            late_bound_names.insert(composed.name.clone());
            late_child_properties.push(composed);
        }
        loop {
            let mut added = false;
            for prop in &child_candidates {
                if late_child_properties
                    .iter()
                    .any(|selected: &&Property| selected.name == prop.name)
                {
                    continue;
                }
                let refs = property_reference_names(prop);
                if refs.iter().any(|name| late_bound_names.contains(name))
                    || (refs.contains(DYNAMIC_SIBLING_REF) && !late_bound_names.is_empty())
                {
                    late_bound_names.insert(prop.name.clone());
                    late_child_properties.push(*prop);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        let late_passes = late_inherited_properties.len() + late_child_properties.len();
        // `super` in a base module's property is that module's own base, not
        // this module's: those keep the value the base computed.
        let reads_super: HashSet<&str> = if super_bound {
            late_inherited_properties
                .iter()
                .filter(|prop| property_reads_module_super(prop))
                .map(|prop| prop.name.as_ref())
                .collect()
        } else {
            HashSet::default()
        };
        // A pass that reproduces every value, as `settled_eq` compares them,
        // is the last: the next would start from the same state and repeat it.
        //
        // A value that holds an object or a lambda is never reproduced in that
        // sense, since each evaluation makes a new one that captures the
        // scope as it then stands. Such a value can still be the same data,
        // and a pass whose values all are is quiet: it changes nothing a
        // reader can see. It is not necessarily the last. An object or
        // lambda keeps the scope it was made in, so a change reaches a
        // property that reads it through such a capture one pass later for
        // every forward reference on the way, and no value changes while it
        // travels. A property listed before a reader is read as this pass
        // has just evaluated it, one listed after it as the previous pass
        // left it.
        //
        // `forward_reference_chain` finds the longest chain of such forward
        // references. As many quiet passes in a row as that are enough if
        // the model holds, and `QUIET_PASS_MARGIN` more run as slack. The
        // values are what end the loop, and they are the safety net for reads
        // the model misses: a pass that changes the data keeps it running.
        // The loop never runs past `late_passes`.
        //
        // Pass 0 never counts as quiet: it starts from the values the base
        // module computed.
        let chain_memo = std::cell::Cell::new(None);
        let settled_after = || -> usize {
            if let Some(settled_after) = chain_memo.get() {
                return settled_after;
            }
            let late_order: Vec<&Property> = late_inherited_properties
                .iter()
                .map(|prop| &**prop)
                .filter(|prop| {
                    !child_property_names.contains(prop.name.as_ref())
                        && !reads_super.contains(prop.name.as_ref())
                })
                .chain(late_child_properties.iter().copied())
                .collect();
            let settled_after = forward_reference_chain(&late_order) + QUIET_PASS_MARGIN;
            chain_memo.set(Some(settled_after));
            settled_after
        };
        // Quiet passes are counted after pass 0 and the loop breaks before
        // counting at `late_passes`, so at most `late_passes - 1` can be
        // counted. If `settled_after` is not below `late_passes`, no run of
        // quiet passes can end the loop, and telling a quiet pass from a
        // changed one decides nothing: only `settled_eq` can end it. Then
        // `same_data` is not called, which matters for values that capture a
        // large scope.
        let can_stop = || settled_after() < late_passes;
        let mut pass = 0;
        let mut quiet_passes = 0;
        loop {
            #[cfg(test)]
            {
                self.late_passes_run += 1;
            }
            let mut change = Change::None;
            let mut errored = false;
            for prop in &late_inherited_properties {
                if child_property_names.contains(prop.name.as_ref())
                    || reads_super.contains(prop.name.as_ref())
                {
                    continue;
                }
                match self.eval_property_with(prop, &scope, depth, true) {
                    Ok(Some(value)) => {
                        change = change.then(scope.get(&prop.name), &value, can_stop).then(
                            all_props.get(prop.name.as_ref()),
                            &value,
                            can_stop,
                        );
                        scope.set(&prop.name, value.clone());
                        module_props_insert(
                            &mut scope,
                            &mut all_props,
                            prop.name.clone(),
                            value.clone(),
                        );
                        if !has_modifier(&prop.modifiers, Modifier::Hidden)
                            && (depth > 0 || should_render_property_value(prop, &value))
                            && requested_output_fields
                                .as_ref()
                                .is_none_or(|fields| fields.contains(&prop.name))
                        {
                            out.insert(prop.name.as_ref().into(), value);
                        }
                        let snapshot = Value::Object(Arc::clone(&all_props), None);
                        scope.set("this", snapshot.clone());
                        scope.set("module", snapshot);
                    }
                    Ok(None) => {}
                    Err(Error::Eval(message))
                        if pass < late_passes
                            || (module_is_abstract(module)
                                && is_unresolved_template_error(&message)) =>
                    {
                        errored = true;
                    }
                    Err(error) => return Err(error),
                }
            }
            for prop in &late_child_properties {
                if requested_eval_fields
                    .as_ref()
                    .is_some_and(|fields| !fields.contains(&prop.name))
                {
                    continue;
                }
                match self
                    .eval_property_with(prop, &scope, depth, true)
                    .and_then(|value| {
                        if synthesized_inherited_types.contains(prop.name.as_ref())
                            && let (Some(value), Some(ty)) = (&value, &prop.type_ann)
                        {
                            self.check_inherited_mapping_value_types(value, ty, &scope, depth)?;
                        }
                        Ok(value)
                    }) {
                    Ok(Some(value)) => {
                        change = change.then(scope.get(&prop.name), &value, can_stop);
                        scope.set(&prop.name, value.clone());
                        if has_modifier(&prop.modifiers, Modifier::Local) {
                            continue;
                        }
                        change = change.then(all_props.get(prop.name.as_ref()), &value, can_stop);
                        module_props_insert(
                            &mut scope,
                            &mut all_props,
                            prop.name.clone(),
                            value.clone(),
                        );
                        if !has_modifier(&prop.modifiers, Modifier::Hidden)
                            && (depth > 0 || should_render_property_value(prop, &value))
                            && requested_output_fields
                                .as_ref()
                                .is_none_or(|fields| fields.contains(&prop.name))
                        {
                            out.insert(prop.name.as_ref().into(), value);
                        }
                        let snapshot = Value::Object(Arc::clone(&all_props), None);
                        scope.set("this", snapshot.clone());
                        scope.set("module", snapshot);
                    }
                    Ok(None) => {}
                    Err(Error::Eval(message))
                        if pass < late_passes
                            || (module_is_abstract(module)
                                && is_unresolved_template_error(&message)) =>
                    {
                        errored = true;
                    }
                    Err(error) => return Err(error),
                }
            }
            if pass >= late_passes {
                break;
            }
            let settled = match change {
                Change::None => true,
                Change::Data => {
                    quiet_passes = 0;
                    false
                }
                Change::Capture => {
                    if pass > 0 {
                        quiet_passes += 1;
                    }
                    quiet_passes >= settled_after()
                }
            };
            if !settled {
                pass += 1;
            } else if errored {
                // Errors are only reported on the last pass, so a settled
                // pass that still failed goes straight there.
                pass = late_passes;
            } else {
                break;
            }
        }

        if depth == 0 {
            for prop in &late_inherited_properties {
                if let Some(value) = all_props.get(prop.name.as_ref())
                    && !should_render_property_value(prop, value)
                    && !child_property_names.contains(prop.name.as_ref())
                {
                    out.shift_remove(prop.name.as_ref());
                }
            }
        }

        // Check declarations only after this module's final scope is complete:
        // constraints and aliases may refer to members declared later.
        if !evaluated_as_base {
            // These are the effective non-local properties after an amendment:
            // an override in this body replaces its inherited declaration.
            // Dependents may themselves be inherited, so error propagation must
            // consider both sets rather than only the child body below.
            let effective_properties = late_inherited_properties
                .iter()
                .map(|prop| prop.as_ref())
                .filter(|prop| !child_property_names.contains(prop.name.as_ref()))
                .chain(body.iter().filter_map(|entry| match entry {
                    Entry::Property(prop) => Some(prop.as_ref()),
                    _ => None,
                }))
                .collect::<Vec<_>>();
            let mut inherited_type_failures = IndexMap::new();
            for entry in body.iter() {
                if let Entry::Property(prop) = entry
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                    && (prop.value.is_some() || prop.body.is_some())
                    && prop.type_ann.is_some()
                    && scope.is_declared(&prop.name)
                    && let Some(value) = scope.get(&prop.name)
                {
                    if synthesized_inherited_types.contains(prop.name.as_ref()) {
                        let ty = prop.type_ann.as_ref().expect("checked above");
                        if !type_has_constraint(ty, &scope) {
                            continue;
                        }
                        let failure = if type_is_runtime_checkable(ty, &scope) {
                            match self.eval_type_check(value, ty, &scope, depth + 1) {
                                Ok(true) => None,
                                Ok(false) => Some(format!(
                                    "property '{}' expected {}, got {}",
                                    prop.name,
                                    display_type_expr(ty),
                                    value_type_name(value)
                                )),
                                // An explicit Pkl `throw` is observable after the
                                // member is materialized. Keep other evaluator
                                // limitations deferred, as before.
                                Err(Error::Eval(message))
                                    if type_constraint_has_explicit_throw(ty, &scope)
                                        && !is_deferred_constraint_evaluator_error(&message) =>
                                {
                                    Some(message)
                                }
                                Err(Error::Eval(_)) => None,
                                Err(error) => return Err(error),
                            }
                        } else {
                            None
                        };
                        if let Some(message) = failure {
                            inherited_type_failures.insert(prop.name.clone(), message);
                        }
                    } else {
                        self.check_declared_property_type(prop, value, &scope, depth)?;
                    }
                }
            }

            // Values are evaluated before these late inherited checks. Carry a
            // constrained member's failure through direct and transitive reads
            // so a dependent output cannot retain a stale value. Probe the
            // expression with failed members hidden rather than relying on a
            // syntactic reference list: an unselected `if` branch, or a
            // caught failed read, must remain usable.
            if !inherited_type_failures.is_empty() {
                let mut dependency_entries = late_inherited_properties
                    .iter()
                    .cloned()
                    .map(Entry::Property)
                    .collect::<Vec<_>>();
                dependency_entries.extend(body.iter().cloned());
                let dependency_plan = module_evaluation_plan(&dependency_entries);
                while !inherited_type_failures.is_empty() {
                    // Find every member that can reach a failed member. This
                    // follows local helpers transitively. Restricting probes
                    // to that closure preserves narrowed evaluation's
                    // laziness without leaving a value computed before its
                    // inherited type check stale.
                    let mut failure_dependents = inherited_type_failures
                        .keys()
                        .cloned()
                        .collect::<HashSet<_>>();
                    loop {
                        let mut expanded = false;
                        for entry in &dependency_entries {
                            let Entry::Property(prop) = entry else {
                                continue;
                            };
                            let refs = property_reference_names(prop);
                            if refs.iter().any(|name| failure_dependents.contains(name))
                                && failure_dependents.insert(prop.name.clone())
                            {
                                expanded = true;
                            }
                        }
                        if !expanded {
                            break;
                        }
                    }

                    let mut added = false;
                    let mut probe_scope = scope.child();
                    let mut probe_properties = (*all_props).clone();
                    for (name, message) in &inherited_type_failures {
                        probe_scope.poison(name.clone(), message.clone());
                        probe_scope.set_member_poison(name, Some(message.clone()));
                        probe_properties.shift_remove(name.as_ref());
                    }
                    let probe_snapshot = Value::Object(Arc::new(probe_properties), None);
                    probe_scope.set("this", probe_snapshot.clone());
                    probe_scope.set("module", probe_snapshot);
                    // A local helper can retain an already-computed value,
                    // and a method captures its scope when it is built.
                    // Rebuild affected locals and lambdas in the probe scope,
                    // so they observe the poisoned member rather than the
                    // prior successful value.
                    for &index in &dependency_plan.order {
                        let Entry::Property(prop) = &dependency_entries[index] else {
                            continue;
                        };
                        if !has_modifier(&prop.modifiers, Modifier::Local)
                            && !matches!(prop.value.as_ref(), Some(Expr::Lambda(..)))
                            || !failure_dependents.contains(&prop.name)
                        {
                            continue;
                        }
                        let Some(value) = &prop.value else {
                            continue;
                        };
                        match self.eval_expr(value, &probe_scope, depth + 1) {
                            Ok(value) => probe_scope.declare(&prop.name, value),
                            Err(Error::Eval(message)) => {
                                probe_scope.declare_poisoned(prop.name.clone(), message)
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    for prop in &effective_properties {
                        if has_modifier(&prop.modifiers, Modifier::Local)
                            || inherited_type_failures.contains_key(prop.name.as_ref())
                            || requested_eval_fields
                                .as_ref()
                                .is_some_and(|fields| !fields.contains(&prop.name))
                            || !failure_dependents.contains(&prop.name)
                        {
                            continue;
                        }
                        if let Err(Error::Eval(message)) =
                            self.eval_property(prop, &probe_scope, depth + 1)
                            && inherited_type_failures
                                .values()
                                .any(|failure| failure == &message)
                        {
                            inherited_type_failures.insert(prop.name.clone(), message.clone());
                            added = true;
                        }
                    }
                    if !added {
                        break;
                    }
                }
            }
            for prop in effective_properties {
                if let Some(message) = inherited_type_failures.get(prop.name.as_ref()) {
                    let rendered = !has_modifier(&prop.modifiers, Modifier::Hidden)
                        && requested_output_fields
                            .as_ref()
                            .is_none_or(|fields| fields.contains(&prop.name));
                    if rendered && depth == 0 {
                        return Err(Error::Eval(message.clone()));
                    }
                    out.shift_remove(prop.name.as_ref());
                    scope.set_member_poison(&prop.name, Some(message.clone()));
                    self.rendered_member_failed |= rendered;
                    poisoned_members.insert(
                        prop.name.clone(),
                        PoisonedMember {
                            message: message.clone(),
                            rendered,
                        },
                    );
                }
            }
        }

        // A top-level amending or extending module renders data, not the
        // classes it inherited. An imported module keeps those classes so
        // dotted access (for example `child.Person`) remains available.
        if depth == 0 {
            out.retain(|name, _| !inherited_type_names.contains(&**name));
        }

        // At the top level (depth 0), strip class definitions and lambdas from
        // the serialized output — they're schema/functions, not data.
        // Imported modules (depth > 0) keep them so dotted access works
        // (e.g., `helpers.ClassName`).
        if depth == 0 {
            for name in &class_names {
                out.shift_remove(name.as_ref());
            }
            out.retain(|_, v| !matches!(v, Value::Lambda(..)));
        }
        if let Some(fields) = &requested_output_fields {
            out.retain(|name, _| fields.contains(&**name));
        }
        // If the module declares any `@Deprecated` properties, attach a
        // minimal ObjectSource carrying just the deprecation map so field
        // access can warn lazily. Modules without @Deprecated keep `None`
        // source to avoid changing amend behavior in the common case.
        // An abstract module also keeps a source, so `new` can reject it.
        let deprecated = collect_deprecated(body);
        let is_abstract = module_is_abstract(module);
        let source = if deprecated.is_empty() && poisoned_members.is_empty() && !is_abstract {
            None
        } else {
            Some(Arc::new(ObjectSource {
                entries: Vec::new().into(),
                captured: SourceScope::default(),
                body_members: HashSet::default(),
                is_open: true,
                is_abstract,
                type_name: None,
                // An abstract module records its name for the `new` error.
                type_identity: is_abstract.then(|| module_display_name(module, path)),
                parent_type_names: Vec::new(),
                parent_type_identities: Vec::new(),
                entry_scopes: Vec::new(),
                evaluated_properties: Vec::new(),
                elements: Vec::new(),
                mapping_value_types: Vec::new(),
                deprecated,
                poisoned_members: (!poisoned_members.is_empty())
                    .then(|| Arc::new(poisoned_members)),
                kind: ObjectKind::Object,
                is_parsed_json: false,
                prototype: None,
            }))
        };
        let mut effective_late_properties = IndexMap::new();
        let template_locals = module_local_bindings(body, &scope);
        for (prop, inherited) in late_inherited_properties
            .iter()
            .map(|prop| (prop, true))
            .chain(body.iter().filter_map(|entry| match entry {
                Entry::Property(prop)
                    if !has_modifier(&prop.modifiers, Modifier::Local)
                        && &*prop.name != "output" =>
                {
                    Some((prop, false))
                }
                _ => None,
            }))
        {
            let locals = (!inherited).then_some(template_locals.as_ref()).flatten();
            let prop = match effective_late_properties.get(&prop.name) {
                Some(base) => compose_amended_object(base, prop, locals),
                None => match (&prop.value, locals) {
                    (Some(value), Some(_)) => Arc::new(Property {
                        value: Some(with_template_locals(locals, prop, value.clone())),
                        ..(**prop).clone()
                    }),
                    _ => prop.clone(),
                },
            };
            effective_late_properties.insert(prop.name.clone(), prop);
        }
        self.module_scopes.insert(
            path.to_path_buf(),
            ModuleScopeSnapshot {
                values: scope.flatten(),
                type_aliases: scope.flatten_type_aliases(),
                module_identities: scope.flatten_module_identities(),
                late_properties: effective_late_properties.into_values().collect(),
            },
        );
        // An imported module whose failed property this output renders fails
        // the output, as rendering that property directly would.
        if depth == 0
            && self.rendered_member_failed
            && let Some(message) = out.values().find_map(rendered_member_failure)
        {
            return Err(Error::Eval(message.to_string()));
        }
        let module_map = Arc::new(out);
        if depth == 0 {
            self.eval_module_output(module, &module_map, &scope, path, depth)?;
        }
        Ok(Value::Object(module_map, source))
    }
}

/// Standard library types that are not annotation classes.
pub(super) const STDLIB_NON_ANNOTATION_TYPES: &[&str] = &[
    "Any",
    "Null",
    "Boolean",
    "Number",
    "Int",
    "Float",
    "String",
    "Duration",
    "DataSize",
    "Regex",
    "Object",
    "Dynamic",
    "Typed",
    "Listing",
    "Mapping",
    "List",
    "Set",
    "Map",
    "Pair",
    "IntSeq",
    "Function",
    "Function0",
    "Function1",
    "Function2",
    "Function3",
    "Function4",
    "Function5",
    "VarArgs",
    "Bytes",
    "Collection",
    "Module",
    "Class",
    "TypeAlias",
];

/// Check that each annotation on a module property names an annotation
/// class: a subclass of `Annotation`, not another class or a value.
pub(super) fn check_module_annotations(module: &Module, scope: &Scope) -> Result<()> {
    let annotated = module
        .body
        .iter()
        .any(|entry| matches!(entry, Entry::Property(prop) if !prop.annotations.is_empty()));
    if !annotated {
        return Ok(());
    }
    let classes: HashMap<&str, Option<&str>> = module
        .body
        .iter()
        .filter_map(|entry| match entry {
            Entry::ClassDef(name, _, parent, _) => Some((name.as_str(), parent.as_deref())),
            _ => None,
        })
        .collect();
    let properties: HashSet<&str> = module
        .body
        .iter()
        .filter_map(|entry| match entry {
            Entry::Property(prop) => Some(prop.name.as_ref()),
            _ => None,
        })
        .collect();
    let resolve_alias = |name: &str| -> Option<String> {
        let mut current = name.to_string();
        let mut seen = HashSet::default();
        while let Some(ty) = scope.get_type_alias(&current) {
            match ty {
                crate::parser::TypeExpr::Named(target)
                    if !target.starts_with('*') && seen.insert(current.clone()) =>
                {
                    current = target.clone();
                }
                _ => return None,
            }
        }
        Some(current)
    };
    let is_annotation_class = |name: &str| {
        let mut current = name.to_string();
        for _ in 0..=classes.len() {
            match classes.get(current.as_str()) {
                Some(Some(parent)) => match resolve_alias(parent) {
                    Some(parent) => current = parent,
                    None => return false,
                },
                Some(None) => return false,
                None => return !STDLIB_NON_ANNOTATION_TYPES.contains(&current.as_str()),
            }
        }
        false
    };
    for entry in module.body.iter() {
        let Entry::Property(prop) = entry else {
            continue;
        };
        for annotation in &prop.annotations {
            if annotation.name.contains('.') {
                continue;
            }
            let Some(resolved) = resolve_alias(&annotation.name) else {
                return Err(Error::Eval("Expected an annotation class.".into()));
            };
            let name = resolved.as_str();
            if classes.contains_key(name) {
                if !is_annotation_class(name) {
                    return Err(Error::Eval("Expected an annotation class.".into()));
                }
                continue;
            }
            if scope.get_type_alias(name).is_some() {
                continue;
            }
            if name == annotation.name && properties.contains(name) {
                return Err(Error::Eval(format!(
                    "Expected `{name}` to be a type, but it is not."
                )));
            }
            match scope.get(name) {
                Some(Value::String(marker)) if &**marker == name => {
                    if name != "Annotation" && STDLIB_NON_ANNOTATION_TYPES.contains(&name) {
                        return Err(Error::Eval("Expected an annotation class.".into()));
                    }
                }
                Some(Value::Object(_, Some(source)))
                    if source.type_name.as_deref() == Some(name) => {}
                Some(_) => {
                    return Err(Error::Eval(format!(
                        "Expected `{name}` to be a type, but it is not."
                    )));
                }
                None if STDLIB_NON_ANNOTATION_TYPES.contains(&name) => {
                    return Err(Error::Eval("Expected an annotation class.".into()));
                }
                None => {}
            }
        }
    }
    Ok(())
}

/// Make standard-library module members available through both the module
/// object and bare names in a module that amends or extends it.
pub(super) fn inherit_stdlib_module(name: &str, base_obj: &mut ObjectMap, scope: &mut Scope) {
    if let Value::Object(members, _) = stdlib_module(name) {
        for (member, value) in members.iter() {
            base_obj.insert(member.clone(), value.clone());
            scope.set(member, value.clone());
        }
    }
}

/// Whether re-evaluating a late-bound property reproduced `old`.
///
/// Cheap and conservative: a lambda or an object that carries its definition
/// scope only counts as unchanged when it is the very same allocation, since
/// comparing the scopes they capture is as costly as the evaluation itself.
/// A false "changed" only costs another pass.
pub(super) fn settled_eq(old: Option<&Value>, new: Option<&Value>) -> bool {
    match (old, new) {
        (None, None) => true,
        (Some(old), Some(new)) => match (old, new) {
            (Value::Object(left, left_source), Value::Object(right, right_source)) => {
                let same_source = match (left_source, right_source) {
                    (None, None) => true,
                    (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                    _ => false,
                };
                same_source
                    && (Arc::ptr_eq(left, right)
                        || (left.len() == right.len()
                            && left.iter().zip(right.iter()).all(
                                |((left_key, left_value), (right_key, right_value))| {
                                    left_key == right_key
                                        && settled_eq(Some(left_value), Some(right_value))
                                },
                            )))
            }
            (Value::List(left), Value::List(right)) => {
                left.kind() == right.kind()
                    && left.len() == right.len()
                    && left
                        .iter()
                        .zip(right.iter())
                        .all(|(left, right)| settled_eq(Some(left), Some(right)))
            }
            (Value::Float(left), Value::Float(right)) => left.to_bits() == right.to_bits(),
            (Value::Lambda(..), _) | (_, Value::Lambda(..)) => false,
            _ => old == new,
        },
        _ => false,
    }
}

#[cfg(test)]
thread_local! {
    /// How many times the late-binding passes called `same_data`, on this thread.
    static SAME_DATA_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn same_data_counted(old: &Value, new: &Value) -> bool {
    #[cfg(test)]
    SAME_DATA_CHECKS.with(|checks| checks.set(checks.get() + 1));
    old.same_data(new)
}

/// How the values a late-binding pass produced compare with the ones they
/// replace, the largest over all of them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Change {
    /// Each value is one `settled_eq` accepts.
    None,
    /// Each value is the same data, but one holds an object or lambda made
    /// anew, which captured the scope as it now stands.
    Capture,
    /// A value is different data.
    Data,
}

impl Change {
    /// The larger of `self` and how `new` compares with `old`. Only a value
    /// that holds an object, list or lambda can be the same data without being
    /// one `settled_eq` accepts, so a scalar that differs is different data
    /// at once. `same_data` is only called if `can_stop` says a quiet pass
    /// could end the loop.
    fn then(self, old: Option<&Value>, new: &Value, can_stop: impl Fn() -> bool) -> Self {
        if self == Self::Data {
            return self;
        }
        let this = if settled_eq(old, Some(new)) {
            Self::None
        } else if matches!(new, Value::Object(..) | Value::List(_) | Value::Lambda(..))
            && old.is_some_and(|old| can_stop() && same_data_counted(old, new))
        {
            Self::Capture
        } else {
            Self::Data
        };
        self.max(this)
    }
}

pub(super) fn stdlib_module(name: &str) -> Value {
    if let Some(module) = stdlib::module(name) {
        return module;
    }
    let mut map = ObjectMap::default();
    if name == "base" {
        map.insert("Regex".into(), Value::String("Regex".into()));
    }
    if name == "test" {
        for (method, builtin) in [("catch", TEST_CATCH), ("catchOrNull", TEST_CATCH_OR_NULL)] {
            map.insert(method.into(), builtin_function(builtin, &["fun"]));
        }
    }
    Value::Object(Arc::new(map), None)
}

/// Late-binding passes re-evaluate every inherited property. They must stop
/// once the values settle, not run once per inherited property.
#[cfg(all(test, feature = "native-io"))]
mod late_binding_pass_tests {
    use super::{Entry, Evaluator, Property, forward_reference_chain};

    /// How many properties the template declares.
    const PROPERTIES: usize = 60;

    /// A pass per property over `PROPERTIES` properties takes more than
    /// `PROPERTIES` passes; settling takes a few.
    const MAX_PASSES: usize = PROPERTIES / 4;

    /// A directory of modules, removed when dropped.
    struct Modules(std::path::PathBuf);

    impl Drop for Modules {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Evaluate `Child.pkl` from the modules in `files`, and return the result
    /// with the number of late-binding passes it ran.
    fn try_evaluate_modules(
        files: &[(&str, &str)],
    ) -> (Result<serde_json::Value, crate::Error>, usize) {
        let dir = Modules(std::env::temp_dir().join(format!(
            "pklr-late-passes-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        for (name, source) in files {
            std::fs::write(dir.0.join(name), source).unwrap();
        }
        let mut evaluator = Evaluator::default();
        let value = evaluator.eval_file(&dir.0.join("Child.pkl"));
        (
            value.map(|value| value.to_json()),
            evaluator.late_passes_run,
        )
    }

    /// [`try_evaluate_modules`] for modules that evaluate.
    fn evaluate_modules(files: &[(&str, &str)]) -> (serde_json::Value, usize) {
        let (value, passes) = try_evaluate_modules(files);
        (value.unwrap(), passes)
    }

    /// Evaluate `Child.pkl`, which amends `Template.pkl`.
    fn evaluate_amendment(template: &str, child: &str) -> (serde_json::Value, usize) {
        evaluate_modules(&[("Template.pkl", template), ("Child.pkl", child)])
    }

    #[test]
    fn amending_module_does_not_run_a_pass_per_inherited_property() {
        // `base`, then `p1 = base + 1`, `p2 = p1 + 1`, and so on.
        let mut template = String::from("base = 1\n");
        for n in 1..=PROPERTIES {
            let previous = if n == 1 {
                "base".to_string()
            } else {
                format!("p{}", n - 1)
            };
            template.push_str(&format!("p{n} = {previous} + 1\n"));
        }
        let (json, passes) = evaluate_amendment(&template, "amends \"Template.pkl\"\nbase = 10\n");

        assert_eq!(json[format!("p{PROPERTIES}")], 10 + PROPERTIES);
        assert!(
            passes <= MAX_PASSES,
            "{passes} late-binding passes for {PROPERTIES} properties (at most {MAX_PASSES} expected)"
        );
    }

    /// A template of objects and a lambda, which keep the scope they were made
    /// in and so are never the same allocation twice, settles in a few passes
    /// as one of plain values does.
    #[test]
    fn amending_a_template_of_objects_does_not_run_a_pass_per_property() {
        let mut template = String::from("base = 1\nadd = (x) -> x + base\n");
        for n in 1..=PROPERTIES {
            template.push_str(&format!(
                "o{n} = new {{ v = base; w = add.apply({n}); items = new Listing {{ base }} }}\n"
            ));
        }
        let (json, passes) = evaluate_amendment(&template, "amends \"Template.pkl\"\nbase = 10\n");

        assert_eq!(json[format!("o{PROPERTIES}")]["v"], 10);
        assert_eq!(json[format!("o{PROPERTIES}")]["w"], 10 + PROPERTIES);
        assert_eq!(json["o1"]["items"][0], 10);
        assert!(
            passes <= MAX_PASSES,
            "{passes} late-binding passes for {PROPERTIES} objects (at most {MAX_PASSES} expected)"
        );
    }

    /// The same, where a module between them amends each object, so that the
    /// amendments are late-bound too, and the last module overrides what the
    /// objects read.
    #[test]
    fn amending_amended_objects_does_not_run_a_pass_per_property() {
        let mut template = String::from("base = 1\n");
        let mut middle = String::from("amends \"Template.pkl\"\n");
        for n in 1..=PROPERTIES {
            template.push_str(&format!("o{n} = new {{ a = {n}; b = base }}\n"));
            middle.push_str(&format!("o{n} {{ a = {} }}\n", n + 1));
        }
        let (json, passes) = evaluate_modules(&[
            ("Template.pkl", &template),
            ("Middle.pkl", &middle),
            ("Child.pkl", "amends \"Middle.pkl\"\nbase = 10\n"),
        ]);

        assert_eq!(json[format!("o{PROPERTIES}")]["a"], PROPERTIES + 1);
        assert_eq!(json[format!("o{PROPERTIES}")]["b"], 10);
        assert_eq!(json["o1"]["b"], 10);
        assert!(
            passes <= 2 * MAX_PASSES,
            "{passes} late-binding passes for {PROPERTIES} amended objects"
        );
    }

    /// `x0 = x1`, `x1 = x2`, and so on to `x{reads} = 1`, with an unrelated
    /// `w` for the amending module to override, so no value ever changes.
    fn chain_of_reads(reads: usize) -> String {
        let mut template = String::new();
        for n in 0..reads {
            template.push_str(&format!("x{n} = x{}\n", n + 1));
        }
        template.push_str(&format!("x{reads} = 1\nw = 0\n"));
        template
    }

    /// The same chain, of objects: each is made anew by every pass, capturing
    /// the scope as it then stands.
    fn chain_of_objects(reads: usize) -> String {
        let mut template = String::new();
        for n in 0..reads {
            template.push_str(&format!("x{n} = new {{ v = x{}.v }}\n", n + 1));
        }
        template.push_str(&format!("x{reads} = new {{ v = 1 }}\nw = 0\n"));
        template
    }

    /// A chain of lambdas that each read the whole module, `f1` to `f{n}`.
    fn chain_of_lambdas_reading_the_module(n: usize) -> String {
        let mut template = String::from("res = f1.apply(1)\n");
        for k in 1..n {
            template.push_str(&format!(
                "hidden f{k} = (x) -> module.f{}.apply(x)\n",
                k + 1
            ));
        }
        template.push_str(&format!("hidden f{n} = (x) -> x + 1\n"));
        template
    }

    /// Where the chain of forward references plus the margin is as long as
    /// the passes can run, no run of quiet passes can end them, so only
    /// `settled_eq` can. The lambdas here capture the module and are never
    /// reproduced, so every pass runs, as on a build without the quiet-pass
    /// rule, and `same_data` is not called, which would walk everything
    /// each captured scope holds on every pass.
    #[test]
    fn a_chain_no_quiet_pass_can_end_never_compares_by_data() {
        let child = "amends \"Template.pkl\"\nbase = 10\n";
        let (json, passes) = evaluate_amendment(&chain_of_lambdas_reading_the_module(30), child);
        assert_eq!(json["res"], 2);
        // The template's own pass and pass 0 of the amending module, then a pass for
        // each of the inherited properties, and one more.
        assert_eq!(passes, 2 + 31, "every pass runs");
        assert_eq!(super::SAME_DATA_CHECKS.with(std::cell::Cell::get), 0);
    }

    /// The chain and the margin as long as the passes, to the pass: the last
    /// quiet pass would be the last pass, so nothing is compared either. Six
    /// properties, a chain of four reads and two passes of margin.
    #[test]
    fn a_chain_exactly_as_long_as_the_passes_is_never_compared_by_data() {
        let child = "amends \"Template.pkl\"\nw = 5\n";
        let (_, passes) = evaluate_amendment(&chain_of_objects(4), child);
        assert_eq!(passes, 1 + 1 + 6, "every pass runs");
        assert_eq!(super::SAME_DATA_CHECKS.with(std::cell::Cell::get), 0);
    }

    /// The guard above must not turn the comparison off where it can end the
    /// passes: a chain of four reads and the margin are fewer passes than
    /// the template has properties.
    #[test]
    fn a_chain_a_quiet_pass_can_end_is_compared_by_data() {
        let child = "amends \"Template.pkl\"\nw = 5\n";
        let mut template = chain_of_objects(4);
        for n in 0..8 {
            template.push_str(&format!("y{n} = {n}\n"));
        }
        let (_, passes) = evaluate_amendment(&template, child);
        assert!(passes < 2 * 6, "{passes} passes");
        assert!(super::SAME_DATA_CHECKS.with(std::cell::Cell::get) > 0);
    }

    /// The same chain, of objects that differ from one pass to the next only
    /// in their bare elements, which are not in the member map: each holds
    /// the elements of the next, and the last holds `base`.
    fn chain_of_elements(reads: usize) -> String {
        let mut template = String::from("base = 1\n");
        for n in 0..reads {
            template.push_str(&format!("x{n} = new Dynamic {{ w = 0; ...x{} }}\n", n + 1));
        }
        template.push_str(&format!("x{reads} = new Dynamic {{ w = 0; base }}\n"));
        template
    }

    /// A change that shows only in the elements of an object is a change, and
    /// keeps the passes running as one in a member does.
    #[test]
    fn a_change_only_in_elements_restarts_the_quiet_count() {
        let child = "amends \"Template.pkl\"\nbase = 10\n";
        let (_, short) = evaluate_amendment(&chain_of_elements(3), child);
        let (_, long) = evaluate_amendment(&chain_of_elements(9), child);
        // The template's own pass and pass 0, then the passes the last change
        // takes to arrive (one per link, as the elements travel the chain)
        // and the quiet ones that follow it: the chain and the margin.
        assert_eq!((short, long), (7, 13));
    }

    /// Values that every pass reproduces end the passes at the first that
    /// changes nothing, however long the chain: that pass would be repeated.
    /// Counted: the template's own pass and pass 0 of the amending module.
    #[test]
    fn reproduced_values_end_the_passes_at_the_first_that_changes_nothing() {
        let child = "amends \"Template.pkl\"\nw = 5\n";
        let (_, short) = evaluate_amendment(&chain_of_reads(0), child);
        let (_, long) = evaluate_amendment(&chain_of_reads(40), child);
        assert_eq!((short, long), (2, 2));
    }

    /// Readers of the whole module read every property, and two of them read
    /// each other. That must not keep the passes of a template of plain values
    /// running: nothing in it captures a scope.
    #[test]
    fn readers_of_the_module_do_not_keep_plain_values_from_settling() {
        let mut template = String::from("base = 1\n");
        for n in 0..PROPERTIES {
            template.push_str(&format!("p{n} = base + {n}\n"));
        }
        template.push_str("hidden m0 = module.base\nhidden m1 = this.base\n");
        let (json, passes) = evaluate_amendment(&template, "amends \"Template.pkl\"\nbase = 10\n");
        assert_eq!(json["p0"], 10);
        assert!(passes <= 3, "{passes} late-binding passes");
    }

    /// Passes that change nothing the data shows still run until more of them
    /// than the longest chain of forward references have passed, and that is
    /// what carries a change through a capture that lags behind. Counted: the
    /// template's own pass, pass 0 of the amending module, a pass for each
    /// of the four reads in the chain, and two quiet passes beyond it.
    #[test]
    fn quiet_passes_run_past_the_longest_forward_chain() {
        let child = "amends \"Template.pkl\"\nw = 5\n";
        // Properties enough that the passes are not capped by their number.
        let with_filler = |mut template: String| {
            for n in 0..12 {
                template.push_str(&format!("y{n} = {n}\n"));
            }
            template
        };
        let (_, without_reads) = evaluate_amendment(&with_filler(chain_of_objects(0)), child);
        let (_, with_reads) = evaluate_amendment(&with_filler(chain_of_objects(4)), child);
        assert_eq!((without_reads, with_reads), (1 + 1 + 2, 1 + 1 + 4 + 2));
    }

    /// A pass that fails and changes nothing is not repeated: errors are only
    /// reported on the last pass, so the passes go straight to it. Counted: the
    /// template's pass, pass 0 and the quiet passes of the amending module
    /// (two beyond a chain of none), and the last.
    #[test]
    fn a_settled_pass_that_still_fails_goes_straight_to_the_last() {
        let mut template = String::from("base = 1\nbad = base + throw(\"boom\")\n");
        for n in 0..PROPERTIES {
            template.push_str(&format!("o{n} = new {{ v = {n} }}\n"));
        }
        let (result, passes) = try_evaluate_modules(&[
            ("Template.pkl", &template),
            ("Child.pkl", "amends \"Template.pkl\"\nbase = 2\n"),
        ]);
        let message = result.unwrap_err().to_string();
        assert!(message.contains("boom"), "{message}");
        assert!(passes <= MAX_PASSES, "{passes} late-binding passes");
    }

    /// An override in the amending module ends a chain of reads: what it reads
    /// is not the template's property any more.
    #[test]
    fn an_overridden_property_ends_the_chain_of_reads() {
        let template = chain_of_objects(4);
        let (_, kept) = evaluate_amendment(&template, "amends \"Template.pkl\"\nw = 5\n");
        let (_, cut) = evaluate_amendment(
            &template,
            "amends \"Template.pkl\"\nw = 5\nx2 = new { v = 7 }\n",
        );
        assert!(
            cut < kept,
            "{cut} passes with the chain cut, {kept} without"
        );
    }

    /// A change can arrive after quiet passes. Each `o{n}` reads the next
    /// through an object it amends, which keeps the scope it was made in, so
    /// an override of `base` reaches `res` one capture per link without any
    /// value showing it on the way. Pass 3 changes `res` after passes 1 and 2
    /// found nothing different; the quiet passes are counted again from it,
    /// and six follow it: the chain of four forward references, and the
    /// margin of two. Dropping the reset ends the passes before `res` is right.
    #[test]
    fn a_change_after_quiet_passes_restarts_the_count() {
        let mut template = String::from("res = (o1) { go = true }.v\n");
        for n in 1..6 {
            template.push_str(&format!(
                "o{n} = new {{ go = false; v = if (go) (o{}) {{ go = true }}.v else 0 }}\n",
                n + 1
            ));
        }
        template
            .push_str("o6 = new { go = false; v = if (go) r else 0 }\nbase = 1\nr = base + 1\n");
        let (json, passes) = evaluate_amendment(&template, "amends \"Template.pkl\"\nbase = 10\n");
        assert_eq!(json["res"], 11);
        // The template's pass, and passes 0 to 9 of the amending module.
        assert_eq!(passes, 1 + 10);
    }

    /// The longest chain among the properties of `source`, listed `times`
    /// times in a row as an amending module lists those it inherits.
    fn longest_chain_listed(source: &str, times: usize) -> usize {
        let tokens = crate::lexer::lex(source).unwrap();
        let module = crate::parser::parse(&tokens).unwrap();
        let once: Vec<&Property> = module
            .body
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some(&**prop),
                _ => None,
            })
            .collect();
        let late: Vec<&Property> = (0..times).flat_map(|_| once.iter().copied()).collect();
        forward_reference_chain(&late)
    }

    fn longest_chain(source: &str) -> usize {
        longest_chain_listed(source, 1)
    }

    fn references(source: &str) -> Vec<String> {
        let tokens = crate::lexer::lex(source).unwrap();
        let module = crate::parser::parse(&tokens).unwrap();
        let Some(Entry::Property(prop)) = module.body.first() else {
            panic!("expected a property");
        };
        let mut names: Vec<String> = super::property_reference_names(prop)
            .iter()
            .map(|name| name.to_string())
            .collect();
        names.sort();
        names
    }

    /// A read by computed key names the module it indexes, so the chain does
    /// not need to treat `DYNAMIC_SIBLING_REF` itself as a read of everything.
    #[test]
    fn a_read_by_computed_key_names_the_module_it_reads_through() {
        for source in [
            "m = module[\"B\".toLowerCase()]\n",
            "m = this[\"B\".toLowerCase()]\n",
            "local m = module\n",
            "local m = this\n",
        ] {
            let names = references(source);
            assert!(
                names.iter().any(|name| name == "module" || name == "this"),
                "{source:?} references only {names:?}"
            );
        }
    }

    /// Which earlier copy of a property a later one reads changes how many
    /// forward references stand in its way: `b` reads the copy of `d` just
    /// before it, whose own read of `a` is not forward, and not the first.
    #[test]
    fn a_property_reads_the_latest_earlier_copy_of_what_it_reads() {
        let source = "d = a\nc = b + d\nb = d\na = 1\n";
        assert_eq!(longest_chain_listed(source, 2), 1);
    }

    #[test]
    fn properties_declared_after_what_they_read_have_no_forward_chain() {
        assert_eq!(longest_chain("a = 1\nb = a\nc = b + a\n"), 0);
    }

    #[test]
    fn each_read_of_a_later_property_adds_one_to_the_chain() {
        assert_eq!(longest_chain("c = b\nb = a\na = 1\n"), 2);
        assert_eq!(longest_chain("c = b\nb = 1\na = 1\n"), 1);
        assert_eq!(longest_chain("c = a\nb = a\na = 1\n"), 1);
    }

    #[test]
    fn a_property_that_reads_the_module_reads_every_other_property() {
        assert_eq!(longest_chain("m = module\nb = 1\n"), 1);
        assert_eq!(longest_chain("m = module\nb = a\na = 1\n"), 2);
        assert_eq!(longest_chain("b = 1\nm = module\n"), 0);
        assert_eq!(longest_chain("m = this\nb = 1\n"), 1);
    }

    #[test]
    fn a_property_that_reads_a_computed_key_reads_every_other_property() {
        assert_eq!(longest_chain("m = module[\"B\".toLowerCase()]\nb = 1\n"), 1);
    }

    #[test]
    fn properties_that_read_each_other_have_the_longest_possible_chain() {
        assert_eq!(longest_chain("a = b\nb = a\nc = 1\n"), 3);
    }

    #[test]
    fn a_property_that_reads_itself_has_the_longest_possible_chain() {
        assert_eq!(longest_chain("f = (x) -> f.apply(x)\nb = 1\n"), 2);
    }

    #[test]
    fn a_property_that_reads_through_outer_reads_every_other_property() {
        assert_eq!(longest_chain("o = new { v = outer.x }\nx = 1\n"), 1);
        assert_eq!(longest_chain("o = new { v = 1 }\nx = 1\n"), 0);
    }
}
