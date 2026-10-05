//! Tests for pkl language features, organized by category.
//!
//! Tests marked `#[ignore]` document features not yet implemented.
//! As features are added, remove the `#[ignore]` attribute.

use pklr::eval::Evaluator;

fn eval(src: &str) -> serde_json::Value {
    let mut ev = Evaluator::new();
    let path = std::path::Path::new("test.pkl");
    let val = ev.eval_source(src, path).unwrap();
    val.to_json()
}

/// Like eval(), but also applies output.renderer.converters (full pipeline).
fn eval_with_converters(src: &str) -> serde_json::Value {
    let mut ev = Evaluator::new();
    let path = std::path::Path::new("test.pkl");
    let val = ev.eval_source(src, path).unwrap();
    let val = ev.apply_converters(val).unwrap();
    val.to_json()
}

fn eval_fails(src: &str) -> String {
    let mut ev = Evaluator::new();
    let path = std::path::Path::new("test.pkl");
    match ev.eval_source(src, path) {
        Err(e) => e.to_string(),
        Ok(v) => panic!("expected error, got: {:?}", v.to_json()),
    }
}

struct TestTempDir {
    path: std::path::PathBuf,
}

impl TestTempDir {
    fn new(name: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{name}_{}_{}", std::process::id(), unique));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[path = "pkl_features/advanced.rs"]
mod advanced;
#[path = "pkl_features/imports.rs"]
mod imports;
#[path = "pkl_features/objects.rs"]
mod objects;
#[path = "pkl_features/render.rs"]
mod render;
#[path = "pkl_features/strings.rs"]
mod strings;
#[path = "pkl_features/syntax.rs"]
mod syntax;
