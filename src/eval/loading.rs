//! Locating, fetching, parsing and caching modules and their imports.

use super::*;

impl Evaluator {
    /// The local file a `file:` or relative module URI in the module at
    /// `current_path` names.
    pub(super) fn local_file_path(&mut self, current_path: &Path, uri: &str) -> Result<PathBuf> {
        if uri.starts_with("file:") {
            file_uri_path(uri)
        } else {
            self.resolve_local_path(current_path, uri)
        }
    }

    /// Resolve a local module or resource URI against the module at
    /// `current_path`. A triple-dot URI is searched for upward from the
    /// module's directory (see [`Self::resolve_triple_dot`]); a path that
    /// cannot be found resolves to a path that does not exist.
    pub(super) fn resolve_local_path(&mut self, current_path: &Path, uri: &str) -> Result<PathBuf> {
        if let Some(triple_dot) = parse_triple_dot_path(uri)? {
            return Ok(self
                .resolve_triple_dot(current_path, triple_dot)?
                // Keep a miss in the importing module's namespace. Returning
                // the URI by itself would let a CWD entry named `...` win.
                .unwrap_or_else(|| current_path.parent().unwrap_or(Path::new(".")).join(uri)));
        }
        Ok(current_path.parent().unwrap_or(Path::new(".")).join(uri))
    }

    /// Resolve the triple-dot `path` for the module at `current_path`.
    /// Inside an extracted package the search stops at the package root.
    pub(super) fn resolve_triple_dot(
        &mut self,
        current_path: &Path,
        path: &str,
    ) -> Result<Option<PathBuf>> {
        // Canonicalize through the evaluator's capabilities, which resolves
        // native relative entry paths and supports virtual path namespaces.
        // Unlike an unconditional `std::path::absolute`, this never
        // substitutes the host CWD for a capability-backed evaluator.
        let current_path = match self.canonicalize_io(current_path) {
            Ok(path) => path,
            // Native evaluators use the default base path. When a relative
            // unsaved path cannot be canonicalized yet, make its ancestors
            // concrete so the normal upward search still works.
            Err(_) if current_path.is_relative() && self.base_path == Path::new(".") => {
                std::path::absolute(current_path).unwrap_or_else(|_| current_path.to_path_buf())
            }
            // A capability host owns its namespace, including relative
            // virtual paths, so retain the path it supplied on a failure.
            Err(_) => current_path.to_path_buf(),
        };
        #[cfg(feature = "package-zip")]
        let roots: Vec<PathBuf> = self.package_dirs.values().cloned().collect();
        #[cfg(feature = "package-zip")]
        let root = roots.into_iter().find_map(|root| {
            let root = self.canonicalize_io(&root).unwrap_or(root);
            current_path.starts_with(&root).then_some(root)
        });
        #[cfg(not(feature = "package-zip"))]
        let root: Option<PathBuf> = None;
        resolve_triple_dot(&current_path, path, root.as_deref(), |candidate| {
            self.path_exists_io(candidate)
        })
    }

    pub(super) fn module_type_namespace(&mut self, path: &Path) -> String {
        if path.to_string_lossy().contains("://") {
            return path.display().to_string();
        }
        self.canonicalize_io(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()
    }

    pub(super) fn fetch_source(&mut self, url: &str) -> Result<String> {
        if self
            .package_http_roots
            .iter()
            .any(|root| url.starts_with(root))
        {
            return self.fetch_package_source(url);
        }
        let rewritten = self.rewrite_url(url);
        let fetch_url = rewritten.as_ref();
        if let Some(cached) = self.http_cache.get(fetch_url) {
            return Ok(cached.clone());
        }
        if self.offline {
            return Err(Error::Eval(format!(
                "offline mode prevented HTTP fetch for {url}"
            )));
        }
        let body = self.fetch_text_io(fetch_url)?;
        self.http_cache.insert(fetch_url.to_string(), body.clone());
        Ok(body)
    }

    /// The names the modules `module` amends or extends (transitively) read,
    /// adding to `type_names` the type aliases and classes they declare.
    pub(super) fn inherited_reference_roots(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
        type_names: &mut HashSet<Name>,
    ) -> Result<HashSet<Name>> {
        let mut refs = HashSet::default();
        if depth > self.max_depth {
            return Ok(refs);
        }
        for uri in [module.amends.as_deref(), module.extends.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some((base_module, source_path)) = self.load_parsed_module(uri, path)? {
                refs.extend(referenced_roots(&base_module.body));
                type_names.extend(base_module.body.iter().filter_map(|entry| match entry {
                    Entry::TypeAlias(name, _) | Entry::ClassDef(name, ..) => Some(nm(name)),
                    _ => None,
                }));
                refs.extend(self.inherited_reference_roots(
                    &base_module,
                    Path::new(&source_path),
                    depth + 1,
                    type_names,
                )?);
            }
        }
        Ok(refs)
    }

    pub(super) fn load_module_source(
        &mut self,
        uri: &str,
        path: &Path,
    ) -> Result<Option<(String, String)>> {
        let resolved = resolve_remote_relative(path, uri);
        let uri = resolved.as_deref().unwrap_or(uri);
        if uri.starts_with("https://") || uri.starts_with("http://") {
            let source = self.fetch_source(uri)?;
            return Ok(Some((source, uri.to_string())));
        }
        if uri.starts_with("package://") {
            let pkg = self.resolve_package(uri)?;
            match &pkg {
                PackageSource::Direct { url, root } => {
                    let source = self.fetch_direct_package_source(url, root)?;
                    return Ok(Some((source, url.clone())));
                }
                PackageSource::Zip(zip_url, entry) => {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(entry);
                        let source = self.read_to_string_io(&local_path)?;
                        return Ok(Some((source, local_path.display().to_string())));
                    }
                    #[cfg(not(feature = "package-zip"))]
                    {
                        let _ = entry;
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                }
            }
        }
        if uri.starts_with("pkl:") || (uri.contains("://") && !uri.starts_with("file://")) {
            return Ok(None);
        }
        let import_path = self.local_file_path(path, uri)?;
        if !self.path_exists_io(&import_path)? {
            return Ok(None);
        }
        let source = self.read_to_string_io(&import_path)?;
        Ok(Some((source, import_path.display().to_string())))
    }

    /// Load and parse the module `uri` names, sharing the parse cache with
    /// evaluation so a base module is not lexed and parsed again for each
    /// analysis. `None` when the module is absent or does not parse.
    pub(super) fn load_parsed_module(
        &mut self,
        uri: &str,
        path: &Path,
    ) -> Result<Option<(Arc<Module>, String)>> {
        let Some((source, source_path)) = self.load_module_source(uri, path)? else {
            return Ok(None);
        };
        // Remote sources are keyed by URL, which never collides with an
        // absolute local path.
        let key = if source_path.contains("://") {
            Some(PathBuf::from(&source_path))
        } else {
            self.canonicalize_io(Path::new(&source_path)).ok()
        };
        if let Some(module) = key.as_ref().and_then(|key| self.parse_cache.get(key)) {
            return Ok(Some((Arc::clone(module), source_path)));
        }
        let Ok(tokens) = lexer::lex_named(&source, &source_path) else {
            return Ok(None);
        };
        let Ok(module) = parser::parse_named(&tokens, &source, &source_path) else {
            return Ok(None);
        };
        let module = Arc::new(module);
        if let Some(key) = key {
            self.parse_cache.insert(key, Arc::clone(&module));
        }
        Ok(Some((module, source_path)))
    }

    /// Evaluate `source` as the module at `path`.
    pub fn eval_source(&mut self, source: &str, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        self.eval_source_inner(source, path)
    }

    pub(super) fn eval_source_inner(&mut self, source: &str, path: &Path) -> Result<Value> {
        // Seed import cache for the entry file so circular back-references work.
        // Mark it in flight too, so a narrowed import that reads this
        // placeholder is not cached.
        let canonical = self.canonicalize_io(path).ok();
        if let Some(canonical) = &canonical {
            self.import_cache
                .insert(canonical.clone(), Value::Object(Arc::default(), None));
            self.imports_in_flight.insert(canonical.clone());
        }
        let result = self.eval_entry_module(source, path);
        if let Some(canonical) = canonical {
            self.imports_in_flight.remove(&canonical);
            // Update cache with real value
            if let Ok(val) = &result {
                self.import_cache.insert(canonical, val.clone());
            }
        }
        result
    }

    pub(super) fn eval_entry_module(&mut self, source: &str, path: &Path) -> Result<Value> {
        let name = path.display().to_string();
        let tokens = lexer::lex_named(source, &name)?;
        let module = parser::parse_named(&tokens, source, &name)?;
        self.eval_module(&module, path, 0)
    }

    /// Evaluate a local pkl file by path.
    pub fn eval_file(&mut self, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        let source = self.read_to_string_io(path)?;
        self.eval_source_inner(&source, path)
    }

    /// Read, lex, parse, and evaluate a local file (with caching).
    /// Inserts a placeholder before evaluation to break circular imports.
    pub(super) fn eval_import_file(&mut self, path: &Path, depth: usize) -> Result<Value> {
        let canonical = self.canonicalize_io(path)?;
        if let Some(cached) = self.cached_import(&canonical) {
            return Ok(cached);
        }
        // Insert empty placeholder to break circular imports
        self.import_cache
            .insert(canonical.clone(), Value::Object(Arc::default(), None));
        self.imports_in_flight.insert(canonical.clone());
        let result = self.eval_file_inner(path, &canonical, depth);
        self.imports_in_flight.remove(&canonical);
        if result.is_err() {
            // Remove stale placeholder on failure so retries can re-evaluate
            self.import_cache.remove(&canonical);
        }
        result
    }

    pub(super) fn eval_file_with_requested_fields(
        &mut self,
        path: &Path,
        depth: usize,
        requested_fields: Option<HashSet<Name>>,
    ) -> Result<Value> {
        if requested_fields.is_none() {
            return self.eval_import_file(path, depth);
        }
        let canonical = self.canonicalize_io(path)?;
        if let Some(cached) = self.cached_import(&canonical) {
            return Ok(cached);
        }
        let mut fields: Vec<Name> = requested_fields.iter().flatten().cloned().collect();
        fields.sort_unstable();
        let key = (canonical.clone(), fields);
        if let Some(cached) = self.narrowed_import_cache.get(&key) {
            return Ok(cached.clone());
        }
        self.import_cache
            .insert(canonical.clone(), Value::Object(Arc::default(), None));
        self.imports_in_flight.insert(canonical.clone());
        let placeholder_reads = self.placeholder_reads;
        let result = self.eval_file_requested_fields_inner(path, depth, requested_fields);
        self.imports_in_flight.remove(&canonical);
        self.import_cache.remove(&canonical);
        // A module evaluated while one of its imports was still in flight may
        // have seen that import's placeholder, so only cache self-contained
        // results.
        if let Ok(value) = &result
            && self.placeholder_reads == placeholder_reads
        {
            self.narrowed_import_cache.insert(key, value.clone());
        }
        result
    }

    /// The cached value of a local import, counting reads of an in-flight
    /// placeholder.
    pub(super) fn cached_import(&mut self, canonical: &Path) -> Option<Value> {
        let cached = self.import_cache.get(canonical)?.clone();
        if self.imports_in_flight.contains(canonical) {
            self.placeholder_reads += 1;
        }
        Some(cached)
    }

    pub(super) fn eval_file_requested_fields_inner(
        &mut self,
        path: &Path,
        depth: usize,
        requested_fields: Option<HashSet<Name>>,
    ) -> Result<Value> {
        let module = self.parse_file(path)?;
        self.eval_module_with_scope(&module, path, depth, None, requested_fields)
    }

    pub(super) fn eval_file_inner(
        &mut self,
        path: &Path,
        canonical: &Path,
        depth: usize,
    ) -> Result<Value> {
        let val = self.eval_file_inner_with_scope(path, depth, None)?;
        self.import_cache
            .insert(canonical.to_path_buf(), val.clone());
        Ok(val)
    }

    pub(super) fn eval_file_with_scope(
        &mut self,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
    ) -> Result<Value> {
        if inherited_scope.is_none() {
            return self.eval_import_file(path, depth);
        }
        let canonical = self.canonicalize_io(path)?;
        if !self.scoped_imports_in_flight.insert(canonical.clone()) {
            self.placeholder_reads += 1;
            return Ok(Value::Object(Arc::default(), None));
        }
        let result = self.eval_file_inner_with_scope(path, depth, inherited_scope);
        self.scoped_imports_in_flight.remove(&canonical);
        result
    }

    pub(super) fn eval_file_inner_with_scope(
        &mut self,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
    ) -> Result<Value> {
        let module = self.parse_file(path)?;
        let val = self.eval_module_with_scope(&module, path, depth, inherited_scope, None)?;
        Ok(val)
    }

    pub(super) fn parse_file(&mut self, path: &Path) -> Result<Arc<Module>> {
        let canonical = self.canonicalize_io(path).ok();
        if let Some(module) = canonical.as_ref().and_then(|c| self.parse_cache.get(c)) {
            return Ok(Arc::clone(module));
        }
        let source = self.read_to_string_io(path)?;
        let name = path.display().to_string();
        let tokens = lexer::lex_named(&source, &name)?;
        let module = Arc::new(parser::parse_named(&tokens, &source, &name)?);
        if let Some(canonical) = canonical {
            self.parse_cache.insert(canonical, Arc::clone(&module));
        }
        Ok(module)
    }

    /// The name pkl gives the class of a module that amends another: the
    /// name of the module at the root of its amends chain.
    pub(super) fn amended_module_name(&mut self, module: &Module, path: &Path) -> String {
        let mut name = module_display_name(module, path);
        let mut current = (module.amends.clone(), path.to_path_buf());
        for _ in 0..self.max_depth {
            let (Some(uri), from) = current else { break };
            if uri.contains("://") {
                break;
            }
            let Ok(base_path) = self.resolve_local_path(&from, &uri) else {
                break;
            };
            let Ok(base) = self.parse_file(&base_path) else {
                break;
            };
            name = module_display_name(&base, &base_path);
            current = (base.amends.clone(), base_path);
        }
        name
    }

    /// Reject a module that amends or extends itself.
    pub(super) fn check_not_self(
        &mut self,
        module: &Module,
        path: &Path,
        base_path: &Path,
        verb: &str,
    ) -> Result<()> {
        let same = match (self.canonicalize_io(path), self.canonicalize_io(base_path)) {
            (Ok(a), Ok(b)) => a == b,
            _ => path == base_path,
        };
        if same {
            return Err(Error::Eval(format!(
                "Module `{}` cannot {verb} itself.",
                module_display_name(module, path)
            )));
        }
        Ok(())
    }

    /// Resolve a glob import pattern to a mapping of matched module paths to
    /// their evaluated values. Shared by `import* "glob" as Alias` declarations
    /// and `import*("glob")` expressions.
    ///
    /// Keys are the matched paths relative to `path`'s directory, matching pkl.
    /// The enclosing module is skipped: pklr evaluates matched modules eagerly,
    /// so including it would recurse until the import depth limit.
    ///
    /// `requested` is the set of keys the importing code reads, when it only
    /// reads the mapping as `Alias["key"]`. Then only those modules are
    /// evaluated. A requested name that is not a matched key (such as `keys`
    /// or `length`) needs the whole mapping, so every module is evaluated.
    pub(super) fn eval_glob_import(
        &mut self,
        uri: &str,
        path: &Path,
        depth: usize,
        requested: Option<&HashSet<Name>>,
    ) -> Result<Value> {
        // Non-local glob imports resolve to an empty mapping.
        if uri.contains("://") {
            return Ok(Value::Object(Arc::default(), None));
        }
        // Match `expand_glob`, which walks `.` for a bare entry path, so the
        // matched paths share this prefix and the keys come out relative.
        let base_dir = module_dir(path.parent().unwrap_or(Path::new(".")));
        let matched = self.glob_io(base_dir, uri)?;
        let matched = matched
            .into_iter()
            .map(|matched_path| {
                let rel_key = pathdiff_or_full(&matched_path, base_dir);
                (matched_path, rel_key)
            })
            .collect::<Vec<_>>();
        let requested = requested.filter(|requested| {
            requested.iter().all(|key| {
                matched
                    .iter()
                    .any(|(_, rel_key)| rel_key.as_str() == &**key)
            })
        });
        let mut mapping = ObjectMap::default();
        for (matched_path, rel_key) in matched {
            if requested.is_some_and(|requested| !requested.contains(rel_key.as_str())) {
                continue;
            }
            if self.same_local_path(&matched_path, path)? {
                continue;
            }
            let val = self.eval_file_with_requested_fields(&matched_path, depth + 1, None)?;
            mapping.insert(rel_key.into(), val);
        }
        Ok(Value::Object(Arc::new(mapping), None))
    }

    /// Evaluate a single `import("uri")` expression, resolving `uri` the same way
    /// an `import "uri"` declaration in `path` would.
    ///
    /// `requested` narrows evaluation to the module properties the expression
    /// actually reads, matching how an import alias used as `Alias.field` is
    /// narrowed. `None` evaluates the whole module.
    pub(super) fn eval_import_expr(
        &mut self,
        uri: &str,
        path: &Path,
        depth: usize,
        requested: Option<HashSet<Name>>,
    ) -> Result<Value> {
        let resolved = resolve_remote_relative(path, uri);
        let uri: &str = resolved.as_deref().unwrap_or(uri);

        if let Some(module_name) = uri.strip_prefix("pkl:") {
            return self.stdlib_module(module_name, depth);
        }

        if !uri.contains("://") || uri.starts_with("file://") {
            let import_path = self.local_file_path(path, uri)?;
            if !self.path_exists_io(&import_path)? {
                return Err(Error::ImportNotFound(import_path.display().to_string()));
            }
            return self.eval_file_with_requested_fields(&import_path, depth + 1, requested);
        }

        let Some((source, name)) = self.load_module_source(uri, path)? else {
            return Err(Error::ImportNotFound(uri.to_string()));
        };
        // Package zips extract to a local file, so evaluate those through the
        // shared import cache like any other local import.
        if !name.contains("://") {
            return self.eval_file_with_requested_fields(Path::new(&name), depth + 1, requested);
        }
        let tokens = lexer::lex_named(&source, &name)?;
        let imported = parser::parse_named(&tokens, &source, &name)?;
        self.eval_module_with_scope(&imported, Path::new(&name), depth + 1, None, requested)
    }

    pub(super) fn same_local_path(&mut self, left: &Path, right: &Path) -> Result<bool> {
        if left == right {
            return Ok(true);
        }
        let left_key = self
            .canonicalize_io(left)
            .unwrap_or_else(|_| left.to_path_buf());
        let right_key = self
            .canonicalize_io(right)
            .unwrap_or_else(|_| right.to_path_buf());
        Ok(left_key == right_key)
    }

    pub(super) fn bind_deferred_inherited_imports(
        &mut self,
        deferred: &[(String, PathBuf)],
        inherited_path: &Path,
        inherited_val: &Value,
        scope: &mut Scope,
    ) -> Result<()> {
        for (alias, alias_path) in deferred {
            if self.same_local_path(alias_path, inherited_path)? {
                scope.declare(alias, inherited_val.clone());
                let identity = self.module_type_namespace(alias_path);
                scope.set_module_identity(alias.clone(), identity);
            }
        }
        Ok(())
    }
}

/// A module's name as pkl reports it: the name from its `module`
/// declaration, else its file name without the extension.
pub(super) fn module_display_name(module: &Module, path: &Path) -> String {
    module.name.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    })
}

#[cfg(test)]
mod requested_field_tests {
    use std::collections::HashSet;

    use super::Evaluator;
    use crate::{Value, lexer, parser};

    #[test]
    fn inherited_late_properties_respect_requested_fields() {
        let test_dir = std::env::temp_dir().join(format!(
            "pklr-requested-fields-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(
            test_dir.join("Base.pkl"),
            "abstract module Base\nwanted = source + 1\nextra = source + 2\nsource = 1\n",
        )
        .unwrap();
        let child_path = test_dir.join("Child.pkl");
        let source = "extends \"Base.pkl\"\nsource = 2\n";
        std::fs::write(&child_path, source).unwrap();
        let tokens = lexer::lex(source).unwrap();
        let module = parser::parse(&tokens).unwrap();

        let value = Evaluator::default()
            .eval_module_with_scope(
                &module,
                &child_path,
                1,
                None,
                Some(HashSet::from_iter([super::nm("wanted")])),
            )
            .unwrap();
        let Value::Object(fields, _) = value else {
            panic!("expected an object");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields["wanted"], Value::Int(3));

        std::fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn mapping_source_captures_import_identities() {
        let test_dir = std::env::temp_dir().join(format!(
            "pklr-mapping-source-imports-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("Config.pkl"), "class Item {}\n").unwrap();
        let main_path = test_dir.join("main.pkl");
        std::fs::write(
            &main_path,
            "import \"Config.pkl\"\nresult = new Mapping { [\"item\"] = new Config.Item {} }\n",
        )
        .unwrap();

        let value = Evaluator::default().eval_file(&main_path).unwrap();
        let Value::Object(fields, _) = value else {
            panic!("expected module object");
        };
        let Value::Object(_, Some(source)) = &fields["result"] else {
            panic!("expected mapping object source");
        };
        assert!(source.scope().contains_key("Config"));
        assert!(source.scope_module_identities().contains_key("Config"));

        std::fs::remove_dir_all(test_dir).unwrap();
    }
}
