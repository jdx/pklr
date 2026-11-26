//! Module and resource readers for custom URI schemes.
//!
//! A host registers a reader for a scheme such as `myapp:` and pklr asks it
//! for the source of `import "myapp:/Lib.pkl"` or the bytes of
//! `read("myapp:/data.txt")`. This mirrors Pkl's
//! [external readers](https://pkl-lang.org/main/current/language-reference/index.html#external-readers);
//! [`crate::external_reader`] adapts a reader process that speaks Pkl's
//! external reader protocol to these traits.
//!
//! Module readers must use hierarchical URIs (`scheme:/path/to/Module.pkl`),
//! so a module can import its neighbours with relative URIs. Modules are
//! named by their URI: `amends "myapp:/Config.pkl"` and a relative
//! `import "../Config.pkl"` that resolves to the same URI load one module.

use crate::Result;

/// Supplies the source of modules under one URI scheme.
pub trait ModuleReader: Send + Sync {
    /// The source of the module at `uri` (for example `myapp:/lib/Base.pkl`),
    /// or `None` when there is no module there. The URI is normalized: no
    /// dot segments, no query or fragment.
    fn read_module(&self, uri: &str) -> Result<Option<String>>;

    /// The external reader process behind this reader, so readers registered
    /// for the same command share one process.
    #[doc(hidden)]
    #[cfg(feature = "external-readers")]
    fn external_process(
        &self,
    ) -> Option<std::sync::Arc<crate::external_reader::ExternalReaderProcess>> {
        None
    }
}

/// Supplies the contents of resources under one URI scheme.
pub trait ResourceReader: Send + Sync {
    /// The contents of the resource at `uri`, or `None` when there is no
    /// resource there.
    fn read_resource(&self, uri: &str) -> Result<Option<Vec<u8>>>;

    /// The external reader process behind this reader, so readers registered
    /// for the same command share one process.
    #[doc(hidden)]
    #[cfg(feature = "external-readers")]
    fn external_process(
        &self,
    ) -> Option<std::sync::Arc<crate::external_reader::ExternalReaderProcess>> {
        None
    }
}

impl<F> ModuleReader for F
where
    F: Fn(&str) -> Result<Option<String>> + Send + Sync,
{
    fn read_module(&self, uri: &str) -> Result<Option<String>> {
        self(uri)
    }
}

impl<F> ResourceReader for F
where
    F: Fn(&str) -> Result<Option<Vec<u8>>> + Send + Sync,
{
    fn read_resource(&self, uri: &str) -> Result<Option<Vec<u8>>> {
        self(uri)
    }
}

/// Whether `scheme` is a valid URI scheme that a reader may claim. The
/// schemes pklr implements itself cannot be replaced.
pub(crate) fn validate_scheme(scheme: &str) -> Result<()> {
    let valid = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid {
        return Err(crate::Error::Eval(format!(
            "`{scheme}` is not a valid URI scheme for a reader."
        )));
    }
    const BUILTIN: &[&str] = &[
        "file",
        "http",
        "https",
        "package",
        "projectpackage",
        "pkl",
        "modulepath",
        "env",
        "prop",
    ];
    if BUILTIN.contains(&scheme) {
        return Err(crate::Error::Eval(format!(
            "Cannot register a reader for the built-in scheme `{scheme}`."
        )));
    }
    Ok(())
}
