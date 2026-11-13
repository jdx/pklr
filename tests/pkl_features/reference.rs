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
