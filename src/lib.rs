#[cfg(feature = "eval-core")]
pub mod capabilities;
pub mod error;
#[cfg(feature = "eval-core")]
pub mod eval;
pub mod lexer;
pub mod parser;
#[cfg(feature = "eval-core")]
pub mod value;

#[cfg(feature = "native-io")]
pub use capabilities::NativeCapabilities;
#[cfg(feature = "eval-core")]
pub use capabilities::{EvalCapabilities, FetchBudget};
pub use error::{Error, Result};
#[cfg(feature = "eval-core")]
pub use eval::Evaluator;
#[cfg(feature = "eval-core")]
pub use value::Value;

/// Re-export reqwest so consumers can build a client for
/// [`EvaluatorBuilder::http_client`] without a separate dependency.
#[cfg(feature = "async")]
pub use reqwest;
/// Re-export ureq so consumers can configure an HTTP agent without a separate
/// dependency.
#[cfg(feature = "http")]
pub use ureq;

#[cfg(feature = "native-io")]
use std::path::Path;

/// The result of an evaluation together with its environment dependencies.
#[cfg(feature = "native-io")]
#[derive(Debug, Clone, PartialEq)]
pub struct EvalOutcome {
    /// The evaluated pkl document as JSON.
    pub json: serde_json::Value,
    /// Environment variables read during evaluation (name → observed value).
    ///
    /// Missing variables are included with a `None` value. Entries are ordered
    /// by variable name so callers can serialize or hash them deterministically.
    pub env_reads: std::collections::BTreeMap<String, Option<String>>,
}

/// Evaluate a Pkl file and return its contents as JSON.
#[cfg(feature = "native-io")]
pub fn eval_to_json(path: &Path) -> Result<serde_json::Value> {
    EvaluatorBuilder::new().eval_to_json(path)
}

/// Evaluate a Pkl file and render its `output.text`, or its default PCF output.
#[cfg(feature = "native-io")]
pub fn eval_to_text(path: &Path) -> Result<String> {
    EvaluatorBuilder::new().eval_to_text(path)
}

/// Evaluate a Pkl file on tokio's blocking thread pool and return its
/// contents as JSON. Must be called from within a tokio runtime.
#[cfg(feature = "async")]
pub async fn eval_to_json_async(path: &Path) -> Result<serde_json::Value> {
    EvaluatorBuilder::new().eval_to_json_async(path).await
}

/// Options for [`eval_with_options`].
#[cfg(feature = "native-io")]
#[derive(Default)]
pub struct EvalOptions {
    /// Custom HTTP agent for proxy, CA, or timeout configuration.
    #[cfg(feature = "http")]
    pub agent: Option<ureq::Agent>,
    /// HTTP URL rewrite rules in `"source_prefix=target_prefix"` format.
    /// Matches pkl CLI's `--http-rewrite` behavior: longest matching prefix wins.
    pub http_rewrites: Vec<String>,
}

/// Builder for configuring a Pkl evaluator with the native host capabilities.
#[cfg(feature = "native-io")]
#[derive(Default)]
pub struct EvaluatorBuilder {
    #[cfg(feature = "http")]
    agent: Option<ureq::Agent>,
    #[cfg(feature = "async")]
    client: Option<reqwest::Client>,
    http_rewrites: Vec<String>,
    package_cache_dir: Option<std::path::PathBuf>,
    offline: bool,
    allowed_resources: Option<Vec<String>>,
    environment: Option<std::collections::BTreeMap<String, String>>,
    external_properties: std::collections::BTreeMap<String, String>,
    preloaded_packages: Vec<PreloadedPackage>,
}

/// Package content a host supplies up front instead of fetching it.
#[cfg(feature = "native-io")]
struct PreloadedPackage {
    url: String,
    extension: String,
    bytes: std::borrow::Cow<'static, [u8]>,
}

#[cfg(feature = "native-io")]
impl EvaluatorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a custom HTTP agent for proxy, certificate, or timeout configuration.
    #[cfg(feature = "http")]
    pub fn http_agent(mut self, agent: ureq::Agent) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Fetch over HTTP with a `reqwest` client on tokio instead of ureq.
    /// Takes precedence over [`http_agent`](Self::http_agent).
    #[cfg(feature = "async")]
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Add HTTP URL rewrite rules in `"source_prefix=target_prefix"` format.
    pub fn http_rewrites(mut self, rules: impl IntoIterator<Item = String>) -> Self {
        self.http_rewrites.extend(rules);
        self
    }

    /// Persist downloaded `package://` content under `path`.
    pub fn package_cache_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.package_cache_dir = Some(path.into());
        self
    }

    /// Disable network access while allowing cached packages to load.
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// Restrict resource reads to URI prefixes.
    pub fn allowed_resources(mut self, resources: impl IntoIterator<Item = String>) -> Self {
        self.allowed_resources = Some(resources.into_iter().collect());
        self
    }

    /// Replace the environment visible through `env:` resources.
    pub fn environment_variables(
        mut self,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.environment = Some(vars.into_iter().collect());
        self
    }

    /// Add properties visible through `prop:` resources.
    pub fn external_properties(
        mut self,
        properties: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.external_properties.extend(properties);
        self
    }

    /// Seed the package cache with content the host already has, instead of
    /// fetching it. Requires [`package_cache_dir`](Self::package_cache_dir).
    pub fn preload_package(
        mut self,
        url: impl Into<String>,
        extension: impl Into<String>,
        bytes: impl Into<std::borrow::Cow<'static, [u8]>>,
    ) -> Self {
        self.preloaded_packages.push(PreloadedPackage {
            url: url.into(),
            extension: extension.into(),
            bytes: bytes.into(),
        });
        self
    }

    /// Build a configured evaluator for direct source evaluation.
    ///
    /// A package that fails to preload is skipped and fetched normally.
    pub fn build(self) -> Evaluator {
        #[cfg(feature = "async")]
        let (capabilities, agent) = match self.client {
            Some(client) => (Some(NativeCapabilities::with_reqwest_client(client)), None),
            None => (None, self.agent),
        };
        #[cfg(all(feature = "http", not(feature = "async")))]
        let (capabilities, agent) = (None, self.agent);
        #[cfg(feature = "http")]
        let capabilities = capabilities.unwrap_or_else(|| match agent {
            Some(agent) => NativeCapabilities::with_http_agent(agent),
            None => NativeCapabilities::new(),
        });
        #[cfg(not(feature = "http"))]
        let capabilities = NativeCapabilities::new();
        let mut evaluator = Evaluator::with_capabilities(capabilities);
        evaluator.set_http_rewrites(&self.http_rewrites);
        if let Some(cache_dir) = self.package_cache_dir {
            evaluator.set_package_cache_dir(cache_dir);
        }
        evaluator.set_offline(self.offline);
        if let Some(resources) = self.allowed_resources {
            evaluator.set_allowed_resources(resources);
        }
        if let Some(environment) = self.environment {
            evaluator.set_environment_variables(environment);
        }
        evaluator.set_external_properties(self.external_properties);
        for package in &self.preloaded_packages {
            let _ = evaluator.preload_package(&package.url, &package.extension, &package.bytes);
        }
        evaluator
    }

    /// Evaluate a Pkl file and return its JSON value.
    pub fn eval_to_json(self, path: &Path) -> Result<serde_json::Value> {
        Ok(self.eval(path)?.json)
    }

    /// Evaluate a Pkl file and render its `output.text`, or its default PCF output.
    pub fn eval_to_text(self, path: &Path) -> Result<String> {
        let mut evaluator = self.build();
        evaluator.set_base_path(path.parent().unwrap_or(Path::new(".")));
        evaluator.eval_file_text_blocking(path)
    }

    /// Evaluate a Pkl file and return its JSON and environment dependencies.
    pub fn eval(self, path: &Path) -> Result<EvalOutcome> {
        self.eval_with_cancel(path, None)
    }

    fn eval_with_cancel(
        self,
        path: &Path,
        cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<EvalOutcome> {
        let mut evaluator = self.build();
        if let Some(cancel) = cancel {
            evaluator.set_cancel_flag(cancel);
        }
        evaluator.set_base_path(path.parent().unwrap_or(Path::new(".")));
        let json = evaluator.eval_file_json_blocking(path)?;
        Ok(EvalOutcome {
            json,
            env_reads: evaluator.take_env_reads(),
        })
    }

    /// [`eval_to_json`](Self::eval_to_json) on tokio's blocking thread pool.
    /// Must be called from within a tokio runtime.
    #[cfg(feature = "async")]
    pub async fn eval_to_json_async(self, path: &Path) -> Result<serde_json::Value> {
        Ok(self.eval_async(path).await?.json)
    }

    /// [`eval`](Self::eval) on tokio's blocking thread pool, so the
    /// synchronous evaluation does not block the caller's runtime. Must be
    /// called from within a tokio runtime.
    ///
    /// Dropping the returned future (for example on a timeout) cancels the
    /// evaluation: it stops at its next expression, read or fetch, and starts
    /// no further downloads.
    #[cfg(feature = "async")]
    pub async fn eval_async(self, path: &Path) -> Result<EvalOutcome> {
        /// Cancels the evaluation when the future holding it is dropped.
        struct CancelOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let path = path.to_path_buf();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _guard = CancelOnDrop(cancel.clone());
        let task = tokio::task::spawn_blocking(move || self.eval_with_cancel(&path, Some(cancel)));
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(Error::Eval(format!("evaluation task failed: {error}"))),
        }
    }
}

/// Evaluate a Pkl file with options and return its result and dependencies.
#[cfg(feature = "native-io")]
pub fn eval_with_options(path: &Path, options: EvalOptions) -> Result<EvalOutcome> {
    let builder = EvaluatorBuilder::new().http_rewrites(options.http_rewrites);
    #[cfg(feature = "http")]
    let builder = match options.agent {
        Some(agent) => builder.http_agent(agent),
        None => builder,
    };
    builder.eval(path)
}

/// Analyze imports of a pkl file, returning all transitive local file dependencies.
#[cfg(feature = "native-io")]
pub fn analyze_imports(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut results = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut seen_results = std::collections::HashSet::new();
    analyze_imports_inner(path, &mut visited, &mut seen_results, &mut results)?;
    Ok(results)
}

#[cfg(feature = "native-io")]
fn analyze_imports_inner(
    path: &Path,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
    seen_results: &mut std::collections::HashSet<std::path::PathBuf>,
    results: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }
    let source = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    let tokens = lexer::lex_named(&source, &path.display().to_string())?;
    let imports = parser::collect_imports(&tokens);
    let base = path.parent().unwrap_or(Path::new("."));
    for uri in imports {
        let mut local_imports = Vec::new();
        if uri.starts_with("file:") {
            local_imports.push(eval::file_uri_path(&uri)?);
        } else if !uri.contains("://") {
            if uri.contains('*') {
                // Expand glob patterns to actual files
                if let Ok(expanded) = eval::expand_glob(base, &uri) {
                    local_imports.extend(expanded);
                }
            } else if let Some(triple_dot) = eval::parse_triple_dot_path(&uri)? {
                let exists = |candidate: &Path| Ok(candidate.exists());
                // `analyze_imports` uses the native file system, so resolve a
                // relative entry path before walking its ancestor directories.
                let path = if path.is_relative() {
                    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
                } else {
                    path.to_path_buf()
                };
                local_imports.extend(eval::resolve_triple_dot(&path, triple_dot, None, exists)?);
            } else {
                local_imports.push(base.join(&uri));
            }
        }
        for import_path in local_imports {
            if !import_path.exists() {
                continue;
            }
            let result_key = import_path
                .canonicalize()
                .unwrap_or_else(|_| import_path.clone());
            if seen_results.insert(result_key) {
                results.push(import_path.clone());
            }
            analyze_imports_inner(&import_path, visited, seen_results, results)?;
        }
    }
    Ok(())
}
