#[cfg(feature = "eval-core")]
pub mod capabilities;
pub mod error;
#[cfg(feature = "eval-core")]
pub mod eval;
pub mod lexer;
pub mod parser;
#[cfg(feature = "eval-core")]
pub mod value;

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

#[cfg(feature = "native-io")]
use std::path::Path;

/// Evaluate a pkl file and return its contents as a JSON value.
/// This is the primary entry point for use in tools like hk.
#[cfg(feature = "native-io")]
pub async fn eval_to_json(path: &Path) -> Result<serde_json::Value> {
    eval_to_json_with_client(path, None).await
}

/// Options for configuring the pkl evaluator.
#[cfg(feature = "native-io")]
#[derive(Default)]
pub struct EvalOptions {
    /// Custom HTTP client for proxy/CA configuration.
    #[cfg(feature = "http")]
    pub client: Option<reqwest::Client>,
    /// HTTP URL rewrite rules in `"source_prefix=target_prefix"` format.
    /// Matches pkl CLI's `--http-rewrite` behavior: longest matching prefix wins.
    pub http_rewrites: Vec<String>,
}

/// Evaluate a pkl file with a custom HTTP client for proxy/CA configuration.
#[cfg(all(feature = "native-io", feature = "http"))]
pub async fn eval_to_json_with_client(
    path: &Path,
    client: Option<reqwest::Client>,
) -> Result<serde_json::Value> {
    eval_to_json_with_options(
        path,
        EvalOptions {
            client,
            ..Default::default()
        },
    )
    .await
}

#[cfg(all(feature = "native-io", not(feature = "http")))]
pub async fn eval_to_json_with_client(
    path: &Path,
    _client: Option<()>,
) -> Result<serde_json::Value> {
    eval_to_json_with_options(path, EvalOptions::default()).await
}

/// Evaluate a pkl file with full configuration options.
#[cfg(feature = "native-io")]
pub async fn eval_to_json_with_options(
    path: &Path,
    options: EvalOptions,
) -> Result<serde_json::Value> {
    let source = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    let mut evaluator = Evaluator::new();
    evaluator.set_base_path(path.parent().unwrap_or(Path::new(".")));
    #[cfg(feature = "http")]
    if let Some(client) = options.client {
        evaluator.set_http_client(client);
    }
    if !options.http_rewrites.is_empty() {
        evaluator.set_http_rewrites(&options.http_rewrites);
    }
    let value = evaluator.eval_source(&source, path).await?;
    let value = evaluator.apply_converters(value).await?;
    Ok(value.to_json())
}

/// Evaluate a pkl file synchronously and return its contents as a JSON value.
///
/// This entry point is available with the `blocking` feature. It is intended
/// for synchronous applications; callers already running inside Tokio should
/// use [`eval_to_json`] instead.
#[cfg(feature = "blocking")]
pub fn eval_to_json_blocking(path: &Path) -> Result<serde_json::Value> {
    eval_to_json_with_options_blocking(path, EvalOptions::default())
}

/// Evaluate a pkl file synchronously with full evaluator options.
#[cfg(feature = "blocking")]
pub fn eval_to_json_with_options_blocking(
    path: &Path,
    options: EvalOptions,
) -> Result<serde_json::Value> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(Error::Eval(
            "blocking evaluation cannot run inside a Tokio runtime; use the async API instead"
                .to_string(),
        ));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Eval(format!("failed to create Tokio runtime: {error}")))?;
    runtime.block_on(eval_to_json_with_options(path, options))
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

#[cfg(all(test, feature = "blocking"))]
mod blocking_tests {
    use super::{eval_to_json, eval_to_json_blocking};
    use std::path::Path;

    const FIXTURE: &str = "tests/fixtures/base.pkl";

    #[test]
    fn blocking_evaluation_matches_async_evaluation() {
        let path = Path::new(FIXTURE);
        let blocking = eval_to_json_blocking(path).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let asynchronous = runtime.block_on(eval_to_json(path)).unwrap();

        assert_eq!(blocking, asynchronous);
    }

    #[test]
    fn blocking_evaluation_rejects_an_active_runtime() {
        let path = Path::new(FIXTURE);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime.block_on(async { eval_to_json_blocking(path).unwrap_err() });

        assert!(
            error
                .to_string()
                .contains("cannot run inside a Tokio runtime")
        );
    }
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
