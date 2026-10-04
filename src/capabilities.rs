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

    /// Fetch several URLs as text, returning one result per URL in order.
    ///
    /// The evaluator uses this to prefetch a module's remote imports in one
    /// batch. The default fetches them one after another with
    /// [`fetch_text`](Self::fetch_text); implementations can override it to
    /// fetch concurrently. A failed result is not cached: the evaluator
    /// fetches that URL again with `fetch_text` if evaluation needs it.
    fn fetch_text_many(&mut self, urls: &[String]) -> Vec<Result<String>> {
        urls.iter().map(|url| self.fetch_text(url)).collect()
    }

    /// Fetch several URLs as bytes, returning one result per URL in order.
    ///
    /// See [`fetch_text_many`](Self::fetch_text_many).
    fn fetch_bytes_many(&mut self, urls: &[String]) -> Vec<Result<Vec<u8>>> {
        urls.iter().map(|url| self.fetch_bytes(url)).collect()
    }

    fn temp_dir(&mut self, prefix: &str) -> Result<PathBuf>;

    fn glob(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>>;
}

/// The native host IO: the standard library for files and environment
/// variables and, with the `http` feature, a `ureq` agent for HTTP.
///
/// Batch fetches ([`EvalCapabilities::fetch_text_many`] and
/// [`EvalCapabilities::fetch_bytes_many`]) run up to
/// eight requests at once on scoped threads sharing the agent. With the
/// `async` feature, [`NativeCapabilities::with_reqwest_client`] uses a
/// `reqwest` client on tokio instead.
#[cfg(feature = "native-io")]
#[derive(Debug, Clone)]
pub struct NativeCapabilities {
    #[cfg(feature = "http")]
    http: HttpBackend,
}

#[cfg(feature = "http")]
#[derive(Debug, Clone)]
enum HttpBackend {
    Ureq(ureq::Agent),
    #[cfg(feature = "async")]
    Reqwest(reqwest::Client),
}

#[cfg(feature = "native-io")]
impl NativeCapabilities {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "http")]
            http: HttpBackend::Ureq(default_http_agent()),
        }
    }

    /// Use `http_agent` for HTTP, for example to configure a proxy,
    /// certificates or timeouts.
    #[cfg(feature = "http")]
    pub fn with_http_agent(http_agent: ureq::Agent) -> Self {
        ensure_crypto_provider();
        Self {
            http: HttpBackend::Ureq(http_agent),
        }
    }

    /// Use a `reqwest` client for HTTP. Requests run on tokio: on the
    /// caller's runtime (under `block_in_place`) when called from a
    /// multi-threaded runtime, and on a private runtime otherwise.
    #[cfg(feature = "async")]
    pub fn with_reqwest_client(client: reqwest::Client) -> Self {
        Self {
            http: HttpBackend::Reqwest(client),
        }
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

/// The most HTTP requests a batch fetch runs at once.
#[cfg(feature = "http")]
const MAX_CONCURRENT_FETCHES: usize = 8;

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
        match &self.http {
            HttpBackend::Ureq(agent) => ureq_fetch_text(agent, url),
            #[cfg(feature = "async")]
            HttpBackend::Reqwest(client) => {
                reqwest_backend::fetch_text_many(client, std::slice::from_ref(&url.to_string()))
                    .pop()
                    .expect("one result per URL")
            }
        }
        #[cfg(not(feature = "http"))]
        Err(crate::Error::Unsupported(format!(
            "HTTP fetch requires pklr's 'http' feature: {url}"
        )))
    }

    fn fetch_bytes(&mut self, url: &str) -> Result<Vec<u8>> {
        #[cfg(feature = "http")]
        match &self.http {
            HttpBackend::Ureq(agent) => ureq_fetch_bytes(agent, url),
            #[cfg(feature = "async")]
            HttpBackend::Reqwest(client) => {
                reqwest_backend::fetch_bytes_many(client, std::slice::from_ref(&url.to_string()))
                    .pop()
                    .expect("one result per URL")
            }
        }
        #[cfg(not(feature = "http"))]
        Err(crate::Error::Unsupported(format!(
            "HTTP byte fetch requires pklr's 'http' feature: {url}"
        )))
    }

    #[cfg(feature = "http")]
    fn fetch_text_many(&mut self, urls: &[String]) -> Vec<Result<String>> {
        match &self.http {
            HttpBackend::Ureq(agent) => fetch_parallel(urls, |url| ureq_fetch_text(agent, url)),
            #[cfg(feature = "async")]
            HttpBackend::Reqwest(client) => reqwest_backend::fetch_text_many(client, urls),
        }
    }

    #[cfg(feature = "http")]
    fn fetch_bytes_many(&mut self, urls: &[String]) -> Vec<Result<Vec<u8>>> {
        match &self.http {
            HttpBackend::Ureq(agent) => fetch_parallel(urls, |url| ureq_fetch_bytes(agent, url)),
            #[cfg(feature = "async")]
            HttpBackend::Reqwest(client) => reqwest_backend::fetch_bytes_many(client, urls),
        }
    }

    fn temp_dir(&mut self, prefix: &str) -> Result<PathBuf> {
        unique_temp_dir(prefix)
    }

    fn glob(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
        crate::eval::expand_glob(base, pattern)
    }
}

/// Run `fetch` for every URL, at most [`MAX_CONCURRENT_FETCHES`] at a time,
/// returning the results in URL order.
#[cfg(feature = "http")]
fn fetch_parallel<T: Send>(
    urls: &[String],
    fetch: impl Fn(&str) -> Result<T> + Sync,
) -> Vec<Result<T>> {
    if urls.len() <= 1 {
        return urls.iter().map(|url| fetch(url)).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = urls.len().min(MAX_CONCURRENT_FETCHES);
    let mut results: Vec<Option<Result<T>>> =
        std::iter::repeat_with(|| None).take(urls.len()).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(url) = urls.get(index) else {
                            break;
                        };
                        done.push((index, fetch(url)));
                    }
                    done
                })
            })
            .collect();
        for handle in handles {
            let done = match handle.join() {
                Ok(done) => done,
                Err(panic) => std::panic::resume_unwind(panic),
            };
            for (index, result) in done {
                results[index] = Some(result);
            }
        }
    });
    results
        .into_iter()
        .map(|result| result.expect("every URL is fetched"))
        .collect()
}

#[cfg(feature = "http")]
fn ureq_fetch_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut response = agent
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

#[cfg(feature = "http")]
fn ureq_fetch_bytes(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>> {
    let mut response = agent
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

#[cfg(feature = "http")]
fn http_error(url: &str, error: ureq::Error) -> crate::Error {
    if matches!(error, ureq::Error::StatusCode(404)) {
        crate::Error::ImportNotFound(url.to_string())
    } else {
        crate::Error::Eval(format!("HTTP fetch failed for {url}: {error}"))
    }
}

/// HTTP through a `reqwest` client on tokio, for the `async` feature.
#[cfg(feature = "async")]
mod reqwest_backend {
    use std::future::Future;
    use std::sync::OnceLock;

    use crate::Result;

    pub(super) fn fetch_text_many(
        client: &reqwest::Client,
        urls: &[String],
    ) -> Vec<Result<String>> {
        fetch_many(
            client,
            urls,
            |response| async move { response.text().await },
        )
    }

    pub(super) fn fetch_bytes_many(
        client: &reqwest::Client,
        urls: &[String],
    ) -> Vec<Result<Vec<u8>>> {
        fetch_many(client, urls, |response| async move {
            response.bytes().await.map(|bytes| bytes.to_vec())
        })
    }

    /// Fetch every URL, at most [`MAX_CONCURRENT_FETCHES`](super::MAX_CONCURRENT_FETCHES)
    /// at a time, and read each body with `read`. Results are in URL order.
    fn fetch_many<T, F, Fut>(client: &reqwest::Client, urls: &[String], read: F) -> Vec<Result<T>>
    where
        T: Send + 'static,
        F: Fn(reqwest::Response) -> Fut + Copy + Send + 'static,
        Fut: Future<Output = reqwest::Result<T>> + Send,
    {
        let client = client.clone();
        let urls = urls.to_vec();
        block_on(async move {
            let permits =
                std::sync::Arc::new(tokio::sync::Semaphore::new(super::MAX_CONCURRENT_FETCHES));
            let mut tasks = tokio::task::JoinSet::new();
            for (index, url) in urls.into_iter().enumerate() {
                let client = client.clone();
                let permits = permits.clone();
                tasks.spawn(async move {
                    let _permit = permits
                        .acquire_owned()
                        .await
                        .expect("semaphore is never closed");
                    (index, fetch(&client, &url, read).await)
                });
            }
            let mut results: Vec<Option<Result<T>>> =
                std::iter::repeat_with(|| None).take(tasks.len()).collect();
            while let Some(joined) = tasks.join_next().await {
                match joined {
                    Ok((index, result)) => results[index] = Some(result),
                    Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                    Err(error) => panic!("pklr fetch task failed: {error}"),
                }
            }
            results
                .into_iter()
                .map(|result| result.expect("every URL is fetched"))
                .collect()
        })
    }

    async fn fetch<T, F, Fut>(client: &reqwest::Client, url: &str, read: F) -> Result<T>
    where
        F: Fn(reqwest::Response) -> Fut,
        Fut: Future<Output = reqwest::Result<T>>,
    {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| crate::Error::Eval(format!("HTTP fetch failed for {url}: {error}")))?
            .error_for_status()
            .map_err(|error| {
                if error.status() == Some(reqwest::StatusCode::NOT_FOUND) {
                    crate::Error::ImportNotFound(url.to_string())
                } else {
                    crate::Error::Eval(format!("HTTP error for {url}: {error}"))
                }
            })?;
        read(response)
            .await
            .map_err(|error| crate::Error::Eval(format!("HTTP read failed for {url}: {error}")))
    }

    /// Run `future` to completion from synchronous code.
    ///
    /// On a multi-threaded runtime it runs on that runtime under
    /// `block_in_place`. Anywhere else (no runtime, or a current-thread
    /// runtime, whose thread must not block) it runs on a private runtime.
    fn block_on<F>(future: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| handle.block_on(future))
            }
            Ok(_) => std::thread::scope(|scope| {
                match scope.spawn(|| private_runtime().block_on(future)).join() {
                    Ok(output) => output,
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }),
            Err(_) => private_runtime().block_on(future),
        }
    }

    /// A runtime shared by all callers outside a multi-threaded runtime. It
    /// lives for the whole process so the client's pooled connections, which
    /// run on it, stay usable between calls.
    fn private_runtime() -> &'static tokio::runtime::Runtime {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("pklr-http")
                .enable_all()
                .build()
                .expect("failed to build pklr's HTTP runtime")
        })
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
