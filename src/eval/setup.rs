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

    pub(super) fn is_file_io(&mut self, path: &Path) -> Result<bool> {
        self.capabilities.is_file(path)
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

    pub(super) fn resource_path_exists_io(&mut self, path: &Path) -> Result<bool> {
        self.capabilities.resource_path_exists(path)
    }

    pub(super) fn resource_is_file_io(&mut self, path: &Path) -> Result<bool> {
        self.capabilities.resource_is_file(path)
    }

    pub(super) fn resource_read_bytes_io(&mut self, path: &Path) -> Result<Vec<u8>> {
        self.capabilities.resource_read_bytes(path)
    }

    pub(super) fn resource_glob_io(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
        self.capabilities.resource_glob(base, pattern)
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
            max_depth: crate::stack::SMALL_STACK_EVAL_DEPTH,
            max_depth_set: false,
            http_cache: HashMap::default(),
            import_cache: HashMap::default(),
            imports_in_flight: HashSet::default(),
            type_violation: None,
            constraint_nesting: 0,
            deferred_const: HashMap::default(),
            leaked_this: None,
            failed_seen: false,
            last_coerced: None,
            eager_param_checks: 0,
            placeholder_reads: 0,
            narrowed_import_cache: HashMap::default(),
            narrowed_in_flight: HashSet::default(),
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
            module_path: Vec::new(),
            allowed_resources: DEFAULT_ALLOWED_RESOURCES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allowed_modules: None,
            package_bases: HashMap::default(),
            module_path_admitted: HashMap::default(),
            environment: None,
            external_properties: BTreeMap::default(),
            module_scopes: HashMap::default(),
            inherited_bases: HashMap::default(),
            scope_keys: HashMap::default(),
            layered_scopes: HashMap::default(),
            rendered_member_failed: false,
            module_arg_members: HashMap::default(),
            module_arg_used: false,
            env_reads: BTreeMap::new(),
            scoped_imports_in_flight: HashSet::default(),
            capabilities: Box::new(capabilities),
            cancel: None,
            #[cfg(feature = "package-zip")]
            package_dirs: HashMap::default(),
            package_checksums: HashMap::default(),
            #[cfg(feature = "package-zip")]
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
            output_format: render::RendererKind::Json,
            warned_deprecated: std::collections::HashSet::default(),
            active_typed_defaults: HashSet::default(),
            mapping_key_values: HashMap::default(),
            module_names: HashMap::default(),
            module_declarations: HashMap::default(),
            parsed_declarations: HashMap::default(),
            grew: false,
        }
    }

    /// Limit how deeply evaluation may recurse before it fails with an error.
    /// The default suits the large stack that evaluation runs on.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth;
        self.max_depth_set = true;
    }

    /// Run `body`, which starts an evaluation, on the caller's stack and, if
    /// that reaches the nesting limit, again on a large one with a higher
    /// default limit.
    pub(super) fn on_grown_stack<R: Send>(
        &mut self,
        mut body: impl FnMut(&mut Self) -> R + Send,
    ) -> R {
        /// Restores the evaluator's depth limit even if `body` panics.
        struct Restore<'a>(&'a mut Evaluator, usize);

        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.max_depth = self.1;
            }
        }

        crate::stack::run(|large| {
            let saved = self.max_depth;
            let guard = Restore(&mut *self, saved);
            if large {
                guard.0.grew = true;
            }
            if large && !guard.0.max_depth_set {
                guard.0.max_depth = crate::stack::EVAL_DEPTH;
            }
            let result = body(&mut *guard.0);
            drop(guard);
            if large {
                // These hold the parsed trees, which free recursively; do it
                // here rather than on the caller's small stack.
                self.parse_cache.clear();
                self.plan_cache.clear();
                self.body_roots_cache.clear();
                self.module_declarations.clear();
                self.parsed_declarations.clear();
            }
            result
        })
    }

    pub fn set_base_path(&mut self, path: &Path) {
        self.base_path = path.to_path_buf();
    }

    /// Set the directories `modulepath:` module URIs are searched in, in order.
    pub fn set_module_path(&mut self, dirs: Vec<PathBuf>) {
        self.module_path = dirs;
    }

    pub fn set_allowed_resources(&mut self, resources: Vec<String>) {
        self.allowed_resources = resources;
    }

    /// Restrict which modules may be loaded, like `pkl --allowed-modules`.
    ///
    /// Each pattern is a regular expression matched against the start of a
    /// module's URI (the entry module, and every module reached through
    /// `import`, `import*`, `amends` and `extends`). `pkl:` standard library
    /// modules are always allowed. An empty list denies every other module.
    /// An invalid pattern makes every module load fail with its error.
    pub fn set_allowed_modules(&mut self, patterns: Vec<String>) {
        self.allowed_modules = Some(
            patterns
                .iter()
                .map(|pattern| {
                    fancy_regex::Regex::new(&format!("^(?:{pattern})"))
                        .map_err(|e| format!("invalid allowed-modules pattern `{pattern}`: {e}"))
                })
                .collect(),
        );
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
        self.release_deep_state();
        self.prune_lambda_guards();
        self.env_reads.clear();
        self.import_cache.clear();
        self.imports_in_flight.clear();
        self.placeholder_reads = 0;
        self.narrowed_import_cache.clear();
        self.parse_cache.clear();
        self.body_roots_cache.clear();
        self.plan_cache.clear();
        self.deferred_const.clear();
        self.resource_cache.clear();
        clear_names();
        self.module_scopes.clear();
        self.inherited_bases.clear();
        self.scope_keys.clear();
        self.layered_scopes.clear();
        self.rendered_member_failed = false;
        self.module_arg_members.clear();
        self.scoped_imports_in_flight.clear();
        self.converters.clear();
        self.output_props.clear();
        self.module_output = None;
        self.output_sets_value = false;
        self.prefetch = prefetch::PrefetchState::new(self.cancel.clone());
        self.active_typed_defaults.clear();
        self.mapping_key_values.clear();
        crate::value::clear_compound_keys();
        self.module_names.clear();
        if self.grew {
            // Alias targets can nest deeply, so free them on a large stack.
            crate::stack::drop_on_large_stack((
                std::mem::take(&mut self.module_declarations),
                std::mem::take(&mut self.parsed_declarations),
            ));
        } else {
            self.module_declarations.clear();
            self.parsed_declarations.clear();
        }
        self.module_path_admitted.clear();
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

impl Evaluator {
    /// Frees the values and trees a deep module left behind on a large stack.
    /// They free recursively, and this runs on the caller's stack, which may
    /// be small.
    fn release_deep_state(&mut self) {
        if !self.grew || crate::stack::on_large_stack() {
            return;
        }
        self.grew = false;
        let held = (
            std::mem::take(&mut self.import_cache),
            std::mem::take(&mut self.narrowed_import_cache),
            std::mem::take(&mut self.parse_cache),
            std::mem::take(&mut self.body_roots_cache),
            std::mem::take(&mut self.plan_cache),
            std::mem::take(&mut self.resource_cache),
            std::mem::take(&mut self.module_scopes),
            std::mem::take(&mut self.layered_scopes),
            std::mem::take(&mut self.converters),
            std::mem::take(&mut self.builtin_classes),
            std::mem::take(&mut self.output_props),
            self.module_output.take(),
            std::mem::take(&mut self.mapping_key_values),
            std::mem::take(&mut self.deferred_const),
            self.last_coerced.take(),
            std::mem::take(&mut self.module_declarations),
            std::mem::take(&mut self.parsed_declarations),
        );
        crate::stack::drop_on_large_stack(held);
    }
}

impl Drop for Evaluator {
    fn drop(&mut self) {
        self.release_deep_state();
    }
}
