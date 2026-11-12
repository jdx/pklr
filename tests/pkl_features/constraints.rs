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

// Expectations compared with `pkl eval` 0.32.2.
#[test]
fn constrained_generic_still_checks_its_element_constraints() {
    let json = eval(
        r#"
import "pkl:test"
class Y {
  function l(l): List<String(!isEmpty)>(!isEmpty) = l
  function map(map): Map<String(!isEmpty), Int(isPositive)>(!isEmpty) = map
}
local y = new Y {}
r1 = test.catch(() -> y.l(List("1", "")))
r2 = test.catch(() -> y.map(Map("1", 1, "2", -2)))
r3 = y.l(List("1"))
"#,
    );
    assert_eq!(
        json["r1"],
        "Type constraint `!isEmpty` violated. Value: \"\""
    );
    assert_eq!(
        json["r2"],
        "Type constraint `isPositive` violated. Value: -2"
    );
    assert_eq!(json["r3"], serde_json::json!(["1"]));
    let error = eval_fails("a: List<String(!isEmpty)>(!isEmpty) = 5\n");
    assert!(
        error.contains("Expected value of type `List`, but got type `Int`."),
        "{error}"
    );
}

#[test]
fn constrained_generic_keeps_element_constraints_in_unions_and_nullables() {
    let err = eval_fails("a: List<String(!isEmpty)|Int>(!isEmpty) = List(\"\")\n");
    assert!(err.contains("`!isEmpty`"), "{err}");
    let err = eval_fails("a: List<String(!isEmpty)>?(true) = List(\"\")\n");
    assert!(err.contains("`!isEmpty`"), "{err}");
    assert_eq!(
        eval(
            "a: List<String(!isEmpty)|Int>(!isEmpty) = List(\"x\", 3)\nb: List<String(!isEmpty)>?(true) = null\n"
        ),
        serde_json::json!({"a": ["x", 3], "b": null})
    );
}

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
