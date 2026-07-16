use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
#[cfg(feature = "native-io")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "http")]
use std::time::Duration;

use crate::Result;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Host-provided IO for evaluating Pkl modules.
///
/// The default evaluator still uses the native implementation, but embedders
/// can provide their own file, environment, HTTP, temp-dir, and glob behavior.
pub trait EvalCapabilities: Send + Sync {
    fn read_to_string<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<String>>;

    fn path_exists<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<bool>>;

    fn canonicalize<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<PathBuf>>;

    fn read_env<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>>;

    fn fetch_text<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<String>>;

    fn fetch_bytes<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>>>;

    #[cfg(feature = "http")]
    fn set_http_client(&mut self, client: reqwest::Client) {
        drop(client);
    }

    fn temp_dir<'a>(&'a mut self, prefix: &'a str) -> BoxFuture<'a, Result<PathBuf>>;

    fn glob<'a>(
        &'a mut self,
        base: &'a Path,
        pattern: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PathBuf>>>;
}

#[cfg(feature = "native-io")]
#[derive(Debug, Clone)]
pub struct NativeCapabilities {
    #[cfg(feature = "http")]
    http_client: reqwest::Client,
}

#[cfg(feature = "native-io")]
impl NativeCapabilities {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "http")]
            http_client: default_http_client(),
        }
    }

    #[cfg(feature = "http")]
    pub fn with_http_client(http_client: reqwest::Client) -> Self {
        Self { http_client }
    }
}

#[cfg(feature = "native-io")]
impl Default for NativeCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(feature = "native-io", feature = "http"))]
fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("default reqwest client should build")
}

#[cfg(feature = "native-io")]
impl EvalCapabilities for NativeCapabilities {
    fn read_to_string<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            std::fs::read_to_string(path)
                .map_err(|error| crate::Error::Io(path.to_path_buf(), error))
        })
    }

    fn path_exists<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { Ok(path.exists()) })
    }

    fn canonicalize<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move {
            path.canonicalize()
                .map_err(|error| crate::Error::Io(path.to_path_buf(), error))
        })
    }

    fn read_env<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { Ok(std::env::var(name).ok()) })
    }

    fn fetch_text<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<String>> {
        #[cfg(feature = "http")]
        let client = self.http_client.clone();
        Box::pin(async move {
            #[cfg(feature = "http")]
            {
                client
                    .get(url)
                    .send()
                    .await
                    .map_err(|error| {
                        crate::Error::Eval(format!("HTTP fetch failed for {url}: {error}"))
                    })?
                    .error_for_status()
                    .map_err(|error| {
                        if error.status() == Some(reqwest::StatusCode::NOT_FOUND) {
                            crate::Error::ImportNotFound(url.to_string())
                        } else {
                            crate::Error::Eval(format!("HTTP error for {url}: {error}"))
                        }
                    })?
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
        #[cfg(feature = "http")]
        let client = self.http_client.clone();
        Box::pin(async move {
            #[cfg(feature = "http")]
            {
                let bytes = client
                    .get(url)
                    .send()
                    .await
                    .map_err(|error| {
                        crate::Error::Eval(format!("HTTP fetch failed for {url}: {error}"))
                    })?
                    .error_for_status()
                    .map_err(|error| {
                        if error.status() == Some(reqwest::StatusCode::NOT_FOUND) {
                            crate::Error::ImportNotFound(url.to_string())
                        } else {
                            crate::Error::Eval(format!("HTTP error for {url}: {error}"))
                        }
                    })?
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

    #[cfg(feature = "http")]
    fn set_http_client(&mut self, client: reqwest::Client) {
        self.http_client = client;
    }

    fn temp_dir<'a>(&'a mut self, prefix: &'a str) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move { unique_temp_dir(prefix) })
    }

    fn glob<'a>(
        &'a mut self,
        base: &'a Path,
        pattern: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PathBuf>>> {
        Box::pin(async move { crate::eval::expand_glob(base, pattern) })
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

#[cfg(all(test, feature = "native-io"))]
mod tests {
    use super::{EvalCapabilities, NativeCapabilities};

    #[tokio::test]
    async fn native_temp_dirs_are_unique_and_empty() {
        let mut capabilities = NativeCapabilities::new();
        let first = capabilities
            .temp_dir("pklr-capabilities-test")
            .await
            .unwrap();
        let second = capabilities
            .temp_dir("pklr-capabilities-test")
            .await
            .unwrap();

        assert_ne!(first, second);
        assert!(std::fs::read_dir(&first).unwrap().next().is_none());
        assert!(std::fs::read_dir(&second).unwrap().next().is_none());

        std::fs::remove_dir(&first).unwrap();
        std::fs::remove_dir(&second).unwrap();
    }
}
