//! Deeply nested modules evaluate, and nesting past what the stack can hold
//! is an error. The test harness runs each test on a 2 MiB stack, so these
//! cover the case where a caller's own stack is far too small.

use pklr::Evaluator;

/// Evaluate `src` as a file through the public entry point, as a host does.
fn eval_source(src: &str) -> pklr::Result<serde_json::Value> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("pklr-nesting-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("nesting.pkl");
    std::fs::write(&path, src).unwrap();
    let result = pklr::eval_to_json(&path);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn nested_lets(n: usize) -> String {
    let mut src = String::from("x = ");
    for i in 0..n {
        let value = if i == 0 {
            "0".to_string()
        } else {
            format!("a{}", i - 1)
        };
        src.push_str(&format!("let (a{i} = {value}) "));
    }
    src.push_str(&format!("a{}", n - 1));
    src
}

fn x(value: &serde_json::Value) -> serde_json::Value {
    value["x"].clone()
}

#[test]
fn nested_lets_past_the_old_evaluator_cap_evaluate() {
    for depth in [32, 33, 40, 200] {
        let value = eval_source(&nested_lets(depth)).unwrap();
        assert_eq!(x(&value), serde_json::json!(0), "depth {depth}");
    }
}

#[test]
fn nested_lets_far_past_the_parser_cap_evaluate() {
    let value = eval_source(&nested_lets(1_000)).unwrap();
    assert_eq!(x(&value), serde_json::json!(0));
}

#[test]
fn nested_parentheses_evaluate() {
    let src = format!("x = {}1{}", "(".repeat(1_500), ")".repeat(1_500));
    assert_eq!(x(&eval_source(&src).unwrap()), serde_json::json!(1));
}

#[test]
fn nested_objects_evaluate() {
    let n = 500;
    let src = format!("x = {}1{}", "new Dynamic { a = ".repeat(n), " }".repeat(n));
    let json = eval_source(&src).unwrap();
    assert!(json["x"]["a"]["a"].is_object());
}

#[test]
fn pathological_nesting_is_an_error_not_a_crash() {
    for n in [200_000, 1_000_000] {
        let src = format!("x = {}1{}", "(".repeat(n), ")".repeat(n));
        let err = eval_source(&src).unwrap_err().to_string();
        assert!(err.contains("nested more than"), "depth {n}: {err}");
    }
}

#[test]
fn evaluation_depth_is_configurable() {
    let mut evaluator = Evaluator::new();
    evaluator.set_max_depth(8);
    let err = evaluator
        .eval_source(&nested_lets(100), std::path::Path::new("nesting.pkl"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("maximum recursion depth exceeded"), "{err}");
}

#[test]
fn direct_parse_keeps_the_conservative_cap() {
    // A debug build spends tens of KiB of stack per level, so give the caller
    // a stack that holds the capped depth.
    let err = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            let src = format!("x = {}1{}", "(".repeat(1_000), ")".repeat(1_000));
            let tokens = pklr::lexer::lex(&src).unwrap();
            pklr::parser::parse(&tokens).unwrap_err().to_string()
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(err.contains("nested more than 128 levels"), "{err}");
}

#[test]
fn dropping_an_evaluator_after_a_deep_module_does_not_overflow() {
    let depth = std::env::var("PKLR_TEST_DEPTH")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(3_000);
    let dir = std::env::temp_dir().join(format!("pklr-nesting-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("deep.pkl");
    std::fs::write(&path, nested_lets(depth)).unwrap();
    let mut evaluator = Evaluator::new();
    let result = evaluator.eval_file(&path);
    // The parsed tree is freed here, on the test's small stack.
    drop(evaluator);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.is_ok());
}

#[test]
fn a_long_amends_chain_is_an_error_or_a_result_not_a_crash() {
    let n: usize = std::env::var("PKLR_TEST_CHAIN")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(300);
    let dir = std::env::temp_dir().join(format!("pklr-nesting-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        let body = if i + 1 == n {
            "x = 1\n".to_string()
        } else {
            format!("amends \"m{}.pkl\"\n", i + 1)
        };
        std::fs::write(dir.join(format!("m{i}.pkl")), body).unwrap();
    }
    let result = pklr::eval_to_json(&dir.join("m0.pkl"));
    let _ = std::fs::remove_dir_all(&dir);
    if let Ok(json) = result {
        assert_eq!(json["x"], 1);
    }
}
