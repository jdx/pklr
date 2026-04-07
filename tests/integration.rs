use pklr::eval::Evaluator;
use pklr::lexer::{TokenKind, lex};
use pklr::parser::{collect_imports, parse};

fn lex_kinds(src: &str) -> Vec<TokenKind> {
    lex(src).unwrap().into_iter().map(|t| t.kind).collect()
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

// --- Evaluator tests ---

fn eval_src(src: &str) -> serde_json::Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut ev = Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut evaluator = pklr::Evaluator::new();
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
    let mut ev = pklr::Evaluator::new();
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
    let mut ev = pklr::Evaluator::new();
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
    let mut ev = pklr::Evaluator::new();
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
