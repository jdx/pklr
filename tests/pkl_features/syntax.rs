use super::*;

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

#[test]
fn minimum_int_requires_a_unary_minus() {
    let json = eval("x = -9223372036854775808");
    assert_eq!(json["x"], i64::MIN);
    for src in ["x = 9223372036854775808", "x = 1 - 9223372036854775808"] {
        let err = eval_fails(src);
        assert!(err.contains("too large"), "{src}: {err}");
    }
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
    assert_eq!(json["x"], "hello\nworld");
}

#[test]
fn string_multiline_with_crlf_line_endings() {
    let src = "x = \"\"\"\r\n  hello\r\n  world\r\n  \"\"\"";
    let json = eval(src);
    assert_eq!(json["x"], "hello\nworld");
}

#[test]
fn string_raw_multiline() {
    let src = "x = #\"\"\"\n  hello\\n\n  world\n  \"\"\"#";
    let json = eval(src);
    assert_eq!(json["x"], "hello\\n\nworld");
}

#[test]
fn string_raw_multiline_with_crlf_line_endings() {
    let src = "x = #\"\"\"\r\n  hello\\n\r\n  world\r\n  \"\"\"#";
    let json = eval(src);
    assert_eq!(json["x"], "hello\\n\nworld");
}

#[test]
fn string_multiline_strips_only_one_opening_newline() {
    let src = "x = \"\"\"\n\n  hello\n  \"\"\"";
    let json = eval(src);
    assert_eq!(json["x"], "\nhello");
}

#[test]
fn string_multiline_allows_closing_after_line_continuation() {
    let json = eval("x = \"\"\"\n  hello\\\n  \"\"\"");
    assert_eq!(json["x"], "hello");
    let json = eval("x = \"\"\"\nhello\\\n\"\"\"");
    assert_eq!(json["x"], "hello");
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
    assert_eq!(json["x"], "hello \"\"\"#\nworld");
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

#[test]
fn string_interpolation_rejects_extra_tokens() {
    let err = eval_fails(
        r#"a = 1
x = "\(a a)"
"#,
    );
    assert!(err.contains("Unexpected token `a`. Expected `)`."), "{err}");
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
    // `/` always divides as floats; `~/` truncates.
    let json = eval(r#"x = 10 / 4"#);
    assert_eq!(json["x"], 2.5);
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
    // `1 / 0` is Infinity, which JSON cannot represent.
    let json = eval(r#"x = (1 / 0).isInfinite"#);
    assert_eq!(json["x"], true);
}

#[test]
fn arithmetic_mod_by_zero() {
    let msg = eval_fails(r#"x = 1 % 0"#);
    assert!(msg.contains("Division by zero."));
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
    assert!(msg.contains("Division by zero."));
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
fn exp_negative_exponent_is_float() {
    let json = eval(r#"x = 2 ** -1"#);
    assert_eq!(json["x"], 0.5);
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

#[test]
fn equality_of_listings() {
    let json = eval(
        r#"
local x = new Listing { "one"; "two" }
same = x == x
equal = x == new Listing { "one"; "two" }
reordered = x == new Listing { "two"; "one" }
amended = x == (x) {}
withDefault = x == (x) { default = 9 }
withLocal = new Listing { y; local y = "one" } == new Listing { "one" }
ne = x != new Listing { "one" }
"#,
    );
    assert_eq!(json["same"], true);
    assert_eq!(json["equal"], true);
    assert_eq!(json["reordered"], false);
    assert_eq!(json["amended"], true);
    assert_eq!(json["withDefault"], true);
    assert_eq!(json["withLocal"], true);
    assert_eq!(json["ne"], true);
}

#[test]
fn amendments_require_a_parenthesized_parent() {
    for src in [
        "local value = new Dynamic {}\nx = value { a = 1 }\n",
        "local make = () -> new Dynamic {}\nx = make() { a = 1 }\n",
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains("wrap the parent in parentheses"),
            "{src}: {err}"
        );
    }
}

#[test]
fn equality_of_mappings_ignores_order() {
    let json = eval(
        r#"
local x = new Mapping { ["one"] = 1; ["two"] = 2 }
reordered = x == new Mapping { ["two"] = 2; ["one"] = 1 }
changed = x == (x) { ["one"] = 2 }
amended = x == (x) { ["one"] = 1 }
dynamic = x == new Dynamic { ["one"] = 1; ["two"] = 2 }
"#,
    );
    assert_eq!(json["reordered"], true);
    assert_eq!(json["changed"], false);
    assert_eq!(json["amended"], true);
    assert_eq!(json["dynamic"], false);
}

#[test]
fn equality_of_collections_depends_on_kind() {
    let json = eval(
        r#"
lists = List(1, 2) == List(1, 2)
listVsListing = List(1, 2) == new Listing { 1; 2 }
listVsSet = List(1, 2) == Set(1, 2)
sets = Set(1, 2) == Set(2, 1)
toSet = List(2, 1, 2).toSet() == Set(1, 2)
toList = new Listing { 1 }.toList() == List(1)
"#,
    );
    assert_eq!(json["lists"], true);
    assert_eq!(json["listVsListing"], false);
    assert_eq!(json["listVsSet"], false);
    assert_eq!(json["sets"], true);
    assert_eq!(json["toSet"], true);
    assert_eq!(json["toList"], true);
}

#[test]
fn equality_of_typed_objects() {
    let json = eval(
        r#"
open class Person {
  name = "Pigeon"
  hidden street: String
  function greet() = "hi \(name)"
}
class Person2 { name = "Pigeon" }
class Student extends Person {}
same = new Person {} == new Person {}
hiddenIgnored = new Person { street = "Fox St." } == new Person {}
changed = new Person { name = "Parrot" } == new Person {}
otherClass = new Person {} == new Person2 {}
subclass = new Student {} == new Person {}
dynamic = new Person {} == new Dynamic { name = "Pigeon" }
classes = Person == Person
classVsInstance = new Person {} == Person
differentClasses = Person == Person2
"#,
    );
    assert_eq!(json["same"], true);
    assert_eq!(json["hiddenIgnored"], true);
    assert_eq!(json["changed"], false);
    assert_eq!(json["otherClass"], false);
    assert_eq!(json["subclass"], false);
    assert_eq!(json["dynamic"], false);
    assert_eq!(json["classes"], true);
    assert_eq!(json["classVsInstance"], false);
    assert_eq!(json["differentClasses"], false);
}

#[test]
fn equality_ignores_methods() {
    // Local methods in object bodies are not members, like a class's methods.
    let json = eval(
        r#"
dynamic = new Dynamic { x = 1; local function f() = 1 } == new Dynamic { x = 1 }
open class P { x = 1 }
typed = new P { local function g() = 2 } == new P {}
"#,
    );
    assert_eq!(json["dynamic"], true);
    assert_eq!(json["typed"], true);
}

#[test]
fn equality_of_lambdas_is_identity() {
    let json = eval(
        r#"
local f = () -> 1
same = f == f
sameBody = (() -> 1) == (() -> 1)
"#,
    );
    assert_eq!(json["same"], true);
    assert_eq!(json["sameBody"], false);
}

#[test]
fn collection_methods_keep_set_kind() {
    let json = eval(
        r#"
filtered = Set(1, 2).filter((x) -> true) == Set(1, 2)
mapped = Set(1, 2).map((x) -> x % 2) == Set(1, 0)
mappedSize = Set(1, 2, 3).map((x) -> x % 2).length
flatMapped = Set(1, 2).flatMap((x) -> List(x, x + 1)).length
nonNull = Set(1, null).filterNonNull() == Set(1)
list = List(1, 2).filter((x) -> true) == List(1, 2)
"#,
    );
    for key in ["filtered", "mapped", "nonNull", "list"] {
        assert_eq!(json[key], true, "{key}");
    }
    assert_eq!(json["mappedSize"], 2);
    assert_eq!(json["flatMapped"], 3);
}

#[test]
fn listing_locals_and_default_see_each_other_in_any_order() {
    let json = eval(
        r#"
local x = "outer"
shadowed = new Listing { y; local y = x; local x = "inner" }
before = new Listing { z; local z = default.apply(0); default = (_) -> 9 }
viaFunction = new Listing { f.apply(); local f = () -> default.apply(0); default = (_) -> 7 }
"#,
    );
    assert_eq!(json["shadowed"], serde_json::json!(["inner"]));
    assert_eq!(json["before"], serde_json::json!([9]));
    assert_eq!(json["viaFunction"], serde_json::json!([7]));
}

#[test]
fn adding_collections_keeps_the_left_kind() {
    let json = eval(
        r#"
listPlusSet = (List(1, 2) + Set(2, 3)) == List(1, 2, 2, 3)
setPlusList = (Set(1, 2) + List(2, 3)) == Set(1, 2, 3)
"#,
    );
    assert_eq!(json["listPlusSet"], true);
    assert_eq!(json["setPlusList"], true);
    let msg = eval_fails("x = List(1) + new Listing { 2 }");
    assert!(msg.contains("Operator `+` is not defined for operand types `List` and `Listing`."));
}

#[test]
fn amending_a_mapping_typed_value_keeps_its_entries() {
    let json = eval(
        r#"
class C { m: Mapping = Map("a", 1).toMapping() }
bare = new C { m { ["b"] = 2 } }
class D { m: Mapping<String, Int> = Map("a", 1).toMapping() }
typed = new D { m { ["b"] = 2 } }
"#,
    );
    assert_eq!(json["bare"]["m"], serde_json::json!({ "a": 1, "b": 2 }));
    assert_eq!(json["typed"]["m"], serde_json::json!({ "a": 1, "b": 2 }));
}

#[test]
fn bare_mapping_default_is_a_mapping() {
    let json = eval(
        r#"
class C { m: Mapping }
mapping = new C {}.m == new Mapping {}
dynamic = new C {}.m == new Dynamic {}
"#,
    );
    assert_eq!(json["mapping"], true);
    assert_eq!(json["dynamic"], false);
}

#[test]
fn mapping_typed_default_is_a_mapping() {
    let json = eval(
        r#"
class C { m: Mapping<String, Int> }
mapping = new C {}.m == new Mapping {}
dynamic = new C {}.m == new Dynamic {}
"#,
    );
    assert_eq!(json["mapping"], true);
    assert_eq!(json["dynamic"], false);
}

#[test]
fn sets_use_pkl_equality() {
    let json = eval(
        r#"
size = Set(new Dynamic { a = 1 }, new Dynamic { a = 1 }).length
contains = List(new Dynamic { a = 1 }).contains(new Dynamic { a = 1 })
union = Set(1, 2) + Set(2, 3)
"#,
    );
    assert_eq!(json["size"], 1);
    assert_eq!(json["contains"], true);
    assert_eq!(json["union"], serde_json::json!([1, 2, 3]));
}

#[test]
fn negated_listing_element() {
    let json = eval(
        r#"
local function f(x) = x > 1
res { !f(0); -1 }
"#,
    );
    assert_eq!(json["res"], serde_json::json!([true, -1]));
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
`outer` {
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

#[test]
fn untyped_mapping_entry_amendment_keeps_inherited_members() {
    let json = eval(
        r#"
local base = new Mapping { ["check"] { steps { ["alpha"] = 1 } } }
local function identity(m) = m
direct = (base) { ["check"] { steps { ["beta"] = 2 } } }
viaCall = (identity(base)) { ["check"] { steps { ["beta"] = 2 } } }
replaced = (base) { ["check"] = new Dynamic { steps { ["beta"] = 2 } } }
"#,
    );
    for name in ["direct", "viaCall"] {
        assert_eq!(json[name]["check"]["steps"]["alpha"], 1, "{name}: {json}");
        assert_eq!(json[name]["check"]["steps"]["beta"], 2, "{name}: {json}");
    }
    assert!(json["replaced"]["check"]["steps"].get("alpha").is_none());
}

#[test]
fn untyped_mapping_listing_entry_amendment() {
    let json = eval(
        r#"
local base = new Mapping { ["k"] = new Listing { 1 2 } }
appended = (base) { ["k"] { 3 } }
indexed = (base) { ["k"] { [0] = 9 } }
"#,
    );
    assert_eq!(json["appended"]["k"], serde_json::json!([1, 2, 3]));
    assert_eq!(json["indexed"]["k"], serde_json::json!([9, 2]));

    let err = eval_fails(
        r#"
local base = new Mapping { ["k"] = new Listing { 1 2 } }
x = (base) { ["k"] { prop = 3 } }
"#,
    );
    assert!(err.contains("cannot have a property"), "{err}");

    let err = eval_fails(
        "local base = new Mapping { [\"k\"] = new Listing { 1 2 } }\nx = (base) { [\"k\"] { for (n in List(1)) { prop = n } } }\n",
    );
    assert!(
        err.contains("A for-generator cannot generate object properties"),
        "{err}"
    );

    for body in [
        "when (true) { prop = 3 }",
        "when (false) { 3 } else { prop = 3 }",
    ] {
        let err = eval_fails(&format!(
            "local base = new Mapping {{ [\"k\"] = new Listing {{ 1 2 }} }}\nx = (base) {{ [\"k\"] {{ {body} }} }}\n"
        ));
        assert!(err.contains("cannot have a property"), "{body}: {err}");
    }
}

#[test]
fn function_built_mapping_entry_amendment_keeps_inherited_members() {
    let json = eval(
        r#"
class Step { check: String? }
class Hook { steps: Mapping<String, Step> = new {} }
local base = new Mapping<String, Step> { ["alpha"] { check = "a" } }
local function hooksFor(input: Mapping<String, Step>): Mapping<String, Hook> = new {
  ["check"] { steps { ...input } }
}
hooks: Mapping<String, Hook> = (hooksFor(base)) {
  ["check"] { steps { ["beta"] { check = "b" } } }
}
"#,
    );
    assert_eq!(json["hooks"]["check"]["steps"]["alpha"]["check"], "a");
    assert_eq!(json["hooks"]["check"]["steps"]["beta"]["check"], "b");
}

#[test]
fn mapping_entry_bodies_of_listings_keep_their_elements() {
    // The shape of pkl:test's `facts` and `examples`.
    let json = eval(
        r#"
examples: Mapping<String, Listing<Any>> = new {
  ["x"] {
    1 + 1
    "two"
  }
  ["y"] {
    for (i in List(1, 2)) { i }
  }
}
typed: Mapping<String, Listing<Int>> = new { ["a"] { 1; 2 } }
local m: Mapping<String, Listing<Int>> = new { ["a"] { 3 } }
fromLocal = m
explicit = new Mapping<String, Listing<Int>> { ["a"] { 4 } }
amended = (explicit) { ["a"] { 5 } }
untyped = new Mapping { ["a"] { 6 } }
dynamic = new Mapping<String, Dynamic> { ["a"] { 7 } }
dynamicUnion = new Mapping<String, *Dynamic|Int> { ["a"] { 8 } }
typealias DynamicDefault = *Dynamic|Int
dynamicAlias = new Mapping<String, DynamicDefault> { ["a"] { 9 } }
"#,
    );
    assert_eq!(json["examples"]["x"], serde_json::json!([2, "two"]));
    assert_eq!(json["examples"]["y"], serde_json::json!([1, 2]));
    assert_eq!(json["typed"]["a"], serde_json::json!([1, 2]));
    assert_eq!(json["fromLocal"]["a"], serde_json::json!([3]));
    assert_eq!(json["explicit"]["a"], serde_json::json!([4]));
    assert_eq!(json["amended"]["a"], serde_json::json!([4, 5]));
    assert_eq!(json["untyped"]["a"], serde_json::json!([6]));
    assert_eq!(json["dynamic"]["a"], serde_json::json!([7]));
    assert_eq!(json["dynamicUnion"]["a"], serde_json::json!([8]));
    assert_eq!(json["dynamicAlias"]["a"], serde_json::json!([9]));
}

#[test]
fn typed_scalar_mapping_entry_body_is_not_a_listing() {
    let err = eval_fails(r#"x = new Mapping<String, Int> { ["a"] { 1 } }"#);
    assert!(
        err.contains("Object of type `Int` cannot have an element."),
        "{err}"
    );
}

#[test]
fn unselected_dynamic_mapping_union_entry_body_is_not_a_listing() {
    for value_type in ["Dynamic|Int", "*Int|Dynamic"] {
        let err = eval_fails(&format!(
            "x = new Mapping<String, {value_type}> {{ [\"a\"] {{ 1 }} }}"
        ));
        assert!(
            err.contains("cannot have an element"),
            "{value_type}: {err}"
        );
    }
}

#[test]
fn listing_mapping_entries_amend_the_default() {
    let json = eval(
        r#"
withDefault = new Mapping<String, Listing<Int>> { default { 0 }; ["a"] { 1 } }
local base = new Mapping<String, Listing<Int>> { default { 0 } }
inherited = (base) { ["b"] { 2 } }
"#,
    );
    assert_eq!(json["withDefault"]["a"], serde_json::json!([0, 1]));
    assert_eq!(json["inherited"]["b"], serde_json::json!([0, 2]));
}

#[test]
fn listing_mapping_entry_cannot_have_a_property() {
    let msg = eval_fails(r#"x = new Mapping<String, Listing<Int>> { ["a"] { 1; foo = 2 } }"#);
    assert!(
        msg.contains("Object of type `Listing` cannot have a property (other than `default`).")
    );
}

#[test]
fn untyped_mapping_entry_with_generated_property_stays_an_object() {
    let json = eval(r#"x = new Mapping { ["a"] { when (true) { y = 1 } } }"#);
    assert_eq!(json["x"]["a"], serde_json::json!({ "y": 1 }));
}

#[test]
fn new_builds_wrapped_declared_types() {
    let json = eval(
        r#"
typealias LM = Mapping<String, Listing<Int>>
nullable: Mapping<String, Listing<Int>>? = new { ["a"] { 1 } }
aliased: LM = new { ["a"] { 2 } }
constrained: Mapping<String, Listing<Int>>(length > 0) = new { ["a"] { 3 } }
class Foo { a = 1; b = 2 }
local foo: Foo? = new { a = 5 }
fromLocal = foo
"#,
    );
    assert_eq!(json["nullable"]["a"], serde_json::json!([1]));
    assert_eq!(json["aliased"]["a"], serde_json::json!([2]));
    assert_eq!(json["constrained"]["a"], serde_json::json!([3]));
    assert_eq!(json["fromLocal"], serde_json::json!({ "a": 5, "b": 2 }));
}

#[test]
fn new_builds_bare_listing_and_mapping() {
    let json = eval(
        r#"
listing: Listing = new { 1 }
mapping: Mapping = new { ["a"] = 1 }
"#,
    );
    assert_eq!(json["listing"], serde_json::json!([1]));
    assert_eq!(json["mapping"], serde_json::json!({ "a": 1 }));
}

#[test]
fn listing_entries_follow_the_declared_value_type() {
    let json = eval(
        r#"
class Bird { name = "b" }
union = new Mapping<String, *Listing<Int>|Bird> { ["a"] { 1 } }
unionClass = new Mapping<String, Listing<Int>|*Bird> { ["a"] { name = "x" } }
local d = new Mapping { default { 0 }; ["a"] { x = 1 } }
untypedWithListDefault = d["a"].x
"#,
    );
    assert_eq!(json["union"]["a"], serde_json::json!([1]));
    assert_eq!(json["unionClass"]["a"], serde_json::json!({ "name": "x" }));
    assert_eq!(json["untypedWithListDefault"], 1);
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
`out` {
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

#[test]
fn function_methods() {
    let json = eval(
        r#"
local add = (a, b) -> a + b
applied = add.apply(1, 2)
appliedToList = add.applyToList(List(1, 2))
text = add.toString()
"#,
    );
    assert_eq!(json["applied"], 3);
    assert_eq!(json["appliedToList"], 3);
    assert_eq!(json["text"], "new Function2 {}");
}

#[test]
fn function_apply_checks_argument_count() {
    let msg = eval_fails(
        r#"
local add = (a, b) -> a + b
x = add.apply(1, 2, 3)
"#,
    );
    assert!(msg.contains("Expected 2 function arguments but got 3."));
}

#[test]
fn function_apply_to_list_rejects_listing_and_set() {
    let listing = eval_fails(
        r#"
local f = (x) -> x
result = f.applyToList(new Listing { 1 })
"#,
    );
    assert!(listing.contains("Expected value of type `List`, but got type `Listing`."));

    let set = eval_fails(
        r#"
local f = (x) -> x
result = f?.applyToList(Set(1))
"#,
    );
    assert!(set.contains("Expected value of type `List`, but got type `Set`."));

    for call in ["f.applyToList()", "f.applyToList(List(1), List(2))"] {
        let msg = eval_fails(&format!("local f = (x) -> x\nresult = {call}"));
        let expected = if call.ends_with("()") { 0 } else { 2 };
        assert!(
            msg.contains(&format!(
                "Expected 1 function arguments but got {expected}."
            )),
            "{msg}"
        );
    }
}

#[test]
fn module_function_reads_module_properties() {
    let json = eval(
        r#"
x = 2
local addX = (y) -> y + x
result = addX.apply(3)
piped = 3 |> addX
"#,
    );
    assert_eq!(json["result"], 5);
    assert_eq!(json["piped"], 5);
}

#[test]
fn null_safe_function_apply_reads_module_properties() {
    let json = eval(
        r#"
x = 2
local addX = (y) -> y + x
result = addX?.apply(3)
"#,
    );
    assert_eq!(json["result"], 5);
}

#[test]
fn test_callback_reads_module_properties() {
    let json = eval(
        r#"
import "pkl:test"
local f = () -> x
x = 2
result = test.catchOrNull(f) == null
"#,
    );
    assert_eq!(json["result"], true);
}

#[test]
fn pipe_binds_tighter_than_null_coalescing() {
    let json = eval(
        r#"
local add1 = (x) -> x + 1
a = 42 ?? 21 |> add1
b = 21 |> add1 ?? 42
c = 1 + 2 |> add1
"#,
    );
    assert_eq!(json["a"], 42);
    assert_eq!(json["b"], 22);
    assert_eq!(json["c"], 4);
}

#[test]
fn pipe_into_a_non_function_fails() {
    let msg = eval_fails("x = 42 |> 21");
    assert!(msg.contains(
        "Operator `|>` is not defined for operand types `Int` and `Int`. \
         Left operand : 42 Right operand: 21"
    ));
}

#[test]
fn amending_a_function_amends_its_result() {
    let json = eval(
        r#"
hidden make = (a) -> new Dynamic { one = a }
hidden amended = (make) { two = 2 }
result = amended.apply(1)
class X {
  hidden f = () -> new Dynamic { zero = 0 }
  res = f.apply()
}
x = new X { f { amended = true } }
"#,
    );
    assert_eq!(json["result"], serde_json::json!({ "one": 1, "two": 2 }));
    assert_eq!(
        json["x"]["res"],
        serde_json::json!({ "zero": 0, "amended": true })
    );
}

#[test]
fn amending_a_list_fails() {
    let msg = eval_fails(r#"x = (List(1)) { 2 }"#);
    assert!(msg.contains("Cannot instantiate, or amend an instance of, external class `List`."));
}

#[test]
fn reading_an_amended_list_property_fails() {
    let msg = eval_fails(
        r#"
class C { xs = List(1) }
result = (new C { xs { 2 } }).xs
"#,
    );
    assert!(msg.contains("Cannot instantiate, or amend an instance of, external class `List`."));
}

#[test]
fn mixin_amends_its_argument() {
    let json = eval(
        r#"
local m = new Mixin { b = 2 }
result = m.apply(new Dynamic { a = 1 })
"#,
    );
    assert_eq!(json["result"], serde_json::json!({ "a": 1, "b": 2 }));
}

#[test]
fn mapping_mixin_value_infers_new_as_mixin() {
    let json = eval(
        r#"
result = new Mapping<String, Mixin<Dynamic>> {
  ["m"] = new { b = 2 }
}["m"].apply(new Dynamic { a = 1 })
"#,
    );
    assert_eq!(json["result"], serde_json::json!({ "a": 1, "b": 2 }));
}

#[test]
fn hidden_members_and_methods() {
    let json = eval(
        r#"
class Multiplier {
  function mult4(x) = x * 4
  hidden mult = (x, y) -> x * y
}
m = new Multiplier {}
dynamic = new Dynamic { local function f() = 1 }
viaHidden = m.mult.apply(2, 3)
viaMethod = m.mult4(2)
"#,
    );
    assert_eq!(json["viaHidden"], 6);
    assert_eq!(json["viaMethod"], 8);
    // Methods and hidden members are not rendered.
    assert_eq!(json["m"], serde_json::json!({}));
    assert_eq!(json["dynamic"], serde_json::json!({}));
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

#[test]
fn import_local_file() {
    let mut ev = pklr::eval::Evaluator::new();
    // Set base path so relative imports resolve correctly
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
import "helper.pkl"
x = helper.value
"#;
    let path = base.join("test_import.pkl");
    let val = ev.eval_source(src, &path).unwrap();
    let json = val.to_json();
    assert_eq!(json["x"], 42);
}

// ============================================================
// Amends resolution
// ============================================================

#[test]
fn amends_local_file() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
amends "base.pkl"
name = "override"
"#;
    let path = base.join("test_amends.pkl");
    let val = ev.eval_source(src, &path).unwrap();
    let json = val.to_json();
    // name is overridden
    assert_eq!(json["name"], "override");
    // version and enabled are inherited from base
    assert_eq!(json["version"], 1);
    assert_eq!(json["enabled"], true);
}

#[test]
fn amending_module_inherits_property_type_and_default() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for child in [
        fixtures.join("amending_property_default_child.pkl"),
        fixtures.join("amending_property_default_new_child.pkl"),
    ] {
        assert_eq!(
            pklr::eval_to_json(&child).unwrap(),
            serde_json::json!({
                "hooks": {
                    "pre": {
                        "label": "default",
                        "steps": { "one": { "a": 3, "b": 2 } },
                    },
                },
            })
        );
    }
}

#[test]
fn amending_module_inherited_types_remain_lazy_until_requested() {
    let temp = TestTempDir::new("pklr_amending_module_inherited_type_laziness");
    std::fs::write(
        temp.path().join("Base.pkl"),
        r#"
checks: Listing(this.length == 1) = new { 1 }
other = 42
"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join("BodyChild.pkl"),
        r#"
amends "Base.pkl"

checks { 2 }
other = 43
"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join("NewChild.pkl"),
        r#"
amends "Base.pkl"

checks = new { 2 }
other = 43
"#,
    )
    .unwrap();

    for child in ["BodyChild", "NewChild"] {
        let unused = temp.path().join(format!("{child}Unused.pkl"));
        std::fs::write(
            &unused,
            format!("import \"{child}.pkl\" as child\nresult = child.other\n"),
        )
        .unwrap();
        assert_eq!(
            pklr::eval_to_json(&unused).unwrap(),
            serde_json::json!({ "result": 43 })
        );

        let read = temp.path().join(format!("{child}Read.pkl"));
        std::fs::write(
            &read,
            format!("import \"{child}.pkl\" as child\nresult = child.checks\n"),
        )
        .unwrap();
        let error = pklr::eval_to_json(&read).unwrap_err().to_string();
        assert!(
            error.contains("checks"),
            "reading the invalid inherited property should report its declared type for {child}, got {error:?}"
        );
    }
}

#[test]
fn inherited_constraint_failure_invalidates_dependent_reads() {
    let temp = TestTempDir::new("pklr_inherited_constraint_dependent_read");
    std::fs::write(
        temp.path().join("Base.pkl"),
        r#"
hidden checks: Listing(this.length == 1) = new { 1 }
summary = checks.length
"#,
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "amends \"Base.pkl\"\nchecks { 2 }\n").unwrap();
    let error = pklr::eval_to_json(&child).unwrap_err().to_string();
    assert!(
        error.contains("checks") || error.contains("constraint"),
        "dependent reads should report the inherited constraint failure, got {error:?}"
    );
}

#[test]
fn inherited_constraint_errors_stay_lazy_but_poison_dependents() {
    let temp = TestTempDir::new("pklr_inherited_constraint_error_laziness");
    std::fs::write(
        temp.path().join("Base.pkl"),
        r#"
hidden checks: Listing(throw("constraint boom")) = new { 1 }
summary = checks.length
other = 42
"#,
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "amends \"Base.pkl\"\nchecks { 2 }\n").unwrap();

    let unused = temp.path().join("Unused.pkl");
    std::fs::write(
        &unused,
        "import \"Child.pkl\" as child\nresult = child.other\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&unused).unwrap(),
        serde_json::json!({ "result": 42 })
    );

    let read = temp.path().join("Read.pkl");
    std::fs::write(
        &read,
        "import \"Child.pkl\" as child\nresult = child.checks\n",
    )
    .unwrap();
    let dependent = temp.path().join("ReadDependent.pkl");
    std::fs::write(
        &dependent,
        "import \"Child.pkl\" as child\nresult = child.summary\n",
    )
    .unwrap();
    for source in [child.clone(), read, dependent] {
        let error = pklr::eval_to_json(&source).unwrap_err().to_string();
        assert!(
            error.contains("constraint boom"),
            "the constraint failure should surface only when requested, got {error:?}"
        );
    }
}

#[test]
fn inherited_named_constraint_errors_stay_lazy_but_surface_when_read() {
    let temp = TestTempDir::new("pklr_inherited_named_constraint_error_laziness");
    std::fs::write(
        temp.path().join("Base.pkl"),
        r#"
local function invalid(_) = throw("indirect constraint boom")
hidden checks: Listing(invalid) = new { 1 }
other = 42
"#,
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "amends \"Base.pkl\"\nchecks { 2 }\n").unwrap();

    let unused = temp.path().join("Unused.pkl");
    std::fs::write(
        &unused,
        "import \"Child.pkl\" as child\nresult = child.other\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&unused).unwrap(),
        serde_json::json!({ "result": 42 })
    );

    let read = temp.path().join("Read.pkl");
    std::fs::write(
        &read,
        "import \"Child.pkl\" as child\nresult = child.checks\n",
    )
    .unwrap();
    let error = pklr::eval_to_json(&read).unwrap_err().to_string();
    assert!(
        error.contains("indirect constraint boom"),
        "the indirect constraint failure should surface when requested, got {error:?}"
    );
}

#[test]
fn amending_module_inherited_mapping_value_types_remain_lazy_until_requested() {
    let temp = TestTempDir::new("pklr_amending_module_inherited_mapping_types");
    std::fs::write(
        temp.path().join("Base.pkl"),
        r#"
typedMap: Mapping<String, Int> = new { ["base"] = 1 }
other = 42
"#,
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        r#"
amends "Base.pkl"

typedMap { ["bad"] = "wrong" }
"#,
    )
    .unwrap();

    let unused = temp.path().join("Unused.pkl");
    std::fs::write(
        &unused,
        "import \"Child.pkl\" as child\nresult = child.other\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&unused).unwrap(),
        serde_json::json!({ "result": 42 })
    );

    let direct = pklr::eval_to_json(&child);
    assert!(
        direct.is_err(),
        "rendering the invalid inherited Mapping should fail, got {direct:?}"
    );

    let read = temp.path().join("Read.pkl");
    std::fs::write(
        &read,
        "import \"Child.pkl\" as child\nresult = child.typedMap\n",
    )
    .unwrap();
    let read = pklr::eval_to_json(&read);
    assert!(
        read.is_err(),
        "reading the invalid inherited Mapping should fail, got {read:?}"
    );
}

#[test]
fn amending_module_inherited_mapping_checks_keys_and_late_values() {
    let temp = TestTempDir::new("pklr_amending_module_inherited_mapping_key_and_late_types");
    std::fs::write(
        temp.path().join("KeyBase.pkl"),
        "typed: Mapping<String, Int> = new {}\n",
    )
    .unwrap();
    let bad_key = temp.path().join("BadKey.pkl");
    std::fs::write(&bad_key, "amends \"KeyBase.pkl\"\ntyped { [1] = 1 }\n").unwrap();
    assert!(
        pklr::eval_to_json(&bad_key).is_err(),
        "an inherited Mapping key outside its declaration should fail"
    );

    std::fs::write(
        temp.path().join("LateBase.pkl"),
        r#"
source = 1
value = source
typed: Mapping<String, Int> = new {}
other = 42
"#,
    )
    .unwrap();
    let late = temp.path().join("Late.pkl");
    std::fs::write(
        &late,
        r#"
amends "LateBase.pkl"
source = "wrong"
typed = new { ["x"] = value }
"#,
    )
    .unwrap();

    let unused = temp.path().join("LateUnused.pkl");
    std::fs::write(
        &unused,
        "import \"Late.pkl\" as child\nresult = child.other\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&unused).unwrap(),
        serde_json::json!({ "result": 42 })
    );
    assert!(
        pklr::eval_to_json(&late).is_err(),
        "a late-recomputed inherited Mapping value outside its declaration should fail"
    );
    let read = temp.path().join("LateRead.pkl");
    std::fs::write(
        &read,
        "import \"Late.pkl\" as child\nresult = child.typed\n",
    )
    .unwrap();
    assert!(
        pklr::eval_to_json(&read).is_err(),
        "reading the late-invalid inherited Mapping should fail"
    );
}

#[test]
fn typed_mapping_defaults_recompute_dependents_after_an_override() {
    let temp = TestTempDir::new("pklr_typed_mapping_default_dependencies");
    std::fs::write(
        temp.path().join("Base.pkl"),
        r#"
class Item {
  value: Int = 1
  doubled: Int = value * 2
}
items: Mapping<String, Item> = new {}
"#,
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        r#"
amends "Base.pkl"
items { ["updated"] { value = 3 } }
"#,
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&child).unwrap(),
        serde_json::json!({
            "items": { "updated": { "value": 3, "doubled": 6 } },
        })
    );
}

#[test]
fn amending_module_defaults_cover_aliases_nullable_listings_and_multiple_levels() {
    let temp = TestTempDir::new("pklr_amending_module_inherited_default_forms");
    std::fs::write(
        temp.path().join("AliasBase.pkl"),
        r#"
class Step {
  value: Int = 1
  label: String = "base"
}
typealias StepMap = Mapping<String, Step>
typealias OptionalStepMap = StepMap?

steps: OptionalStepMap = new { ["base"] {} }
"#,
    )
    .unwrap();
    let alias_child = temp.path().join("AliasChild.pkl");
    std::fs::write(
        &alias_child,
        r#"
amends "AliasBase.pkl"

steps { ["child"] { value = 2 } }
"#,
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&alias_child).unwrap(),
        serde_json::json!({
            "steps": {
                "base": { "value": 1, "label": "base" },
                "child": { "value": 2, "label": "base" },
            },
        })
    );

    std::fs::write(
        temp.path().join("ListingBase.pkl"),
        "values: Listing<Int> = new { 1; 2 }\n",
    )
    .unwrap();
    for (child, source) in [
        (
            "ListingBodyChild.pkl",
            "amends \"ListingBase.pkl\"\nvalues { 3 }\n",
        ),
        (
            "ListingNewChild.pkl",
            "amends \"ListingBase.pkl\"\nvalues = new { 3 }\n",
        ),
    ] {
        let child = temp.path().join(child);
        std::fs::write(&child, source).unwrap();
        assert_eq!(
            pklr::eval_to_json(&child).unwrap(),
            serde_json::json!({ "values": [1, 2, 3] })
        );
    }

    std::fs::write(
        temp.path().join("Base.pkl"),
        r#"
class Step {
  value: Int = 1
  label: String = "base"
}

steps: Mapping<String, Step> = new { ["base"] {} }
"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Mid.pkl"),
        r#"
amends "Base.pkl"

steps { ["mid"] { value = 2 } }
"#,
    )
    .unwrap();
    let leaf = temp.path().join("Leaf.pkl");
    std::fs::write(
        &leaf,
        r#"
amends "Mid.pkl"

steps { ["leaf"] { value = 3 } }
"#,
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&leaf).unwrap(),
        serde_json::json!({
            "steps": {
                "base": { "value": 1, "label": "base" },
                "mid": { "value": 2, "label": "base" },
                "leaf": { "value": 3, "label": "base" },
            },
        })
    );
}

#[test]
fn amends_strips_inherited_class_definitions() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
amends "base_with_class.pkl"
name = "override"
"#;
    let path = base.join("test_amends_class.pkl");
    let val = ev.eval_source(src, &path).unwrap();
    let json = val.to_json();
    assert_eq!(json["name"], "override");
    assert!(
        json.get("Script").is_none(),
        "inherited class 'Script' should be stripped from amends output, got: {json}"
    );
}

#[test]
fn extends_strips_inherited_class_definitions() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_with_class.pkl"
name = "child"
"#;
    let path = base.join("test_extends_class.pkl");
    let val = ev.eval_source(src, &path).unwrap();
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

#[test]
fn circular_import_does_not_loop() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let path = base.join("circular_a.pkl");
    let val = ev.eval_file(&path).unwrap();
    let json = val.to_json();
    assert_eq!(json["a_value"], "from_a");
    // b_ref resolves to from_b via circular_b.pkl
    assert_eq!(json["b_ref"], "from_b");
}

#[test]
fn partial_imports_keep_circular_placeholder() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["result"], "from_a");
}

// ============================================================
// Static member checks
// ============================================================

#[test]
fn duplicate_member_definitions_are_rejected() {
    for (src, name) in [
        ("foo = 1\nfoo = 2\n", "foo"),
        ("foo = 1\nlocal foo = 2\n", "foo"),
        ("function f() = 1\nfunction f(x) = x\n", "f"),
        ("class C\nC = 1\n", "C"),
        ("typealias T = Int\nT = 1\n", "T"),
        ("import \"pkl:test\"\ntest = 1\n", "test"),
        ("class C {\n  a: Int\n  a: String\n}\n", "a"),
        ("obj {\n  a = 1\n  a = 2\n}\n", "a"),
        ("obj {\n  a { b = 1 }\n  a { c = 1 }\n}\n", "a"),
        (
            "m = new Mapping {\n  [\"k\"] = 1\n  [\"k\"] = 2\n}\n",
            "\"k\"",
        ),
        ("m = new Mapping {\n  [1] = 1\n  [1] = 2\n}\n", "1"),
        ("xs = List(1)\nobj {\n  for (i, i in xs) { i }\n}\n", "i"),
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains(&format!("Duplicate definition of member `{name}`")),
            "{src}: {err}"
        );
    }
}

#[test]
fn members_in_separate_namespaces_are_not_duplicates() {
    // Properties and methods have separate namespaces, and object bodies
    // keep local properties, non-local properties and entries apart.
    let src = r#"
function foo() = 1
foo = 5
class C {
  bar: Int = 3
  function bar() = 2
}
obj {
  a = 1
  local a = 2
  local function a() = 4
  ["a"] = 3
}
"#;
    pklr::parser::parse(&pklr::lexer::lex(src).unwrap()).unwrap();

    // Duplicates a generator adds are only found when it runs.
    let json = eval("gen {\n  a = 1\n  when (false) { a = 2 }\n}\n");
    assert_eq!(json["gen"]["a"], 1);

    // Variance markers are not type parameter names.
    let json = eval("typealias P<out A, out B> = List<A|B>\nx: P<Int, Int> = List(1, 2)\n");
    assert_eq!(json["x"], serde_json::json!([1, 2]));
    let json = eval("gen {\n  a = 1\n  when (false) { a = 2 }\n}\n");
    assert_eq!(json["gen"]["a"], 1);
}

#[test]
fn invalid_modifiers_are_rejected() {
    for (src, message) in [
        (
            "fixed module foo\n",
            "Modifier `fixed` is not applicable to modules.",
        ),
        (
            "hidden class Foo\n",
            "Modifier `hidden` is not applicable to classes.",
        ),
        (
            "abstract typealias Foo = Int\n",
            "Modifier `abstract` is not applicable to type aliases.",
        ),
        (
            "open function foo() = 1\n",
            "Modifier `open` is not applicable to methods.",
        ),
        (
            "open foo: Int = 1\n",
            "Modifier `open` is not applicable to properties.",
        ),
        (
            "class Foo {\n  open function f() = 1\n}\n",
            "Modifier `open` is not applicable to methods.",
        ),
        (
            "foo {\n  abstract bar = 1\n}\n",
            "Modifier `abstract` is not applicable to object members.",
        ),
        (
            "foo {\n  fixed bar = 1\n}\n",
            "Modifier `fixed` is not applicable to object members.",
        ),
        (
            "foo = new Dynamic {\n  const bar = 1\n}\n",
            "Modifier `const` can only be applied to object members that are also `local`.",
        ),
        (
            "external function foo()\n",
            "External members can only be defined by standard library modules.",
        ),
        (
            "class Foo {\n  external bar: String\n}\n",
            "External members can only be defined by standard library modules.",
        ),
        (
            "local hidden name: String = \"\"\n",
            "Modifier `hidden` is redundant here; just use `local`.",
        ),
        (
            "local fixed name: String = \"\"\n",
            "Modifier `fixed` is redundant here; just use `local`.",
        ),
        (
            "abstract open class Person\n",
            "Modifier `open` is redundant here; just use `abstract`.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
}

#[test]
fn invalid_member_definitions_are_rejected() {
    for (src, message) in [
        ("local x: Int\n", "Missing property value."),
        (
            "class Box<A> {\n  element: A\n}\n",
            "Only standard library members can have type parameters.",
        ),
        (
            "local function f<T>(x: T) = x\n",
            "Only standard library members can have type parameters.",
        ),
        (
            "typealias Pair<A, A> = List<A>\n",
            "Duplicate type parameter `A`.",
        ),
        (
            "local typealias Pair<A, A> = List<A>\nx = 1\n",
            "Duplicate type parameter `A`.",
        ),
        (
            "typealias Pair<out A, out A> = List<A>\n",
            "Duplicate type parameter `A`.",
        ),
        (
            "obj {\n  for (n in List(1)) { foo = n }\n}\n",
            "A for-generator cannot generate object properties (only entries and elements).",
        ),
        (
            "obj {\n  for (n in List(1)) { when (true) { local foo = n } }\n}\n",
            "A for-generator cannot generate object properties (only entries and elements).",
        ),
        (
            "obj {\n  for (n in List(1)) { local function f() = n }\n}\n",
            "A for-generator cannot generate object methods (only entries and elements).",
        ),
        (
            "obj {\n  function f() = 1\n}\n",
            "Method needs a `local` modifier because it is defined in an object, not a class.",
        ),
        (
            "obj {\n  x: Int = 1\n}\n",
            "A non-local object property cannot have a type annotation.",
        ),
        (
            "obj {\n  local x { a = 1 }\n}\n",
            "A local property definition cannot be amended.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
}

#[test]
fn amending_module_member_rules() {
    let temp = TestTempDir::new("pklr_test_amending_module_member_rules");
    let dir = temp.path();
    std::fs::write(dir.join("base.pkl"), "name: String = \"\"\n").unwrap();
    let path = dir.join("main.pkl");
    for (src, message) in [
        (
            "amends \"base.pkl\"\nname: String = \"x\"\n",
            "A non-local object property cannot have a type annotation.",
        ),
        (
            "amends \"base.pkl\"\nfunction foo() = 1\n",
            "Method needs a `local` modifier because it is defined in an object, not a class.",
        ),
        (
            "amends \"base.pkl\"\nclass Other\n",
            "Class needs a `local` modifier.",
        ),
        (
            "amends \"base.pkl\"\ntypealias Other = Int\n",
            "Type alias needs a `local` modifier.",
        ),
        (
            "amends \"base.pkl\"\nlocal object {\n  a = 1\n}\n",
            "A local property definition cannot be amended.",
        ),
        (
            "amends \"base.pkl\"\nhidden name = \"x\"\n",
            "Modifier `hidden` is not applicable to object members.",
        ),
        (
            "open module foo\namends \"base.pkl\"\n",
            "Modifier `open` is not applicable to modules that amend another module.",
        ),
    ] {
        std::fs::write(&path, src).unwrap();
        let err = pklr::eval_to_json(&path).unwrap_err().to_string();
        assert!(err.contains(message), "{src}: {err}");
    }
    std::fs::write(
        &path,
        "amends \"base.pkl\"\nlocal suffix: String = \"!\"\nlocal function f(s) = s + suffix\nlocal class C\nname = f(\"x\")\n",
    )
    .unwrap();
    assert_eq!(pklr::eval_to_json(&path).unwrap()["name"], "x!");
}

// ============================================================
// Type definition checks
// ============================================================

#[test]
fn invalid_type_definitions_are_rejected() {
    for (src, message) in [
        (
            "typealias Foo = List<Foo>\n",
            "Type alias definitions must not be cyclic.",
        ),
        (
            "typealias Foo = List<Bar>\ntypealias Bar = Set<Baz>\ntypealias Baz = Map<String, Foo>\n",
            "Type alias definitions must not be cyclic.",
        ),
        (
            "foo: *Int|*String = 1\n",
            "A type union cannot have more than one default type.",
        ),
        (
            "foo: *\"foo\" = \"foo\"\n",
            "Only type unions can have a default marker (*).",
        ),
        (
            "local x = new {\n  y = 1\n}\nlisting = new x.y.Listing {}\n",
            "Invalid type name `x.y.Listing`.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
    // An alias may use its own type parameter names and refer to other
    // aliases without a cycle.
    let json = eval(
        "typealias Pair<Foo> = List<Foo>\ntypealias Foo = Pair<Int>\nx: Foo = List(1)\ny: *Int|String = 1\n",
    );
    assert_eq!(json["x"], serde_json::json!([1]));
}

#[test]
fn annotations_must_name_annotation_classes() {
    for (src, message) in [
        (
            "class NoAnn {}\n@NoAnn\nfoo = \"hi\"\n",
            "Expected an annotation class.",
        ),
        (
            "@Mapping {\n  a = \"hi\"\n}\nfoo: String = \"hi\"\n",
            "Expected an annotation class.",
        ),
        (
            "@Int foo: String = \"hi\"\n",
            "Expected an annotation class.",
        ),
        ("@Function0 foo = 1\n", "Expected an annotation class."),
        ("@Int function foo() = 1\n", "Expected an annotation class."),
        (
            "integer: Int = 1\n@integer foo: String = \"hi\"\n",
            "Expected `integer` to be a type, but it is not.",
        ),
        (
            "class MyAnn extends Annotation {}\nlocal myAnn = MyAnn\n@myAnn\nfoo = 1\n",
            "Expected `myAnn` to be a type, but it is not.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
    let json = eval(
        "open class MyAnn extends Annotation {}\nclass Sub extends MyAnn {}\n@MyAnn\na = 1\n@Sub\nb = 2\n@Deprecated\nc = 3\n",
    );
    assert_eq!(json["a"], 1);
    assert_eq!(json["c"], 3);
}

#[test]
fn type_alias_cycle_check_follows_type_structure() {
    // A cycle through a default-marked union is still a cycle.
    let err = eval_fails("typealias Foo = *Bar|Int\ntypealias Bar = Listing<Foo>\nx = 1\n");
    assert!(
        err.contains("Type alias definitions must not be cyclic."),
        "{err}"
    );
    // String literal types and constraint expressions name no aliases, and
    // pkl only resolves a local alias when something uses it.
    let json = eval(
        "typealias Tag = \"Tag\"\nt: Tag = \"Tag\"\ntypealias N = Int(this is N)\nlocal typealias L = List<L>\nx = 1\n",
    );
    assert_eq!(json["t"], "Tag");
    // A long chain of aliases is checked without deep recursion.
    let mut src = String::new();
    for i in 0..5000 {
        src.push_str(&format!("typealias A{i} = A{}\n", i + 1));
    }
    src.push_str("typealias A5000 = Int\nx: A0 = 1\n");
    assert_eq!(eval(&src)["x"], 1);
}

#[test]
fn annotation_checks_follow_aliases_and_trust_other_modules() {
    let err = eval_fails("typealias I = Int\n@I\nfoo = 1\n");
    assert!(err.contains("Expected an annotation class."), "{err}");
    let json = eval("open class MyAnn extends Annotation {}\ntypealias A = MyAnn\n@A\nfoo = 1\n");
    assert_eq!(json["foo"], 1);

    let temp = TestTempDir::new("pklr_test_imported_annotation_parent");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        "open class BaseAnn extends Annotation {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"Lib.pkl\"\nclass Sub extends Lib.BaseAnn {}\n@Sub\nfoo = 1\n",
    )
    .unwrap();
    assert_eq!(pklr::eval_to_json(&dir.join("main.pkl")).unwrap()["foo"], 1);
}

#[test]
fn annotation_aliases_must_name_annotation_classes() {
    let err =
        eval_fails("open class MyAnn extends Annotation {}\ntypealias A = MyAnn?\n@A\nfoo = 1\n");
    assert!(err.contains("Expected an annotation class."), "{err}");
    let json = eval(
        "open class MyAnn extends Annotation {}\ntypealias B = MyAnn\ntypealias A = B\n@A\nfoo = 1\n",
    );
    assert_eq!(json["foo"], 1);
}

#[test]
fn type_alias_cycle_check_uses_qualified_names() {
    let temp = TestTempDir::new("pklr_test_alias_qualified_refs");
    let dir = temp.path();
    std::fs::write(dir.join("Lib.pkl"), "class Config { a = 1 }\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"Lib.pkl\"\ntypealias Config = Listing<Lib.Config>\nx: Config = new Listing<Lib.Config> {}\ny = 1\n",
    )
    .unwrap();
    assert_eq!(pklr::eval_to_json(&dir.join("main.pkl")).unwrap()["y"], 1);
}

#[test]
fn trailing_semicolons_before_end_of_file() {
    let json = eval("x = 1;\ny { a = 1; };");
    assert_eq!(json, serde_json::json!({"x": 1, "y": {"a": 1}}));
    let json = eval("x = \"\\(1;)\"");
    assert_eq!(json["x"], "1");
}

#[test]
fn function_types_are_accepted_in_type_positions() {
    let json = eval(
        "zero: () -> Int = () -> 1\none: (Int,) -> String = (x) -> x.toString()\ntwo: (Int, String) -> String = (x, y) -> y\nnested: ((Int) -> Int) -> Int = (f) -> f(2)\nresult = List(zero(), one(3), two(0, \"x\"), nested((x) -> x + 1))",
    );
    assert_eq!(json["result"], serde_json::json!([1, "3", "x", 3]));
}

#[test]
fn function_types_support_aliases_unions_generics_and_method_returns() {
    let json = eval(
        r#"
typealias Unary = (Int,) -> Int
typealias OptionalUnary = Unary|Null
local increment: Unary = (x) -> x + 1
local maybe: OptionalUnary = increment
local callbacks: Listing<(Int) -> String> = new Listing { (x) -> x.toString() }
function makeFormatter(): (Int) -> String = (x) -> x.toString()
function applyTwice(f: (Int) -> Int): Int = f.apply(f.apply(1))
result = List(
  applyTwice(increment),
  callbacks.length,
  makeFormatter().apply(5),
  ((f: (Int) -> Int) -> f.apply(6)).apply(increment),
)
"#,
    );
    assert_eq!(json["result"], serde_json::json!([3, 1, "5", 7]));
}

#[test]
fn function_types_reject_non_lambdas_and_invalid_parenthesized_forms() {
    let err = eval_fails("bad: (Int) -> Int = 1");
    assert!(err.contains("Function1"), "{err}");

    for src in [
        "bad: () = () -> 1",
        "bad: (Int, String) = (x, y) -> x",
        "bad: (Int,,) -> Int = (x) -> x",
    ] {
        assert!(!eval_fails(src).is_empty(), "{src}");
    }
}

#[test]
fn const_members_only_read_const_members() {
    let err = eval_fails("x = 1\nconst y = x + 1\n");
    assert!(err.contains("Cannot reference property `x`"), "{err}");
    let err = eval_fails("x = 1\nconst y { z = x }\n");
    assert!(err.contains("Cannot reference property `x`"), "{err}");
    let err = eval_fails("x = 1\nconst y: Int(this == x) = 1\n");
    assert!(err.contains("Cannot reference property `x`"), "{err}");
    let err = eval_fails("x = 1\nconst y = 1 is Int(this == x)\n");
    assert!(err.contains("Cannot reference property `x`"), "{err}");
    let err = eval_fails("x = 1\ncontainer { local const y = x }\n");
    assert!(err.contains("Cannot reference property `x`"), "{err}");
    let err = eval_fails("x = 1\nclass Child extends Missing { const y = module.x }\n");
    assert!(err.contains("Cannot reference property `x`"), "{err}");
    let json = eval("x = 5\nconst y = let (x = 2) x + 1\nconst z = 1\nconst w = z + 1\n");
    assert_eq!(json["y"], 3);
    assert_eq!(json["w"], 2);
}

#[test]
fn lambda_calls_require_the_declared_arity() {
    for src in [
        "f = (x) -> x\nresult = f()",
        "obj { f = (x) -> x }\nresult = obj.f()",
        "result = List(1).map((x, y) -> x)",
    ] {
        let err = eval_fails(src);
        assert!(err.contains("Expected"), "{src}: {err}");
        assert!(err.contains("function arguments"), "{src}: {err}");
    }
}

#[test]
fn member_predicates_are_reported_as_unsupported() {
    for src in [
        "base { 1 2 }\nres = (base) { [[this == 1]] = 3 }\n",
        "base = new Listing { 1 2 }\nres = (base) { [[this == 1]] = 3 }\n",
        "base = new Mapping { [\"a\"] = 1 }\nres = (base) { [[this == 1]] = 3 }\n",
    ] {
        let err = eval_fails(src);
        assert!(err.contains("member predicates"), "{src}: {err}");
    }
}
