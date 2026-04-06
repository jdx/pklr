use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use async_recursion::async_recursion;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{
    self, Annotation, BinOp, Entry, Expr, Modifier, Module, Property, StringInterpPart, UnOp,
};
use crate::value::{ObjectSource, Value};

/// Evaluates pkl source files to [`Value`].
pub struct Evaluator {
    base_path: PathBuf,
    /// Maximum import depth to prevent infinite recursion
    max_depth: usize,
    /// Cache for fetched HTTP sources (URL → source text)
    http_cache: HashMap<String, String>,
    /// Cache for evaluated local imports (canonical path → Value)
    import_cache: HashMap<PathBuf, Value>,
    /// Reusable HTTP client for connection pooling
    http_client: reqwest::Client,
    /// Extracted package zip directories (zip URL → temp dir path)
    package_dirs: HashMap<String, PathBuf>,
    /// HTTP URL rewrite rules (source_prefix → target_prefix).
    /// Longest matching prefix wins.
    http_rewrites: Vec<(String, String)>,
    /// Converters extracted from `output.renderer.converters`.
    /// Each entry maps a class name to a converter lambda.
    converters: Vec<(String, Value)>,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::new(),
            import_cache: HashMap::new(),
            http_client: reqwest::Client::new(),
            package_dirs: HashMap::new(),
            http_rewrites: Vec::new(),
            converters: Vec::new(),
        }
    }
}

impl Evaluator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_base_path(&mut self, path: &Path) {
        self.base_path = path.to_path_buf();
    }

    /// Set a custom HTTP client for fetching remote imports and packages.
    /// Use this to configure proxy settings, CA certificates, timeouts, etc.
    pub fn set_http_client(&mut self, client: reqwest::Client) {
        self.http_client = client;
    }

    /// Add HTTP URL rewrite rules. Each rule is a `"source_prefix=target_prefix"` string
    /// (matching pkl CLI's `--http-rewrite` format). When a URL matches a source prefix,
    /// the prefix is replaced with the target. Longest matching prefix wins.
    pub fn set_http_rewrites(&mut self, rules: &[String]) {
        self.http_rewrites = rules
            .iter()
            .filter_map(|rule| {
                let Some((src, tgt)) = rule.split_once('=') else {
                    eprintln!("pklr: ignoring malformed rewrite rule (missing '='): {rule}");
                    return None;
                };
                if src.is_empty() {
                    eprintln!("pklr: ignoring rewrite rule with empty source prefix: {rule}");
                    return None;
                }
                Some((src.to_string(), tgt.to_string()))
            })
            .collect();
    }

    /// Apply rewrite rules to a URL. Returns the rewritten URL or the original.
    pub fn rewrite_url<'a>(&self, url: &'a str) -> std::borrow::Cow<'a, str> {
        if self.http_rewrites.is_empty() {
            return std::borrow::Cow::Borrowed(url);
        }
        // Find the longest matching prefix
        let best = self
            .http_rewrites
            .iter()
            .filter(|(src, _)| url.starts_with(src.as_str()))
            .max_by_key(|(src, _)| src.len());
        match best {
            Some((src, tgt)) => std::borrow::Cow::Owned(format!("{}{}", tgt, &url[src.len()..])),
            None => std::borrow::Cow::Borrowed(url),
        }
    }

    /// Read a resource by URI scheme.
    async fn read_resource(&mut self, uri: &str) -> Result<Value> {
        if let Some(path) = uri.strip_prefix("file://") {
            // file:// — read local file
            let content = std::fs::read_to_string(path)
                .map_err(|e| Error::Eval(format!("read() failed for {uri}: {e}")))?;
            Ok(Value::String(content))
        } else if let Some(var_name) = uri.strip_prefix("env:") {
            // env: — read environment variable
            match std::env::var(var_name) {
                Ok(val) => Ok(Value::String(val)),
                Err(_) => Err(Error::Eval(format!(
                    "environment variable not found: {var_name}"
                ))),
            }
        } else if let Some(prop_name) = uri.strip_prefix("prop:") {
            // prop: — system properties (not standard in Rust, return empty)
            Err(Error::Eval(format!(
                "system property not available: {prop_name}"
            )))
        } else if uri.starts_with("https://") || uri.starts_with("http://") {
            // HTTP/HTTPS
            let content = self.fetch_source(uri).await?;
            Ok(Value::String(content))
        } else {
            // Bare path — treat as file relative to base_path
            let file_path = self.base_path.join(uri);
            let content = std::fs::read_to_string(&file_path)
                .map_err(|e| Error::Eval(format!("read() failed for {uri}: {e}")))?;
            Ok(Value::String(content))
        }
    }

    async fn fetch_source(&mut self, url: &str) -> Result<String> {
        let rewritten = self.rewrite_url(url);
        let fetch_url = rewritten.as_ref();
        if let Some(cached) = self.http_cache.get(fetch_url) {
            return Ok(cached.clone());
        }
        let err_ctx = if fetch_url != url {
            format!("{url} (rewritten to {fetch_url})")
        } else {
            fetch_url.to_string()
        };
        let body = self
            .http_client
            .get(fetch_url)
            .send()
            .await
            .map_err(|e| Error::Eval(format!("HTTP fetch failed for {err_ctx}: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Eval(format!("HTTP error for {err_ctx}: {e}")))?
            .text()
            .await
            .map_err(|e| Error::Eval(format!("HTTP read failed for {err_ctx}: {e}")))?;
        self.http_cache.insert(fetch_url.to_string(), body.clone());
        Ok(body)
    }

    /// Download a package zip and extract it to a temp directory.
    /// Returns the path to the extracted directory. Caches by zip URL.
    async fn extract_package_zip(&mut self, zip_url: &str) -> Result<PathBuf> {
        let rewritten = self.rewrite_url(zip_url);
        let fetch_url = rewritten.as_ref();
        // Check if already extracted
        if let Some(dir) = self.package_dirs.get(fetch_url) {
            return Ok(dir.clone());
        }
        let err_ctx = if fetch_url != zip_url {
            format!("{zip_url} (rewritten to {fetch_url})")
        } else {
            fetch_url.to_string()
        };
        let bytes = self
            .http_client
            .get(fetch_url)
            .send()
            .await
            .map_err(|e| Error::Eval(format!("HTTP fetch failed for {err_ctx}: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Eval(format!("HTTP error for {err_ctx}: {e}")))?
            .bytes()
            .await
            .map_err(|e| Error::Eval(format!("HTTP read failed for {err_ctx}: {e}")))?;
        let cursor = std::io::Cursor::new(bytes);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| Error::Eval(format!("zip error: {e}")))?;
        let dir = std::env::temp_dir().join(format!("pklr-pkg-{}", self.package_dirs.len()));
        std::fs::create_dir_all(&dir).map_err(|e| Error::Eval(format!("mkdir error: {e}")))?;
        archive
            .extract(&dir)
            .map_err(|e| Error::Eval(format!("zip extract error: {e}")))?;
        self.package_dirs.insert(fetch_url.to_string(), dir.clone());
        Ok(dir)
    }

    pub async fn eval_source(&mut self, source: &str, path: &Path) -> Result<Value> {
        self.converters.clear();
        // Seed import cache for the entry file so circular back-references work
        if let Ok(canonical) = path.canonicalize() {
            self.import_cache
                .insert(canonical, Value::Object(Arc::new(IndexMap::new()), None));
        }
        let name = path.display().to_string();
        let tokens = lexer::lex_named(source, &name)?;
        let module = parser::parse_named(&tokens, source, &name)?;
        let val = self.eval_module(&module, path, 0).await?;
        // Update cache with real value
        if let Ok(canonical) = path.canonicalize() {
            self.import_cache.insert(canonical, val.clone());
        }
        Ok(val)
    }

    /// Evaluate a local pkl file by path (public entry point).
    pub async fn eval_file_pub(&mut self, path: &Path) -> Result<Value> {
        self.eval_file(path, 0).await
    }

    /// Read, lex, parse, and evaluate a local file (with caching).
    /// Inserts a placeholder before evaluation to break circular imports.
    async fn eval_file(&mut self, path: &Path, depth: usize) -> Result<Value> {
        let canonical = path
            .canonicalize()
            .map_err(|e| Error::Io(path.to_path_buf(), e))?;
        if let Some(cached) = self.import_cache.get(&canonical) {
            return Ok(cached.clone());
        }
        // Insert empty placeholder to break circular imports
        self.import_cache.insert(
            canonical.clone(),
            Value::Object(Arc::new(IndexMap::new()), None),
        );
        let result = self.eval_file_inner(path, &canonical, depth).await;
        if result.is_err() {
            // Remove stale placeholder on failure so retries can re-evaluate
            self.import_cache.remove(&canonical);
        }
        result
    }

    async fn eval_file_inner(
        &mut self,
        path: &Path,
        canonical: &Path,
        depth: usize,
    ) -> Result<Value> {
        let source = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
        let name = path.display().to_string();
        let tokens = lexer::lex_named(&source, &name)?;
        let module = parser::parse_named(&tokens, &source, &name)?;
        let val = self.eval_module(&module, path, depth).await?;
        self.import_cache
            .insert(canonical.to_path_buf(), val.clone());
        Ok(val)
    }

    #[async_recursion(?Send)]
    async fn eval_module(&mut self, module: &Module, path: &Path, depth: usize) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval(format!(
                "max import depth {} exceeded",
                self.max_depth
            )));
        }
        let mut scope = Scope::default();
        seed_builtins(&mut scope);

        // Process imports
        for import in &module.imports {
            let uri = &import.uri;

            // Handle glob imports: import* "dir/*.pkl" as Alias
            if import.is_glob {
                let alias = import
                    .alias
                    .clone()
                    .ok_or_else(|| Error::Eval("import* requires an alias".into()))?;

                // Non-local glob imports bind an empty mapping
                if uri.contains("://") {
                    scope.set(alias, Value::Object(Arc::new(IndexMap::new()), None));
                    continue;
                }

                let base_dir = path.parent().unwrap_or(Path::new("."));
                let matched = expand_glob(base_dir, uri)?;
                let mut mapping = IndexMap::new();
                for matched_path in matched {
                    let rel_key = pathdiff_or_full(&matched_path, base_dir);
                    let val = self.eval_file(&matched_path, depth + 1).await?;
                    mapping.insert(rel_key, val);
                }
                scope.set(alias, Value::Object(Arc::new(mapping), None));
                continue;
            }

            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP import
                let source = self.fetch_source(uri).await?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, uri)?;
                    let imp_module = parser::parse_named(&tokens, &source, uri)?;
                    self.eval_module(&imp_module, Path::new(uri), depth + 1)
                        .await?
                };
                let alias = import.alias.clone().unwrap_or_else(|| {
                    uri.rsplit('/')
                        .next()
                        .unwrap_or(uri)
                        .strip_suffix(".pkl")
                        .unwrap_or(uri)
                        .to_string()
                });
                scope.set(alias, imported_val);
                continue;
            }

            if uri.starts_with("package://") {
                let pkg = resolve_package_uri(uri)?;
                let fragment = uri.split_once('#').map(|(_, f)| f).unwrap_or("");
                let file_path = fragment.strip_prefix('/').unwrap_or(fragment);
                // For zip packages, extract to temp dir and eval as local file
                if let PackageSource::Zip(zip_url, _) = &pkg {
                    let pkg_dir = self.extract_package_zip(zip_url).await?;
                    let local_path = pkg_dir.join(file_path);
                    let imported_val = self.eval_file(&local_path, depth + 1).await?;
                    let alias = import.alias.clone().unwrap_or_else(|| {
                        file_path
                            .rsplit('/')
                            .next()
                            .unwrap_or(file_path)
                            .strip_suffix(".pkl")
                            .unwrap_or(file_path)
                            .to_string()
                    });
                    scope.set(alias, imported_val);
                    continue;
                }
                let url = match &pkg {
                    PackageSource::Direct(url) => url.clone(),
                    PackageSource::Zip(..) => unreachable!(),
                };
                let source = self.fetch_source(&url).await?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, &url)?;
                    let imp_module = parser::parse_named(&tokens, &source, &url)?;
                    self.eval_module(&imp_module, Path::new(&url), depth + 1)
                        .await?
                };
                let alias = import.alias.clone().unwrap_or_else(|| {
                    file_path
                        .rsplit('/')
                        .next()
                        .unwrap_or(file_path)
                        .strip_suffix(".pkl")
                        .unwrap_or(file_path)
                        .to_string()
                });
                scope.set(alias, imported_val);
                continue;
            }

            // Handle pkl: standard library imports
            if let Some(module_name) = uri.strip_prefix("pkl:") {
                let stdlib_val = stdlib_module(module_name);
                let alias = import
                    .alias
                    .clone()
                    .unwrap_or_else(|| module_name.to_string());
                scope.set(alias, stdlib_val);
                continue;
            }

            // Skip other non-local imports
            if uri.contains("://") && !uri.starts_with("file://") {
                continue;
            }

            let import_path = if let Some(rel) = uri.strip_prefix("file://") {
                PathBuf::from(rel)
            } else {
                let base = path.parent().unwrap_or(Path::new("."));
                base.join(uri)
            };
            if !import_path.exists() {
                return Err(Error::ImportNotFound(import_path.display().to_string()));
            }
            {
                let imported_val = self.eval_file(&import_path, depth + 1).await?;
                // Determine the binding name: alias or filename stem
                let alias = import.alias.clone().unwrap_or_else(|| {
                    import_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                });
                scope.set(alias, imported_val);
            }
        }

        // Process amends: load base module as starting values
        let mut base_obj = IndexMap::new();
        if let Some(uri) = &module.amends {
            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP amends
                let source = self.fetch_source(uri).await?;
                let tokens = lexer::lex_named(&source, uri)?;
                let base_module = parser::parse_named(&tokens, &source, uri)?;
                let base_val = self
                    .eval_module(&base_module, Path::new(uri), depth + 1)
                    .await?;
                if let Value::Object(m, _) = base_val {
                    base_obj = (*m).clone();
                }
            } else if uri.starts_with("package://") {
                let pkg = resolve_package_uri(uri)?;
                if let PackageSource::Zip(zip_url, entry) = &pkg {
                    let pkg_dir = self.extract_package_zip(zip_url).await?;
                    let local_path = pkg_dir.join(entry);
                    let base_val = self.eval_file(&local_path, depth + 1).await?;
                    if let Value::Object(m, _) = base_val {
                        base_obj = (*m).clone();
                    }
                } else if let PackageSource::Direct(url) = &pkg {
                    let source = self.fetch_source(url).await?;
                    let tokens = lexer::lex_named(&source, url)?;
                    let base_module = parser::parse_named(&tokens, &source, url)?;
                    let base_val = self
                        .eval_module(&base_module, Path::new(url.as_str()), depth + 1)
                        .await?;
                    if let Value::Object(m, _) = base_val {
                        base_obj = (*m).clone();
                    }
                }
            } else if !uri.starts_with("pkl:")
                && (!uri.contains("://") || uri.starts_with("file://"))
            {
                let amends_path = if let Some(rel) = uri.strip_prefix("file://") {
                    PathBuf::from(rel)
                } else {
                    let base = path.parent().unwrap_or(Path::new("."));
                    base.join(uri)
                };
                if amends_path.exists() {
                    let base_val = self.eval_file(&amends_path, depth + 1).await?;
                    if let Value::Object(m, _) = base_val {
                        base_obj = (*m).clone();
                    }
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
        if let Some(uri) = &module.amends {
            let base_source = if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP source was already fetched and cached
                self.http_cache.get(uri).cloned()
            } else if uri.starts_with("package://") {
                // For package:// URIs with Direct source, the source was cached
                if let Ok(pkg) = resolve_package_uri(uri) {
                    match &pkg {
                        PackageSource::Direct(url) => self.http_cache.get(url).cloned(),
                        PackageSource::Zip(zip_url, entry) => {
                            // For zip packages, read from the extracted directory
                            self.package_dirs
                                .get(zip_url.as_str())
                                .and_then(|dir| std::fs::read_to_string(dir.join(entry)).ok())
                        }
                    }
                } else {
                    None
                }
            } else if !uri.starts_with("pkl:")
                && (!uri.contains("://") || uri.starts_with("file://"))
            {
                let amends_path = if let Some(rel) = uri.strip_prefix("file://") {
                    PathBuf::from(rel)
                } else {
                    let base = path.parent().unwrap_or(Path::new("."));
                    base.join(uri)
                };
                std::fs::read_to_string(&amends_path).ok()
            } else {
                None
            };
            if let Some(src) = base_source
                && let Ok(tokens) = lexer::lex(&src)
                && let Ok(base_module) = parser::parse(&tokens)
            {
                for entry in &base_module.body {
                    if let Entry::ClassDef(name, class_mods, parent, body) = entry {
                        let defaults = self
                            .eval_class_def(
                                name,
                                class_mods,
                                parent.as_deref(),
                                body,
                                &scope,
                                depth,
                            )
                            .await?;
                        scope.set(name.clone(), defaults);
                        // Remove inherited class definitions from base output —
                        // they were included at depth > 0 for dotted access but
                        // should not appear in the amending module's data output.
                        base_obj.shift_remove(name);
                    }
                    // Extract converters from the base module's output block
                    // (the amending module inherits them; child overrides if present).
                    if let Entry::Property(prop) = entry
                        && prop.name == "output"
                        && depth == 0
                    {
                        self.extract_converters_from_ast(prop, &scope, depth).await;
                    }
                }
            }
            // Remove function values from base output (not data)
            base_obj.retain(|_, v| !matches!(v, Value::Lambda(..)));
        }

        // Process extends: load base module, inherit all members and scope
        if let Some(uri) = &module.extends {
            if !uri.contains("://") || uri.starts_with("file://") {
                let extends_path = if let Some(rel) = uri.strip_prefix("file://") {
                    PathBuf::from(rel)
                } else {
                    let base = path.parent().unwrap_or(Path::new("."));
                    base.join(uri)
                };
                if extends_path.exists() {
                    let source = std::fs::read_to_string(&extends_path)
                        .map_err(|e| Error::Io(extends_path.clone(), e))?;
                    let name = extends_path.display().to_string();
                    let tokens = lexer::lex_named(&source, &name)?;
                    let ext_module = parser::parse_named(&tokens, &source, &name)?;
                    // Evaluate the base module to get its properties
                    let ext_val = self
                        .eval_module(&ext_module, &extends_path, depth + 1)
                        .await?;
                    if let Value::Object(m, _) = ext_val {
                        base_obj = (*m).clone();
                    }
                    // Also evaluate the base module's scope (classes, locals) into our scope
                    // by re-processing its body entries
                    for entry in &ext_module.body {
                        match entry {
                            Entry::ClassDef(cls_name, cls_mods, parent, body) => {
                                let defaults = self
                                    .eval_class_def(
                                        cls_name,
                                        cls_mods,
                                        parent.as_deref(),
                                        body,
                                        &scope,
                                        depth,
                                    )
                                    .await?;
                                scope.set(cls_name.clone(), defaults);
                                base_obj.shift_remove(cls_name);
                            }
                            Entry::TypeAlias(name, ty) => {
                                self.eval_type_alias(name, ty, &mut scope);
                            }
                            Entry::Property(prop) if prop.name == "output" && depth == 0 => {
                                self.extract_converters_from_ast(prop, &scope, depth).await;
                            }
                            _ => {}
                        }
                    }
                }
            } else if uri.starts_with("https://") || uri.starts_with("http://") {
                let source = self.fetch_source(uri).await?;
                let tokens = lexer::lex_named(&source, uri)?;
                let ext_module = parser::parse_named(&tokens, &source, uri)?;
                let ext_val = self
                    .eval_module(&ext_module, Path::new(uri), depth + 1)
                    .await?;
                if let Value::Object(m, _) = ext_val {
                    base_obj = (*m).clone();
                }
                // Inject class definitions from HTTP base into scope
                for entry in &ext_module.body {
                    if let Entry::ClassDef(cls_name, cls_mods, parent, body) = entry {
                        let defaults = self
                            .eval_class_def(
                                cls_name,
                                cls_mods,
                                parent.as_deref(),
                                body,
                                &scope,
                                depth,
                            )
                            .await?;
                        scope.set(cls_name.clone(), defaults);
                        base_obj.shift_remove(cls_name);
                    }
                }
            }
        }

        // First pass: collect locals, class definitions, and type aliases in
        // declaration order so they can reference each other
        for entry in &module.body {
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    let val = self
                        .eval_expr(prop.value.as_ref().unwrap(), &scope, depth)
                        .await?;
                    scope.set(prop.name.clone(), val);
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self
                        .eval_class_def(name, class_mods, parent.as_deref(), body, &scope, depth)
                        .await?;
                    scope.set(name.clone(), defaults);
                }
                Entry::TypeAlias(name, ty) => {
                    self.eval_type_alias(name, ty, &mut scope);
                }
                _ => {}
            }
        }

        // Export class definitions so they're accessible via dotted paths
        // (e.g., `import "helpers.pkl"` → `helpers.ClassName`).
        // Track class names to exclude from serialized output.
        let mut class_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in &module.body {
            if let Entry::ClassDef(name, ..) = entry
                && let Some(cls_val) = scope.get(name)
            {
                base_obj.insert(name.clone(), cls_val.clone());
                class_names.insert(name.clone());
            }
        }

        // Second pass: evaluate non-local entries into output object
        let mut out = base_obj;
        // all_props includes hidden properties — used for `this`/`module` snapshots
        let mut all_props = out.clone();
        // Seed scope with base properties so body amendments can find them
        // (e.g., `hooks { ... }` needs to find the base hooks Mapping in scope
        // to properly amend it with type-aware merging).
        for (k, v) in &out {
            scope.set(k.clone(), v.clone());
        }
        // Bind `this` at module level so properties can reference the module object
        scope.set(
            "this".into(),
            Value::Object(Arc::new(all_props.clone()), None),
        );
        // Also bind `module` to the same value
        scope.set(
            "module".into(),
            Value::Object(Arc::new(all_props.clone()), None),
        );
        for entry in &module.body {
            if let Entry::Property(prop) = entry {
                let mods = &prop.modifiers;
                if has_modifier(mods, Modifier::Local) {
                    continue; // already collected
                }
                check_deprecated(&prop.annotations, &prop.name);
                // Extract renderer converters from the `output` block AST,
                // then skip it (it's not included in the output).
                // Clear any base-inherited converters so child overrides take precedence.
                if prop.name == "output" {
                    if depth == 0 {
                        self.converters.clear();
                        self.extract_converters_from_ast(prop, &scope, depth).await;
                    }
                    continue;
                }
                // abstract/external properties must have a value (or be overridden)
                if (has_modifier(mods, Modifier::Abstract)
                    || has_modifier(mods, Modifier::External))
                    && prop.value.is_none()
                    && prop.body.is_none()
                {
                    if let Some(v) = out.get(&prop.name) {
                        // Satisfied by base — add to scope so other properties can reference it
                        scope.set(prop.name.clone(), v.clone());
                    } else {
                        let kind = if has_modifier(mods, Modifier::Abstract) {
                            "abstract"
                        } else {
                            "external"
                        };
                        return Err(Error::Eval(format!(
                            "{kind} property '{}' must be assigned a value",
                            prop.name
                        )));
                    }
                    continue;
                }
                let val = self.eval_property(prop, &scope, depth).await?;
                if let Some(v) = val {
                    // const/fixed: error if overriding an immutable property from base
                    if (has_modifier(mods, Modifier::Const) || has_modifier(mods, Modifier::Fixed))
                        && out.contains_key(&prop.name)
                    {
                        let kind = if has_modifier(mods, Modifier::Const) {
                            "const"
                        } else {
                            "fixed"
                        };
                        return Err(Error::Eval(format!(
                            "cannot override {kind} property '{}'",
                            prop.name
                        )));
                    }
                    // Always add to scope so other properties can reference it
                    scope.set(prop.name.clone(), v.clone());
                    // Track in all_props (including hidden) for `this`/`module`
                    all_props.insert(prop.name.clone(), v.clone());
                    if !has_modifier(mods, Modifier::Hidden) {
                        out.insert(prop.name.clone(), v);
                    }
                    // Update `this` and `module` with all properties (including hidden)
                    let snapshot = Value::Object(Arc::new(all_props.clone()), None);
                    scope.set("this".into(), snapshot.clone());
                    scope.set("module".into(), snapshot);
                }
            }
        }

        // At the top level (depth 0), strip class definitions and lambdas from
        // the serialized output — they're schema/functions, not data.
        // Imported modules (depth > 0) keep them so dotted access works
        // (e.g., `helpers.ClassName`).
        if depth == 0 {
            for name in &class_names {
                out.shift_remove(name);
            }
            out.retain(|_, v| !matches!(v, Value::Lambda(..)));
        }
        Ok(Value::Object(Arc::new(out), None))
    }

    #[async_recursion(?Send)]
    async fn eval_property(
        &mut self,
        prop: &Property,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Some(expr) = &prop.value {
            return Ok(Some(self.eval_expr(expr, scope, depth).await?));
        }
        if let Some(body) = &prop.body {
            // `foo { ... }` — object body amendment.
            // If the property already has a value in scope (e.g., from a base class),
            // amend that value so its ObjectSource (type info, default template) is preserved.
            if let Some(Value::Object(_, Some(src))) = scope.get(&prop.name) {
                let base_entries = src.entries.clone();
                let base_scope = src.scope.clone();
                return Ok(Some(
                    self.eval_amended_object(&base_entries, &base_scope, body, scope, depth)
                        .await?,
                ));
            }
            let val = self.eval_entries(body, scope, depth).await?;
            return Ok(Some(val));
        }
        Ok(None) // bare type-only declaration
    }

    #[async_recursion(?Send)]
    async fn eval_entries(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let mut child_scope = scope.child();
        // Set `outer` to a snapshot of the parent scope's variables as an object.
        // Also insert Null for any nullable-no-default properties declared in these
        // entries but absent from the parent scope, so that `outer.optionalProp`
        // resolves to Null rather than failing with "field not found".
        let mut outer_map = scope.flatten();
        for entry in entries {
            if let Entry::Property(prop) = entry
                && prop.value.is_none()
                && prop.body.is_none()
                && !has_modifier(&prop.modifiers, Modifier::Local)
                && !outer_map.contains_key(&prop.name)
                && matches!(prop.type_ann, Some(crate::parser::TypeExpr::Nullable(_)))
            {
                outer_map.insert(prop.name.clone(), Value::Null);
            }
        }
        let outer_obj = Value::Object(Arc::new(outer_map), None);
        child_scope.set("outer".into(), outer_obj);
        // First pass: collect locals, class definitions, and type aliases in
        // declaration order so they can reference each other correctly.
        // Non-lambda locals are evaluated eagerly; lambda locals are deferred
        // to a second pass so they capture the fully-populated scope.
        let mut deferred_lambdas: Vec<(String, &crate::parser::Expr)> = Vec::new();
        for entry in entries {
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    let expr = prop.value.as_ref().unwrap();
                    if matches!(expr, crate::parser::Expr::Lambda(..)) {
                        // Defer lambda locals so they capture the final scope
                        deferred_lambdas.push((prop.name.clone(), expr));
                    } else {
                        let val = self.eval_expr(expr, &child_scope, depth).await?;
                        child_scope.set(prop.name.clone(), val);
                    }
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self
                        .eval_class_def(
                            name,
                            class_mods,
                            parent.as_deref(),
                            body,
                            &child_scope,
                            depth,
                        )
                        .await?;
                    child_scope.set(name.clone(), defaults);
                }
                Entry::TypeAlias(name, ty) => {
                    self.eval_type_alias(name, ty, &mut child_scope);
                }
                _ => {}
            }
        }

        let default_template = self
            .find_default_template(entries, &child_scope, depth)
            .await?;

        let mut map: IndexMap<String, Value> = IndexMap::new();
        // all_props includes hidden properties — used for `this` snapshot
        let mut all_props: IndexMap<String, Value> = IndexMap::new();
        // Bind `this` so properties can reference the object being built
        child_scope.set(
            "this".into(),
            Value::Object(Arc::new(all_props.clone()), None),
        );
        for entry in entries {
            match entry {
                Entry::Property(prop) => {
                    let mods = &prop.modifiers;
                    if has_modifier(mods, Modifier::Local) {
                        continue;
                    }
                    check_deprecated(&prop.annotations, &prop.name);
                    // Skip the `default` property — it's a template, not an output entry
                    if prop.name == "default" && default_template.is_some() {
                        continue;
                    }
                    if has_modifier(mods, Modifier::Abstract)
                        && prop.value.is_none()
                        && prop.body.is_none()
                    {
                        continue; // abstract without value — skip (must be overridden)
                    }
                    if let Some(v) = self.eval_property(prop, &child_scope, depth).await? {
                        child_scope.set(prop.name.clone(), v.clone());
                        all_props.insert(prop.name.clone(), v.clone());
                        if !has_modifier(mods, Modifier::Hidden) {
                            map.insert(prop.name.clone(), v);
                        }
                        // Update `this` with all properties (including hidden)
                        child_scope.set(
                            "this".into(),
                            Value::Object(Arc::new(all_props.clone()), None),
                        );
                    }
                }
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, &child_scope, depth).await?;
                    let val = if let Some(Value::Object(_, Some(src))) = &default_template
                        && let Expr::ObjectBody(body) = val_expr
                    {
                        // Default template has ObjectSource — use eval_amended_object
                        // so nested property amendments work properly.
                        let mut result = self
                            .eval_amended_object(
                                &src.entries,
                                &src.scope,
                                body,
                                &child_scope,
                                depth,
                            )
                            .await?;
                        // Propagate the template's type_name so converters can match.
                        if let Some(ref tn) = src.type_name
                            && let Value::Object(_, ref mut result_src) = result
                        {
                            let new_src = match result_src.as_ref() {
                                Some(s) => {
                                    let mut ns = (**s).clone();
                                    ns.type_name = Some(tn.clone());
                                    ns
                                }
                                None => ObjectSource {
                                    entries: vec![],
                                    scope: IndexMap::new(),
                                    is_open: true,
                                    type_name: Some(tn.clone()),
                                },
                            };
                            *result_src = Some(std::sync::Arc::new(new_src));
                        }
                        result
                    } else {
                        let mut val = self.eval_expr(val_expr, &child_scope, depth).await?;
                        if let Some(ref tpl) = default_template {
                            val = merge_values(tpl.clone(), val);
                        }
                        val
                    };
                    let key_str = value_to_key(&key)?;
                    map.insert(key_str, val);
                }
                Entry::Spread(expr) => {
                    let val = self.eval_expr(expr, &child_scope, depth).await?;
                    if let Value::Object(m, _) = val {
                        map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self
                        .eval_expr(&fgen.collection, &child_scope, depth)
                        .await?;
                    let items = collection_to_items(collection);
                    for (k, v) in items {
                        let mut iter_scope = child_scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(key_var) = &fgen.key_var {
                            iter_scope.set(key_var.clone(), k);
                        }
                        let body_val = self.eval_entries(&fgen.body, &iter_scope, depth).await?;
                        if let Value::Object(m, _) = body_val {
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                    }
                }
                Entry::WhenGenerator(wgen) => {
                    let cond = self.eval_expr(&wgen.condition, &child_scope, depth).await?;
                    if is_truthy(&cond) {
                        let body_val = self.eval_entries(&wgen.body, &child_scope, depth).await?;
                        if let Value::Object(m, _) = body_val {
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                    } else if let Some(else_body) = &wgen.else_body {
                        let else_val = self.eval_entries(else_body, &child_scope, depth).await?;
                        if let Value::Object(m, _) = else_val {
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                    }
                }
                Entry::Elem(_) => {} // bare elements only valid in Listing bodies
                Entry::ClassDef(..) | Entry::TypeAlias(..) => {} // handled in scope setup
            }
        }
        // Evaluate deferred local lambdas (function definitions) AFTER all
        // properties so they capture overridden values (late binding).
        for (name, expr) in deferred_lambdas {
            let val = self.eval_expr(expr, &child_scope, depth).await?;
            child_scope.set(name, val);
        }
        let source = ObjectSource {
            entries: entries.to_vec(),
            scope: child_scope.flatten(),
            is_open: true, // default: allow new properties
            type_name: None,
        };
        Ok(Value::Object(Arc::new(map), Some(Arc::new(source))))
    }

    /// Evaluate a class definition, optionally inheriting from a parent class.
    ///
    /// If `parent_name` is provided, the parent class is looked up in scope,
    /// its defaults are used as a base, and `super` is bound to the parent
    /// value so the child class body can reference it.
    #[async_recursion(?Send)]
    async fn eval_class_def(
        &mut self,
        class_name: &str,
        class_mods: &[Modifier],
        parent_name: Option<&str>,
        body: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let parent_val = parent_name.and_then(|name| resolve_dotted(scope, name));

        let mut child_scope = scope.child();
        if let Some(ref pv) = parent_val {
            child_scope.set("super".into(), pv.clone());
        }

        let child_defaults = self.eval_entries(body, &child_scope, depth + 1).await?;

        if let Some(Value::Object(parent_map, parent_src)) = parent_val {
            // Merge: parent defaults first, child overrides on top
            let mut merged: IndexMap<String, Value> = (*parent_map).clone();
            if let Value::Object(child_map, child_src) = child_defaults {
                for (k, v) in child_map.iter() {
                    merged.insert(k.clone(), v.clone());
                }
                // Preserve the child's ObjectSource for late binding,
                // but prepend parent entries so inherited props are available
                let source = if let Some(child_arc) = child_src {
                    let mut src = (*child_arc).clone();
                    // Collect child property names (including dynamic string-key entries)
                    let child_names: std::collections::HashSet<String> = body
                        .iter()
                        .filter_map(|e| match e {
                            Entry::Property(p) => Some(p.name.clone()),
                            Entry::DynProperty(Expr::String(s), _) => Some(s.clone()),
                            _ => None,
                        })
                        .collect();
                    if let Some(psrc) = parent_src {
                        let mut combined_entries = Vec::new();
                        for pe in &psrc.entries {
                            if let Entry::Property(p) = pe
                                && !child_names.contains(&p.name)
                            {
                                combined_entries.push(pe.clone());
                            }
                        }
                        combined_entries.extend(src.entries);
                        src.entries = combined_entries;
                    }
                    Some(Arc::new(src))
                } else {
                    None
                };
                Ok(Value::Object(Arc::new(merged), source))
            } else {
                Ok(Value::Object(Arc::new(merged), None))
            }
        } else {
            Ok(child_defaults)
        }
        .map(|val| {
            // Set is_open flag and class_name on the result's ObjectSource
            let is_open = has_modifier(class_mods, Modifier::Open);
            if let Value::Object(map, Some(src)) = val {
                let mut new_src = (*src).clone();
                new_src.is_open = is_open;
                new_src.type_name = Some(class_name.to_string());
                Value::Object(map, Some(Arc::new(new_src)))
            } else {
                val
            }
        })
    }

    /// Evaluate a type alias declaration.
    ///
    /// If the aliased type is a named type that exists in scope (e.g. a class),
    /// the alias name is bound to the same value so `new AliasName { ... }` works.
    fn eval_type_alias(&self, name: &str, ty: &crate::parser::TypeExpr, scope: &mut Scope) {
        // Store the TypeExpr so `is`/`as` can resolve alias names to their definitions
        scope.set_type_alias(name.to_string(), ty.clone());
        match ty {
            crate::parser::TypeExpr::Named(target) => {
                // Alias to a class or another alias already in scope
                if let Some(val) = scope.get(target) {
                    scope.set(name.to_string(), val.clone());
                }
            }
            crate::parser::TypeExpr::Nullable(inner) => {
                // typealias Foo = Bar? -- alias to the inner type
                if let crate::parser::TypeExpr::Named(target) = inner.as_ref()
                    && let Some(val) = scope.get(target)
                {
                    scope.set(name.to_string(), val.clone());
                }
            }
            crate::parser::TypeExpr::Constrained(base, _) => {
                // Alias to a constrained type -- also bind as the base class if available
                if let Some(val) = scope.get(base) {
                    scope.set(name.to_string(), val.clone());
                }
            }
            // Union types, generics, etc. -- no runtime representation needed
            _ => {}
        }
    }

    /// Check if a value matches a type expression, including constraint evaluation.
    #[async_recursion(?Send)]
    async fn eval_type_check(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<bool> {
        use crate::parser::TypeExpr;
        match ty {
            TypeExpr::Named(name) => {
                // Check if name is a type alias; if so, resolve to the aliased type
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_type_check(val, &resolved, scope, depth + 1).await;
                }
                // Otherwise, plain type check
                Ok(value_is_type(val, ty))
            }
            TypeExpr::Constrained(base_name, constraint) => {
                // First check the base type
                if !value_is_type(val, &TypeExpr::Named(base_name.clone())) {
                    return Ok(false);
                }
                // Evaluate the constraint with `this` bound to the value
                let mut constraint_scope = scope.child();
                constraint_scope.set("this".into(), val.clone());
                // Also bind common properties directly so `length`, `isEmpty` etc. work
                match val {
                    Value::String(s) => {
                        constraint_scope.set("length".into(), Value::Int(s.chars().count() as i64));
                        constraint_scope.set("isEmpty".into(), Value::Bool(s.is_empty()));
                    }
                    Value::Int(_) | Value::Float(_) => {}
                    Value::List(items) => {
                        constraint_scope.set("length".into(), Value::Int(items.len() as i64));
                        constraint_scope.set("isEmpty".into(), Value::Bool(items.is_empty()));
                    }
                    _ => {}
                }
                let result = self
                    .eval_expr(constraint, &constraint_scope, depth + 1)
                    .await?;
                Ok(is_truthy(&result))
            }
            TypeExpr::Nullable(inner) => {
                if matches!(val, Value::Null) {
                    return Ok(true);
                }
                self.eval_type_check(val, inner, scope, depth).await
            }
            TypeExpr::Union(variants) => {
                for v in variants {
                    if self.eval_type_check(val, v, scope, depth).await? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            // Non-constrained types: delegate to the simple check
            _ => Ok(value_is_type(val, ty)),
        }
    }

    /// Evaluate an amended object with late binding.
    ///
    /// Merges the base object's original entries with the overlay entries,
    /// then re-evaluates everything so that dependent properties pick up
    /// overridden values.
    #[async_recursion(?Send)]
    async fn eval_amended_object(
        &mut self,
        base_entries: &[Entry],
        base_scope: &IndexMap<String, Value>,
        overlay_entries: &[Entry],
        current_scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Build merged entry list preserving base order.
        // Overridden properties are replaced in-place so that later
        // properties that reference them see the new value.
        let mut merged: Vec<Entry> = Vec::new();
        let mut overlay_by_name: IndexMap<String, &Entry> = IndexMap::new();
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry {
                overlay_by_name.insert(prop.name.clone(), entry);
            }
        }

        // Walk base entries: substitute overridden properties in-place.
        // If the overlay has a body amendment (no `=`), keep the base entry first
        // so its value is in scope, then add the overlay body entry after.
        let mut used_overlay: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in base_entries {
            if let Entry::Property(prop) = entry
                && let Some(replacement) = overlay_by_name.get(&prop.name)
            {
                if let Entry::Property(overlay_prop) = replacement
                    && overlay_prop.body.is_some()
                    && overlay_prop.value.is_none()
                    && (prop.value.is_some() || prop.body.is_some())
                {
                    // Body amendment: keep base entry (for its value) AND overlay
                    // entry (for body amendment). eval_property will see the base
                    // value in scope and amend it.
                    merged.push(entry.clone());
                }
                merged.push((*replacement).clone());
                used_overlay.insert(prop.name.clone());
                continue;
            }
            merged.push(entry.clone());
        }

        // Append overlay entries that are genuinely new (not replacing a base entry)
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry
                && used_overlay.contains(&prop.name)
            {
                continue; // already placed in-order above
            }
            merged.push(entry.clone());
        }

        // Build scope: start with the base's captured scope, then layer current scope
        let mut eval_scope = Scope::default();
        for (k, v) in base_scope {
            eval_scope.set(k.clone(), v.clone());
        }
        // Layer in current scope values (imports, module-level locals, etc.)
        for (k, v) in current_scope.flatten() {
            eval_scope.set(k, v);
        }
        // Propagate type aliases so `is`/`as` constraints work inside amended objects
        for (k, ty) in current_scope.flatten_type_aliases() {
            eval_scope.set_type_alias(k, ty);
        }
        // Seed Null for nullable-no-default base properties absent from eval_scope.
        // This ensures `outer.optProp` resolves to Null rather than "field not found"
        // when the property was never assigned a value in the base class or any overlay.
        for entry in base_entries {
            if let Entry::Property(prop) = entry
                && prop.value.is_none()
                && prop.body.is_none()
                && !has_modifier(&prop.modifiers, Modifier::Local)
                && eval_scope.get(&prop.name).is_none()
                && matches!(prop.type_ann, Some(crate::parser::TypeExpr::Nullable(_)))
            {
                eval_scope.set(prop.name.clone(), Value::Null);
            }
        }

        // Evaluate the merged entries (eval_entries handles locals, classes,
        // and evaluates properties in order with each added to scope)
        self.eval_entries(&merged, &eval_scope, depth + 1).await
    }

    #[async_recursion(?Send)]
    async fn eval_expr(&mut self, expr: &Expr, scope: &Scope, depth: usize) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        match expr {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::String(s) => Ok(Value::String(s.clone())),
            Expr::StringInterpolation(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        StringInterpPart::Literal(s) => result.push_str(s),
                        StringInterpPart::Expr(e) => {
                            let val = self.eval_expr(e, scope, depth + 1).await?;
                            result.push_str(&value_to_display(&val));
                        }
                    }
                }
                Ok(Value::String(result))
            }
            Expr::Ident(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| Error::Eval(format!("undefined variable: {name}"))),
            Expr::Lambda(params, body) => {
                // Capture current scope values (Arc-wrapped for O(1) clone)
                let captured = Arc::new(scope.flatten());
                Ok(Value::Lambda(params.clone(), (**body).clone(), captured))
            }
            Expr::New(type_name, entries, generic_params) => {
                match type_name.as_deref() {
                    Some("Listing") => {
                        let mut items = Vec::new();
                        for entry in entries {
                            match entry {
                                Entry::Elem(e) => {
                                    items.push(self.eval_expr(e, scope, depth + 1).await?);
                                }
                                Entry::Property(p) if p.value.is_some() => {
                                    items.push(
                                        self.eval_expr(p.value.as_ref().unwrap(), scope, depth + 1)
                                            .await?,
                                    );
                                }
                                Entry::Spread(e) => {
                                    let v = self.eval_expr(e, scope, depth + 1).await?;
                                    match v {
                                        Value::List(l) => items.extend(l),
                                        Value::Object(m, _) => items.extend(m.values().cloned()),
                                        other => items.push(other),
                                    }
                                }
                                Entry::ForGenerator(fgen) => {
                                    let collection =
                                        self.eval_expr(&fgen.collection, scope, depth + 1).await?;
                                    for (k, v) in collection_to_items(collection) {
                                        let mut iter_scope = scope.child();
                                        iter_scope.set(fgen.val_var.clone(), v);
                                        if let Some(key_var) = &fgen.key_var {
                                            iter_scope.set(key_var.clone(), k);
                                        }
                                        for sub in &fgen.body {
                                            if let Entry::Elem(e) = sub {
                                                items.push(
                                                    self.eval_expr(e, &iter_scope, depth + 1)
                                                        .await?,
                                                );
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(Value::List(items))
                    }
                    Some("Mapping") | Some("Map") => {
                        // If the Mapping has a value type param (e.g., Mapping<String, Step>),
                        // resolve it as a default template so entries inherit the class type.
                        let value_type_default = generic_params
                            .get(1)
                            .and_then(|name| resolve_dotted(scope, name));
                        let mut map = IndexMap::new();
                        self.eval_mapping_entries_with_type_default(
                            entries,
                            scope,
                            depth,
                            &mut map,
                            value_type_default.as_ref(),
                        )
                        .await?;
                        // Build ObjectSource with a synthetic `default` entry so that
                        // body amendments (`steps { ["x"] { ... } }`) merge new entries
                        // with the value type class, preserving type_name for converters.
                        let mut src_entries = entries.to_vec();
                        if value_type_default.is_some()
                            && !entries
                                .iter()
                                .any(|e| matches!(e, Entry::Property(p) if p.name == "default"))
                        {
                            // Inject a synthetic default property referencing the value type
                            let vt_name = generic_params[1].clone();
                            src_entries.push(Entry::Property(Property {
                                annotations: vec![],
                                modifiers: vec![],
                                name: "default".into(),
                                type_ann: None,
                                value: Some(Expr::New(Some(vt_name), vec![], vec![])),
                                body: None,
                            }));
                        }
                        let mut source_scope = scope.flatten();
                        source_scope.shift_remove("outer");
                        source_scope.shift_remove("this");
                        let source = ObjectSource {
                            entries: src_entries,
                            scope: source_scope,
                            is_open: true,
                            type_name: None,
                        };
                        Ok(Value::Object(Arc::new(map), Some(Arc::new(source))))
                    }
                    Some("Dynamic") => self.eval_entries(entries, scope, depth + 1).await,
                    _ => {
                        // Check if type name matches a class in scope (supports dotted names)
                        let base = type_name.as_ref().and_then(|name| {
                            let parts: Vec<&str> = name.split('.').collect();
                            let mut val = scope.get(parts[0])?.clone();
                            for part in &parts[1..] {
                                val = match val {
                                    Value::Object(ref map, _) => map.get(*part)?.clone(),
                                    _ => return None,
                                };
                            }
                            Some(val)
                        });
                        if let Some(Value::Object(ref base_map, Some(ref base_src))) = base {
                            // Enforce open modifier: non-open classes reject new properties
                            if !base_src.is_open {
                                // Collect all declared property names from the base class
                                // (includes those with no default value)
                                let base_names: std::collections::HashSet<String> = base_src
                                    .entries
                                    .iter()
                                    .filter_map(|e| {
                                        if let Entry::Property(p) = e {
                                            Some(p.name.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .chain(base_map.keys().cloned())
                                    .collect();
                                for entry in entries {
                                    match entry {
                                        Entry::Property(p)
                                            if !has_modifier(&p.modifiers, Modifier::Local)
                                                && !base_names.contains(&p.name) =>
                                        {
                                            return Err(Error::Eval(format!(
                                                "cannot add property '{}' to non-open class",
                                                p.name
                                            )));
                                        }
                                        Entry::DynProperty(Expr::String(key), _)
                                            if !base_names.contains(key) =>
                                        {
                                            return Err(Error::Eval(format!(
                                                "cannot add property '{}' to non-open class",
                                                key
                                            )));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            let is_open = base_src.is_open;
                            // Late binding: re-evaluate merged base + overlay entries
                            let mut result = self
                                .eval_amended_object(
                                    &base_src.entries.clone(),
                                    &base_src.scope.clone(),
                                    entries,
                                    scope,
                                    depth,
                                )
                                .await?;
                            // Preserve the base class's is_open flag and tag the
                            // type_name so output.renderer.converters can match it.
                            if let Value::Object(_, ref mut src_slot) = result {
                                let tn = type_name.clone();
                                let new_src = if let Some(src) = src_slot.as_ref() {
                                    let mut s = (**src).clone();
                                    if s.is_open != is_open {
                                        s.is_open = is_open;
                                    }
                                    s.type_name = tn;
                                    s
                                } else {
                                    ObjectSource {
                                        entries: Vec::new(),
                                        scope: IndexMap::new(),
                                        is_open,
                                        type_name: tn,
                                    }
                                };
                                *src_slot = Some(Arc::new(new_src));
                            }
                            Ok(result)
                        } else if let Some(Value::Object(base_map, _)) = base {
                            // Fallback: eager merge
                            let overlay = self.eval_entries(entries, scope, depth + 1).await?;
                            let mut merged: IndexMap<String, Value> = (*base_map).clone();
                            if let Value::Object(overlay_map, _) = overlay {
                                merged.extend(
                                    overlay_map.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                            }
                            let src = ObjectSource {
                                entries: Vec::new(),
                                scope: IndexMap::new(),
                                is_open: true,
                                type_name: type_name.clone(),
                            };
                            Ok(Value::Object(Arc::new(merged), Some(Arc::new(src))))
                        } else {
                            self.eval_entries(entries, scope, depth + 1).await
                        }
                    }
                }
            }
            Expr::ObjectBody(entries) => self.eval_entries(entries, scope, depth + 1).await,
            Expr::Field(obj_expr, field) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
                // Built-in properties
                match (&obj, field.as_str()) {
                    (Value::List(items), "length") => return Ok(Value::Int(items.len() as i64)),
                    (Value::List(items), "isEmpty") => return Ok(Value::Bool(items.is_empty())),
                    (Value::List(items), "first") => {
                        return items
                            .first()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::List(items), "last") => {
                        return items
                            .last()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::String(s), "length") => {
                        return Ok(Value::Int(s.chars().count() as i64));
                    }
                    (Value::String(s), "isEmpty") => return Ok(Value::Bool(s.is_empty())),
                    (Value::Object(map, _), "length") => return Ok(Value::Int(map.len() as i64)),
                    (Value::Object(map, _), "isEmpty") => return Ok(Value::Bool(map.is_empty())),
                    (Value::Object(map, _), "keys") => {
                        return Ok(Value::List(
                            map.keys().map(|k| Value::String(k.clone())).collect(),
                        ));
                    }
                    (Value::Object(map, _), "values") => {
                        return Ok(Value::List(map.values().cloned().collect()));
                    }
                    // Duration and DataSize units on numbers
                    (
                        Value::Int(_) | Value::Float(_),
                        "ns" | "us" | "ms" | "s" | "min" | "h" | "d" | "b" | "kb" | "mb" | "gb"
                        | "tb" | "pb" | "kib" | "mib" | "gib" | "tib" | "pib",
                    ) => {
                        return Ok(make_unit_object(obj, field));
                    }
                    _ => {}
                }
                match &obj {
                    Value::Object(map, _) => map
                        .get(field)
                        .cloned()
                        .ok_or_else(|| Error::Eval(format!("field not found: {field}"))),
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::NullSafeField(obj_expr, field) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
                match &obj {
                    Value::Null => Ok(Value::Null),
                    Value::Object(map, _) => Ok(map.get(field).cloned().unwrap_or(Value::Null)),
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::Index(obj_expr, key_expr) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
                let key = self.eval_expr(key_expr, scope, depth + 1).await?;
                let key_str = value_to_key(&key)?;
                match obj {
                    Value::Object(map, _) => map
                        .get(&key_str)
                        .cloned()
                        .ok_or_else(|| Error::Eval(format!("key not found: {key_str}"))),
                    _ => Err(Error::Eval("cannot index non-object".into())),
                }
            }
            Expr::Call(func_expr, args) => self.eval_call(func_expr, args, scope, depth).await,
            Expr::If(cond, then_expr, else_expr) => {
                let c = self.eval_expr(cond, scope, depth + 1).await?;
                if is_truthy(&c) {
                    self.eval_expr(then_expr, scope, depth + 1).await
                } else {
                    self.eval_expr(else_expr, scope, depth + 1).await
                }
            }
            Expr::Let(name, val_expr, body_expr) => {
                let val = self.eval_expr(val_expr, scope, depth + 1).await?;
                let mut child = scope.child();
                child.set(name.clone(), val);
                self.eval_expr(body_expr, &child, depth + 1).await
            }
            Expr::Binop(op, left, right) => self.eval_binop(*op, left, right, scope, depth).await,
            Expr::Unop(op, operand) => {
                let v = self.eval_expr(operand, scope, depth + 1).await?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => Err(Error::Eval("cannot negate non-number".into())),
                    },
                    UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
                    UnOp::NonNull => {
                        if matches!(v, Value::Null) {
                            Err(Error::Eval(
                                "non-null assertion failed: value is null".into(),
                            ))
                        } else {
                            Ok(v)
                        }
                    }
                }
            }
            Expr::Is(expr, ty) => {
                let val = self.eval_expr(expr, scope, depth + 1).await?;
                let matches = self.eval_type_check(&val, ty, scope, depth).await?;
                Ok(Value::Bool(matches))
            }
            Expr::As(expr, ty) => {
                let val = self.eval_expr(expr, scope, depth + 1).await?;
                let matches = self.eval_type_check(&val, ty, scope, depth).await?;
                if matches {
                    Ok(val)
                } else {
                    Err(Error::Eval(format!(
                        "cannot cast {} to {}",
                        value_type_name(&val),
                        display_type_expr(ty)
                    )))
                }
            }
            Expr::Throw(msg_expr) => {
                let msg = self.eval_expr(msg_expr, scope, depth + 1).await?;
                Err(Error::Eval(format!("throw: {}", value_to_display(&msg))))
            }
            Expr::Trace(expr) => {
                let v = self.eval_expr(expr, scope, depth + 1).await?;
                eprintln!("[pklr trace] {}", value_to_display(&v));
                Ok(v)
            }
            Expr::Read(uri_expr) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1).await?;
                let uri_str = value_to_display(&uri);
                self.read_resource(&uri_str).await
            }
            Expr::ReadOrNull(uri_expr) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1).await?;
                let uri_str = value_to_display(&uri);
                match self.read_resource(&uri_str).await {
                    Ok(v) => Ok(v),
                    Err(_) => Ok(Value::Null),
                }
            }
        }
    }

    #[async_recursion(?Send)]
    async fn eval_call(
        &mut self,
        func_expr: &Expr,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Handle method calls: obj.method(args)
        if let Expr::Field(obj_expr, method) = func_expr {
            let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1).await?);
            }
            if let Some(result) = self
                .eval_method_call(&obj, method, &evaled_args, depth)
                .await?
            {
                return Ok(result);
            }
            // If the field is a Lambda on an Object (class method), invoke it
            // with the object's properties layered into the call scope so that
            // overridden properties are visible to the function body and any
            // local functions it calls.
            if let Value::Object(ref map, _) = obj
                && let Some(Value::Lambda(params, body, captured)) = map.get(method)
            {
                let mut call_scope = Scope::default();
                for (k, v) in captured.iter() {
                    call_scope.set(k.clone(), v.clone());
                }
                // Layer in ALL of the instance's properties including lambdas,
                // so local functions called by this method also see overrides
                for (k, v) in map.iter() {
                    call_scope.set(k.clone(), v.clone());
                }
                call_scope.set("this".into(), obj.clone());
                for (i, param) in params.iter().enumerate() {
                    if let Some(arg) = evaled_args.get(i) {
                        call_scope.set(param.clone(), arg.clone());
                    }
                }
                return self.eval_expr(body, &call_scope, depth + 1).await;
            }
        }

        // Handle built-in functions: List(), Listing(), Map()
        if let Expr::Ident(name) = func_expr {
            match name.as_str() {
                "List" | "Listing" => {
                    let mut items = Vec::new();
                    for a in args {
                        items.push(self.eval_expr(a, scope, depth + 1).await?);
                    }
                    return Ok(Value::List(items));
                }
                "Set" => {
                    let mut items = Vec::new();
                    for a in args {
                        let val = self.eval_expr(a, scope, depth + 1).await?;
                        if !items.contains(&val) {
                            items.push(val);
                        }
                    }
                    return Ok(Value::List(items)); // deduplicated
                }
                "Regex" => {
                    if let Some(arg) = args.first() {
                        let val = self.eval_expr(arg, scope, depth + 1).await?;
                        let mut map = IndexMap::new();
                        map.insert("pattern".to_string(), val);
                        return Ok(Value::Object(Arc::new(map), None));
                    }
                    return Err(Error::Eval("Regex() requires a pattern argument".into()));
                }
                "Map" => {
                    // Map(k1, v1, k2, v2, ...)
                    let mut map = IndexMap::new();
                    let mut evaled = Vec::new();
                    for a in args {
                        evaled.push(self.eval_expr(a, scope, depth + 1).await?);
                    }
                    for pair in evaled.chunks(2) {
                        if let [k, v] = pair {
                            map.insert(value_to_key(k)?, v.clone());
                        }
                    }
                    return Ok(Value::Object(Arc::new(map), None));
                }
                _ => {}
            }
        }

        // Evaluate the function expression
        let func_val = self.eval_expr(func_expr, scope, depth + 1).await?;

        // Lambda call
        if let Value::Lambda(params, body, captured) = func_val {
            let mut call_scope = Scope::default();
            // Restore captured scope
            for (k, v) in captured.iter() {
                call_scope.set(k.clone(), v.clone());
            }
            // If we're inside a method call context (scope has `this` as an Object),
            // layer the instance's properties so local functions see overridden values
            if let Some(Value::Object(this_map, _)) = scope.get("this") {
                for (k, v) in this_map.iter() {
                    call_scope.set(k.clone(), v.clone());
                }
            }
            // Bind arguments to parameters
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1).await?);
            }
            for (param, arg) in params.iter().zip(evaled_args) {
                call_scope.set(param.clone(), arg);
            }
            return self.eval_expr(&body, &call_scope, depth + 1).await;
        }

        // Built-in type constructors resolved from scope (e.g. base.Regex)
        if let Value::String(ref name) = func_val
            && name == "Regex"
            && let Some(arg) = args.first()
        {
            let val = self.eval_expr(arg, scope, depth + 1).await?;
            let mut map = IndexMap::new();
            map.insert("pattern".to_string(), val);
            return Ok(Value::Object(Arc::new(map), None));
        }

        // Plain call with no args on an object — return the object
        if args.is_empty() {
            return Ok(func_val);
        }
        Err(Error::Eval("cannot call non-function".into()))
    }

    #[async_recursion(?Send)]
    async fn eval_method_call(
        &mut self,
        obj: &Value,
        method: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        match (obj, method) {
            // String methods
            (Value::String(s), "contains") => {
                let arg = require_str_arg(args, 0, "contains")?;
                Ok(Some(Value::Bool(s.contains(arg))))
            }
            (Value::String(s), "startsWith") => {
                let arg = require_str_arg(args, 0, "startsWith")?;
                Ok(Some(Value::Bool(s.starts_with(arg))))
            }
            (Value::String(s), "endsWith") => {
                let arg = require_str_arg(args, 0, "endsWith")?;
                Ok(Some(Value::Bool(s.ends_with(arg))))
            }
            (Value::String(s), "replaceAll") => {
                let from = require_str_arg(args, 0, "replaceAll")?;
                let to = require_str_arg(args, 1, "replaceAll")?;
                Ok(Some(Value::String(s.replace(from, to))))
            }
            (Value::String(s), "split") => {
                let sep = require_str_arg(args, 0, "split")?;
                Ok(Some(Value::List(
                    s.split(sep).map(|p| Value::String(p.to_string())).collect(),
                )))
            }
            (Value::String(s), "trim") => Ok(Some(Value::String(s.trim().to_string()))),
            (Value::String(s), "trimStart") => Ok(Some(Value::String(s.trim_start().to_string()))),
            (Value::String(s), "trimEnd") => Ok(Some(Value::String(s.trim_end().to_string()))),
            (Value::String(s), "toUpperCase") => Ok(Some(Value::String(s.to_uppercase()))),
            (Value::String(s), "toLowerCase") => Ok(Some(Value::String(s.to_lowercase()))),
            (Value::String(s), "toInt") => s
                .parse::<i64>()
                .map(|n| Some(Value::Int(n)))
                .map_err(|_| Error::Eval(format!("cannot convert '{s}' to Int"))),

            // List methods
            (Value::List(items), "contains") => {
                let arg = args.first().cloned().unwrap_or(Value::Null);
                Ok(Some(Value::Bool(items.contains(&arg))))
            }
            (Value::List(items), "toList") => Ok(Some(Value::List(items.clone()))),
            (Value::List(items), "toSet") => {
                let mut seen = Vec::new();
                for item in items {
                    if !seen.contains(item) {
                        seen.push(item.clone());
                    }
                }
                Ok(Some(Value::List(seen)))
            }
            (Value::List(items), "map") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("map requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    result.push(
                        self.invoke_lambda(lambda, std::slice::from_ref(item), depth)
                            .await?,
                    );
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "flatMap") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("flatMap requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    let val = self
                        .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                        .await?;
                    if let Value::List(inner) = val {
                        result.extend(inner);
                    } else {
                        result.push(val);
                    }
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    let cond = self
                        .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                        .await?;
                    if is_truthy(&cond) {
                        result.push(item.clone());
                    }
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "fold") => {
                let init = args
                    .first()
                    .ok_or_else(|| Error::Eval("fold requires initial value".into()))?
                    .clone();
                let lambda = args
                    .get(1)
                    .ok_or_else(|| Error::Eval("fold requires a function argument".into()))?;
                let mut acc = init;
                for item in items {
                    acc = self
                        .invoke_lambda(lambda, &[acc, item.clone()], depth)
                        .await?;
                }
                Ok(Some(acc))
            }
            (Value::List(items), "any") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("any requires a function argument".into()))?;
                for item in items {
                    if is_truthy(
                        &self
                            .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                            .await?,
                    ) {
                        return Ok(Some(Value::Bool(true)));
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            (Value::List(items), "every") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("every requires a function argument".into()))?;
                for item in items {
                    if !is_truthy(
                        &self
                            .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                            .await?,
                    ) {
                        return Ok(Some(Value::Bool(false)));
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            (Value::List(items), "join") => {
                let sep = args.first().and_then(|v| v.as_str()).unwrap_or(",");
                let s: Vec<String> = items.iter().map(value_to_display).collect();
                Ok(Some(Value::String(s.join(sep))))
            }
            (Value::List(items), "reverse") => {
                let mut rev = items.clone();
                rev.reverse();
                Ok(Some(Value::List(rev)))
            }

            // Object/Mapping methods
            (Value::Object(map, _), "containsKey") => {
                let key = args.first().and_then(|v| v.as_str()).unwrap_or("");
                Ok(Some(Value::Bool(map.contains_key(key))))
            }
            (Value::Object(map, _), "toMap") => Ok(Some(Value::Object(map.clone(), None))),
            (Value::Object(map, _), "mapValues") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("mapValues requires a function".into()))?;
                let mut result = IndexMap::new();
                for (k, v) in map.iter() {
                    let new_v = self
                        .invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)
                        .await?;
                    result.insert(k.clone(), new_v);
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
            }
            (Value::Object(..), "toList") | (Value::Object(..), "toDynamic") => {
                Ok(Some(obj.clone()))
            }

            // Int/Float methods
            (Value::Int(n), "toString") => Ok(Some(Value::String(n.to_string()))),
            (Value::Float(f), "toString") => Ok(Some(Value::String(f.to_string()))),
            (Value::Bool(b), "toString") => Ok(Some(Value::String(b.to_string()))),

            // Lambda.apply()
            (Value::Lambda(params, body, captured), "apply") => {
                let mut call_scope = Scope::default();
                for (k, v) in captured.iter() {
                    call_scope.set(k.clone(), v.clone());
                }
                for (param, arg) in params.iter().zip(args.iter()) {
                    call_scope.set(param.clone(), arg.clone());
                }
                Ok(Some(self.eval_expr(body, &call_scope, depth + 1).await?))
            }

            _ => Ok(None), // not a known method
        }
    }

    #[async_recursion(?Send)]
    async fn invoke_lambda(
        &mut self,
        lambda: &Value,
        args: &[Value],
        depth: usize,
    ) -> Result<Value> {
        if let Value::Lambda(params, body, captured) = lambda {
            let mut scope = Scope::default();
            for (k, v) in captured.iter() {
                scope.set(k.clone(), v.clone());
            }
            for (param, arg) in params.iter().zip(args.iter()) {
                scope.set(param.clone(), arg.clone());
            }
            self.eval_expr(body, &scope, depth + 1).await
        } else {
            Err(Error::Eval("expected a function".into()))
        }
    }

    #[async_recursion(?Send)]
    async fn eval_binop(
        &mut self,
        op: BinOp,
        left: &Expr,
        right: &Expr,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Special case: object amendment `base + ObjectBody(entries)`
        if let BinOp::Add = op
            && let Expr::ObjectBody(overlay_entries) = right
        {
            let base = self.eval_expr(left, scope, depth + 1).await?;
            // Late binding: if the base carries its original entry definitions,
            // merge entry lists and re-evaluate so dependent properties pick up
            // overridden values.
            if let Value::Object(_, Some(base_src)) = &base {
                return self
                    .eval_amended_object(
                        &base_src.entries.clone(),
                        &base_src.scope.clone(),
                        overlay_entries,
                        scope,
                        depth,
                    )
                    .await;
            }
            // Fallback: eager merge when base has no entry source
            let overlay = self.eval_entries(overlay_entries, scope, depth + 1).await?;
            return Ok(merge_values(base, overlay));
        }

        let l = self.eval_expr(left, scope, depth + 1).await?;
        let r = self.eval_expr(right, scope, depth + 1).await?;
        match op {
            BinOp::Add => add_values(l, r),
            BinOp::Sub => arithmetic(l, r, |a, b| Ok(a - b), |a, b| Ok(a - b)),
            BinOp::Mul => arithmetic(l, r, |a, b| Ok(a * b), |a, b| Ok(a * b)),
            BinOp::Div => arithmetic(
                l,
                r,
                |a, b| {
                    if b == 0 {
                        Err(Error::Eval("division by zero".into()))
                    } else {
                        Ok(a / b)
                    }
                },
                |a, b| Ok(a / b),
            ),
            BinOp::Mod => arithmetic(
                l,
                r,
                |a, b| {
                    if b == 0 {
                        Err(Error::Eval("modulo by zero".into()))
                    } else {
                        Ok(a % b)
                    }
                },
                |a, b| Ok(a % b),
            ),
            BinOp::Eq => Ok(Value::Bool(values_eq(&l, &r))),
            BinOp::Ne => Ok(Value::Bool(!values_eq(&l, &r))),
            BinOp::Lt => compare(l, r, std::cmp::Ordering::Less),
            BinOp::Le => compare_or_eq(l, r, std::cmp::Ordering::Less),
            BinOp::Gt => compare(l, r, std::cmp::Ordering::Greater),
            BinOp::Ge => compare_or_eq(l, r, std::cmp::Ordering::Greater),
            BinOp::And => Ok(Value::Bool(is_truthy(&l) && is_truthy(&r))),
            BinOp::Or => Ok(Value::Bool(is_truthy(&l) || is_truthy(&r))),
            BinOp::IntDiv => arithmetic(
                l,
                r,
                |a, b| {
                    if b == 0 {
                        Err(Error::Eval("division by zero".into()))
                    } else {
                        Ok(a / b)
                    }
                },
                |a, b| Ok((a / b).floor()),
            ),
            BinOp::Pow => arithmetic(
                l,
                r,
                |a, b| {
                    if b < 0 {
                        Err(Error::Eval(
                            "integer exponentiation with negative exponent is not supported".into(),
                        ))
                    } else {
                        Ok(a.pow(b as u32))
                    }
                },
                |a, b| Ok(a.powf(b)),
            ),
            BinOp::NullCoalesce => {
                if matches!(l, Value::Null) {
                    Ok(r)
                } else {
                    Ok(l)
                }
            }
            BinOp::Pipe => {
                // x |> f  is equivalent to  f(x)
                match r {
                    Value::Lambda(params, body, captured) => {
                        if params.len() != 1 {
                            return Err(Error::Eval(format!(
                                "pipe operator requires a single-parameter function, got {}",
                                params.len()
                            )));
                        }
                        let mut call_scope = Scope::default();
                        for (k, v) in captured.iter() {
                            call_scope.set(k.clone(), v.clone());
                        }
                        call_scope.set(params[0].clone(), l);
                        self.eval_expr(&body, &call_scope, depth + 1).await
                    }
                    _ => Err(Error::Eval(
                        "pipe operator requires a function on the right side".into(),
                    )),
                }
            }
        }
    }

    /// Find and evaluate a `default { ... }` property in an entry list.
    #[async_recursion(?Send)]
    async fn find_default_template(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        for entry in entries {
            if let Entry::Property(prop) = entry
                && prop.name == "default"
                && !has_modifier(&prop.modifiers, Modifier::Local)
            {
                return self.eval_property(prop, scope, depth).await;
            }
        }
        Ok(None)
    }

    #[async_recursion(?Send)]
    async fn eval_mapping_entries_with_type_default(
        &mut self,
        entries: &[crate::parser::Entry],
        scope: &Scope,
        depth: usize,
        map: &mut IndexMap<String, Value>,
        type_default: Option<&Value>,
    ) -> Result<()> {
        let explicit_default = self.find_default_template(entries, scope, depth).await?;
        let default_template = explicit_default.as_ref().or(type_default);

        for entry in entries {
            match entry {
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, scope, depth + 1).await?;
                    // If the default template has ObjectSource, use eval_amended_object
                    // so nested property amendments work (e.g., `steps { ["x"] { ... } }`
                    // inside a Hook default properly amends the Hook's steps Mapping).
                    let val = if let Some(Value::Object(_, Some(src))) = default_template
                        && let Expr::ObjectBody(body) = val_expr
                    {
                        let mut result = self
                            .eval_amended_object(&src.entries, &src.scope, body, scope, depth)
                            .await?;
                        if let Some(ref tn) = src.type_name
                            && let Value::Object(_, ref mut result_src) = result
                        {
                            let new_src = match result_src.as_ref() {
                                Some(s) => {
                                    let mut ns = (**s).clone();
                                    ns.type_name = Some(tn.clone());
                                    ns
                                }
                                None => ObjectSource {
                                    entries: vec![],
                                    scope: IndexMap::new(),
                                    is_open: true,
                                    type_name: Some(tn.clone()),
                                },
                            };
                            *result_src = Some(std::sync::Arc::new(new_src));
                        }
                        result
                    } else {
                        let mut val = self.eval_expr(val_expr, scope, depth + 1).await?;
                        if let Some(tpl) = default_template {
                            val = merge_values(tpl.clone(), val);
                        }
                        val
                    };
                    map.insert(value_to_key(&key)?, val);
                }
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Property(prop) if prop.name == "default" && default_template.is_some() => {}
                Entry::Spread(e) => {
                    let val = self.eval_expr(e, scope, depth + 1).await?;
                    if let Value::Object(m, _) = val {
                        map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self.eval_expr(&fgen.collection, scope, depth + 1).await?;
                    for (k, v) in collection_to_items(collection) {
                        let mut iter_scope = scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(kv) = &fgen.key_var {
                            iter_scope.set(kv.clone(), k);
                        }
                        self.eval_mapping_entries_with_type_default(
                            &fgen.body,
                            &iter_scope,
                            depth + 1,
                            map,
                            type_default,
                        )
                        .await?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Walk the AST of the `output` property to extract converter lambdas.
    /// Looks for the structure: `output { renderer { converters { [Type] = (x) -> ... } } }`.
    /// Converter keys are type identifiers (e.g., `[Regex]`), which we preserve as
    /// strings for matching against `ObjectSource.type_name`.
    async fn extract_converters_from_ast(
        &mut self,
        output_prop: &Property,
        scope: &Scope,
        depth: usize,
    ) {
        let Some(output_body) = &output_prop.body else {
            return;
        };
        // Find `renderer { converters { ... } }` inside output
        let Some(renderer_body) = output_body.iter().find_map(|entry| {
            if let Entry::Property(p) = entry
                && p.name == "renderer"
            {
                p.body.as_ref()
            } else {
                None
            }
        }) else {
            return;
        };
        let Some(converters_body) = renderer_body.iter().find_map(|entry| {
            if let Entry::Property(p) = entry
                && p.name == "converters"
            {
                p.body.as_ref()
            } else {
                None
            }
        }) else {
            return;
        };
        // Each converter is a DynProperty: [ClassName] = (x) -> expr
        for centry in converters_body {
            if let Entry::DynProperty(key_expr, val_expr) = centry {
                // Extract the class name from the key expression
                let class_name = match key_expr {
                    Expr::Ident(name) => name.clone(),
                    Expr::Field(_, name) => name.clone(),
                    Expr::String(s) => s.clone(),
                    _ => continue,
                };
                // Evaluate the lambda value
                if let Ok(lambda) = self.eval_expr(val_expr, scope, depth).await
                    && matches!(lambda, Value::Lambda(..))
                {
                    self.converters.push((class_name, lambda));
                }
            }
        }
    }

    /// Apply `output.renderer.converters` to a value tree.
    /// Walks recursively, replacing typed objects with their converter output.
    pub async fn apply_converters(&mut self, value: Value) -> Result<Value> {
        if self.converters.is_empty() {
            return Ok(value);
        }
        let converters = self.converters.clone();
        self.apply_converters_recursive(value, &converters).await
    }

    #[async_recursion(?Send)]
    async fn apply_converters_recursive(
        &mut self,
        value: Value,
        converters: &[(String, Value)],
    ) -> Result<Value> {
        match value {
            Value::Object(map, ref src) => {
                // Check if this object has a type_name that matches a converter
                let type_name = src.as_ref().and_then(|s| s.type_name.as_deref());

                if let Some(tn) = type_name {
                    for (conv_name, lambda) in converters {
                        // Match exact name, or as a dotted suffix
                        // (e.g., converter "Step" matches type "Step" or "Config.Step")
                        let matches = conv_name == tn
                            || (tn.len() > conv_name.len()
                                && tn.ends_with(conv_name.as_str())
                                && tn.as_bytes()[tn.len() - conv_name.len() - 1] == b'.')
                            || (conv_name.len() > tn.len()
                                && conv_name.ends_with(tn)
                                && conv_name.as_bytes()[conv_name.len() - tn.len() - 1] == b'.');
                        if matches && let Value::Lambda(params, body, captured) = lambda {
                            let mut call_scope = Scope::default();
                            for (k, v) in captured.iter() {
                                call_scope.set(k.clone(), v.clone());
                            }
                            // Bind the object as the first parameter
                            if let Some(param) = params.first() {
                                call_scope
                                    .set(param.clone(), Value::Object(map.clone(), src.clone()));
                            }
                            let result = self.eval_expr(body, &call_scope, 0).await?;
                            // Recursively apply converters to the result
                            return self.apply_converters_recursive(result, converters).await;
                        }
                    }
                }

                // No converter matched — recurse into children
                let mut new_map = IndexMap::new();
                for (k, v) in map.iter() {
                    new_map.insert(
                        k.clone(),
                        self.apply_converters_recursive(v.clone(), converters)
                            .await?,
                    );
                }
                Ok(Value::Object(Arc::new(new_map), src.clone()))
            }
            Value::List(items) => {
                let mut new_items = Vec::with_capacity(items.len());
                for item in items {
                    new_items.push(self.apply_converters_recursive(item, converters).await?);
                }
                Ok(Value::List(new_items))
            }
            other => Ok(other),
        }
    }
}

// --- Scope ---

#[derive(Debug, Default, Clone)]
struct Scope {
    vars: IndexMap<String, Value>,
    type_aliases: IndexMap<String, crate::parser::TypeExpr>,
    parent: Option<Rc<Scope>>,
}

impl Scope {
    fn child(&self) -> Self {
        Self {
            vars: IndexMap::new(),
            type_aliases: IndexMap::new(),
            parent: Some(Rc::new(self.clone())),
        }
    }

    fn set(&mut self, name: String, val: Value) {
        self.vars.insert(name, val);
    }

    fn set_type_alias(&mut self, name: String, ty: crate::parser::TypeExpr) {
        self.type_aliases.insert(name, ty);
    }

    fn get_type_alias(&self, name: &str) -> Option<&crate::parser::TypeExpr> {
        self.type_aliases
            .get(name)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_type_alias(name)))
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.vars
            .get(name)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get(name)))
    }

    fn flatten(&self) -> IndexMap<String, Value> {
        let mut result = self
            .parent
            .as_ref()
            .map(|p| p.flatten())
            .unwrap_or_default();
        result.extend(self.vars.clone());
        result
    }

    fn flatten_type_aliases(&self) -> IndexMap<String, crate::parser::TypeExpr> {
        let mut result = self
            .parent
            .as_ref()
            .map(|p| p.flatten_type_aliases())
            .unwrap_or_default();
        result.extend(self.type_aliases.clone());
        result
    }
}

// --- Helpers ---

/// Resolved package info: either a direct file URL or a zip archive + entry path.
enum PackageSource {
    /// Direct file download (pkg.pkl-lang.org format)
    Direct(String),
    /// Zip archive URL + path within the archive
    Zip(String, String),
}

/// Resolve a `package://` URI to a download source.
fn resolve_package_uri(uri: &str) -> Result<PackageSource> {
    // Format 1: package://pkg.pkl-lang.org/github.com/owner/repo@version#/path.pkl
    // These resolve to direct file downloads from GitHub releases
    if let Some(rest) = uri.strip_prefix("package://pkg.pkl-lang.org/github.com/")
        && let Some((repo_ver, fragment)) = rest.split_once('#')
        && let Some((repo, version)) = repo_ver.split_once('@')
    {
        let file_path = fragment.strip_prefix('/').unwrap_or(fragment);
        return Ok(PackageSource::Direct(format!(
            "https://github.com/{repo}/releases/download/{version}/{file_path}"
        )));
    }
    // Format 2: package://github.com/owner/repo/releases/download/v1.0/name@1.0#/path.pkl
    // These are zip archives; the fragment is a path within the zip
    if let Some(rest) = uri.strip_prefix("package://github.com/")
        && let Some((base, fragment)) = rest.split_once('#')
    {
        let file_path = fragment.strip_prefix('/').unwrap_or(fragment);
        let zip_url = format!("https://github.com/{base}.zip");
        return Ok(PackageSource::Zip(zip_url, file_path.to_string()));
    }
    Err(Error::Eval(format!("unsupported package URI: {uri}")))
}

fn check_deprecated(annotations: &[Annotation], prop_name: &str) {
    for ann in annotations {
        if ann.name == "Deprecated" {
            // Look for a "message" property in the annotation body
            let mut message = None;
            for entry in &ann.body {
                if let Entry::Property(p) = entry
                    && p.name == "message"
                    && let Some(Expr::String(s)) = &p.value
                {
                    message = Some(s.clone());
                }
            }
            if let Some(msg) = message {
                eprintln!("[pklr] WARNING: property '{prop_name}' is deprecated: {msg}");
            } else {
                eprintln!("[pklr] WARNING: property '{prop_name}' is deprecated");
            }
        }
    }
}

/// Resolve a potentially dotted name (e.g. "Foo.Bar") in scope.
fn resolve_dotted(scope: &Scope, name: &str) -> Option<Value> {
    let parts: Vec<&str> = name.split('.').collect();
    let mut val = scope.get(parts[0])?.clone();
    for part in &parts[1..] {
        val = match val {
            Value::Object(ref map, _) => map.get(*part)?.clone(),
            _ => return None,
        };
    }
    Some(val)
}

fn has_modifier(mods: &[Modifier], target: Modifier) -> bool {
    mods.contains(&target)
}

fn require_str_arg<'a>(args: &'a [Value], idx: usize, method: &str) -> Result<&'a str> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(Error::Eval(format!(
            "{method}() requires a String argument, got {}",
            value_type_name(other)
        ))),
        None => Err(Error::Eval(format!(
            "{method}() requires {} argument(s)",
            idx + 1
        ))),
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "Boolean",
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::String(_) => "String",
        Value::Object(..) => "Object",
        Value::List(_) => "List",
        Value::Lambda(..) => "Function",
    }
}

fn value_to_key(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Float(f) => Ok(f.to_string()),
        Value::Object(_, _) | Value::List(_) | Value::Lambda(..) | Value::Null => {
            Ok(value_to_display(v))
        }
    }
}

fn value_to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => s.clone(),
        _ => format!("{v:?}"),
    }
}

/// Format a TypeExpr for user-facing error messages.
fn display_type_expr(ty: &crate::parser::TypeExpr) -> String {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Nullable(inner) => format!("{}?", display_type_expr(inner)),
        TypeExpr::Union(variants) => variants
            .iter()
            .map(display_type_expr)
            .collect::<Vec<_>>()
            .join("|"),
        TypeExpr::Generic(name, args) => {
            let args_str: Vec<_> = args.iter().map(display_type_expr).collect();
            format!("{}<{}>", name, args_str.join(", "))
        }
        TypeExpr::Constrained(name, _) => format!("{name}(...)"),
    }
}

/// Check if a value matches a Pkl type expression (non-constrained types only).
/// For constrained types, use `Evaluator::eval_type_check` instead.
fn value_is_type(val: &Value, ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => match name.as_str() {
            "Null" => matches!(val, Value::Null),
            "Boolean" | "Bool" => matches!(val, Value::Bool(_)),
            "Int" => matches!(val, Value::Int(_)),
            "Float" => matches!(val, Value::Float(_)),
            "Number" => matches!(val, Value::Int(_) | Value::Float(_)),
            "String" => matches!(val, Value::String(_)),
            "List" | "Listing" | "Set" => matches!(val, Value::List(_)),
            "Map" | "Mapping" | "Object" | "Dynamic" => matches!(val, Value::Object(..)),
            "Function" => matches!(val, Value::Lambda(..)),
            "Any" => true,
            _ => {
                // Unknown type name -- could be a class; treat objects as matching
                matches!(val, Value::Object(..))
            }
        },
        TypeExpr::Nullable(inner) => matches!(val, Value::Null) || value_is_type(val, inner),
        TypeExpr::Union(variants) => variants.iter().any(|v| value_is_type(val, v)),
        TypeExpr::Generic(name, _) => {
            // Check the base type, ignore type parameters
            match name.as_str() {
                "List" | "Listing" | "Set" => matches!(val, Value::List(_)),
                "Map" | "Mapping" => matches!(val, Value::Object(..)),
                _ => matches!(val, Value::Object(..)),
            }
        }
        TypeExpr::Constrained(base_name, _) => {
            // Just check the base type; constraint requires async eval
            value_is_type(val, &TypeExpr::Named(base_name.clone()))
        }
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
        (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

fn add_values(l: Value, r: Value) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
        (Value::String(a), Value::String(b)) => Ok(Value::String(a + &b)),
        (Value::List(mut a), Value::List(b)) => {
            a.extend(b);
            Ok(Value::List(a))
        }
        (Value::Object(mut a, _), Value::Object(b, _)) => {
            Arc::make_mut(&mut a).extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
            Ok(Value::Object(a, None))
        }
        (l, r) => Err(Error::Eval(format!("cannot add {:?} and {:?}", l, r))),
    }
}

fn arithmetic(
    l: Value,
    r: Value,
    fi: impl Fn(i64, i64) -> Result<i64>,
    ff: impl Fn(f64, f64) -> Result<f64>,
) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(fi(a, b)?)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(ff(a, b)?)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(ff(a as f64, b)?)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(ff(a, b as f64)?)),
        (l, r) => Err(Error::Eval(format!(
            "arithmetic type mismatch: {:?} vs {:?}",
            l, r
        ))),
    }
}

fn compare(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    Ok(Value::Bool(value_cmp(&l, &r)? == ord))
}

fn compare_or_eq(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    let c = value_cmp(&l, &r)?;
    Ok(Value::Bool(c == ord || c == std::cmp::Ordering::Equal))
}

fn value_cmp(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => {
            Ok(x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
        }
        (Value::Int(x), Value::Float(y)) => Ok((*x as f64)
            .partial_cmp(y)
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::Float(x), Value::Int(y)) => Ok(x
            .partial_cmp(&(*y as f64))
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::String(x), Value::String(y)) => Ok(x.cmp(y)),
        _ => Err(Error::Eval(format!("cannot compare {:?} and {:?}", a, b))),
    }
}

fn merge_values(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut b, base_src), Value::Object(o, _)) => {
            let b_map = Arc::make_mut(&mut b);
            for (k, v) in o.iter() {
                if let Some(existing) = b_map.shift_remove(k) {
                    b_map.insert(k.clone(), merge_values(existing, v.clone()));
                } else {
                    b_map.insert(k.clone(), v.clone());
                }
            }
            Value::Object(b, base_src)
        }
        (_, overlay) => overlay,
    }
}

fn make_unit_object(value: Value, unit: &str) -> Value {
    let mut map = IndexMap::new();
    map.insert("value".to_string(), value);
    map.insert("unit".to_string(), Value::String(unit.to_string()));
    Value::Object(Arc::new(map), None)
}

/// Expand a simple glob pattern relative to a base directory.
/// Supports `dir/*.ext` and `*.ext` patterns (single `*` only).
/// Expand a simple glob pattern relative to a base directory.
/// Supports `dir/*.ext` and `*.ext` patterns (single `*` only).
pub fn expand_glob(base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let full = base.join(pattern);
    let dir = full.parent().unwrap_or(base).to_path_buf();
    let file_pattern = full
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    if !dir.is_dir() {
        return Ok(vec![]);
    }

    // Convert simple glob to prefix/suffix matching
    let (prefix, suffix) = if let Some(star_pos) = file_pattern.find('*') {
        (
            file_pattern[..star_pos].to_string(),
            file_pattern[star_pos + 1..].to_string(),
        )
    } else {
        // No wildcard — exact match
        let p = dir.join(&file_pattern);
        return if p.exists() { Ok(vec![p]) } else { Ok(vec![]) };
    };

    let min_len = prefix.len() + suffix.len();
    let mut results = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Io(dir.to_path_buf(), e))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.len() >= min_len
            && name.starts_with(&prefix)
            && name.ends_with(&suffix)
            && entry.path().is_file()
        {
            results.push(entry.path());
        }
    }
    results.sort();
    Ok(results)
}

/// Get a relative path string from `path` relative to `base`, or the full path if not a prefix.
fn pathdiff_or_full(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn stdlib_module(name: &str) -> Value {
    let mut map = IndexMap::new();
    if name == "base" {
        map.insert("Regex".to_string(), Value::String("Regex".to_string()));
    }
    Value::Object(Arc::new(map), None)
}

fn seed_builtins(scope: &mut Scope) {
    for name in [
        "Regex",
        "Dynamic",
        "Annotation",
        "Duration",
        "DataSize",
        "IntSeq",
        "Pair",
    ] {
        scope.set(name.to_string(), Value::String(name.to_string()));
    }
}

fn collection_to_items(v: Value) -> Vec<(Value, Value)> {
    match v {
        Value::List(items) => items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Value::Int(i as i64), v))
            .collect(),
        Value::Object(map, _) => map
            .iter()
            .map(|(k, v)| (Value::String(k.clone()), v.clone()))
            .collect(),
        _ => vec![],
    }
}
