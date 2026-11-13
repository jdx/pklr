//! `pkl:ref`: references to a value of a known type. Expected results are
//! what `pkl` 0.32 prints for the same snippets.

use super::*;
use serde_json::json;

const PRELUDE: &str = r#"
import "pkl:ref"

class D extends ref.Domain {
  function renderReference(reference: ref.Reference<D, Any>): String =
    let (path = reference.getPath().map((e) -> if (e.isProperty) ".\(e.property)" else "[\(e.key)]"))
      "$" + path.join("")
}
local const d: D = new {}

class Inner {
  n: Int
  tags: Listing<String>
  m: Mapping<String, Int?>
}
class Doc {
  title: String
  inner: Inner
  items: List<Inner>
  maybe: String?
}
local data = new Doc {
  title = "t"
  inner = new Inner { n = 1; tags { "a" }; m { ["k"] = 2 } }
  items = List()
  maybe = null
}
local r: ref.Reference<D, Doc> = ref.Reference(d, Doc, data)
"#;

fn run(body: &str) -> serde_json::Value {
    eval(&format!("{PRELUDE}\n{body}"))
}

fn fails(body: &str) -> String {
    eval_fails(&format!("{PRELUDE}\n{body}"))
}

#[test]
fn reference_paths_render_through_the_domain() {
    let result = run(r#"
title = "\(r.title)"
nested = "\(r.inner.n)"
tag = "\(r.inner.tags[0])"
mapped = "\(r.inner.m["k"])"
listItem = "\(r.items[2].tags[1])"
"#);
    assert_eq!(
        result,
        json!({
            "title": "$.title",
            "nested": "$.inner.n",
            "tag": "$.inner.tags[0]",
            "mapped": "$.inner.m[k]",
            "listItem": "$.items[2].tags[1]",
        })
    );
}

#[test]
fn reference_accessors_expose_domain_data_and_path() {
    let result = run(r#"
isRef = r.title is ref.Reference
sameDomain = r.inner.getDomain() == d
sameData = r.inner.getData() == data
path = r.inner.tags[3].getPath().map((e) -> if (e.isProperty) e.property else e.key.toString())
firstIsProperty = r.title.getPath()[0].isProperty
key = r.inner.tags[3].getPath()[2].key
str = r.title.toString()
"#);
    assert_eq!(
        result,
        json!({
            "isRef": true,
            "sameDomain": true,
            "sameData": true,
            "path": ["inner", "tags", "3"],
            "firstIsProperty": true,
            "key": 3,
            "str": "$.title",
        })
    );
}

#[test]
fn reference_member_must_exist_in_the_referent_type() {
    for body in [
        "x = r.nope",
        "x = r.title.foo",
        r#"x = r.inner.tags["a"]"#,
        "x = r.items[0].n.zzz",
    ] {
        let err = fails(body);
        assert!(
            err.contains("Cannot") || err.contains("subscript"),
            "{body}: {err}"
        );
    }
}

#[test]
fn reference_to_dynamic_allows_any_property_but_default() {
    let result = run(r#"x = "\(ref.Reference(d, Dynamic, null).anything.deeper)""#);
    assert_eq!(result["x"], "$.anything.deeper");
    let err = fails(r#"x = ref.Reference(d, Dynamic, null).default"#);
    assert!(err.contains("default"), "{err}");
}

#[test]
fn reference_type_checks_against_its_referent_type() {
    let result = run(r#"
typealias Ref<T> = ref.Reference<D, T>
ok1 = r.title is Ref<String>
ok2 = r.maybe is Ref<String?>
ok3 = r.title is Ref<String | Int>
ok4 = r.title is Ref<Int>
ok5 = r.inner.n is Ref<Any>
"#);
    assert_eq!(
        result,
        json!({"ok1": true, "ok2": true, "ok3": true, "ok4": false, "ok5": true})
    );
}

#[test]
fn reference_referent_type_may_not_have_constraints() {
    let err = fails(r#"x = r.title as ref.Reference<D, String(length > 1)>"#);
    assert!(err.contains("may not include type constraints"), "{err}");
}

#[test]
fn reference_hidden_default_can_name_its_own_class() {
    let result = eval(
        r#"
import "pkl:ref"

class D extends ref.Domain {
  function renderReference(reference: ref.Reference<D, Any>): String =
    "ref:\(reference.getData().name)"
}
local const d: D = new {}

class A {
  name: String
  hidden fixed $: ref.Reference<D, A> = ref.Reference(d, A, this)
}

a: A = new { name = "x" }
shown = "\(a.$)"
isA = a.$.getData() is A
"#,
    );
    assert_eq!(result["a"], json!({"name": "x"}));
    assert_eq!(result["shown"], "ref:x");
    assert_eq!(result["isA"], true);
}

#[test]
fn reference_without_a_converter_renders_as_text_in_strings() {
    let result = run(r#"
joined = new Listing { r.title; r.inner.n }.toList().join(",")
"#);
    assert_eq!(result["joined"], "$.title,$.inner.n");
}

#[test]
fn reference_accessors_take_no_arguments() {
    let err = fails("x = r.getDomain(1)");
    assert!(
        err.contains("Expected 0 function arguments but got 1."),
        "{err}"
    );
    let err = eval_fails("import \"pkl:ref\"\nx = ref.Reference()");
    assert!(
        err.contains("Expected 3 function arguments but got 0."),
        "{err}"
    );
}

#[test]
fn reference_type_arguments_follow_the_ancestor() {
    let body = |src: &str, dst: &str| {
        format!(
            "class P {{ v: {src} }}\nlocal p = ref.Reference(d, P, null)\nlocal ok: ref.Reference<D, {dst}> = p.v\nres = ok.getPath().length"
        )
    };
    for (src, dst) in [
        ("List<String>", "Collection<String>"),
        ("Mapping<String, Int>", "Mapping<Any, Int>"),
        ("Map<Int8, Int>", "Map<Int, Int>"),
    ] {
        assert_eq!(run(&body(src, dst))["res"], 1, "{src} -> {dst}");
    }
    for (src, dst) in [
        ("List<String>", "Collection<Int>"),
        ("Listing<Int>", "Object"),
        ("Mapping<Any, Int>", "Mapping<String, Int>"),
        ("List", "List<Int>"),
    ] {
        let err = fails(&body(src, dst));
        assert!(
            err.contains("Expected value of type"),
            "{src} -> {dst}: {err}"
        );
    }
}

#[test]
fn reference_user_class_is_a_typed_and_object() {
    let result = run(r#"
class U {}
class P { v: U }
local p = ref.Reference(d, P, null)
local a: ref.Reference<D, Typed> = p.v
local b: ref.Reference<D, Object> = p.v
ok = a.getPath().length + b.getPath().length
"#);
    assert_eq!(result["ok"], 2);
}

#[test]
fn reference_subscript_key_is_of_the_key_class() {
    let result = run(r#"
class P { dur: Mapping<Duration, Int> = new {}; ds: Mapping<DataSize, Int> = new {} }
local p = ref.Reference(d, P, null)
a = p.dur[1.s].getPath().length
b = p.ds[1.mb].getPath().length
"#);
    assert_eq!(result["a"], 2);
    assert_eq!(result["b"], 2);
    let err = fails(
        r#"
class P { m: Mapping<Number, Int> = new {} }
local p = ref.Reference(d, P, null)
x = p.m[1]
"#,
    );
    assert!(err.contains("Cannot subscript"), "{err}");
}

#[test]
fn reference_hidden_default_reads_members_of_its_own_class() {
    let result = run(r#"
class A {
  n: Int = 1
  hidden fixed `$` = ref.Reference(d, A, this).n
}
n = new A {}.n
"#);
    assert_eq!(result["n"], 1);
}

#[test]
fn reference_has_no_document_form() {
    let mut ev = Evaluator::new();
    let src = format!("{PRELUDE}\nx = r");
    let value = ev
        .eval_source(&src, std::path::Path::new("test.pkl"))
        .unwrap();
    let json = value.to_json().to_string();
    assert!(!json.contains("\\u0000"), "{json}");
}

#[test]
fn reference_to_a_reference_checks_nested_type_arguments() {
    let result = run(r#"
class P { v: ref.Reference<D, String> }
local p = ref.Reference(d, P, null)
local ok: ref.Reference<D, ref.Reference<D, Any>> = p.v
res = ok.getPath().length
"#);
    assert_eq!(result["res"], 1);
}

#[test]
fn reference_converters_see_class_declarations_after_evaluation() {
    let src = format!(
        r#"{PRELUDE}
output {{
  renderer = new JsonRenderer {{
    converters {{ [ref.Reference] = (x) -> x.title.toString() }}
  }}
}}
v = r
"#
    );
    let result = eval_with_converters(&src);
    assert_eq!(result["v"], "$.title");
}

#[test]
fn reference_map_keys_match_map_key_types() {
    let result = run(r#"
class P { a: Mapping<Map<String, Int>, Int> = new {}; b: Map<Map<String, Int>, Int> = Map() }
local p = ref.Reference(d, P, null)
x = p.a[Map("a", 1)].getPath().length
y = p.b[Map("a", 1)].getPath().length
"#);
    assert_eq!(result["x"], 2);
    assert_eq!(result["y"], 2);
}

#[test]
fn reference_self_class_placeholder_keeps_inherited_declaration_scopes() {
    let result = run(r#"
typealias Names = List<String>
open class A { names: Names = List() }
class B extends A {
  hidden fixed `$` = ref.Reference(d, B, this).names
  n = 1
}
local b: ref.Reference<D, List<String>> = new B {}.`$`
n = new B {}.n
len = b.getPath().length
"#);
    assert_eq!(result["n"], 1);
    assert_eq!(result["len"], 1);
}

#[test]
fn reference_constrained_generic_alias_gets_its_type_arguments() {
    let prelude = r#"
class Item { n: Int }
typealias NonEmpty<T> = Listing<T>(length > 0)
class P { items: NonEmpty<Item> }
local p = ref.Reference(d, P, null)
"#;
    let result = run(&format!(
        "{prelude}\nlocal ok: ref.Reference<D, Listing<Item>> = p.items\nres = ok.getPath().length\nn = p.items[0].n.getPath().length"
    ));
    assert_eq!(result["res"], 1);
    assert_eq!(result["n"], 3);
    let err = fails(&format!("{prelude}\nbad = p.items[0].nope"));
    assert!(err.contains("Cannot find property `nope`"), "{err}");
    let err = fails(&format!(
        "{prelude}\nbad: ref.Reference<D, Listing<String>> = p.items"
    ));
    assert!(err.contains("Expected value of type"), "{err}");
}

#[test]
fn reference_generic_alias_with_nested_constraints_keeps_its_arguments() {
    let prelude = r#"
class Item { n: Int }
typealias Rows<T> = Mapping<String(!isEmpty), T>(length > 0)
typealias Items<T> = Listing<T(n > 0)>(length > 0)
class P { rows: Rows<Item>; items: Items<Item> }
local p = ref.Reference(d, P, null)
"#;
    let result = run(&format!(
        "{prelude}\nrow = p.rows[\"a\"].n.getPath().length\nitem = p.items[0].n.getPath().length"
    ));
    assert_eq!(result["row"], 3);
    assert_eq!(result["item"], 3);
    for member in ["p.rows[\"a\"].nope", "p.items[0].nope"] {
        let err = fails(&format!("{prelude}\nbad = {member}"));
        assert!(
            err.contains("Cannot find property `nope`"),
            "{member}: {err}"
        );
    }
}

#[test]
fn reference_generic_alias_parameters_do_not_collide_with_user_names() {
    let result = run(r#"
class Item { n: Int }
typealias __refarg0 = String
typealias Wrap<T> = Listing<T>
class P { v: Wrap<Item> }
local p = ref.Reference(d, P, null)
n = p.v[0].n.getPath().length
"#);
    assert_eq!(result["n"], 3);
}
