pub mod error;
pub mod eval;
pub mod lexer;
pub mod parser;
pub mod value;

pub use error::{Error, Result};
pub use eval::Evaluator;
pub use value::Value;

use std::path::Path;

/// Evaluate a pkl file and return its contents as a JSON value.
/// This is the primary entry point for use in tools like hk.
pub async fn eval_to_json(path: &Path) -> Result<serde_json::Value> {
    let source = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    let mut evaluator = Evaluator::new();
    evaluator.set_base_path(path.parent().unwrap_or(Path::new(".")));
    let value = evaluator.eval_source(&source, path).await?;
    Ok(value.to_json())
}

/// Analyze imports of a pkl file, returning all transitive local file dependencies.
pub fn analyze_imports(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let source = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    let tokens = lexer::lex_named(&source, &path.display().to_string())?;
    let imports = parser::collect_imports(&tokens);
    let base = path.parent().unwrap_or(Path::new("."));
    Ok(imports
        .into_iter()
        .filter_map(|uri| {
            if let Some(rel) = uri.strip_prefix("file://") {
                Some(std::path::PathBuf::from(rel))
            } else if !uri.contains("://") {
                Some(base.join(&uri))
            } else {
                None
            }
        })
        .collect())
}
