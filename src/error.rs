// The Lex/Parse variant fields are read by miette's Diagnostic derive macro,
// but rustc can't see through the proc-macro expansion.
#![allow(unused_assignments)]

use std::path::PathBuf;

use miette::NamedSource;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Error {
    #[error("IO error reading {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),

    #[error("{message}")]
    #[diagnostic()]
    Lex {
        #[source_code]
        src: NamedSource<String>,
        #[label("{message}")]
        span: miette::SourceOffset,
        message: String,
    },

    #[error("{message}")]
    #[diagnostic()]
    Parse {
        #[source_code]
        src: NamedSource<String>,
        #[label("{message}")]
        span: miette::SourceOffset,
        message: String,
    },

    #[error("Eval error: {0}")]
    Eval(String),

    #[error("Import not found: {0}")]
    ImportNotFound(String),

    #[error("Unsupported feature: {0}")]
    Unsupported(String),
}
