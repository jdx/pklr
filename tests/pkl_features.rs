//! Tests for pkl language features, organized by category.
//!
//! Tests marked `#[ignore]` document features not yet implemented.
//! As features are added, remove the `#[ignore]` attribute.

use pklr::eval::Evaluator;

fn eval(src: &str) -> serde_json::Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let path = std::path::Path::new("test.pkl");
        let val = ev.eval_source(src, path).await.unwrap();
        val.to_json()
    })
}

/// Like eval(), but also applies output.renderer.converters (full pipeline).
fn eval_with_converters(src: &str) -> serde_json::Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let path = std::path::Path::new("test.pkl");
        let val = ev.eval_source(src, path).await.unwrap();
        let val = ev.apply_converters(val).await.unwrap();
        val.to_json()
    })
}

fn eval_fails(src: &str) -> String {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let path = std::path::Path::new("test.pkl");
        match ev.eval_source(src, path).await {
            Err(e) => e.to_string(),
            Ok(v) => panic!("expected error, got: {:?}", v.to_json()),
        }
    })
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

// ============================================================
// Primitives
// ============================================================

#[test]
fn primitives_int() {
    let json = eval(r#"x = 42"#);
    assert_eq!(json["x"], 42);
}

#[test]
fn primitives_negative_int() {
    let json = eval(r#"x = -7"#);
    assert_eq!(json["x"], -7);
}

#[test]
fn primitives_hex() {
    let json = eval(r#"x = 0xFF"#);
    assert_eq!(json["x"], 255);
}

#[test]
fn primitives_octal() {
    let json = eval(r#"x = 0o77"#);
    assert_eq!(json["x"], 63);
}

#[test]
fn primitives_binary() {
    let json = eval(r#"x = 0b1010"#);
    assert_eq!(json["x"], 10);
}

#[test]
fn primitives_float() {
    let json = eval(r#"x = 1.5"#);
    assert_eq!(json["x"], 1.5);
}

#[test]
fn primitives_float_exponent() {
    let json = eval(r#"x = 1e3"#);
    assert_eq!(json["x"], 1000.0);
}

#[test]
fn primitives_bool_true() {
    let json = eval(r#"x = true"#);
    assert_eq!(json["x"], true);
}

#[test]
fn primitives_bool_false() {
    let json = eval(r#"x = false"#);
    assert_eq!(json["x"], false);
}

#[test]
fn primitives_null() {
    let json = eval(r#"x = null"#);
    assert!(json["x"].is_null());
}

#[test]
fn primitives_underscored_int() {
    let json = eval(r#"x = 1_000_000"#);
    assert_eq!(json["x"], 1_000_000);
}

// ============================================================
// NaN and Infinity
// ============================================================

#[test]
fn nan_literal() {
    // NaN serializes to null in JSON (JSON has no NaN)
    let json = eval(r#"x = NaN"#);
    assert!(json["x"].is_null());
}

#[test]
fn infinity_literal() {
    // Infinity serializes to null in JSON (JSON has no Infinity)
    let json = eval(r#"x = Infinity"#);
    assert!(json["x"].is_null());
}

#[test]
fn negative_infinity() {
    let json = eval(r#"x = -Infinity"#);
    assert!(json["x"].is_null());
}

#[test]
fn nan_is_not_equal_to_itself() {
    let json = eval(r#"x = NaN == NaN"#);
    assert_eq!(json["x"], false);
}

#[test]
fn nan_comparison() {
    let json = eval(r#"x = NaN != NaN"#);
    assert_eq!(json["x"], true);
}

// ============================================================
// Strings
// ============================================================

#[test]
fn string_basic() {
    let json = eval(r#"x = "hello world""#);
    assert_eq!(json["x"], "hello world");
}

#[test]
fn string_escapes() {
    let json = eval(r#"x = "a\nb\tc""#);
    assert_eq!(json["x"], "a\nb\tc");
}

#[test]
fn string_multiline() {
    let src = "x = \"\"\"\n  hello\n  world\n  \"\"\"";
    let json = eval(src);
    assert_eq!(json["x"], "hello\nworld\n");
}

#[test]
fn string_raw_multiline() {
    let src = "x = #\"\"\"\n  hello\\n\n  world\n  \"\"\"#";
    let json = eval(src);
    assert_eq!(json["x"], "hello\\n\nworld\n");
}

#[test]
fn string_multiline_strips_only_one_opening_newline() {
    let src = "x = \"\"\"\n\n  hello\n  \"\"\"";
    let json = eval(src);
    assert_eq!(json["x"], "\nhello\n");
}

#[test]
fn string_multi_hash_raw() {
    let json = eval(r####"x = ##"hello "# world"##"####);
    assert_eq!(json["x"], "hello \"# world");
}

#[test]
fn string_multi_hash_raw_multiline() {
    let src = "x = ##\"\"\"\n  hello \"\"\"#\n  world\n  \"\"\"##";
    let json = eval(src);
    assert_eq!(json["x"], "hello \"\"\"#\nworld\n");
}

#[test]
fn string_unicode_escape() {
    let json = eval(r#"x = "\u{26} \u{E9} \u{1F600}""#);
    assert_eq!(json["x"], "& \u{E9} \u{1F600}");
}

#[test]
fn string_unicode_escape_simple() {
    let json = eval(r#"x = "\u{41}""#);
    assert_eq!(json["x"], "A");
}

#[test]
fn string_concatenation() {
    let json = eval(r#"x = "hello" + " " + "world""#);
    assert_eq!(json["x"], "hello world");
}

#[test]
fn string_interpolation() {
    let json = eval(
        r#"
local name = "world"
x = "hello \(name)"
"#,
    );
    assert_eq!(json["x"], "hello world");
}

#[test]
fn string_interpolation_expr() {
    let json = eval(
        r#"
x = "2 + 2 = \(2 + 2)"
"#,
    );
    assert_eq!(json["x"], "2 + 2 = 4");
}

// ============================================================
// Arithmetic
// ============================================================

#[test]
fn arithmetic_add() {
    let json = eval(r#"x = 2 + 3"#);
    assert_eq!(json["x"], 5);
}

#[test]
fn arithmetic_sub() {
    let json = eval(r#"x = 10 - 3"#);
    assert_eq!(json["x"], 7);
}

#[test]
fn arithmetic_mul() {
    let json = eval(r#"x = 4 * 5"#);
    assert_eq!(json["x"], 20);
}

#[test]
fn arithmetic_div() {
    let json = eval(r#"x = 10 / 3"#);
    assert_eq!(json["x"], 3);
}

#[test]
fn arithmetic_mod() {
    let json = eval(r#"x = 10 % 3"#);
    assert_eq!(json["x"], 1);
}

#[test]
fn arithmetic_float_div() {
    let json = eval(r#"x = 10.0 / 3.0"#);
    let v = json["x"].as_f64().unwrap();
    assert!((v - (10.0 / 3.0)).abs() < 1e-9);
}

#[test]
fn arithmetic_precedence() {
    let json = eval(r#"x = 2 + 3 * 4"#);
    assert_eq!(json["x"], 14);
}

#[test]
fn arithmetic_parens() {
    let json = eval(r#"x = (2 + 3) * 4"#);
    assert_eq!(json["x"], 20);
}

#[test]
fn arithmetic_div_by_zero() {
    let msg = eval_fails(r#"x = 1 / 0"#);
    assert!(msg.contains("division by zero") || msg.contains("divide by zero"));
}

#[test]
fn arithmetic_mod_by_zero() {
    let msg = eval_fails(r#"x = 1 % 0"#);
    assert!(msg.contains("modulo by zero"));
}

// ============================================================
// Integer division
// ============================================================

#[test]
fn int_div_basic() {
    let json = eval(r#"x = 7 ~/ 2"#);
    assert_eq!(json["x"], 3);
}

#[test]
fn int_div_negative() {
    let json = eval(r#"x = -7 ~/ 2"#);
    assert_eq!(json["x"], -3);
}

#[test]
fn int_div_float() {
    let json = eval(r#"x = 7.5 ~/ 2.0"#);
    assert_eq!(json["x"], 3.0);
}

#[test]
fn int_div_by_zero() {
    let msg = eval_fails(r#"x = 7 ~/ 0"#);
    assert!(msg.contains("division by zero"));
}

// ============================================================
// Exponentiation
// ============================================================

#[test]
fn exp_basic() {
    let json = eval(r#"x = 2 ** 10"#);
    assert_eq!(json["x"], 1024);
}

#[test]
fn exp_float() {
    let json = eval(r#"x = 2.0 ** 3.0"#);
    assert_eq!(json["x"], 8.0);
}

#[test]
fn exp_right_associative() {
    // 2 ** 3 ** 2 should be 2 ** (3 ** 2) = 2 ** 9 = 512
    let json = eval(r#"x = 2 ** 3 ** 2"#);
    assert_eq!(json["x"], 512);
}

#[test]
fn exp_precedence() {
    // 2 * 3 ** 2 should be 2 * (3 ** 2) = 2 * 9 = 18
    let json = eval(r#"x = 2 * 3 ** 2"#);
    assert_eq!(json["x"], 18);
}

#[test]
fn exp_negative_exponent_errors() {
    let msg = eval_fails(r#"x = 2 ** -1"#);
    assert!(msg.contains("negative exponent"));
}

#[test]
fn exp_float_negative_exponent() {
    let json = eval(r#"x = 2.0 ** -1.0"#);
    assert_eq!(json["x"], 0.5);
}

// ============================================================
// Non-null assertion
// ============================================================

#[test]
fn non_null_assertion_pass() {
    let json = eval(
        r#"
local x = 42
y = x!!
"#,
    );
    assert_eq!(json["y"], 42);
}

#[test]
fn non_null_assertion_fail() {
    let msg = eval_fails(
        r#"
local x = null
y = x!!
"#,
    );
    assert!(msg.contains("non-null assertion failed"));
}

#[test]
fn non_null_assertion_string() {
    let json = eval(
        r#"
local x = "hello"
y = x!!
"#,
    );
    assert_eq!(json["y"], "hello");
}

// ============================================================
// Pipe operator
// ============================================================

#[test]
fn pipe_basic() {
    let json = eval(
        r#"
local double = (x) -> x * 2
result = 5 |> double
"#,
    );
    assert_eq!(json["result"], 10);
}

#[test]
fn pipe_chain() {
    let json = eval(
        r#"
local double = (x) -> x * 2
local addOne = (x) -> x + 1
result = 5 |> double |> addOne
"#,
    );
    assert_eq!(json["result"], 11);
}

#[test]
fn pipe_multi_param_errors() {
    let msg = eval_fails(
        r#"
local add = (a, b) -> a + b
result = 5 |> add
"#,
    );
    assert!(msg.contains("single-parameter"));
}

// ============================================================
// Comparison and logical operators
// ============================================================

#[test]
fn comparison_eq() {
    let json = eval(r#"x = 1 == 1"#);
    assert_eq!(json["x"], true);
}

#[test]
fn comparison_ne() {
    let json = eval(r#"x = 1 != 2"#);
    assert_eq!(json["x"], true);
}

#[test]
fn comparison_lt() {
    let json = eval(r#"x = 1 < 2"#);
    assert_eq!(json["x"], true);
}

#[test]
fn comparison_gt() {
    let json = eval(r#"x = 2 > 1"#);
    assert_eq!(json["x"], true);
}

#[test]
fn logical_and() {
    let json = eval(r#"x = true && false"#);
    assert_eq!(json["x"], false);
}

#[test]
fn logical_or() {
    let json = eval(r#"x = true || false"#);
    assert_eq!(json["x"], true);
}

#[test]
fn logical_not() {
    let json = eval(r#"x = !false"#);
    assert_eq!(json["x"], true);
}

#[test]
fn logical_and_short_circuits() {
    // The right operand must not be evaluated when the left is false:
    // `missing.field` would error if it were touched.
    let json = eval(
        r#"
local v = new Dynamic { other = 1 }
x = if (false && v.missing) "y" else "n"
"#,
    );
    assert_eq!(json["x"], "n");
}

#[test]
fn logical_or_short_circuits() {
    // The right operand must not be evaluated when the left is true.
    let json = eval(
        r#"
local v = new Dynamic { other = 1 }
x = if (true || v.missing) "y" else "n"
"#,
    );
    assert_eq!(json["x"], "y");
}

// ============================================================
// Null coalescing
// ============================================================

#[test]
fn null_coalesce_non_null() {
    let json = eval(r#"x = "hello" ?? "default""#);
    assert_eq!(json["x"], "hello");
}

#[test]
fn null_coalesce_null() {
    let json = eval(r#"x = null ?? "default""#);
    assert_eq!(json["x"], "default");
}

// ============================================================
// If/else expressions
// ============================================================

#[test]
fn if_else_true() {
    let json = eval(r#"x = if (true) "yes" else "no""#);
    assert_eq!(json["x"], "yes");
}

#[test]
fn if_else_false() {
    let json = eval(r#"x = if (false) "yes" else "no""#);
    assert_eq!(json["x"], "no");
}

#[test]
fn if_else_complex_condition() {
    let json = eval(
        r#"
local n = 10
x = if (n > 5) "big" else "small"
"#,
    );
    assert_eq!(json["x"], "big");
}

// ============================================================
// Let expressions
// ============================================================

#[test]
fn let_basic() {
    let json = eval(
        r#"
x = let (a = 1) let (b = 2) a + b
"#,
    );
    assert_eq!(json["x"], 3);
}

// ============================================================
// Local variables
// ============================================================

#[test]
fn local_basic() {
    let json = eval(
        r#"
local greeting = "hello"
x = greeting
"#,
    );
    assert_eq!(json["x"], "hello");
}

#[test]
fn local_not_in_output() {
    let json = eval(
        r#"
local secret = "hidden"
visible = "shown"
"#,
    );
    assert!(json.get("secret").is_none());
    assert_eq!(json["visible"], "shown");
}

#[test]
fn local_reference_other_local() {
    let json = eval(
        r#"
local a = "hello"
local b = a + " world"
x = b
"#,
    );
    assert_eq!(json["x"], "hello world");
}

// ============================================================
// Objects
// ============================================================

#[test]
fn object_nested() {
    let json = eval(
        r#"
outer {
    inner {
        value = 42
    }
}
"#,
    );
    assert_eq!(json["outer"]["inner"]["value"], 42);
}

#[test]
fn object_dynamic_key() {
    let json = eval(
        r#"
data {
    ["my-key"] = "value"
}
"#,
    );
    assert_eq!(json["data"]["my-key"], "value");
}

#[test]
fn object_dynamic_key_with_body() {
    let json = eval(
        r#"
data {
    ["my-key"] {
        nested = true
    }
}
"#,
    );
    assert_eq!(json["data"]["my-key"]["nested"], true);
}

// ============================================================
// Listings (List)
// ============================================================

#[test]
fn list_function() {
    let json = eval(r#"x = List(1, 2, 3)"#);
    assert_eq!(json["x"], serde_json::json!([1, 2, 3]));
}

#[test]
fn list_strings() {
    let json = eval(r#"x = List("a", "b", "c")"#);
    assert_eq!(json["x"], serde_json::json!(["a", "b", "c"]));
}

#[test]
fn list_empty() {
    let json = eval(r#"x = List()"#);
    assert_eq!(json["x"], serde_json::json!([]));
}

#[test]
fn list_concatenation() {
    let json = eval(r#"x = List(1, 2) + List(3, 4)"#);
    assert_eq!(json["x"], serde_json::json!([1, 2, 3, 4]));
}

#[test]
fn listing_body() {
    let json = eval(
        r#"
x = new Listing {
    "a"
    "b"
    "c"
}
"#,
    );
    assert_eq!(json["x"], serde_json::json!(["a", "b", "c"]));
}

// ============================================================
// Mappings
// ============================================================

#[test]
fn mapping_basic() {
    let json = eval(
        r#"
x = new Mapping {
    ["a"] = 1
    ["b"] = 2
}
"#,
    );
    assert_eq!(json["x"]["a"], 1);
    assert_eq!(json["x"]["b"], 2);
}

#[test]
fn mapping_with_body() {
    let json = eval(
        r#"
x = new Mapping {
    ["key"] {
        nested = true
    }
}
"#,
    );
    assert_eq!(json["x"]["key"]["nested"], true);
}

#[test]
fn map_function() {
    let json = eval(r#"x = Map("a", 1, "b", 2)"#);
    assert_eq!(json["x"]["a"], 1);
    assert_eq!(json["x"]["b"], 2);
}

#[test]
fn new_mapping_with_generic_params() {
    let json = eval(
        r#"
x = new Mapping<String, String> {
    ["a"] = "hello"
    ["b"] = "world"
}
"#,
    );
    assert_eq!(json["x"]["a"], "hello");
    assert_eq!(json["x"]["b"], "world");
}

#[test]
fn new_listing_with_generic_params() {
    let json = eval(
        r#"
x = new Listing<String> {
    "a"
    "b"
    "c"
}
"#,
    );
    assert_eq!(json["x"], serde_json::json!(["a", "b", "c"]));
}

#[test]
fn new_mapping_nested_generic_params() {
    let json = eval(
        r#"
x = new Mapping<String, Mapping<String, Int>> {
    ["outer"] = new Mapping<String, Int> {
        ["inner"] = 42
    }
}
"#,
    );
    assert_eq!(json["x"]["outer"]["inner"], 42);
}

// ============================================================
// Spread operator
// ============================================================

#[test]
fn spread_into_object() {
    let json = eval(
        r#"
local base = new Mapping {
    ["a"] = 1
    ["b"] = 2
}
x {
    ...base
    ["c"] = 3
}
"#,
    );
    assert_eq!(json["x"]["a"], 1);
    assert_eq!(json["x"]["b"], 2);
    assert_eq!(json["x"]["c"], 3);
}

// ============================================================
// For generators
// ============================================================

#[test]
fn for_generator_list() {
    let json = eval(
        r#"
local items = List("a", "b")
x {
    for (_i, v in items) {
        [v] = true
    }
}
"#,
    );
    assert_eq!(json["x"]["a"], true);
    assert_eq!(json["x"]["b"], true);
}

#[test]
fn for_generator_object() {
    let json = eval(
        r#"
local src = new Mapping {
    ["x"] = 1
    ["y"] = 2
}
out {
    for (k, v in src) {
        [k] = v
    }
}
"#,
    );
    assert_eq!(json["out"]["x"], 1);
    assert_eq!(json["out"]["y"], 2);
}

// ============================================================
// When generators
// ============================================================

#[test]
fn when_true() {
    let json = eval(
        r#"
local enabled = true
x {
    when (enabled) {
        feature = "on"
    }
}
"#,
    );
    assert_eq!(json["x"]["feature"], "on");
}

#[test]
fn when_false() {
    let json = eval(
        r#"
local enabled = false
x {
    when (enabled) {
        feature = "on"
    }
}
"#,
    );
    assert!(json["x"].get("feature").is_none());
}

#[test]
fn when_else() {
    let json = eval(
        r#"
local enabled = false
x {
    when (enabled) {
        mode = "fast"
    } else {
        mode = "slow"
    }
}
"#,
    );
    assert_eq!(json["x"]["mode"], "slow");
}

// ============================================================
// String interpolation (future)
// ============================================================

#[test]
fn interpolation_in_key() {
    let json = eval(
        r#"
local prefix = "my"
x {
    ["\(prefix)-key"] = "value"
}
"#,
    );
    assert_eq!(json["x"]["my-key"], "value");
}

// ============================================================
// Lambdas / function expressions (future)
// ============================================================

#[test]
fn lambda_basic() {
    let json = eval(
        r#"
local double = (x) -> x * 2
result = double.apply(5)
"#,
    );
    assert_eq!(json["result"], 10);
}

#[test]
fn lambda_two_params() {
    let json = eval(
        r#"
local add = (a, b) -> a + b
result = add.apply(3, 4)
"#,
    );
    assert_eq!(json["result"], 7);
}

#[test]
fn lambda_captures_scope() {
    let json = eval(
        r#"
local multiplier = 3
local mul = (x) -> x * multiplier
result = mul.apply(5)
"#,
    );
    assert_eq!(json["result"], 15);
}

// ============================================================
// Method calls on values (future)
// ============================================================

#[test]
fn method_length() {
    let json = eval(
        r#"
x = List(1, 2, 3).length
"#,
    );
    assert_eq!(json["x"], 3);
}

#[test]
fn method_is_empty() {
    let json = eval(
        r#"
x = List().isEmpty
"#,
    );
    assert_eq!(json["x"], true);
}

#[test]
fn string_to_boolean() {
    let json = eval(
        r#"
truthy = "true".toBoolean()
falsy = "false".toBoolean()
uppercase_truthy = "TRUE".toBoolean()
mixed_falsy = "False".toBoolean()
nullish = null?.toBoolean()
null_safe = "false"?.toBoolean()
"#,
    );
    assert_eq!(json["truthy"], true);
    assert_eq!(json["falsy"], false);
    assert_eq!(json["uppercase_truthy"], true);
    assert_eq!(json["mixed_falsy"], false);
    assert_eq!(json["nullish"], serde_json::Value::Null);
    assert_eq!(json["null_safe"], false);
}

// ============================================================
// Import resolution (future)
// ============================================================

#[tokio::test]
async fn import_local_file() {
    let mut ev = pklr::eval::Evaluator::new_async();
    // Set base path so relative imports resolve correctly
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
import "helper.pkl"
x = helper.value
"#;
    let path = base.join("test_import.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["x"], 42);
}

// ============================================================
// Amends resolution
// ============================================================

#[tokio::test]
async fn amends_local_file() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
amends "base.pkl"
name = "override"
"#;
    let path = base.join("test_amends.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    // name is overridden
    assert_eq!(json["name"], "override");
    // version and enabled are inherited from base
    assert_eq!(json["version"], 1);
    assert_eq!(json["enabled"], true);
}

#[tokio::test]
async fn amends_strips_inherited_class_definitions() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
amends "base_with_class.pkl"
name = "override"
"#;
    let path = base.join("test_amends_class.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["name"], "override");
    assert!(
        json.get("Script").is_none(),
        "inherited class 'Script' should be stripped from amends output, got: {json}"
    );
}

#[tokio::test]
async fn extends_strips_inherited_class_definitions() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_with_class.pkl"
name = "child"
"#;
    let path = base.join("test_extends_class.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["name"], "child");
    assert!(
        json.get("Script").is_none(),
        "inherited class 'Script' should be stripped from extends output, got: {json}"
    );
}

// ============================================================
// Circular imports
// ============================================================

#[tokio::test]
async fn circular_import_does_not_loop() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let path = base.join("circular_a.pkl");
    let val = ev.eval_file_pub(&path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["a_value"], "from_a");
    // b_ref resolves to from_b via circular_b.pkl
    assert_eq!(json["b_ref"], "from_b");
}

#[tokio::test]
async fn partial_imports_keep_circular_placeholder() {
    let temp = TestTempDir::new("pklr_test_partial_import_cycle");
    let dir = temp.path();
    std::fs::write(
        dir.join("a.pkl"),
        r#"
import "b.pkl"
a_value = "from_a"
b_ref = b.b_value
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("b.pkl"),
        r#"
import "a.pkl"
b_value = "from_b"
a_ref = a.a_value
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "a.pkl"
result = a.a_value
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "from_a");
}

// ============================================================
// Glob imports (import*)
// ============================================================

#[tokio::test]
async fn import_glob() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
import* "items/*.pkl" as Items
alpha_val = Items["items/alpha.pkl"].value
beta_val = Items["items/beta.pkl"].value
"#;
    let path = base.join("test_glob.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["alpha_val"], "alpha");
    assert_eq!(json["beta_val"], "beta");
}

#[tokio::test]
async fn import_glob_value_is_available_to_class_output() {
    let temp = TestTempDir::new("pklr_test_import_glob_class_output");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("builtins")).unwrap();
    std::fs::write(
        dir.join("builtins/prettier.pkl"),
        r#"
prettier { check = "prettier --check" }
prettier_stdin { check = "prettier --stdin-filepath" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "builtins/*.pkl" as Raw

class Factory {
    stdin: Boolean = false
    fixed step = if (stdin)
        Raw["builtins/prettier.pkl"].prettier_stdin
    else
        Raw["builtins/prettier.pkl"].prettier
}

factory = new Factory {}
amended = (factory) { stdin = true }
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["factory"]["step"]["check"], "prettier --check");
    assert_eq!(val["amended"]["step"]["check"], "prettier --stdin-filepath");
}

#[tokio::test]
async fn import_glob_double_star_crosses_directories() {
    let temp = TestTempDir::new("pklr_test_import_glob_double_star");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("config/foo.pkl"), r#"value = "foo""#).unwrap();
    std::fs::write(
        dir.join("hk.pkl"),
        r#"
import* "**.pkl" as Index
value = Index["config/foo.pkl"].value
has_self = Index.containsKey("hk.pkl")
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("hk.pkl")).await.unwrap();
    assert_eq!(val["value"], "foo");
    assert_eq!(val["has_self"], false);
}

#[tokio::test]
async fn import_glob_star_matches_one_directory_segment() {
    let temp = TestTempDir::new("pklr_test_import_glob_star_directory_segment");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("nested/config")).unwrap();
    std::fs::write(dir.join("config/foo.pkl"), r#"value = "foo""#).unwrap();
    std::fs::write(dir.join("nested/config/bar.pkl"), r#"value = "bar""#).unwrap();
    std::fs::write(
        dir.join("hk.pkl"),
        r#"
import* "*/*.pkl" as Index
value = Index["config/foo.pkl"].value
has_nested = Index.containsKey("nested/config/bar.pkl")
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("hk.pkl")).await.unwrap();
    assert_eq!(val["value"], "foo");
    assert_eq!(val["has_nested"], false);
}

#[tokio::test]
async fn import_glob_double_star_slash_matches_root_and_nested_files() {
    let temp = TestTempDir::new("pklr_test_import_glob_double_star_slash");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("foo.pkl"), r#"value = "root""#).unwrap();
    std::fs::write(dir.join("nested/foo.pkl"), r#"value = "nested""#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "**/foo.pkl" as Index
root_value = Index["foo.pkl"].value
nested_value = Index["nested/foo.pkl"].value
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["root_value"], "root");
    assert_eq!(val["nested_value"], "nested");
}

#[cfg(unix)]
#[tokio::test]
async fn import_glob_matches_symlinked_files() {
    let temp = TestTempDir::new("pklr_test_import_glob_symlinked_files");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("real")).unwrap();
    std::fs::write(dir.join("real/foo.pkl"), r#"value = "foo""#).unwrap();
    std::os::unix::fs::symlink(dir.join("real/foo.pkl"), dir.join("linked.pkl")).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "*.pkl" as Index
value = Index["linked.pkl"].value
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["value"], "foo");
}

#[cfg(unix)]
#[tokio::test]
async fn import_glob_skips_broken_symlinks() {
    let temp = TestTempDir::new("pklr_test_import_glob_broken_symlinks");
    let dir = temp.path();
    std::fs::write(dir.join("good.pkl"), r#"value = "good""#).unwrap();
    std::os::unix::fs::symlink(dir.join("missing.pkl"), dir.join("broken.pkl")).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "*.pkl" as Index
value = Index["good.pkl"].value
has_broken = Index.containsKey("broken.pkl")
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["value"], "good");
    assert_eq!(val["has_broken"], false);
}

#[tokio::test]
async fn unused_import_is_not_evaluated() {
    let temp = TestTempDir::new("pklr_test_unused_import");
    let dir = temp.path();
    std::fs::write(
        dir.join("broken.pkl"),
        r#"
value = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "broken.pkl"
result = "ok"
"#,
    )
    .unwrap();

    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["result"], "ok");
}

#[test]
fn missing_unused_import_is_not_evaluated() {
    // Unused imports are intentionally lazy, so missing paths only fail once
    // the imported binding is referenced.
    let json = eval(
        r#"
import "does-not-exist.pkl"
result = "ok"
"#,
    );
    assert_eq!(json["result"], "ok");
}

#[test]
fn shadowed_unused_import_is_not_evaluated() {
    let json = eval(
        r#"
import "does-not-exist.pkl" as Foo
class Foo {}
result = new Foo {}
"#,
    );
    assert!(json["result"].is_object());
}

#[test]
fn nested_shadowed_unused_import_is_not_evaluated() {
    let json = eval(
        r#"
import "does-not-exist.pkl" as Foo
class Outer {
    class Foo {}
    x = new Foo {}
}
result = new Outer {}
"#,
    );
    assert!(json["result"]["x"].is_object());
}

#[test]
fn unused_import_glob_without_alias_is_still_invalid() {
    let err = eval_fails(r#"import* "items/*.pkl""#);
    assert!(err.contains("import* requires an alias"), "{err}");
}

#[tokio::test]
async fn import_used_by_inherited_class_default_is_loaded() {
    let temp = TestTempDir::new("pklr_test_inherited_import_ref");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
class Project {
    name = meta.name
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    let src = r#"
amends "Base.pkl"
import "meta.pkl"
result = new Project {}
"#;

    let mut ev = Evaluator::new_async();
    let val = ev.eval_source(src, &dir.join("child.pkl")).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["result"]["name"], "hk");
}

#[tokio::test]
async fn imported_amends_base_uses_inherited_scope() {
    let temp = TestTempDir::new("pklr_test_imported_amends_base_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
name = meta.name
"#,
    )
    .unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    std::fs::write(
        dir.join("child.pkl"),
        r#"
amends "Base.pkl"
import "meta.pkl"
import "Base.pkl" as Base
baseName = Base.name
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("child.pkl"))
        .await
        .unwrap();
    assert_eq!(val["name"], "hk");
    assert_eq!(val["baseName"], "hk");
}

#[tokio::test]
async fn scoped_inherited_base_does_not_pollute_import_cache() {
    let temp = TestTempDir::new("pklr_test_scoped_base_cache");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
name = meta.name
"#,
    )
    .unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    std::fs::write(
        dir.join("child.pkl"),
        r#"
amends "Base.pkl"
import "meta.pkl"
result = name
"#,
    )
    .unwrap();

    let mut ev = Evaluator::new_async();
    let child_val = ev.eval_file_pub(&dir.join("child.pkl")).await.unwrap();
    assert_eq!(child_val.to_json()["result"], "hk");

    let err = ev.eval_file_pub(&dir.join("Base.pkl")).await.unwrap_err();
    assert!(
        err.to_string().contains("undefined variable: meta"),
        "{err}"
    );
}

#[tokio::test]
async fn imported_amends_and_extends_bases_keep_separate_values() {
    let temp = TestTempDir::new("pklr_test_imported_dual_inherited_bases");
    let dir = temp.path();
    std::fs::write(dir.join("AmendsBase.pkl"), r#"amendsName = meta.name"#).unwrap();
    std::fs::write(dir.join("ExtendsBase.pkl"), r#"extendsName = meta.name"#).unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    std::fs::write(
        dir.join("child.pkl"),
        r#"
amends "AmendsBase.pkl"
extends "ExtendsBase.pkl"
import "meta.pkl"
import "AmendsBase.pkl" as AmendsBase
import "ExtendsBase.pkl" as ExtendsBase
amended = AmendsBase.amendsName
extended = ExtendsBase.extendsName
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("child.pkl"))
        .await
        .unwrap();
    assert_eq!(val["extendsName"], "hk");
    assert_eq!(val["amended"], "hk");
    assert_eq!(val["extended"], "hk");
}

#[tokio::test]
async fn import_used_only_by_annotation_does_not_create_builtin_cycle() {
    let temp = TestTempDir::new("pklr_test_annotation_import_cycle");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("builtins")).unwrap();
    std::fs::write(
        dir.join("Builtins.pkl"),
        r#"
import* "builtins/*.pkl" as RawBuiltins

class meta extends Annotation {
    description: String?
}

class PrettierFactory {
    stdin: Boolean = false
    fixed step = if (stdin)
        RawBuiltins["builtins/prettier.pkl"].prettier_stdin
    else
        RawBuiltins["builtins/prettier.pkl"].prettier
}
prettier = new PrettierFactory {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("builtins").join("prettier.pkl"),
        r#"
import "../Builtins.pkl"

@Builtins.meta { description = "formatter" }
prettier = "ok"
prettier_stdin = "stdin"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Builtins.pkl"
result = Builtins.prettier.step
amended = ((Builtins.prettier) { stdin = true }).step
"#,
    )
    .unwrap();

    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["result"], "ok");
    assert_eq!(val["amended"], "stdin");
}

#[tokio::test]
async fn unused_import_glob_field_does_not_evaluate() {
    let temp = TestTempDir::new("pklr_test_unused_import_glob_field");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("builtins")).unwrap();
    std::fs::write(
        dir.join("Builtins.pkl"),
        r#"
import* "builtins/*.pkl" as Builtins
prettier = Builtins["builtins/prettier.pkl"].prettier
staticcheck = Builtins["builtins/staticcheck.pkl"].staticcheck
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("builtins").join("prettier.pkl"),
        r#"prettier = "ok""#,
    )
    .unwrap();
    std::fs::write(
        dir.join("builtins").join("staticcheck.pkl"),
        r#"
static_check = "misspelled"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Builtins.pkl"
result = Builtins.prettier
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "ok");
}

#[tokio::test]
async fn partial_import_expands_this_and_module_dependencies() {
    let temp = TestTempDir::new("pklr_test_partial_import_this_deps");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
x = 41
y = this.x + 1
z = module.x + 2
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
y = Dep.y
z = Dep.z
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["y"], 42);
    assert_eq!(val["z"], 43);
}

#[tokio::test]
async fn partial_import_includes_type_annotation_fields() {
    let temp = TestTempDir::new("pklr_test_partial_import_type_ann");
    let dir = temp.path();
    std::fs::write(
        dir.join("types.pkl"),
        r#"
Step = new Dynamic {
    enabled = true
}
Other = "other"
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "types.pkl" as Types
other = Types.Other
steps: Mapping<String, Types.Step> = new Mapping {}
steps {
    ["a"] {
        name = "alpha"
    }
}
enabled = steps["a"].enabled
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["other"], "other");
    assert_eq!(val["enabled"], true);
}

#[tokio::test]
async fn partial_import_includes_generic_param_fields() {
    let temp = TestTempDir::new("pklr_test_partial_import_generic_param");
    let dir = temp.path();
    std::fs::write(
        dir.join("types.pkl"),
        r#"
Step = new Dynamic {
    enabled = true
}
Other = "other"
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "types.pkl" as Types
other = Types.Other
steps = new Mapping<String, Types.Step> {
    ["a"] {
        name = "alpha"
    }
}
enabled = steps["a"].enabled
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["other"], "other");
    assert_eq!(val["enabled"], true);
}

#[tokio::test]
async fn partial_import_ignores_imports_used_only_by_skipped_properties() {
    let temp = TestTempDir::new("pklr_test_partial_import_skipped_import");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
import "broken.pkl" as Broken
wanted = "ok"
unused = Broken.value
"#,
    )
    .unwrap();
    std::fs::write(dir.join("broken.pkl"), r#"value = missing.field"#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.wanted
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "ok");
}

#[tokio::test]
async fn partial_import_treats_object_method_receiver_as_whole_import() {
    let temp = TestTempDir::new("pklr_test_partial_import_object_method");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
first = "one"
second = "two"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.toMap().toMapping()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"]["first"], "one");
    assert_eq!(val["result"]["second"], "two");
}

#[tokio::test]
async fn partial_import_treats_object_map_values_receiver_as_whole_import() {
    let temp = TestTempDir::new("pklr_test_partial_import_map_values");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
first = 1
second = 2
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.mapValues((k, v) -> v + 1).toMapping()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"]["first"], 2);
    assert_eq!(val["result"]["second"], 3);
}

#[tokio::test]
async fn partial_import_keeps_user_defined_method_names_field_scoped() {
    let temp = TestTempDir::new("pklr_test_partial_import_user_method");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
map = (n) -> n + 1
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.map(41)
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], 42);
}

#[tokio::test]
async fn partial_import_includes_sibling_function_called_by_requested_function() {
    let temp = TestTempDir::new("pklr_test_partial_import_sibling_function");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
function helper(): String = "ok"
function picked(): String = helper()
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.picked()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "ok");
}

#[test]
fn object_entries_can_be_separated_by_semicolons() {
    let json = eval(
        r#"
x {
  ["FOO"] = "foo"; ["BAR"] = "bar"
}
"#,
    );
    assert_eq!(json["x"]["FOO"], "foo");
    assert_eq!(json["x"]["BAR"], "bar");
}

#[test]
fn object_to_mapping_returns_mapping_like_object() {
    let json = eval(
        r#"
local Builtins = new Mapping {
  ["one"] = "1"
}
x = Builtins.toMap().toMapping()
"#,
    );
    assert_eq!(json["x"]["one"], "1");
}

#[test]
fn top_level_bare_elements_are_invalid() {
    let err = eval_fails("BROKEN SYNTAX");
    assert!(err.contains("Invalid property definition"), "{err}");
}

#[tokio::test]
async fn imported_typed_mapping_does_not_leak_schema_classes() {
    let temp = TestTempDir::new("pklr_test_imported_typed_mapping");
    let dir = temp.path();
    std::fs::write(
        dir.join("Config.pkl"),
        r#"
class Script {
  linux: String?
}

class Step {
  check: (String | Script)?
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("other.pkl"),
        r#"
import "./Config.pkl"
STEPS = new Mapping<String, Config.Step> {
  ["original"] { check = "echo original" }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "./Config.pkl"
import "./other.pkl"
steps = other.STEPS
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["steps"]["original"]["check"], "echo original");
    assert!(val["steps"]["original"].get("Script").is_none(), "{val}");
}

#[test]
fn typed_mapping_amendment_preserves_existing_keyed_entries() {
    let json = eval(
        r#"
class Step {
  check: String?
  env: Mapping<String, String> = new Mapping<String, String> {}
}

class Hook {
  steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

local hooks = new Mapping<String, Hook> {
  ["check"] {
    steps {
      ["echo"] { check = "env" }
    }
  }
}

result = (hooks) {
  ["check"] {
    steps {
      ["echo"] {
        env {
          ["STEP_VAR"] = "step_value"
        }
      }
      ["new step"] {
        check = "echo hello"
      }
    }
  }
}
"#,
    );

    assert_eq!(json["result"]["check"]["steps"]["echo"]["check"], "env");
    assert_eq!(
        json["result"]["check"]["steps"]["echo"]["env"]["STEP_VAR"],
        "step_value"
    );
    assert_eq!(
        json["result"]["check"]["steps"]["new step"]["check"],
        "echo hello"
    );
}

// ============================================================
// Class instantiation (future)
// ============================================================

#[test]
fn class_new_with_defaults() {
    let json = eval(
        r#"
class Person {
    name: String
    age: Int = 0
}
x = new Person {
    name = "Alice"
}
"#,
    );
    assert_eq!(json["x"]["name"], "Alice");
    assert_eq!(json["x"]["age"], 0);
}

// ============================================================
// Object amendment (future)
// ============================================================

#[test]
fn object_amendment() {
    let json = eval(
        r#"
local base = new Mapping {
    ["check"] = "echo hello"
    ["fix"] = "echo fix"
}
x = (base) {
    ["check"] = "echo override"
}
"#,
    );
    assert_eq!(json["x"]["check"], "echo override");
    assert_eq!(json["x"]["fix"], "echo fix");
}

// ============================================================
// Throw and trace
// ============================================================

#[test]
fn throw_produces_error() {
    let msg = eval_fails(r#"x = throw("boom")"#);
    assert!(msg.contains("boom"));
}

// ============================================================
// Null-safe access (future)
// ============================================================

#[test]
fn null_safe_access() {
    let json = eval(
        r#"
local x = null
result = x?.name ?? "default"
"#,
    );
    assert_eq!(json["result"], "default");
}

// ============================================================
// Module header
// ============================================================

#[test]
fn module_header_skipped() {
    let json = eval(
        r#"
module my.Config
x = 42
"#,
    );
    assert_eq!(json["x"], 42);
}

// ============================================================
// Higher-order methods (map, filter, fold)
// ============================================================

#[test]
fn list_map() {
    let json = eval(
        r#"
local items = List(1, 2, 3)
x = items.map((n) -> n * 2)
"#,
    );
    assert_eq!(json["x"], serde_json::json!([2, 4, 6]));
}

#[test]
fn list_filter() {
    let json = eval(
        r#"
local items = List(1, 2, 3, 4, 5)
x = items.filter((n) -> n > 2)
"#,
    );
    assert_eq!(json["x"], serde_json::json!([3, 4, 5]));
}

#[test]
fn list_fold() {
    let json = eval(
        r#"
local items = List(1, 2, 3, 4)
x = items.fold(0, (acc, n) -> acc + n)
"#,
    );
    assert_eq!(json["x"], 10);
}

#[test]
fn list_any_every() {
    let json = eval(
        r#"
local items = List(1, 2, 3)
has_even = items.any((n) -> n % 2 == 0)
all_positive = items.every((n) -> n > 0)
"#,
    );
    assert_eq!(json["has_even"], true);
    assert_eq!(json["all_positive"], true);
}

// ============================================================
// Higher-order methods on Map / Mapping
// ============================================================

#[test]
fn map_filter() {
    let json = eval(
        r#"
local items = new Mapping<String, Int> {
    ["a"] = 1
    ["b"] = 2
    ["c"] = 3
}
x = items.toMap().filter((k, v) -> v > 1).toMapping()
"#,
    );
    assert_eq!(json["x"]["b"], 2);
    assert_eq!(json["x"]["c"], 3);
    assert!(json["x"].get("a").is_none());
}

#[test]
fn map_filter_then_map_values_chain() {
    // toMap().filter().mapValues().toMapping() is a common transformation chain.
    let json = eval(
        r#"
local items = new Mapping<String, Int> {
    ["a"] = 1
    ["b"] = 2
    ["c"] = 3
}
x =
    items
        .toMap()
        .filter((k, v) -> v > 1)
        .mapValues((k, v) -> v * 10)
        .toMapping()
"#,
    );
    assert_eq!(json["x"]["b"], 20);
    assert_eq!(json["x"]["c"], 30);
    assert!(json["x"].get("a").is_none());
}

#[test]
fn object_amendment_with_named_property() {
    let json = eval(
        r#"
local base = new Mapping {
    ["a"] {
        value = 1
    }
}
x = (base) {
    ["a"] {
        value = 2
    }
    ["b"] {
        value = 3
    }
}
"#,
    );
    assert_eq!(json["x"]["a"]["value"], 2);
    assert_eq!(json["x"]["b"]["value"], 3);
}

// ============================================================
// Late binding
// ============================================================

#[test]
fn late_binding_basic() {
    // Overriding x should cause y to re-evaluate
    let json = eval(
        r#"
local base = new {
    x = 1
    y = x + 1
}
result = (base) {
    x = 10
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 11);
}

#[test]
fn late_binding_chained() {
    // Chained dependency: x -> y -> z
    let json = eval(
        r#"
local base = new {
    x = 1
    y = x + 1
    z = y + 1
}
result = (base) {
    x = 10
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 11);
    assert_eq!(json["result"]["z"], 12);
}

#[test]
fn late_binding_unrelated_preserved() {
    // Properties not depending on overridden ones stay the same
    let json = eval(
        r#"
local base = new {
    x = 1
    y = x + 1
    name = "hello"
}
result = (base) {
    x = 10
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 11);
    assert_eq!(json["result"]["name"], "hello");
}

#[test]
fn late_binding_class_new() {
    // Late binding with class defaults
    let json = eval(
        r#"
class Config {
    port: Int = 8080
    url: String = "http://localhost:\(port)"
}
result = new Config {
    port = 3000
}
"#,
    );
    assert_eq!(json["result"]["port"], 3000);
    assert_eq!(json["result"]["url"], "http://localhost:3000");
}

#[test]
fn late_binding_string_interpolation() {
    // Late binding with string interpolation
    let json = eval(
        r#"
local base = new {
    name = "world"
    greeting = "Hello, \(name)!"
}
result = (base) {
    name = "Pkl"
}
"#,
    );
    assert_eq!(json["result"]["name"], "Pkl");
    assert_eq!(json["result"]["greeting"], "Hello, Pkl!");
}

// ============================================================
// this / outer keywords
// ============================================================

#[test]
fn outer_keyword() {
    let json = eval(
        r#"
local prefix = "test"
data {
    local before = "\(prefix)-data"
    inner {
        name = outer.before
    }
}
"#,
    );
    assert_eq!(json["data"]["inner"]["name"], "test-data");
}

#[test]
fn this_keyword_basic() {
    // `this` refers to the current object
    let json = eval(
        r#"
data {
    x = 1
    y = this.x + 1
}
"#,
    );
    assert_eq!(json["data"]["x"], 1);
    assert_eq!(json["data"]["y"], 2);
}

#[test]
fn this_keyword_nested() {
    // `this` in a nested object refers to the inner object, not the outer
    let json = eval(
        r#"
data {
    x = 10
    inner {
        x = 20
        y = this.x + 1
    }
}
"#,
    );
    assert_eq!(json["data"]["x"], 10);
    assert_eq!(json["data"]["inner"]["x"], 20);
    assert_eq!(json["data"]["inner"]["y"], 21);
}

#[test]
fn this_keyword_module_level() {
    // `this` at module level refers to the module object
    let json = eval(
        r#"
x = 42
y = this.x + 1
"#,
    );
    assert_eq!(json["x"], 42);
    assert_eq!(json["y"], 43);
}

#[test]
fn this_keyword_in_string_interpolation() {
    let json = eval(
        r#"
data {
    name = "world"
    greeting = "Hello, \(this.name)!"
}
"#,
    );
    assert_eq!(json["data"]["greeting"], "Hello, world!");
}

#[test]
fn this_keyword_with_hidden_property() {
    let json = eval(
        r#"
data {
    hidden base = "https://example.com"
    url = this.base + "/api"
}
"#,
    );
    // base must not appear in output (hidden)
    assert!(json["data"].get("base").is_none());
    // but url must resolve via this.base
    assert_eq!(json["data"]["url"], "https://example.com/api");
}

#[test]
fn this_keyword_hidden_at_module_level() {
    let json = eval(
        r#"
hidden secret = "abc123"
derived = this.secret + "-derived"
"#,
    );
    assert!(json.get("secret").is_none());
    assert_eq!(json["derived"], "abc123-derived");
}

#[test]
fn module_keyword_at_top_level() {
    // `module` refers to the top-level module object
    let json = eval(
        r#"
x = 1
y = module.x + 10
"#,
    );
    assert_eq!(json["x"], 1);
    assert_eq!(json["y"], 11);
}

// ============================================================
// Class definitions
// ============================================================

#[test]
fn class_multiple_defaults() {
    let json = eval(
        r#"
class Config {
    debug: Boolean = false
    port: Int = 8080
    host: String = "localhost"
}
x = new Config {
    debug = true
}
"#,
    );
    assert_eq!(json["x"]["debug"], true);
    assert_eq!(json["x"]["port"], 8080);
    assert_eq!(json["x"]["host"], "localhost");
}

#[test]
fn class_defaults_reference_locals() {
    let json = eval(
        r#"
local DEFAULT_PORT = 8080

class Config {
    port: Int = DEFAULT_PORT
}
x = new Config {}
"#,
    );
    assert_eq!(json["x"]["port"], 8080);
}

#[test]
fn class_local_this_alias_tracks_amended_inputs() {
    let json = eval(
        r#"
class Factory {
    local factory = this
    staged: Boolean = false
    fixed step = (new { staged = false }) {
        staged = factory.staged
    }
}
x = new Factory {
    staged = true
}
"#,
    );
    assert_eq!(json["x"]["step"]["staged"], true);
    assert!(json["x"].get("factory").is_none());
}

#[test]
fn class_local_this_alias_is_complete_for_deferred_methods() {
    let json = eval(
        r#"
class Factory {
    local factory = this
    local secondAlias = factory
    first: String = "first"
    last: String = "last"
    function lastValue(): String = secondAlias.last
}

local factory = new Factory {}
result = factory.lastValue()
"#,
    );
    assert_eq!(json["result"], "last");
}

#[test]
fn class_with_type_params() {
    let json = eval(
        r#"
class Container<T> {
    value: T = "default"
}
x = new Container {
    value = "custom"
}
"#,
    );
    assert_eq!(json["x"]["value"], "custom");
}

#[test]
fn new_with_dotted_type_name() {
    // Dotted type names in new: resolves Config then .Step
    let json = eval(
        r#"
local Config = new {
    Step = new {
        check = "default"
        glob = "*.rs"
    }
}
x = new Config.Step {
    check = "custom"
}
"#,
    );
    assert_eq!(json["x"]["check"], "custom");
    assert_eq!(json["x"]["glob"], "*.rs");
}

// ============================================================
// Class inheritance (extends) and super keyword
// ============================================================

#[test]
fn class_extends_basic() {
    let json = eval(
        r#"
class Animal {
    name: String = "unknown"
    legs: Int = 4
}
class Dog extends Animal {
    breed: String = "mixed"
}
x = new Dog {
    name = "Rex"
}
"#,
    );
    assert_eq!(json["x"]["name"], "Rex");
    assert_eq!(json["x"]["legs"], 4);
    assert_eq!(json["x"]["breed"], "mixed");
}

#[test]
fn class_extends_override_parent_default() {
    let json = eval(
        r#"
class Base {
    port: Int = 8080
    host: String = "localhost"
}
class Production extends Base {
    port: Int = 443
    tls: Boolean = true
}
x = new Production {}
"#,
    );
    assert_eq!(json["x"]["port"], 443);
    assert_eq!(json["x"]["host"], "localhost");
    assert_eq!(json["x"]["tls"], true);
}

#[test]
fn class_extends_instance_override() {
    // Instance overrides both parent and child defaults
    let json = eval(
        r#"
class Base {
    x: Int = 1
    y: Int = 2
}
class Child extends Base {
    z: Int = 3
}
result = new Child {
    x = 10
    z = 30
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 2);
    assert_eq!(json["result"]["z"], 30);
}

#[test]
fn super_keyword_basic() {
    let json = eval(
        r#"
class Base {
    greeting: String = "hello"
}
class Child extends Base {
    greeting: String = super.greeting + " world"
}
x = new Child {}
"#,
    );
    assert_eq!(json["x"]["greeting"], "hello world");
}

#[test]
fn super_keyword_field_access() {
    let json = eval(
        r#"
class Config {
    port: Int = 8080
    url: String = "http://localhost"
}
class AppConfig extends Config {
    port: Int = 3000
    url: String = super.url + ":\(port)"
}
x = new AppConfig {}
"#,
    );
    assert_eq!(json["x"]["port"], 3000);
    assert_eq!(json["x"]["url"], "http://localhost:3000");
}

#[test]
fn class_extends_chain() {
    // Three-level inheritance chain
    let json = eval(
        r#"
class A {
    x: Int = 1
}
class B extends A {
    y: Int = 2
}
class C extends B {
    z: Int = 3
}
result = new C {}
"#,
    );
    assert_eq!(json["result"]["x"], 1);
    assert_eq!(json["result"]["y"], 2);
    assert_eq!(json["result"]["z"], 3);
}

// ============================================================
// Durations
// ============================================================

#[test]
fn duration_minutes() {
    let json = eval(r#"x = 5.min"#);
    assert_eq!(json["x"]["value"], 5);
    assert_eq!(json["x"]["unit"], "min");
}

#[test]
fn duration_seconds() {
    let json = eval(r#"x = 3.s"#);
    assert_eq!(json["x"]["value"], 3);
    assert_eq!(json["x"]["unit"], "s");
}

#[test]
fn duration_hours() {
    let json = eval(r#"x = 2.h"#);
    assert_eq!(json["x"]["value"], 2);
    assert_eq!(json["x"]["unit"], "h");
}

#[test]
fn duration_days() {
    let json = eval(r#"x = 7.d"#);
    assert_eq!(json["x"]["value"], 7);
    assert_eq!(json["x"]["unit"], "d");
}

#[test]
fn duration_milliseconds() {
    let json = eval(r#"x = 100.ms"#);
    assert_eq!(json["x"]["value"], 100);
    assert_eq!(json["x"]["unit"], "ms");
}

#[test]
fn duration_nanoseconds() {
    let json = eval(r#"x = 50.ns"#);
    assert_eq!(json["x"]["value"], 50);
    assert_eq!(json["x"]["unit"], "ns");
}

#[test]
fn duration_microseconds() {
    let json = eval(r#"x = 10.us"#);
    assert_eq!(json["x"]["value"], 10);
    assert_eq!(json["x"]["unit"], "us");
}

#[test]
fn duration_float_value() {
    let json = eval(r#"x = 5.5.min"#);
    assert_eq!(json["x"]["value"], 5.5);
    assert_eq!(json["x"]["unit"], "min");
}

// ============================================================
// Data sizes
// ============================================================

#[test]
fn datasize_bytes() {
    let json = eval(r#"x = 512.b"#);
    assert_eq!(json["x"]["value"], 512);
    assert_eq!(json["x"]["unit"], "b");
}

#[test]
fn datasize_kilobytes() {
    let json = eval(r#"x = 10.kb"#);
    assert_eq!(json["x"]["value"], 10);
    assert_eq!(json["x"]["unit"], "kb");
}

#[test]
fn datasize_megabytes() {
    let json = eval(r#"x = 256.mb"#);
    assert_eq!(json["x"]["value"], 256);
    assert_eq!(json["x"]["unit"], "mb");
}

#[test]
fn datasize_gigabytes() {
    let json = eval(r#"x = 4.gb"#);
    assert_eq!(json["x"]["value"], 4);
    assert_eq!(json["x"]["unit"], "gb");
}

#[test]
fn datasize_terabytes() {
    let json = eval(r#"x = 1.tb"#);
    assert_eq!(json["x"]["value"], 1);
    assert_eq!(json["x"]["unit"], "tb");
}

#[test]
fn datasize_petabytes() {
    let json = eval(r#"x = 2.pb"#);
    assert_eq!(json["x"]["value"], 2);
    assert_eq!(json["x"]["unit"], "pb");
}

#[test]
fn datasize_gibibytes() {
    let json = eval(r#"x = 8.gib"#);
    assert_eq!(json["x"]["value"], 8);
    assert_eq!(json["x"]["unit"], "gib");
}

#[test]
fn datasize_mebibytes() {
    let json = eval(r#"x = 16.mib"#);
    assert_eq!(json["x"]["value"], 16);
    assert_eq!(json["x"]["unit"], "mib");
}

#[test]
fn datasize_tebibytes() {
    let json = eval(r#"x = 1.tib"#);
    assert_eq!(json["x"]["value"], 1);
    assert_eq!(json["x"]["unit"], "tib");
}

#[test]
fn datasize_pebibytes() {
    let json = eval(r#"x = 1.pib"#);
    assert_eq!(json["x"]["value"], 1);
    assert_eq!(json["x"]["unit"], "pib");
}

#[test]
fn datasize_kibibytes() {
    let json = eval(r#"x = 64.kib"#);
    assert_eq!(json["x"]["value"], 64);
    assert_eq!(json["x"]["unit"], "kib");
}

#[test]
fn unicode_escape_without_braces_errors() {
    let msg = eval_fails(r#"x = "\u0041""#);
    assert!(msg.contains("unicode escape"));
}

#[test]
fn unicode_escape_empty_braces_errors() {
    let msg = eval_fails(r#"x = "\u{}""#);
    assert!(msg.contains("hex digit"));
}

// ============================================================
// Property modifiers
// ============================================================

#[test]
fn hidden_not_in_output() {
    let json = eval(
        r#"
hidden secret = "s3cr3t"
visible = "hello"
"#,
    );
    assert!(json.get("secret").is_none());
    assert_eq!(json["visible"], "hello");
}

#[test]
fn hidden_accessible_by_other_properties() {
    let json = eval(
        r#"
hidden base_url = "https://example.com"
api_url = base_url + "/api"
"#,
    );
    assert!(json.get("base_url").is_none());
    assert_eq!(json["api_url"], "https://example.com/api");
}

#[test]
fn const_property() {
    // const properties work normally when not overridden
    let json = eval(
        r#"
const name = "fixed"
x = name
"#,
    );
    assert_eq!(json["x"], "fixed");
}

#[test]
fn abstract_property_with_value() {
    // abstract property with a value is fine
    let json = eval(
        r#"
class Base {
    abstract name: String = "default"
}
x = new Base {}
"#,
    );
    assert_eq!(json["x"]["name"], "default");
}

#[test]
fn fixed_property() {
    let json = eval(
        r#"
fixed version = 1
x = version
"#,
    );
    assert_eq!(json["x"], 1);
}

#[test]
fn hidden_in_nested_object() {
    let json = eval(
        r#"
config {
    hidden internal = "private"
    public = "visible"
}
"#,
    );
    assert!(json["config"].get("internal").is_none());
    assert_eq!(json["config"]["public"], "visible");
}

#[test]
fn fixed_cannot_override() {
    let json = eval(
        r#"
fixed version = 1
x = version
"#,
    );
    // fixed works fine when not overridden
    assert_eq!(json["x"], 1);
}

#[test]
fn external_requires_value() {
    let msg = eval_fails(
        r#"
external name: String
x = name
"#,
    );
    assert!(msg.contains("external"));
    assert!(msg.contains("must be assigned"));
}

#[tokio::test]
async fn const_cannot_override_in_amends() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    // Create a base file with const property
    let base_src = r#"const version = 1"#;
    std::fs::write(base.join("const_base.pkl"), base_src).unwrap();
    let src = r#"
amends "const_base.pkl"
const version = 2
"#;
    let path = base.join("test_const_override.pkl");
    let result = ev.eval_source(src, &path).await;
    std::fs::remove_file(base.join("const_base.pkl")).ok();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("const"));
}

// ============================================================
// Default elements/values
// ============================================================

#[test]
fn default_value_in_object() {
    let json = eval(
        r#"
config {
    default {
        enabled = true
        port = 8080
    }
    ["api"] {
        port = 9090
    }
    ["web"] {
        port = 3000
    }
}
"#,
    );
    // api: port overridden, enabled inherited from default
    assert_eq!(json["config"]["api"]["port"], 9090);
    assert_eq!(json["config"]["api"]["enabled"], true);
    // web: port overridden, enabled inherited from default
    assert_eq!(json["config"]["web"]["port"], 3000);
    assert_eq!(json["config"]["web"]["enabled"], true);
}

#[test]
fn default_not_in_output() {
    let json = eval(
        r#"
services {
    default {
        replicas = 1
    }
    ["app"] {
        replicas = 3
    }
}
"#,
    );
    // default itself should not appear in output
    assert!(json["services"].get("default").is_none());
    assert_eq!(json["services"]["app"]["replicas"], 3);
}

#[test]
fn default_in_mapping() {
    let json = eval(
        r#"
x = new Mapping {
    default {
        active = true
    }
    ["a"] {
        name = "alpha"
    }
    ["b"] {
        name = "beta"
        active = false
    }
}
"#,
    );
    assert_eq!(json["x"]["a"]["name"], "alpha");
    assert_eq!(json["x"]["a"]["active"], true);
    assert_eq!(json["x"]["b"]["name"], "beta");
    assert_eq!(json["x"]["b"]["active"], false);
}

#[test]
fn no_default_no_merge() {
    // Without a default, dynamic entries should not be merged
    let json = eval(
        r#"
x {
    ["a"] {
        name = "alpha"
    }
}
"#,
    );
    assert_eq!(json["x"]["a"]["name"], "alpha");
    assert!(json["x"]["a"].get("enabled").is_none());
}

#[test]
fn default_nested_merge() {
    let json = eval(
        r#"
services {
    default {
        config {
            timeout = 30
            retries = 3
        }
    }
    ["api"] {
        config {
            timeout = 60
        }
    }
}
"#,
    );
    // timeout overridden, retries inherited from default
    assert_eq!(json["services"]["api"]["config"]["timeout"], 60);
    assert_eq!(json["services"]["api"]["config"]["retries"], 3);
}

// ============================================================
// Type aliases
// ============================================================

#[test]
fn typealias_to_class() {
    // typealias acts as an alternative constructor for a class
    let json = eval(
        r#"
class Server {
    host: String = "localhost"
    port: Int = 8080
}
typealias Srv = Server
x = new Srv {
    port = 3000
}
"#,
    );
    assert_eq!(json["x"]["host"], "localhost");
    assert_eq!(json["x"]["port"], 3000);
}

#[test]
fn typealias_chain() {
    // Alias of an alias
    let json = eval(
        r#"
class Base {
    value: Int = 1
}
typealias A = Base
typealias B = A
x = new B {}
"#,
    );
    assert_eq!(json["x"]["value"], 1);
}

#[test]
fn typealias_simple_type_ignored() {
    // typealias to a simple type (not a class) is a no-op, shouldn't error
    let json = eval(
        r#"
typealias Name = String
x = "hello"
"#,
    );
    assert_eq!(json["x"], "hello");
}

#[test]
fn typealias_with_constraint() {
    // typealias with type constraint -- constraint is skipped but should parse
    let json = eval(
        r#"
typealias Port = Int(isBetween(1, 65535))
x = 8080
"#,
    );
    assert_eq!(json["x"], 8080);
}

#[test]
fn type_defaults_cover_literals_unions_and_collections() {
    let json = eval(
        r#"
literal: "only"
selected: "first" | *"second"
items: Listing<String>
mapping: Mapping<String, Int>
"#,
    );
    assert_eq!(json["literal"], "only");
    assert_eq!(json["selected"], "second");
    assert_eq!(json["items"], serde_json::json!([]));
    assert!(json.get("mapping").is_none());
}

#[test]
fn selected_structured_union_defaults_retain_semantics() {
    let json = eval(
        r#"
nullable: String | *Null
listing: String | *Listing<String>
nullableResult = nullable == null
stringMatches = "ok" is Int | *String
"#,
    );
    assert_eq!(json["nullableResult"], true);
    assert_eq!(json["listing"], serde_json::json!([]));
    assert_eq!(json["stringMatches"], true);
    assert!(json.get("nullable").is_none());
}

#[test]
fn union_without_selected_default_fails() {
    let message = eval_fails(r#"value: "first" | "second""#);
    assert!(message.contains("no selected default"));
}

#[test]
fn selected_union_member_without_implicit_value_stays_undefined() {
    let json = eval(
        r#"
optional: *String | Int
provided: *String | Int = "value"
"#,
    );
    assert!(json.get("optional").is_none());
    assert_eq!(json["provided"], "value");
}

#[test]
fn multiple_type_constraints_are_conjunctive() {
    let json = eval(
        r#"
local small = 5
local large = 20
smallMatches = small is Int(this > 0, this < 10)
largeMatches = large is Int(this > 0, this < 10)
"#,
    );
    assert_eq!(json["smallMatches"], true);
    assert_eq!(json["largeMatches"], false);
}

#[test]
fn failing_constraint_in_list_rejects_cast() {
    let message = eval_fails("result = 20 as Int(this > 0, this < 10)");
    assert!(message.contains("cannot cast"));
}

#[test]
fn typed_lambda_parameters_evaluate() {
    let json = eval(
        r#"
local choose = (value: String) -> value
result = choose("ok")
"#,
    );
    assert_eq!(json["result"], "ok");
}

#[test]
fn nullable_class_default_can_be_amended() {
    let json = eval(
        r#"
class Options { enabled: Boolean = false }
base { options: Options? }
result = (base) { options { enabled = true } }
"#,
    );
    assert_eq!(json["result"]["options"]["enabled"], true);
}

#[test]
fn nullable_amendment_prefers_existing_non_null_value() {
    let json = eval(
        r#"
class Options { enabled: Boolean = false; label: String = "default" }
base { options: Options? = new Options { enabled = true } }
result = (base) { options { label = "changed" } }
"#,
    );
    assert_eq!(json["result"]["options"]["enabled"], true);
    assert_eq!(json["result"]["options"]["label"], "changed");
}

#[test]
fn listing_body_amendment_appends_elements() {
    let json = eval(
        r#"
base { items: Listing<String> }
result = (base) {
  items {
    local prefix = ""
    "first"
    for (item in List("second")) { prefix + item }
    when (true) { "third" } else { "wrong" }
  }
}
"#,
    );
    assert_eq!(
        json["result"]["items"],
        serde_json::json!(["first", "second", "third"])
    );
}

#[test]
fn listing_body_amendment_applies_index_updates() {
    let json = eval(
        r#"
base { items = List("old", "stay") }
result = (base) {
  items {
    [0] = "new"
    "appended"
  }
}
"#,
    );
    assert_eq!(
        json["result"]["items"],
        serde_json::json!(["new", "stay", "appended"])
    );
}

#[test]
fn listing_index_body_amends_existing_element() {
    let json = eval(
        r#"
base {
  items = List(new Dynamic {
    kept = 1
    changed = 1
  })
}
result = (base) {
  items {
    [0] {
      changed = 2
      added = 3
    }
  }
}
"#,
    );
    assert_eq!(
        json["result"]["items"][0],
        serde_json::json!({"kept": 1, "changed": 2, "added": 3})
    );
}

#[test]
fn listing_named_members_are_not_elements() {
    let json = eval(
        r#"
items = new Listing {
  default = "template"
  "value"
}
"#,
    );
    assert_eq!(json["items"], serde_json::json!(["value"]));
}

#[test]
fn listing_shaped_body_preserves_existing_object_kind() {
    let json = eval(
        r#"
base {
  value = new Dynamic {
    kept = 1
  }
}
result = (base) {
  value {
    "element"
    added = 2
  }
}
"#,
    );
    assert_eq!(
        json["result"]["value"],
        serde_json::json!({"kept": 1, "added": 2})
    );
}

#[test]
fn poisoned_local_shadows_parent_binding() {
    let message = eval_fails(
        r#"
name = "outer"
result {
  local name = throw("local failed")
  value = name
}
"#,
    );
    assert!(message.contains("local failed"), "{message}");
}

#[test]
fn poisoned_local_is_preserved_in_closure_capture() {
    let message = eval_fails(
        r#"
name = "outer"
result {
  local name = throw("local failed")
  local getName = () -> name
  value = getName()
}
"#,
    );
    assert!(message.contains("local failed"), "{message}");
}

#[test]
fn current_binding_shadows_outer_poison_in_closure_capture() {
    let json = eval(
        r#"
local name = throw("outer failed")
result {
  local name = 1
  local getName = () -> name
  value = getName()
}
"#,
    );
    assert_eq!(json["result"]["value"], 1);
}

#[test]
fn inherited_late_binding_recomputes_dependency_chains() {
    let temp = TestTempDir::new("pklr_inherited_chain");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\na = this.b\nm = module.b\nb = c\nd = b + 1\nc = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\nc = 2\ne = d + 1\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["a"], 2);
    assert_eq!(json["m"], 2);
    assert_eq!(json["b"], 2);
    assert_eq!(json["d"], 3);
    assert_eq!(json["e"], 4);
}

#[test]
fn child_locals_recompute_after_inherited_properties_are_overridden() {
    let temp = TestTempDir::new("pklr_child_local_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nsource = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nlocal derived = source + 1\nresult = derived\nsource = 2\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["source"], 2);
    assert_eq!(json["result"], 3);
    assert!(json.get("derived").is_none());
}

#[test]
fn inherited_late_binding_preserves_parent_locals() {
    let temp = TestTempDir::new("pklr_parent_local_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nlocal offset = 1\nsource = 1\nderived = source + offset\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\nsource = 2\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["source"], 2);
    assert_eq!(json["derived"], 3);
    assert!(json.get("offset").is_none());
}

#[test]
fn inherited_late_binding_reaches_grandparent_declarations() {
    let temp = TestTempDir::new("pklr_grandparent_late_binding");
    std::fs::write(
        temp.path().join("Grand.pkl"),
        "abstract module Grand\nsource = 1\nderived = source + 1\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Parent.pkl"),
        "abstract module Parent\nextends \"Grand.pkl\"\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Parent.pkl\"\nsource = 2\nresult = derived\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["source"], 2);
    assert_eq!(json["derived"], 3);
    assert_eq!(json["result"], 3);
}

#[test]
fn computed_sibling_access_recomputes_after_child_override() {
    let temp = TestTempDir::new("pklr_computed_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nsource = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nlocal keyName = \"source\"\nresult = this[keyName]\nsource = 2\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["result"], 2);
}

#[test]
fn aliased_module_snapshot_recomputes_after_child_override() {
    let temp = TestTempDir::new("pklr_aliased_snapshot_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nsource = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nlocal snapshot = this\nresult = snapshot.source\nsource = 2\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["result"], 2);
}

#[test]
fn amended_scope_only_defaults_stay_out_of_output() {
    let temp = TestTempDir::new("pklr_amended_scope_default");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "implicit: Mapping<String, String>\nvisible = implicit.length\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "amends \"Base.pkl\"\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert!(json.get("implicit").is_none());
    assert_eq!(json["visible"], 0);
}

#[test]
fn inherited_late_binding_propagates_errors() {
    let temp = TestTempDir::new("pklr_inherited_error");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nderived = 1 / denominator\ndenominator = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\ndenominator = 0\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap_err()
        .to_string();
    assert!(error.contains("division by zero"));
}

#[test]
fn constrained_nullable_mapping_preserves_value_defaults() {
    let json = eval(
        r#"
class Item { enabled: Boolean = false }
base { items: Mapping<String, Item>?(length >= 0) }
result = (base) { items { ["example"] { enabled = true } } }
"#,
    );
    assert_eq!(json["result"]["items"]["example"]["enabled"], true);
}

#[test]
fn used_unresolved_abstract_member_still_errors() {
    let temp = TestTempDir::new("pklr_abstract_member");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nabstract missing: String\ndependent = missing\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\nresult = dependent\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap_err()
        .to_string();
    assert!(error.contains("abstract property") || error.contains("undefined variable"));
}

#[test]
fn concrete_module_must_implement_inherited_abstract_property() {
    let temp = TestTempDir::new("pklr_required_abstract_member");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nabstract required: Listing<String>\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap_err()
        .to_string();
    assert!(error.contains("abstract property 'required'"));

    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nrequired = List(\"implemented\")\n",
    )
    .unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["required"], serde_json::json!(["implemented"]));
}

#[test]
fn typealias_union_parses() {
    // Union type alias should parse without error
    let json = eval(
        r#"
typealias StringOrInt = String|Int
x = 42
"#,
    );
    assert_eq!(json["x"], 42);
}

#[test]
fn typealias_nullable_class() {
    // typealias Foo = Bar? should still work as constructor
    let json = eval(
        r#"
class Config {
    debug: Boolean = false
}
typealias MaybeConfig = Config?
x = new MaybeConfig {
    debug = true
}
"#,
    );
    assert_eq!(json["x"]["debug"], true);
}

#[test]
fn typealias_generic_parses() {
    // Generic type alias should parse without error
    let json = eval(
        r#"
typealias StringMap = Mapping<String, String>
x = new Mapping {
    ["a"] = "b"
}
"#,
    );
    assert_eq!(json["x"]["a"], "b");
}

// ============================================================
// is / as type operators
// ============================================================

#[test]
fn is_operator_string() {
    let json = eval(
        r#"
local x = "hello"
a = x is String
b = x is Int
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn is_operator_int() {
    let json = eval(
        r#"
local x = 42
a = x is Int
b = x is Number
c = x is String
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
}

#[test]
fn is_operator_null() {
    let json = eval(
        r#"
local x = null
a = x is Null
b = x is String?
c = x is String
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
}

#[test]
fn is_operator_nullable() {
    let json = eval(
        r#"
local x = "hello"
a = x is String?
b = x is Int?
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn is_operator_union() {
    let json = eval(
        r#"
local x = 42
local y = "hello"
a = x is String|Int
b = y is String|Int
c = x is String|Boolean
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
}

#[test]
fn is_operator_object_and_list() {
    let json = eval(
        r#"
local obj = new { x = 1 }
local lst = List(1, 2, 3)
a = obj is Object
b = lst is List
c = obj is List
d = lst is Object
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
    assert_eq!(json["d"], false);
}

#[test]
fn is_operator_any() {
    let json = eval(
        r#"
a = 42 is Any
b = "hello" is Any
c = null is Any
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], true);
}

#[test]
fn as_operator_success() {
    let json = eval(
        r#"
local x = 42
result = x as Int
"#,
    );
    assert_eq!(json["result"], 42);
}

#[test]
fn as_operator_failure() {
    let msg = eval_fails(
        r#"
local x = "hello"
result = x as Int
"#,
    );
    assert!(msg.contains("cannot cast"));
}

#[test]
fn as_operator_nullable() {
    let json = eval(
        r#"
local x = null
result = x as String?
"#,
    );
    assert_eq!(json["result"], serde_json::Value::Null);
}

#[test]
fn is_in_conditional() {
    let json = eval(
        r#"
local x = 42
result = if (x is Int) "integer" else "other"
"#,
    );
    assert_eq!(json["result"], "integer");
}

// ============================================================
// Type constraints
// ============================================================

#[test]
fn constraint_is_check_pass() {
    let json = eval(
        r#"
local x = 42
result = x is Int(this >= 0)
"#,
    );
    assert_eq!(json["result"], true);
}

#[test]
fn constraint_is_check_fail() {
    let json = eval(
        r#"
local x = -1
result = x is Int(this >= 0)
"#,
    );
    assert_eq!(json["result"], false);
}

#[test]
fn constraint_is_wrong_base_type() {
    let json = eval(
        r#"
local x = "hello"
result = x is Int(this >= 0)
"#,
    );
    assert_eq!(json["result"], false);
}

#[test]
fn constraint_as_pass() {
    let json = eval(
        r#"
local x = 42
result = x as Int(this > 0)
"#,
    );
    assert_eq!(json["result"], 42);
}

#[test]
fn constraint_as_fail() {
    let msg = eval_fails(
        r#"
local x = -1
result = x as Int(this > 0)
"#,
    );
    assert!(msg.contains("cannot cast"));
}

#[test]
fn constraint_string_not_empty() {
    let json = eval(
        r#"
local a = "hello"
local b = ""
x = a is String(!isEmpty)
y = b is String(!isEmpty)
"#,
    );
    assert_eq!(json["x"], true);
    assert_eq!(json["y"], false);
}

#[test]
fn constraint_string_length() {
    let json = eval(
        r#"
local x = "hi"
a = x is String(length <= 5)
b = x is String(length > 10)
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn constraint_comparison() {
    let json = eval(
        r#"
local x = 8080
a = x is Int(this >= 1 && this <= 65535)
b = x is Int(this < 0)
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn typealias_with_constraint_enforced() {
    // typealias with constraint, checked via is
    let json = eval(
        r#"
typealias PositiveInt = Int(this > 0)
local x = 42
local y = -1
a = x is PositiveInt
b = y is PositiveInt
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn typealias_constraint_works_inside_amended_object() {
    // type aliases must be available inside amended/extended objects
    // (regression: flatten() used to drop type_aliases)
    let json = eval(
        r#"
typealias PositiveInt = Int(this > 0)
base {
    port = 8080
}
result = (base) {
    check = 42 is PositiveInt
}
"#,
    );
    assert_eq!(json["result"]["check"], true);
}

#[test]
fn typealias_constraint_inside_amended_object_rejects_invalid() {
    let json = eval(
        r#"
typealias PositiveInt = Int(this > 0)
local neg = 0 - 5
base {
    port = 8080
}
result = (base) {
    check = neg is PositiveInt
}
"#,
    );
    assert_eq!(json["result"]["check"], false);
}

// ============================================================
// Annotations
// ============================================================

#[test]
fn annotation_module_info_parsed() {
    // @ModuleInfo should be parsed without error
    let json = eval(
        r#"
@ModuleInfo { minPklVersion = "0.25.0" }
module my.Config
x = 42
"#,
    );
    assert_eq!(json["x"], 42);
}

#[test]
fn annotation_deprecated_property() {
    // @Deprecated annotation should not prevent evaluation
    let json = eval(
        r#"
@Deprecated { message = "use newName instead" }
oldName = "value"
result = oldName
"#,
    );
    assert_eq!(json["oldName"], "value");
    assert_eq!(json["result"], "value");
}

#[test]
fn annotation_multiple() {
    let json = eval(
        r#"
@Since { version = "1.0" }
@Deprecated { message = "removed in 2.0" }
legacy = true
current = false
"#,
    );
    assert_eq!(json["legacy"], true);
    assert_eq!(json["current"], false);
}

#[test]
fn annotation_on_class() {
    let json = eval(
        r#"
@Deprecated { message = "use NewConfig" }
class OldConfig {
    name: String = "old"
}
x = new OldConfig {}
"#,
    );
    assert_eq!(json["x"]["name"], "old");
}

#[test]
fn annotation_empty() {
    // @Foo with no body
    let json = eval(
        r#"
@Experimental
feature = true
"#,
    );
    assert_eq!(json["feature"], true);
}

// ============================================================
// Class extends
// ============================================================

#[test]
fn class_extends_inherits_defaults() {
    let json = eval(
        r#"
class Animal {
    name: String = "unknown"
    legs: Int = 4
}
class Dog extends Animal {
    breed: String = "mixed"
}
x = new Dog {
    name = "Rex"
}
"#,
    );
    assert_eq!(json["x"]["name"], "Rex");
    assert_eq!(json["x"]["legs"], 4);
    assert_eq!(json["x"]["breed"], "mixed");
}

#[test]
fn class_extends_override_parent() {
    let json = eval(
        r#"
class Base {
    value: Int = 1
}
class Child extends Base {
    value: Int = 2
    extra: String = "new"
}
x = new Child {}
"#,
    );
    assert_eq!(json["x"]["value"], 2);
    assert_eq!(json["x"]["extra"], "new");
}

// ============================================================
// Module extends
// ============================================================

#[tokio::test]
async fn module_extends_inherits_properties() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_module.pkl"
default_name = "extended"
extra = "new property"
"#;
    let path = base.join("test_extends.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    // default_name overridden
    assert_eq!(json["default_name"], "extended");
    // version inherited from base
    assert_eq!(json["version"], 1);
    // new property added
    assert_eq!(json["extra"], "new property");
}

#[tokio::test]
async fn module_extends_inherits_classes() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_module.pkl"
x = new Config {
    debug = true
}
"#;
    let path = base.join("test_extends_classes.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["x"]["debug"], true);
    assert_eq!(json["x"]["port"], 8080);
}

// ============================================================
// read() and read?()
// ============================================================

#[test]
fn read_env_variable() {
    unsafe { std::env::set_var("PKLR_TEST_VAR", "hello_pklr") };
    let json = eval(
        r#"
x = read("env:PKLR_TEST_VAR")
"#,
    );
    assert_eq!(json["x"], "hello_pklr");
    unsafe { std::env::remove_var("PKLR_TEST_VAR") };
}

#[test]
fn read_or_null_missing_env() {
    let json = eval(
        r#"
x = read?("env:DEFINITELY_NOT_SET_12345")
"#,
    );
    assert!(json["x"].is_null());
}

#[tokio::test]
async fn read_local_file() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
x = read("readme.txt")
"#;
    let path = base.join("test_read.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["x"], "Hello from pklr!\n");
}

#[tokio::test]
async fn read_file_uri() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let file_path = base.join("readme.txt");
    let src = format!(
        r#"
x = read("file://{}")
"#,
        file_path.display()
    );
    let path = base.join("test_read_file.pkl");
    let val = ev.eval_source(&src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["x"], "Hello from pklr!\n");
}

#[test]
fn read_or_null_missing_file() {
    let json = eval(
        r#"
x = read?("file:///nonexistent/path/to/file.txt")
"#,
    );
    assert!(json["x"].is_null());
}

#[test]
fn read_env_in_interpolation() {
    unsafe { std::env::set_var("PKLR_NAME", "world") };
    let json = eval(
        r#"
x = "hello \(read("env:PKLR_NAME"))"
"#,
    );
    assert_eq!(json["x"], "hello world");
    unsafe { std::env::remove_var("PKLR_NAME") };
}

// ============================================================
// Set() deduplication
// ============================================================

#[test]
fn set_deduplicates() {
    let json = eval(r#"x = Set(1, 2, 3, 2, 1)"#);
    assert_eq!(json["x"], serde_json::json!([1, 2, 3]));
}

#[test]
fn set_preserves_order() {
    let json = eval(r#"x = Set("b", "a", "c", "a")"#);
    assert_eq!(json["x"], serde_json::json!(["b", "a", "c"]));
}

#[test]
fn set_empty() {
    let json = eval(r#"x = Set()"#);
    assert_eq!(json["x"], serde_json::json!([]));
}

// ============================================================
// open modifier
// ============================================================

#[test]
fn open_class_allows_new_properties() {
    let json = eval(
        r#"
open class Config {
    port: Int = 8080
}
x = new Config {
    port = 9090
    host = "localhost"
}
"#,
    );
    assert_eq!(json["x"]["port"], 9090);
    assert_eq!(json["x"]["host"], "localhost");
}

#[test]
fn non_open_class_rejects_new_properties() {
    let msg = eval_fails(
        r#"
class Config {
    port: Int = 8080
}
x = new Config {
    port = 9090
    host = "localhost"
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("host"));
}

#[test]
fn non_open_class_rejects_dyn_property() {
    let msg = eval_fails(
        r#"
class Config {
    port: Int = 8080
}
x = new Config {
    ["host"] = "localhost"
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("host"));
}

#[test]
fn non_open_class_preserves_constraint_on_re_instantiation() {
    // After instantiating a non-open class, the is_open=false flag must be
    // preserved so that re-using the result as a base still enforces the constraint.
    let msg = eval_fails(
        r#"
class Config {
    port: Int = 8080
}
base = new Config { port = 9090 }
x = new Config {
    port = base.port
    host = "bad"
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("host"));
}

#[test]
fn non_open_class_allows_overrides() {
    let json = eval(
        r#"
class Config {
    port: Int = 8080
    debug: Boolean = false
}
x = new Config {
    port = 9090
    debug = true
}
"#,
    );
    assert_eq!(json["x"]["port"], 9090);
    assert_eq!(json["x"]["debug"], true);
}

// ============================================================
// Output block handling
// ============================================================

#[test]
fn output_block_is_skipped() {
    let json = eval(
        r#"
x = 1
output {
    renderer {
        converters {
            ["test"] = "hello"
        }
    }
}
"#,
    );
    assert_eq!(json["x"], 1);
    assert!(
        json.get("output").is_none(),
        "output should be skipped, got: {}",
        serde_json::to_string_pretty(&json).unwrap()
    );
}

// ============================================================
// new Dynamic
// ============================================================

#[test]
fn new_dynamic_creates_object() {
    let json = eval(
        r#"
x = new Dynamic {
    _type = "step"
    name = "test"
}
"#,
    );
    assert_eq!(json["x"]["_type"], "step");
    assert_eq!(json["x"]["name"], "test");
}

#[test]
fn new_dynamic_with_spread() {
    let json = eval(
        r#"
base {
    port = 8080
}
x = new Dynamic {
    _type = "config"
    ...base
}
"#,
    );
    assert_eq!(json["x"]["_type"], "config");
    assert_eq!(json["x"]["port"], 8080);
}

// ============================================================
// Class functions
// ============================================================

#[test]
fn class_function_basic() {
    let json = eval(
        r#"
class Greeter {
    name: String = "World"
    function greet(prefix: String): String = prefix + " " + name
}
g = new Greeter {}
result = g.greet("Hello")
"#,
    );
    assert_eq!(json["result"], "Hello World");
}

#[test]
fn null_safe_class_function_call() {
    let json = eval(
        r#"
class Greeter {
    name: String = "World"
    function greet(prefix: String): String = prefix + " " + name
}
g = new Greeter {}
a = g?.greet("Hello")
b = null?.greet("Hello")
"#,
    );
    assert_eq!(json["a"], "Hello World");
    assert_eq!(json["b"], serde_json::Value::Null);
}

#[test]
fn null_safe_unknown_method_errors_without_falling_through() {
    let error = eval_fails(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
result = g?.missing("Hello")
"#,
    );
    assert!(error.contains("unknown method 'missing' on Object"));
}

#[test]
fn null_safe_regex_constructor_emits_type_tag() {
    let json = eval(
        r##"
import "pkl:base" as base
glob = base?.Regex(#"^.*\.json$"#)
"##,
    );
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(json["glob"]["pattern"], r"^.*\.json$");
}

#[test]
fn null_safe_non_function_field_errors_like_regular_call() {
    let error = eval_fails(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
result = g?.name("Hello")
"#,
    );
    assert!(error.contains("cannot call non-function"));
}

#[test]
fn zero_arg_field_call_returns_field_value() {
    let json = eval(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
a = g.name()
b = g?.name()
"#,
    );
    assert_eq!(json["a"], "World");
    assert_eq!(json["b"], "World");
}

#[test]
fn regular_unknown_method_errors_without_falling_through() {
    let error = eval_fails(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
result = g.missing("Hello")
"#,
    );
    assert!(error.contains("unknown method 'missing' on Object"));
}

#[test]
fn class_lambda_valued_property_is_preserved() {
    let json = eval(
        r#"
class Transformer {
    transform = (x) -> x + 1
}
t = new Transformer {}
result = t.transform.apply(2)
"#,
    );
    assert_eq!(json["result"], 3);
    assert_eq!(json["t"]["transform"], "<lambda>");
}

#[test]
fn class_same_named_typed_property_is_preserved() {
    let json = eval(
        r#"
class Script {
    linux: String = "echo ok"
}

class Holder {
    Script = new Script {}
}

h = new Holder {}
"#,
    );
    assert_eq!(json["h"]["Script"]["linux"], "echo ok");
}

#[test]
fn class_function_testmaker_pattern() {
    // First check: does the class itself have checkFail?
    let json1 = eval(
        r#"
class TestMaker {
    filename: String = "file.txt"
    function checkFail(contents: String, code: Int): String = "check:" + filename
}
local testMaker = new TestMaker {}
result = testMaker.checkFail("bad", 1)
"#,
    );
    assert_eq!(json1["result"], "check:file.txt");

    // Second check: does it survive property override?
    let json2 = eval(
        r#"
class TestMaker {
    filename: String = "file.txt"
    function checkFail(contents: String, code: Int): String = "check:" + filename
}
local testMaker = new TestMaker { filename = "main.rs" }
result = testMaker.checkFail("bad", 1)
"#,
    );
    assert_eq!(json2["result"], "check:main.rs");
}

#[test]
fn class_function_with_local() {
    let json = eval(
        r#"
class Calc {
    base: Int = 10
    local function helper(x: Int): Int = x + base
    function compute(x: Int): Int = helper(x)
}
c = new Calc {}
result = c.compute(5)
"#,
    );
    assert_eq!(json["result"], 15);
}

// ============================================================
// hk.pkl compatibility
// ============================================================

#[test]
fn regex_constructor_emits_type_tag() {
    let json = eval(
        r##"
glob = Regex(#"^.*\.json$"#)
"##,
    );
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(json["glob"]["pattern"], r"^.*\.json$");
}

#[tokio::test]
async fn imported_regex_constructor_emits_type_tag() {
    let temp = TestTempDir::new("pklr_test_imported_regex_type_tag");
    let dir = temp.path();
    std::fs::write(
        dir.join("Types.pkl"),
        r#"
import "pkl:base"
function Regex(pattern: String) = base.Regex(pattern)
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("test.pkl"),
        r##"
import "Types.pkl"
glob = Types.Regex(#"^.*\.yaml$"#)
"##,
    )
    .unwrap();

    let mut ev = Evaluator::new_async();
    let path = dir.join("test.pkl");
    let val = ev
        .eval_source(&std::fs::read_to_string(&path).unwrap(), &path)
        .await
        .unwrap();
    let json = val.to_json();
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(json["glob"]["pattern"], r"^.*\.yaml$");
}

#[tokio::test]
async fn hk_step_regex_glob_emits_type_tag() {
    let temp = TestTempDir::new("pklr_test_hk_step_regex_glob");
    let dir = temp.path();
    std::fs::write(
        dir.join("Config.pkl"),
        r#"
import "pkl:base" as base
function Regex(pattern: String) = base.Regex(pattern)
class Step {
    glob: (String | List<String> | Regex)?
    check: String?
}
class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}
hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("hk.pkl"),
        r##"
amends "Config.pkl"
hooks {
    ["check"] {
        steps {
            ["regex-test"] {
                glob = Regex(#"^.*\.json$"#)
                check = "echo {{files}}"
            }
        }
    }
}
"##,
    )
    .unwrap();

    let mut ev = Evaluator::new_async();
    let path = dir.join("hk.pkl");
    let val = ev
        .eval_source(&std::fs::read_to_string(&path).unwrap(), &path)
        .await
        .unwrap();
    let json = val.to_json();
    let glob = &json["hooks"]["check"]["steps"]["regex-test"]["glob"];
    assert_eq!(glob["_type"], "regex");
    assert_eq!(glob["pattern"], r"^.*\.json$");
}

#[test]
fn hk_multiline_regex_pattern_is_normalized() {
    let json = eval(
        r####"
glob = Regex(#"""
    (?x)
    ^.*airflow\.template\.yaml$|
    ^chart/(?:templates|files)/.*\.yaml$
    """#)
"####,
    );
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(
        json["glob"]["pattern"],
        "(?x)\n^.*airflow\\.template\\.yaml$|\n^chart/(?:templates|files)/.*\\.yaml$\n"
    );
}

#[tokio::test]
async fn eval_amends_perf() {
    // Minimal amends test to check performance
    let temp = TestTempDir::new("pklr_test_perf");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
class Step {
    glob: (String | List<String>)?
    check: String?
    fix: String?
    check_first: Boolean = true
    batch: Boolean = false
}
class Hook {
    fix: Boolean?
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}
hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("test.pkl"),
        r#"
amends "Base.pkl"
hooks = new {
    ["pre-commit"] {
        fix = true
        steps = new {
            ["lint"] { check = "lint" }
        }
    }
}
"#,
    )
    .unwrap();
    let path = dir.join("test.pkl");
    let start = std::time::Instant::now();
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    let elapsed = start.elapsed();
    eprintln!("eval_amends_perf: {:?}", elapsed);
    assert!(
        elapsed.as_secs() < 5,
        "amends eval took too long: {:?}",
        elapsed
    );
    assert!(val["hooks"]["pre-commit"]["fix"] == true);
}

#[tokio::test]
async fn class_function_nested_in_new() {
    // Matches the hk builtin pattern: testMaker.checkFail() inside new Config.Step { tests { ... } }
    let temp = TestTempDir::new("pklr_test_nested");
    let dir = temp.path();
    std::fs::write(
        dir.join("helpers.pkl"),
        r#"
class TestMaker {
    filename: String = "file.txt"
    local function makeTest(runType: String, code: Int): String = runType + ":" + filename
    function checkFail(contents: String, code: Int): String = makeTest("check", code)
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "helpers.pkl"
local const testMaker = new helpers.TestMaker { filename = "src/main.rs" }
x {
    tests {
        ["check bad file"] = testMaker.checkFail("bad", 1)
    }
}
"#,
    )
    .unwrap();
    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["x"]["tests"]["check bad file"], "check:src/main.rs");
}

#[tokio::test]
async fn class_function_cross_module() {
    let temp = TestTempDir::new("pklr_test_cross_module");
    let dir = temp.path();
    std::fs::write(
        dir.join("helpers.pkl"),
        r#"
class TestMaker {
    filename: String = "file.txt"
    local function makeTest(runType: String, code: Int): String = runType + ":" + filename
    function checkFail(contents: String, code: Int): String = makeTest("check", code)
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "helpers.pkl"
local const testMaker = new helpers.TestMaker { filename = "main.rs" }
result = testMaker.checkFail("bad", 1)
"#,
    )
    .unwrap();
    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["result"], "check:main.rs");
}

// ============================================================
// HTTP URL rewriting
// ============================================================

#[test]
fn rewrite_url_longest_prefix_wins() {
    let mut ev = Evaluator::new_async();
    ev.set_http_rewrites(&[
        "https://example.com/=https://mirror.local/".to_string(),
        "https://example.com/special/=https://special.local/".to_string(),
    ]);
    // Longest prefix should win
    assert_eq!(
        ev.rewrite_url("https://example.com/special/foo.pkl"),
        "https://special.local/foo.pkl"
    );
    // Shorter prefix matches the rest
    assert_eq!(
        ev.rewrite_url("https://example.com/other/bar.pkl"),
        "https://mirror.local/other/bar.pkl"
    );
    // No match returns original
    assert_eq!(
        ev.rewrite_url("https://other.com/foo.pkl"),
        "https://other.com/foo.pkl"
    );
}

#[test]
fn rewrite_url_no_rules_is_identity() {
    let ev = Evaluator::new_async();
    assert_eq!(
        ev.rewrite_url("https://example.com/foo.pkl"),
        "https://example.com/foo.pkl"
    );
}

#[test]
fn class_instance_rejects_wrong_property_type() {
    let message = eval_fails(
        r#"
class Factory {
    enabled: Boolean = false
}

factory = new Factory { enabled = "yes" }
"#,
    );
    assert!(message.contains("enabled"), "{message}");
    assert!(message.contains("Boolean"), "{message}");
}

#[test]
fn class_instance_rejects_value_outside_string_literal_union() {
    let message = eval_fails(
        r#"
class Factory {
    version: "3" | "4" = "4"
}

factory = new Factory { version = "5" }
"#,
    );
    assert!(message.contains("version"), "{message}");
    assert!(message.contains("\"3\"|\"4\""), "{message}");
}

#[test]
fn class_instance_validates_hidden_property_types() {
    let message = eval_fails(
        r#"
class Factory {
    hidden enabled: Boolean = false
}

factory = new Factory { enabled = "yes" }
"#,
    );
    assert!(message.contains("enabled"), "{message}");
    assert!(message.contains("Boolean"), "{message}");
}

#[test]
fn class_instance_does_not_validate_missing_property_against_enclosing_scope() {
    let json = eval(
        r#"
local enabled = "not a boolean"

class Factory {
    hidden enabled: Boolean
}

factory = new Factory {}
"#,
    );
    assert_eq!(json["factory"], serde_json::json!({}));
}

// ============================================================
// output.renderer.converters
// ============================================================

#[test]
fn converter_injects_type_tag() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toMap().toDynamic()
            }
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "cargo test");
}

#[test]
fn converter_applies_to_amended_instance() {
    // Amending an instance preserves its class identity, so the class-keyed
    // converter still matches the amended value.
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toMap().toDynamic()
            }
        }
    }
}

local base = new Step { check = "a" }
myStep = (base) { check = "b" }
"#,
    );
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "b");
}

#[test]
fn converter_applies_to_subclass_instance() {
    let json = eval_with_converters(
        r#"
class Factory {
    fixed output = new { check = "base" }
}
class Prettier extends Factory {}

output {
    renderer {
        converters {
            [Factory] = (factory) -> factory.output
        }
    }
}

step = new Prettier {}
"#,
    );
    assert_eq!(json["step"]["check"], "base");
    assert!(json["step"].get("output").is_none());
}

#[test]
fn converter_prefers_most_specific_class() {
    let json = eval_with_converters(
        r#"
abstract class Factory {}
class Prettier extends Factory {}

output {
    renderer {
        converters {
            [Factory] = (_) -> "factory"
            [Prettier] = (_) -> "prettier"
        }
    }
}

value = new Prettier {}
"#,
    );
    assert_eq!(json["value"], "prettier");
}

#[test]
fn converter_can_call_module_local_helper() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

local function renderStep(step) = new Dynamic {
    _type = "step"
    ...step.toMap().toDynamic()
}

output {
    renderer {
        converters {
            [Step] = (step) -> renderStep(step)
        }
    }
}

step = new Step { check = "cargo test" }
"#,
    );
    assert_eq!(json["step"]["_type"], "step");
    assert_eq!(json["step"]["check"], "cargo test");
}

#[test]
fn converter_can_call_output_local_helper() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    local function renderStep(step) = new Dynamic {
        _type = "step"
        ...step.toMap().toDynamic()
    }
    renderer {
        converters {
            [Step] = (step) -> renderStep(step)
        }
    }
}

step = new Step { check = "cargo test" }
"#,
    );
    assert_eq!(json["step"]["_type"], "step");
    assert_eq!(json["step"]["check"], "cargo test");
}

#[test]
fn hk_style_factories_support_options_step_amendments_and_containers() {
    let json = eval_with_converters(
        r#"
open class Step {
    check: String = ""
    batch: Boolean = false
}

abstract class BuiltinFactory {
    step: Step
}

class Gitleaks extends BuiltinFactory {
    staged: Boolean = false
    local factory = this
    step = new Step {
        check = if (factory.staged) "gitleaks --staged" else "gitleaks"
    }
}

local gitleaks = new Gitleaks {}

plain = gitleaks
configured = (gitleaks) {
    staged = true
    step { batch = true }
}
steps: Mapping<String, BuiltinFactory | Step> = new Mapping {
    ["factory"] = (gitleaks) { staged = true }
    ["manual"] = new Step { check = "cargo test" }
}
all: Mapping<String, BuiltinFactory> = new Mapping {
    ["gitleaks"] = gitleaks
}

output {
    local function renderStep(step) = new Dynamic {
        _type = "step"
        ...step.toMap().toDynamic()
    }
    renderer {
        converters {
            [BuiltinFactory] = (factory) -> renderStep(factory.step)
            [Step] = (step) -> renderStep(step)
        }
    }
}
"#,
    );

    assert_eq!(json["plain"]["check"], "gitleaks");
    assert_eq!(json["plain"]["batch"], false);
    assert_eq!(json["plain"]["_type"], "step");
    assert_eq!(json["configured"]["check"], "gitleaks --staged");
    assert_eq!(json["configured"]["batch"], true);
    assert_eq!(json["configured"]["_type"], "step");
    assert_eq!(json["steps"]["factory"]["check"], "gitleaks --staged");
    assert_eq!(json["steps"]["manual"]["check"], "cargo test");
    assert_eq!(json["all"]["gitleaks"]["check"], "gitleaks");
    assert!(json["configured"].get("staged").is_none());
    assert!(json["configured"].get("step").is_none());
}

#[test]
fn hk_style_factory_rejects_unknown_input() {
    let message = eval_fails(
        r#"
class Step {}
abstract class BuiltinFactory { step: Step }
class Prettier extends BuiltinFactory { step = new Step {} }
prettier = new Prettier { futureOption = true }
"#,
    );
    assert!(message.contains("futureOption"), "{message}");
    assert!(message.contains("non-open"), "{message}");
}

#[test]
fn converter_coerces_values() {
    let json = eval_with_converters(
        r#"
class Step {
    depends: String|List<String> = ""
    stash: Boolean|String = false
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s
                    .toMap()
                    .mapValues((k, v) ->
                        if (k == "depends" && v is String)
                            List(v)
                        else if (k == "stash" && v is Boolean)
                            if (v) "git" else "none"
                        else
                            v
                    )
                    .toDynamic()
            }
        }
    }
}

myStep = new Step {
    depends = "other"
    stash = true
}
"#,
    );
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["depends"], serde_json::json!(["other"]));
    assert_eq!(json["myStep"]["stash"], "git");
}

#[test]
fn converter_to_dynamic_removes_type_metadata() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Step {
                ...s
                    .toMap()
                    .mapValues((k, v) -> v)
                    .toDynamic()
            }.toDynamic()
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["check"], "cargo test");
}

#[test]
fn converter_does_not_reconvert_its_root_result() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Step {
                check = "\(s.check)!"
            }
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["check"], "cargo test!");
}

#[test]
fn converter_can_chain_to_different_root_type() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

class RenderedStep {
    label: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new RenderedStep {
                label = s.check
            }
            [RenderedStep] = (s) -> new Dynamic {
                rendered = s.label
            }
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["rendered"], "cargo test");
}

#[test]
fn converter_multiple_types() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

myGroup = new Group {
    steps {
        ["lint"] = new Step {
            check = "eslint"
        }
    }
}
"#,
    );
    assert_eq!(json["myGroup"]["_type"], "group");
    assert_eq!(json["myGroup"]["steps"]["lint"]["_type"], "step");
    assert_eq!(json["myGroup"]["steps"]["lint"]["check"], "eslint");
}

#[test]
fn converter_union_mapping_chooses_matching_default_type() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
    shared: Boolean = false
}

class Hook {
    steps: Mapping<String, Step | Group> = new Mapping<String, Step | Group> {
        default {
            shared = true
        }
    }
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

hook = new Hook {
    steps {
        ["group"] {
            steps {
                ["lint"] {
                    check = "eslint"
                }
            }
        }
        ["echo"] {
            check = "echo ok"
        }
    }
}
"#,
    );
    assert_eq!(json["hook"]["steps"]["group"]["_type"], "group");
    assert_eq!(json["hook"]["steps"]["group"]["shared"], true);
    assert_eq!(
        json["hook"]["steps"]["group"]["steps"]["lint"]["_type"],
        "step"
    );
    assert_eq!(
        json["hook"]["steps"]["group"]["steps"]["lint"]["check"],
        "eslint"
    );
    assert_eq!(json["hook"]["steps"]["echo"]["_type"], "step");
    assert_eq!(json["hook"]["steps"]["echo"]["shared"], true);
    assert_eq!(json["hook"]["steps"]["echo"]["check"], "echo ok");
}

#[test]
fn converter_union_mapping_layers_explicit_default_over_type_default() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
    shared: Boolean = false
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, Step | Group> {
    default {
        shared = true
    }
    ["group"] {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
    ["echo"] {
        check = "echo ok"
    }
}
"#,
    );
    assert_eq!(json["steps"]["group"]["_type"], "group");
    assert_eq!(json["steps"]["group"]["shared"], true);
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["shared"], true);
}

#[test]
fn converter_union_mapping_preserves_explicit_new_type() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
    shared: Boolean = false
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, Step | Group> {
    default {
        shared = true
    }
    ["group"] = new Group {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
    ["echo"] {
        check = "echo ok"
    }
}
"#,
    );
    assert_eq!(json["steps"]["group"]["_type"], "group");
    assert_eq!(json["steps"]["group"]["shared"], true);
    assert_eq!(json["steps"]["group"]["steps"]["lint"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["shared"], true);
}

#[test]
fn converter_union_mapping_preserves_variable_value_type_after_default_merge() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
    shared: Boolean = false
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

local echo = new Step {
    check = "echo ok"
}

steps = new Mapping<String, Dynamic | Step> {
    default {
        shared = true
    }
    ["echo"] = echo
}
"#,
    );
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["check"], "echo ok");
    assert_eq!(json["steps"]["echo"]["shared"], false);
}

#[test]
fn converter_union_mapping_explicit_new_validates_class_body() {
    let msg = eval_fails(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

steps = new Mapping<String, Step | Group> {
    ["group"] = new Group {
        unknown = true
    }
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("unknown"));
}

#[test]
fn converter_union_mapping_explicit_default_does_not_open_new_body() {
    let msg = eval_fails(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

steps = new Mapping<String, Step | Group> {
    default {
        extra = false
    }
    ["group"] = new Group {
        extra = true
    }
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("extra"));
}

#[test]
fn converter_union_mapping_untyped_new_stays_untyped() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, Step | Group> {
    ["plain"] = new {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
}
"#,
    );
    assert_eq!(json["steps"]["plain"]["_type"], serde_json::Value::Null);
    assert_eq!(json["steps"]["plain"]["steps"]["lint"]["check"], "eslint");
}

#[test]
fn converter_union_mapping_explicit_new_without_type_default_uses_constructor() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
}

typealias GroupAlias = Group

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, GroupAlias | Step> {
    default {
        shared = true
    }
    ["group"] = new Group {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
}
"#,
    );
    assert_eq!(json["steps"]["group"]["_type"], "group");
    assert_eq!(json["steps"]["group"]["shared"], true);
    assert_eq!(json["steps"]["group"]["steps"]["lint"]["_type"], "step");
}

#[test]
fn mapping_amendment_preserves_type_aliases() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

typealias StepAlias = Step

hook = new Hook {
    steps {
        ["echo"] {
            check = (new Step { check = "echo ok" } as StepAlias).check
        }
    }
}
"#,
    );
    assert_eq!(json["hook"]["steps"]["echo"]["check"], "echo ok");
}

#[test]
fn mapping_local_const_is_visible_to_dynamic_entries() {
    let json = eval(
        r#"
values = new Mapping<String, String> {
    local const prefix = "hello"
    ["message"] = "\(prefix), world"
}
"#,
    );
    assert_eq!(json["values"]["message"], "hello, world");
}

#[test]
fn mapping_local_function_is_visible_to_dynamic_entries() {
    let json = eval(
        r#"
values = new Mapping<String, String> {
    local const prefix = "value"
    local function wrap(s: String): String = "[\(prefix):\(s)]"
    ["message"] = wrap("ok")
}
"#,
    );
    assert_eq!(json["values"]["message"], "[value:ok]");
}

#[test]
fn mapping_local_lambda_is_visible_to_sibling_local() {
    // A lambda local must be in scope for a later (non-lambda) local that uses it.
    let json = eval(
        r#"
values = new Mapping<String, Int> {
    local f = (k, v) -> v * 2
    local doubled = (new Mapping<String, Int> { ["a"] = 1 }).toMap().mapValues(f).toMapping()
    ["out"] = doubled["a"]
}
"#,
    );
    assert_eq!(json["values"]["out"], 2);
}

#[test]
fn mapping_local_lambda_is_visible_to_sibling_local_untyped() {
    // Same, for an untyped `new Mapping {}` body (different eval path).
    let json = eval(
        r#"
values = new Mapping {
    local f = (x) -> x + 1
    local g = f.apply(10)
    ["out"] = g
}
"#,
    );
    assert_eq!(json["values"]["out"], 11);
}

#[test]
fn object_local_lambda_does_not_capture_later_local_for_early_call() {
    let msg = eval_fails(
        r#"
values {
    local f = (x) -> x + h
    local g = f.apply(1)
    local h = 42
    out = g
}
"#,
    );
    assert!(msg.contains("undefined variable: h"), "{msg}");
}

#[test]
fn mapping_local_lambda_does_not_capture_later_local_for_early_call() {
    let msg = eval_fails(
        r#"
values = new Mapping {
    local f = (x) -> x + h
    local g = f.apply(1)
    local h = 42
    ["out"] = g
}
"#,
    );
    assert!(msg.contains("undefined variable: h"), "{msg}");
}

#[test]
fn mapping_local_body_is_visible_to_dynamic_entries() {
    let json = eval(
        r#"
values = new Mapping<String, Int> {
    local options {
        port = 3000
    }
    ["port"] = options.port
}
"#,
    );
    assert_eq!(json["values"]["port"], 3000);
}

#[test]
fn single_type_mapping_amendment_preserves_default_template() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
    enabled: Boolean = false
}

class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {
        default {
            enabled = true
        }
    }
}

hook = new Hook {
    steps {
        ["echo"] {
            check = "echo ok"
        }
    }
}
"#,
    );
    assert_eq!(json["hook"]["steps"]["echo"]["check"], "echo ok");
    assert_eq!(json["hook"]["steps"]["echo"]["enabled"], true);
}

#[test]
fn mapping_entry_body_recomputes_late_bound_type_properties() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
    label: String = "Step: \(check)"
    enabled: Boolean = false
}

steps = new Mapping<String, Step> {
    default {
        enabled = true
    }
    ["lint"] {
        check = "eslint"
    }
}
"#,
    );
    assert_eq!(json["steps"]["lint"]["check"], "eslint");
    assert_eq!(json["steps"]["lint"]["label"], "Step: eslint");
    assert_eq!(json["steps"]["lint"]["enabled"], true);
}

#[test]
fn converter_no_converters_is_noop() {
    let json = eval_with_converters(
        r#"
x = 1
y = "hello"
"#,
    );
    assert_eq!(json["x"], 1);
    assert_eq!(json["y"], "hello");
}

#[test]
fn converter_output_not_in_result() {
    let json = eval_with_converters(
        r#"
class Foo {
    x: Int = 0
}

output {
    renderer {
        converters {
            [Foo] = (f) -> new Dynamic {
                _type = "foo"
                ...f.toDynamic()
            }
        }
    }
}

item = new Foo { x = 42 }
"#,
    );
    assert!(json.get("output").is_none());
    assert_eq!(json["item"]["_type"], "foo");
    assert_eq!(json["item"]["x"], 42);
}

#[test]
fn converter_inherited_from_amends_base() {
    use std::io::Write;
    let dir =
        std::env::temp_dir().join(format!("pklr_test_amends_converter_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Base module with class + converter
    let base_path = dir.join("Base.pkl");
    let mut base_file = std::fs::File::create(&base_path).unwrap();
    write!(
        base_file,
        r#"
class Step {{
    check: String = ""
}}

output {{
    renderer {{
        converters {{
            [Step] = (s) -> new Dynamic {{
                _type = "step"
                ...s.toDynamic()
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    // Amending module
    let child_path = dir.join("child.pkl");
    let mut child_file = std::fs::File::create(&child_path).unwrap();
    write!(
        child_file,
        r#"amends "Base.pkl"

myStep = new Step {{
    check = "cargo test"
}}
"#
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let val = ev
            .eval_source(&std::fs::read_to_string(&child_path).unwrap(), &child_path)
            .await
            .unwrap();
        let val = ev.apply_converters(val).await.unwrap();
        val.to_json()
    });
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "cargo test");
}

#[test]
fn converter_inherited_from_extends_base() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_extends_converter_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let base_path = dir.join("Base.pkl");
    let mut base_file = std::fs::File::create(&base_path).unwrap();
    write!(
        base_file,
        r#"
open class Step {{
    check: String = ""
}}

output {{
    renderer {{
        converters {{
            [Step] = (s) -> new Dynamic {{
                _type = "step"
                ...s.toDynamic()
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    let child_path = dir.join("child.pkl");
    let mut child_file = std::fs::File::create(&child_path).unwrap();
    write!(
        child_file,
        r#"extends "Base.pkl"

myStep = new Step {{
    check = "make test"
}}
"#
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let val = ev
            .eval_source(&std::fs::read_to_string(&child_path).unwrap(), &child_path)
            .await
            .unwrap();
        let val = ev.apply_converters(val).await.unwrap();
        val.to_json()
    });
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "make test");
}

/// Simple: amends with bare object in a typed Mapping property
#[test]
fn converter_amends_simple_typed_mapping() {
    let json = eval_with_converters(
        r#"
open class Step {
    check: String = ""
}

steps: Mapping<String, Step> = new Mapping<String, Step> {}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps {
    ["echo"] {
        check = "echo ok"
    }
}
"#,
    );
    eprintln!(
        "simple JSON: {}",
        serde_json::to_string_pretty(&json).unwrap()
    );
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["check"], "echo ok");
}

/// Reproduces the hk pattern: base defines classes + typed Mappings + converters,
/// child amends with bare object bodies (no `new Step`).
#[test]
fn converter_amends_bare_object_in_mapping() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("pklr_test_bare_mapping_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let base_path = dir.join("Config.pkl");
    let mut f = std::fs::File::create(&base_path).unwrap();
    write!(
        f,
        r#"
open class Step {{
    check: String = ""
    fix: String = ""
}}

open class Hook {{
    steps: Mapping<String, Step> = new Mapping<String, Step> {{}}
}}

hooks: Mapping<String, Hook> = new Mapping<String, Hook> {{}}

output {{
    renderer {{
        converters {{
            [Step] = (s) -> new Dynamic {{
                _type = "step"
                ...s.toDynamic()
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    let child_path = dir.join("hk.pkl");
    let mut f = std::fs::File::create(&child_path).unwrap();
    write!(
        f,
        r#"amends "Config.pkl"

hooks {{
    ["check"] {{
        steps {{
            ["echo"] {{
                check = "echo ok"
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let val = ev
            .eval_source(&std::fs::read_to_string(&child_path).unwrap(), &child_path)
            .await
            .unwrap();
        let val = ev.apply_converters(val).await.unwrap();
        val.to_json()
    });
    eprintln!("JSON: {}", serde_json::to_string_pretty(&json).unwrap());
    assert_eq!(json["hooks"]["check"]["steps"]["echo"]["_type"], "step");
    assert_eq!(json["hooks"]["check"]["steps"]["echo"]["check"], "echo ok");
}

#[test]
fn package_amends_with_rewrite_inherits_hk_style_converter() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_package_rewrite_converter");
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Config.pkl", options).unwrap();
        zip.write_all(
            br#"
class Step {
    check: String = ""
}

class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}

output {
    renderer {
        converters {
            [Step] = (s) -> new Step {
                ...s
                    .toMap()
                    .mapValues((k, v) ->
                        if (k == "check")
                            "\(v)!"
                        else
                            v
                    )
                    .toDynamic()
            }.toDynamic()
        }
    }
}
"#,
        )
        .unwrap();
        zip.finish().unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            zip_bytes.len()
        )
        .unwrap();
        stream.write_all(&zip_bytes).unwrap();
    });

    let child_path = temp.path().join("hk.pkl");
    std::fs::write(
        &child_path,
        r#"
amends "package://example.com/v1.0.0/hk@1.0.0#/Config.pkl"

hooks {
    ["check"] {
        steps {
            ["echo"] {
                check = "echo ok"
            }
        }
    }
}
"#,
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt
        .block_on(async {
            pklr::eval_to_json_with_options_async(
                &child_path,
                pklr::AsyncEvalOptions {
                    http_rewrites: vec![format!("https://example.com/=http://{addr}/")],
                    ..Default::default()
                },
            )
            .await
        })
        .unwrap();
    server.join().unwrap();

    assert_eq!(json["hooks"]["check"]["steps"]["echo"]["check"], "echo ok!");
}

#[test]
fn package_cache_survives_across_offline_evaluators() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_persistent_package_cache");
    let cache_dir = temp.path().join("cache");
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file("Config.pkl", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"answer = 42\n").unwrap();
        zip.finish().unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            zip_bytes.len()
        )
        .unwrap();
        stream.write_all(&zip_bytes).unwrap();
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();
    let rewrite = format!("https://example.com/=http://{addr}/");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite.clone()])
                .package_cache_dir(cache_dir.clone())
                .eval_to_json(&config_path),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(first["answer"], 42);

    // A new evaluator succeeds after the one-shot server has shut down.
    let second = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite])
                .package_cache_dir(cache_dir)
                .offline(true)
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(second["answer"], 42);
}

/// Build a single-entry package zip holding `contents` at `name`.
#[cfg(feature = "package-zip-core")]
fn package_zip(name: &str, contents: &str) -> Vec<u8> {
    use std::io::Write;

    let mut bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    bytes
}

#[test]
fn preloaded_package_evaluates_offline_without_a_cold_start() {
    let temp = TestTempDir::new("pklr_test_preloaded_package");
    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();

    // No server is ever started: the preloaded zip is the only source.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(temp.path().join("cache"))
                .offline(true)
                .preload_package(
                    "https://example.com/pkg@1.0.0.zip",
                    "zip",
                    package_zip("Config.pkl", "answer = 42\n"),
                )
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(json["answer"], 42);
}

#[test]
fn preloaded_package_does_not_override_a_cached_download() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_preload_precedence");
    let cache_dir = temp.path().join("cache");
    let fetched = package_zip("Config.pkl", "answer = 42\n");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            fetched.len()
        )
        .unwrap();
        stream.write_all(&fetched).unwrap();
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();
    let rewrite = format!("https://example.com/=http://{addr}/");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(
        pklr::AsyncEvaluatorBuilder::new()
            .http_rewrites([rewrite])
            .package_cache_dir(cache_dir.clone())
            .eval_to_json(&config_path),
    )
    .unwrap();
    server.join().unwrap();

    // The downloaded package is already cached, so the preloaded copy is ignored.
    let json = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(cache_dir)
                .offline(true)
                .preload_package(
                    "https://example.com/pkg@1.0.0.zip",
                    "zip",
                    package_zip("Config.pkl", "answer = 7\n"),
                )
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(json["answer"], 42);
}

#[test]
fn preloading_a_package_for_another_version_is_a_cache_miss() {
    let temp = TestTempDir::new("pklr_test_preload_version_mismatch");
    let config_path = temp.path().join("config.pkl");
    // The config pins 2.0.0 while the preloaded package is 1.0.0.
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@2.0.0#/Config.pkl\"\n",
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let error = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(temp.path().join("cache"))
                .offline(true)
                .preload_package(
                    "https://example.com/pkg@1.0.0.zip",
                    "zip",
                    package_zip("Config.pkl", "answer = 42\n"),
                )
                .eval_to_json(&config_path),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("package is not cached and offline mode is enabled"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn preloading_invalid_package_bytes_is_rejected() {
    let temp = TestTempDir::new("pklr_test_preload_invalid");
    let mut evaluator = pklr::Evaluator::new_async();
    evaluator.set_package_cache_dir(temp.path().join("cache"));
    let error = evaluator
        .preload_package_async("https://example.com/pkg@1.0.0.zip", "zip", b"not a zip")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("package archive is invalid"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn preloading_reports_a_cache_write_failure() {
    let temp = TestTempDir::new("pklr_test_preload_write_failure");
    // A file where the cache directory belongs: the seed cannot be stored, and
    // reporting success would leave the host expecting a usable cache entry.
    let cache_path = temp.path().join("cache");
    std::fs::write(&cache_path, b"not a directory").unwrap();

    let mut evaluator = pklr::Evaluator::new_async();
    evaluator.set_package_cache_dir(&cache_path);
    let error = evaluator
        .preload_package_async(
            "https://example.com/pkg@1.0.0.zip",
            "zip",
            &package_zip("Config.pkl", "answer = 42\n"),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&cache_path.display().to_string()),
        "error should name the cache path: {error}"
    );
}

#[test]
fn direct_package_relatives_survive_across_offline_evaluators() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_direct_package_relative_cache");
    let cache_dir = temp.path().join("cache");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let body = if request.starts_with("GET /Config.pkl ") {
                "amends \"./Base.pkl\"\nanswer = 42\n"
            } else if request.starts_with("GET /Base.pkl ") {
                "base = 41\n"
            } else {
                panic!("unexpected request: {request}");
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://pkg.pkl-lang.org/github.com/acme/pkg@v1#/Config.pkl\"\n",
    )
    .unwrap();
    let rewrite = format!("https://github.com/acme/pkg/releases/download/v1/=http://{addr}/");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite.clone()])
                .package_cache_dir(cache_dir.clone())
                .eval_to_json(&config_path),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(first["base"], 41);
    assert_eq!(first["answer"], 42);

    let second = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite])
                .package_cache_dir(cache_dir)
                .offline(true)
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(second["base"], 41);
    assert_eq!(second["answer"], 42);
}

#[test]
fn unreadable_package_cache_is_a_miss_while_online() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_unreadable_package_cache");
    let cache_path = temp.path().join("cache");
    std::fs::write(&cache_path, "not a directory").unwrap();
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file("Config.pkl", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"answer = 42\n").unwrap();
        zip.finish().unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            zip_bytes.len()
        )
        .unwrap();
        stream.write_all(&zip_bytes).unwrap();
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([format!("https://example.com/=http://{addr}/")])
                .package_cache_dir(cache_path)
                .eval_to_json(&config_path),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(json["answer"], 42);
}

#[test]
fn offline_package_cache_miss_is_actionable() {
    let temp = TestTempDir::new("pklr_test_offline_package_cache_miss");
    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let error = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(temp.path().join("cache"))
                .offline(true)
                .eval_to_json(&config_path),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("package is not cached and offline mode is enabled"));
    assert!(error.contains("https://example.com/pkg@1.0.0.zip"));
}
