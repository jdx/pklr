//! Optional module source IO, independent from resources and package caches.

use std::path::{Path, PathBuf};

use crate::Result;

/// A host-provided namespace for local Pkl modules.
///
/// Resource reads and package cache IO continue through `EvalCapabilities`.
pub trait ModuleSource: Send + Sync {
    /// Return the stable identity used by module caches.
    fn identity(&mut self, path: &Path) -> Result<PathBuf>;
    /// Whether a local module exists in this namespace.
    fn exists(&mut self, path: &Path) -> Result<bool>;
    /// Read a local module's source text.
    fn read(&mut self, path: &Path) -> Result<String>;
}
