use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::Result;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Host-provided IO for evaluating Pkl modules.
///
/// The default evaluator still uses the native implementation, but embedders
/// can provide their own file, environment, HTTP, temp-dir, and glob behavior.
pub trait EvalCapabilities {
    fn read_to_string<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<String>>;

    fn read_env<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>>;

    fn fetch_text<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<String>>;

    fn fetch_bytes<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>>>;

    fn temp_dir<'a>(&'a mut self, prefix: &'a str) -> BoxFuture<'a, Result<PathBuf>>;

    fn glob<'a>(
        &'a mut self,
        base: &'a Path,
        pattern: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PathBuf>>>;
}

#[cfg(feature = "native-io")]
#[derive(Debug, Clone, Default)]
pub struct NativeCapabilities;

#[cfg(feature = "native-io")]
impl EvalCapabilities for NativeCapabilities {
    fn read_to_string<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            std::fs::read_to_string(path)
                .map_err(|error| crate::Error::Io(path.to_path_buf(), error))
        })
    }

    fn read_env<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { Ok(std::env::var(name).ok()) })
    }

    fn fetch_text<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            #[cfg(feature = "http")]
            {
                reqwest::Client::new()
                    .get(url)
                    .send()
                    .await
                    .map_err(|error| {
                        crate::Error::Eval(format!("HTTP fetch failed for {url}: {error}"))
                    })?
                    .error_for_status()
                    .map_err(|error| crate::Error::Eval(format!("HTTP error for {url}: {error}")))?
                    .text()
                    .await
                    .map_err(|error| {
                        crate::Error::Eval(format!("HTTP read failed for {url}: {error}"))
                    })
            }
            #[cfg(not(feature = "http"))]
            {
                Err(crate::Error::Unsupported(format!(
                    "HTTP fetch requires pklr's 'http' feature: {url}"
                )))
            }
        })
    }

    fn fetch_bytes<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            #[cfg(feature = "http")]
            {
                let bytes = reqwest::Client::new()
                    .get(url)
                    .send()
                    .await
                    .map_err(|error| {
                        crate::Error::Eval(format!("HTTP fetch failed for {url}: {error}"))
                    })?
                    .error_for_status()
                    .map_err(|error| crate::Error::Eval(format!("HTTP error for {url}: {error}")))?
                    .bytes()
                    .await
                    .map_err(|error| {
                        crate::Error::Eval(format!("HTTP read failed for {url}: {error}"))
                    })?;
                Ok(bytes.to_vec())
            }
            #[cfg(not(feature = "http"))]
            {
                Err(crate::Error::Unsupported(format!(
                    "HTTP byte fetch requires pklr's 'http' feature: {url}"
                )))
            }
        })
    }

    fn temp_dir<'a>(&'a mut self, prefix: &'a str) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move {
            let dir = std::env::temp_dir().join(prefix);
            std::fs::create_dir_all(&dir).map_err(|error| {
                crate::Error::Eval(format!("mkdir failed for {}: {error}", dir.display()))
            })?;
            Ok(dir)
        })
    }

    fn glob<'a>(
        &'a mut self,
        base: &'a Path,
        pattern: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PathBuf>>> {
        Box::pin(async move { crate::eval::expand_glob(base, pattern) })
    }
}
