//! Module and resource readers for custom URI schemes:
//! `EvaluatorBuilder::module_reader` and `EvaluatorBuilder::resource_reader`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pklr::EvaluatorBuilder;

struct TestTempDir(std::path::PathBuf);

impl TestTempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(std::fs::canonicalize(dir).unwrap())
    }

    fn write(&self, name: &str, source: &str) -> std::path::PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A module reader serving `modules` (URI to source), counting reads.
fn modules(
    modules: &[(&str, &str)],
) -> (
    impl Fn(&str) -> pklr::Result<Option<String>> + Send + Sync + 'static,
    Arc<AtomicUsize>,
) {
    let modules: HashMap<String, String> = modules
        .iter()
        .map(|(uri, source)| (uri.to_string(), source.to_string()))
        .collect();
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let reader = move |uri: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(modules.get(uri).cloned())
    };
    (reader, reads)
}

const LIB: &[(&str, &str)] = &[
    (
        "test:/Config.pkl",
        "module test.Config\nimport \"lib/Defaults.pkl\"\nname: String = Defaults.name\nport: Int = 80\n",
    ),
    ("test:/lib/Defaults.pkl", "name = \"default\"\n"),
    (
        "test:/lib/Extra.pkl",
        "import \"../Config.pkl\"\nmade: Config = new Config { port = 1 }\n",
    ),
];

#[test]
fn amends_a_reader_module() {
    let dir = TestTempDir::new("pklr_reader_amends");
    let main = dir.write("main.pkl", "amends \"test:/Config.pkl\"\nport = 8080\n");
    let (reader, _) = modules(LIB);
    let json = EvaluatorBuilder::new()
        .module_reader("test", reader)
        .eval_to_json(&main)
        .unwrap();
    assert_eq!(json, serde_json::json!({"name": "default", "port": 8080}));
}

#[test]
fn relative_imports_resolve_to_one_module() {
    let dir = TestTempDir::new("pklr_reader_relative");
    let main = dir.write(
        "main.pkl",
        "import \"test:/lib/Extra.pkl\"\nimport \"test:/Config.pkl\"\nport = Extra.made.port\nsame = Extra.made is Config\n",
    );
    let (reader, reads) = modules(LIB);
    let json = EvaluatorBuilder::new()
        .module_reader("test", reader)
        .eval_to_json(&main)
        .unwrap();
    assert_eq!(json, serde_json::json!({"port": 1, "same": true}));
    // Extra, Config and Defaults, each read once.
    assert_eq!(reads.load(Ordering::SeqCst), 3);
}

#[test]
fn dot_segments_are_normalized() {
    let dir = TestTempDir::new("pklr_reader_dots");
    let main = dir.write(
        "main.pkl",
        "import \"test:/lib/../lib/Defaults.pkl\"\nname = Defaults.name\n",
    );
    let (reader, _) = modules(LIB);
    let json = EvaluatorBuilder::new()
        .module_reader("test", reader)
        .eval_to_json(&main)
        .unwrap();
    assert_eq!(json, serde_json::json!({"name": "default"}));
}

#[test]
fn missing_reader_module_is_an_error() {
    let dir = TestTempDir::new("pklr_reader_missing");
    let main = dir.write("main.pkl", "import \"test:/Nope.pkl\"\nx = Nope\n");
    let (reader, _) = modules(LIB);
    let err = EvaluatorBuilder::new()
        .module_reader("test", reader)
        .eval_to_json(&main)
        .unwrap_err()
        .to_string();
    assert!(err.contains("test:/Nope.pkl"), "{err}");
}

#[test]
fn reader_errors_are_reported() {
    let dir = TestTempDir::new("pklr_reader_error");
    let main = dir.write("main.pkl", "import \"test:/Config.pkl\"\nx = 1\n");
    let err = EvaluatorBuilder::new()
        .module_reader("test", |_: &str| -> pklr::Result<Option<String>> {
            Err(pklr::Error::Eval("reader exploded".into()))
        })
        .eval_to_json(&main)
        .unwrap_err()
        .to_string();
    assert!(err.contains("reader exploded"), "{err}");
}

#[test]
fn unregistered_scheme_is_still_rejected() {
    let dir = TestTempDir::new("pklr_reader_unregistered");
    let main = dir.write("main.pkl", "import \"other:/Config.pkl\"\nx = Config\n");
    let (reader, _) = modules(LIB);
    assert!(
        EvaluatorBuilder::new()
            .module_reader("test", reader)
            .eval_to_json(&main)
            .is_err()
    );
}

#[test]
fn builtin_schemes_cannot_be_replaced() {
    let dir = TestTempDir::new("pklr_reader_builtin");
    let main = dir.write("main.pkl", "x = 1\n");
    let (reader, _) = modules(LIB);
    let err = EvaluatorBuilder::new()
        .module_reader("file", reader)
        .eval_to_json(&main)
        .unwrap_err()
        .to_string();
    assert!(err.contains("built-in scheme `file`"), "{err}");
}

#[test]
fn allowed_modules_apply_to_reader_modules() {
    let dir = TestTempDir::new("pklr_reader_allowed");
    let main = dir.write("main.pkl", "amends \"test:/Config.pkl\"\n");
    let (reader, _) = modules(LIB);
    let err = EvaluatorBuilder::new()
        .module_reader("test", reader)
        .allowed_modules(["file:".to_string()])
        .eval_to_json(&main)
        .unwrap_err()
        .to_string();
    assert!(err.contains("test:/Config.pkl"), "{err}");
    let (reader, _) = modules(LIB);
    let json = EvaluatorBuilder::new()
        .module_reader("test", reader)
        .allowed_modules(["file:".to_string(), "test:".to_string()])
        .eval_to_json(&main)
        .unwrap();
    assert_eq!(json["port"], 80);
}

#[test]
fn reads_resources_from_a_reader() {
    let dir = TestTempDir::new("pklr_reader_resource");
    let main = dir.write(
        "main.pkl",
        "text = read(\"data:/greeting.txt\").text\nmissing = read?(\"data:/nope.txt\") == null\n",
    );
    let json = EvaluatorBuilder::new()
        .resource_reader("data", |uri: &str| -> pklr::Result<Option<Vec<u8>>> {
            Ok((uri == "data:/greeting.txt").then(|| b"hello".to_vec()))
        })
        .eval_to_json(&main)
        .unwrap();
    assert_eq!(json, serde_json::json!({"text": "hello", "missing": true}));
}

#[test]
fn resource_scheme_respects_explicit_allowed_resources() {
    let dir = TestTempDir::new("pklr_reader_resource_allowed");
    let main = dir.write("main.pkl", "text = read(\"data:/greeting.txt\").text\n");
    let err = EvaluatorBuilder::new()
        .resource_reader("data", |_: &str| -> pklr::Result<Option<Vec<u8>>> {
            Ok(Some(b"hello".to_vec()))
        })
        .allowed_resources(["file:".to_string()])
        .eval_to_json(&main)
        .unwrap_err()
        .to_string();
    assert!(err.contains("data:/greeting.txt"), "{err}");
}
