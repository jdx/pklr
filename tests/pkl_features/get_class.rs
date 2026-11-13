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
fn get_class_on_a_map_is_a_clear_error() {
    // pklr cannot tell a `Map` or a computed `Dynamic` from each other; it
    // refuses instead of answering wrongly.
    let error = eval_fails("a = Map(1, 2).getClass()");
    assert!(error.contains("getClass() is not supported on"), "{error}");
}

#[test]
fn get_class_on_a_module_is_its_module_class() {
    // Pkl: every module class has the simple name `ModuleClass` and prints as
    // the module's name.
    let result = eval(
        r#"
module Foo

class P {}

simple = module.getClass().simpleName
text = module.getClass().toString()
interpolated = "\(module.getClass())"
same = module.getClass() == module.getClass()
isDynamic = module.getClass() == Dynamic
isClass = module.getClass() is Class
isUserClass = module.getClass() == new P {}.getClass()
"#,
    );
    assert_eq!(
        result,
        serde_json::json!({
            "simple": "ModuleClass", "text": "Foo", "interpolated": "Foo",
            "same": true, "isDynamic": false,
            "isClass": true, "isUserClass": false
        })
    );
}

#[test]
fn get_class_takes_no_arguments() {
    let error = eval_fails("a = 1.getClass(1)");
    assert!(error.contains("unknown method 'getClass'"), "{error}");
}

#[test]
fn builtin_class_names_are_class_values() {
    // `Duration`, `DataSize`, `Regex` and `Dynamic` are seeded as markers.
    let result = eval(
        r#"
a = 1.s.getClass() == Duration
b = 1.kb.getClass() == DataSize
c = Regex("a").getClass() == Regex
d = new Dynamic {}.getClass() == Dynamic
e = Regex.simpleName
f = Duration.simpleName
g = "\(DataSize)"
h = Map(Duration, 1)[1.s.getClass()]
"#,
    );
    assert_eq!(
        result,
        json!({"a": true, "b": true, "c": true, "d": true, "e": "Regex",
               "f": "Duration", "g": "DataSize", "h": 1})
    );
}

#[test]
fn builtin_class_names_keep_working_as_constructors_and_types() {
    let result = eval(
        r#"
a = Regex("a+").matchEntire("aa") != null
b = (1.s) is Duration
c = new Dynamic { x = 1 }.x
"#,
    );
    assert_eq!(result, json!({"a": true, "b": true, "c": 1}));
}

#[test]
fn user_binding_shadows_builtin_class_name() {
    let result = eval("local Int = 5\na = Int\nb = 1.getClass().simpleName");
    assert_eq!(result, json!({"a": 5, "b": "Int"}));
}

#[test]
fn simple_name_works_through_null_safe_access() {
    let result = eval(
        r#"
class Foo {}
a = 1.getClass()?.simpleName
b = Foo?.simpleName
c = Duration?.simpleName
"#,
    );
    assert_eq!(result, json!({"a": "Int", "b": "Foo", "c": "Duration"}));
}

#[test]
fn simple_name_is_not_shadowed_by_a_property_named_simple_name() {
    let result = eval(
        r#"
class Bar { simpleName = "inst" }
a = Bar.simpleName
b = Bar?.simpleName
c = new Bar {}.getClass().simpleName
d = new Bar {}.simpleName
"#,
    );
    assert_eq!(
        result,
        json!({"a": "Bar", "b": "Bar", "c": "Bar", "d": "inst"})
    );
}

#[test]
fn class_keys_match_by_identity() {
    let result = eval(
        r#"
class Foo { x = 1 }
local m = Map(Foo, "hit")
a = m[new Foo {}.getClass()]
b = m[Foo]
d = Map(new Foo {}.getClass(), "hit")[Foo]
e = Set(Foo, new Foo {}.getClass()).length
"#,
    );
    assert_eq!(result, json!({"a": "hit", "b": "hit", "d": "hit", "e": 1}));
}

#[test]
fn user_strings_equal_to_a_builtin_name_stay_strings() {
    let result = eval(
        r#"
a = let (Duration = "Duration") Duration == "Duration"
function f(Regex) = Regex.length
b = f("Regex")
local DataSize = "DataSize"
c = DataSize.length
class C { Dynamic = "Dynamic" }
d = new C {}.Dynamic == "Dynamic"
e = (new C {}) { Dynamic = "Dynamic" }.Dynamic.length
f2 = let (Regex = "Regex") Regex + "!"
g = 1.s.getClass() == Duration
h = Regex.simpleName
"#,
    );
    assert_eq!(
        result,
        json!({"a": true, "b": 5, "c": 8, "d": true, "e": 7, "f2": "Regex!",
               "g": true, "h": "Regex"})
    );
}

#[test]
fn user_string_marker_is_kept_inside_lambdas_and_amended_objects() {
    let result = eval(
        r#"
local Duration = "Duration"
a = List(1).map((_) -> Duration)
b = new Dynamic { x = Duration } { y = Duration }
"#,
    );
    assert_eq!(
        result,
        json!({"a": ["Duration"], "b": {"x": "Duration", "y": "Duration"}})
    );
}

#[test]
fn class_values_are_class_and_not_dynamic() {
    // pkl 0.32: a Class value is a `Class`, never a `Dynamic`.
    let result = eval(
        r#"
class Foo {}
r {
  durationIsDynamic = Duration is Dynamic
  durationIsClass = Duration is Class
  dynamicIsDynamic = Dynamic is Dynamic
  dynamicIsClass = Dynamic is Class
  regexIsClass = Regex is Class
  dataSizeIsMapping = DataSize is Mapping
  getClassIsClass = 1.getClass() is Class
  getClassIsDynamic = 1.getClass() is Dynamic
  getClassIsObject = 1.getClass() is Object
  newDynamicIsClass = new Dynamic {} is Class
  newDynamicIsDynamic = new Dynamic {} is Dynamic
  userClassIsClass = Foo is Class
  userClassIsDynamic = Foo is Dynamic
  instanceIsFoo = new Foo {} is Foo
  instanceIsClass = new Foo {} is Class
}
"#,
    );
    assert_eq!(
        result["r"],
        json!({
            "durationIsDynamic": false, "durationIsClass": true,
            "dynamicIsDynamic": false, "dynamicIsClass": true,
            "regexIsClass": true, "dataSizeIsMapping": false,
            "getClassIsClass": true, "getClassIsDynamic": false,
            "getClassIsObject": false, "newDynamicIsClass": false,
            "newDynamicIsDynamic": true, "userClassIsClass": true,
            "userClassIsDynamic": false, "instanceIsFoo": true,
            "instanceIsClass": false
        })
    );
}

#[test]
fn class_value_cast_to_dynamic_fails() {
    let err = eval_fails("a = Duration as Dynamic");
    assert!(err.contains("Dynamic"), "{err}");
}

#[test]
fn amending_the_dynamic_class_makes_a_dynamic_object() {
    let json = eval("x = (Dynamic) { a = 1 }");
    assert_eq!(json["x"], serde_json::json!({ "a": 1 }));
}

#[test]
fn amending_a_builtin_class_value_is_rejected() {
    for src in [
        "x = (Duration) { a = 1 }",
        "x = (DataSize) { a = 1 }",
        "x = (Regex) { a = 1 }",
        "x = (1.getClass()) { a = 1 }",
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains("Cannot instantiate, or amend an instance of, external class"),
            "{src}: {err}"
        );
    }
}

#[test]
fn module_get_class_works_in_a_lambda_a_method_and_null_safe() {
    // Real pkl: the module's class is the same wherever it is asked, and a
    // file name may hold an `@`.
    let result = eval(
        r#"
module Foo

local f = () -> module.getClass().toString()
function g() = module.getClass().toString()

viaLambda = f()
viaMethod = g()
nullSafe = module?.getClass().toString()
"#,
    );
    assert_eq!(
        result,
        serde_json::json!({"viaLambda": "Foo", "viaMethod": "Foo", "nullSafe": "Foo"})
    );
}

#[test]
fn reflect_class_of_a_user_class() {
    let result = eval(
        r#"
import "pkl:reflect"
class Foo { x: Int = 1 }
name = reflect.Class(Foo).name
same = new Foo {}.getClass() == Foo
"#,
    );
    assert_eq!(result, json!({"name": "Foo", "same": true}));
}

#[test]
fn get_class_without_a_receiver_names_the_enclosing_object() {
    let result = eval(
        r#"
class Foo { c = getClass() }
dyn = new Dynamic { c = getClass() }.c == Dynamic
typed = new Foo {}.c == Foo
"#,
    );
    assert_eq!(result, json!({"dyn": true, "typed": true}));
}

#[test]
fn type_alias_text_names_its_module() {
    let result = eval("typealias Bar = String\ntext = Bar.toString()");
    assert_eq!(result["text"], "test#Bar");
}

#[test]
fn a_class_has_no_members_of_the_class_it_stands_for() {
    let err = eval_fails("class Foo { x = 1 }\nres = 1.getClass().nope");
    assert!(err.contains("Cannot find property `nope`"), "{err}");
    let err = eval_fails("res = 1.getClass().nope()");
    assert!(err.contains("Cannot find method `nope`"), "{err}");
}

#[test]
fn a_user_class_has_no_members_of_the_class_it_stands_for() {
    let result = eval(
        r#"
import "pkl:test"
class Person { foo = 1; function bar() = 2 }
res1 = test.catch(() -> Person.foo)
res2 = test.catch(() -> Person.bar())
"#,
    );
    assert!(
        result["res1"]
            .as_str()
            .unwrap()
            .contains("Cannot find property `foo`")
    );
    assert!(
        result["res2"]
            .as_str()
            .unwrap()
            .contains("Cannot find method `bar`")
    );
}

#[test]
fn null_safe_access_on_a_class_finds_no_instance_members() {
    let result = eval(
        r#"
import "pkl:test"
class Person { foo = 1; function bar() = 2 }
res1 = test.catch(() -> Person?.foo)
res2 = test.catch(() -> Person?.bar())
"#,
    );
    assert!(
        result["res1"]
            .as_str()
            .unwrap()
            .contains("Cannot find property `foo`")
    );
    assert!(
        result["res2"]
            .as_str()
            .unwrap()
            .contains("Cannot find method `bar`")
    );
}
