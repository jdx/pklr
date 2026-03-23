use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Error {
    #[error("IO error reading {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),

    #[error("Lex error at line {line}, col {col}: {message}")]
    Lex {
        line: usize,
        col: usize,
        message: String,
    },

    #[error("Parse error at line {line}, col {col}: {message}")]
    Parse {
        line: usize,
        col: usize,
        message: String,
    },

    #[error("Eval error: {0}")]
    Eval(String),

    #[error("Import not found: {0}")]
    ImportNotFound(String),

    #[error("Unsupported feature: {0}")]
    Unsupported(String),
}
