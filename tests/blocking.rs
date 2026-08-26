use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pklr::capabilities::BoxFuture;
use pklr::{EvalCapabilities, Evaluator};

#[derive(Clone, Default)]
struct MemoryCacheCapabilities {
    files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
}

impl EvalCapabilities for MemoryCacheCapabilities {
    fn read_to_string<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<String>> {
        let bytes = self.files.lock().unwrap().get(path).cloned();
        Box::pin(async move {
            let bytes = bytes.ok_or_else(|| {
                pklr::Error::Io(
                    path.to_path_buf(),
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                )
            })?;
            String::from_utf8(bytes).map_err(|error| pklr::Error::Eval(error.to_string()))
        })
    }

    fn path_exists<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<bool>> {
        let exists = self.files.lock().unwrap().contains_key(path);
        Box::pin(async move { Ok(exists) })
    }

    fn canonicalize<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<PathBuf>> {
        Box::pin(async move { Ok(path.to_path_buf()) })
    }

    fn read_bytes<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<Vec<u8>>> {
        let bytes = self.files.lock().unwrap().get(path).cloned();
        Box::pin(async move {
            bytes.ok_or_else(|| {
                pklr::Error::Io(
                    path.to_path_buf(),
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                )
            })
        })
    }

    fn create_dir_all<'a>(&'a mut self, _path: &'a Path) -> BoxFuture<'a, pklr::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn write_atomic<'a>(
        &'a mut self,
        path: &'a Path,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, pklr::Result<()>> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), bytes.to_vec());
        Box::pin(async { Ok(()) })
    }

    fn read_env<'a>(&'a mut self, _name: &'a str) -> BoxFuture<'a, pklr::Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    fn fetch_text<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, pklr::Result<String>> {
        Box::pin(async move { Err(pklr::Error::Unsupported(url.to_string())) })
    }

    fn fetch_bytes<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, pklr::Result<Vec<u8>>> {
        Box::pin(async move { Err(pklr::Error::Unsupported(url.to_string())) })
    }

    fn temp_dir<'a>(&'a mut self, prefix: &'a str) -> BoxFuture<'a, pklr::Result<PathBuf>> {
        Box::pin(async move { Ok(PathBuf::from(prefix)) })
    }

    fn glob<'a>(
        &'a mut self,
        _base: &'a Path,
        _pattern: &'a str,
    ) -> BoxFuture<'a, pklr::Result<Vec<PathBuf>>> {
        Box::pin(async { Ok(Vec::new()) })
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
fn evaluation_works_inside_and_outside_a_runtime() {
    let path = std::path::Path::new("tests/fixtures/base.pkl");
    let expected = pklr::eval_to_json(path).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let actual = runtime.block_on(async { pklr::eval_to_json(path).unwrap() });

    assert_eq!(actual, expected);
}

#[test]
fn import_analysis_is_synchronous() {
    let dir =
        std::env::temp_dir().join(format!("pklr_test_blocking_imports_{}", std::process::id()));
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
    let dir = std::env::temp_dir().join(format!("pklr_test_blocking_http_{}", std::process::id()));
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
fn default_evaluator_fetches_http_with_blocking_capabilities() {
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

    let value = pollster::block_on(evaluator.eval_file_pub(&path)).unwrap();

    assert_eq!(value.to_json()["result"], 42);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_entry_read_resets_evaluation_state() {
    let mut evaluator = Evaluator::new();
    pollster::block_on(
        evaluator.eval_source("value = read?(\"env:PATH\")\n", Path::new("first.pkl")),
    )
    .unwrap();
    assert!(evaluator.env_reads().contains_key("PATH"));

    let missing = std::env::temp_dir().join(format!(
        "pklr_test_missing_entry_{}_{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let _ = std::fs::remove_file(&missing);
    let result = pollster::block_on(evaluator.eval_file_pub(&missing));

    assert!(result.is_err());
    assert!(evaluator.env_reads().is_empty());
}

#[test]
fn blocking_http_rejects_invalid_utf8() {
    let base = spawn_test_http_bytes_server("/invalid.pkl", b"value = \"\xff\"\n".to_vec());
    let mut capabilities = pklr::BlockingCapabilities::new();

    let error = pollster::block_on(capabilities.fetch_text(&format!("{base}/invalid.pkl")))
        .unwrap_err()
        .to_string();

    assert!(error.contains("HTTP read failed"), "{error}");
}

#[test]
fn evaluation_loads_preloaded_package_archives() {
    let dir =
        std::env::temp_dir().join(format!("pklr_test_blocking_package_{}", std::process::id()));
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
fn package_root_imports_work_in_default_blocking_builds() {
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_blocking_package_root_{}",
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
fn blocking_preload_uses_custom_capabilities() {
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

#[test]
#[cfg(feature = "async")]
fn native_evaluator_works_without_tokio_when_both_modes_are_enabled() {
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_native_without_tokio_{}",
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

    let mut evaluator = Evaluator::new_async();
    evaluator.set_package_cache_dir(dir.join("cache"));
    evaluator.set_offline(true);
    evaluator
        .preload_package(
            "https://example.com/pkg@1.0.0.zip",
            "zip",
            &package_zip_entries(&[
                ("Base.pkl", "answer = 42\n"),
                ("nested/Config.pkl", "amends \".../Base.pkl\"\n"),
            ]),
        )
        .unwrap();

    let value = pollster::block_on(evaluator.eval_file_pub(&path)).unwrap();

    assert_eq!(value.to_json()["answer"], 42);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(feature = "async")]
fn blocking_evaluator_rejects_reqwest_configuration() {
    let mut evaluator = Evaluator::new();

    let error = evaluator
        .set_http_client(pklr::reqwest::Client::new())
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("do not accept a reqwest HTTP client"),
        "{error}"
    );
}
