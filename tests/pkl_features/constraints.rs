use super::*;

// The error of a violated type constraint names the constraint that failed
// and shows the value, as `pkl eval` 0.32.2 does, which is what
// `test.catch` returns. Each expectation was compared with `pkl eval`.

#[test]
fn violated_constraints_are_reported_by_their_source_text() {
    let json = eval(
        r#"
import "pkl:test"
class Gauge { min: Int; max: Int(this >= min, this < 100) }
a = test.catch(() -> new Gauge { min = 4; max = 3 }.max)
b = test.catch(() -> new Gauge { min = 4; max = 100 }.max)
class P { name: String(length.isBetween(3, 5)) }
c = test.catch(() -> new P { name = "ab" }.name)
class A { street: String; city: String }
class Q { a: A(street.endsWith("St.")) }
d = test.catch(() -> new Q { a { street = "Blvd."; city = "x" } }.a)
g = test.catch(() -> new Gauge { min = 1; max = "x" }.max)
"#,
    );
    assert_eq!(
        json["a"],
        "Type constraint `this >= min` violated. Value: 3"
    );
    assert_eq!(
        json["b"],
        "Type constraint `this < 100` violated. Value: 100"
    );
    assert_eq!(
        json["c"],
        "Type constraint `length.isBetween(3, 5)` violated. Value: \"ab\""
    );
    assert_eq!(
        json["d"],
        "Type constraint `street.endsWith(\"St.\")` violated. Value: new A { street = \"Blvd.\"; city = ? }"
    );
    assert_eq!(
        json["g"],
        "Expected value of type `Int`, but got type `String`. Value: \"x\""
    );
}

#[test]
fn a_constraint_that_is_not_a_boolean_or_predicate_is_an_error() {
    let json = eval(
        r#"
import "pkl:test"
class X { a: Int("not a boolean"); b: Int((x) -> "no") }
e = test.catch(() -> new X { a = 1 }.a)
f = test.catch(() -> new X { b = 1 }.b)
"#,
    );
    assert_eq!(
        json["e"],
        "Expected value of type `Boolean` or `Function`, but got type `String`. Value: \"not a boolean\""
    );
    assert_eq!(
        json["f"],
        "Expected value of type `Boolean`, but got type `String`. Value: \"no\""
    );
}

#[test]
fn an_uncaught_property_error_names_the_property() {
    let error = eval_fails("class P { age: Int(this > 0) }\nx = new P { age = -1 }\n");
    assert!(
        error.contains("Type constraint `this > 0` violated."),
        "{error}"
    );
    assert!(error.contains("property 'age'"), "{error}");
}

#[test]
fn map_entries_are_pairs_with_key_and_value() {
    let json = eval(r#"m = Map("a", 1, "b", 2).entries.map((p) -> p.key + p.value.toString())"#);
    assert_eq!(json["m"], serde_json::json!(["a1", "b2"]));
}

#[test]
fn map_entries_keep_compound_keys() {
    let json = eval(r#"k = Map(List(1, 2), "x").entries.first.key.length"#);
    assert_eq!(json["k"], 2);
}

#[test]
fn a_union_branch_that_passes_hides_an_earlier_violation() {
    let json = eval(
        r#"
import "pkl:test"
class X { xs: List<Int(this > 0)|Int> }
a = test.catch(() -> new X { xs = List(-1, "x") }.xs)
"#,
    );
    let message = json["a"].as_str().unwrap();
    assert!(
        !message.contains("this > 0` violated. Value: -1"),
        "{message}"
    );
}

#[test]
fn every_constraint_of_an_alias_use_is_checked_on_its_own() {
    let json = eval(
        r#"
import "pkl:test"
typealias Positive = Int(this > 0)
class X { n: Positive(this < 10, this != 5) }
a = test.catch(() -> new X { n = 5 }.n)
b = test.catch(() -> new X { n = -1 }.n)
"#,
    );
    assert_eq!(json["a"], "Type constraint `this != 5` violated. Value: 5");
    assert_eq!(json["b"], "Type constraint `this > 0` violated. Value: -1");
}

#[test]
fn a_predicate_reading_a_missing_property_of_a_dynamic_fails() {
    let error = eval_fails("x = new Dynamic {} is Dynamic((v) -> v.required)\n");
    assert!(error.contains("required"), "{error}");
}

#[test]
fn a_predicate_reading_a_missing_property_of_an_untyped_object_fails() {
    let error = eval_fails("x = new { a = 1 } is Dynamic((v) -> v.required)\n");
    assert!(error.contains("required"), "{error}");
}

#[test]
fn stdlib_aliases_carry_their_constraints() {
    let err = eval_fails("foo: UInt8 = -5\n");
    assert!(
        err.contains("Type constraint `isBetween(0, 255)` violated."),
        "{err}"
    );
    let err = eval_fails("foo: Int16 = 40000\n");
    assert!(err.contains("isBetween(-32768, 32767)"), "{err}");
    let err = eval_fails("foo: UInt = -1\n");
    assert!(err.contains("`isPositive`"), "{err}");
    let err = eval_fails("foo: NonNull = null\n");
    assert!(
        err.contains("Type constraint `!(this is Null)` violated."),
        "{err}"
    );
    let err = eval_fails("foo: Char = \"ab\"\n");
    assert!(err.contains("`length == 1`"), "{err}");
    // A value of the wrong base type is reported as that type.
    let err = eval_fails("foo: Int16 = \"abc\"\n");
    assert!(
        err.contains("Expected value of type `Int`, but got type `String`."),
        "{err}"
    );
    let err = eval_fails("foo: Uri = 42\n");
    assert!(
        err.contains("Expected value of type `String`, but got type `Int`."),
        "{err}"
    );
    let err = eval_fails("class A { x: UInt16 = 65536 }\na = new A {}.x\n");
    assert!(err.contains("isBetween(0, 65535)"), "{err}");
    let json = eval("a: UInt8 = 255\nb: Int8 = -128\nc: Uri = \"x\"\nd: NonNull = 1\n");
    assert_eq!(json["a"], 255);
}

#[test]
fn null_function_returns_null() {
    let json = eval("a = Null(new Dynamic {})\n");
    assert!(json["a"].is_null());
}

#[test]
fn a_marked_nested_union_defaults_to_its_inner_default() {
    let json = eval("b: *(\"a\"|*\"b\")|\"c\"\nc: (\"a\"|*\"b\")|*\"c\"\n");
    assert_eq!(json["b"], "b");
    assert_eq!(json["c"], "c");
}

#[test]
fn a_converter_fills_in_a_nested_undefined_property() {
    let json = eval_with_converters(
        "class Person { name: String; age: Int }\nres = new Person { name = \"pigeon\" }\noutput { renderer = new JsonRenderer { converters { [Person] = (it) -> (it) { age = 40 } } } }\n",
    );
    assert_eq!(json["res"]["age"], 40);
}

#[test]
fn stdlib_alias_with_extra_constraint() {
    assert_eq!(
        eval("a: UInt8(this != 1) = 2\n"),
        serde_json::json!({"a": 2})
    );
    let err = eval_fails("a: UInt8(this != 1) = 1\n");
    assert!(err.contains("`this != 1`"), "{err}");
    let err = eval_fails("local n: UInt8 = -5\na = n\n");
    assert!(err.contains("isBetween(0, 255)"), "{err}");
}

#[test]
fn stdlib_alias_ignores_user_names() {
    // A user class or function of the same name is not the alias's.
    assert_eq!(
        eval("class Int {}\na: UInt8 = 2\n"),
        serde_json::json!({"a": 2})
    );
    let err = eval_fails("local function isBetween(a, b) = true\na: UInt8 = 300\n");
    assert!(err.contains("isBetween(0, 255)"), "{err}");
    let err = eval_fails("class Char {}\na: Char = 1\n");
    assert!(err.contains("type `Char`"), "{err}");
}

#[test]
fn null_default_accepts_a_dynamic_or_listing_reference() {
    let json = eval(
        "local d = new Dynamic { 1 }\nlocal l = new Listing { 1 }\na = Null(d)\nb = Null(l)\n",
    );
    assert!(json["a"].is_null() && json["b"].is_null());
    let err = eval_fails("a = Null(List(1))\n");
    assert!(err.contains("Object | Function<Object>"), "{err}");
}

#[test]
fn an_unrelated_converter_keeps_an_undefined_member_failing() {
    let err = eval_fails(
        "class P { age: Int }\nclass Q { x = 1 }\nres = new P {}\noutput { renderer = new JsonRenderer { converters { [Q] = (it) -> it } } }\n",
    );
    assert!(err.contains("`age`"), "{err}");
}

#[test]
fn null_rejects_a_class_and_names_an_int_seq() {
    let err = eval_fails("a = Null(Int)\n");
    assert!(err.contains("Object | Function<Object>"), "{err}");
    let err = eval_fails("a = Null(IntSeq(1, 3))\n");
    assert!(err.contains("got type `IntSeq`"), "{err}");
}

// Expectations compared with `pkl eval` 0.32.2.
#[test]
fn constraint_reads_local_member_of_inherited_class() {
    let json = eval(
        r#"
import "pkl:test"
open class Base {
  x: Int = 1
  yy: Int(isValid1) = 2
  local isValid1 = (n) -> n > x
}
class D extends Base
r = (new D {}) { yy = 3; x = 2 }
r2 = test.catch(() -> (r) { yy = 0 }.yy)
s: String(List(1).map((it) -> getClass()).single == String) = "abc"
"#,
    );
    assert_eq!(json["r"]["yy"], 3);
    assert_eq!(json["r2"], "Type constraint `isValid1` violated. Value: 0");
    assert_eq!(json["s"], "abc");
}

#[test]
fn list_single_rest_and_or_null_members() {
    let json = eval(
        "l = List(1, 2, 3)\na = l.firstOrNull\nb = l.lastOrNull\nc = l.rest\nd = l.singleOrNull\ne = List(5).single\nf = l.lastIndex\n",
    );
    assert_eq!(json["a"], 1);
    assert_eq!(json["b"], 3);
    assert_eq!(json["c"], serde_json::json!([2, 3]));
    assert!(json["d"].is_null());
    assert_eq!(json["e"], 5);
    assert_eq!(json["f"], 2);
    let error = eval_fails("a = List(1, 2).single\n");
    assert!(
        error.contains("Expected a single-element collection."),
        "{error}"
    );
}
