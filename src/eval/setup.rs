//! Constructing and configuring an [`Evaluator`], and its access to the host
//! capabilities.

use super::*;

/// Access to the host capabilities.
impl Evaluator {
    pub(super) fn read_to_string_io(&mut self, path: &Path) -> Result<String> {
        self.check_cancelled()?;
        self.capabilities.read_to_string(path)
    }

    pub(super) fn path_exists_io(&mut self, path: &Path) -> Result<bool> {
        self.capabilities.path_exists(path)
    }

    pub(super) fn canonicalize_io(&mut self, path: &Path) -> Result<PathBuf> {
        self.capabilities.canonicalize(path)
    }

    pub(super) fn read_bytes_io(&mut self, path: &Path) -> Result<Vec<u8>> {
        self.capabilities.read_bytes(path)
    }

    pub(super) fn create_dir_all_io(&mut self, path: &Path) -> Result<()> {
        self.capabilities.create_dir_all(path)
    }

    pub(super) fn write_atomic_io(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.capabilities.write_atomic(path, bytes)
    }

    pub(super) fn remove_file_io(&mut self, path: &Path) -> Result<()> {
        self.capabilities.remove_file(path)
    }

    #[cfg(feature = "package-zip")]
    pub(super) fn extract_zip_io(&mut self, bytes: Vec<u8>, destination: &Path) -> Result<()> {
        self.capabilities.extract_zip(bytes, destination)
    }

    pub(super) fn read_env_io(&mut self, name: &str) -> Result<Option<String>> {
        self.capabilities.read_env(name)
    }

    pub(super) fn fetch_text_io(&mut self, url: &str) -> Result<String> {
        self.check_cancelled()?;
        self.capabilities.fetch_text(url)
    }

    pub(super) fn fetch_bytes_io(&mut self, url: &str) -> Result<Vec<u8>> {
        self.check_cancelled()?;
        self.capabilities.fetch_bytes(url)
    }

    #[cfg(feature = "package-zip")]
    pub(super) fn temp_dir_io(&mut self, prefix: &str) -> Result<PathBuf> {
        self.capabilities.temp_dir(prefix)
    }

    pub(super) fn glob_io(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
        self.capabilities.glob(base, pattern)
    }
}

impl Evaluator {
    /// Construct an evaluator with the native host capabilities.
    #[cfg(feature = "native-io")]
    pub fn new() -> Self {
        Self::with_capabilities(crate::capabilities::NativeCapabilities::new())
    }

    pub fn with_capabilities(capabilities: impl EvalCapabilities + 'static) -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::default(),
            import_cache: HashMap::default(),
            imports_in_flight: HashSet::default(),
            placeholder_reads: 0,
            narrowed_import_cache: HashMap::default(),
            parse_cache: HashMap::default(),
            body_roots_cache: HashMap::default(),
            plan_cache: HashMap::default(),
            lambda_guards: HashMap::default(),
            lambda_guard_prune_at: LAMBDA_GUARD_PRUNE_MIN,
            #[cfg(test)]
            lambda_guard_prune_scans: 0,
            #[cfg(test)]
            late_passes_run: 0,
            resource_cache: HashMap::default(),
            allowed_resources: DEFAULT_ALLOWED_RESOURCES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            environment: None,
            external_properties: BTreeMap::default(),
            module_scopes: HashMap::default(),
            rendered_member_failed: false,
            env_reads: BTreeMap::new(),
            scoped_imports_in_flight: HashSet::default(),
            capabilities: Box::new(capabilities),
            cancel: None,
            #[cfg(feature = "package-zip")]
            package_dirs: HashMap::default(),
            package_checksums: HashMap::default(),
            verified_packages: HashMap::default(),
            package_cache_dir: None,
            package_http_roots: HashSet::default(),
            prefetch: prefetch::PrefetchState::default(),
            offline: false,
            http_rewrites: Vec::new(),
            converters: Vec::new(),
            builtin_classes: HashMap::default(),
            output_props: Vec::new(),
            module_output: None,
            output_sets_value: false,
            output_sets_omit_nulls: false,
            output_format: render::RendererKind::Json,
            warned_deprecated: std::collections::HashSet::default(),
            active_typed_defaults: HashSet::default(),
            mapping_key_values: HashMap::default(),
            module_names: HashMap::default(),
        }
    }

    pub fn set_base_path(&mut self, path: &Path) {
        self.base_path = path.to_path_buf();
    }

    pub fn set_allowed_resources(&mut self, resources: Vec<String>) {
        self.allowed_resources = resources;
    }

    pub fn set_environment_variables(&mut self, vars: BTreeMap<String, String>) {
        self.environment = Some(vars);
    }

    pub fn set_external_properties(&mut self, properties: BTreeMap<String, String>) {
        self.external_properties = properties;
    }

    /// Persist downloaded package content under `path`.
    pub fn set_package_cache_dir(&mut self, path: impl Into<PathBuf>) {
        self.package_cache_dir = Some(path.into());
    }

    /// Disable network access. Package imports can still use the persistent cache.
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    /// Stop evaluating, with an "evaluation cancelled" error, once `flag` is
    /// set, for example from another thread after a timeout. The evaluator
    /// checks it before each expression, module read and fetch, and starts
    /// no further prefetch requests once it is set.
    pub fn set_cancel_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.cancel = Some(flag);
    }

    /// Fail if the cancel flag is set. A single `Option` test when there is
    /// none.
    #[inline]
    pub(super) fn check_cancelled(&self) -> Result<()> {
        match &self.cancel {
            Some(flag) if flag.load(std::sync::atomic::Ordering::Relaxed) => Err(cancelled()),
            _ => Ok(()),
        }
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

    pub(super) fn begin_evaluation(&mut self) {
        // Lambda values can outlive an evaluation, so retain guards for every
        // capture map that is still reachable through one. Dead capture maps
        // are otherwise only held here weakly; drop their registry entries at
        // each evaluation boundary.
        self.prune_lambda_guards();
        self.env_reads.clear();
        self.import_cache.clear();
        self.imports_in_flight.clear();
        self.placeholder_reads = 0;
        self.narrowed_import_cache.clear();
        self.parse_cache.clear();
        self.body_roots_cache.clear();
        self.plan_cache.clear();
        self.resource_cache.clear();
        clear_names();
        self.module_scopes.clear();
        self.rendered_member_failed = false;
        self.scoped_imports_in_flight.clear();
        self.converters.clear();
        self.output_props.clear();
        self.module_output = None;
        self.output_sets_value = false;
        self.output_sets_omit_nulls = false;
        self.prefetch = prefetch::PrefetchState::new(self.cancel.clone());
        self.active_typed_defaults.clear();
        self.mapping_key_values.clear();
        self.module_names.clear();
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
    pub(super) fn warn_if_deprecated_access(
        &mut self,
        source: &Option<Arc<ObjectSource>>,
        field: &str,
    ) {
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
}
