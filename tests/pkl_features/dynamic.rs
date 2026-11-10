use super::*;

// An element-only `Dynamic` (`new Dynamic { 1; 2 }`) is an object, not a
// collection. Every expectation below was compared with `pkl eval` 0.32.2.

fn is(expr: &str) -> bool {
    eval(&format!("e = {expr}\n"))["e"].as_bool().unwrap()
}

#[test]
fn dynamic_is_an_object_and_no_collection() {
    for (expr, expected) in [
        ("new Dynamic { 1; 2 } is Dynamic", true),
        ("new Dynamic { 1; 2 } is Object", true),
        ("new Dynamic { 1; 2 } is Any", true),
        ("new Dynamic { 1; 2 } is Int|Dynamic", true),
        ("new Dynamic { 1; 2 } is Listing", false),
        ("new Dynamic { 1; 2 } is Listing<Int>", false),
        ("new Dynamic { 1; 2 } is List", false),
        ("new Dynamic { 1; 2 } is Set", false),
        ("new Dynamic { 1; 2 } is Collection", false),
        ("new Dynamic { 1; 2 } is Mapping", false),
        ("new Dynamic { 1; 2 } is module", false),
        ("new Dynamic { a = 1 } is module", false),
        ("new Listing { 1 } is Dynamic", false),
        ("new Listing { 1 } is Object", true),
        ("new Listing { 1 } is List", false),
        ("List(1) is Listing", false),
        ("List(1) is Object", false),
        ("List(1) is Collection", true),
        ("Set(1) is List", false),
        ("Set(1) is Collection", true),
        ("new Mapping {} is Object", true),
        ("new Mapping {} is Dynamic", false),
        ("new Dynamic {} is Mapping", false),
    ] {
        assert_eq!(is(expr), expected, "{expr}");
    }
}

#[test]
fn dynamic_as_casts_follow_the_type() {
    assert_eq!(
        eval("e = new Dynamic { 1; 2 } as Dynamic")["e"],
        serde_json::json!([1, 2])
    );
    assert_eq!(
        eval("e = new Dynamic { 1; 2 } as Object")["e"],
        serde_json::json!([1, 2])
    );
    for ty in ["Listing", "List<Int>", "Set<Int>", "module"] {
        eval_fails(&format!("e = new Dynamic {{ 1; 2 }} as {ty}\n"));
    }
}

#[test]
fn declared_types_reject_a_dynamic_where_pkl_does() {
    for ty in [
        "Listing<Int>",
        "List<Int>",
        "Set<Int>",
        "Collection<Int>",
        "Mapping<Int, Int>",
        "Int",
        "(Listing<Int>|Int)?",
    ] {
        eval_fails(&format!("x: {ty} = new Dynamic {{ 1 }}\n"));
    }
    for ty in ["Dynamic", "Object", "Any", "Any?", "Listing<Int>|Dynamic"] {
        let json = eval(&format!("x: {ty} = new Dynamic {{ 1 }}\n"));
        assert_eq!(json["x"], serde_json::json!([1]), "{ty}");
    }
    let json = eval("typealias A = Dynamic\nx: A = new Dynamic { 1 }\n");
    assert_eq!(json["x"], serde_json::json!([1]));
    eval_fails("typealias A = Listing<Int>\nx: A = new Dynamic { 1 }\n");
}

#[test]
fn declared_types_reject_a_collection_of_another_kind() {
    for src in [
        "x: Listing<Int> = List(1)\n",
        "x: List<Int> = new Listing { 1 }\n",
        "x: List<Int> = Set(1)\n",
        "x: Set<Int> = List(1)\n",
        "x: Dynamic = List(1)\n",
        "x: Dynamic = new Listing { 1 }\n",
    ] {
        eval_fails(src);
    }
    for src in [
        "x: Collection<Int> = List(1)\n",
        "x: Collection<Int> = Set(1)\n",
        "x: Object = new Listing { 1 }\n",
        "x: Any = List(1)\n",
    ] {
        eval(src);
    }
}

#[test]
fn dynamic_has_no_collection_members() {
    for member in ["length", "isEmpty", "first", "last"] {
        let err = eval_fails(&format!("e = new Dynamic {{ 1; 2 }}.{member}\n"));
        assert!(err.contains("Cannot find property"), "{member}: {err}");
    }
    for method in ["map((x) -> x)", "contains(1)", "fold(0, (a, b) -> a + b)"] {
        let err = eval_fails(&format!("e = new Dynamic {{ 1; 2 }}.{method}\n"));
        assert!(err.contains("Cannot find method"), "{method}: {err}");
    }
    assert_eq!(
        eval("e = new Dynamic { 1; 2 }.hasProperty(\"length\")")["e"],
        false
    );
    assert_eq!(
        eval("e = new Dynamic { 1; 2 }.getPropertyOrNull(\"length\")")["e"],
        serde_json::Value::Null
    );
    eval_fails("e = new Dynamic { 1; 2 }.getProperty(\"length\")\n");
}

#[test]
fn dynamic_constraints_see_a_dynamic() {
    // `length` is no property of a Dynamic: pkl errors, it does not pass.
    for src in [
        "x: Dynamic(this.length == 1) = new Dynamic { 1 }\n",
        "x: Dynamic(length == 1) = new Dynamic { 1 }\n",
        "x: Dynamic(isEmpty) = new Dynamic { 1 }\n",
        "x: Any(this is Listing) = new Dynamic { 1 }\n",
        "x: Dynamic(toList().length == 2) = new Dynamic { 1 }\n",
    ] {
        eval_fails(src);
    }
    for src in [
        "x: Dynamic(toList().length == 1) = new Dynamic { 1 }\n",
        "x: Any(this is Dynamic) = new Dynamic { 1 }\n",
        "x: Any(this is Object) = new Dynamic { 1 }\n",
        "x: Any(!(this is Listing)) = new Dynamic { 1 }\n",
    ] {
        eval(src);
    }
    // The same through a property of a class and an alias.
    let src = "typealias A = Dynamic(toList().length == 1)\nclass U { g: A? = null }\na = new U { g { \"b\" } }\n";
    assert_eq!(eval(src)["a"]["g"], serde_json::json!(["b"]));
    eval_fails(
        "typealias A = Dynamic(toList().length == 2)\nclass U { g: A? = null }\na = new U { g { \"b\" } }\n",
    );
}

#[test]
fn dynamic_conversions() {
    assert_eq!(
        eval("e = new Dynamic { 1; 2 }.toList()")["e"],
        serde_json::json!([1, 2])
    );
    assert_eq!(
        eval("e = new Dynamic { 1; 2 }.toMap()")["e"],
        serde_json::json!({})
    );
    assert_eq!(
        eval("e = new Dynamic { 1; x = 2 }.toList()")["e"],
        serde_json::json!([1])
    );
    assert_eq!(
        eval("e = new Dynamic { 1; x = 2 }.toMap()")["e"],
        serde_json::json!({"x": 2})
    );
    assert_eq!(
        eval("e = new Dynamic { 1; 2 }.toList().map((x) -> x + 1)")["e"],
        serde_json::json!([2, 3])
    );
    eval_fails("e = new Dynamic { 1; 2 }.toListing()\n");
}

#[test]
fn dynamic_elements_spread_iterate_and_amend() {
    let json = eval(
        r#"
a = new Dynamic { 1; 2 }
spread = new Dynamic { ...a; 3 }
into_listing = new Listing { ...a; 3 }
looped = new Dynamic { for (x in a) { x * 2 } }
from_listing = new Dynamic { ...new Listing { 1; 2 } }
from_list = new Dynamic { ...List(1, 2) }
from_set = new Dynamic { ...Set(1, 2) }
amended = (a) { 3 }
indexed = a[1]
for_listing = new Listing { for (x in a) { x } }
"#,
    );
    assert_eq!(json["spread"], serde_json::json!([1, 2, 3]));
    assert_eq!(json["into_listing"], serde_json::json!([1, 2, 3]));
    assert_eq!(json["looped"], serde_json::json!([2, 4]));
    assert_eq!(json["from_listing"], serde_json::json!([1, 2]));
    assert_eq!(json["from_list"], serde_json::json!([1, 2]));
    assert_eq!(json["from_set"], serde_json::json!([1, 2]));
    assert_eq!(json["amended"], serde_json::json!([1, 2, 3]));
    assert_eq!(json["indexed"], 2);
    assert_eq!(json["for_listing"], serde_json::json!([1, 2]));
    assert!(is("new Dynamic { ...List(1, 2) } is Dynamic"));
}

#[test]
fn dynamic_equality_and_addition() {
    for (expr, expected) in [
        ("new Dynamic { 1; 2 } == new Dynamic { 1; 2 }", true),
        ("new Dynamic { 1; 2 } != new Dynamic { 1; 3 }", true),
        ("new Dynamic { 1; 2 } == new Listing { 1; 2 }", false),
        ("new Dynamic { 1; 2 } == List(1, 2)", false),
        (
            "new Dynamic {} == new Dynamic { for (x in List()) { x } }",
            true,
        ),
        ("new Dynamic {} == new Listing {}", false),
        ("new Dynamic { 1; x = 2 } == new Dynamic { 1; x = 2 }", true),
        (
            "new Dynamic { 1; x = 2 } == new Dynamic { 2; x = 2 }",
            false,
        ),
    ] {
        assert_eq!(is(expr), expected, "{expr}");
    }
    eval_fails("e = new Dynamic { 1 } + new Dynamic { 2 }\n");
    eval_fails("e = List(1) + new Dynamic { 2 }\n");
}

#[test]
fn dynamic_to_string() {
    for (expr, expected) in [
        ("\"\\(new Dynamic { 1; 2 })\"", "new Dynamic { 1; 2 }"),
        (
            "new Dynamic { \"a\"; 2 }.toString()",
            "new Dynamic { \"a\"; 2 }",
        ),
        ("\"\\(new Listing { 1; 2 })\"", "new Listing { 1; 2 }"),
        ("\"\\(Set(1, 2))\"", "Set(1, 2)"),
        ("\"\\(List(1, 2))\"", "List(1, 2)"),
        (
            "\"\\(new Dynamic { 1; x = 2 })\"",
            "new Dynamic { x = 2; 1 }",
        ),
        (
            "\"\\(new Dynamic { a = new Dynamic { 1; 2 }; b = new Listing { 1 } })\"",
            "new Dynamic { a { 1; 2 }; b { 1 } }",
        ),
    ] {
        let json = eval(&format!("e = {expr}\n"));
        assert_eq!(json["e"], expected, "{expr}");
    }
}

#[test]
fn dynamic_with_elements_and_members() {
    // Pkl cannot render an object with both as JSON, YAML or a property list.
    for format in ["JsonRenderer", "YamlRenderer", "PListRenderer"] {
        let err = eval_fails(&format!(
            "e = new {format} {{}}.renderValue(new Dynamic {{ 1; x = 2 }})\n"
        ));
        assert!(
            err.contains("both elements and properties/entries"),
            "{format}: {err}"
        );
    }
    let mut ev = Evaluator::new();
    let value = ev
        .eval_source(
            "b = new Dynamic { 1; x = 2 }\n",
            std::path::Path::new("test.pkl"),
        )
        .unwrap();
    let err = value.try_to_json().unwrap_err().to_string();
    assert!(
        err.contains("both elements and properties/entries as JSON"),
        "{err}"
    );
    let json = eval("e = new PcfRenderer {}.renderValue(new Dynamic { x = 2; 1 })\n");
    assert_eq!(json["e"], "{\n  x = 2\n  1\n}");
    // Amending an element-only Dynamic with members keeps its elements.
    let json = eval("a = new Dynamic { 1; 2 }\nb = (a) { x = 3 }.toList()\n");
    assert_eq!(json["b"], serde_json::json!([1, 2]));
}

#[test]
fn dynamic_renders_as_an_array() {
    let json = eval(
        r#"
a = new Dynamic { 1; 2 }
nested = new Dynamic { a = new Dynamic { 1; "x" }; b = 1 }
json = new JsonRenderer {}.renderValue(new Dynamic { 1; 2 })
yaml = new YamlRenderer {}.renderValue(new Dynamic { 1; 2 })
plist = new PListRenderer {}.renderValue(new Dynamic { 1; 2 })
"#,
    );
    assert_eq!(json["a"], serde_json::json!([1, 2]));
    assert_eq!(json["nested"], serde_json::json!({"a": [1, "x"], "b": 1}));
    assert_eq!(json["json"], "[\n  1,\n  2\n]");
    assert_eq!(json["yaml"], "- 1\n- 2");
    assert_eq!(
        json["plist"],
        "<array>\n  <integer>1</integer>\n  <integer>2</integer>\n</array>"
    );
}

#[test]
fn dynamic_converters_match_the_dynamic_class() {
    let convert = |class: &str| {
        let src = format!(
            "output {{ renderer = new JsonRenderer {{ converters {{ [{class}] = (x) -> \"C\" }} }} }}\nd = new Dynamic {{ 1; 2 }}\nl = new Listing {{ 1 }}\nlist = List(1)\ns = Set(1)\n"
        );
        eval_with_converters(&src)
    };
    let json = convert("Dynamic");
    assert_eq!(json["d"], "C");
    assert_eq!(json["l"], serde_json::json!([1]));
    let json = convert("Listing");
    assert_eq!(json["d"], serde_json::json!([1, 2]));
    assert_eq!(json["l"], "C");
    let json = convert("List");
    assert_eq!(json["d"], serde_json::json!([1, 2]));
    assert_eq!(json["l"], serde_json::json!([1]));
    assert_eq!(json["list"], "C");
    let json = convert("Set");
    assert_eq!(json["s"], "C");
    assert_eq!(json["list"], serde_json::json!([1]));
}

#[test]
fn dynamic_get_class_and_class_values() {
    for (expr, expected) in [
        ("new Dynamic { 1 }.getClass() == Dynamic", true),
        ("new Dynamic { 1 }.getClass() == Listing", false),
        ("new Listing { 1 }.getClass() == Listing", true),
        ("List(1).getClass() == List", true),
        (
            "new Dynamic { 1 }.getClass().simpleName == \"Dynamic\"",
            true,
        ),
        // A `Class` value is a `Class`, not an instance of what it names.
        ("new Dynamic { 1 }.getClass() is Class", true),
        ("new Dynamic { 1 }.getClass() is Dynamic", false),
        ("new Dynamic { 1 }.getClass() is Listing", false),
        ("new Dynamic { 1 }.getClass() is Object", false),
    ] {
        assert_eq!(is(expr), expected, "{expr}");
    }
    eval_fails("x: Listing<Int> = new Dynamic { 1 }.getClass()\n");
}

#[test]
fn amending_an_explicit_null_default_makes_a_dynamic() {
    // The issue's examples: pkl rejects the amended `null` for these types.
    for ty in [
        "Listing<String>?",
        "(Listing<String>|Int)?",
        "List<String>?",
        "Set<String>?",
        "Listing<String>(true)?",
    ] {
        let err = eval_fails(&format!(
            "class U {{ g: {ty} = null }}\na = new U {{ g {{ \"adm\" }} }}\n"
        ));
        assert!(err.contains("Dynamic"), "{ty}: {err}");
    }
    // pkl accepts it where the declared type takes a Dynamic.
    let json = eval(
        r#"
class U {
  a: Dynamic? = null
  b: Any? = null
  c: Object? = null
  d: (Listing<String>|Dynamic)? = null
  e = null
}
x = new U { a { "1" }; b { "2" }; c { "3" }; d { "4" }; e { "5" } }
"#,
    );
    for (key, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4"), ("e", "5")] {
        assert_eq!(json["x"][key], serde_json::json!([value]), "{key}");
    }
    // What was amended is a Dynamic, not a Listing.
    let json = eval(
        "class U { g = null }\na = new U { g { \"b\" } }\nd = a.g is Dynamic\nl = a.g is Listing\n",
    );
    assert_eq!(json["d"], true);
    assert_eq!(json["l"], false);
}

#[test]
fn amending_what_is_not_an_explicit_null_keeps_the_declared_default() {
    // `T?` without a default amends the type's default.
    let json = eval("class U { g: Listing<String>? }\na = new U { g { \"adm\" } }\n");
    assert_eq!(json["a"]["g"], serde_json::json!(["adm"]));
    let json = eval("class U { g: (*Listing<String>|Int)? }\na = new U { g { \"adm\" } }\n");
    assert_eq!(json["a"]["g"], serde_json::json!(["adm"]));
    // A Listing default is amended and stays a Listing.
    let json = eval(
        "class U { g: Listing<String> = new Listing { \"a\" } }\na = new U { g { \"b\" } }\nl = a.g is Listing\n",
    );
    assert_eq!(json["a"]["g"], serde_json::json!(["a", "b"]));
    assert_eq!(json["l"], true);
    let json =
        eval("class U { g: Listing<String> }\na = new U { g { \"b\" } }\nl = a.g is Listing\n");
    assert_eq!(json["l"], true);
    // A Dynamic default is amended and stays a Dynamic (pkl accepts this).
    let json = eval(
        "class U { g: Dynamic = new Dynamic { \"a\" } }\na = new U { g { \"b\" } }\nd = a.g is Dynamic\n",
    );
    assert_eq!(json["a"]["g"], serde_json::json!(["a", "b"]));
    assert_eq!(json["d"], true);
    // Amending an amended explicit null again amends the Dynamic.
    let json = eval(
        "class U { g: Dynamic? = null }\na = new U { g { \"x\" } }\nb = (a) { g { \"y\" } }\n",
    );
    assert_eq!(json["b"]["g"], serde_json::json!(["x", "y"]));
    // A property of an explicit-null amendment is an object Dynamic.
    let json = eval("class U { g: Dynamic? = null }\na = new U { g { x = 1 } }\n");
    assert_eq!(json["a"]["g"], serde_json::json!({"x": 1}));
    eval_fails("class Foo { x: Int }\nclass U { g: Foo? = null }\na = new U { g { x = 1 } }\n");
    eval_fails("class U { g: Mapping<String, Int>? = null }\na = new U { g { [\"a\"] = 1 } }\n");
}

#[test]
fn a_dynamic_through_a_variable_is_rejected_like_a_literal() {
    // The heuristic this replaced only caught a literal `new Dynamic`.
    for src in [
        "local d = new Dynamic { \"x\" }\nx: Listing<String> = d\n",
        "local function f() = new Dynamic { \"x\" }\nx: Listing<String> = f()\n",
        "class U { g: Listing<String> }\nlocal d = new Dynamic { \"x\" }\na = new U { g = d }\n",
        "class U { g: Listing<String> = new Dynamic { \"x\" } }\na = new U {}\n",
    ] {
        eval_fails(src);
    }
    eval("local d = new Dynamic { \"x\" }\nx: Dynamic = d\n");
}

#[test]
fn a_body_that_amends_nothing_makes_a_dynamic() {
    let json = eval(
        r#"
a { "x" }
m = new Mapping { ["k"] { "x" } }
u = new { "x" }
ad = a is Dynamic
md = m["k"] is Dynamic
ud = u is Dynamic
al = a is Listing
"#,
    );
    assert_eq!(json["ad"], true);
    assert_eq!(json["md"], true);
    assert_eq!(json["ud"], true);
    assert_eq!(json["al"], false);
    // With a declared Listing, the body amends the Listing's default.
    let json = eval("x: Listing<String> = new { \"x\" }\nl = x is Listing\n");
    assert_eq!(json["l"], true);
}

#[test]
fn this_is_module_is_false_for_a_dynamic() {
    // A module is not a type a Dynamic is an instance of.
    assert!(!is("new Dynamic { 1 } is module"));
    assert!(!is("new Dynamic { a = 1 } is module"));
    // In a constraint on the value, too.
    eval_fails("class C { x: Any(this is module) = new Dynamic { 1 } }\nc = new C {}\n");
}

#[test]
fn an_amended_dynamic_is_still_a_dynamic() {
    eval_fails("x: Mapping<String, Int> = (new Dynamic { a = 1 }) { b = 2 }\n");
    assert!(!is("(new Dynamic { a = 1 }) { b = 2 } is Mapping"));
    assert!(is("(new Dynamic { a = 1 }) { b = 2 } is Dynamic"));
}

#[test]
fn an_empty_dynamic_equals_only_a_dynamic() {
    for (expr, expected) in [
        (
            "new Dynamic { for (x in List()) { x } } == new Dynamic {}",
            true,
        ),
        (
            "new Dynamic { for (x in List()) { x } } == new Mapping {}",
            false,
        ),
        ("new Dynamic { for (x in List()) { x } } == Map()", false),
    ] {
        assert_eq!(is(expr), expected, "{expr}");
    }
    // Equal values are the same Mapping key.
    let json = eval(
        r#"
local a = new Dynamic { for (i in List()) { i } }
local m = new Mapping { [new Dynamic {}] = 1 }
e = m[a]
"#,
    );
    assert_eq!(json["e"], 1);
}

#[test]
fn generators_contribute_elements_beside_members() {
    for body in [
        "for (i in List(1, 2)) { ...List(i) }",
        "for (i in List(1, 2)) { i }",
        "when (true) { ...List(1, 2) }",
        "when (false) { 0 } else { ...List(1, 2) }",
    ] {
        let json = eval(&format!("e = new Dynamic {{ {body}; x = 3 }}.toList()\n"));
        assert_eq!(json["e"], serde_json::json!([1, 2]), "{body}");
    }
}

#[test]
fn amending_a_dynamic_with_members_twice_keeps_its_elements() {
    let json = eval(
        r#"
local a = new Dynamic { 1; 2 }
local b = (a) { x = 3 }
twice = (b) { y = 4 }.toList()
more = (b) { 5 }.toList()
"#,
    );
    assert_eq!(json["twice"], serde_json::json!([1, 2]));
    assert_eq!(json["more"], serde_json::json!([1, 2, 5]));
}

#[test]
fn an_empty_dynamic_is_not_the_key_of_an_empty_map() {
    eval_fails(
        "local m = new Mapping { [Map()] = 1 }\ne = m[new Dynamic { for (i in List()) { i } }]\n",
    );
    eval_fails("local m = new Mapping { [new Dynamic {}] = 1 }\ne = m[Map()]\n");
}

#[test]
fn a_computed_dynamic_is_a_dynamic_and_a_dynamic_key() {
    // `toDynamic()` of a class instance or a Map builds a Dynamic.
    for src in [
        "class C { x = 1 }\nlocal d = new C {}.toDynamic()\nlocal m = new Mapping { [new Dynamic { x = 1 }] = 7 }\ne = m[d]\n",
        "local d = Map(\"x\", 1).toDynamic()\nlocal m = new Mapping { [new Dynamic { x = 1 }] = 7 }\ne = m[d]\n",
    ] {
        assert_eq!(eval(src)["e"], 7, "{src}");
    }
    assert!(is("Map(\"x\", 1).toDynamic() is Dynamic"));
}

#[test]
fn amending_keeps_members_of_a_computed_dynamic_and_of_a_dynamic_property() {
    let json = eval(
        r#"
local d = Map("x", 1).toDynamic()
amended = (d) { y = 2 }
same = (d) {}
class C { x = 1 }
typed = (new C {}.toDynamic()) { y = 2 }
class U { g: Dynamic = new Dynamic { "a" } }
property = new U { g { x = 1 } }.g
"#,
    );
    assert_eq!(json["amended"], serde_json::json!({"x": 1, "y": 2}));
    assert_eq!(json["same"], serde_json::json!({"x": 1}));
    assert_eq!(json["typed"], serde_json::json!({"x": 1, "y": 2}));
    // Pkl keeps the element of the default beside the member; JSON cannot show both.
    let json = eval(
        "class U { g: Dynamic = new Dynamic { \"a\" } }\ne = new U { g { x = 1 } }.g.toList()\n",
    );
    assert_eq!(json["e"], serde_json::json!(["a"]));
}

#[test]
fn a_dynamic_with_elements_and_members_is_no_mapping() {
    assert!(!is("new Dynamic { 1; x = 2 } is Mapping"));
    eval_fails("x: Mapping<Int, Int> = new Dynamic { 1; x = 2 }\n");
}

#[test]
fn assigning_a_member_of_a_computed_dynamic_replaces_it() {
    let json = eval(
        r#"
local d = Map("x", new Dynamic { a = 1 }).toDynamic()
replaced = (d) { x = new Dynamic { b = 2 } }
merged = (d) { x { b = 2 } }
"#,
    );
    assert_eq!(json["replaced"], serde_json::json!({"x": {"b": 2}}));
    assert_eq!(json["merged"], serde_json::json!({"x": {"a": 1, "b": 2}}));
}

#[test]
fn an_amendment_of_a_computed_dynamic_reads_its_members_unqualified() {
    let json = eval(
        r#"
class P { x: Int = 1; y: Int = 2 }
local d = new P {}.toDynamic()
a = (d) { z = x + y }
b = (d) { x = 10; w = y }
"#,
    );
    assert_eq!(json["a"], serde_json::json!({"x": 1, "y": 2, "z": 3}));
    assert_eq!(json["b"], serde_json::json!({"x": 10, "y": 2, "w": 2}));
}

// Every expectation below was compared with `pkl eval` 0.32.2. A string key
// becomes a property of `toDynamic()`'s result, whatever its spelling; any
// other key becomes an entry.
#[test]
fn to_dynamic_keeps_non_string_keys_as_entries() {
    let json = eval(
        r#"
local m1 = Map(1, "a").toDynamic()
local m2 = Map("a", 1, "a b", 2, "1x", 3, "ok_1", 4, 5, 6).toDynamic()
pcf1 = new PcfRenderer {}.renderValue(m1)
pcf2 = new PcfRenderer {}.renderValue(m2)
pcf3 = new PcfRenderer {}.renderValue((m2) { [5] = 60; a = 9 })
yaml1 = new YamlRenderer {}.renderValue(m1)
yaml2 = new YamlRenderer {}.renderValue(m2)
byKey = m1[1]
amended = (m1) { [1] = "b" }[1]
size = (m2) { [5] = 60 }.toMap().length
"#,
    );
    assert_eq!(json["pcf1"], "{\n  [1] = \"a\"\n}");
    assert_eq!(
        json["pcf2"],
        "{\n  a = 1\n  `a b` = 2\n  `1x` = 3\n  ok_1 = 4\n  [5] = 6\n}"
    );
    assert_eq!(
        json["pcf3"],
        "{\n  a = 9\n  `a b` = 2\n  `1x` = 3\n  ok_1 = 4\n  [5] = 60\n}"
    );
    assert_eq!(json["yaml1"], "1: a");
    assert_eq!(json["yaml2"], "a: 1\na b: 2\n1x: 3\nok_1: 4\n5: 6");
    assert_eq!(json["byKey"], "a");
    assert_eq!(json["amended"], "b");
    assert_eq!(json["size"], 5);
}

#[test]
fn to_dynamic_members_read_unqualified_beside_entries() {
    let json = eval(
        r#"
local d = Map("a", 1, "ok_1", 4, 5, 6).toDynamic()
z = (d) { z = a + ok_1 }.z
"#,
    );
    assert_eq!(json["z"], 5);
}

// An entry body of only elements in a mapping whose value type is a `Listing`
// (or a `*Listing|Int` union) makes a `Listing`, as `pkl eval` 0.32.2 does.
#[test]
fn typed_mapping_entries_of_listing_type_are_listings() {
    let json = eval(
        r#"
class C { m: Mapping<String, Listing<Int>> }
class Base { m: Mapping<String, Listing<Int>> = new { ["x"] { 1 } } }
class D { m: Mapping<String, Listing<Int>> = new { default { 5 }; ["p"] {} } }
a = new Mapping<String, Listing<Int>> { ["k"] { 1 } }
b = new Mapping<String, Listing<Int>> { ["k"] = new { 1 } }
c = new Mapping<String, Listing<Int>> { default { 1 }; ["k"] {} }
d = new C { m { ["k"] { 1 } } }
e = new Base { m { ["x"] { 2 } } }
f = new Mapping<String, *Listing<Int>|Int> { ["k"] { 1 } }
g = new D {}
h = new Mapping<String, *Listing<Int>|Int> { ["k"] = new { 1 } }
j = new Mapping<String, Dynamic> { ["k"] { 1 } }
isa = a["k"] is Listing
isb = b["k"] is Listing
isc = c["k"] is Listing
isd = d.m["k"] is Listing
ise = e.m["x"] is Listing
isf = f["k"] is Listing
isg = g.m["p"] is Listing
ish = h["k"] is Listing
isj = j["k"] is Dynamic
len = a["k"].length
lend = d.m["k"].length
"#,
    );
    for name in [
        "isa", "isb", "isc", "isd", "ise", "isf", "isg", "ish", "isj",
    ] {
        assert_eq!(json[name], true, "{name}");
    }
    assert_eq!(json["len"], 1);
    assert_eq!(json["lend"], 1);
    assert_eq!(json["e"]["m"]["x"], serde_json::json!([1, 2]));
}

#[test]
fn to_dynamic_keeps_compound_keys_as_entries() {
    let json = eval(
        r#"
r = new PcfRenderer {}.renderValue(Map(List(1), "v").toDynamic())
"#,
    );
    let rendered = json["r"].as_str().unwrap();
    assert!(!rendered.contains("pklr:"), "{rendered}");
    assert!(rendered.starts_with("{\n  ["), "{rendered}");
}

// A body that amends nothing and has elements as well as members is a
// `Dynamic` with both. Each expectation was compared with `pkl eval` 0.32.2.
#[test]
fn an_untyped_body_with_elements_and_members_keeps_both() {
    let json = eval(
        r#"
local d = new Dynamic { y { 5; z = 1 } }
a1 = d.y.z
a2 = d.y.toList()
a3 = d.y.toMap()
local e = new Dynamic { y { z = 1; 5 } }
b1 = e.y.z
b2 = e.y.toList()
local n = new Dynamic { a { b { 5; z = 1 } } }
c1 = n.a.b.z
c2 = n.a.b.toList()
local g = new Dynamic { y { 1; 2; z = 1; 3 } }
d1 = g.y.toList()
d2 = g.y.z
"#,
    );
    assert_eq!(json["a1"], 1);
    assert_eq!(json["a2"], serde_json::json!([5]));
    assert_eq!(json["a3"], serde_json::json!({"z": 1}));
    assert_eq!(json["b1"], 1);
    assert_eq!(json["b2"], serde_json::json!([5]));
    assert_eq!(json["c1"], 1);
    assert_eq!(json["c2"], serde_json::json!([5]));
    assert_eq!(json["d1"], serde_json::json!([1, 2, 3]));
    assert_eq!(json["d2"], 1);
}

#[test]
fn a_generator_body_with_elements_and_members_keeps_both() {
    let json = eval(
        r#"
local d = new Dynamic { y { for (i in List(1, 2)) { i }; when (true) { z = 1 } } }
a1 = d.y.z
a2 = d.y.toList()
local f = new Dynamic { y { for (k in List("a")) { ["k\(k)"] = 1 }; 7 } }
b1 = f.y.toMap()
b2 = f.y.toList()
"#,
    );
    assert_eq!(json["a1"], 1);
    assert_eq!(json["a2"], serde_json::json!([1, 2]));
    assert_eq!(json["b1"], serde_json::json!({"ka": 1}));
    assert_eq!(json["b2"], serde_json::json!([7]));
}

#[test]
fn an_untyped_body_with_elements_and_members_in_a_listing_or_typed_dynamic() {
    let json = eval(
        r#"
local l = new Listing { new { 5; z = 1 } }
a1 = l[0].z
a2 = l[0].toList()
class D { x: Dynamic? }
local d = new D { x { 5; z = 1 } }
b1 = d.x.z
b2 = d.x.toList()
local m = new Dynamic { y { z = 1 } }
c = m.y.z
"#,
    );
    assert_eq!(json["a1"], 1);
    assert_eq!(json["a2"], serde_json::json!([5]));
    assert_eq!(json["b1"], 1);
    assert_eq!(json["b2"], serde_json::json!([5]));
    assert_eq!(json["c"], 1);
}

#[test]
fn amending_a_mixed_body_adds_to_both() {
    let json = eval(
        r#"
local d = new Dynamic { y { 5; z = 1 } }
local d2 = (d) { y { 6; w = 2 } }
a1 = d2.y.toList()
a2 = d2.y.z
a3 = d2.y.w
"#,
    );
    assert_eq!(json["a1"], serde_json::json!([5, 6]));
    assert_eq!(json["a2"], 1);
    assert_eq!(json["a3"], 2);
}

#[test]
fn a_listing_typed_body_with_only_elements_stays_a_listing() {
    let json = eval(
        r#"
class C { l: Listing<Int> }
local c = new C { l { 1; 2 } }
a = c.l is Listing
b = c.l
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], serde_json::json!([1, 2]));
}

// An element, spread or generator may read a member declared after it, and
// keeps its place among the elements. Compared with `pkl eval` 0.32.2.
#[test]
fn an_element_may_read_a_member_declared_later() {
    let json = eval(
        r#"
local d = new Dynamic { y { z; z = 1 } }
a1 = d.y.toList()
a2 = d.y.z
local t = new Dynamic { z; z = 1 }
b = t.toList()
local o = new Dynamic { z; 5; z = 1 }
c1 = o.toList()
local e = new Dynamic { 5; z; k = 2; w; w = 3; z = 1 }
c2 = e.toList()
"#,
    );
    assert_eq!(json["a1"], serde_json::json!([1]));
    assert_eq!(json["a2"], 1);
    assert_eq!(json["b"], serde_json::json!([1]));
    assert_eq!(json["c1"], serde_json::json!([1, 5]));
    assert_eq!(json["c2"], serde_json::json!([5, 1, 3]));
}

#[test]
fn a_generator_may_read_a_member_declared_later() {
    let json = eval(
        r#"
local d = new Dynamic { y { when (true) { z }; z = 1 } }
a = d.y.toList()
local e = new Dynamic { y { for (i in List(1)) { z + i }; z = 1 } }
b = e.y.toList()
local f = new Dynamic { y { 7; for (i in List(1, 2)) { z + i }; z = 10 } }
c = f.y.toList()
"#,
    );
    assert_eq!(json["a"], serde_json::json!([1]));
    assert_eq!(json["b"], serde_json::json!([2]));
    assert_eq!(json["c"], serde_json::json!([7, 11, 12]));
}

#[test]
fn a_member_may_read_an_earlier_member_of_a_mixed_body() {
    let json = eval(
        r#"
local d = new Dynamic { y { 5; k = 2; z = k } }
a = d.y.z
"#,
    );
    assert_eq!(json["a"], 2);
}

#[test]
fn many_spreads_before_a_member_keep_every_element() {
    let spreads = "...a; ".repeat(200);
    let json = eval(&format!(
        "local a = List(1, 2, 3)\nlocal d = new Dynamic {{ {spreads}z = 1 }}\nr = d.toList().length\nr2 = d.z\n"
    ));
    assert_eq!(json["r"], 600);
    assert_eq!(json["r2"], 1);
}

#[test]
fn a_partly_evaluated_generator_before_a_member_still_reads_it() {
    let json = eval(
        r#"
local d = new Dynamic { y { when (true) { 1; z }; z = 2 } }
r = d.y.toList()
"#,
    );
    assert_eq!(json["r"], serde_json::json!([1, 2]));
}

#[test]
fn a_body_whose_only_elements_come_from_generators_reads_a_later_member() {
    let json = eval(
        r#"
local d = new Dynamic { when (true) { z }; z = 1 }
a = d.toList()
local e = new Dynamic { for (i in List(1, 2)) { z + i }; z = 10 }
b = e.toList()
"#,
    );
    assert_eq!(json["a"], serde_json::json!([1]));
    assert_eq!(json["b"], serde_json::json!([11, 12]));
}
