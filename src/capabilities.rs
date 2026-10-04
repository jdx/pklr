//! Host IO for the evaluator.
//!
//! Every file, environment, HTTP, temp-dir and glob access the evaluator makes
//! goes through an [`EvalCapabilities`] implementation. `NativeCapabilities`
//! (the `native-io` feature) uses the standard library for the file system and,
//! with the `http` feature, a `ureq` agent for HTTP. Embedders that need a
//! sandbox, an in-memory file system or their own HTTP stack implement the
//! trait themselves and pass it to [`Evaluator::with_capabilities`]; that only
//! needs the `eval-core` feature.
//!
//! The trait is synchronous. A host whose IO is asynchronous can run the
//! evaluator on a blocking thread (for example `tokio::task::spawn_blocking`)
//! and block on its own futures inside the capability methods.
//!
//! [`Evaluator::with_capabilities`]: crate::Evaluator::with_capabilities

use std::path::{Path, PathBuf};
#[cfg(feature = "native-io")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "http")]
use std::time::Duration;

use crate::Result;

/// Host-provided IO for evaluating Pkl modules.
///
/// The required methods cover reading modules, environment variables, HTTP,
/// temp directories and globs. The defaulted methods (`read_bytes`,
/// `create_dir_all`, `write_atomic`, `remove_file` and, with the
/// `package-zip` feature, `extract_zip`) use the standard library; the
/// evaluator only calls them for `package://` imports and the persistent
/// package cache.
pub trait EvalCapabilities: Send + Sync {
    fn read_to_string(&mut self, path: &Path) -> Result<String>;

    fn path_exists(&mut self, path: &Path) -> Result<bool>;

    fn canonicalize(&mut self, path: &Path) -> Result<PathBuf>;

    fn read_bytes(&mut self, path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path).map_err(|error| crate::Error::Io(path.to_path_buf(), error))
    }

    fn create_dir_all(&mut self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path).map_err(|error| crate::Error::Io(path.to_path_buf(), error))
    }

    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        crate::eval::write_atomic(path, bytes)
            .map_err(|error| crate::Error::Io(path.to_path_buf(), error))
    }

    fn remove_file(&mut self, path: &Path) -> Result<()> {
        std::fs::remove_file(path).map_err(|error| crate::Error::Io(path.to_path_buf(), error))
    }

    #[cfg(feature = "package-zip")]
    fn extract_zip(&mut self, bytes: Vec<u8>, destination: &Path) -> Result<()> {
        extract_zip_sync(bytes, destination)
    }

    fn read_env(&mut self, name: &str) -> Result<Option<String>>;

    fn fetch_text(&mut self, url: &str) -> Result<String>;

    fn fetch_bytes(&mut self, url: &str) -> Result<Vec<u8>>;

    fn temp_dir(&mut self, prefix: &str) -> Result<PathBuf>;

    fn glob(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>>;
}

/// The native host IO: the standard library for files and environment
/// variables and, with the `http` feature, a `ureq` agent for HTTP.
#[cfg(feature = "native-io")]
#[derive(Debug, Clone)]
pub struct NativeCapabilities {
    #[cfg(feature = "http")]
    http_agent: ureq::Agent,
}

#[cfg(feature = "native-io")]
impl NativeCapabilities {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "http")]
            http_agent: default_http_agent(),
        }
    }

    /// Use `http_agent` for HTTP, for example to configure a proxy,
    /// certificates or timeouts.
    #[cfg(feature = "http")]
    pub fn with_http_agent(http_agent: ureq::Agent) -> Self {
        ensure_crypto_provider();
        Self { http_agent }
    }
}

#[cfg(feature = "native-io")]
impl Default for NativeCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "http")]
fn ensure_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}

#[cfg(feature = "http")]
fn default_http_agent() -> ureq::Agent {
    ensure_crypto_provider();
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

#[cfg(feature = "http")]
const HTTP_BODY_LIMIT: u64 = 64 * 1024 * 1024;

#[cfg(feature = "native-io")]
impl EvalCapabilities for NativeCapabilities {
    fn read_to_string(&mut self, path: &Path) -> Result<String> {
        std::fs::read_to_string(path).map_err(|error| crate::Error::Io(path.to_path_buf(), error))
    }

    fn path_exists(&mut self, path: &Path) -> Result<bool> {
        Ok(path.exists())
    }

    fn canonicalize(&mut self, path: &Path) -> Result<PathBuf> {
        path.canonicalize()
            .map_err(|error| crate::Error::Io(path.to_path_buf(), error))
    }

    fn read_env(&mut self, name: &str) -> Result<Option<String>> {
        Ok(std::env::var(name).ok())
    }

    fn fetch_text(&mut self, url: &str) -> Result<String> {
        #[cfg(feature = "http")]
        {
            let mut response = self
                .http_agent
                .get(url)
                .call()
                .map_err(|error| http_error(url, error))?;
            response
                .body_mut()
                .with_config()
                .limit(HTTP_BODY_LIMIT)
                .lossy_utf8(false)
                .read_to_string()
                .map_err(|error| crate::Error::Eval(format!("HTTP read failed for {url}: {error}")))
        }
        #[cfg(not(feature = "http"))]
        {
            Err(crate::Error::Unsupported(format!(
                "HTTP fetch requires pklr's 'http' feature: {url}"
            )))
        }
    }

    fn fetch_bytes(&mut self, url: &str) -> Result<Vec<u8>> {
        #[cfg(feature = "http")]
        {
            let mut response = self
                .http_agent
                .get(url)
                .call()
                .map_err(|error| http_error(url, error))?;
            response
                .body_mut()
                .with_config()
                .limit(HTTP_BODY_LIMIT)
                .read_to_vec()
                .map_err(|error| crate::Error::Eval(format!("HTTP read failed for {url}: {error}")))
        }
        #[cfg(not(feature = "http"))]
        {
            Err(crate::Error::Unsupported(format!(
                "HTTP byte fetch requires pklr's 'http' feature: {url}"
            )))
        }
    }

    fn temp_dir(&mut self, prefix: &str) -> Result<PathBuf> {
        unique_temp_dir(prefix)
    }

    fn glob(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
        crate::eval::expand_glob(base, pattern)
    }
}

#[cfg(feature = "http")]
fn http_error(url: &str, error: ureq::Error) -> crate::Error {
    if matches!(error, ureq::Error::StatusCode(404)) {
        crate::Error::ImportNotFound(url.to_string())
    } else {
        crate::Error::Eval(format!("HTTP fetch failed for {url}: {error}"))
    }
}

#[cfg(feature = "native-io")]
static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "native-io")]
fn unique_temp_dir(prefix: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir();
    for _ in 0..100 {
        let counter = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("{prefix}-{}-{counter}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(crate::Error::Eval(format!(
                    "mkdir failed for {}: {error}",
                    dir.display()
                )));
            }
        }
    }
    Err(crate::Error::Eval(format!(
        "mkdir failed for {}: unable to create a unique directory",
        base.join(prefix).display()
    )))
}

#[cfg(feature = "package-zip")]
fn extract_zip_sync(bytes: Vec<u8>, destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| crate::Error::Eval(format!("zip error: {error}")))?;
    archive
        .extract(destination)
        .map_err(|error| crate::Error::Eval(format!("zip extract error: {error}")))?;
    Ok(())
}

#[cfg(all(test, feature = "native-io"))]
mod tests {
    use super::{EvalCapabilities, NativeCapabilities};

    #[cfg(feature = "http")]
    #[test]
    fn native_capabilities_install_a_crypto_provider() {
        let _ = NativeCapabilities::new();

        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn native_temp_dirs_are_unique_and_empty() {
        let mut capabilities = NativeCapabilities::new();
        let first = capabilities.temp_dir("pklr-capabilities-test").unwrap();
        let second = capabilities.temp_dir("pklr-capabilities-test").unwrap();

        assert_ne!(first, second);
        assert!(std::fs::read_dir(&first).unwrap().next().is_none());
        assert!(std::fs::read_dir(&second).unwrap().next().is_none());

        std::fs::remove_dir(&first).unwrap();
        std::fs::remove_dir(&second).unwrap();
    }

    #[test]
    fn native_path_exists_treats_metadata_errors_as_missing() {
        let mut capabilities = NativeCapabilities::new();

        assert!(
            !capabilities
                .path_exists(std::path::Path::new("invalid\0path"))
                .unwrap()
        );
    }
}
