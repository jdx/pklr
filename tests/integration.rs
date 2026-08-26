use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pklr::capabilities::{BoxFuture, EvalCapabilities};
use pklr::eval::Evaluator;
use pklr::lexer::{TokenKind, lex};
use pklr::parser::{BinOp, Entry, Expr, TypeExpr, collect_imports, parse};

fn lex_kinds(src: &str) -> Vec<TokenKind> {
    lex(src).unwrap().into_iter().map(|t| t.kind).collect()
}

struct MemoryCapabilities {
    modules: HashMap<String, String>,
    env: HashMap<String, String>,
    env_fetches: Arc<Mutex<Vec<String>>>,
    fetches: Arc<Mutex<Vec<String>>>,
}

impl EvalCapabilities for MemoryCapabilities {
    fn read_to_string<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<String>> {
        let key = path.display().to_string().replace('\\', "/");
        let source = self.modules.get(&key).cloned();
        Box::pin(async move { source.ok_or(pklr::Error::ImportNotFound(key)) })
    }

    fn path_exists<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<bool>> {
        let key = path.display().to_string().replace('\\', "/");
        let exists = self.modules.contains_key(&key);
        Box::pin(async move { Ok(exists) })
    }

    fn canonicalize<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<PathBuf>> {
        if path == Path::new("uncanonicalized.pkl") {
            let path = path.to_path_buf();
            return Box::pin(async move {
                Err(pklr::Error::Io(
                    path,
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "canonicalization unavailable in test capabilities",
                    ),
                ))
            });
        }
        let path = path.to_path_buf();
        Box::pin(async move { Ok(path) })
    }

    fn read_env<'a>(&'a mut self, name: &'a str) -> BoxFuture<'a, pklr::Result<Option<String>>> {
        self.env_fetches.lock().unwrap().push(name.to_string());
        let value = self.env.get(name).cloned();
        Box::pin(async move { Ok(value) })
    }

    fn fetch_text<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, pklr::Result<String>> {
        self.fetches.lock().unwrap().push(url.to_string());
        let source = self.modules.get(url).cloned();
        let url = url.to_string();
        Box::pin(async move { source.ok_or(pklr::Error::ImportNotFound(url)) })
    }

    fn fetch_bytes<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, pklr::Result<Vec<u8>>> {
        let url = url.to_string();
        Box::pin(async move {
            Err(pklr::Error::Unsupported(format!(
                "byte fetch unavailable in test capabilities: {url}"
            )))
        })
    }

    fn temp_dir<'a>(&'a mut self, prefix: &'a str) -> BoxFuture<'a, pklr::Result<PathBuf>> {
        let prefix = prefix.to_string();
        Box::pin(async move {
            Err(pklr::Error::Unsupported(format!(
                "temp dir unavailable in test capabilities: {prefix}"
            )))
        })
    }

    fn glob<'a>(
        &'a mut self,
        _base: &'a Path,
        _pattern: &'a str,
    ) -> BoxFuture<'a, pklr::Result<Vec<PathBuf>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

struct SandboxPackageCapabilities {
    archive: Vec<u8>,
    extraction_dir: PathBuf,
}

impl EvalCapabilities for SandboxPackageCapabilities {
    fn read_to_string<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<String>> {
        Box::pin(async move {
            std::fs::read_to_string(path)
                .map_err(|error| pklr::Error::Io(path.to_path_buf(), error))
        })
    }

    fn path_exists<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<bool>> {
        Box::pin(async move { Ok(path.exists()) })
    }

    fn canonicalize<'a>(&'a mut self, path: &'a Path) -> BoxFuture<'a, pklr::Result<PathBuf>> {
        Box::pin(async move {
            path.canonicalize()
                .map_err(|error| pklr::Error::Io(path.to_path_buf(), error))
        })
    }

    fn read_env<'a>(&'a mut self, _name: &'a str) -> BoxFuture<'a, pklr::Result<Option<String>>> {
        Box::pin(async move { Ok(None) })
    }

    fn fetch_text<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, pklr::Result<String>> {
        let url = url.to_string();
        Box::pin(async move {
            Err(pklr::Error::Unsupported(format!(
                "text fetch unavailable in package test: {url}"
            )))
        })
    }

    fn fetch_bytes<'a>(&'a mut self, _url: &'a str) -> BoxFuture<'a, pklr::Result<Vec<u8>>> {
        let archive = self.archive.clone();
        Box::pin(async move { Ok(archive) })
    }

    fn temp_dir<'a>(&'a mut self, _prefix: &'a str) -> BoxFuture<'a, pklr::Result<PathBuf>> {
        let extraction_dir = self.extraction_dir.clone();
        Box::pin(async move {
            std::fs::create_dir_all(&extraction_dir)
                .map_err(|error| pklr::Error::Io(extraction_dir.clone(), error))?;
            Ok(extraction_dir)
        })
    }

    fn glob<'a>(
        &'a mut self,
        _base: &'a Path,
        _pattern: &'a str,
    ) -> BoxFuture<'a, pklr::Result<Vec<PathBuf>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

#[test]
fn evaluator_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<pklr::Evaluator>();
}

#[tokio::test]
async fn entry_file_evaluates_when_canonicalization_is_unavailable() {
    let path = Path::new("uncanonicalized.pkl");
    let mut evaluator = Evaluator::with_capabilities(MemoryCapabilities {
        modules: HashMap::from([(path.display().to_string(), "answer = 42\n".to_string())]),
        env: HashMap::new(),
        env_fetches: Arc::new(Mutex::new(Vec::new())),
        fetches: Arc::new(Mutex::new(Vec::new())),
    });

    let json = evaluator.eval_file_pub(path).await.unwrap().to_json();

    assert_eq!(json["answer"], 42);
}

#[tokio::test]
async fn custom_capabilities_handle_http_import() {
    let fetches = Arc::new(Mutex::new(Vec::new()));
    let mut modules = HashMap::new();
    modules.insert(
        "http://example.test/Main.pkl".to_string(),
        "value = 42\n".to_string(),
    );

    let mut evaluator = pklr::Evaluator::with_capabilities(MemoryCapabilities {
        modules,
        env: HashMap::new(),
        env_fetches: Arc::new(Mutex::new(Vec::new())),
        fetches: fetches.clone(),
    });
    let json = evaluator
        .eval_source(
            "import \"http://example.test/Main.pkl\" as Main\nresult = Main.value\n",
            Path::new("entry.pkl"),
        )
        .await
        .unwrap()
        .to_json();

    assert_eq!(json["result"], 42);
    assert_eq!(
        *fetches.lock().unwrap(),
        vec!["http://example.test/Main.pkl".to_string()]
    );
}

#[tokio::test]
async fn package_extraction_uses_custom_temp_dir() {
    use std::io::Write;

    let root = std::env::temp_dir().join(format!(
        "pklr_test_custom_package_temp_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let extraction_dir = root.join("sandbox");
    let mut archive = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut archive));
        zip.start_file(
            "Config.pkl",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"answer = 42\n").unwrap();
        zip.finish().unwrap();
    }
    let mut evaluator = Evaluator::with_capabilities(SandboxPackageCapabilities {
        archive,
        extraction_dir: extraction_dir.clone(),
    });

    let json = evaluator
        .eval_source(
            "amends \"package://example.com/package@1.0.0#/Config.pkl\"\n",
            &root.join("main.pkl"),
        )
        .await
        .unwrap()
        .to_json();

    assert_eq!(json["answer"], 42);
    assert!(extraction_dir.join("Config.pkl").is_file());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn custom_capabilities_preserve_fetch_errors() {
    let fetches = Arc::new(Mutex::new(Vec::new()));
    let mut evaluator = pklr::Evaluator::with_capabilities(MemoryCapabilities {
        modules: HashMap::new(),
        env: HashMap::new(),
        env_fetches: Arc::new(Mutex::new(Vec::new())),
        fetches,
    });
    let error = evaluator
        .eval_source(
            "import \"http://example.test/missing.pkl\" as Missing\nresult = Missing.value\n",
            Path::new("entry.pkl"),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        pklr::Error::ImportNotFound(url) if url == "http://example.test/missing.pkl"
    ));
}

#[tokio::test]
async fn custom_capabilities_handle_virtual_local_import() {
    let mut modules = HashMap::new();
    modules.insert("virtual/Main.pkl".to_string(), "value = 42\n".to_string());

    let mut evaluator = pklr::Evaluator::with_capabilities(MemoryCapabilities {
        modules,
        env: HashMap::new(),
        env_fetches: Arc::new(Mutex::new(Vec::new())),
        fetches: Arc::new(Mutex::new(Vec::new())),
    });
    let json = evaluator
        .eval_source(
            "import \"Main.pkl\" as Main\nresult = Main.value\n",
            Path::new("virtual/entry.pkl"),
        )
        .await
        .unwrap()
        .to_json();

    assert_eq!(json["result"], 42);
}

#[tokio::test]
async fn evaluator_records_environment_hits_and_misses_in_name_order() {
    let mut evaluator = pklr::Evaluator::with_capabilities(MemoryCapabilities {
        modules: HashMap::new(),
        env: HashMap::from([
            ("ZEBRA".to_string(), "last".to_string()),
            ("ALPHA".to_string(), "first".to_string()),
        ]),
        env_fetches: Arc::new(Mutex::new(Vec::new())),
        fetches: Arc::new(Mutex::new(Vec::new())),
    });

    evaluator
        .eval_source(
            r#"
zebra = read("env:ZEBRA")
missing = read?("env:MISSING")
alpha = read("env:ALPHA")
"#,
            Path::new("entry.pkl"),
        )
        .await
        .unwrap();

    let reads = evaluator.env_reads();
    assert_eq!(
        reads.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["ALPHA", "MISSING", "ZEBRA"]
    );
    assert_eq!(reads["ALPHA"].as_deref(), Some("first"));
    assert_eq!(reads["MISSING"], None);
    assert_eq!(reads["ZEBRA"].as_deref(), Some("last"));
}

#[tokio::test]
async fn evaluator_records_environment_reads_from_imported_modules() {
    let mut modules = HashMap::new();
    modules.insert(
        "virtual/Imported.pkl".to_string(),
        "value = read(\"env:IMPORTED_VALUE\")\n".to_string(),
    );
    let mut evaluator = pklr::Evaluator::with_capabilities(MemoryCapabilities {
        modules,
        env: HashMap::from([("IMPORTED_VALUE".to_string(), "transitive".to_string())]),
        env_fetches: Arc::new(Mutex::new(Vec::new())),
        fetches: Arc::new(Mutex::new(Vec::new())),
    });

    let json = evaluator
        .eval_source(
            "import \"Imported.pkl\" as Imported\nresult = Imported.value\n",
            Path::new("virtual/entry.pkl"),
        )
        .await
        .unwrap()
        .to_json();

    assert_eq!(json["result"], "transitive");
    assert_eq!(
        evaluator.env_reads()["IMPORTED_VALUE"].as_deref(),
        Some("transitive")
    );
}

#[tokio::test]
async fn evaluator_resets_environment_reads_between_evaluations() {
    let mut evaluator = pklr::Evaluator::with_capabilities(MemoryCapabilities {
        modules: HashMap::new(),
        env: HashMap::from([
            ("FIRST".to_string(), "one".to_string()),
            ("SECOND".to_string(), "two".to_string()),
        ]),
        env_fetches: Arc::new(Mutex::new(Vec::new())),
        fetches: Arc::new(Mutex::new(Vec::new())),
    });

    evaluator
        .eval_source("value = read(\"env:FIRST\")\n", Path::new("first.pkl"))
        .await
        .unwrap();
    evaluator
        .eval_source("value = read(\"env:SECOND\")\n", Path::new("second.pkl"))
        .await
        .unwrap();

    assert_eq!(
        evaluator
            .env_reads()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["SECOND"]
    );
}

#[tokio::test]
async fn evaluator_reevaluates_imports_between_evaluations() {
    let mut modules = HashMap::new();
    modules.insert(
        "virtual/Imported.pkl".to_string(),
        "value = read(\"env:IMPORTED_VALUE\")\n".to_string(),
    );
    let env_fetches = Arc::new(Mutex::new(Vec::new()));
    let mut evaluator = pklr::Evaluator::with_capabilities(MemoryCapabilities {
        modules,
        env: HashMap::from([("IMPORTED_VALUE".to_string(), "cached".to_string())]),
        env_fetches: env_fetches.clone(),
        fetches: Arc::new(Mutex::new(Vec::new())),
    });
    let source = "import \"Imported.pkl\" as Imported\nresult = Imported.value\n";

    evaluator
        .eval_source(source, Path::new("virtual/first.pkl"))
        .await
        .unwrap();
    evaluator
        .eval_source(source, Path::new("virtual/second.pkl"))
        .await
        .unwrap();

    assert_eq!(
        evaluator.env_reads()["IMPORTED_VALUE"].as_deref(),
        Some("cached")
    );
    assert_eq!(
        *env_fetches.lock().unwrap(),
        vec!["IMPORTED_VALUE", "IMPORTED_VALUE"]
    );
}

#[tokio::test]
async fn eval_outcome_exposes_environment_reads() {
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_eval_outcome_env_reads_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(&path, "value = read?(\"env:PATH\")\n").unwrap();

    let outcome = pklr::eval_with_options_async(&path, pklr::AsyncEvalOptions::default())
        .await
        .unwrap();

    assert_eq!(outcome.env_reads["PATH"], std::env::var("PATH").ok());
    assert_eq!(
        outcome.json["value"].as_str(),
        std::env::var("PATH").ok().as_deref()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Lexer tests ---

#[test]
fn lex_amends_line() {
    let src = r#"amends "pkg://github.com/jdx/hk/releases/download/v1.0.0/hk@1.0.0#/Config.pkl""#;
    let kinds = lex_kinds(src);
    assert!(matches!(kinds[0], TokenKind::KwAmends));
    assert!(matches!(&kinds[1], TokenKind::StringLit(s) if s.contains("Config.pkl")));
}

#[test]
fn lex_multiline_string() {
    let src = "x = \"\"\"\n  hello\n  world\n\"\"\"";
    let tokens = lex(src).unwrap();
    let str_tok = tokens
        .iter()
        .find(|t| matches!(&t.kind, TokenKind::StringLit(_)))
        .unwrap();
    if let TokenKind::StringLit(s) = &str_tok.kind {
        assert!(s.contains("hello"));
        assert!(s.contains("world"));
    }
}

// --- Parser tests ---

#[test]
fn parse_simple_assignment() {
    let src = r#"
amends "pkl/Config.pkl"
fail_fast = false
"#;
    let tokens = lex(src).unwrap();
    let module = parse(&tokens).unwrap();
    assert_eq!(module.amends.as_deref(), Some("pkl/Config.pkl"));
    assert_eq!(module.body.len(), 1);
}

#[test]
fn parser_accepts_github_actions_type_syntax() {
    let source = r#"
abstract module Example
`runs-on`: "ubuntu"|*"macos"
local select = (jobs: Mapping<String, String>) -> jobs
value: Mapping<String, String>?(length > 0, !isEmpty)
"#;
    let module = parse(&lex(source).unwrap()).unwrap();
    assert!(
        module
            .annotations
            .iter()
            .any(|annotation| annotation.name == "pklr:module:Abstract")
    );
    let Entry::Property(runs_on) = &module.body[0] else {
        panic!("expected runs-on property");
    };
    assert_eq!(runs_on.name, "runs-on");
    assert!(matches!(
        runs_on.type_ann,
        Some(TypeExpr::Union(ref members))
            if matches!(members[0], TypeExpr::Named(ref value) if value == "\"ubuntu\"")
                && matches!(members[1], TypeExpr::Named(ref value) if value == "*\"macos\"")
    ));
    let Entry::Property(select) = &module.body[1] else {
        panic!("expected lambda property");
    };
    assert!(matches!(
        select.value,
        Some(Expr::Lambda(ref params, _))
            if params == &["jobs".to_string()]
    ));
    let Entry::Property(value) = &module.body[2] else {
        panic!("expected constrained property");
    };
    assert!(matches!(
        value.type_ann,
        Some(TypeExpr::Constrained(_, ref constraint))
            if matches!(constraint.as_ref(), Expr::Binop(BinOp::And, _, _))
    ));
}

#[test]
fn type_constraints_do_not_consume_next_line_elements() {
    let source = r#"
items {
  value: String
  ("next")
}
"#;
    let module = parse(&lex(source).unwrap()).unwrap();
    let Entry::Property(items) = &module.body[0] else {
        panic!("expected items property");
    };
    let body = items.body.as_ref().expect("expected items body");
    assert_eq!(body.len(), 2);
    assert!(matches!(body[0], Entry::Property(_)));
    assert!(matches!(body[1], Entry::Elem(Expr::String(ref value)) if value == "next"));
}

#[test]
fn quoted_identifier_reports_missing_terminator() {
    let error = lex("`runs-on = true").unwrap_err().to_string();
    assert!(error.contains("unterminated quoted identifier"));
}

#[test]
fn collect_imports_finds_amends() {
    let src = r#"
amends "pkl/Config.pkl"
import "pkl/Builtins.pkl"
"#;
    let tokens = lex(src).unwrap();
    let imports = collect_imports(&tokens);
    assert!(imports.contains(&"pkl/Config.pkl".to_string()));
    assert!(imports.contains(&"pkl/Builtins.pkl".to_string()));
}

#[test]
fn parser_allows_semicolons_between_header_directives() {
    let src = r#"
amends "base.pkl"; import "helper.pkl"; x = helper.value
"#;
    let tokens = lex(src).unwrap();
    let module = parse(&tokens).unwrap();
    assert_eq!(module.amends.as_deref(), Some("base.pkl"));
    assert_eq!(module.imports.len(), 1);
    assert_eq!(module.imports[0].uri, "helper.pkl");
}

#[tokio::test]
async fn analyze_imports_deduplicates_diamond_graph() {
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_analyze_imports_diamond_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "left.pkl"
import "right.pkl"
"#,
    )
    .unwrap();
    std::fs::write(dir.join("left.pkl"), r#"import "shared.pkl""#).unwrap();
    std::fs::write(dir.join("right.pkl"), r#"import "shared.pkl""#).unwrap();
    std::fs::write(dir.join("shared.pkl"), "x = 1").unwrap();

    let imports = pklr::analyze_imports_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    let shared = dir.join("shared.pkl");
    assert_eq!(
        imports.iter().filter(|path| **path == shared).count(),
        1,
        "{imports:?}"
    );
}

#[tokio::test]
async fn analyze_imports_excludes_missing_files() {
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_analyze_imports_missing_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "missing.pkl"
import "existing.pkl"
"#,
    )
    .unwrap();
    std::fs::write(dir.join("existing.pkl"), "x = 1").unwrap();

    let imports = pklr::analyze_imports_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(imports, vec![dir.join("existing.pkl")]);
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Evaluator tests ---

fn eval_src(src: &str) -> serde_json::Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let path = std::path::Path::new("test.pkl");
        let val = ev.eval_source(src, path).await.unwrap();
        val.to_json()
    })
}

#[test]
fn eval_simple_object() {
    let src = r#"
amends "pkl/Config.pkl"
fail_fast = false
"#;
    let json = eval_src(src);
    assert_eq!(json["fail_fast"], serde_json::json!(false));
}

#[test]
fn eval_string_property() {
    let src = r#"
amends "pkl/Config.pkl"
default_branch = "main"
"#;
    let json = eval_src(src);
    assert_eq!(json["default_branch"], "main");
}

#[test]
fn eval_list_function() {
    let src = r#"
amends "pkl/Config.pkl"
warnings = List("missing-profiles", "no-steps")
"#;
    let json = eval_src(src);
    assert_eq!(
        json["warnings"],
        serde_json::json!(["missing-profiles", "no-steps"])
    );
}

#[test]
fn eval_nested_object_body() {
    let src = r#"
amends "pkl/Config.pkl"
hooks {
    ["pre-commit"] {
        fix = true
    }
}
"#;
    let json = eval_src(src);
    assert_eq!(json["hooks"]["pre-commit"]["fix"], serde_json::json!(true));
}

#[test]
fn eval_local_variable() {
    let src = r#"
amends "pkl/Config.pkl"
local myval = "hello"
default_branch = myval
"#;
    let json = eval_src(src);
    assert_eq!(json["default_branch"], "hello");
}

#[test]
fn eval_new_mapping() {
    let src = r#"
amends "pkl/Config.pkl"
local steps = new Mapping {
    ["cargo-fmt"] {
        glob = "**/*.rs"
        check = "cargo fmt --check"
        fix = "cargo fmt"
    }
}
hooks {
    ["pre-commit"] {
        steps = steps
    }
}
"#;
    let json = eval_src(src);
    assert_eq!(
        json["hooks"]["pre-commit"]["steps"]["cargo-fmt"]["glob"],
        "**/*.rs"
    );
}

#[test]
fn eval_mapping_amendment_uses_union_type_annotation_defaults() {
    let src = r#"
class Step {
    check: String?
}

class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Hook {
    steps: Mapping<String, Step | Group> = new Mapping<String, Step> {}
}

local formatters = new Mapping<String, Step> {
    ["echo"] {
        check = "echo ok"
    }
}

local baseHook = new Hook {
    steps {
        ["formatters"] = new Group {
            steps = formatters
        }
    }
}

hooks {
    ["check"] = baseHook
}
"#;
    let json = eval_src(src);
    assert_eq!(
        json["hooks"]["check"]["steps"]["formatters"]["steps"]["echo"]["check"],
        "echo ok"
    );
}

#[test]
fn eval_spread_operator() {
    let src = r#"
amends "pkl/Config.pkl"
local extra = new Mapping {
    ["b"] {
        check = "echo b"
    }
}
hooks {
    ["check"] {
        steps {
            ["a"] {
                check = "echo a"
            }
            ...extra
        }
    }
}
"#;
    let json = eval_src(src);
    assert_eq!(json["hooks"]["check"]["steps"]["a"]["check"], "echo a");
    assert_eq!(json["hooks"]["check"]["steps"]["b"]["check"], "echo b");
}

#[test]
fn eval_integer_and_bool() {
    let src = r#"
amends "pkl/Config.pkl"
fail_fast = true
"#;
    let json = eval_src(src);
    assert_eq!(json["fail_fast"], true);
}

#[tokio::test]
#[ignore = "requires network access to fetch package zip"]
async fn test_hk_config_amends() {
    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator.eval_source(
        r#"amends "package://github.com/jdx/hk/releases/download/v1.40.0/hk@1.40.0#/Config.pkl""#,
        std::path::Path::new("test_hk.pkl"),
    ).await;
    eprintln!("eval completed, is_ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_github_actions_workflow() {
    let mut evaluator = pklr::Evaluator::new_async();
    let cache_dir = std::env::temp_dir().join(format!(
        "pklr-github-actions-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    evaluator.set_package_cache_dir(&cache_dir);
    evaluator
        .preload_package_async(
            "https://github.com/apple/pkl-pantry/releases/download/com.github.actions@1.9.0/com.github.actions@1.9.0.zip",
            "zip",
            include_bytes!("fixtures/com.github.actions-1.9.0.zip"),
        )
        .await
        .unwrap();
    evaluator.set_offline(true);
    let source = r#"
amends "package://pkg.pkl-lang.org/pkl-pantry/com.github.actions@1.9.0#/Workflow.pkl"
import "package://pkg.pkl-lang.org/pkl-pantry/com.github.actions@1.9.0#/catalog.pkl"

jobs {
  ["build"] {
    `runs-on` = "ubuntu-latest"
    steps {
      new { run = "echo hello" }
      (catalog.`actions/checkout@v6`) { with { `fetch-depth` = 0 } }
    }
  }
}
"#;
    let value = evaluator
        .eval_source(source, std::path::Path::new("workflow.pkl"))
        .await
        .unwrap();
    let value = evaluator.apply_converters(value).await.unwrap();
    assert_eq!(value.to_json()["jobs"]["build"]["runs-on"], "ubuntu-latest");
    assert_eq!(
        value.to_json()["jobs"]["build"]["steps"][0]["run"],
        "echo hello"
    );
    assert_eq!(
        value.to_json()["jobs"]["build"]["steps"][1]["uses"],
        "actions/checkout@v6"
    );
    assert_eq!(
        value.to_json()["jobs"]["build"]["steps"][1]["with"]["fetch-depth"],
        0
    );
}

#[tokio::test]
#[ignore = "requires network access to fetch package zip"]
async fn test_hk_full_config() {
    let src = r#"
amends "package://github.com/jdx/hk/releases/download/v1.40.0/hk@1.40.0#/Config.pkl"
import "package://github.com/jdx/hk/releases/download/v1.40.0/hk@1.40.0#/Builtins.pkl"

local linters = new Mapping<String, Step> {
    ["inko-fmt"] {
        glob = "*.inko"
        check = "inko fmt --check {{files}}"
        fix = "inko fmt {{files}}"
    }
}

hooks {
    ["pre-commit"] {
        fix = true
        stash = "git"
        steps = linters
    }
}
"#;
    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(src, std::path::Path::new("test_hk_full.pkl"))
        .await;
    eprintln!("eval completed, is_ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    } else {
        eprintln!("eval ok");
    }
    assert!(result.is_ok());
}

/// Minimal, hermetic HTTP/1.1 server for tests. Serves a fixed set of
/// `path -> body` responses on a background thread and returns its base URL
/// (e.g. `http://127.0.0.1:PORT`). The thread runs for the lifetime of the
/// test process; each request gets `Connection: close` so the client does not
/// reuse connections.
fn spawn_test_http_server(routes: Vec<(&'static str, &'static str)>) -> String {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let routes: HashMap<String, String> = routes
        .into_iter()
        .map(|(p, b)| (p.to_string(), b.to_string()))
        .collect();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            // "GET /path HTTP/1.1"
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_string();
            // Drain remaining request headers.
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let response = match routes.get(&path) {
                Some(body) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
            };
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://127.0.0.1:{port}")
}

fn spawn_header_check_http_server(
    path: &'static str,
    header_name: &'static str,
    header_value: &'static str,
    body: &'static str,
) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let expected_header = format!("{}: {}", header_name.to_ascii_lowercase(), header_value);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let request_path = request_line.split_whitespace().nth(1).unwrap_or("/");
            let mut has_header = false;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {
                        if line.trim_end().to_ascii_lowercase() == expected_header {
                            has_header = true;
                        }
                    }
                    Err(_) => break,
                }
            }
            let response = if request_path == path && has_header {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            } else {
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            };
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn native_capabilities_fetch_bytes_uses_configured_client() {
    let base = spawn_header_check_http_server("/pkg.zip", "x-pklr-test", "ok", "zip-bytes");
    let mut headers = pklr::reqwest::header::HeaderMap::new();
    headers.insert(
        pklr::reqwest::header::HeaderName::from_static("x-pklr-test"),
        pklr::reqwest::header::HeaderValue::from_static("ok"),
    );
    let client = pklr::reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let mut capabilities = pklr::NativeCapabilities::with_http_client(client);

    let bytes = capabilities
        .fetch_bytes(&format!("{base}/pkg.zip"))
        .await
        .unwrap();

    assert_eq!(bytes, b"zip-bytes");
}

#[tokio::test]
async fn native_capabilities_map_not_found_to_import_errors() {
    let base = spawn_test_http_server(vec![]);
    let mut capabilities = pklr::NativeCapabilities::new();

    let text_url = format!("{base}/missing.pkl");
    let text_error = capabilities.fetch_text(&text_url).await.unwrap_err();
    assert!(matches!(
        text_error,
        pklr::Error::ImportNotFound(url) if url == text_url
    ));

    let bytes_url = format!("{base}/missing.zip");
    let bytes_error = capabilities.fetch_bytes(&bytes_url).await.unwrap_err();
    assert!(matches!(
        bytes_error,
        pklr::Error::ImportNotFound(url) if url == bytes_url
    ));
}

/// A module loaded over HTTP that itself uses a relative `import` should
/// resolve that import against its own (HTTP) URL, not the local filesystem.
#[tokio::test]
async fn http_module_resolves_relative_import() {
    let base = spawn_test_http_server(vec![
        (
            "/cfg/Main.pkl",
            "import \"../Lib.pkl\"\nvalue = Lib.value\n",
        ),
        ("/Lib.pkl", "value = 42\n"),
    ]);
    let src = format!("import \"{base}/cfg/Main.pkl\" as Main\nresult = Main.value\n");

    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(&src, std::path::Path::new("entry.pkl"))
        .await;
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    let json = result.unwrap().to_json();
    assert_eq!(json["result"], 42);
}

/// A module loaded over HTTP that itself uses a relative `amends` should
/// resolve that base against its own (HTTP) URL. With the relative base
/// unresolved, properties inherited through it (here `version`) go missing.
#[tokio::test]
async fn http_module_resolves_relative_amends() {
    let base = spawn_test_http_server(vec![
        (
            "/cfg/Main.pkl",
            "amends \"../Base.pkl\"\nname = \"override\"\n",
        ),
        ("/Base.pkl", "name = \"base\"\nversion = 1\n"),
    ]);
    let src = format!("amends \"{base}/cfg/Main.pkl\"\n");

    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(&src, std::path::Path::new("entry.pkl"))
        .await;
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    let json = result.unwrap().to_json();
    assert_eq!(json["name"], "override");
    assert_eq!(json["version"], 1);
}

/// A module loaded over HTTP that itself uses a relative `extends` should
/// resolve that base against its own (HTTP) URL, not the local filesystem.
#[tokio::test]
async fn http_module_resolves_relative_extends() {
    let base = spawn_test_http_server(vec![
        (
            "/cfg/Main.pkl",
            "extends \"../Base.pkl\"\nname = \"override\"\n",
        ),
        ("/Base.pkl", "name = \"base\"\nversion = 1\n"),
    ]);
    let src = format!("import \"{base}/cfg/Main.pkl\" as Main\nresult = Main\n");

    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(&src, std::path::Path::new("entry.pkl"))
        .await;
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    let json = result.unwrap().to_json();
    assert_eq!(json["result"]["name"], "override");
    assert_eq!(json["result"]["version"], 1);
}

#[tokio::test]
async fn test_step_amend_minimal() {
    // Test: does amending a class with many properties hang?
    let src = r#"
class Step {
  a: String?
  b: String?
  c: String?
  d: String?
  e: String?
  f: String?
  g: String?
  h: String?
  i: String?
  j: String?
  env: Mapping<String, String> = new Mapping<String, String> {}
  tests: Mapping<String, String> = new Mapping<String, String> {}
}

local steps = new Mapping<String, Step> {
    ["step1"] {
        a = "hello"
    }
}

result = steps
"#;
    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(src, std::path::Path::new("test_min.pkl"))
        .await;
    eprintln!("eval completed, is_ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_nested_class_amend() {
    // Closer to real Config.pkl: classes referencing each other
    let src = r#"
class StepTestExpect {
  code = 0
  stdout: String?
  stderr: String?
  files: Mapping<String, String> = new Mapping<String, String> {}
}

class StepTest {
  run: String = "check"
  files: (String)?
  env: Mapping<String, String> = new Mapping<String, String> {}
  expect: StepTestExpect = new StepTestExpect {}
}

class Step {
  a: String?
  b: String?
  c: String?
  d: String?
  e: String?
  f: String?
  g: String?
  h: String?
  i: String?
  j: String?
  k: String?
  l: String?
  m: String?
  n: String?
  o: String?
  p: String?
  q: String?
  r: String?
  s: String?
  t: String?
  env: Mapping<String, String> = new Mapping<String, String> {}
  tests: Mapping<String, StepTest> = new Mapping<String, StepTest> {}
}

class Hook {
  fix: Boolean?
  stash: String?
  env: Mapping<String, String> = new Mapping<String, String> {}
  steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}
"#;
    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(src, std::path::Path::new("test_nested.pkl"))
        .await;
    eprintln!("eval completed, is_ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_nested_class_with_amend() {
    let src = r#"
class StepTestExpect {
  code = 0
  stdout: String?
  files: Mapping<String, String> = new Mapping<String, String> {}
}

class StepTest {
  run: String = "check"
  env: Mapping<String, String> = new Mapping<String, String> {}
  expect: StepTestExpect = new StepTestExpect {}
}

class Step {
  a: String?
  b: String?
  c: String?
  d: String?
  e: String?
  f: String?
  g: String?
  h: String?
  env: Mapping<String, String> = new Mapping<String, String> {}
  tests: Mapping<String, StepTest> = new Mapping<String, StepTest> {}
}

class Hook {
  fix: Boolean?
  stash: String?
  env: Mapping<String, String> = new Mapping<String, String> {}
  steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

local linters = new Mapping<String, Step> {
    ["step1"] {
        a = "hello"
    }
}

hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}

hooks {
    ["pre-commit"] {
        fix = true
        steps = linters
    }
}
"#;
    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(src, std::path::Path::new("test_nested_amend.pkl"))
        .await;
    eprintln!("eval completed, is_ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "requires /tmp/hk-extracted/ from local dev setup"]
async fn test_local_config_pkl() {
    // Test with the actual extracted Config.pkl
    let src = std::fs::read_to_string("/tmp/hk-extracted/Config.pkl").unwrap();
    let mut evaluator = pklr::Evaluator::new_async();
    let result = evaluator
        .eval_source(&src, std::path::Path::new("/tmp/hk-extracted/Config.pkl"))
        .await;
    eprintln!("eval completed, is_ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "requires /tmp/hk-extracted/ from local dev setup"]
async fn test_single_builtin() {
    // Test evaluating just one builtin file directly
    let src = std::fs::read_to_string("/tmp/hk-extracted/builtins/actionlint.pkl").unwrap();
    let mut evaluator = pklr::Evaluator::new_async();
    let start = std::time::Instant::now();
    let result = evaluator
        .eval_source(
            &src,
            std::path::Path::new("/tmp/hk-extracted/builtins/actionlint.pkl"),
        )
        .await;
    eprintln!(
        "single builtin eval: {}ms, ok={}",
        start.elapsed().as_millis(),
        result.is_ok()
    );
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
}

#[tokio::test]
#[ignore = "requires /tmp/hk-extracted/ from local dev setup"]
async fn test_builtins_pkl() {
    // Test evaluating Builtins.pkl (which imports all 128 builtins)
    let src = std::fs::read_to_string("/tmp/hk-extracted/Builtins.pkl").unwrap();
    let mut evaluator = pklr::Evaluator::new_async();
    let start = std::time::Instant::now();
    let result = evaluator
        .eval_source(&src, std::path::Path::new("/tmp/hk-extracted/Builtins.pkl"))
        .await;
    eprintln!(
        "builtins eval: {}ms, ok={}",
        start.elapsed().as_millis(),
        result.is_ok()
    );
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
}

#[tokio::test]
async fn test_outer_before_pattern() {
    let src = r#"
class StepTest {
  run: String = "check"
  before: String?
}

class TestMaker {
  before: String?
  local function makeTest(runType: String): StepTest = new StepTest {
    run = runType
    before = outer.before
  }
  function checkPass(): StepTest = makeTest("check")
}

local tm = new TestMaker { before = "git init" }
result = tm.checkPass()
"#;
    let mut ev = pklr::Evaluator::new_async();
    let result = ev
        .eval_source(src, std::path::Path::new("test_outer.pkl"))
        .await;
    eprintln!("result: {:?}", result.as_ref().map(|v| v.to_json()));
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
    assert_eq!(result.unwrap().to_json()["result"]["before"], "git init");
}

#[tokio::test]
#[ignore = "requires /tmp/hk-extracted/ from local dev setup"]
async fn test_outer_before_with_local_config() {
    // Simulate the helpers.pkl pattern with real Config.pkl
    let src =
        std::fs::read_to_string("/tmp/hk-extracted/builtins/test/helpers.pkl").unwrap_or_default();
    if src.is_empty() {
        return;
    }
    // Test: can we evaluate helpers.pkl which uses outer.before?
    let mut ev = pklr::Evaluator::new_async();
    let result = ev
        .eval_source(
            &src,
            std::path::Path::new("/tmp/hk-extracted/builtins/test/helpers.pkl"),
        )
        .await;
    eprintln!("helpers eval: ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "requires /tmp/hk-extracted/ from local dev setup"]
async fn test_outer_before_single_builtin() {
    let src = std::fs::read_to_string("/tmp/hk-extracted/builtins/no_commit_to_branch.pkl")
        .unwrap_or_default();
    if src.is_empty() {
        return;
    }
    let mut ev = pklr::Evaluator::new_async();
    let result = ev
        .eval_source(
            &src,
            std::path::Path::new("/tmp/hk-extracted/builtins/no_commit_to_branch.pkl"),
        )
        .await;
    eprintln!("no_commit_to_branch eval: ok={}", result.is_ok());
    if let Err(ref e) = result {
        eprintln!("error: {e}");
    }
    assert!(result.is_ok());
}
