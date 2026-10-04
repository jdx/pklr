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
    ///
    /// Response bodies must be charged to `budget`. Once it is spent, no new
    /// request may start; return an error for the remaining URLs instead.
    /// The default checks it between requests, so it overshoots by at most
    /// one response; a concurrent implementation should also refuse a body
    /// that does not fit in what is left (see [`FetchBudget::try_take`]).
    fn fetch_text_many(&mut self, urls: &[String], budget: &FetchBudget) -> Vec<Result<String>> {
        urls.iter()
            .map(|url| {
                if budget.is_spent() {
                    return Err(budget_spent(url));
                }
                let result = self.fetch_text(url);
                if let Ok(body) = &result {
                    budget.charge(body.len() as u64);
                }
                result
            })
            .collect()
    }

    /// Fetch several URLs as bytes, returning one result per URL in order.
    ///
    /// See [`fetch_text_many`](Self::fetch_text_many).
    fn fetch_bytes_many(&mut self, urls: &[String], budget: &FetchBudget) -> Vec<Result<Vec<u8>>> {
        urls.iter()
            .map(|url| {
                if budget.is_spent() {
                    return Err(budget_spent(url));
                }
                let result = self.fetch_bytes(url);
                if let Ok(body) = &result {
                    budget.charge(body.len() as u64);
                }
                result
            })
            .collect()
    }

    fn temp_dir(&mut self, prefix: &str) -> Result<PathBuf>;

    fn glob(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>>;
}

/// A byte budget shared by the requests of a batch fetch
/// ([`EvalCapabilities::fetch_text_many`] and
/// [`EvalCapabilities::fetch_bytes_many`]). It is safe to use from several
/// threads at once.
#[derive(Debug)]
pub struct FetchBudget {
    remaining: AtomicU64,
}

impl FetchBudget {
    /// A budget of `bytes` response bytes.
    pub fn new(bytes: u64) -> Self {
        Self {
            remaining: AtomicU64::new(bytes),
        }
    }

    /// A budget that is never spent.
    pub fn unlimited() -> Self {
        Self::new(u64::MAX)
    }

    /// The bytes left.
    pub fn remaining(&self) -> u64 {
        self.remaining.load(Ordering::Acquire)
    }

    /// Whether no bytes are left, so no new request should start.
    pub fn is_spent(&self) -> bool {
        self.remaining() == 0
    }

    /// Take `bytes` if that many are left, returning whether it did. Nothing
    /// is taken when they do not fit.
    pub fn try_take(&self, bytes: u64) -> bool {
        self.update(|left| left.checked_sub(bytes)).is_some()
    }

    /// Charge `bytes` already downloaded, leaving the budget spent if they
    /// do not fit.
    pub fn charge(&self, bytes: u64) {
        self.update(|left| Some(left.saturating_sub(bytes)));
    }

    /// Atomically replace the remaining bytes with `next(remaining)`, unless
    /// it returns `None`. Returns the new value.
    fn update(&self, next: impl Fn(u64) -> Option<u64>) -> Option<u64> {
        let mut left = self.remaining.load(Ordering::Acquire);
        loop {
            let new = next(left)?;
            match self.remaining.compare_exchange_weak(
                left,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(new),
                Err(actual) => left = actual,
            }
        }
    }
}

/// The error for a URL a batch fetch skipped or dropped because its budget
/// was spent.
fn budget_spent(url: &str) -> crate::Error {
    crate::Error::Eval(format!("fetch budget spent before {url}"))
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
            HttpBackend::Reqwest(client) => reqwest_backend::fetch_text_many(
                client,
                std::slice::from_ref(&url.to_string()),
                &FetchBudget::unlimited(),
            )
            .pop()
            .expect("one result per URL"),
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
            HttpBackend::Reqwest(client) => reqwest_backend::fetch_bytes_many(
                client,
                std::slice::from_ref(&url.to_string()),
                &FetchBudget::unlimited(),
            )
            .pop()
            .expect("one result per URL"),
        }
        #[cfg(not(feature = "http"))]
        Err(crate::Error::Unsupported(format!(
            "HTTP byte fetch requires pklr's 'http' feature: {url}"
        )))
    }

    #[cfg(feature = "http")]
    fn fetch_text_many(&mut self, urls: &[String], budget: &FetchBudget) -> Vec<Result<String>> {
        match &self.http {
            HttpBackend::Ureq(agent) => fetch_parallel(urls, budget, |url| {
                let body = ureq_fetch_within(agent, url, budget)?;
                String::from_utf8(body).map_err(|error| {
                    crate::Error::Eval(format!("HTTP read failed for {url}: {error}"))
                })
            }),
            #[cfg(feature = "async")]
            HttpBackend::Reqwest(client) => reqwest_backend::fetch_text_many(client, urls, budget),
        }
    }

    #[cfg(feature = "http")]
    fn fetch_bytes_many(&mut self, urls: &[String], budget: &FetchBudget) -> Vec<Result<Vec<u8>>> {
        match &self.http {
            HttpBackend::Ureq(agent) => {
                fetch_parallel(urls, budget, |url| ureq_fetch_within(agent, url, budget))
            }
            #[cfg(feature = "async")]
            HttpBackend::Reqwest(client) => reqwest_backend::fetch_bytes_many(client, urls, budget),
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
///
/// No request starts once `budget` is spent; `fetch` charges the bodies it
/// reads to it.
#[cfg(feature = "http")]
fn fetch_parallel<T: Send>(
    urls: &[String],
    budget: &FetchBudget,
    fetch: impl Fn(&str) -> Result<T> + Sync,
) -> Vec<Result<T>> {
    let fetch = |url: &str| {
        if budget.is_spent() {
            return Err(budget_spent(url));
        }
        fetch(url)
    };
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

/// Read `url`'s body, taking each chunk from `budget` as it arrives and
/// giving up (dropping the connection) as soon as a chunk does not fit. The
/// bytes read across concurrent fetches therefore stay within the budget
/// plus at most one chunk per fetch.
#[cfg(feature = "http")]
fn ureq_fetch_within(agent: &ureq::Agent, url: &str, budget: &FetchBudget) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| http_error(url, error))?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(HTTP_BODY_LIMIT)
        .reader();
    let mut body = Vec::new();
    let mut chunk = vec![0; FETCH_CHUNK];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| crate::Error::Eval(format!("HTTP read failed for {url}: {error}")))?;
        if read == 0 {
            return Ok(body);
        }
        if !budget.try_take(read as u64) {
            return Err(budget_spent(url));
        }
        body.extend_from_slice(&chunk[..read]);
    }
}

/// The most bytes a budgeted fetch reads before charging them.
#[cfg(feature = "http")]
const FETCH_CHUNK: usize = 64 * 1024;

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
    use std::sync::{Arc, OnceLock};

    use super::{FetchBudget, HTTP_BODY_LIMIT, budget_spent};
    use crate::Result;

    pub(super) fn fetch_text_many(
        client: &reqwest::Client,
        urls: &[String],
        budget: &FetchBudget,
    ) -> Vec<Result<String>> {
        fetch_many(client, urls, budget, |body, url| {
            String::from_utf8(body)
                .map_err(|error| crate::Error::Eval(format!("HTTP read failed for {url}: {error}")))
        })
    }

    pub(super) fn fetch_bytes_many(
        client: &reqwest::Client,
        urls: &[String],
        budget: &FetchBudget,
    ) -> Vec<Result<Vec<u8>>> {
        fetch_many(client, urls, budget, |body, _| Ok(body))
    }

    /// Fetch every URL, at most [`MAX_CONCURRENT_FETCHES`](super::MAX_CONCURRENT_FETCHES)
    /// at a time, and turn each body into a result with `convert`. Results
    /// are in URL order. Bodies are charged to `budget` as they are read.
    fn fetch_many<T>(
        client: &reqwest::Client,
        urls: &[String],
        budget: &FetchBudget,
        convert: fn(Vec<u8>, &str) -> Result<T>,
    ) -> Vec<Result<T>>
    where
        T: Send + 'static,
    {
        let client = client.clone();
        let urls = urls.to_vec();
        // The tasks need a budget they can own; settle with the caller's
        // when they are done.
        let start = budget.remaining();
        let shared = Arc::new(FetchBudget::new(start));
        let task_budget = shared.clone();
        let results = block_on(async move {
            let permits = Arc::new(tokio::sync::Semaphore::new(super::MAX_CONCURRENT_FETCHES));
            let mut tasks = tokio::task::JoinSet::new();
            for (index, url) in urls.into_iter().enumerate() {
                let client = client.clone();
                let permits = permits.clone();
                let budget = task_budget.clone();
                tasks.spawn(async move {
                    let _permit = permits
                        .acquire_owned()
                        .await
                        .expect("semaphore is never closed");
                    let result = fetch(&client, &url, &budget)
                        .await
                        .and_then(|body| convert(body, &url));
                    (index, result)
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
        });
        budget.charge(start - shared.remaining());
        results
    }

    /// Fetch `url`'s body. No request starts once `budget` is spent, and a
    /// body is dropped as soon as a chunk of it does not fit.
    async fn fetch(client: &reqwest::Client, url: &str, budget: &FetchBudget) -> Result<Vec<u8>> {
        if budget.is_spent() {
            return Err(budget_spent(url));
        }
        let mut response = client
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
        // Take each chunk from the budget as it arrives, and give up as
        // soon as one does not fit.
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| crate::Error::Eval(format!("HTTP read failed for {url}: {error}")))?
        {
            if (body.len() + chunk.len()) as u64 > HTTP_BODY_LIMIT {
                return Err(crate::Error::Eval(format!(
                    "HTTP read failed for {url}: body is too large"
                )));
            }
            if !budget.try_take(chunk.len() as u64) {
                return Err(budget_spent(url));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
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
