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
