//! Tests for pkl language features, organized by category.
//!
//! Tests marked `#[ignore]` document features not yet implemented.
//! As features are added, remove the `#[ignore]` attribute.

use pklr::eval::Evaluator;

fn eval(src: &str) -> serde_json::Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut ev = Evaluator::new();
        let path = std::path::Path::new("test.pkl");
        let val = ev.eval_source(src, path).await.unwrap();
        val.to_json()
    })
}

fn eval_fails(src: &str) -> String {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut ev = Evaluator::new();
        let path = std::path::Path::new("test.pkl");
        match ev.eval_source(src, path).await {
            Err(e) => e.to_string(),
            Ok(v) => panic!("expected error, got: {:?}", v.to_json()),
        }
    })
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

// ============================================================
// Import resolution (future)
// ============================================================

#[tokio::test]
async fn import_local_file() {
    let mut ev = pklr::eval::Evaluator::new();
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
    let mut ev = pklr::eval::Evaluator::new();
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
    let mut ev = pklr::eval::Evaluator::new();
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
