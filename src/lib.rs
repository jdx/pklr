pub mod error;
pub mod eval;
pub mod lexer;
pub mod parser;
pub mod value;

pub use error::{Error, Result};
pub use eval::Evaluator;
pub use value::Value;

/// Re-export reqwest so consumers can build a Client without a separate dependency.
pub use reqwest;

use std::path::Path;

/// Evaluate a pkl file and return its contents as a JSON value.
/// This is the primary entry point for use in tools like hk.
pub async fn eval_to_json(path: &Path) -> Result<serde_json::Value> {
    eval_to_json_with_client(path, None).await
}

/// Options for configuring the pkl evaluator.
#[derive(Default)]
pub struct EvalOptions {
    /// Custom HTTP client for proxy/CA configuration.
    pub client: Option<reqwest::Client>,
    /// HTTP URL rewrite rules in `"source_prefix=target_prefix"` format.
    /// Matches pkl CLI's `--http-rewrite` behavior: longest matching prefix wins.
    pub http_rewrites: Vec<String>,
}

/// Evaluate a pkl file with a custom HTTP client for proxy/CA configuration.
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

/// Evaluate a pkl file with full configuration options.
pub async fn eval_to_json_with_options(
    path: &Path,
    options: EvalOptions,
) -> Result<serde_json::Value> {
    let source = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    let mut evaluator = Evaluator::new();
    evaluator.set_base_path(path.parent().unwrap_or(Path::new(".")));
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

/// Analyze imports of a pkl file, returning all transitive local file dependencies.
pub fn analyze_imports(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let source = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    let tokens = lexer::lex_named(&source, &path.display().to_string())?;
    let imports = parser::collect_imports(&tokens);
    let base = path.parent().unwrap_or(Path::new("."));
    let mut results = Vec::new();
    for uri in imports {
        if let Some(rel) = uri.strip_prefix("file://") {
            results.push(std::path::PathBuf::from(rel));
        } else if !uri.contains("://") {
            if uri.contains('*') {
                // Expand glob patterns to actual files
                if let Ok(expanded) = eval::expand_glob(base, &uri) {
                    results.extend(expanded);
                }
            } else {
                results.push(base.join(&uri));
            }
        }
    }
    Ok(results)
}
