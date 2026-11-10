//! `Any.getClass()` and the `Class` values it returns. Expected results are
//! what `pkl` 0.32 prints for the same snippets.

use super::*;
use serde_json::json;

#[test]
fn get_class_simple_name_of_class_instance_and_int() {
    // The snippet from jdx/pklr#380.
    let result = eval(
        r#"
class Foo {}
a = new Foo {}.getClass().simpleName
b = 1.getClass().simpleName
"#,
    );
    assert_eq!(result, json!({"a": "Foo", "b": "Int"}));
}

#[test]
fn get_class_simple_name_of_each_builtin_type() {
    let result = eval(
        r#"
r {
  null_ = null.getClass().simpleName
  bool = true.getClass().simpleName
  int = 1.getClass().simpleName
  float = 1.5.getClass().simpleName
  string = "s".getClass().simpleName
  duration = 1.s.getClass().simpleName
  dataSize = 1.kb.getClass().simpleName
  regex = Regex("a").getClass().simpleName
  list = List(1).getClass().simpleName
  set = Set(1).getClass().simpleName
  listing = new Listing { 1 }.getClass().simpleName
  mapping = new Mapping { ["a"] = 1 }.getClass().simpleName
  dynamic = new Dynamic { a = 1 }.getClass().simpleName
  lambda = ((x) -> x).getClass().simpleName
  class_ = 1.getClass().getClass().simpleName
}
"#,
    );
    assert_eq!(
        result["r"],
        json!({
            "null_": "Null", "bool": "Boolean", "int": "Int", "float": "Float",
            "string": "String", "duration": "Duration", "dataSize": "DataSize",
            "regex": "Regex", "list": "List", "set": "Set", "listing": "Listing",
            "mapping": "Mapping", "dynamic": "Dynamic", "lambda": "Function1",
            "class_": "Class"
        })
    );
}

#[test]
fn get_class_equality_is_by_class() {
    let result = eval(
        r#"
open class Foo {}
class Bar extends Foo {}
same_instances = new Foo {}.getClass() == new Foo {}.getClass()
instance_vs_class = new Foo {}.getClass() == Foo
subclass = new Bar {}.getClass() == new Foo {}.getClass()
ints = 1.getClass() == 2.getClass()
int_vs_string = 1.getClass() == "a".getClass()
not_equal = new Foo {}.getClass() != 1.getClass()
list_vs_set = List(1).getClass() == Set(1).getClass()
"#,
    );
    assert_eq!(
        result,
        json!({
            "same_instances": true, "instance_vs_class": true, "subclass": false,
            "ints": true, "int_vs_string": false, "not_equal": true,
            "list_vs_set": false
        })
    );
}

#[test]
fn get_class_to_string() {
    let result = eval(
        r#"
class Foo {}
user = new Foo {}.getClass().toString()
interpolated = "\(new Foo {}.getClass())"
builtin = 1.getClass().toString()
"#,
    );
    assert_eq!(
        result,
        json!({"user": "test#Foo", "interpolated": "test#Foo", "builtin": "Int"})
    );
}

#[test]
fn get_class_compares_with_builtin_class_names() {
    let result = eval(
        r#"
a = 1.getClass() == Int
b = "x".getClass() == Int
c = "x".getClass() == String
d = Int.simpleName
e = new Listing {}.getClass() == Listing
"#,
    );
    assert_eq!(
        result,
        json!({"a": true, "b": false, "c": true, "d": "Int", "e": true})
    );
}

#[test]
fn get_class_of_a_class_value_is_class() {
    let result = eval(
        r#"
class Foo {}
a = Foo.getClass().simpleName
b = Foo.simpleName
"#,
    );
    assert_eq!(result, json!({"a": "Class", "b": "Foo"}));
}

#[test]
fn get_class_null_safe_call_on_null_is_null() {
    let result = eval("a = null\nb = a?.getClass()\nc = 1?.getClass().simpleName");
    assert_eq!(result["b"], json!(null));
    assert_eq!(result["c"], json!("Int"));
}

#[test]
fn class_has_no_properties_but_simple_name() {
    let error = eval_fails("a = 1.getClass().foo");
    assert!(
        error.contains("Cannot find property `foo` in object of type `Class`"),
        "{error}"
    );
}

#[test]
fn get_class_on_map_and_module_is_a_clear_error() {
    // pklr cannot tell a `Map` or a computed `Dynamic` from each other, nor
    // a module from a `Dynamic`; it refuses instead of answering wrongly.
    for src in ["a = Map(1, 2).getClass()", "a = module.getClass()"] {
        let error = eval_fails(src);
        assert!(error.contains("getClass() is not supported on"), "{error}");
    }
}

#[test]
fn get_class_takes_no_arguments() {
    let error = eval_fails("a = 1.getClass(1)");
    assert!(error.contains("unknown method 'getClass'"), "{error}");
}
