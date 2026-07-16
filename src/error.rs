use std::path::PathBuf;

#[cfg(feature = "miette-diagnostics")]
use miette::NamedSource;

pub type Result<T> = std::result::Result<T, Error>;

#[cfg_attr(feature = "miette-diagnostics", derive(miette::Diagnostic))]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error reading {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),

    #[error("{message}")]
    #[cfg_attr(feature = "miette-diagnostics", diagnostic())]
    Lex {
        #[cfg(feature = "miette-diagnostics")]
        #[source_code]
        src: NamedSource<String>,
        #[cfg(feature = "miette-diagnostics")]
        #[label("{message}")]
        span: miette::SourceOffset,
        #[cfg(not(feature = "miette-diagnostics"))]
        source_name: String,
        #[cfg(not(feature = "miette-diagnostics"))]
        offset: usize,
        message: String,
    },

    #[error("{message}")]
    #[cfg_attr(feature = "miette-diagnostics", diagnostic())]
    Parse {
        #[cfg(feature = "miette-diagnostics")]
        #[source_code]
        src: NamedSource<String>,
        #[cfg(feature = "miette-diagnostics")]
        #[label("{message}")]
        span: miette::SourceOffset,
        #[cfg(not(feature = "miette-diagnostics"))]
        source_name: String,
        #[cfg(not(feature = "miette-diagnostics"))]
        offset: usize,
        message: String,
    },

    #[error("Eval error: {0}")]
    Eval(String),

    #[error("Import not found: {0}")]
    ImportNotFound(String),

    #[error("Unsupported feature: {0}")]
    Unsupported(String),
}

impl Error {
    pub fn lex(source_name: &str, source: &str, offset: usize, message: String) -> Self {
        #[cfg(feature = "miette-diagnostics")]
        {
            Self::Lex {
                src: NamedSource::new(source_name, source.to_string()),
                span: miette::SourceOffset::from(offset),
                message,
            }
        }
        #[cfg(not(feature = "miette-diagnostics"))]
        {
            let _ = source;
            Self::Lex {
                source_name: source_name.to_string(),
                offset,
                message,
            }
        }
    }

    pub fn parse(source_name: &str, source: &str, offset: usize, message: String) -> Self {
        #[cfg(feature = "miette-diagnostics")]
        {
            Self::Parse {
                src: NamedSource::new(source_name, source.to_string()),
                span: miette::SourceOffset::from(offset),
                message,
            }
        }
        #[cfg(not(feature = "miette-diagnostics"))]
        {
            let _ = source;
            Self::Parse {
                source_name: source_name.to_string(),
                offset,
                message,
            }
        }
    }

    pub fn source_offset(&self) -> Option<usize> {
        match self {
            #[cfg(feature = "miette-diagnostics")]
            Self::Lex { span, .. } | Self::Parse { span, .. } => Some(span.offset()),
            #[cfg(not(feature = "miette-diagnostics"))]
            Self::Lex { offset, .. } | Self::Parse { offset, .. } => Some(*offset),
            _ => None,
        }
    }
}
