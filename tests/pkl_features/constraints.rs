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
fn null_default_is_amended() {
    let src = "class Foo { x: Int = 1; y: Int = 2 }\nclass Bar { foo: Foo? = Null(new Foo { x = 5 }) }\nb = new Bar { foo { y = 9 } }\n";
    assert_eq!(
        eval(src),
        serde_json::json!({"b": {"foo": {"x": 5, "y": 9}}})
    );
}
