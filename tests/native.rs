mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pklr::{EvalCapabilities, Evaluator};

#[derive(Clone, Default)]
struct MemoryCacheCapabilities {
    files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
}

fn not_found(path: &Path) -> pklr::Error {
    pklr::Error::Io(
        path.to_path_buf(),
        std::io::Error::from(std::io::ErrorKind::NotFound),
    )
}

impl EvalCapabilities for MemoryCacheCapabilities {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        let bytes = self.read_bytes(path)?;
        String::from_utf8(bytes).map_err(|error| pklr::Error::Eval(error.to_string()))
    }

    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        Ok(self.files.lock().unwrap().contains_key(path))
    }

    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        Ok(path.to_path_buf())
    }

    fn read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        let bytes = self.files.lock().unwrap().get(path).cloned();
        bytes.ok_or_else(|| not_found(path))
    }

    fn create_dir_all(&mut self, _path: &Path) -> pklr::Result<()> {
        Ok(())
    }

    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> pklr::Result<()> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn read_env(&mut self, _name: &str) -> pklr::Result<Option<String>> {
        Ok(None)
    }

    fn fetch_text(&mut self, url: &str) -> pklr::Result<String> {
        Err(pklr::Error::Unsupported(url.to_string()))
    }

    fn fetch_bytes(&mut self, url: &str) -> pklr::Result<Vec<u8>> {
        Err(pklr::Error::Unsupported(url.to_string()))
    }

    fn temp_dir(&mut self, prefix: &str) -> pklr::Result<PathBuf> {
        Ok(PathBuf::from(prefix))
    }

    fn glob(&mut self, _base: &Path, _pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

fn spawn_test_http_server(path: &'static str, body: &'static str) -> String {
    spawn_test_http_bytes_server(path, body.as_bytes().to_vec())
}

fn spawn_test_http_bytes_server(path: &'static str, body: Vec<u8>) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let request_path = request_line.split_whitespace().nth(1).unwrap_or("/");
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
        }
        let response = if request_path == path {
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(&body);
            response
        } else {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        };
        stream.write_all(&response).unwrap();
    });
    format!("http://127.0.0.1:{port}")
}

fn package_zip(name: &str, contents: &str) -> Vec<u8> {
    package_zip_entries(&[(name, contents)])
}

fn package_zip_entries(entries: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;

    let mut bytes = Vec::new();
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
    for (name, contents) in entries {
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(contents.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
    bytes
}

#[test]
fn import_analysis_is_synchronous() {
    let dir = std::env::temp_dir().join(format!("pklr_test_native_imports_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dependency = dir.join("dependency.pkl");
    let main = dir.join("main.pkl");
    std::fs::write(&dependency, "answer = 42\n").unwrap();
    std::fs::write(
        &main,
        "import \"dependency.pkl\" as dependency\nanswer = dependency.answer\n",
    )
    .unwrap();

    let imports = pklr::analyze_imports(&main).unwrap();

    assert_eq!(imports, vec![dependency]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn configured_evaluation_returns_environment_reads() {
    let base = spawn_test_http_server("/Imported.pkl", "value = 42\n");
    let dir = std::env::temp_dir().join(format!("pklr_test_native_http_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(
        &path,
        "import \"https://example.com/Imported.pkl\" as Imported\nresult = Imported.value\nenvironment = read?(\"env:PATH\")\n",
    )
    .unwrap();

    let outcome = pklr::eval_with_options(
        &path,
        pklr::EvalOptions {
            http_rewrites: vec![format!("https://example.com/={base}/")],
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(outcome.json["result"], 42);
    assert_eq!(
        outcome.env_reads.get("PATH"),
        Some(&std::env::var("PATH").ok())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_evaluator_fetches_http() {
    let base = spawn_test_http_server("/Imported.pkl", "value = 42\n");
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_default_evaluator_http_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(
        &path,
        "import \"https://example.com/Imported.pkl\" as Imported\nresult = Imported.value\n",
    )
    .unwrap();
    let mut evaluator = Evaluator::new();
    evaluator.set_http_rewrites(&[format!("https://example.com/={base}/")]);

    let value = evaluator.eval_file(&path).unwrap();

    assert_eq!(value.to_json()["result"], 42);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_entry_read_resets_evaluation_state() {
    let mut evaluator = Evaluator::new();
    evaluator
        .eval_source("value = read?(\"env:PATH\")\n", Path::new("first.pkl"))
        .unwrap();
    assert!(evaluator.env_reads().contains_key("PATH"));

    let missing = std::env::temp_dir().join(format!(
        "pklr_test_missing_entry_{}_{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let _ = std::fs::remove_file(&missing);
    let result = evaluator.eval_file(&missing);

    assert!(result.is_err());
    assert!(evaluator.env_reads().is_empty());
}

#[test]
fn http_rejects_invalid_utf8() {
    let base = spawn_test_http_bytes_server("/invalid.pkl", b"value = \"\xff\"\n".to_vec());
    let mut capabilities = pklr::NativeCapabilities::new();

    let error = capabilities
        .fetch_text(&format!("{base}/invalid.pkl"))
        .unwrap_err()
        .to_string();

    assert!(error.contains("HTTP read failed"), "{error}");
}

#[test]
fn evaluation_loads_preloaded_package_archives() {
    let dir = std::env::temp_dir().join(format!("pklr_test_native_package_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(
        &path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();

    let json = pklr::EvaluatorBuilder::new()
        .package_cache_dir(dir.join("cache"))
        .offline(true)
        .preload_package(
            "https://example.com/pkg@1.0.0.zip",
            "zip",
            package_zip("Config.pkl", "answer = 42\n"),
        )
        .eval_to_json(&path)
        .unwrap();

    assert_eq!(json["answer"], 42);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn package_root_imports_work_in_default_builds() {
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_native_package_root_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(
        &path,
        "amends \"package://example.com/pkg@1.0.0#/nested/Config.pkl\"\n",
    )
    .unwrap();

    let json = pklr::EvaluatorBuilder::new()
        .package_cache_dir(dir.join("cache"))
        .offline(true)
        .preload_package(
            "https://example.com/pkg@1.0.0.zip",
            "zip",
            package_zip_entries(&[
                ("Base.pkl", "answer = 42\n"),
                ("nested/Config.pkl", "amends \".../Base.pkl\"\n"),
            ]),
        )
        .eval_to_json(&path)
        .unwrap();

    assert_eq!(json["answer"], 42);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preload_uses_custom_capabilities() {
    let capabilities = MemoryCacheCapabilities::default();
    let files = capabilities.files.clone();
    let mut evaluator = Evaluator::with_capabilities(capabilities);
    evaluator.set_package_cache_dir("virtual-cache");

    evaluator
        .preload_package("https://example.com/pkg@1.0.0.pkl", "pkl", b"answer = 42\n")
        .unwrap();

    let files = files.lock().unwrap();
    assert_eq!(files.len(), 2);
    assert!(files.values().any(|value| value == b"answer = 42\n"));
    assert!(
        files
            .values()
            .any(|value| value == b"https://example.com/pkg@1.0.0.pkl")
    );
}

/// Native capabilities without batch fetching, so every remote module is
/// fetched when evaluation reaches it, as it was before prefetching.
struct NoPrefetch(pklr::NativeCapabilities);

impl EvalCapabilities for NoPrefetch {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.0.read_to_string(path)
    }

    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        self.0.path_exists(path)
    }

    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        self.0.canonicalize(path)
    }

    fn read_env(&mut self, name: &str) -> pklr::Result<Option<String>> {
        self.0.read_env(name)
    }

    fn fetch_text(&mut self, url: &str) -> pklr::Result<String> {
        self.0.fetch_text(url)
    }

    fn fetch_bytes(&mut self, url: &str) -> pklr::Result<Vec<u8>> {
        self.0.fetch_bytes(url)
    }

    fn fetch_text_many(&mut self, urls: &[String]) -> Vec<pklr::Result<String>> {
        urls.iter()
            .map(|url| Err(pklr::Error::Unsupported(url.clone())))
            .collect()
    }

    fn fetch_bytes_many(&mut self, urls: &[String]) -> Vec<pklr::Result<Vec<u8>>> {
        urls.iter()
            .map(|url| Err(pklr::Error::Unsupported(url.clone())))
            .collect()
    }

    fn temp_dir(&mut self, prefix: &str) -> pklr::Result<PathBuf> {
        self.0.temp_dir(prefix)
    }

    fn glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        self.0.glob(base, pattern)
    }
}

#[test]
fn remote_imports_are_prefetched_concurrently() {
    const DEPS: usize = 6;
    let (routes, expected) = common::fan_out_routes(DEPS);
    let server = common::DelayedServer::start(&common::borrow_routes(&routes), common::DELAY);
    let path = common::write_entry(
        "prefetch_concurrent",
        "main.pkl",
        "import \"https://example.com/Main.pkl\"\nresult = Main.total\n",
    );

    let started = std::time::Instant::now();
    let json = pklr::EvaluatorBuilder::new()
        .http_rewrites([format!("https://example.com/={}/", server.base)])
        .eval_to_json(&path)
        .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(json["result"], expected);
    assert_eq!(server.requests(), DEPS + 1);
    // Fetched one after another this takes (DEPS + 1) delays; prefetching
    // fetches Main, then all of its imports at once.
    let sequential = common::DELAY * (DEPS as u32 + 1);
    assert!(
        elapsed < sequential * 6 / 10,
        "took {elapsed:?}; sequential fetching takes {sequential:?}"
    );
}

#[test]
fn prefetching_does_not_change_results() {
    let (routes, _) = common::fan_out_routes(3);
    let server =
        common::DelayedServer::start(&common::borrow_routes(&routes), std::time::Duration::ZERO);
    let source = format!("import \"{}/Main.pkl\"\nresult = Main.total\n", server.base);

    let prefetched = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();
    let fetched = Evaluator::with_capabilities(NoPrefetch(pklr::NativeCapabilities::new()))
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(prefetched, fetched);
    assert_eq!(prefetched["result"], 3);
}

#[test]
fn unused_remote_import_that_fails_to_prefetch_is_not_an_error() {
    let server = common::DelayedServer::start(
        &[
            (
                "/Main.pkl",
                "import \"Used.pkl\"\nimport \"Missing.pkl\"\nresult = Used.value\n",
            ),
            ("/Used.pkl", "value = 42\n"),
        ],
        std::time::Duration::ZERO,
    );
    let source = format!(
        "import \"{}/Main.pkl\"\nresult = Main.result\n",
        server.base
    );

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["result"], 42);
}

#[test]
fn used_remote_import_that_fails_to_prefetch_reports_the_fetch_error() {
    let server = common::DelayedServer::start(
        &[(
            "/Main.pkl",
            "import \"Missing.pkl\"\nresult = Missing.value\n",
        )],
        std::time::Duration::ZERO,
    );
    let source = format!(
        "import \"{}/Main.pkl\"\nresult = Main.result\n",
        server.base
    );

    let prefetched = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap_err()
        .to_string();
    let fetched = Evaluator::with_capabilities(NoPrefetch(pklr::NativeCapabilities::new()))
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap_err()
        .to_string();

    assert_eq!(prefetched, fetched);
    assert!(prefetched.contains("Missing.pkl"), "{prefetched}");
}

#[test]
fn offline_evaluation_does_not_prefetch() {
    let (routes, _) = common::fan_out_routes(2);
    let server =
        common::DelayedServer::start(&common::borrow_routes(&routes), std::time::Duration::ZERO);
    let path = common::write_entry(
        "prefetch_offline",
        "main.pkl",
        &format!("import \"{}/Main.pkl\"\nresult = Main.total\n", server.base),
    );

    let error = pklr::EvaluatorBuilder::new()
        .offline(true)
        .eval_to_json(&path)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("offline mode prevented HTTP fetch"),
        "{error}"
    );
    assert_eq!(server.requests(), 0);
}

#[test]
fn native_batch_fetch_returns_results_in_order() {
    let server = common::DelayedServer::start(
        &[("/a", "first"), ("/c", "third")],
        std::time::Duration::from_millis(20),
    );
    let urls: Vec<String> = ["a", "b", "c"]
        .iter()
        .map(|path| format!("{}/{path}", server.base))
        .collect();

    let results = pklr::NativeCapabilities::new().fetch_text_many(&urls);

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].as_ref().unwrap(), "first");
    assert!(matches!(results[1], Err(pklr::Error::ImportNotFound(_))));
    assert_eq!(results[2].as_ref().unwrap(), "third");
}

#[test]
fn remote_imports_of_local_modules_are_prefetched() {
    const DEPS: usize = 4;
    let (routes, expected) = common::fan_out_routes(DEPS);
    let server = common::DelayedServer::start(&common::borrow_routes(&routes), common::DELAY);
    let mut lib = String::new();
    let mut sum = Vec::new();
    for index in 0..DEPS {
        lib.push_str(&format!(
            "import \"{}/Dep{index}.pkl\" as Dep{index}\n",
            server.base
        ));
        sum.push(format!("Dep{index}.value"));
    }
    lib.push_str(&format!("total = {}\n", sum.join(" + ")));
    let lib_path = common::write_entry("prefetch_local", "lib.pkl", &lib);
    let path = lib_path.with_file_name("main.pkl");
    std::fs::write(&path, "amends \"lib.pkl\"\n").unwrap();

    let started = std::time::Instant::now();
    let json = pklr::eval_to_json(&path).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(json["total"], expected);
    let sequential = common::DELAY * DEPS as u32;
    assert!(
        elapsed < sequential * 6 / 10,
        "took {elapsed:?}; sequential fetching takes {sequential:?}"
    );
}

#[test]
fn prefetching_an_endless_import_chain_stops_within_its_budget() {
    let server = common::endless_chain_server();
    let source = format!(
        "import \"{}/0.pkl\" as Zero\nresult = Zero.value\n",
        server.base
    );

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["result"], 0);
    // Prefetching follows at most eight levels of imports; evaluation uses
    // none of them past 0.pkl.
    assert!(server.requests() <= 8, "{} requests", server.requests());
}

#[test]
fn prefetching_stops_at_its_request_budget() {
    const DEPS: usize = 300;
    let mut main = String::new();
    for index in 0..DEPS {
        main.push_str(&format!("import \"Dep{index}.pkl\"\n"));
    }
    main.push_str("value = 42\n");
    let server = common::DelayedServer::start_with(
        move |path| match path {
            "/Main.pkl" => Some(main.clone()),
            _ => Some("value = 1\n".to_string()),
        },
        std::time::Duration::ZERO,
    );
    let source = format!("import \"{}/Main.pkl\"\nresult = Main.value\n", server.base);

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["result"], 42);
    assert_eq!(server.requests(), 256);
}

fn delayed(routes: &[(&str, &str)]) -> common::DelayedServer {
    common::DelayedServer::start(routes, std::time::Duration::from_millis(100))
}

#[test]
fn amends_and_import_expressions_are_prefetched_in_the_first_batch() {
    let server = delayed(&[
        ("/Base.pkl", "a = 0\nb = 0\n"),
        ("/A.pkl", "value = 1\n"),
        ("/B.pkl", "value = 2\n"),
    ]);
    let source = format!(
        "amends \"{0}/Base.pkl\"\na = import(\"{0}/A.pkl\").value\nb = \"\\(import(\"{0}/B.pkl\").value)\"\n",
        server.base
    );

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["a"], 1);
    assert_eq!(json["b"], "2");
    assert_eq!(server.requests(), 3);
    assert_eq!(server.peak_in_flight(), 3);
}

#[test]
fn extends_and_imports_are_prefetched_in_the_first_batch() {
    let server = delayed(&[
        ("/Base.pkl", "base = 0\n"),
        ("/A.pkl", "value = 1\n"),
        ("/B.pkl", "value = 2\n"),
    ]);
    let source = format!(
        "extends \"{0}/Base.pkl\"\nimport \"{0}/A.pkl\"\na = A.value\nb = import(\"{0}/B.pkl\").value\n",
        server.base
    );

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["a"], 1);
    assert_eq!(json["b"], 2);
    assert_eq!(server.requests(), 3);
    assert_eq!(server.peak_in_flight(), 3);
}

#[test]
fn every_import_form_of_a_fetched_module_is_in_one_batch() {
    let server = delayed(&[
        (
            "/Main.pkl",
            "extends \"Base.pkl\"\nimport \"Y.pkl\"\nx = import(\"X.pkl\").value\ny = Y.value\n",
        ),
        ("/Base.pkl", "base = 0\n"),
        ("/X.pkl", "value = 1\n"),
        ("/Y.pkl", "value = 2\n"),
    ]);
    let source = format!(
        "import \"{}/Main.pkl\"\nx = Main.x\ny = Main.y\n",
        server.base
    );

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["x"], 1);
    assert_eq!(json["y"], 2);
    assert_eq!(server.requests(), 4);
    assert_eq!(server.peak_in_flight(), 3);
}

#[test]
fn native_batch_fetch_is_bounded() {
    let server = common::DelayedServer::start_with(
        |_| Some("value = 1\n".to_string()),
        std::time::Duration::from_millis(50),
    );
    let urls: Vec<String> = (0..20)
        .map(|index| format!("{}/{index}.pkl", server.base))
        .collect();

    let results = pklr::NativeCapabilities::new().fetch_text_many(&urls);

    assert!(results.iter().all(|result| result.is_ok()));
    assert!(server.peak_in_flight() <= 8, "{}", server.peak_in_flight());
    assert!(server.peak_in_flight() > 1, "{}", server.peak_in_flight());
}

/// Native capabilities that record every batch fetch and zip extraction.
#[derive(Default)]
struct Recording {
    native: pklr::NativeCapabilities,
    text_batches: Arc<Mutex<Vec<Vec<String>>>>,
    bytes_batches: Arc<Mutex<Vec<Vec<String>>>>,
    extractions: Arc<Mutex<usize>>,
}

impl EvalCapabilities for Recording {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.native.read_to_string(path)
    }

    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        self.native.path_exists(path)
    }

    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        self.native.canonicalize(path)
    }

    fn extract_zip(&mut self, bytes: Vec<u8>, destination: &Path) -> pklr::Result<()> {
        *self.extractions.lock().unwrap() += 1;
        self.native.extract_zip(bytes, destination)
    }

    fn read_env(&mut self, name: &str) -> pklr::Result<Option<String>> {
        self.native.read_env(name)
    }

    fn fetch_text(&mut self, url: &str) -> pklr::Result<String> {
        self.native.fetch_text(url)
    }

    fn fetch_bytes(&mut self, url: &str) -> pklr::Result<Vec<u8>> {
        self.native.fetch_bytes(url)
    }

    fn fetch_text_many(&mut self, urls: &[String]) -> Vec<pklr::Result<String>> {
        self.text_batches.lock().unwrap().push(urls.to_vec());
        self.native.fetch_text_many(urls)
    }

    fn fetch_bytes_many(&mut self, urls: &[String]) -> Vec<pklr::Result<Vec<u8>>> {
        self.bytes_batches.lock().unwrap().push(urls.to_vec());
        self.native.fetch_bytes_many(urls)
    }

    fn temp_dir(&mut self, prefix: &str) -> pklr::Result<PathBuf> {
        self.native.temp_dir(prefix)
    }

    fn glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        self.native.glob(base, pattern)
    }
}

#[test]
fn entries_of_one_package_zip_share_one_fetch_and_extraction() {
    let zip = package_zip_entries(&[
        (
            "A.pkl",
            "import \"https://example.com/X.pkl\"\nvalue = X.value\n",
        ),
        (
            "B.pkl",
            "import \"https://example.com/Y.pkl\"\nvalue = Y.value\n",
        ),
    ]);
    let server = common::DelayedServer::start_bytes(
        move |path| match path {
            "/pkg@1.0.0.zip" => Some(zip.clone()),
            "/X.pkl" => Some(b"value = 1\n".to_vec()),
            "/Y.pkl" => Some(b"value = 2\n".to_vec()),
            _ => None,
        },
        std::time::Duration::ZERO,
    );
    let capabilities = Recording::default();
    let text_batches = capabilities.text_batches.clone();
    let bytes_batches = capabilities.bytes_batches.clone();
    let extractions = capabilities.extractions.clone();
    let mut evaluator = Evaluator::with_capabilities(capabilities);
    evaluator.set_http_rewrites(&[format!("https://example.com/={}/", server.base)]);

    let json = evaluator
        .eval_source(
            "import \"package://example.com/pkg@1.0.0#/A.pkl\"\nimport \"package://example.com/pkg@1.0.0#/B.pkl\"\na = A.value\nb = B.value\n",
            Path::new("entry.pkl"),
        )
        .unwrap()
        .to_json();

    assert_eq!(json["a"], 1);
    assert_eq!(json["b"], 2);
    assert_eq!(server.requests(), 3);
    assert_eq!(*extractions.lock().unwrap(), 1);
    assert_eq!(
        *bytes_batches.lock().unwrap(),
        vec![vec![format!("{}/pkg@1.0.0.zip", server.base)]]
    );
    let text_batches = text_batches.lock().unwrap();
    assert_eq!(text_batches.len(), 1);
    let mut next_level = text_batches[0].clone();
    next_level.sort();
    assert_eq!(
        next_level,
        vec![
            format!("{}/X.pkl", server.base),
            format!("{}/Y.pkl", server.base)
        ]
    );
}

#[test]
fn a_declaration_and_an_import_expression_of_one_url_share_one_request() {
    let server =
        common::DelayedServer::start(&[("/A.pkl", "value = 1\n")], std::time::Duration::ZERO);
    let source = format!(
        "import \"{0}/A.pkl\"\na = A.value\nb = import(\"{0}/A.pkl\").value\n",
        server.base
    );

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["a"], 1);
    assert_eq!(json["b"], 1);
    assert_eq!(server.requests(), 1);
}

#[test]
fn deeply_nested_unused_remote_modules_do_not_crash_prefetching() {
    const DEPTH: usize = 100_000;
    let parens = format!("value = {}1{}\n", "(".repeat(DEPTH), ")".repeat(DEPTH));
    let interpolations = format!(
        "value = {}1{}\n",
        "\"\\(".repeat(DEPTH),
        ")\"".repeat(DEPTH)
    );
    let server = common::DelayedServer::start_with(
        move |path| match path {
            "/Main.pkl" => Some(
                "import \"Parens.pkl\"\nimport \"Interpolations.pkl\"\nvalue = 42\n".to_string(),
            ),
            "/Parens.pkl" => Some(parens.clone()),
            "/Interpolations.pkl" => Some(interpolations.clone()),
            _ => None,
        },
        std::time::Duration::ZERO,
    );
    let source = format!("import \"{}/Main.pkl\"\nresult = Main.value\n", server.base);

    let json = Evaluator::new()
        .eval_source(&source, Path::new("entry.pkl"))
        .unwrap()
        .to_json();

    assert_eq!(json["result"], 42);
    assert_eq!(server.requests(), 3);
}

#[test]
fn nesting_past_the_limit_is_a_parse_error() {
    let depth = pklr::parser::MAX_NESTING_DEPTH + 1;
    let path = common::write_entry(
        "nesting_limit",
        "main.pkl",
        &format!("value = {}1{}\n", "(".repeat(depth), ")".repeat(depth)),
    );

    let error = on_large_stack(move || pklr::eval_to_json(&path))
        .unwrap_err()
        .to_string();

    assert!(error.contains("levels deep"), "{error}");
}

#[test]
fn nesting_up_to_the_limit_parses() {
    // The module body and the property's value take two levels.
    let depth = pklr::parser::MAX_NESTING_DEPTH - 2;
    let path = common::write_entry(
        "nesting_below_limit",
        "main.pkl",
        &format!("value = {}1{}\n", "(".repeat(depth), ")".repeat(depth)),
    );

    let json = on_large_stack(move || pklr::eval_to_json(&path)).unwrap();

    assert_eq!(json["value"], 1);
}

#[test]
fn interpolation_nesting_past_the_limit_is_a_lex_error() {
    let depth = pklr::parser::MAX_NESTING_DEPTH + 1;
    let path = common::write_entry(
        "interpolation_limit",
        "main.pkl",
        &format!(
            "value = {}1{}\n",
            "\"\\(".repeat(depth),
            ")\"".repeat(depth)
        ),
    );

    let error = on_large_stack(move || pklr::eval_to_json(&path))
        .unwrap_err()
        .to_string();

    assert!(error.contains("levels deep"), "{error}");
}

/// Run `f` on a thread with a large stack: a debug build of the parser uses
/// many times the stack per nesting level that an optimized build does.
fn on_large_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn a_shared_download_fills_the_package_cache_for_offline_use() {
    let server = common::DelayedServer::start(
        &[("/o/r/releases/download/v1/Config.pkl", "value = 21\n")],
        std::time::Duration::ZERO,
    );
    let rewrite = format!("https://github.com/={}/", server.base);
    let path = common::write_entry(
        "shared_download_cache",
        "main.pkl",
        "import \"https://github.com/o/r/releases/download/v1/Config.pkl\" as Direct\nimport \"package://pkg.pkl-lang.org/github.com/o/r@v1#/Config.pkl\" as Pkg\nresult = Direct.value + Pkg.value\n",
    );
    let cache_dir = path.with_file_name("cache");

    let json = pklr::EvaluatorBuilder::new()
        .http_rewrites([rewrite.clone()])
        .package_cache_dir(&cache_dir)
        .eval_to_json(&path)
        .unwrap();
    assert_eq!(json["result"], 42);
    assert_eq!(server.requests(), 1);

    let offline_path = path.with_file_name("offline.pkl");
    std::fs::write(
        &offline_path,
        "import \"package://pkg.pkl-lang.org/github.com/o/r@v1#/Config.pkl\" as Pkg\nresult = Pkg.value\n",
    )
    .unwrap();
    let json = pklr::EvaluatorBuilder::new()
        .http_rewrites([rewrite])
        .package_cache_dir(&cache_dir)
        .offline(true)
        .eval_to_json(&offline_path)
        .unwrap();
    assert_eq!(json["result"], 21);
    assert_eq!(server.requests(), 1);
}

#[test]
fn glob_imports_in_fetched_modules_are_not_prefetched() {
    let server = common::DelayedServer::start(
        &[
            (
                "/Main.pkl",
                "import* \"https://example.com/a.pkl\" as Globbed\nvalue = 1\n",
            ),
            ("/a.pkl", "value = 2\n"),
        ],
        std::time::Duration::ZERO,
    );
    let mut evaluator = Evaluator::new();
    evaluator.set_http_rewrites(&[format!("https://example.com/={}/", server.base)]);

    let json = evaluator
        .eval_source(
            "import \"https://example.com/Main.pkl\"\nresult = Main.value\n",
            Path::new("entry.pkl"),
        )
        .unwrap()
        .to_json();

    assert_eq!(json["result"], 1);
    assert_eq!(server.requests(), 1);
}
