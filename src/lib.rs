#[cfg(feature = "eval-core")]
pub mod capabilities;
pub mod error;
#[cfg(feature = "eval-core")]
pub mod eval;
pub mod lexer;
pub mod parser;
#[cfg(feature = "eval-core")]
pub mod value;

#[cfg(feature = "blocking")]
pub use capabilities::BlockingCapabilities;
#[cfg(feature = "eval-core")]
pub use capabilities::EvalCapabilities;
#[cfg(feature = "native-io")]
pub use capabilities::NativeCapabilities;
pub use error::{Error, Result};
#[cfg(feature = "eval-core")]
pub use eval::Evaluator;
#[cfg(feature = "eval-core")]
pub use value::Value;

/// Re-export reqwest so consumers can build a Client without a separate dependency.
#[cfg(feature = "http")]
pub use reqwest;
/// Re-export ureq so blocking consumers can configure an HTTP agent.
#[cfg(feature = "blocking")]
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

/// Evaluate a Pkl file synchronously and return its contents as JSON.
#[cfg(feature = "blocking")]
pub fn eval_to_json(path: &Path) -> Result<serde_json::Value> {
    EvaluatorBuilder::new().eval_to_json(path)
}

/// Evaluate a Pkl file asynchronously and return its contents as JSON.
#[cfg(feature = "async")]
pub async fn eval_to_json_async(path: &Path) -> Result<serde_json::Value> {
    eval_to_json_with_client_async(path, None).await
}

/// Options for configuring the pkl evaluator.
#[cfg(feature = "async")]
#[derive(Default)]
pub struct AsyncEvalOptions {
    /// Custom HTTP client for proxy/CA configuration.
    #[cfg(feature = "http")]
    pub client: Option<reqwest::Client>,
    /// HTTP URL rewrite rules in `"source_prefix=target_prefix"` format.
    /// Matches pkl CLI's `--http-rewrite` behavior: longest matching prefix wins.
    pub http_rewrites: Vec<String>,
}

/// Options for configuring the blocking Pkl evaluator.
#[cfg(feature = "blocking")]
#[derive(Default)]
pub struct EvalOptions {
    /// Custom synchronous HTTP agent for proxy, CA, or timeout configuration.
    pub agent: Option<ureq::Agent>,
    /// HTTP URL rewrite rules in `"source_prefix=target_prefix"` format.
    pub http_rewrites: Vec<String>,
}

/// Extensible builder for configuring a Pkl evaluator.
#[cfg(feature = "async")]
#[derive(Default)]
pub struct AsyncEvaluatorBuilder {
    #[cfg(feature = "http")]
    client: Option<reqwest::Client>,
    http_rewrites: Vec<String>,
    package_cache_dir: Option<std::path::PathBuf>,
    offline: bool,
    preloaded_packages: Vec<PreloadedPackage>,
}

/// Extensible builder for synchronous Pkl evaluation.
#[cfg(feature = "blocking")]
#[derive(Default)]
pub struct EvaluatorBuilder {
    agent: Option<ureq::Agent>,
    http_rewrites: Vec<String>,
    package_cache_dir: Option<std::path::PathBuf>,
    offline: bool,
    preloaded_packages: Vec<PreloadedPackage>,
}

/// Package content a host supplies up front instead of fetching it.
#[cfg(feature = "native-io")]
struct PreloadedPackage {
    url: String,
    extension: String,
    bytes: std::borrow::Cow<'static, [u8]>,
}

#[cfg(feature = "async")]
impl AsyncEvaluatorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a custom HTTP client for proxy, certificate, or timeout configuration.
    #[cfg(feature = "http")]
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
    pub async fn build(self) -> Evaluator {
        let mut evaluator = Evaluator::new_async();
        #[cfg(feature = "http")]
        if let Some(client) = self.client {
            evaluator
                .set_http_client(client)
                .expect("native async capabilities accept reqwest clients");
        }
        evaluator.set_http_rewrites(&self.http_rewrites);
        if let Some(cache_dir) = self.package_cache_dir {
            evaluator.set_package_cache_dir(cache_dir);
        }
        evaluator.set_offline(self.offline);
        for package in &self.preloaded_packages {
            let _ = evaluator
                .preload_package_async(&package.url, &package.extension, &package.bytes)
                .await;
        }
        evaluator
    }

    /// Evaluate a Pkl file and return its JSON value.
    pub async fn eval_to_json(self, path: &Path) -> Result<serde_json::Value> {
        Ok(self.eval(path).await?.json)
    }

    /// Evaluate a Pkl file and return its JSON and environment dependencies.
    pub async fn eval(self, path: &Path) -> Result<EvalOutcome> {
        let evaluator = self.build().await;
        eval_with_evaluator(path, evaluator).await
    }
}

#[cfg(feature = "blocking")]
impl EvaluatorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a custom synchronous HTTP agent.
    pub fn http_agent(mut self, agent: ureq::Agent) -> Self {
        self.agent = Some(agent);
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

    /// Seed the package cache with content the host already has.
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
    pub fn build(self) -> Evaluator {
        let capabilities = match self.agent {
            Some(agent) => BlockingCapabilities::with_http_agent(agent),
            None => BlockingCapabilities::new(),
        };
        let mut evaluator = Evaluator::with_capabilities(capabilities);
        evaluator.set_http_rewrites(&self.http_rewrites);
        if let Some(cache_dir) = self.package_cache_dir {
            evaluator.set_package_cache_dir(cache_dir);
        }
        evaluator.set_offline(self.offline);
        for package in &self.preloaded_packages {
            let _ = evaluator.preload_package(&package.url, &package.extension, &package.bytes);
        }
        evaluator
    }

    /// Evaluate a Pkl file synchronously and return its JSON value.
    pub fn eval_to_json(self, path: &Path) -> Result<serde_json::Value> {
        Ok(self.eval(path)?.json)
    }

    /// Evaluate a Pkl file synchronously and return its result and dependencies.
    pub fn eval(self, path: &Path) -> Result<EvalOutcome> {
        pollster::block_on(eval_with_evaluator(path, self.build()))
    }
}

/// Evaluate a pkl file with a custom HTTP client for proxy/CA configuration.
#[cfg(feature = "async")]
pub async fn eval_to_json_with_client_async(
    path: &Path,
    client: Option<reqwest::Client>,
) -> Result<serde_json::Value> {
    eval_to_json_with_options_async(
        path,
        AsyncEvalOptions {
            client,
            ..Default::default()
        },
    )
    .await
}

/// Evaluate a Pkl file asynchronously with full configuration options.
#[cfg(feature = "async")]
pub async fn eval_to_json_with_options_async(
    path: &Path,
    options: AsyncEvalOptions,
) -> Result<serde_json::Value> {
    Ok(eval_with_options_async(path, options).await?.json)
}

/// Evaluate a Pkl file synchronously with full configuration options.
#[cfg(feature = "blocking")]
pub fn eval_to_json_with_options(path: &Path, options: EvalOptions) -> Result<serde_json::Value> {
    Ok(eval_with_options(path, options)?.json)
}

/// Evaluate a Pkl file asynchronously and return its result and dependencies.
#[cfg(feature = "async")]
pub async fn eval_with_options_async(
    path: &Path,
    options: AsyncEvalOptions,
) -> Result<EvalOutcome> {
    let builder = AsyncEvaluatorBuilder::new().http_rewrites(options.http_rewrites);
    let builder = match options.client {
        Some(client) => builder.http_client(client),
        None => builder,
    };
    builder.eval(path).await
}

/// Evaluate a Pkl file synchronously and return its result and dependencies.
#[cfg(feature = "blocking")]
pub fn eval_with_options(path: &Path, options: EvalOptions) -> Result<EvalOutcome> {
    let builder = EvaluatorBuilder::new().http_rewrites(options.http_rewrites);
    let builder = match options.agent {
        Some(agent) => builder.http_agent(agent),
        None => builder,
    };
    builder.eval(path)
}

#[cfg(feature = "native-io")]
async fn eval_with_evaluator(path: &Path, mut evaluator: Evaluator) -> Result<EvalOutcome> {
    evaluator.set_base_path(path.parent().unwrap_or(Path::new(".")));
    let value = evaluator.eval_file_pub(path).await?;
    let value = evaluator.apply_converters(value).await?;
    Ok(EvalOutcome {
        json: value.to_json(),
        env_reads: evaluator.take_env_reads(),
    })
}

/// Analyze imports of a pkl file, returning all transitive local file dependencies.
#[cfg(feature = "blocking")]
pub fn analyze_imports(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut results = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut seen_results = std::collections::HashSet::new();
    analyze_imports_inner(path, &mut visited, &mut seen_results, &mut results)?;
    Ok(results)
}

#[cfg(feature = "blocking")]
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
        if let Some(rel) = uri.strip_prefix("file://") {
            local_imports.push(std::path::PathBuf::from(rel));
        } else if !uri.contains("://") {
            if uri.contains('*') {
                // Expand glob patterns to actual files
                if let Ok(expanded) = eval::expand_glob(base, &uri) {
                    local_imports.extend(expanded);
                }
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

/// Analyze imports asynchronously, returning all transitive local file dependencies.
#[cfg(feature = "async")]
pub async fn analyze_imports_async(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut results = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut seen_results = std::collections::HashSet::new();
    let mut capabilities = NativeCapabilities::new();
    analyze_imports_inner_async(
        path,
        &mut capabilities,
        &mut visited,
        &mut seen_results,
        &mut results,
    )
    .await?;
    Ok(results)
}

#[cfg(feature = "async")]
#[async_recursion::async_recursion(?Send)]
async fn analyze_imports_inner_async(
    path: &Path,
    capabilities: &mut NativeCapabilities,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
    seen_results: &mut std::collections::HashSet<std::path::PathBuf>,
    results: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    let canonical = capabilities
        .canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }
    let source = capabilities.read_to_string(path).await?;
    let tokens = lexer::lex_named(&source, &path.display().to_string())?;
    let imports = parser::collect_imports(&tokens);
    let base = path.parent().unwrap_or(Path::new("."));
    for uri in imports {
        let mut local_imports = Vec::new();
        if let Some(rel) = uri.strip_prefix("file://") {
            local_imports.push(std::path::PathBuf::from(rel));
        } else if !uri.contains("://") {
            if uri.contains('*') {
                if let Ok(expanded) = capabilities.glob(base, &uri).await {
                    local_imports.extend(expanded);
                }
            } else {
                local_imports.push(base.join(&uri));
            }
        }
        for import_path in local_imports {
            if !capabilities
                .path_exists(&import_path)
                .await
                .unwrap_or(false)
            {
                continue;
            }
            let result_key = capabilities
                .canonicalize(&import_path)
                .await
                .unwrap_or_else(|_| import_path.clone());
            if seen_results.insert(result_key) {
                results.push(import_path.clone());
            }
            analyze_imports_inner_async(&import_path, capabilities, visited, seen_results, results)
                .await?;
        }
    }
    Ok(())
}
