use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use async_recursion::async_recursion;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use crate::capabilities::EvalCapabilities;
use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{self, BinOp, Entry, Expr, Modifier, Module, Property, StringInterpPart, UnOp};
use crate::value::{ObjectSource, Value};

const DYNAMIC_SIBLING_REF: &str = "\0pklr:dynamic-sibling";

/// Evaluates pkl source files to [`Value`].
pub struct Evaluator {
    base_path: PathBuf,
    /// Maximum import depth to prevent infinite recursion
    max_depth: usize,
    /// Cache for fetched HTTP sources (URL → source text)
    http_cache: HashMap<String, String>,
    /// Cache for evaluated local imports (canonical path → Value)
    import_cache: HashMap<PathBuf, Value>,
    /// Final scopes for modules evaluated in this run, used to preserve inherited locals.
    module_scopes: HashMap<PathBuf, ModuleScopeSnapshot>,
    /// Environment variables read during evaluation (name → observed value).
    env_reads: BTreeMap<String, Option<String>>,
    /// Local files currently being evaluated with inherited scope.
    scoped_imports_in_flight: HashSet<PathBuf>,
    /// Host-provided IO for files, environment, HTTP, packages, and globs.
    capabilities: Box<dyn EvalCapabilities>,
    /// Extracted package zip directories (zip URL → temp dir path)
    #[cfg(feature = "package-zip")]
    package_dirs: HashMap<String, PathBuf>,
    /// Directory used to persist downloaded package content across evaluators.
    package_cache_dir: Option<PathBuf>,
    /// HTTP URL roots whose sources belong to direct-download packages.
    package_http_roots: HashSet<String>,
    /// Whether network access is disabled for this evaluator.
    offline: bool,
    /// HTTP URL rewrite rules (source_prefix → target_prefix).
    /// Longest matching prefix wins.
    http_rewrites: Vec<(String, String)>,
    /// Converters extracted from `output.renderer.converters`.
    /// Each entry maps a class name to a converter lambda.
    converters: Vec<(String, Value)>,
    /// Set of (property_name, message) pairs already warned about.
    /// Used to deduplicate `@Deprecated` warnings so a deprecated property
    /// referenced inside a loop or template doesn't flood stderr.
    warned_deprecated: std::collections::HashSet<(String, Option<String>)>,
}

#[derive(Clone, Default)]
struct MappingInheritedDefault {
    value: Option<Value>,
    entries: Option<Vec<Entry>>,
}

#[derive(Clone, Default)]
struct ModuleScopeSnapshot {
    values: IndexMap<String, Value>,
    type_aliases: IndexMap<String, crate::parser::TypeExpr>,
    late_properties: Vec<Property>,
}

fn regex_value(pattern: Value) -> Value {
    let mut map = IndexMap::new();
    map.insert("_type".to_string(), Value::String("regex".to_string()));
    map.insert("pattern".to_string(), pattern);
    Value::Object(Arc::new(map), None)
}

#[cfg(feature = "native-io")]
impl Default for Evaluator {
    fn default() -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::new(),
            import_cache: HashMap::new(),
            module_scopes: HashMap::new(),
            env_reads: BTreeMap::new(),
            scoped_imports_in_flight: HashSet::new(),
            capabilities: Box::new(crate::capabilities::NativeCapabilities::new()),
            #[cfg(feature = "package-zip")]
            package_dirs: HashMap::new(),
            package_cache_dir: None,
            package_http_roots: HashSet::new(),
            offline: false,
            http_rewrites: Vec::new(),
            converters: Vec::new(),
            warned_deprecated: std::collections::HashSet::new(),
        }
    }
}

impl Evaluator {
    #[cfg(feature = "native-io")]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capabilities(capabilities: impl EvalCapabilities + 'static) -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::new(),
            import_cache: HashMap::new(),
            module_scopes: HashMap::new(),
            env_reads: BTreeMap::new(),
            scoped_imports_in_flight: HashSet::new(),
            capabilities: Box::new(capabilities),
            #[cfg(feature = "package-zip")]
            package_dirs: HashMap::new(),
            package_cache_dir: None,
            package_http_roots: HashSet::new(),
            offline: false,
            http_rewrites: Vec::new(),
            converters: Vec::new(),
            warned_deprecated: std::collections::HashSet::new(),
        }
    }

    pub fn set_base_path(&mut self, path: &Path) {
        self.base_path = path.to_path_buf();
    }

    fn resolve_local_path(&self, current_path: &Path, uri: &str) -> PathBuf {
        #[cfg(feature = "package-zip")]
        if let Some(from_root) = uri.strip_prefix(".../")
            && let Some(root) = self
                .package_dirs
                .values()
                .find(|root| current_path.starts_with(root))
        {
            return root.join(from_root);
        }
        if let Some(from_root) = uri.strip_prefix(".../") {
            return self.base_path.join(from_root);
        }
        current_path.parent().unwrap_or(Path::new(".")).join(uri)
    }

    /// Persist downloaded package content under `path`.
    pub fn set_package_cache_dir(&mut self, path: impl Into<PathBuf>) {
        self.package_cache_dir = Some(path.into());
    }

    /// Disable network access. Package imports can still use the persistent cache.
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    /// Return the environment variables observed by the latest evaluation.
    ///
    /// Missing variables are included with a `None` value. Entries are ordered
    /// by variable name so callers can serialize or hash them deterministically.
    pub fn env_reads(&self) -> &BTreeMap<String, Option<String>> {
        &self.env_reads
    }

    #[cfg(feature = "native-io")]
    pub(crate) fn take_env_reads(&mut self) -> BTreeMap<String, Option<String>> {
        std::mem::take(&mut self.env_reads)
    }

    fn begin_evaluation(&mut self) {
        self.env_reads.clear();
        self.import_cache.clear();
        self.module_scopes.clear();
        self.scoped_imports_in_flight.clear();
    }

    /// Set a custom HTTP client for fetching remote imports and packages.
    /// Use this to configure proxy settings, CA certificates, timeouts, etc.
    #[cfg(feature = "http")]
    pub fn set_http_client(&mut self, client: reqwest::Client) {
        self.capabilities.set_http_client(client);
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

    /// If `field` is marked `@Deprecated` in `source`, emit the warning at
    /// most once per (field, message) pair. Called from field-access
    /// expressions so the warning fires when a deprecated property is *used*,
    /// not when its containing module loads. Per-call dedup mirrors pkl-jvm,
    /// which avoids flooding stderr when a deprecated property is referenced
    /// inside a loop or template.
    fn warn_if_deprecated_access(&mut self, source: &Option<Arc<ObjectSource>>, field: &str) {
        let Some(src) = source else { return };
        let Some(message) = src.deprecated.get(field) else {
            return;
        };
        let key = (field.to_string(), message.clone());
        if !self.warned_deprecated.insert(key) {
            return;
        }
        if let Some(msg) = message {
            eprintln!("[pklr] WARNING: property '{field}' is deprecated: {msg}");
        } else {
            eprintln!("[pklr] WARNING: property '{field}' is deprecated");
        }
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
            let content = self.capabilities.read_to_string(Path::new(path)).await?;
            Ok(Value::String(content))
        } else if let Some(var_name) = uri.strip_prefix("env:") {
            // env: — read environment variable
            let value = self.capabilities.read_env(var_name).await?;
            self.env_reads.insert(var_name.to_string(), value.clone());
            let Some(val) = value else {
                return Err(Error::Eval(format!(
                    "environment variable not found: {var_name}"
                )));
            };
            Ok(Value::String(val))
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
            let content = self.capabilities.read_to_string(&file_path).await?;
            Ok(Value::String(content))
        }
    }

    async fn fetch_source(&mut self, url: &str) -> Result<String> {
        if self
            .package_http_roots
            .iter()
            .any(|root| url.starts_with(root))
        {
            return self.fetch_package_source(url).await;
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
        let body = self.capabilities.fetch_text(fetch_url).await?;
        self.http_cache.insert(fetch_url.to_string(), body.clone());
        Ok(body)
    }

    #[async_recursion(?Send)]
    async fn inherited_reference_roots(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
    ) -> Result<HashSet<String>> {
        let mut refs = HashSet::new();
        if depth > self.max_depth {
            return Ok(refs);
        }
        for uri in [module.amends.as_deref(), module.extends.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some((source, source_path)) = self.load_module_source(uri, path).await?
                && let Ok(tokens) = lexer::lex_named(&source, &source_path)
                && let Ok(base_module) = parser::parse_named(&tokens, &source, &source_path)
            {
                refs.extend(referenced_roots(&base_module.body));
                refs.extend(
                    self.inherited_reference_roots(
                        &base_module,
                        Path::new(&source_path),
                        depth + 1,
                    )
                    .await?,
                );
            }
        }
        Ok(refs)
    }

    async fn load_module_source(
        &mut self,
        uri: &str,
        path: &Path,
    ) -> Result<Option<(String, String)>> {
        let resolved = resolve_remote_relative(path, uri);
        let uri = resolved.as_deref().unwrap_or(uri);
        if uri.starts_with("https://") || uri.starts_with("http://") {
            let source = self.fetch_source(uri).await?;
            return Ok(Some((source, uri.to_string())));
        }
        if uri.starts_with("package://") {
            let pkg = resolve_package_uri(uri)?;
            match &pkg {
                PackageSource::Direct { url, root } => {
                    let source = self.fetch_direct_package_source(url, root).await?;
                    return Ok(Some((source, url.clone())));
                }
                PackageSource::Zip(zip_url, entry) => {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url).await?;
                        let local_path = pkg_dir.join(entry);
                        let source = self.capabilities.read_to_string(&local_path).await?;
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
        let import_path = if let Some(rel) = uri.strip_prefix("file://") {
            PathBuf::from(rel)
        } else {
            self.resolve_local_path(path, uri)
        };
        if !self.capabilities.path_exists(&import_path).await? {
            return Ok(None);
        }
        let source = self.capabilities.read_to_string(&import_path).await?;
        Ok(Some((source, import_path.display().to_string())))
    }

    async fn fetch_package_source(&mut self, url: &str) -> Result<String> {
        if let Some(cached) = self.http_cache.get(url) {
            return Ok(cached.clone());
        }
        let bytes = self.fetch_package_bytes(url, "pkl").await?;
        let source = String::from_utf8(bytes)
            .map_err(|error| Error::Eval(format!("package source is not UTF-8: {url}: {error}")))?;
        self.http_cache.insert(url.to_string(), source.clone());
        Ok(source)
    }

    async fn fetch_direct_package_source(&mut self, url: &str, root: &str) -> Result<String> {
        self.package_http_roots.insert(root.to_string());
        self.fetch_package_source(url).await
    }

    async fn fetch_package_bytes(&mut self, url: &str, extension: &str) -> Result<Vec<u8>> {
        match self.read_package_cache(url, extension) {
            Ok(Some(bytes)) => match validate_package_bytes(url, extension, &bytes) {
                Ok(()) => return Ok(bytes),
                Err(error) if self.offline => return Err(error),
                Err(_) => self.remove_package_cache(url, extension),
            },
            Ok(None) => {}
            Err(error) if self.offline => return Err(error),
            Err(_) => {
                // The persistent cache is an optimization while online. If it
                // is unreadable, fetch from the network and continue without it.
            }
        }
        if self.offline {
            let cache = self
                .package_cache_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<disabled>".to_string());
            return Err(Error::Eval(format!(
                "package is not cached and offline mode is enabled: {url} (cache: {cache})"
            )));
        }
        let fetch_url = self.rewrite_url(url).into_owned();
        let bytes = self.capabilities.fetch_bytes(&fetch_url).await?;
        validate_package_bytes(url, extension, &bytes)?;
        // Best-effort: the bytes are already in hand.
        let _ = self.write_package_cache(url, extension, &bytes);
        Ok(bytes)
    }

    fn read_package_cache(&self, url: &str, extension: &str) -> Result<Option<Vec<u8>>> {
        let Some(cache_dir) = &self.package_cache_dir else {
            return Ok(None);
        };
        let (data_path, url_path) = package_cache_paths(cache_dir, url, extension);
        let cached_url = match std::fs::read_to_string(&url_path) {
            Ok(cached_url) => cached_url,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::Io(url_path, error)),
        };
        if cached_url != url {
            return Ok(None);
        }
        match std::fs::read(&data_path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Io(data_path, error)),
        }
    }

    /// Cache `bytes` for `url`. The fetch path ignores the error; preloading does not.
    fn write_package_cache(&self, url: &str, extension: &str, bytes: &[u8]) -> Result<()> {
        let Some(cache_dir) = &self.package_cache_dir else {
            return Ok(());
        };
        let (data_path, url_path) = package_cache_paths(cache_dir, url, extension);
        let Some(parent) = data_path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent).map_err(|error| Error::Io(parent.to_path_buf(), error))?;
        write_atomic(&data_path, bytes).map_err(|error| Error::Io(data_path, error))?;
        write_atomic(&url_path, url.as_bytes()).map_err(|error| Error::Io(url_path, error))
    }

    /// Seed the persistent package cache with `bytes` for `url`.
    ///
    /// `extension` is `"zip"` for archive packages and `"pkl"` for direct file
    /// downloads. A valid cache entry already present wins, so preloading never
    /// overrides content fetched from the network. Does nothing when no package
    /// cache directory is configured.
    pub fn preload_package(&self, url: &str, extension: &str, bytes: &[u8]) -> Result<()> {
        if self.package_cache_dir.is_none() {
            return Ok(());
        }
        if let Ok(Some(cached)) = self.read_package_cache(url, extension)
            && validate_package_bytes(url, extension, &cached).is_ok()
        {
            return Ok(());
        }
        validate_package_bytes(url, extension, bytes)?;
        self.write_package_cache(url, extension, bytes)
    }

    fn remove_package_cache(&self, url: &str, extension: &str) {
        let Some(cache_dir) = &self.package_cache_dir else {
            return;
        };
        let (data_path, url_path) = package_cache_paths(cache_dir, url, extension);
        let _ = std::fs::remove_file(data_path);
        let _ = std::fs::remove_file(url_path);
    }

    /// Download a package zip and extract it to a temp directory.
    /// Returns the path to the extracted directory. Caches by zip URL.
    #[cfg(feature = "package-zip")]
    async fn extract_package_zip(&mut self, zip_url: &str) -> Result<PathBuf> {
        // Check if already extracted
        if let Some(dir) = self.package_dirs.get(zip_url) {
            return Ok(dir.clone());
        }
        let bytes = self.fetch_package_bytes(zip_url, "zip").await?;
        let cursor = std::io::Cursor::new(bytes);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| Error::Eval(format!("zip error: {e}")))?;
        let dir = self
            .capabilities
            .temp_dir(&format!("pklr-pkg-{}", self.package_dirs.len()))
            .await?;
        archive
            .extract(&dir)
            .map_err(|e| Error::Eval(format!("zip extract error: {e}")))?;
        self.package_dirs.insert(zip_url.to_string(), dir.clone());
        Ok(dir)
    }

    #[cfg(all(test, feature = "package-zip"))]
    fn package_dir_for_zip(&self, zip_url: &str) -> Option<&PathBuf> {
        self.package_dirs.get(zip_url)
    }

    pub async fn eval_source(&mut self, source: &str, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        self.converters.clear();
        // Seed import cache for the entry file so circular back-references work
        if let Ok(canonical) = self.capabilities.canonicalize(path).await {
            self.import_cache
                .insert(canonical, Value::Object(Arc::new(IndexMap::new()), None));
        }
        let name = path.display().to_string();
        let tokens = lexer::lex_named(source, &name)?;
        let module = parser::parse_named(&tokens, source, &name)?;
        let val = self.eval_module(&module, path, 0).await?;
        // Update cache with real value
        if let Ok(canonical) = self.capabilities.canonicalize(path).await {
            self.import_cache.insert(canonical, val.clone());
        }
        Ok(val)
    }

    /// Evaluate a local pkl file by path (public entry point).
    pub async fn eval_file_pub(&mut self, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        self.eval_file(path, 0).await
    }

    /// Read, lex, parse, and evaluate a local file (with caching).
    /// Inserts a placeholder before evaluation to break circular imports.
    async fn eval_file(&mut self, path: &Path, depth: usize) -> Result<Value> {
        let canonical = self.capabilities.canonicalize(path).await?;
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

    async fn eval_file_with_requested_fields(
        &mut self,
        path: &Path,
        depth: usize,
        requested_fields: Option<HashSet<String>>,
    ) -> Result<Value> {
        if requested_fields.is_none() {
            return self.eval_file(path, depth).await;
        }
        let canonical = self.capabilities.canonicalize(path).await?;
        if let Some(cached) = self.import_cache.get(&canonical) {
            return Ok(cached.clone());
        }
        self.import_cache.insert(
            canonical.clone(),
            Value::Object(Arc::new(IndexMap::new()), None),
        );
        let result = self
            .eval_file_requested_fields_inner(path, depth, requested_fields)
            .await;
        self.import_cache.remove(&canonical);
        result
    }

    async fn eval_file_requested_fields_inner(
        &mut self,
        path: &Path,
        depth: usize,
        requested_fields: Option<HashSet<String>>,
    ) -> Result<Value> {
        let module = self.parse_file(path).await?;
        self.eval_module_with_scope(&module, path, depth, None, requested_fields)
            .await
    }

    async fn eval_file_inner(
        &mut self,
        path: &Path,
        canonical: &Path,
        depth: usize,
    ) -> Result<Value> {
        let val = self.eval_file_inner_with_scope(path, depth, None).await?;
        self.import_cache
            .insert(canonical.to_path_buf(), val.clone());
        Ok(val)
    }

    async fn eval_file_with_scope(
        &mut self,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
    ) -> Result<Value> {
        if inherited_scope.is_none() {
            return self.eval_file(path, depth).await;
        }
        let canonical = self.capabilities.canonicalize(path).await?;
        if !self.scoped_imports_in_flight.insert(canonical.clone()) {
            return Ok(Value::Object(Arc::new(IndexMap::new()), None));
        }
        let result = self
            .eval_file_inner_with_scope(path, depth, inherited_scope)
            .await;
        self.scoped_imports_in_flight.remove(&canonical);
        result
    }

    async fn eval_file_inner_with_scope(
        &mut self,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
    ) -> Result<Value> {
        let module = self.parse_file(path).await?;
        let val = self
            .eval_module_with_scope(&module, path, depth, inherited_scope, None)
            .await?;
        Ok(val)
    }

    async fn parse_file(&mut self, path: &Path) -> Result<Module> {
        let source = self.capabilities.read_to_string(path).await?;
        let name = path.display().to_string();
        let tokens = lexer::lex_named(&source, &name)?;
        parser::parse_named(&tokens, &source, &name)
    }

    async fn eval_module(&mut self, module: &Module, path: &Path, depth: usize) -> Result<Value> {
        self.eval_module_with_scope(module, path, depth, None, None)
            .await
    }

    fn layer_evaluated_module_scope(&self, path: &Path, scope: &mut Scope) {
        let Some(inherited) = self.module_scopes.get(path) else {
            return;
        };
        for (name, value) in &inherited.values {
            if name != "this" && name != "module" {
                scope.set(name.clone(), value.clone());
            }
        }
        for (name, ty) in &inherited.type_aliases {
            scope.set_type_alias(name.clone(), ty.clone());
        }
    }

    fn inherited_late_properties(&self, path: &Path) -> Vec<Property> {
        self.module_scopes
            .get(path)
            .map(|snapshot| snapshot.late_properties.clone())
            .unwrap_or_default()
    }

    #[async_recursion(?Send)]
    async fn eval_module_with_scope(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
        requested_fields: Option<HashSet<String>>,
    ) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval(format!(
                "max import depth {} exceeded",
                self.max_depth
            )));
        }
        if module
            .body
            .iter()
            .any(|entry| matches!(entry, Entry::Elem(_)))
        {
            return Err(Error::Eval("Invalid property definition".into()));
        }
        let mut scope = Scope::default();
        seed_builtins(&mut scope);
        if let Some(inherited_scope) = inherited_scope {
            for (key, value) in inherited_scope.flatten() {
                scope.set(key, value);
            }
            for (key, ty) in inherited_scope.flatten_type_aliases() {
                scope.set_type_alias(key, ty);
            }
        }
        let requested_output_fields = requested_fields
            .as_ref()
            .map(|fields| expand_requested_fields(&module.body, fields));
        let analysis_entries =
            analysis_entries_for_requested_fields(&module.body, requested_output_fields.as_ref());
        let mut referenced_imports = referenced_roots(&analysis_entries);
        referenced_imports.extend(
            self.inherited_reference_roots(module, path, depth + 1)
                .await?,
        );
        let import_field_uses = import_field_uses(&analysis_entries);

        let inherited_local_paths: Vec<_> = module
            .amends
            .iter()
            .chain(module.extends.iter())
            .filter_map(|uri| local_module_path(path, uri))
            .collect();
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
                if !referenced_imports.contains(&alias) {
                    continue;
                }

                // Non-local glob imports bind an empty mapping
                if uri.contains("://") {
                    scope.set(alias, Value::Object(Arc::new(IndexMap::new()), None));
                    continue;
                }

                let base_dir = path.parent().unwrap_or(Path::new("."));
                let matched = self.capabilities.glob(base_dir, uri).await?;
                let mut mapping = IndexMap::new();
                for matched_path in matched {
                    if self.same_local_path(&matched_path, path).await? {
                        continue;
                    }
                    let rel_key = pathdiff_or_full(&matched_path, base_dir);
                    let val = self
                        .eval_file_with_requested_fields(&matched_path, depth + 1, None)
                        .await?;
                    mapping.insert(rel_key, val);
                }
                scope.set(alias, Value::Object(Arc::new(mapping), None));
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
                if !referenced_imports.contains(&alias) {
                    continue;
                }
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                let source = self.fetch_source(uri).await?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, uri)?;
                    let imp_module = parser::parse_named(&tokens, &source, uri)?;
                    self.eval_module_with_scope(
                        &imp_module,
                        Path::new(uri),
                        depth + 1,
                        None,
                        requested,
                    )
                    .await?
                };
                scope.set(alias, imported_val);
                continue;
            }

            if uri.starts_with("package://") {
                let pkg = resolve_package_uri(uri)?;
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
                if !referenced_imports.contains(&alias) {
                    continue;
                }
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                // For zip packages, extract to temp dir and eval as local file
                if let PackageSource::Zip(zip_url, _) = &pkg {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url).await?;
                        let local_path = pkg_dir.join(file_path);
                        let imported_val = self
                            .eval_file_with_requested_fields(&local_path, depth + 1, requested)
                            .await?;
                        scope.set(alias, imported_val);
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
                let source = self.fetch_direct_package_source(&url, root).await?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, &url)?;
                    let imp_module = parser::parse_named(&tokens, &source, &url)?;
                    self.eval_module_with_scope(
                        &imp_module,
                        Path::new(&url),
                        depth + 1,
                        None,
                        requested,
                    )
                    .await?
                };
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
                if !referenced_imports.contains(&alias) {
                    continue;
                }
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
                self.resolve_local_path(path, uri)
            };
            let alias = import.alias.clone().unwrap_or_else(|| {
                import_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });
            if !referenced_imports.contains(&alias) {
                // Unused imports are intentionally lazy: missing local paths are
                // reported only if the imported binding is actually referenced.
                continue;
            }
            if !self.capabilities.path_exists(&import_path).await? {
                return Err(Error::ImportNotFound(import_path.display().to_string()));
            }
            let mut inherited_path = None;
            for candidate in &inherited_local_paths {
                if self.same_local_path(candidate, &import_path).await? {
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
                let imported_val = self
                    .eval_file_with_requested_fields(&import_path, depth + 1, requested)
                    .await?;
                scope.set(alias, imported_val);
            }
        }

        // Process amends: load base module as starting values
        let mut base_obj = IndexMap::new();
        let mut late_inherited_properties = Vec::new();
        if let Some(amends_uri) = &module.amends {
            // A relative amends inside a remote module resolves against that URL.
            let resolved_amends = resolve_remote_relative(path, amends_uri);
            let uri: &str = resolved_amends.as_deref().unwrap_or(amends_uri);
            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP amends
                let source = self.fetch_source(uri).await?;
                let tokens = lexer::lex_named(&source, uri)?;
                let base_module = parser::parse_named(&tokens, &source, uri)?;
                let base_val = self
                    .eval_module_with_scope(
                        &base_module,
                        Path::new(uri),
                        depth + 1,
                        Some(scope.clone()),
                        None,
                    )
                    .await?;
                self.layer_evaluated_module_scope(Path::new(uri), &mut scope);
                late_inherited_properties.extend(self.inherited_late_properties(Path::new(uri)));
                if let Value::Object(m, _) = base_val {
                    base_obj = (*m).clone();
                }
            } else if uri.starts_with("package://") {
                let pkg = resolve_package_uri(uri)?;
                if let PackageSource::Zip(zip_url, entry) = &pkg {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url).await?;
                        let local_path = pkg_dir.join(entry);
                        let source = self.capabilities.read_to_string(&local_path).await?;
                        let name = local_path.display().to_string();
                        let tokens = lexer::lex_named(&source, &name)?;
                        let base_module = parser::parse_named(&tokens, &source, &name)?;
                        let base_val = self
                            .eval_module_with_scope(
                                &base_module,
                                &local_path,
                                depth + 1,
                                Some(scope.clone()),
                                None,
                            )
                            .await?;
                        self.layer_evaluated_module_scope(&local_path, &mut scope);
                        late_inherited_properties
                            .extend(self.inherited_late_properties(&local_path));
                        if let Value::Object(m, _) = base_val {
                            base_obj = (*m).clone();
                        }
                    }
                    #[cfg(not(feature = "package-zip"))]
                    {
                        let _ = entry;
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                } else if let PackageSource::Direct { url, root } = &pkg {
                    let source = self.fetch_direct_package_source(url, root).await?;
                    let tokens = lexer::lex_named(&source, url)?;
                    let base_module = parser::parse_named(&tokens, &source, url)?;
                    let base_val = self
                        .eval_module_with_scope(
                            &base_module,
                            Path::new(url.as_str()),
                            depth + 1,
                            Some(scope.clone()),
                            None,
                        )
                        .await?;
                    self.layer_evaluated_module_scope(Path::new(url.as_str()), &mut scope);
                    late_inherited_properties
                        .extend(self.inherited_late_properties(Path::new(url.as_str())));
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
                    self.resolve_local_path(path, uri)
                };
                if self.capabilities.path_exists(&amends_path).await? {
                    let base_val = self
                        .eval_file_with_scope(&amends_path, depth + 1, Some(scope.clone()))
                        .await?;
                    self.layer_evaluated_module_scope(&amends_path, &mut scope);
                    late_inherited_properties.extend(self.inherited_late_properties(&amends_path));
                    if let Value::Object(m, _) = &base_val {
                        self.bind_deferred_inherited_imports(
                            &deferred_inherited_imports,
                            &amends_path,
                            &base_val,
                            &mut scope,
                        )
                        .await?;
                        base_obj = (**m).clone();
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
        if let Some(amends_uri) = &module.amends {
            let resolved_amends = resolve_remote_relative(path, amends_uri);
            let uri: &str = resolved_amends.as_deref().unwrap_or(amends_uri);
            let base_source = self
                .load_module_source(uri, path)
                .await?
                .map(|(source, _)| source);
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
                    if let Entry::Property(prop) = entry
                        && !has_modifier(&prop.modifiers, Modifier::Local)
                        && prop.name != "output"
                    {
                        late_inherited_properties.push(prop.clone());
                    }
                }
            }
            // Remove function values from base output (not data)
            base_obj.retain(|_, v| !matches!(v, Value::Lambda(..)));
        }

        // Process extends: load base module, inherit all members and scope
        if let Some(extends_uri) = &module.extends {
            // A relative extends inside a remote module resolves against that URL.
            let resolved_extends = resolve_remote_relative(path, extends_uri);
            let uri: &str = resolved_extends.as_deref().unwrap_or(extends_uri);
            if !uri.contains("://") || uri.starts_with("file://") {
                let extends_path = if let Some(rel) = uri.strip_prefix("file://") {
                    PathBuf::from(rel)
                } else {
                    self.resolve_local_path(path, uri)
                };
                if self.capabilities.path_exists(&extends_path).await? {
                    let ext_val = self
                        .eval_file_with_scope(&extends_path, depth + 1, Some(scope.clone()))
                        .await?;
                    self.layer_evaluated_module_scope(&extends_path, &mut scope);
                    late_inherited_properties.extend(self.inherited_late_properties(&extends_path));
                    let name = extends_path.display().to_string();
                    let source = self.capabilities.read_to_string(&extends_path).await?;
                    let tokens = lexer::lex_named(&source, &name)?;
                    let ext_module = parser::parse_named(&tokens, &source, &name)?;
                    if let Value::Object(m, _) = &ext_val {
                        self.bind_deferred_inherited_imports(
                            &deferred_inherited_imports,
                            &extends_path,
                            &ext_val,
                            &mut scope,
                        )
                        .await?;
                        base_obj = (**m).clone();
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
                            Entry::Property(prop)
                                if !has_modifier(&prop.modifiers, Modifier::Local)
                                    && prop.name != "output" =>
                            {
                                late_inherited_properties.push(prop.clone());
                                if let Ok(Some(value)) =
                                    self.eval_property(prop, &scope, depth).await
                                {
                                    scope.set(prop.name.clone(), value);
                                }
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
                    .eval_module_with_scope(
                        &ext_module,
                        Path::new(uri),
                        depth + 1,
                        Some(scope.clone()),
                        None,
                    )
                    .await?;
                self.layer_evaluated_module_scope(Path::new(uri), &mut scope);
                late_inherited_properties.extend(self.inherited_late_properties(Path::new(uri)));
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
                    if let Entry::Property(prop) = entry
                        && !has_modifier(&prop.modifiers, Modifier::Local)
                        && prop.name != "output"
                    {
                        late_inherited_properties.push(prop.clone());
                        if let Ok(Some(value)) = self.eval_property(prop, &scope, depth).await {
                            scope.set(prop.name.clone(), value);
                        }
                    }
                }
            }
        }

        let requested_eval_fields = requested_output_fields.as_ref().map(|fields| {
            let mut dependency_entries = late_inherited_properties
                .iter()
                .cloned()
                .map(Entry::Property)
                .collect::<Vec<_>>();
            dependency_entries.extend(module.body.iter().cloned());
            expand_requested_fields(&dependency_entries, fields)
        });

        // Locals are evaluated before the main property pass, but `this` and
        // `module` must already expose inherited members at that point.
        let inherited_snapshot = Value::Object(Arc::new(base_obj.clone()), None);
        scope.set("this".into(), inherited_snapshot.clone());
        scope.set("module".into(), inherited_snapshot);

        // First pass: collect locals, class definitions, and type aliases in
        // declaration order so they can reference each other
        for entry in &module.body {
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    match self
                        .eval_expr(prop.value.as_ref().unwrap(), &scope, depth)
                        .await
                    {
                        Ok(val) => scope.set(prop.name.clone(), val),
                        Err(Error::Eval(message)) => scope.poison(prop.name.clone(), message),
                        Err(error) => return Err(error),
                    }
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
                    if let Some(v) = out.get(&prop.name) {
                        // Satisfied by base — add to scope so other properties can reference it
                        scope.set(prop.name.clone(), v.clone());
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
                let val = match self.eval_property(prop, &scope, depth).await {
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
                    Err(error) => return Err(error),
                };
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
                    if !has_modifier(mods, Modifier::Hidden)
                        && (depth > 0 || should_render_property_value(prop, &v))
                        && requested_output_fields
                            .as_ref()
                            .is_none_or(|fields| fields.contains(&prop.name))
                    {
                        out.insert(prop.name.clone(), v);
                    }
                    // Update `this` and `module` with all properties (including hidden)
                    let snapshot = Value::Object(Arc::new(all_props.clone()), None);
                    scope.set("this".into(), snapshot.clone());
                    scope.set("module".into(), snapshot);
                }
            }
        }

        // Pkl properties are late-bound. Re-evaluate inherited expressions after
        // child overrides have populated the scope (for example `uses` derived
        // from an action module's overridden `action` and `version`).
        let child_property_names: HashSet<&str> = module
            .body
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some(prop.name.as_str()),
                _ => None,
            })
            .collect();
        if depth == 0
            && !module_is_abstract(module)
            && let Some(prop) = late_inherited_properties.iter().find(|prop| {
                has_modifier(&prop.modifiers, Modifier::Abstract)
                    && prop.value.is_none()
                    && prop.body.is_none()
                    && !child_property_names.contains(prop.name.as_str())
            })
        {
            return Err(Error::Eval(format!(
                "abstract property '{}' must be assigned a value in {}",
                prop.name,
                path.display()
            )));
        }
        let child_candidates = module
            .body
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop)
                    if prop.name != "output" && (prop.value.is_some() || prop.body.is_some()) =>
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
        let mut late_child_properties = Vec::new();
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
        for pass in 0..=late_passes {
            for prop in &late_inherited_properties {
                if child_property_names.contains(prop.name.as_str()) {
                    continue;
                }
                match self.eval_property(prop, &scope, depth).await {
                    Ok(Some(value)) => {
                        scope.set(prop.name.clone(), value.clone());
                        all_props.insert(prop.name.clone(), value.clone());
                        if !has_modifier(&prop.modifiers, Modifier::Hidden)
                            && (depth > 0 || should_render_property_value(prop, &value))
                            && requested_output_fields
                                .as_ref()
                                .is_none_or(|fields| fields.contains(&prop.name))
                        {
                            out.insert(prop.name.clone(), value);
                        }
                        let snapshot = Value::Object(Arc::new(all_props.clone()), None);
                        scope.set("this".into(), snapshot.clone());
                        scope.set("module".into(), snapshot);
                    }
                    Ok(None) => {}
                    Err(Error::Eval(message))
                        if pass < late_passes
                            || (module_is_abstract(module)
                                && is_unresolved_template_error(&message)) => {}
                    Err(error) => return Err(error),
                }
            }
            for prop in &late_child_properties {
                match self.eval_property(prop, &scope, depth).await {
                    Ok(Some(value)) => {
                        scope.set(prop.name.clone(), value.clone());
                        if has_modifier(&prop.modifiers, Modifier::Local) {
                            continue;
                        }
                        all_props.insert(prop.name.clone(), value.clone());
                        if !has_modifier(&prop.modifiers, Modifier::Hidden)
                            && (depth > 0 || should_render_property_value(prop, &value))
                            && requested_output_fields
                                .as_ref()
                                .is_none_or(|fields| fields.contains(&prop.name))
                        {
                            out.insert(prop.name.clone(), value);
                        }
                        let snapshot = Value::Object(Arc::new(all_props.clone()), None);
                        scope.set("this".into(), snapshot.clone());
                        scope.set("module".into(), snapshot);
                    }
                    Ok(None) => {}
                    Err(Error::Eval(message))
                        if pass < late_passes
                            || (module_is_abstract(module)
                                && is_unresolved_template_error(&message)) => {}
                    Err(error) => return Err(error),
                }
            }
        }

        if depth == 0 {
            for prop in &late_inherited_properties {
                if let Some(value) = all_props.get(&prop.name)
                    && !should_render_property_value(prop, value)
                    && !child_property_names.contains(prop.name.as_str())
                {
                    out.shift_remove(&prop.name);
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
        if let Some(fields) = &requested_output_fields {
            out.retain(|name, _| fields.contains(name));
        }
        // If the module declares any `@Deprecated` properties, attach a
        // minimal ObjectSource carrying just the deprecation map so field
        // access can warn lazily. Modules without @Deprecated keep `None`
        // source to avoid changing amend behavior in the common case.
        let deprecated = collect_deprecated(&module.body);
        let source = if deprecated.is_empty() {
            None
        } else {
            Some(Arc::new(ObjectSource {
                entries: Vec::new(),
                scope: IndexMap::new(),
                is_open: true,
                type_name: None,
                parent_type_names: Vec::new(),
                mapping_value_types: Vec::new(),
                deprecated,
            }))
        };
        let mut effective_late_properties = IndexMap::new();
        for prop in late_inherited_properties
            .iter()
            .chain(module.body.iter().filter_map(|entry| match entry {
                Entry::Property(prop)
                    if !has_modifier(&prop.modifiers, Modifier::Local) && prop.name != "output" =>
                {
                    Some(prop)
                }
                _ => None,
            }))
        {
            effective_late_properties.insert(prop.name.clone(), prop.clone());
        }
        self.module_scopes.insert(
            path.to_path_buf(),
            ModuleScopeSnapshot {
                values: scope.flatten(),
                type_aliases: scope.flatten_type_aliases(),
                late_properties: effective_late_properties.into_values().collect(),
            },
        );
        Ok(Value::Object(Arc::new(out), source))
    }

    #[async_recursion(?Send)]
    async fn eval_property(
        &mut self,
        prop: &Property,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Some(expr) = &prop.value {
            let mut value = self.eval_expr(expr, scope, depth).await?;
            apply_mapping_type_annotation(&mut value, prop.type_ann.as_ref());
            return Ok(Some(value));
        }
        if let Some(body) = &prop.body {
            let mut nullable_default = prop
                .type_ann
                .as_ref()
                .and_then(|ty| nullable_inner_default(ty, scope));
            if let Some(default) = &mut nullable_default {
                apply_mapping_type_annotation(default, prop.type_ann.as_ref());
            }
            let scoped = scope.get(&prop.name);
            let amendment_base = scoped
                .filter(|value| !is_null_value(value))
                .or(nullable_default.as_ref())
                .or(scoped);
            if matches!(amendment_base, Some(Value::List(_)))
                || (!matches!(amendment_base, Some(Value::Object(..)))
                    && (prop.type_ann.as_ref().is_some_and(type_is_listing)
                        || entries_are_listing_amendment(body)))
            {
                let mut items = match amendment_base {
                    Some(Value::List(existing)) => existing.clone(),
                    _ => Vec::new(),
                };
                self.eval_listing_entries(body, scope, depth, &mut items)
                    .await?;
                return Ok(Some(Value::List(items)));
            }
            // `foo { ... }` — object body amendment.
            // If the property already has a value in scope (e.g., from a base class),
            // amend that value so its ObjectSource (type info, default template) is preserved.
            if let Some(Value::Object(existing_map, Some(src))) = amendment_base {
                if !src.mapping_value_types.is_empty() {
                    // Mapping ObjectSource entries are mapping body entries such as
                    // `default` and dynamic keys. Rebuild the entry map with the
                    // type-aware evaluator so single-type and union mappings both keep
                    // mapping defaults plus converter type metadata after amendment.
                    let mut type_scope = Scope::default();
                    for (key, value) in &src.scope {
                        type_scope.set(key.clone(), value.clone());
                    }
                    for (key, value) in scope.flatten() {
                        type_scope.set(key, value);
                    }
                    for (key, ty) in scope.flatten_type_aliases() {
                        type_scope.set_type_alias(key, ty);
                    }
                    let value_type_defaults = src
                        .mapping_value_types
                        .iter()
                        .filter_map(|name| {
                            resolve_dotted(&type_scope, name).map(|value| (name.clone(), value))
                        })
                        .collect::<Vec<_>>();
                    let inherited_default = self
                        .find_default_template(&src.entries, &type_scope, depth)
                        .await?;
                    let mut amended = IndexMap::new();
                    self.eval_mapping_entries_with_type_default(
                        &src.entries,
                        &type_scope,
                        depth,
                        &mut amended,
                        &value_type_defaults,
                        MappingInheritedDefault::default(),
                    )
                    .await?;
                    amended.extend(existing_map.iter().map(|(k, v)| (k.clone(), v.clone())));
                    self.eval_mapping_entries_with_type_default(
                        body,
                        &type_scope,
                        depth,
                        &mut amended,
                        &value_type_defaults,
                        MappingInheritedDefault {
                            value: inherited_default,
                            entries: find_default_body_entries(&src.entries),
                        },
                    )
                    .await?;
                    return Ok(Some(Value::Object(
                        Arc::new(amended),
                        Some(Arc::clone(src)),
                    )));
                }
                let base_entries = src.entries.clone();
                let base_scope = src.scope.clone();
                let base_type_name = src.type_name.clone();
                return Ok(Some(
                    self.eval_amended_object(
                        &base_entries,
                        &base_scope,
                        body,
                        scope,
                        depth,
                        base_type_name.map(|name| (name, src.parent_type_names.clone())),
                    )
                    .await?,
                ));
            }
            let val = self.eval_entries(body, scope, depth).await?;
            return Ok(Some(val));
        }
        if let Some(ty) = &prop.type_ann {
            let mut default = type_default_value(ty, scope);
            if let Some(default) = &mut default {
                apply_mapping_type_annotation(default, Some(ty));
            }
            if default.is_none()
                && matches!(ty, crate::parser::TypeExpr::Union(variants) if !variants.iter().any(is_default_type))
            {
                return Err(Error::Eval(format!(
                    "union property '{}' has no selected default",
                    prop.name
                )));
            }
            return Ok(default);
        }
        Ok(None)
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
        // Class-as-a-function definitions commonly use `local self = this` so
        // output properties can close over the amended instance. Bind `this`
        // before locals are evaluated, then keep direct aliases synchronized as
        // properties populate the instance.
        let mut all_props: IndexMap<String, Value> = IndexMap::new();
        let mut this_aliases = Vec::new();
        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
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
                    // Bind every local in declaration order so later locals and
                    // entries can reference it (e.g. a non-lambda local that
                    // calls a lambda local defined just above it).
                    match self.eval_expr(expr, &child_scope, depth).await {
                        Ok(val) => {
                            child_scope.set(prop.name.clone(), val);
                            if matches!(expr, Expr::Ident(name) if name == "this" || this_aliases.contains(name))
                            {
                                this_aliases.push(prop.name.clone());
                            }
                        }
                        Err(Error::Eval(message)) => child_scope.poison(prop.name.clone(), message),
                        Err(error) => return Err(error),
                    }
                    if matches!(expr, crate::parser::Expr::Lambda(..)) {
                        // Lambda evaluation only captures the current scope; it
                        // does not run the body. Bind once for declaration-order
                        // visibility, then re-bind after properties for late
                        // binding of overrides.
                        deferred_lambdas.push((prop.name.clone(), expr));
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
        for entry in entries {
            match entry {
                Entry::Property(prop) => {
                    let mods = &prop.modifiers;
                    if has_modifier(mods, Modifier::Local) {
                        continue;
                    }
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
                    refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                    if let Some(v) = self.eval_property(prop, &child_scope, depth).await? {
                        child_scope.set(prop.name.clone(), v.clone());
                        all_props.insert(prop.name.clone(), v.clone());
                        if !has_modifier(mods, Modifier::Hidden) {
                            map.insert(prop.name.clone(), v);
                        }
                        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
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
                                src.type_name
                                    .clone()
                                    .map(|name| (name, src.parent_type_names.clone())),
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
                                    parent_type_names: Vec::new(),
                                    mapping_value_types: Vec::new(),
                                    deprecated: merge_deprecated(&src.deprecated, body),
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
                    all_props.insert(key_str.clone(), val.clone());
                    map.insert(key_str, val);
                    refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                }
                Entry::Spread(expr) => {
                    let val = self.eval_expr(expr, &child_scope, depth).await?;
                    if let Value::Object(m, _) = val {
                        all_props.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                        map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
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
                            all_props.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        }
                    }
                }
                Entry::WhenGenerator(wgen) => {
                    let cond = self.eval_expr(&wgen.condition, &child_scope, depth).await?;
                    if is_truthy(&cond) {
                        let body_val = self.eval_entries(&wgen.body, &child_scope, depth).await?;
                        if let Value::Object(m, _) = body_val {
                            all_props.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        }
                    } else if let Some(else_body) = &wgen.else_body {
                        let else_val = self.eval_entries(else_body, &child_scope, depth).await?;
                        if let Value::Object(m, _) = else_val {
                            all_props.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        }
                    }
                }
                Entry::Elem(_) => {} // bare elements only valid in Listing bodies
                Entry::ClassDef(..) | Entry::TypeAlias(..) => {} // handled in scope setup
            }
        }
        // Evaluate deferred local lambdas (function definitions) AFTER all
        // properties so they capture overridden values (late binding).
        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
        for (name, expr) in deferred_lambdas {
            let val = self.eval_expr(expr, &child_scope, depth).await?;
            child_scope.set(name, val);
        }
        let source = ObjectSource {
            entries: entries.to_vec(),
            scope: child_scope.flatten(),
            is_open: true, // default: allow new properties
            type_name: None,
            parent_type_names: Vec::new(),
            mapping_value_types: Vec::new(),
            deprecated: collect_deprecated(entries),
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
        let parent_type_names = match &parent_val {
            Some(Value::Object(_, Some(source))) => source
                .type_name
                .iter()
                .cloned()
                .chain(source.parent_type_names.iter().cloned())
                .collect(),
            _ => Vec::new(),
        };

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
                let data_property_names = body
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::Property(prop)
                            if !has_modifier(&prop.modifiers, Modifier::Local) =>
                        {
                            Some(prop.name.as_str())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let schema_member_names = body
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::ClassDef(name, ..)
                            if !data_property_names.contains(name.as_str()) =>
                        {
                            Some(name.as_str())
                        }
                        Entry::Property(prop)
                            if matches!(prop.value, Some(Expr::Lambda(..)))
                                && has_modifier(&prop.modifiers, Modifier::Local)
                                && !data_property_names.contains(prop.name.as_str()) =>
                        {
                            Some(prop.name.as_str())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let mut map = map;
                Arc::make_mut(&mut map)
                    .retain(|key, _| !schema_member_names.contains(key.as_str()));
                let mut new_src = (*src).clone();
                new_src.is_open = is_open;
                new_src.type_name = Some(class_name.to_string());
                new_src.parent_type_names = parent_type_names;
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
                self.bind_type_alias_value(name, base, scope);
            }
            // Union types, generics, etc. -- no runtime representation needed
            _ => {}
        }
    }

    fn bind_type_alias_value(&self, name: &str, target: &str, scope: &mut Scope) {
        if let Some(val) = scope.get(target.trim_end_matches('?')).cloned() {
            scope.set(name.to_string(), val);
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
                if let Some(expected) = string_literal_type_value(name) {
                    return Ok(matches!(val, Value::String(actual) if actual == expected));
                }
                // Check if name is a type alias; if so, resolve to the aliased type
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_type_check(val, &resolved, scope, depth + 1).await;
                }
                // Otherwise, plain type check
                Ok(value_is_type(val, ty))
            }
            TypeExpr::Constrained(base, constraint) => {
                // First check the base type
                if !value_is_named_type(val, base) {
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
                    Value::Object(items, _) => {
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
                if is_null_value(val) {
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
        base_type: Option<(String, Vec<String>)>,
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
                    && (prop.value.is_some() || prop.body.is_some() || prop.type_ann.is_some())
                {
                    // Body amendment: keep base entry (for its value) AND overlay
                    // entry (for body amendment). eval_property will see the base
                    // value in scope and amend it.
                    merged.push(entry.clone());
                }
                let mut replacement = (*replacement).clone();
                if let Entry::Property(overlay_prop) = &mut replacement
                    && overlay_prop.type_ann.is_none()
                {
                    overlay_prop.type_ann = prop.type_ann.clone();
                }
                merged.push(replacement);
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
        let result = self.eval_entries(&merged, &eval_scope, depth + 1).await?;
        let assigned_property_names = merged
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) if prop.value.is_some() || prop.body.is_some() => {
                    Some(prop.name.as_str())
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        if let Value::Object(map, source) = &result {
            for entry in base_entries {
                let Entry::Property(prop) = entry else {
                    continue;
                };
                let Some(type_ann) = &prop.type_ann else {
                    continue;
                };
                let value = map.get(&prop.name).or_else(|| {
                    assigned_property_names
                        .contains(prop.name.as_str())
                        .then(|| {
                            source
                                .as_ref()
                                .and_then(|source| source.scope.get(&prop.name))
                        })
                        .flatten()
                });
                let Some(value) = value else {
                    continue;
                };
                if type_is_runtime_checkable(type_ann, &eval_scope)
                    && !self
                        .eval_type_check(value, type_ann, &eval_scope, depth + 1)
                        .await?
                {
                    return Err(Error::Eval(format!(
                        "property '{}' expected {}, got {}",
                        prop.name,
                        display_type_expr(type_ann),
                        value_type_name(value)
                    )));
                }
            }
        }
        // Amending an object preserves its class identity (so `is Foo` and
        // output converters still match). eval_entries does not know the base
        // type, so re-tag the result here.
        match (base_type, result) {
            (Some((type_name, parent_type_names)), Value::Object(map, Some(src))) => {
                let mut new_src = (*src).clone();
                new_src.type_name = Some(type_name);
                new_src.parent_type_names = parent_type_names;
                Ok(Value::Object(map, Some(Arc::new(new_src))))
            }
            (_, other) => Ok(other),
        }
    }

    #[async_recursion(?Send)]
    async fn eval_object_body_over_template(
        &mut self,
        template_map: &Arc<IndexMap<String, Value>>,
        template_src: &Arc<ObjectSource>,
        body: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let mut template_scope = scope.child();
        for (key, value) in template_map.iter() {
            template_scope.set(key.clone(), value.clone());
        }
        let overlay = self.eval_entries(body, &template_scope, depth + 1).await?;
        Ok(merge_values(
            Value::Object(Arc::clone(template_map), Some(Arc::clone(template_src))),
            overlay,
        ))
    }

    #[async_recursion(?Send)]
    async fn eval_listing_entries(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        items: &mut Vec<Value>,
    ) -> Result<()> {
        let mut listing_scope = scope.child();
        for entry in entries {
            match entry {
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {
                    if let Some(expr) = &prop.value {
                        let value = self.eval_expr(expr, &listing_scope, depth + 1).await?;
                        listing_scope.set(prop.name.clone(), value);
                    }
                }
                Entry::Elem(expr) => {
                    items.push(self.eval_expr(expr, &listing_scope, depth + 1).await?)
                }
                Entry::Property(prop) if prop.value.is_some() => {
                    let value = self
                        .eval_expr(prop.value.as_ref().unwrap(), &listing_scope, depth + 1)
                        .await?;
                    listing_scope.set(prop.name.clone(), value);
                }
                Entry::DynProperty(index, value) => {
                    let index = self.eval_expr(index, &listing_scope, depth + 1).await?;
                    let Value::Int(index) = index else {
                        return Err(Error::Eval(
                            "listing index amendment requires an Int index".into(),
                        ));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| Error::Eval("listing index cannot be negative".into()))?;
                    let value = if index < items.len()
                        && let Expr::ObjectBody(entries) = value
                    {
                        self.eval_value_amendment(
                            items[index].clone(),
                            entries,
                            &listing_scope,
                            depth + 1,
                        )
                        .await?
                    } else {
                        self.eval_expr(value, &listing_scope, depth + 1).await?
                    };
                    if index < items.len() {
                        items[index] = value;
                    } else if index == items.len() {
                        items.push(value);
                    } else {
                        return Err(Error::Eval(format!(
                            "listing index {index} is out of bounds for length {}",
                            items.len()
                        )));
                    }
                }
                Entry::Spread(expr) => match self.eval_expr(expr, &listing_scope, depth + 1).await?
                {
                    Value::List(values) => items.extend(values),
                    Value::Object(values, _) => items.extend(values.values().cloned()),
                    value => items.push(value),
                },
                Entry::ForGenerator(generator) => {
                    let collection = self
                        .eval_expr(&generator.collection, &listing_scope, depth + 1)
                        .await?;
                    for (key, value) in collection_to_items(collection) {
                        let mut iter_scope = listing_scope.child();
                        iter_scope.set(generator.val_var.clone(), value);
                        if let Some(key_var) = &generator.key_var {
                            iter_scope.set(key_var.clone(), key);
                        }
                        self.eval_listing_entries(&generator.body, &iter_scope, depth + 1, items)
                            .await?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition = self
                        .eval_expr(&generator.condition, &listing_scope, depth + 1)
                        .await?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref()
                    };
                    if let Some(selected) = selected {
                        self.eval_listing_entries(selected, &listing_scope, depth + 1, items)
                            .await?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
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
            Expr::Ident(name) => scope.get(name).cloned().ok_or_else(|| {
                Error::Eval(
                    scope
                        .poison_of(name)
                        .cloned()
                        .unwrap_or_else(|| format!("undefined variable: {name}")),
                )
            }),
            Expr::Lambda(params, body) => {
                // Capture current scope values (Arc-wrapped for O(1) clone)
                let captured = Arc::new(scope.flatten());
                let mut refs = HashSet::new();
                let shadows = params.iter().cloned().collect::<HashSet<_>>();
                collect_expr_refs(body, &mut refs, &shadows);
                let captured_body = refs
                    .iter()
                    .filter(|name| scope.get(name).is_none())
                    .find_map(|name| scope.poison_of(name))
                    .map(|message| Expr::Throw(Box::new(Expr::String(message.clone()))))
                    .unwrap_or_else(|| (**body).clone());
                Ok(Value::Lambda(params.clone(), captured_body, captured))
            }
            Expr::New(type_name, entries, generic_params) => {
                match type_name.as_deref() {
                    Some("Listing") => {
                        let mut items = Vec::new();
                        self.eval_listing_entries(entries, scope, depth + 1, &mut items)
                            .await?;
                        Ok(Value::List(items))
                    }
                    Some("Mapping") | Some("Map") => {
                        // If the Mapping has a value type param (e.g., Mapping<String, Step>),
                        // resolve it as a default template so entries inherit the class type.
                        let value_type_defaults = generic_params
                            .iter()
                            .skip(1)
                            .filter_map(|name| {
                                resolve_dotted(scope, name).map(|value| (name.clone(), value))
                            })
                            .collect::<Vec<_>>();
                        let mut map = IndexMap::new();
                        self.eval_mapping_entries_with_type_default(
                            entries,
                            scope,
                            depth,
                            &mut map,
                            &value_type_defaults,
                            MappingInheritedDefault::default(),
                        )
                        .await?;
                        // Build ObjectSource with a synthetic `default` entry so that
                        // body amendments (`steps { ["x"] { ... } }`) merge new entries
                        // with the value type class, preserving type_name for converters.
                        let mut src_entries = entries.to_vec();
                        if value_type_defaults.len() == 1
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
                        let deprecated = collect_deprecated(&src_entries);
                        let source = ObjectSource {
                            entries: src_entries,
                            scope: source_scope,
                            is_open: true,
                            type_name: None,
                            parent_type_names: Vec::new(),
                            mapping_value_types: generic_params.iter().skip(1).cloned().collect(),
                            deprecated,
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
                                    base_src
                                        .type_name
                                        .clone()
                                        .map(|name| (name, base_src.parent_type_names.clone())),
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
                                    s.parent_type_names = base_src.parent_type_names.clone();
                                    s
                                } else {
                                    ObjectSource {
                                        entries: Vec::new(),
                                        scope: IndexMap::new(),
                                        is_open,
                                        type_name: tn,
                                        parent_type_names: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&base_src.deprecated, entries),
                                    }
                                };
                                *src_slot = Some(Arc::new(new_src));
                            }
                            Ok(result)
                        } else if let Some(Value::Object(base_map, base_src)) = base {
                            // Fallback: eager merge
                            let overlay = self.eval_entries(entries, scope, depth + 1).await?;
                            let mut merged: IndexMap<String, Value> = (*base_map).clone();
                            let mut deprecated = base_src
                                .as_ref()
                                .map(|s| s.deprecated.clone())
                                .unwrap_or_default();
                            if let Value::Object(overlay_map, overlay_src) = &overlay {
                                merged.extend(
                                    overlay_map.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                                if let Some(os) = overlay_src.as_ref() {
                                    for (k, v) in &os.deprecated {
                                        deprecated.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                            let src = ObjectSource {
                                entries: Vec::new(),
                                scope: IndexMap::new(),
                                is_open: true,
                                type_name: type_name.clone(),
                                parent_type_names: Vec::new(),
                                mapping_value_types: Vec::new(),
                                deprecated,
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
                    Value::Object(map, source) => {
                        let val = map
                            .get(field)
                            .cloned()
                            .ok_or_else(|| Error::Eval(format!("field not found: {field}")))?;
                        self.warn_if_deprecated_access(source, field);
                        Ok(val)
                    }
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
                    Value::Object(map, source) => {
                        let val = map.get(field).cloned().unwrap_or(Value::Null);
                        if !is_null_value(&val) {
                            self.warn_if_deprecated_access(source, field);
                        }
                        Ok(val)
                    }
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
                        if is_null_value(&v) {
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
            if let Some(result) = self
                .eval_object_method_call(&obj, method, &evaled_args, depth)
                .await?
            {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_field_call(&obj, method, &evaled_args)? {
                return Ok(result);
            }
            return Err(Error::Eval(format!(
                "unknown method '{method}' on {}",
                value_type_name(&obj)
            )));
        }
        // Handle null-safe method calls: obj?.method(args)
        if let Expr::NullSafeField(obj_expr, method) = func_expr {
            let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
            if is_null_value(&obj) {
                return Ok(Value::Null);
            }
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
            if let Some(result) = self
                .eval_object_method_call(&obj, method, &evaled_args, depth)
                .await?
            {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_field_call(&obj, method, &evaled_args)? {
                return Ok(result);
            }
            return Err(Error::Eval(format!(
                "unknown method '{method}' on {}",
                value_type_name(&obj)
            )));
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
                        return Ok(regex_value(val));
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
            return Ok(regex_value(val));
        }

        // Plain call with no args on an object — return the object
        if args.is_empty() {
            return Ok(func_val);
        }
        Err(Error::Eval("cannot call non-function".into()))
    }

    fn eval_object_field_call(
        &mut self,
        obj: &Value,
        method: &str,
        evaled_args: &[Value],
    ) -> Result<Option<Value>> {
        if let Value::Object(map, source) = obj
            && let Some(func_val) = map.get(method).cloned()
        {
            self.warn_if_deprecated_access(source, method);
            if let Value::String(ref name) = func_val
                && name == "Regex"
                && let Some(arg) = evaled_args.first()
            {
                return Ok(Some(regex_value(arg.clone())));
            }
            if evaled_args.is_empty() {
                return Ok(Some(func_val));
            }
            return Err(Error::Eval("cannot call non-function".into()));
        }
        Ok(None)
    }

    #[async_recursion(?Send)]
    async fn eval_object_method_call(
        &mut self,
        obj: &Value,
        method: &str,
        evaled_args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Value::Object(map, _) = obj
            && let Some(Value::Lambda(params, body, captured)) = map.get(method)
        {
            let mut call_scope = Scope::default();
            for (k, v) in captured.iter() {
                call_scope.set(k.clone(), v.clone());
            }
            // Layer in all instance properties, including lambdas, so local
            // functions called by this method see overrides.
            for (k, v) in map.iter() {
                call_scope.set(k.clone(), v.clone());
            }
            call_scope.set("this".into(), obj.clone());
            for (i, param) in params.iter().enumerate() {
                if let Some(arg) = evaled_args.get(i) {
                    call_scope.set(param.clone(), arg.clone());
                }
            }
            return Ok(Some(self.eval_expr(body, &call_scope, depth + 1).await?));
        }
        Ok(None)
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
            (Value::String(s), "toBoolean") => match s.to_ascii_lowercase().as_str() {
                "true" => Ok(Some(Value::Bool(true))),
                "false" => Ok(Some(Value::Bool(false))),
                _ => Err(Error::Eval(format!("cannot convert '{s}' to Boolean"))),
            },

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
            (Value::List(items), "filterNonNull") => Ok(Some(Value::List(
                items
                    .iter()
                    .filter(|item| !is_null_value(item))
                    .cloned()
                    .collect(),
            ))),
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
            (Value::Object(map, _), "toMap" | "toMapping") => {
                Ok(Some(Value::Object(map.clone(), None)))
            }
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
            (Value::Object(map, _), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function".into()))?;
                let mut result = IndexMap::new();
                for (k, v) in map.iter() {
                    let keep = self
                        .invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)
                        .await?;
                    if is_truthy(&keep) {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
            }
            (Value::Object(..), "toList") => Ok(Some(obj.clone())),
            (Value::Object(map, _), "toDynamic") => Ok(Some(Value::Object(map.clone(), None))),

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
    async fn eval_value_amendment(
        &mut self,
        base: Value,
        overlay_entries: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        if let Value::List(existing) = base {
            let mut amended = existing;
            self.eval_listing_entries(overlay_entries, scope, depth + 1, &mut amended)
                .await?;
            return Ok(Value::List(amended));
        }
        if let Value::Object(_, Some(base_src)) = &base {
            if !base_src.mapping_value_types.is_empty() {
                let mut type_scope = Scope::default();
                for (key, value) in &base_src.scope {
                    type_scope.set(key.clone(), value.clone());
                }
                for (key, value) in scope.flatten() {
                    type_scope.set(key, value);
                }
                for (key, ty) in scope.flatten_type_aliases() {
                    type_scope.set_type_alias(key, ty);
                }
                let value_type_defaults = base_src
                    .mapping_value_types
                    .iter()
                    .filter_map(|name| {
                        resolve_dotted(&type_scope, name).map(|value| (name.clone(), value))
                    })
                    .collect::<Vec<_>>();
                let inherited_default = self
                    .find_default_template(&base_src.entries, &type_scope, depth)
                    .await?;
                let mut amended = IndexMap::new();
                self.eval_mapping_entries_with_type_default(
                    &base_src.entries,
                    &type_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    MappingInheritedDefault::default(),
                )
                .await?;
                if let Value::Object(existing_map, _) = &base {
                    amended.extend(existing_map.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                self.eval_mapping_entries_with_type_default(
                    overlay_entries,
                    &type_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    MappingInheritedDefault {
                        value: inherited_default,
                        entries: find_default_body_entries(&base_src.entries),
                    },
                )
                .await?;
                return Ok(Value::Object(Arc::new(amended), Some(Arc::clone(base_src))));
            }
            return self
                .eval_amended_object(
                    &base_src.entries.clone(),
                    &base_src.scope.clone(),
                    overlay_entries,
                    scope,
                    depth,
                    base_src
                        .type_name
                        .clone()
                        .map(|name| (name, base_src.parent_type_names.clone())),
                )
                .await;
        }
        let mut amendment_scope = scope.child();
        if let Value::Object(existing, _) = &base {
            for (name, value) in existing.iter() {
                amendment_scope.set(name.clone(), value.clone());
            }
        }
        let overlay = self
            .eval_entries(overlay_entries, &amendment_scope, depth + 1)
            .await?;
        Ok(merge_values(base, overlay))
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
            return self
                .eval_value_amendment(base, overlay_entries, scope, depth)
                .await;
        }

        // Logical `&&` and `||` short-circuit: the right operand must not be
        // evaluated when the left already determines the result (e.g.
        // `x is Foo && x.fooField` must not touch `fooField` when `x` is not a
        // `Foo`).
        if matches!(op, BinOp::And | BinOp::Or) {
            let left_truthy = is_truthy(&self.eval_expr(left, scope, depth + 1).await?);
            let short_circuit = match op {
                BinOp::And => !left_truthy,
                _ => left_truthy,
            };
            if short_circuit {
                return Ok(Value::Bool(left_truthy));
            }
            let right_truthy = is_truthy(&self.eval_expr(right, scope, depth + 1).await?);
            return Ok(Value::Bool(right_truthy));
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
                if is_null_value(&l) {
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
        type_defaults: &[(String, Value)],
        inherited_default: MappingInheritedDefault,
    ) -> Result<()> {
        let mut entry_scope = scope.child();
        let mut deferred_lambdas: Vec<(String, &crate::parser::Expr)> = Vec::new();
        for entry in entries {
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    let expr = prop.value.as_ref().unwrap();
                    // Bind every local in declaration order so later locals and
                    // entries can reference it (e.g. a non-lambda local that
                    // calls a lambda local defined just above it).
                    let val = self.eval_expr(expr, &entry_scope, depth).await?;
                    entry_scope.set(prop.name.clone(), val);
                    if matches!(expr, crate::parser::Expr::Lambda(..)) {
                        // Lambda evaluation only captures the current scope; it
                        // does not run the body. Bind once for declaration-order
                        // visibility, then re-bind after properties for late
                        // binding of overrides.
                        deferred_lambdas.push((prop.name.clone(), expr));
                    }
                }
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.body.is_some() =>
                {
                    let val = self
                        .eval_entries(prop.body.as_ref().unwrap(), &entry_scope, depth)
                        .await?;
                    entry_scope.set(prop.name.clone(), val);
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self
                        .eval_class_def(
                            name,
                            class_mods,
                            parent.as_deref(),
                            body,
                            &entry_scope,
                            depth,
                        )
                        .await?;
                    entry_scope.set(name.clone(), defaults);
                }
                Entry::TypeAlias(name, ty) => {
                    self.eval_type_alias(name, ty, &mut entry_scope);
                }
                _ => {}
            }
        }
        for (name, expr) in deferred_lambdas {
            let val = self.eval_expr(expr, &entry_scope, depth).await?;
            entry_scope.set(name, val);
        }

        let explicit_default = self
            .find_default_template(entries, &entry_scope, depth)
            .await?
            .or(inherited_default.value);
        let explicit_default_entries =
            find_default_body_entries(entries).or(inherited_default.entries);

        for entry in entries {
            match entry {
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, &entry_scope, depth + 1).await?;
                    let key_str = value_to_key(&key)?;
                    if let Some(Value::Object(existing_map, Some(existing_src))) = map.get(&key_str)
                        && let Expr::ObjectBody(body) = val_expr
                    {
                        let val = self
                            .eval_object_body_over_template(
                                existing_map,
                                existing_src,
                                body,
                                &entry_scope,
                                depth,
                            )
                            .await?;
                        map.insert(key_str, val);
                        continue;
                    }
                    let type_default = match val_expr {
                        Expr::ObjectBody(body) => select_mapping_type_default(type_defaults, body)
                            .map(|(name, value)| (Some(name.as_str()), value)),
                        Expr::New(Some(type_name), _, _) => {
                            select_mapping_type_default_for_new(type_defaults, type_name)
                                .map(|(name, value)| (Some(name.as_str()), value))
                        }
                        Expr::New(None, _, _) => None,
                        _ => type_defaults
                            .first()
                            .map(|(name, value)| (Some(name.as_str()), value)),
                    };
                    let default_template = match (type_default, explicit_default.as_ref()) {
                        (Some((type_name, type_default)), Some(explicit_default)) => Some((
                            type_name,
                            merge_values(type_default.clone(), explicit_default.clone()),
                        )),
                        (Some((type_name, type_default)), None) => {
                            Some((type_name, type_default.clone()))
                        }
                        (None, Some(explicit_default)) => Some((None, explicit_default.clone())),
                        (None, None) => None,
                    };
                    // If the default template has ObjectSource, amend its source entries
                    // so late-bound class properties are recomputed after overrides.
                    let val =
                        if let Some((default_type_name, Value::Object(template_map, Some(src)))) =
                            default_template.as_ref()
                            && let Some(body) = mapping_entry_body(val_expr)
                        {
                            if let Expr::New(Some(type_name), _, _) = val_expr {
                                validate_new_object_body(type_name, body, src)?;
                            }
                            let mut result =
                                if let Some(default_entries) = explicit_default_entries.as_ref() {
                                    let mut overlay_entries = default_entries.clone();
                                    overlay_entries.extend(body.iter().cloned());
                                    self.eval_amended_object(
                                        &src.entries,
                                        &src.scope,
                                        &overlay_entries,
                                        &entry_scope,
                                        depth,
                                        src.type_name
                                            .clone()
                                            .map(|name| (name, src.parent_type_names.clone())),
                                    )
                                    .await?
                                } else if explicit_default.is_some() && type_default.is_some() {
                                    self.eval_object_body_over_template(
                                        template_map,
                                        src,
                                        body,
                                        &entry_scope,
                                        depth,
                                    )
                                    .await?
                                } else {
                                    self.eval_amended_object(
                                        &src.entries,
                                        &src.scope,
                                        body,
                                        &entry_scope,
                                        depth,
                                        src.type_name
                                            .clone()
                                            .map(|name| (name, src.parent_type_names.clone())),
                                    )
                                    .await?
                                };
                            let type_name = src.type_name.as_deref().or(*default_type_name);
                            if let Some(tn) = type_name
                                && let Value::Object(_, ref mut result_src) = result
                            {
                                let new_src = match result_src.as_ref() {
                                    Some(s) => {
                                        let mut ns = (**s).clone();
                                        ns.type_name = Some(tn.to_string());
                                        ns
                                    }
                                    None => ObjectSource {
                                        entries: vec![],
                                        scope: IndexMap::new(),
                                        is_open: true,
                                        type_name: Some(tn.to_string()),
                                        parent_type_names: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&src.deprecated, body),
                                    },
                                };
                                *result_src = Some(std::sync::Arc::new(new_src));
                            }
                            result
                        } else {
                            let mut val = self.eval_expr(val_expr, &entry_scope, depth + 1).await?;
                            if let Some((_, tpl)) = default_template {
                                val = merge_values(tpl, val);
                            }
                            val
                        };
                    map.insert(key_str, val);
                }
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Property(prop)
                    if prop.name == "default"
                        && (explicit_default.is_some() || !type_defaults.is_empty()) => {}
                Entry::Spread(e) => {
                    let val = self.eval_expr(e, &entry_scope, depth + 1).await?;
                    if let Value::Object(m, _) = val {
                        map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self
                        .eval_expr(&fgen.collection, &entry_scope, depth + 1)
                        .await?;
                    for (k, v) in collection_to_items(collection) {
                        let mut iter_scope = entry_scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(kv) = &fgen.key_var {
                            iter_scope.set(kv.clone(), k);
                        }
                        self.eval_mapping_entries_with_type_default(
                            &fgen.body,
                            &iter_scope,
                            depth + 1,
                            map,
                            type_defaults,
                            MappingInheritedDefault {
                                value: explicit_default.clone(),
                                entries: explicit_default_entries.clone(),
                            },
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
        let mut converter_scope = scope.child();
        for entry in output_body {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
                && let Ok(value) = self.eval_expr(expr, &converter_scope, depth).await
            {
                converter_scope.set(prop.name.clone(), value);
            }
        }
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
        for entry in renderer_body {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
                && let Ok(value) = self.eval_expr(expr, &converter_scope, depth).await
            {
                converter_scope.set(prop.name.clone(), value);
            }
        }
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
                if let Ok(lambda) = self.eval_expr(val_expr, &converter_scope, depth).await
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
        self.apply_converters_recursive(value, &converters, Vec::new())
            .await
    }

    #[async_recursion(?Send)]
    async fn apply_converters_recursive(
        &mut self,
        value: Value,
        converters: &[(String, Value)],
        blocked_root_converters: Vec<String>,
    ) -> Result<Value> {
        match value {
            Value::Object(map, ref src) => {
                // Check if this object has a type_name that matches a converter
                let type_names = src
                    .iter()
                    .flat_map(|source| {
                        source
                            .type_name
                            .iter()
                            .chain(source.parent_type_names.iter())
                    })
                    .collect::<Vec<_>>();

                if !type_names.is_empty() {
                    for type_name in type_names {
                        for (conv_name, lambda) in converters {
                            if type_names_match(conv_name, type_name)
                                && !blocked_root_converters.contains(conv_name)
                                && let Value::Lambda(params, body, captured) = lambda
                            {
                                let mut call_scope = Scope::default();
                                for (k, v) in captured.iter() {
                                    call_scope.set(k.clone(), v.clone());
                                }
                                // Bind the object as the first parameter
                                if let Some(param) = params.first() {
                                    call_scope.set(
                                        param.clone(),
                                        Value::Object(map.clone(), src.clone()),
                                    );
                                }
                                let result = self.eval_expr(body, &call_scope, 0).await?;
                                let mut blocked = blocked_root_converters;
                                blocked.push(conv_name.clone());
                                return self
                                    .apply_converters_recursive(result, converters, blocked)
                                    .await;
                            }
                        }
                    }
                }

                // No converter matched — recurse into children
                let mut new_map = IndexMap::new();
                for (k, v) in map.iter() {
                    new_map.insert(
                        k.clone(),
                        self.apply_converters_recursive(v.clone(), converters, Vec::new())
                            .await?,
                    );
                }
                Ok(Value::Object(Arc::new(new_map), src.clone()))
            }
            Value::List(items) => {
                let mut new_items = Vec::with_capacity(items.len());
                for item in items {
                    new_items.push(
                        self.apply_converters_recursive(item, converters, Vec::new())
                            .await?,
                    );
                }
                Ok(Value::List(new_items))
            }
            other => Ok(other),
        }
    }
}

fn refresh_this_aliases(
    scope: &mut Scope,
    aliases: &[String],
    properties: &IndexMap<String, Value>,
) {
    let snapshot = Value::Object(Arc::new(properties.clone()), None);
    scope.set("this".into(), snapshot.clone());
    for alias in aliases {
        scope.set(alias.clone(), snapshot.clone());
    }
}

fn select_mapping_type_default<'a>(
    type_defaults: &'a [(String, Value)],
    body: &[Entry],
) -> Option<&'a (String, Value)> {
    if type_defaults.len() <= 1 {
        return type_defaults.first();
    }

    let field_names = body.iter().filter_map(|entry| match entry {
        Entry::Property(prop) if !has_modifier(&prop.modifiers, Modifier::Local) => {
            Some(prop.name.as_str())
        }
        Entry::DynProperty(Expr::String(name), _) => Some(name.as_str()),
        _ => None,
    });

    let mut best = None;
    let mut best_score = 0;
    for candidate in type_defaults {
        let score = field_names
            .clone()
            .filter(|field| object_declares_field(&candidate.1, field))
            .count();
        if score > best_score {
            best = Some(candidate);
            best_score = score;
        }
    }

    // Empty bodies, unknown fields, and ties fall back to the declared union
    // order. This mirrors Pkl's first assignable type behavior when there is no
    // stronger structural signal in the entry body.
    best.or_else(|| type_defaults.first())
}

fn select_mapping_type_default_for_new<'a>(
    type_defaults: &'a [(String, Value)],
    type_name: &str,
) -> Option<&'a (String, Value)> {
    type_defaults
        .iter()
        .find(|(name, _)| name == type_name)
        .or_else(|| {
            type_defaults
                .iter()
                .find(|(name, _)| type_names_match(name, type_name))
        })
        .or_else(|| {
            type_defaults.iter().find(|(_, value)| {
                matches!(
                    value,
                    Value::Object(_, Some(src))
                        if src
                            .type_name
                            .as_deref()
                            .is_some_and(|tn| type_names_match(tn, type_name))
                )
            })
        })
}

fn type_names_match(a: &str, b: &str) -> bool {
    a == b
        || (a.len() > b.len() && a.ends_with(b) && a.as_bytes()[a.len() - b.len() - 1] == b'.')
        || (b.len() > a.len() && b.ends_with(a) && b.as_bytes()[b.len() - a.len() - 1] == b'.')
}

fn mapping_entry_body(expr: &Expr) -> Option<&[Entry]> {
    match expr {
        Expr::ObjectBody(body) => Some(body),
        Expr::New(Some(_), body, _) => Some(body),
        _ => None,
    }
}

fn apply_mapping_type_annotation(value: &mut Value, type_ann: Option<&crate::parser::TypeExpr>) {
    let Some(type_ann) = type_ann else {
        return;
    };
    let type_names = mapping_value_type_names(type_ann);
    if type_names.is_empty() {
        return;
    }
    let Value::Object(_, src_slot) = value else {
        return;
    };

    let mut src = src_slot
        .as_ref()
        .map(|src| (**src).clone())
        .unwrap_or_else(|| ObjectSource {
            entries: Vec::new(),
            scope: IndexMap::new(),
            is_open: true,
            type_name: None,
            parent_type_names: Vec::new(),
            mapping_value_types: Vec::new(),
            deprecated: IndexMap::new(),
        });
    for name in type_names {
        if !src.mapping_value_types.contains(&name) {
            src.mapping_value_types.push(name);
        }
    }
    *src_slot = Some(Arc::new(src));
}

fn mapping_value_type_names(type_ann: &crate::parser::TypeExpr) -> Vec<String> {
    use crate::parser::TypeExpr;

    let mut names = Vec::new();
    match type_ann {
        TypeExpr::Generic(name, params) if name == "Mapping" || name == "Map" => {
            if let Some(value_type) = params.get(1) {
                collect_mapping_value_type_names(value_type, &mut names);
            }
        }
        TypeExpr::Nullable(inner) => return mapping_value_type_names(inner),
        TypeExpr::Constrained(base, _) => {
            if let Some(value_type) = mapping_value_type_from_name(base) {
                names.push(value_type);
            }
        }
        _ => {}
    }
    names
}

fn mapping_value_type_from_name(name: &str) -> Option<String> {
    let name = name.trim_start_matches('*').trim_end_matches('?');
    let inner = name
        .strip_prefix("Mapping<")
        .or_else(|| name.strip_prefix("Map<"))?
        .strip_suffix('>')?;
    let mut depth = 0usize;
    for (index, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let value_type = inner[index + 1..].trim();
                return Some(
                    value_type
                        .split('<')
                        .next()
                        .unwrap_or(value_type)
                        .trim_start_matches('*')
                        .trim_end_matches('?')
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    None
}

fn collect_mapping_value_type_names(type_ann: &crate::parser::TypeExpr, names: &mut Vec<String>) {
    use crate::parser::TypeExpr;

    match type_ann {
        TypeExpr::Named(name) => {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        TypeExpr::Constrained(base, _) => {
            if !names.contains(base) {
                names.push(base.clone());
            }
        }
        // Keep only the top-level value type. For example,
        // Mapping<String, Mapping<String, Step>> needs a structured Mapping
        // default, not Step as the default for the outer mapping's entries.
        TypeExpr::Generic(name, _) => {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        TypeExpr::Nullable(inner) => collect_mapping_value_type_names(inner, names),
        TypeExpr::Union(variants) => {
            for variant in variants {
                collect_mapping_value_type_names(variant, names);
            }
        }
    }
}

fn validate_new_object_body(
    type_name: &str,
    entries: &[Entry],
    base_src: &ObjectSource,
) -> Result<()> {
    if base_src.is_open {
        return Ok(());
    }

    let base_names: HashSet<String> = base_src
        .entries
        .iter()
        .filter_map(|entry| {
            if let Entry::Property(prop) = entry {
                Some(prop.name.clone())
            } else {
                None
            }
        })
        .collect();

    for entry in entries {
        match entry {
            Entry::Property(prop)
                if !has_modifier(&prop.modifiers, Modifier::Local)
                    && !base_names.contains(&prop.name) =>
            {
                return Err(Error::Eval(format!(
                    "cannot add property '{}' to non-open class {type_name}",
                    prop.name
                )));
            }
            Entry::DynProperty(Expr::String(key), _) if !base_names.contains(key) => {
                return Err(Error::Eval(format!(
                    "cannot add property '{key}' to non-open class {type_name}"
                )));
            }
            _ => {}
        }
    }

    Ok(())
}

fn find_default_body_entries(entries: &[Entry]) -> Option<Vec<Entry>> {
    entries.iter().find_map(|entry| {
        if let Entry::Property(prop) = entry
            && prop.name == "default"
            && !has_modifier(&prop.modifiers, Modifier::Local)
        {
            return prop.body.clone();
        }
        None
    })
}

fn object_declares_field(value: &Value, field: &str) -> bool {
    let Value::Object(map, source) = value else {
        return false;
    };
    map.contains_key(field)
        || source.as_ref().is_some_and(|source| {
            source.entries.iter().any(|entry| match entry {
                Entry::Property(prop) => prop.name == field,
                Entry::DynProperty(Expr::String(name), _) => name == field,
                _ => false,
            })
        })
}

fn analysis_entries_for_requested_fields(
    entries: &[Entry],
    requested_fields: Option<&HashSet<String>>,
) -> Vec<Entry> {
    let Some(requested_fields) = requested_fields else {
        return entries.to_vec();
    };
    entries
        .iter()
        .filter(|entry| match entry {
            Entry::Property(prop) => {
                has_modifier(&prop.modifiers, Modifier::Local)
                    || requested_fields.contains(&prop.name)
            }
            // These entries are still evaluated outside the property output
            // filter, so their imports must remain visible to the analysis.
            Entry::DynProperty(..)
            | Entry::Spread(_)
            | Entry::ForGenerator(_)
            | Entry::WhenGenerator(_)
            | Entry::ClassDef(..)
            | Entry::TypeAlias(..) => true,
            Entry::Elem(_) => false,
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ImportUse {
    Fields(HashSet<String>),
    Whole,
}

fn requested_fields_for_import(
    uses: &HashMap<String, ImportUse>,
    alias: &str,
) -> Option<HashSet<String>> {
    match uses.get(alias) {
        Some(ImportUse::Fields(fields)) => Some(fields.clone()),
        Some(ImportUse::Whole) | None => None,
    }
}

fn import_field_uses(entries: &[Entry]) -> HashMap<String, ImportUse> {
    let mut uses = HashMap::new();
    let shadows = HashSet::new();
    collect_entry_import_field_uses(entries, &mut uses, &shadows);
    uses
}

fn collect_entry_import_field_uses(
    entries: &[Entry],
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
) {
    let mut entry_shadows = shadows.clone();
    entry_shadows.extend(declared_entry_roots(entries));
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(ty) = &prop.type_ann {
                    collect_type_import_field_uses(ty, uses, &entry_shadows);
                }
                if let Some(expr) = &prop.value {
                    collect_expr_import_field_uses(expr, uses, &entry_shadows);
                }
                if let Some(body) = &prop.body {
                    collect_entry_import_field_uses(body, uses, &entry_shadows);
                }
            }
            Entry::DynProperty(key, value) => {
                collect_expr_import_field_uses(key, uses, &entry_shadows);
                collect_expr_import_field_uses(value, uses, &entry_shadows);
            }
            Entry::ForGenerator(fgen) => {
                collect_expr_import_field_uses(&fgen.collection, uses, &entry_shadows);
                let mut body_shadows = entry_shadows.clone();
                body_shadows.insert(fgen.val_var.clone());
                if let Some(key_var) = &fgen.key_var {
                    body_shadows.insert(key_var.clone());
                }
                collect_entry_import_field_uses(&fgen.body, uses, &body_shadows);
            }
            Entry::WhenGenerator(wgen) => {
                collect_expr_import_field_uses(&wgen.condition, uses, &entry_shadows);
                collect_entry_import_field_uses(&wgen.body, uses, &entry_shadows);
                if let Some(else_body) = &wgen.else_body {
                    collect_entry_import_field_uses(else_body, uses, &entry_shadows);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => {
                collect_expr_import_field_uses(expr, uses, &entry_shadows);
            }
            Entry::ClassDef(_, _, _, body) => {
                collect_entry_import_field_uses(body, uses, &entry_shadows);
            }
            Entry::TypeAlias(..) => {}
        }
    }
}

fn collect_expr_import_field_uses(
    expr: &Expr,
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
) {
    match expr {
        Expr::Ident(name) => record_whole_import_use(uses, shadows, name),
        Expr::Field(base, field) | Expr::NullSafeField(base, field) => {
            if let Expr::Ident(name) = base.as_ref() {
                record_field_import_use(uses, shadows, name, field);
            } else {
                collect_expr_import_field_uses(base, uses, shadows);
            }
        }
        Expr::Index(base, index) | Expr::Binop(_, base, index) => {
            collect_expr_import_field_uses(base, uses, shadows);
            collect_expr_import_field_uses(index, uses, shadows);
        }
        Expr::New(type_name, entries, generic_params) => {
            if let Some(type_name) = type_name {
                if let Some((root, field)) = type_name.split_once('.') {
                    record_field_import_use(uses, shadows, root, field);
                } else {
                    record_whole_import_use(uses, shadows, type_name);
                }
            }
            for param in generic_params {
                record_type_name_import_use(uses, shadows, param);
            }
            collect_entry_import_field_uses(entries, uses, shadows);
        }
        Expr::Call(callee, args) => {
            if let Some(name) = object_method_import_receiver(callee.as_ref()) {
                record_whole_import_use(uses, shadows, name);
            } else {
                collect_expr_import_field_uses(callee, uses, shadows);
            }
            for arg in args {
                collect_expr_import_field_uses(arg, uses, shadows);
            }
        }
        Expr::If(cond, then_expr, else_expr) => {
            collect_expr_import_field_uses(cond, uses, shadows);
            collect_expr_import_field_uses(then_expr, uses, shadows);
            collect_expr_import_field_uses(else_expr, uses, shadows);
        }
        Expr::Let(name, value, body) => {
            collect_expr_import_field_uses(value, uses, shadows);
            let mut body_shadows = shadows.clone();
            body_shadows.insert(name.clone());
            collect_expr_import_field_uses(body, uses, &body_shadows);
        }
        Expr::Is(value, ty) | Expr::As(value, ty) => {
            collect_expr_import_field_uses(value, uses, shadows);
            collect_type_import_field_uses(ty, uses, shadows);
        }
        Expr::Lambda(params, value) => {
            let mut body_shadows = shadows.clone();
            body_shadows.extend(params.iter().cloned());
            collect_expr_import_field_uses(value, uses, &body_shadows);
        }
        Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::Trace(value)
        | Expr::Read(value)
        | Expr::ReadOrNull(value) => collect_expr_import_field_uses(value, uses, shadows),
        Expr::ObjectBody(entries) => collect_entry_import_field_uses(entries, uses, shadows),
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(expr) = part {
                    collect_expr_import_field_uses(expr, uses, shadows);
                }
            }
        }
        Expr::Null | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => {}
    }
}

fn object_method_import_receiver(expr: &Expr) -> Option<&str> {
    let (base, method) = match expr {
        Expr::Field(base, method) | Expr::NullSafeField(base, method) => (base.as_ref(), method),
        _ => return None,
    };
    if !is_intrinsic_object_method(method) {
        return None;
    }
    match base {
        Expr::Ident(name) => Some(name),
        _ => None,
    }
}

fn is_intrinsic_object_method(method: &str) -> bool {
    // Import aliases evaluate to module objects. Only methods dispatched for
    // `Value::Object` are whole-object uses here; list/string method names can
    // still be user-defined exported methods such as `Dep.map()`.
    matches!(
        method,
        "containsKey" | "toMap" | "toMapping" | "mapValues" | "filter" | "toList" | "toDynamic"
    )
}

fn collect_type_import_field_uses(
    ty: &crate::parser::TypeExpr,
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
) {
    match ty {
        crate::parser::TypeExpr::Named(name) => {
            if string_literal_type_value(name).is_none() {
                record_type_name_import_use(uses, shadows, name);
            }
        }
        crate::parser::TypeExpr::Nullable(inner) => {
            collect_type_import_field_uses(inner, uses, shadows);
        }
        crate::parser::TypeExpr::Union(types) => {
            for ty in types {
                collect_type_import_field_uses(ty, uses, shadows);
            }
        }
        crate::parser::TypeExpr::Generic(name, params) => {
            record_type_name_import_use(uses, shadows, name);
            for param in params {
                collect_type_import_field_uses(param, uses, shadows);
            }
        }
        crate::parser::TypeExpr::Constrained(base, constraint) => {
            for component in constrained_type_components(base) {
                record_type_name_import_use(uses, shadows, component);
            }
            collect_expr_import_field_uses(constraint, uses, shadows);
        }
    }
}

fn record_type_name_import_use(
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
    name: &str,
) {
    if let Some((root, field)) = name.split_once('.') {
        record_field_import_use(uses, shadows, root, field);
    } else {
        record_whole_import_use(uses, shadows, name);
    }
}

fn record_field_import_use(
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
    name: &str,
    field: &str,
) {
    if shadows.contains(name) {
        return;
    }
    match uses.entry(name.to_string()) {
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if let ImportUse::Fields(fields) = entry.get_mut() {
                fields.insert(field.to_string());
            }
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(ImportUse::Fields(HashSet::from([field.to_string()])));
        }
    }
}

fn record_whole_import_use(
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
    name: &str,
) {
    if !shadows.contains(name) {
        uses.insert(name.to_string(), ImportUse::Whole);
    }
}

fn expand_requested_fields(entries: &[Entry], requested: &HashSet<String>) -> HashSet<String> {
    let property_names: HashSet<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Property(prop) if !has_modifier(&prop.modifiers, Modifier::Local) => {
                Some(prop.name.clone())
            }
            _ => None,
        })
        .collect();
    let mut expanded = requested.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for entry in entries {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if !expanded.contains(&prop.name) {
                continue;
            }
            let mut refs = HashSet::new();
            let shadows = HashSet::new();
            if let Some(ty) = &prop.type_ann {
                collect_type_refs(ty, &mut refs, &shadows);
            }
            if let Some(expr) = &prop.value {
                collect_expr_refs(expr, &mut refs, &shadows);
                collect_sibling_field_refs_expr(expr, &mut refs, true);
            }
            if let Some(body) = &prop.body {
                collect_entry_refs(body, &mut refs, &shadows);
                collect_sibling_field_refs_entries(body, &mut refs);
            }
            for dep in refs {
                if dep == DYNAMIC_SIBLING_REF {
                    for name in &property_names {
                        if expanded.insert(name.clone()) {
                            changed = true;
                        }
                    }
                    continue;
                }
                if property_names.contains(&dep) && expanded.insert(dep) {
                    changed = true;
                }
            }
        }
    }
    expanded
}

fn collect_sibling_field_refs_entries(entries: &[Entry], refs: &mut HashSet<String>) {
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(expr) = &prop.value {
                    collect_sibling_field_refs_expr(expr, refs, false);
                }
                if let Some(body) = &prop.body {
                    collect_sibling_field_refs_entries(body, refs);
                }
            }
            Entry::DynProperty(key, value) => {
                collect_sibling_field_refs_expr(key, refs, false);
                collect_sibling_field_refs_expr(value, refs, false);
            }
            Entry::ForGenerator(fgen) => {
                collect_sibling_field_refs_expr(&fgen.collection, refs, false);
                collect_sibling_field_refs_entries(&fgen.body, refs);
            }
            Entry::WhenGenerator(wgen) => {
                collect_sibling_field_refs_expr(&wgen.condition, refs, false);
                collect_sibling_field_refs_entries(&wgen.body, refs);
                if let Some(else_body) = &wgen.else_body {
                    collect_sibling_field_refs_entries(else_body, refs);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => {
                collect_sibling_field_refs_expr(expr, refs, false);
            }
            Entry::ClassDef(_, _, _, body) => collect_sibling_field_refs_entries(body, refs),
            Entry::TypeAlias(..) => {}
        }
    }
}

fn collect_sibling_field_refs_expr(expr: &Expr, refs: &mut HashSet<String>, include_this: bool) {
    match expr {
        Expr::Field(base, field) | Expr::NullSafeField(base, field) => {
            if is_module_sibling_ref(base, include_this) {
                refs.insert(field.clone());
            }
            collect_sibling_field_refs_expr(base, refs, include_this);
        }
        Expr::Index(base, index) => {
            if is_module_sibling_ref(base, include_this) {
                if let Expr::String(key) = index.as_ref() {
                    refs.insert(key.clone());
                } else {
                    refs.insert(DYNAMIC_SIBLING_REF.to_string());
                }
            }
            collect_sibling_field_refs_expr(base, refs, include_this);
            collect_sibling_field_refs_expr(index, refs, include_this);
        }
        Expr::Binop(_, left, right) => {
            collect_sibling_field_refs_expr(left, refs, include_this);
            collect_sibling_field_refs_expr(right, refs, include_this);
        }
        Expr::New(_, entries, _) | Expr::ObjectBody(entries) => {
            collect_sibling_field_refs_entries(entries, refs);
        }
        Expr::Call(callee, args) => {
            collect_sibling_field_refs_expr(callee, refs, include_this);
            for arg in args {
                collect_sibling_field_refs_expr(arg, refs, include_this);
            }
        }
        Expr::If(cond, then_expr, else_expr) => {
            collect_sibling_field_refs_expr(cond, refs, include_this);
            collect_sibling_field_refs_expr(then_expr, refs, include_this);
            collect_sibling_field_refs_expr(else_expr, refs, include_this);
        }
        Expr::Let(_, value, body) => {
            collect_sibling_field_refs_expr(value, refs, include_this);
            collect_sibling_field_refs_expr(body, refs, include_this);
        }
        Expr::Is(value, _) | Expr::As(value, _) => {
            collect_sibling_field_refs_expr(value, refs, include_this);
        }
        Expr::Lambda(_, body)
        | Expr::Unop(_, body)
        | Expr::Throw(body)
        | Expr::Trace(body)
        | Expr::Read(body)
        | Expr::ReadOrNull(body) => {
            collect_sibling_field_refs_expr(body, refs, include_this);
        }
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(expr) = part {
                    collect_sibling_field_refs_expr(expr, refs, include_this);
                }
            }
        }
        Expr::Ident(_)
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_) => {}
    }
}

fn property_reference_names(prop: &Property) -> HashSet<String> {
    let mut refs = HashSet::new();
    let shadows = HashSet::new();
    if let Some(expr) = &prop.value {
        collect_expr_refs(expr, &mut refs, &shadows);
        collect_sibling_field_refs_expr(expr, &mut refs, true);
        if has_modifier(&prop.modifiers, Modifier::Local) && is_module_sibling_ref(expr, true) {
            refs.insert(DYNAMIC_SIBLING_REF.to_string());
        }
    }
    if let Some(body) = &prop.body {
        collect_entry_refs(body, &mut refs, &shadows);
        collect_sibling_field_refs_entries(body, &mut refs);
    }
    refs
}

fn is_module_sibling_ref(expr: &Expr, include_this: bool) -> bool {
    matches!(expr, Expr::Ident(name) if name == "module" || (include_this && name == "this"))
}

fn referenced_roots(entries: &[Entry]) -> HashSet<String> {
    let mut refs = HashSet::new();
    let shadows = HashSet::new();
    collect_entry_refs(entries, &mut refs, &shadows);
    refs
}

fn collect_entry_refs(entries: &[Entry], refs: &mut HashSet<String>, shadows: &HashSet<String>) {
    let mut entry_shadows = shadows.clone();
    entry_shadows.extend(declared_entry_roots(entries));
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(ty) = &prop.type_ann {
                    collect_type_refs(ty, refs, &entry_shadows);
                }
                if let Some(expr) = &prop.value {
                    collect_expr_refs(expr, refs, &entry_shadows);
                }
                if let Some(body) = &prop.body {
                    collect_entry_refs(body, refs, &entry_shadows);
                }
            }
            Entry::DynProperty(key, value) => {
                collect_expr_refs(key, refs, &entry_shadows);
                collect_expr_refs(value, refs, &entry_shadows);
            }
            Entry::ForGenerator(fgen) => {
                collect_expr_refs(&fgen.collection, refs, &entry_shadows);
                let mut body_shadows = entry_shadows.clone();
                body_shadows.insert(fgen.val_var.clone());
                if let Some(key_var) = &fgen.key_var {
                    body_shadows.insert(key_var.clone());
                }
                collect_entry_refs(&fgen.body, refs, &body_shadows);
            }
            Entry::WhenGenerator(wgen) => {
                collect_expr_refs(&wgen.condition, refs, &entry_shadows);
                collect_entry_refs(&wgen.body, refs, &entry_shadows);
                if let Some(else_body) = &wgen.else_body {
                    collect_entry_refs(else_body, refs, &entry_shadows);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => {
                collect_expr_refs(expr, refs, &entry_shadows)
            }
            Entry::ClassDef(_, _, parent, body) => {
                if let Some(parent) = parent {
                    collect_name_root(parent, refs, &entry_shadows);
                }
                collect_entry_refs(body, refs, &entry_shadows);
            }
            Entry::TypeAlias(_, ty) => collect_type_refs(ty, refs, &entry_shadows),
        }
    }
}

fn collect_expr_refs(expr: &Expr, refs: &mut HashSet<String>, shadows: &HashSet<String>) {
    match expr {
        Expr::Ident(name) => {
            if !shadows.contains(name) {
                refs.insert(name.clone());
            }
        }
        Expr::New(type_name, entries, generic_params) => {
            if let Some(type_name) = type_name {
                collect_name_root(type_name, refs, shadows);
            }
            for param in generic_params {
                collect_name_root(param, refs, shadows);
            }
            collect_entry_refs(entries, refs, shadows);
        }
        Expr::Field(base, _) | Expr::NullSafeField(base, _) => {
            collect_expr_refs(base, refs, shadows);
        }
        Expr::Index(base, index) | Expr::Binop(_, base, index) => {
            collect_expr_refs(base, refs, shadows);
            collect_expr_refs(index, refs, shadows);
        }
        Expr::Call(callee, args) => {
            collect_expr_refs(callee, refs, shadows);
            for arg in args {
                collect_expr_refs(arg, refs, shadows);
            }
        }
        Expr::If(cond, then_expr, else_expr) => {
            collect_expr_refs(cond, refs, shadows);
            collect_expr_refs(then_expr, refs, shadows);
            collect_expr_refs(else_expr, refs, shadows);
        }
        Expr::Let(name, value, body) => {
            collect_expr_refs(value, refs, shadows);
            let mut body_shadows = shadows.clone();
            body_shadows.insert(name.clone());
            collect_expr_refs(body, refs, &body_shadows);
        }
        Expr::Is(value, ty) | Expr::As(value, ty) => {
            collect_expr_refs(value, refs, shadows);
            collect_type_refs(ty, refs, shadows);
        }
        Expr::Lambda(params, value) => {
            let mut body_shadows = shadows.clone();
            body_shadows.extend(params.iter().cloned());
            collect_expr_refs(value, refs, &body_shadows);
        }
        Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::Trace(value)
        | Expr::Read(value)
        | Expr::ReadOrNull(value) => collect_expr_refs(value, refs, shadows),
        Expr::ObjectBody(entries) => collect_entry_refs(entries, refs, shadows),
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(expr) = part {
                    collect_expr_refs(expr, refs, shadows);
                }
            }
        }
        Expr::Null | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => {}
    }
}

fn collect_type_refs(
    ty: &crate::parser::TypeExpr,
    refs: &mut HashSet<String>,
    shadows: &HashSet<String>,
) {
    match ty {
        crate::parser::TypeExpr::Named(name) => collect_name_root(name, refs, shadows),
        crate::parser::TypeExpr::Nullable(inner) => collect_type_refs(inner, refs, shadows),
        crate::parser::TypeExpr::Union(types) => {
            for ty in types {
                collect_type_refs(ty, refs, shadows);
            }
        }
        crate::parser::TypeExpr::Generic(name, params) => {
            collect_name_root(name, refs, shadows);
            for param in params {
                collect_type_refs(param, refs, shadows);
            }
        }
        crate::parser::TypeExpr::Constrained(base, constraint) => {
            for component in constrained_type_components(base) {
                collect_name_root(component, refs, shadows);
            }
            collect_expr_refs(constraint, refs, shadows);
        }
    }
}

fn collect_name_root(name: &str, refs: &mut HashSet<String>, shadows: &HashSet<String>) {
    if let Some(root) = name.split('.').next()
        && !root.is_empty()
        && !shadows.contains(root)
    {
        refs.insert(root.to_string());
    }
}

fn constrained_type_components(name: &str) -> impl Iterator<Item = &str> {
    name.trim_start_matches('*')
        .trim_end_matches('?')
        .split(['<', '>', ','])
        .map(str::trim)
        .filter(|component| !component.is_empty())
}

fn declared_entry_roots(entries: &[Entry]) -> HashSet<String> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::ClassDef(name, ..) | Entry::TypeAlias(name, _) => Some(name.clone()),
            Entry::Property(prop)
                if has_modifier(&prop.modifiers, Modifier::Local)
                    || matches!(prop.value, Some(Expr::Lambda(..))) =>
            {
                Some(prop.name.clone())
            }
            _ => None,
        })
        .collect()
}

// --- Scope ---

#[derive(Debug, Default, Clone)]
struct Scope {
    vars: IndexMap<String, Value>,
    type_aliases: IndexMap<String, crate::parser::TypeExpr>,
    poisoned: IndexMap<String, String>,
    parent: Option<Rc<Scope>>,
}

impl Scope {
    fn child(&self) -> Self {
        Self {
            vars: IndexMap::new(),
            type_aliases: IndexMap::new(),
            poisoned: IndexMap::new(),
            parent: Some(Rc::new(self.clone())),
        }
    }

    fn set(&mut self, name: String, val: Value) {
        self.poisoned.shift_remove(&name);
        self.vars.insert(name, val);
    }

    fn poison(&mut self, name: String, message: String) {
        self.poisoned.insert(name, message);
    }

    fn poison_of(&self, name: &str) -> Option<&String> {
        self.poisoned.get(name).or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.poison_of(name))
        })
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
        if self.poisoned.contains_key(name) {
            return None;
        }
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
        for name in self.poisoned.keys() {
            result.shift_remove(name);
        }
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
#[derive(Debug)]
enum PackageSource {
    /// Direct file download (pkg.pkl-lang.org format)
    Direct { url: String, root: String },
    /// Zip archive URL + path within the archive
    Zip(String, String),
}

fn package_cache_paths(cache_dir: &Path, url: &str, extension: &str) -> (PathBuf, PathBuf) {
    // FNV-1a gives package URLs stable, filesystem-safe names without making
    // the cache layout depend on platform path rules. The adjacent URL file
    // detects the extraordinarily unlikely hash collision.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let base = cache_dir.join("packages").join(format!("{hash:016x}"));
    (base.with_extension(extension), base.with_extension("url"))
}

fn validate_package_bytes(url: &str, extension: &str, bytes: &[u8]) -> Result<()> {
    if extension == "pkl" {
        std::str::from_utf8(bytes)
            .map_err(|error| Error::Eval(format!("package source is not UTF-8: {url}: {error}")))?;
    }
    #[cfg(feature = "package-zip")]
    if extension == "zip" {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|error| Error::Eval(format!("package archive is invalid: {url}: {error}")))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| {
                Error::Eval(format!("package archive is invalid: {url}: {error}"))
            })?;
            std::io::copy(&mut entry, &mut std::io::sink()).map_err(|error| {
                Error::Eval(format!(
                    "package archive entry is invalid: {url}: {}: {error}",
                    entry.name()
                ))
            })?;
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cache");
    let temp_path = path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    let rename_result = match std::fs::rename(&temp_path, path) {
        Ok(()) => return Ok(()),
        // Windows rename does not replace an existing file. There is a brief
        // non-atomic gap on that platform, but the destination must not retain
        // stale bytes while its URL sidecar is updated.
        Err(_) if path.exists() => {
            std::fs::remove_file(path).and_then(|()| std::fs::rename(&temp_path, path))
        }
        Err(error) => Err(error),
    };
    if rename_result.is_err() {
        let _ = std::fs::remove_file(temp_path);
    }
    rename_result
}

/// Resolve a `package://` URI to a download source.
fn resolve_package_uri(uri: &str) -> Result<PackageSource> {
    fn sanitize_package_entry(fragment: &str) -> Result<String> {
        if fragment.is_empty()
            || fragment.starts_with('/')
            || fragment.contains('\\')
            || fragment.split('/').any(|part| part == "..")
        {
            return Err(Error::Eval(format!(
                "invalid package entry path: {fragment}"
            )));
        }
        let path = Path::new(fragment);
        if path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return Err(Error::Eval(format!(
                "invalid package entry path: {fragment}"
            )));
        }
        Ok(fragment.to_string())
    }

    // Format 1: package://pkg.pkl-lang.org/github.com/owner/repo@version#/path.pkl
    // These resolve to direct file downloads from GitHub releases
    if let Some(rest) = uri.strip_prefix("package://pkg.pkl-lang.org/github.com/")
        && let Some((repo_ver, fragment)) = rest.split_once('#')
        && let Some((repo, version)) = repo_ver.split_once('@')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        let root = format!("https://github.com/{repo}/releases/download/{version}/");
        return Ok(PackageSource::Direct {
            url: format!("{root}{file_path}"),
            root,
        });
    }
    // Format 2: package://pkg.pkl-lang.org/pkl-pantry/package@version#/path.pkl
    // These are under https://github.com/apple/pkl-pantry's release named `package@version`
    if let Some(rest) = uri.strip_prefix("package://pkg.pkl-lang.org/pkl-pantry/")
        && let Some((package_ver, fragment)) = rest.split_once('#')
        && let Some((package, version)) = package_ver.split_once('@')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        return Ok(PackageSource::Zip(
            format!(
                "https://github.com/apple/pkl-pantry/releases/download/{package}@{version}/{package}@{version}.zip"
            ),
            file_path,
        ));
    }
    // Format 3: package://github.com/owner/repo/releases/download/v1.0/name@1.0#/path.pkl
    // These are zip archives; the fragment is a path within the zip
    if let Some(rest) = uri.strip_prefix("package://github.com/")
        && let Some((base, fragment)) = rest.split_once('#')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        let zip_url = format!("https://github.com/{base}.zip");
        return Ok(PackageSource::Zip(zip_url, file_path));
    }
    // Format 4: package://host/path/name@version#/path.pkl
    // Generic package hosts are zip archives and can be redirected with
    // HTTP rewrite rules after resolving to https://host/path/name@version.zip.
    if let Some(rest) = uri.strip_prefix("package://")
        && !rest.starts_with("pkg.pkl-lang.org/")
        && let Some((base, fragment)) = rest.split_once('#')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        let zip_url = format!("https://{base}.zip");
        return Ok(PackageSource::Zip(zip_url, file_path));
    }
    Err(Error::Eval(format!("unsupported package URI: {uri}")))
}

/// Collect `@Deprecated` annotations from a list of entries into a map of
/// property name → optional message. Used to populate `ObjectSource.deprecated`
/// so field access can warn lazily, instead of warning eagerly when a module
/// or object body is evaluated.
fn collect_deprecated(entries: &[Entry]) -> IndexMap<String, Option<String>> {
    let mut out: IndexMap<String, Option<String>> = IndexMap::new();
    for entry in entries {
        if let Entry::Property(prop) = entry {
            for ann in &prop.annotations {
                if ann.name != "Deprecated" {
                    continue;
                }
                let mut message = None;
                for e in &ann.body {
                    if let Entry::Property(p) = e
                        && p.name == "message"
                        && let Some(Expr::String(s)) = &p.value
                    {
                        message = Some(s.clone());
                    }
                }
                out.insert(prop.name.clone(), message);
            }
        }
    }
    out
}

/// Combine a base deprecation map with any `@Deprecated` annotations on a
/// list of overlay entries, with overlay winning on conflict. Used by the
/// amend/merge code paths so an amended object's `ObjectSource.deprecated`
/// reflects deprecations from both the base and the overlay.
fn merge_deprecated(
    base: &IndexMap<String, Option<String>>,
    overlay_entries: &[Entry],
) -> IndexMap<String, Option<String>> {
    let mut out = base.clone();
    for (k, v) in collect_deprecated(overlay_entries) {
        out.insert(k, v);
    }
    out
}

/// Resolve a potentially dotted name (e.g. "Foo.Bar") in scope.
fn type_default_value(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<Value> {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Constrained(base, _) if base.ends_with('?') => Some(Value::Null),
        TypeExpr::Constrained(base, _) => type_default_for_name(base, scope),
        TypeExpr::Nullable(_) => Some(Value::Null),
        TypeExpr::Named(name) if name == "Null" => Some(Value::Null),
        TypeExpr::Named(name) => {
            if let Some(value) = string_literal_type_value(name) {
                return Some(Value::String(value.to_string()));
            }
            if let Some(alias) = scope.get_type_alias(name).cloned() {
                return type_default_value(&alias, scope);
            }
            type_default_for_name(name, scope)
        }
        TypeExpr::Generic(name, _) => match name.as_str() {
            "Collection" | "List" | "Set" | "Listing" => Some(Value::List(Vec::new())),
            "Map" | "Mapping" => Some(Value::Object(Arc::new(IndexMap::new()), None)),
            _ => resolve_dotted(scope, name),
        },
        TypeExpr::Union(variants) => variants
            .iter()
            .find(|ty| is_default_type(ty))
            .and_then(|ty| type_default_value(ty, scope)),
    }
}

fn type_default_for_name(name: &str, scope: &Scope) -> Option<Value> {
    let name = name.trim_start_matches('*').trim_end_matches('?');
    let base_name = name.split('<').next().unwrap_or(name);
    match base_name {
        "Null" => Some(Value::Null),
        "Collection" | "List" | "Set" | "Listing" => Some(Value::List(Vec::new())),
        "Map" | "Mapping" => Some(Value::Object(Arc::new(IndexMap::new()), None)),
        "String" | "Boolean" | "Bool" | "Int" | "Float" | "Number" | "Any" | "Dynamic"
        | "Duration" | "DataSize" | "Pair" | "Regex" => None,
        other => resolve_dotted(scope, other),
    }
}

fn nullable_inner_default(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<Value> {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Nullable(inner) => type_default_value(inner, scope),
        TypeExpr::Constrained(name, _) if name.ends_with('?') => {
            type_default_for_name(name.trim_end_matches('?'), scope)
        }
        _ => None,
    }
}

fn type_is_listing(ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) | TypeExpr::Generic(name, _) => name == "Listing",
        TypeExpr::Nullable(inner) => type_is_listing(inner),
        TypeExpr::Constrained(name, _) => {
            name.trim_end_matches('?') == "Listing" || name.starts_with("Listing<")
        }
        TypeExpr::Union(variants) => variants.iter().any(type_is_listing),
    }
}

fn entries_are_listing_amendment(entries: &[Entry]) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::Elem(_) => true,
        Entry::ForGenerator(generator) => entries_are_listing_amendment(&generator.body),
        Entry::WhenGenerator(generator) => {
            entries_are_listing_amendment(&generator.body)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(entries_are_listing_amendment)
        }
        _ => false,
    })
}

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

fn should_render_property_value(prop: &Property, value: &Value) -> bool {
    prop.value.is_some() || prop.body.is_some() || !matches!(value, Value::Null | Value::Object(..))
}

fn module_is_abstract(module: &Module) -> bool {
    module
        .annotations
        .iter()
        .any(|annotation| annotation.name == "pklr:module:Abstract")
}

fn is_unresolved_template_error(message: &str) -> bool {
    message.contains("undefined variable") || message.contains("field not found")
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

fn string_literal_type_value(name: &str) -> Option<&str> {
    name.strip_prefix('*')
        .unwrap_or(name)
        .strip_prefix('"')?
        .strip_suffix('"')
}

fn is_default_type(ty: &crate::parser::TypeExpr) -> bool {
    matches!(ty, crate::parser::TypeExpr::Named(name) if name.starts_with('*'))
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
        TypeExpr::Constrained(base, _) => format!("{base}(... )"),
    }
}

/// Check if a value matches a Pkl type expression (non-constrained types only).
/// For constrained types, use `Evaluator::eval_type_check` instead.
fn value_is_type(val: &Value, ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) if string_literal_type_value(name).is_some() => {
            matches!(val, Value::String(actual) if Some(actual.as_str()) == string_literal_type_value(name))
        }
        TypeExpr::Named(name) => match name.strip_prefix('*').unwrap_or(name) {
            "Null" => is_null_value(val),
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
        TypeExpr::Nullable(inner) => is_null_value(val) || value_is_type(val, inner),
        TypeExpr::Union(variants) => variants.iter().any(|v| value_is_type(val, v)),
        TypeExpr::Generic(name, _) => {
            // Check the base type, ignore type parameters
            match name.as_str() {
                "List" | "Listing" | "Set" => matches!(val, Value::List(_)),
                "Map" | "Mapping" => matches!(val, Value::Object(..)),
                _ => matches!(val, Value::Object(..)),
            }
        }
        TypeExpr::Constrained(base, _) => {
            // Just check the base type; constraint requires async eval
            value_is_named_type(val, base)
        }
    }
}

fn value_is_named_type(val: &Value, name: &str) -> bool {
    if name.ends_with('?') && is_null_value(val) {
        return true;
    }
    value_is_type(
        val,
        &crate::parser::TypeExpr::Named(
            name.trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(name)
                .to_string(),
        ),
    )
}

fn type_is_runtime_checkable(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => {
            let runtime_name = name.strip_prefix('*').unwrap_or(name);
            string_literal_type_value(name).is_some()
                || matches!(
                    runtime_name,
                    "Null"
                        | "Boolean"
                        | "Bool"
                        | "Int"
                        | "Float"
                        | "Number"
                        | "String"
                        | "List"
                        | "Listing"
                        | "Set"
                        | "Map"
                        | "Mapping"
                        | "Object"
                        | "Dynamic"
                        | "Function"
                        | "Any"
                )
                || scope.get_type_alias(runtime_name).is_some()
                || resolve_dotted(scope, runtime_name).is_some()
        }
        TypeExpr::Nullable(inner) => type_is_runtime_checkable(inner, scope),
        TypeExpr::Union(variants) => variants
            .iter()
            .all(|variant| type_is_runtime_checkable(variant, scope)),
        TypeExpr::Generic(_, _) => true,
        TypeExpr::Constrained(base, _) => {
            let runtime_name = base
                .trim_start_matches('*')
                .trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(base);
            type_is_runtime_checkable(&TypeExpr::Named(runtime_name.into()), scope)
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

fn is_null_value(value: &Value) -> bool {
    matches!(value, Value::Null)
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (left, right) if is_null_value(left) && is_null_value(right) => true,
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
        (Value::Object(mut b, base_src), Value::Object(o, overlay_src)) => {
            let b_map = Arc::make_mut(&mut b);
            for (k, v) in o.iter() {
                if let Some(existing) = b_map.shift_remove(k) {
                    b_map.insert(k.clone(), merge_values(existing, v.clone()));
                } else {
                    b_map.insert(k.clone(), v.clone());
                }
            }
            // Keep the base's source (entries/scope for late binding), but when it
            // carries no class identity, inherit the overlay's. This preserves a
            // concrete value's type when it is merged onto a typeless default
            // template (e.g. a union-typed Mapping value).
            let src = match (base_src, overlay_src) {
                (Some(b), Some(o)) if b.type_name.is_none() && o.type_name.is_some() => {
                    let mut nb = (*b).clone();
                    nb.type_name = o.type_name.clone();
                    Some(Arc::new(nb))
                }
                (Some(b), _) => Some(b),
                (None, o) => o,
            };
            Value::Object(b, src)
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

/// Expand a Pkl glob pattern relative to a base directory.
pub fn expand_glob(base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    if !base.is_dir() {
        return Ok(vec![]);
    }

    if !pattern.contains('*') {
        let path = base.join(pattern);
        return if path.is_file() {
            Ok(vec![path])
        } else {
            Ok(vec![])
        };
    }

    let max_depth = max_glob_depth(pattern);
    let mut results = Vec::new();
    collect_glob_matches(base, base, pattern, max_depth, 0, &mut results)?;
    results.sort();
    Ok(results)
}

fn collect_glob_matches(
    base: &Path,
    dir: &Path,
    pattern: &str,
    max_depth: Option<usize>,
    depth: usize,
    results: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::Io(dir.to_path_buf(), e)),
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::Io(path.clone(), e)),
        };
        if file_type.is_dir() {
            if max_depth.is_none_or(|max_depth| depth < max_depth) {
                collect_glob_matches(base, &path, pattern, max_depth, depth + 1, results)?;
            }
        } else if path.is_file() {
            let relative = pathdiff_or_full(&path, base);
            if glob_matches(pattern, &relative) {
                results.push(path);
            }
        }
    }
    Ok(())
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = normalize_pkl_path(pattern);
    let path = normalize_pkl_path(path);
    glob_matches_chars(
        &pattern.chars().collect::<Vec<_>>(),
        &path.chars().collect::<Vec<_>>(),
    )
}

fn glob_matches_chars(pattern: &[char], path: &[char]) -> bool {
    let mut prev = vec![false; path.len() + 1];
    prev[0] = true;
    let mut i = 0;
    while i < pattern.len() {
        let mut next = vec![false; path.len() + 1];
        if pattern[i] == '*' && pattern.get(i + 1) == Some(&'*') && pattern.get(i + 2) == Some(&'/')
        {
            next[0] = prev[0];
            let mut can_consume_to_slash = prev[0];
            for j in 1..=path.len() {
                next[j] = prev[j] || (path[j - 1] == '/' && can_consume_to_slash);
                can_consume_to_slash |= prev[j];
            }
            i += 3;
        } else if pattern[i] == '*' && pattern.get(i + 1) == Some(&'*') {
            next[0] = prev[0];
            for j in 1..=path.len() {
                next[j] = prev[j] || next[j - 1];
            }
            i += 2;
        } else if pattern[i] == '*' {
            next[0] = prev[0];
            for j in 1..=path.len() {
                next[j] = prev[j] || (path[j - 1] != '/' && next[j - 1]);
            }
            i += 1;
        } else {
            for j in 1..=path.len() {
                next[j] = prev[j - 1] && pattern[i] == path[j - 1];
            }
            i += 1;
        }
        prev = next;
    }
    prev[path.len()]
}

fn max_glob_depth(pattern: &str) -> Option<usize> {
    let pattern = normalize_pkl_path(pattern);
    if pattern.contains("**") {
        None
    } else {
        Some(pattern.chars().filter(|c| *c == '/').count())
    }
}

/// Get a relative path string from `path` relative to `base`, or the full path if not a prefix.
fn pathdiff_or_full(path: &Path, base: &Path) -> String {
    let path = path
        .strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    normalize_pkl_path(&path)
}

fn normalize_pkl_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod requested_field_tests {
    use std::collections::HashSet;

    use super::Evaluator;
    use crate::{Value, lexer, parser};

    #[tokio::test]
    async fn inherited_late_properties_respect_requested_fields() {
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

        let value = Evaluator::new()
            .eval_module_with_scope(
                &module,
                &child_path,
                1,
                None,
                Some(HashSet::from(["wanted".to_string()])),
            )
            .await
            .unwrap();
        let Value::Object(fields, _) = value else {
            panic!("expected an object");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields["wanted"], Value::Int(3));

        std::fs::remove_dir_all(test_dir).unwrap();
    }
}

#[cfg(test)]
mod glob_tests {
    use super::{glob_matches, max_glob_depth};

    #[test]
    fn double_star_crosses_directories() {
        assert!(glob_matches("**.pkl", "config/foo.pkl"));
        assert!(glob_matches("a/**/b.pkl", "a/x/y/b.pkl"));
    }

    #[test]
    fn double_star_slash_keeps_literal_separator() {
        assert!(glob_matches("**/foo.pkl", "foo.pkl"));
        assert!(glob_matches("**/foo.pkl", "config/foo.pkl"));
    }

    #[test]
    fn star_stays_in_one_directory_segment() {
        assert!(glob_matches("*/*.pkl", "config/foo.pkl"));
        assert!(!glob_matches("*/*.pkl", "nested/config/foo.pkl"));
    }

    #[test]
    fn non_recursive_patterns_have_bounded_depth() {
        assert_eq!(max_glob_depth("*.pkl"), Some(0));
        assert_eq!(max_glob_depth("*/*.pkl"), Some(1));
        assert_eq!(max_glob_depth("**.pkl"), None);
    }
}

fn local_module_path(current_path: &Path, uri: &str) -> Option<PathBuf> {
    if uri.contains("://") && !uri.starts_with("file://") {
        return None;
    }
    // A relative reference inside a remote (http/https) module is not a local file.
    if !uri.starts_with("file://")
        && let Some(base) = current_path.to_str()
        && (base.starts_with("http://") || base.starts_with("https://"))
    {
        return None;
    }
    Some(if let Some(rel) = uri.strip_prefix("file://") {
        PathBuf::from(rel)
    } else {
        current_path.parent().unwrap_or(Path::new(".")).join(uri)
    })
}

/// If `current_path` is a remote (http/https) module URL and `uri` is a
/// relative reference (no scheme), resolve `uri` against that URL so the
/// referenced module is fetched over HTTP rather than from the local
/// filesystem. Returns the rewritten absolute URL, or `None` when no rewrite
/// applies (local base module, or `uri` already carries a scheme).
fn resolve_remote_relative(current_path: &Path, uri: &str) -> Option<String> {
    if uri.contains("://") || uri.starts_with("pkl:") {
        return None;
    }
    let base = current_path.to_str()?;
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return None;
    }
    resolve_http_relative(base, uri)
}

fn resolve_http_relative(base: &str, uri: &str) -> Option<String> {
    url::Url::parse(base)
        .ok()?
        .join(uri)
        .ok()
        .map(|url| url.to_string())
}

#[cfg(test)]
mod remote_relative_tests {
    use super::resolve_http_relative;

    #[test]
    fn http_relative_resolves_against_base_directory() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "../Lib.pkl").as_deref(),
            Some("https://example.com/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_ignores_base_query_and_fragment() {
        assert_eq!(
            resolve_http_relative(
                "https://example.com/cfg/Main.pkl?version=2#part",
                "../Lib.pkl"
            )
            .as_deref(),
            Some("https://example.com/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_keeps_absolute_path_references_absolute() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "/shared/Lib.pkl").as_deref(),
            Some("https://example.com/shared/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_clamps_above_root_segments() {
        assert_eq!(
            resolve_http_relative("https://example.com/Main.pkl", "../../Lib.pkl").as_deref(),
            Some("https://example.com/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_resolves_query_only_references() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "?v=2").as_deref(),
            Some("https://example.com/cfg/Main.pkl?v=2")
        );
    }

    #[test]
    fn http_relative_resolves_fragment_only_references() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "#frag").as_deref(),
            Some("https://example.com/cfg/Main.pkl#frag")
        );
    }

    #[test]
    fn http_relative_resolves_network_path_references() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "//cdn.example/Lib.pkl")
                .as_deref(),
            Some("https://cdn.example/Lib.pkl")
        );
    }
}

impl Evaluator {
    async fn same_local_path(&mut self, left: &Path, right: &Path) -> Result<bool> {
        if left == right {
            return Ok(true);
        }
        let left_key = self
            .capabilities
            .canonicalize(left)
            .await
            .unwrap_or_else(|_| left.to_path_buf());
        let right_key = self
            .capabilities
            .canonicalize(right)
            .await
            .unwrap_or_else(|_| right.to_path_buf());
        Ok(left_key == right_key)
    }

    async fn bind_deferred_inherited_imports(
        &mut self,
        deferred: &[(String, PathBuf)],
        inherited_path: &Path,
        inherited_val: &Value,
        scope: &mut Scope,
    ) -> Result<()> {
        for (alias, alias_path) in deferred {
            if self.same_local_path(alias_path, inherited_path).await? {
                scope.set(alias.clone(), inherited_val.clone());
            }
        }
        Ok(())
    }
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

#[cfg(test)]
mod auto_trait_tests {
    use super::Evaluator;

    #[test]
    fn evaluator_is_sync() {
        fn assert_sync<T: Sync>() {}

        assert_sync::<Evaluator>();
    }
}

#[cfg(test)]
mod package_uri_tests {
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    use std::path::PathBuf;

    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    use super::Evaluator;
    use super::{PackageSource, resolve_package_uri};

    #[test]
    fn atomic_write_replaces_existing_file() {
        let test_dir = std::env::temp_dir().join(format!(
            "pklr-write-atomic-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        let path = test_dir.join("package.pkl");

        super::write_atomic(&path, b"old").unwrap();
        super::write_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        std::fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn generic_package_uri_resolves_to_zip_url() {
        let pkg =
            resolve_package_uri("package://example.com/v1.26.0/hk@1.26.0#/Config.pkl").unwrap();
        match pkg {
            PackageSource::Zip(zip_url, entry) => {
                assert_eq!(zip_url, "https://example.com/v1.26.0/hk@1.26.0.zip");
                assert_eq!(entry, "Config.pkl");
            }
            PackageSource::Direct { .. } => panic!("expected zip package source"),
        }
    }

    #[test]
    #[cfg(feature = "package-zip")]
    fn package_archive_validation_reads_entry_payloads() {
        use std::io::Write;

        let payload = b"payload whose checksum must be validated";
        let mut bytes = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut bytes);
            let mut archive = zip::ZipWriter::new(cursor);
            archive
                .start_file(
                    "Config.pkl",
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            archive.write_all(payload).unwrap();
            archive.finish().unwrap();
        }
        let payload_start = bytes
            .windows(payload.len())
            .position(|window| window == payload)
            .unwrap();
        bytes[payload_start] ^= 0xff;

        let error = super::validate_package_bytes("https://example.com/package.zip", "zip", &bytes)
            .unwrap_err()
            .to_string();
        assert!(error.contains("package archive entry is invalid"));
    }

    #[test]
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    fn package_dir_lookup_uses_source_zip_url() {
        let mut evaluator = Evaluator::new();
        evaluator.set_http_rewrites(&["https://example.com/=https://mirror.local/".to_string()]);
        let dir = PathBuf::from("/tmp/pklr-test-package");
        evaluator
            .package_dirs
            .insert("https://example.com/pkg@1.0.zip".to_string(), dir.clone());

        assert_eq!(
            evaluator
                .package_dir_for_zip("https://example.com/pkg@1.0.zip")
                .cloned(),
            Some(dir)
        );
    }

    #[test]
    fn generic_package_uri_rejects_path_traversal_entries() {
        let err = resolve_package_uri("package://example.com/pkg@1.0#/../secret.pkl")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid package entry path"));
    }

    #[test]
    fn malformed_registry_uri_does_not_fall_back_to_generic_zip() {
        let err =
            resolve_package_uri("package://pkg.pkl-lang.org/github.com/owner/repo#/Config.pkl")
                .unwrap_err()
                .to_string();
        assert!(err.contains("unsupported package URI"));
    }
}
