use super::*;

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

// ============================================================
// Higher-order methods on Map / Mapping
// ============================================================

#[test]
fn map_filter() {
    let json = eval(
        r#"
local items = new Mapping<String, Int> {
    ["a"] = 1
    ["b"] = 2
    ["c"] = 3
}
x = items.toMap().filter((k, v) -> v > 1).toMapping()
"#,
    );
    assert_eq!(json["x"]["b"], 2);
    assert_eq!(json["x"]["c"], 3);
    assert!(json["x"].get("a").is_none());
}

#[test]
fn map_filter_then_map_values_chain() {
    // toMap().filter().mapValues().toMapping() is a common transformation chain.
    let json = eval(
        r#"
local items = new Mapping<String, Int> {
    ["a"] = 1
    ["b"] = 2
    ["c"] = 3
}
x =
    items
        .toMap()
        .filter((k, v) -> v > 1)
        .mapValues((k, v) -> v * 10)
        .toMapping()
"#,
    );
    assert_eq!(json["x"]["b"], 20);
    assert_eq!(json["x"]["c"], 30);
    assert!(json["x"].get("a").is_none());
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
// Late binding
// ============================================================

#[test]
fn late_binding_basic() {
    // Overriding x should cause y to re-evaluate
    let json = eval(
        r#"
local base = new {
    x = 1
    y = x + 1
}
result = (base) {
    x = 10
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 11);
}

#[test]
fn late_binding_chained() {
    // Chained dependency: x -> y -> z
    let json = eval(
        r#"
local base = new {
    x = 1
    y = x + 1
    z = y + 1
}
result = (base) {
    x = 10
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 11);
    assert_eq!(json["result"]["z"], 12);
}

#[test]
fn late_binding_unrelated_preserved() {
    // Properties not depending on overridden ones stay the same
    let json = eval(
        r#"
local base = new {
    x = 1
    y = x + 1
    name = "hello"
}
result = (base) {
    x = 10
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 11);
    assert_eq!(json["result"]["name"], "hello");
}

#[test]
fn late_binding_class_new() {
    // Late binding with class defaults
    let json = eval(
        r#"
class Config {
    port: Int = 8080
    url: String = "http://localhost:\(port)"
}
result = new Config {
    port = 3000
}
"#,
    );
    assert_eq!(json["result"]["port"], 3000);
    assert_eq!(json["result"]["url"], "http://localhost:3000");
}

#[test]
fn late_binding_string_interpolation() {
    // Late binding with string interpolation
    let json = eval(
        r#"
local base = new {
    name = "world"
    greeting = "Hello, \(name)!"
}
result = (base) {
    name = "Pkl"
}
"#,
    );
    assert_eq!(json["result"]["name"], "Pkl");
    assert_eq!(json["result"]["greeting"], "Hello, Pkl!");
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

#[test]
fn this_alias_reachable_from_nested_objects() {
    // Nested objects leave out `this` aliases they never name; every way of
    // still reaching the alias (directly, from a deeper body, from a lambda,
    // through `outer`, or through a property holding `this`) must keep it.
    let json = eval(
        r#"
obj {
  local self = this
  a = 1
  me = this
  plain { x = 0 }
  direct { y = self.a }
  deep { inner { z = self.a } }
  viaOuter { w = outer.a }
  viaMember { w = outer.me.a }
  fn { local f = (n) -> n + self.a; r = f.apply(1) }
  b = 2
}
amended = (obj.plain) { q = obj.b }
reamended = (obj.deep) { extra = 3 }
"#,
    );
    assert_eq!(json["obj"]["direct"]["y"], 1);
    assert_eq!(json["obj"]["deep"]["inner"]["z"], 1);
    assert_eq!(json["obj"]["viaOuter"]["w"], 1);
    assert_eq!(json["obj"]["viaMember"]["w"], 1);
    assert_eq!(json["obj"]["fn"]["r"], 2);
    assert_eq!(json["amended"], serde_json::json!({"x": 0, "q": 2}));
    assert_eq!(
        json["reamended"],
        serde_json::json!({"inner": {"z": 1}, "extra": 3})
    );
}

#[test]
fn this_snapshot_unaffected_by_later_entries() {
    let json = eval(
        r#"
obj {
  local self = this
  a = 1
  early = self.a
  inner { y = outer.a }
  snap = this
  b = 2
  late = self.b
}
m {
  ["one"] = 1
  ["nested"] { v = 5 }
  held = this
  ["two"] = this["one"] + 1
}
"#,
    );
    assert_eq!(json["obj"]["early"], 1);
    assert_eq!(json["obj"]["inner"]["y"], 1);
    assert_eq!(json["obj"]["late"], 2);
    assert_eq!(
        json["obj"]["snap"],
        serde_json::json!({"a": 1, "early": 1, "inner": {"y": 1}})
    );
    assert_eq!(json["m"]["two"], 2);
    assert_eq!(
        json["m"]["held"],
        serde_json::json!({"one": 1, "nested": {"v": 5}})
    );
}

#[test]
fn this_keyword_basic() {
    // `this` refers to the current object
    let json = eval(
        r#"
data {
    x = 1
    y = this.x + 1
}
"#,
    );
    assert_eq!(json["data"]["x"], 1);
    assert_eq!(json["data"]["y"], 2);
}

#[test]
fn this_keyword_nested() {
    // `this` in a nested object refers to the inner object, not the outer
    let json = eval(
        r#"
data {
    x = 10
    inner {
        x = 20
        y = this.x + 1
    }
}
"#,
    );
    assert_eq!(json["data"]["x"], 10);
    assert_eq!(json["data"]["inner"]["x"], 20);
    assert_eq!(json["data"]["inner"]["y"], 21);
}

#[test]
fn this_keyword_module_level() {
    // `this` at module level refers to the module object
    let json = eval(
        r#"
x = 42
y = this.x + 1
"#,
    );
    assert_eq!(json["x"], 42);
    assert_eq!(json["y"], 43);
}

#[test]
fn this_keyword_in_string_interpolation() {
    let json = eval(
        r#"
data {
    name = "world"
    greeting = "Hello, \(this.name)!"
}
"#,
    );
    assert_eq!(json["data"]["greeting"], "Hello, world!");
}

#[test]
fn this_keyword_with_hidden_property() {
    let json = eval(
        r#"
class Data {
    hidden base = "https://example.com"
    url = this.base + "/api"
}
data = new Data {}
"#,
    );
    // base must not appear in output (hidden)
    assert!(json["data"].get("base").is_none());
    // but url must resolve via this.base
    assert_eq!(json["data"]["url"], "https://example.com/api");
}

#[test]
fn this_keyword_hidden_at_module_level() {
    let json = eval(
        r#"
hidden secret = "abc123"
derived = this.secret + "-derived"
"#,
    );
    assert!(json.get("secret").is_none());
    assert_eq!(json["derived"], "abc123-derived");
}

#[test]
fn module_keyword_at_top_level() {
    // `module` refers to the top-level module object
    let json = eval(
        r#"
x = 1
y = module.x + 10
"#,
    );
    assert_eq!(json["x"], 1);
    assert_eq!(json["y"], 11);
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
fn class_local_this_alias_tracks_amended_inputs() {
    let json = eval(
        r#"
class Factory {
    local factory = this
    staged: Boolean = false
    fixed step = (new { staged = false }) {
        staged = factory.staged
    }
}
x = new Factory {
    staged = true
}
"#,
    );
    assert_eq!(json["x"]["step"]["staged"], true);
    assert!(json["x"].get("factory").is_none());
}

#[test]
fn class_local_this_alias_is_complete_for_deferred_methods() {
    let json = eval(
        r#"
class Factory {
    local factory = this
    local secondAlias = factory
    first: String = "first"
    last: String = "last"
    function lastValue(): String = secondAlias.last
}

local factory = new Factory {}
result = factory.lastValue()
"#,
    );
    assert_eq!(json["result"], "last");
}

#[test]
fn class_with_type_params_is_rejected() {
    let err = eval_fails(
        r#"
class Container<T> {
    value: T = "default"
}
"#,
    );
    assert!(
        err.contains("Only standard library members can have type parameters"),
        "{err}"
    );
}

#[test]
fn new_with_dotted_type_name() {
    // Dotted type names in new: resolves Config then .Step
    let json = eval(
        r#"
local Config = new {
    Step = new {
        check = "default"
        glob = "*.rs"
    }
}
x = new Config.Step {
    check = "custom"
}
"#,
    );
    assert_eq!(json["x"]["check"], "custom");
    assert_eq!(json["x"]["glob"], "*.rs");
}

// ============================================================
// Class inheritance (extends) and super keyword
// ============================================================

#[test]
fn class_extends_basic() {
    let json = eval(
        r#"
open class Animal {
    name: String = "unknown"
    legs: Int = 4
}
class Dog extends Animal {
    breed: String = "mixed"
}
x = new Dog {
    name = "Rex"
}
"#,
    );
    assert_eq!(json["x"]["name"], "Rex");
    assert_eq!(json["x"]["legs"], 4);
    assert_eq!(json["x"]["breed"], "mixed");
}

#[test]
fn class_extends_override_parent_default() {
    let json = eval(
        r#"
open class Base {
    port: Int = 8080
    host: String = "localhost"
}
class Production extends Base {
    port: Int = 443
    tls: Boolean = true
}
x = new Production {}
"#,
    );
    assert_eq!(json["x"]["port"], 443);
    assert_eq!(json["x"]["host"], "localhost");
    assert_eq!(json["x"]["tls"], true);
}

#[test]
fn class_extends_instance_override() {
    // Instance overrides both parent and child defaults
    let json = eval(
        r#"
open class Base {
    x: Int = 1
    y: Int = 2
}
class Child extends Base {
    z: Int = 3
}
result = new Child {
    x = 10
    z = 30
}
"#,
    );
    assert_eq!(json["result"]["x"], 10);
    assert_eq!(json["result"]["y"], 2);
    assert_eq!(json["result"]["z"], 30);
}

#[test]
fn super_keyword_basic() {
    let json = eval(
        r#"
open class Base {
    greeting: String = "hello"
}
class Child extends Base {
    greeting: String = super.greeting + " world"
}
x = new Child {}
"#,
    );
    assert_eq!(json["x"]["greeting"], "hello world");
}

#[test]
fn super_keyword_field_access() {
    let json = eval(
        r#"
open class Config {
    port: Int = 8080
    url: String = "http://localhost"
}
class AppConfig extends Config {
    port: Int = 3000
    url: String = super.url + ":\(port)"
}
x = new AppConfig {}
"#,
    );
    assert_eq!(json["x"]["port"], 3000);
    assert_eq!(json["x"]["url"], "http://localhost:3000");
}

#[test]
fn class_extends_chain() {
    // Three-level inheritance chain
    let json = eval(
        r#"
open class A {
    x: Int = 1
}
open class B extends A {
    y: Int = 2
}
class C extends B {
    z: Int = 3
}
result = new C {}
"#,
    );
    assert_eq!(json["result"]["x"], 1);
    assert_eq!(json["result"]["y"], 2);
    assert_eq!(json["result"]["z"], 3);
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
fn hidden_in_nested_object_is_rejected() {
    let err = eval_fails(
        r#"
config {
    hidden internal = "private"
    public = "visible"
}
"#,
    );
    assert!(
        err.contains("Modifier `hidden` is not applicable to object members"),
        "{err}"
    );
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
    assert!(
        msg.contains("External members can only be defined by standard library modules"),
        "{msg}"
    );
}

#[tokio::test]
async fn const_cannot_override_in_amends() {
    let mut ev = pklr::eval::Evaluator::new_async();
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

#[test]
fn default_nested_merge() {
    let json = eval(
        r#"
services {
    default {
        config {
            timeout = 30
            retries = 3
        }
    }
    ["api"] {
        config {
            timeout = 60
        }
    }
}
"#,
    );
    // timeout overridden, retries inherited from default
    assert_eq!(json["services"]["api"]["config"]["timeout"], 60);
    assert_eq!(json["services"]["api"]["config"]["retries"], 3);
}

// ============================================================
// Type aliases
// ============================================================

#[test]
fn typealias_to_class() {
    // typealias acts as an alternative constructor for a class
    let json = eval(
        r#"
class Server {
    host: String = "localhost"
    port: Int = 8080
}
typealias Srv = Server
x = new Srv {
    port = 3000
}
"#,
    );
    assert_eq!(json["x"]["host"], "localhost");
    assert_eq!(json["x"]["port"], 3000);
}

#[test]
fn typealias_chain() {
    // Alias of an alias
    let json = eval(
        r#"
class Base {
    value: Int = 1
}
typealias A = Base
typealias B = A
x = new B {}
"#,
    );
    assert_eq!(json["x"]["value"], 1);
}

#[test]
fn typealias_simple_type_ignored() {
    // typealias to a simple type (not a class) is a no-op, shouldn't error
    let json = eval(
        r#"
typealias Name = String
x = "hello"
"#,
    );
    assert_eq!(json["x"], "hello");
}

#[test]
fn typealias_with_constraint() {
    // typealias with type constraint -- constraint is skipped but should parse
    let json = eval(
        r#"
typealias Port = Int(isBetween(1, 65535))
x = 8080
"#,
    );
    assert_eq!(json["x"], 8080);
}

#[test]
fn type_defaults_cover_literals_unions_and_collections() {
    let json = eval(
        r#"
literal: "only"
selected: "first" | *"second"
items: Listing<String>
mapping: Mapping<String, Int>
"#,
    );
    assert_eq!(json["literal"], "only");
    assert_eq!(json["selected"], "second");
    assert_eq!(json["items"], serde_json::json!([]));
    assert!(json.get("mapping").is_none());
}

#[test]
fn selected_structured_union_defaults_retain_semantics() {
    let json = eval(
        r#"
nullable: String | *Null
listing: String | *Listing<String>
nullableResult = nullable == null
stringMatches = "ok" is Int | *String
"#,
    );
    assert_eq!(json["nullableResult"], true);
    assert_eq!(json["listing"], serde_json::json!([]));
    assert_eq!(json["stringMatches"], true);
    assert!(json.get("nullable").is_none());
}

#[test]
fn union_without_selected_default_fails() {
    let message = eval_fails(r#"value: "first" | "second""#);
    assert!(message.contains("no selected default"));
}

#[test]
fn selected_union_member_without_implicit_value_stays_undefined() {
    let json = eval(
        r#"
optional: *String | Int
provided: *String | Int = "value"
"#,
    );
    assert!(json.get("optional").is_none());
    assert_eq!(json["provided"], "value");
}

#[test]
fn multiple_type_constraints_are_conjunctive() {
    let json = eval(
        r#"
local small = 5
local large = 20
smallMatches = small is Int(this > 0, this < 10)
largeMatches = large is Int(this > 0, this < 10)
"#,
    );
    assert_eq!(json["smallMatches"], true);
    assert_eq!(json["largeMatches"], false);
}

#[test]
fn failing_constraint_in_list_rejects_cast() {
    let message = eval_fails("result = 20 as Int(this > 0, this < 10)");
    assert!(message.contains("cannot cast"));
}

#[test]
fn typed_lambda_parameters_evaluate() {
    let json = eval(
        r#"
local choose = (value: String) -> value
result = choose("ok")
"#,
    );
    assert_eq!(json["result"], "ok");
}

#[test]
fn nullable_class_default_can_be_amended() {
    let json = eval(
        r#"
class Options { enabled: Boolean = false }
class Base { options: Options? }
base = new Base {}
result = (base) { options { enabled = true } }
"#,
    );
    assert_eq!(json["result"]["options"]["enabled"], true);
}

#[test]
fn nullable_amendment_prefers_existing_non_null_value() {
    let json = eval(
        r#"
class Options { enabled: Boolean = false; label: String = "default" }
class Base { options: Options? = new Options { enabled = true } }
base = new Base {}
result = (base) { options { label = "changed" } }
"#,
    );
    assert_eq!(json["result"]["options"]["enabled"], true);
    assert_eq!(json["result"]["options"]["label"], "changed");
}

#[test]
fn listing_body_amendment_appends_elements() {
    let json = eval(
        r#"
class Base { items: Listing<String> }
base = new Base {}
result = (base) {
  items {
    local prefix = ""
    "first"
    for (item in List("second")) { prefix + item }
    when (true) { "third" } else { "wrong" }
  }
}
"#,
    );
    assert_eq!(
        json["result"]["items"],
        serde_json::json!(["first", "second", "third"])
    );
}

#[test]
fn listing_body_amendment_applies_index_updates() {
    let json = eval(
        r#"
base { items = List("old", "stay") }
result = (base) {
  items {
    [0] = "new"
    "appended"
  }
}
"#,
    );
    assert_eq!(
        json["result"]["items"],
        serde_json::json!(["new", "stay", "appended"])
    );
}

#[test]
fn listing_index_body_amends_existing_element() {
    let json = eval(
        r#"
base {
  items = List(new Dynamic {
    kept = 1
    changed = 1
  })
}
result = (base) {
  items {
    [0] {
      changed = 2
      added = 3
    }
  }
}
"#,
    );
    assert_eq!(
        json["result"]["items"][0],
        serde_json::json!({"kept": 1, "changed": 2, "added": 3})
    );
}

#[test]
fn listing_named_members_are_not_elements() {
    let json = eval(
        r#"
items = new Listing {
  default = "template"
  "value"
}
"#,
    );
    assert_eq!(json["items"], serde_json::json!(["value"]));
}

#[test]
fn listing_shaped_body_preserves_existing_object_kind() {
    let json = eval(
        r#"
base {
  value = new Dynamic {
    kept = 1
  }
}
result = (base) {
  value {
    "element"
    added = 2
  }
}
"#,
    );
    assert_eq!(
        json["result"]["value"],
        serde_json::json!({"kept": 1, "added": 2})
    );
}

#[test]
fn poisoned_local_shadows_parent_binding() {
    let message = eval_fails(
        r#"
name = "outer"
result {
  local name = throw("local failed")
  value = name
}
"#,
    );
    assert!(message.contains("local failed"), "{message}");
}

#[test]
fn poisoned_local_is_preserved_in_closure_capture() {
    let message = eval_fails(
        r#"
name = "outer"
result {
  local name = throw("local failed")
  local getName = () -> name
  value = getName()
}
"#,
    );
    assert!(message.contains("local failed"), "{message}");
}

#[test]
fn current_binding_shadows_outer_poison_in_closure_capture() {
    let json = eval(
        r#"
local name = throw("outer failed")
result {
  local name = 1
  local getName = () -> name
  value = getName()
}
"#,
    );
    assert_eq!(json["result"]["value"], 1);
}

#[test]
fn inherited_late_binding_recomputes_dependency_chains() {
    let temp = TestTempDir::new("pklr_inherited_chain");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\na = this.b\nm = module.b\nb = c\nd = b + 1\nc = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\nc = 2\ne = d + 1\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["a"], 2);
    assert_eq!(json["m"], 2);
    assert_eq!(json["b"], 2);
    assert_eq!(json["d"], 3);
    assert_eq!(json["e"], 4);
}

#[test]
fn child_locals_recompute_after_inherited_properties_are_overridden() {
    let temp = TestTempDir::new("pklr_child_local_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nsource = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nlocal derived = source + 1\nresult = derived\nsource = 2\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["source"], 2);
    assert_eq!(json["result"], 3);
    assert!(json.get("derived").is_none());
}

#[test]
fn inherited_late_binding_preserves_parent_locals() {
    let temp = TestTempDir::new("pklr_parent_local_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nlocal offset = 1\nsource = 1\nderived = source + offset\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\nsource = 2\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["source"], 2);
    assert_eq!(json["derived"], 3);
    assert!(json.get("offset").is_none());
}

#[test]
fn inherited_late_binding_reaches_grandparent_declarations() {
    let temp = TestTempDir::new("pklr_grandparent_late_binding");
    std::fs::write(
        temp.path().join("Grand.pkl"),
        "abstract module Grand\nsource = 1\nderived = source + 1\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Parent.pkl"),
        "abstract module Parent\nextends \"Grand.pkl\"\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Parent.pkl\"\nsource = 2\nresult = derived\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["source"], 2);
    assert_eq!(json["derived"], 3);
    assert_eq!(json["result"], 3);
}

#[test]
fn computed_sibling_access_recomputes_after_child_override() {
    let temp = TestTempDir::new("pklr_computed_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nsource = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nlocal keyName = \"source\"\nresult = this[keyName]\nsource = 2\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["result"], 2);
}

#[test]
fn aliased_module_snapshot_recomputes_after_child_override() {
    let temp = TestTempDir::new("pklr_aliased_snapshot_late_binding");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nsource = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nlocal snapshot = this\nresult = snapshot.source\nsource = 2\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["result"], 2);
}

#[test]
fn amended_scope_only_defaults_stay_out_of_output() {
    let temp = TestTempDir::new("pklr_amended_scope_default");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "implicit: Mapping<String, String>\nvisible = implicit.length\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "amends \"Base.pkl\"\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert!(json.get("implicit").is_none());
    assert_eq!(json["visible"], 0);
}

#[test]
fn inherited_late_binding_propagates_errors() {
    let temp = TestTempDir::new("pklr_inherited_error");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nderived = 1 / denominator\ndenominator = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\ndenominator = 0\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap_err()
        .to_string();
    assert!(error.contains("division by zero"));
}

#[test]
fn constrained_nullable_mapping_preserves_value_defaults() {
    let json = eval(
        r#"
class Item { enabled: Boolean = false }
class Base { items: Mapping<String, Item>(length >= 0)? }
base = new Base {}
result = (base) { items { ["example"] { enabled = true } } }
"#,
    );
    assert_eq!(json["result"]["items"]["example"]["enabled"], true);
}

#[test]
fn used_unresolved_abstract_member_still_errors() {
    let temp = TestTempDir::new("pklr_abstract_member");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nabstract missing: String\ndependent = missing\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\nresult = dependent\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap_err()
        .to_string();
    assert!(error.contains("abstract property") || error.contains("undefined variable"));
}

#[test]
fn concrete_module_must_implement_inherited_abstract_property() {
    let temp = TestTempDir::new("pklr_required_abstract_member");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nabstract required: Listing<String>\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\n").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap_err()
        .to_string();
    assert!(error.contains("abstract property 'required'"));

    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nrequired = List(\"implemented\")\n",
    )
    .unwrap();
    let json = runtime
        .block_on(pklr::AsyncEvaluatorBuilder::new().eval_to_json(&child))
        .unwrap();
    assert_eq!(json["required"], serde_json::json!(["implemented"]));
}

#[test]
fn typealias_union_parses() {
    // Union type alias should parse without error
    let json = eval(
        r#"
typealias StringOrInt = String|Int
x = 42
"#,
    );
    assert_eq!(json["x"], 42);
}

#[test]
fn typealias_nullable_class() {
    // typealias Foo = Bar? should still work as constructor
    let json = eval(
        r#"
class Config {
    debug: Boolean = false
}
typealias MaybeConfig = Config?
x = new MaybeConfig {
    debug = true
}
"#,
    );
    assert_eq!(json["x"]["debug"], true);
}

#[test]
fn typealias_generic_parses() {
    // Generic type alias should parse without error
    let json = eval(
        r#"
typealias StringMap = Mapping<String, String>
x = new Mapping {
    ["a"] = "b"
}
"#,
    );
    assert_eq!(json["x"]["a"], "b");
}

// ============================================================
// is / as type operators
// ============================================================

#[test]
fn is_operator_string() {
    let json = eval(
        r#"
local x = "hello"
a = x is String
b = x is Int
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn is_operator_int() {
    let json = eval(
        r#"
local x = 42
a = x is Int
b = x is Number
c = x is String
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
}

#[test]
fn is_operator_null() {
    let json = eval(
        r#"
local x = null
a = x is Null
b = x is String?
c = x is String
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
}

#[test]
fn is_operator_nullable() {
    let json = eval(
        r#"
local x = "hello"
a = x is String?
b = x is Int?
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn is_operator_union() {
    let json = eval(
        r#"
local x = 42
local y = "hello"
a = x is String|Int
b = y is String|Int
c = x is String|Boolean
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
}

#[test]
fn is_operator_object_and_list() {
    let json = eval(
        r#"
local obj = new { x = 1 }
local lst = List(1, 2, 3)
a = obj is Object
b = lst is List
c = obj is List
d = lst is Object
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
    assert_eq!(json["d"], false);
}

#[test]
fn is_operator_user_defined_classes() {
    let json = eval(
        r#"
local class Step { check: String = "true" }
local class Ext { off: Boolean = false }
local plain = new Step {}
local ext = new Ext {}
plainIsStep = plain is Step
plainIsExt = plain is Ext
extIsStep = ext is Step
extIsExt = ext is Ext
"#,
    );
    assert_eq!(json["plainIsStep"], true);
    assert_eq!(json["plainIsExt"], false);
    assert_eq!(json["extIsStep"], false);
    assert_eq!(json["extIsExt"], true);
}

#[test]
fn is_operator_user_defined_class_inheritance_and_aliases() {
    let json = eval(
        r#"
open class Base { enabled: Boolean = true }
class Derived extends Base { name: String = "derived" }
class Other { name: String = "other" }
typealias BaseAlias = Base
local base = new Base {}
local derived = new Derived {}
local aliased = new BaseAlias {}
baseIsDerived = base is Derived
derivedIsBase = derived is Base
derivedIsAlias = derived is BaseAlias
derivedIsOther = derived is Other
aliasedIsBase = aliased is Base
constrainedBase = derived is Base(this.enabled)
"#,
    );
    assert_eq!(json["baseIsDerived"], false);
    assert_eq!(json["derivedIsBase"], true);
    assert_eq!(json["derivedIsAlias"], true);
    assert_eq!(json["derivedIsOther"], false);
    assert_eq!(json["aliasedIsBase"], true);
    assert_eq!(json["constrainedBase"], true);
}

#[test]
fn constrained_type_checks_preserve_nullable_and_generic_bases() {
    let json = eval(
        r#"
local items = List("one")
local nothing = null
typealias WholeNumber = Int
genericMatches = items is List<String>(this.length > 0)
nullableMatches = nothing is String?(this == null)
innerNullableRejectsNull = nothing is List<String?>(this == null)
aliasMatches = 42 is WholeNumber(this > 0)
"#,
    );
    assert_eq!(json["genericMatches"], true);
    assert_eq!(json["nullableMatches"], true);
    assert_eq!(json["innerNullableRejectsNull"], false);
    assert_eq!(json["aliasMatches"], true);
}

#[test]
fn as_operator_rejects_unrelated_user_defined_class() {
    let msg = eval_fails(
        r#"
class Left {}
class Right {}
result = new Left {} as Right
"#,
    );
    assert!(msg.contains("cannot cast Object to Right"), "{msg}");
}

#[tokio::test]
async fn is_operator_distinguishes_qualified_classes_with_the_same_name() {
    let dir = TestTempDir::new("pklr_is_qualified_classes");
    std::fs::write(
        dir.path.join("left.pkl"),
        "class Item { side = \"left\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path.join("right.pkl"),
        "class Item { side = \"right\" }\n",
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "left.pkl"
import "right.pkl"
local item = new left.Item {}
same = item is left.Item
different = item is right.Item
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["same"], true);
    assert_eq!(json["different"], false);
}

#[tokio::test]
async fn is_operator_distinguishes_local_and_imported_classes_with_the_same_name() {
    let dir = TestTempDir::new("pklr_is_local_and_imported_classes");
    std::fs::write(
        dir.path.join("imported.pkl"),
        r#"
open class Item {}
class Derived extends Item {}
instance = new Item {}
derived = new Derived {}
function make(): Item = new Item {}
"#,
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "imported.pkl"
class Item {}
local localItem = new Item {}
local importedDirect = new imported.Item {}
local importedValue = imported.instance
local importedDerived = imported.derived
local importedFromFunction = imported.make()
localIsImported = localItem is imported.Item
directIsImported = importedDirect is imported.Item
valueIsImported = importedValue is imported.Item
derivedIsImportedBase = importedDerived is imported.Item
functionValueIsImported = importedFromFunction is imported.Item
valueIsLocal = importedValue is Item
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["localIsImported"], false);
    assert_eq!(json["directIsImported"], true);
    assert_eq!(json["valueIsImported"], true);
    assert_eq!(json["derivedIsImportedBase"], true);
    assert_eq!(json["functionValueIsImported"], true);
    assert_eq!(json["valueIsLocal"], false);
}

#[tokio::test]
async fn imported_class_identity_survives_reexports_and_captured_imports() {
    let dir = TestTempDir::new("pklr_imported_class_identity");
    std::fs::write(
        dir.path.join("types.pkl"),
        r#"
class Item {}
instance = new Item {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path.join("wrapper.pkl"),
        r#"
import "types.pkl"
reexported = types.instance
amended = (types.instance) {}
function make(): types.Item = new types.Item {}
function accepts(value): Boolean = value is types.Item
"#,
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "types.pkl"
import "wrapper.pkl"
reexportedMatches = wrapper.reexported is types.Item
amendedMatches = wrapper.amended is types.Item
functionResultMatches = wrapper.make() is types.Item
capturedImportMatches = wrapper.accepts(types.instance)
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["reexportedMatches"], true);
    assert_eq!(json["amendedMatches"], true);
    assert_eq!(json["functionResultMatches"], true);
    assert_eq!(json["capturedImportMatches"], true);
}

#[tokio::test]
async fn imported_helper_preserves_nested_class_identity_in_returned_instance() {
    let dir = TestTempDir::new("pklr_imported_helper_nested_class_identity");
    std::fs::write(
        dir.path.join("Config.pkl"),
        r#"
class Expect {
    code: Int = 0
}
class Test {
    expect: Expect = new Expect {}
}
function marker(): Int = 1
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path.join("helpers.pkl"),
        r#"
import "Config.pkl"
class TestMaker {
    local function makeTest(expectedCode: Int): Config.Test = new Config.Test {
        expect = new Config.Expect { code = expectedCode }
    }
    function make(code: Int): Config.Test = makeTest(code)
}
class Container {
    test: Config.Test
}
"#,
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "Config.pkl"
import "helpers.pkl"
local maker = new helpers.TestMaker {}
marker = Config.marker()
result = new helpers.Container {
    test = maker.make(1)
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["marker"], 1);
    assert_eq!(json["result"]["test"]["expect"]["code"], 1);
}

#[tokio::test]
async fn ordinary_scope_objects_are_not_merged_as_partial_modules() {
    let dir = TestTempDir::new("pklr_ordinary_scope_objects");
    std::fs::write(dir.path.join("types.pkl"), "class Item {}\n").unwrap();
    std::fs::write(
        dir.path.join("base.pkl"),
        r#"
import "types.pkl"
local Config = new {
    ["Item"] = new types.Item {}
    selected = "base"
    stale = true
}
open class Holder {
    selected = Config.selected
    hasStale = Config.containsKey("stale")
}
holder = new Holder {}
"#,
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "types.pkl"
import "base.pkl"
local Config = new {
    ["Item"] = new types.Item {}
    selected = "current"
}
result = (base.holder) {}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["result"]["selected"], "base");
    assert_eq!(json["result"]["hasStale"], true);
}

#[tokio::test]
async fn partial_views_of_wrapper_with_distinct_reexports_are_merged() {
    let dir = TestTempDir::new("pklr_partial_wrapper_reexports");
    std::fs::write(
        dir.path.join("left.pkl"),
        "class Left { name = \"left\" }\ninstance = new Left {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path.join("right.pkl"),
        "class Right { name = \"right\" }\ninstance = new Right {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path.join("wrapper.pkl"),
        r#"
import "left.pkl"
import "right.pkl"
Left = left.instance
Right = right.instance
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path.join("helper.pkl"),
        r#"
import "wrapper.pkl" as Wrapper
open class Holder {
    left = Wrapper.Left.name
}
holder = new Holder {}
"#,
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "wrapper.pkl" as Wrapper
import "helper.pkl"
result = (helper.holder) {
    right = Wrapper.Right.name
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["result"]["left"], "left");
    assert_eq!(json["result"]["right"], "right");
}

#[tokio::test]
async fn partial_views_of_distinct_wrappers_are_not_merged() {
    let dir = TestTempDir::new("pklr_distinct_partial_wrappers");
    std::fs::write(
        dir.path.join("types.pkl"),
        "class Item {}\ninstance = new Item {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path.join("wrapper_a.pkl"),
        r#"
import "types.pkl"
Item = types.instance
baseOnly = true
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path.join("wrapper_b.pkl"),
        r#"
import "types.pkl"
Item = types.instance
currentOnly = true
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path.join("helper.pkl"),
        r#"
import "wrapper_a.pkl" as Wrapper
open class Holder {
    hasBase = Wrapper.containsKey("baseOnly")
    hasCurrent = Wrapper.containsKey("currentOnly")
}
holder = new Holder {}
"#,
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "wrapper_b.pkl" as Wrapper
import "helper.pkl"
selected = Wrapper.currentOnly
result = (helper.holder) {}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["selected"], true);
    assert_eq!(json["result"]["hasBase"], true);
    assert_eq!(json["result"]["hasCurrent"], false);
}

#[test]
fn is_operator_any() {
    let json = eval(
        r#"
a = 42 is Any
b = "hello" is Any
c = null is Any
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], true);
}

#[test]
fn as_operator_success() {
    let json = eval(
        r#"
local x = 42
result = x as Int
"#,
    );
    assert_eq!(json["result"], 42);
}

#[test]
fn as_operator_failure() {
    let msg = eval_fails(
        r#"
local x = "hello"
result = x as Int
"#,
    );
    assert!(msg.contains("cannot cast"));
}

#[test]
fn as_operator_nullable() {
    let json = eval(
        r#"
local x = null
result = x as String?
"#,
    );
    assert_eq!(json["result"], serde_json::Value::Null);
}

#[test]
fn is_in_conditional() {
    let json = eval(
        r#"
local x = 42
result = if (x is Int) "integer" else "other"
"#,
    );
    assert_eq!(json["result"], "integer");
}

// ============================================================
// Type constraints
// ============================================================

#[test]
fn constraint_is_check_pass() {
    let json = eval(
        r#"
local x = 42
result = x is Int(this >= 0)
"#,
    );
    assert_eq!(json["result"], true);
}

#[test]
fn constraint_is_check_fail() {
    let json = eval(
        r#"
local x = -1
result = x is Int(this >= 0)
"#,
    );
    assert_eq!(json["result"], false);
}

#[test]
fn constraint_is_wrong_base_type() {
    let json = eval(
        r#"
local x = "hello"
result = x is Int(this >= 0)
"#,
    );
    assert_eq!(json["result"], false);
}

#[test]
fn constraint_as_pass() {
    let json = eval(
        r#"
local x = 42
result = x as Int(this > 0)
"#,
    );
    assert_eq!(json["result"], 42);
}

#[test]
fn constraint_as_fail() {
    let msg = eval_fails(
        r#"
local x = -1
result = x as Int(this > 0)
"#,
    );
    assert!(msg.contains("cannot cast"));
}

#[test]
fn constraint_string_not_empty() {
    let json = eval(
        r#"
local a = "hello"
local b = ""
x = a is String(!isEmpty)
y = b is String(!isEmpty)
"#,
    );
    assert_eq!(json["x"], true);
    assert_eq!(json["y"], false);
}

#[test]
fn constraint_string_length() {
    let json = eval(
        r#"
local x = "hi"
a = x is String(length <= 5)
b = x is String(length > 10)
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn constraint_comparison() {
    let json = eval(
        r#"
local x = 8080
a = x is Int(this >= 1 && this <= 65535)
b = x is Int(this < 0)
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn typealias_with_constraint_enforced() {
    // typealias with constraint, checked via is
    let json = eval(
        r#"
typealias PositiveInt = Int(this > 0)
local x = 42
local y = -1
a = x is PositiveInt
b = y is PositiveInt
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn typealias_constraint_works_inside_amended_object() {
    // type aliases must be available inside amended/extended objects
    // (regression: flatten() used to drop type_aliases)
    let json = eval(
        r#"
typealias PositiveInt = Int(this > 0)
base {
    port = 8080
}
result = (base) {
    check = 42 is PositiveInt
}
"#,
    );
    assert_eq!(json["result"]["check"], true);
}

#[test]
fn typealias_constraint_inside_amended_object_rejects_invalid() {
    let json = eval(
        r#"
typealias PositiveInt = Int(this > 0)
local neg = 0 - 5
base {
    port = 8080
}
result = (base) {
    check = neg is PositiveInt
}
"#,
    );
    assert_eq!(json["result"]["check"], false);
}

// ============================================================
// Annotations
// ============================================================

#[test]
fn annotation_module_info_parsed() {
    // @ModuleInfo should be parsed without error
    let json = eval(
        r#"
@ModuleInfo { minPklVersion = "0.25.0" }
module my.Config
x = 42
"#,
    );
    assert_eq!(json["x"], 42);
}

#[test]
fn annotation_deprecated_property() {
    // @Deprecated annotation should not prevent evaluation
    let json = eval(
        r#"
@Deprecated { message = "use newName instead" }
oldName = "value"
result = oldName
"#,
    );
    assert_eq!(json["oldName"], "value");
    assert_eq!(json["result"], "value");
}

#[test]
fn annotation_multiple() {
    let json = eval(
        r#"
@Since { version = "1.0" }
@Deprecated { message = "removed in 2.0" }
legacy = true
current = false
"#,
    );
    assert_eq!(json["legacy"], true);
    assert_eq!(json["current"], false);
}

#[test]
fn annotation_on_class() {
    let json = eval(
        r#"
@Deprecated { message = "use NewConfig" }
class OldConfig {
    name: String = "old"
}
x = new OldConfig {}
"#,
    );
    assert_eq!(json["x"]["name"], "old");
}

#[test]
fn annotation_empty() {
    // @Foo with no body
    let json = eval(
        r#"
@Experimental
feature = true
"#,
    );
    assert_eq!(json["feature"], true);
}

// ============================================================
// Class extends
// ============================================================

#[test]
fn class_extends_inherits_defaults() {
    let json = eval(
        r#"
open class Animal {
    name: String = "unknown"
    legs: Int = 4
}
class Dog extends Animal {
    breed: String = "mixed"
}
x = new Dog {
    name = "Rex"
}
"#,
    );
    assert_eq!(json["x"]["name"], "Rex");
    assert_eq!(json["x"]["legs"], 4);
    assert_eq!(json["x"]["breed"], "mixed");
}

#[test]
fn class_extends_override_parent() {
    let json = eval(
        r#"
open class Base {
    value: Int = 1
}
class Child extends Base {
    value: Int = 2
    extra: String = "new"
}
x = new Child {}
"#,
    );
    assert_eq!(json["x"]["value"], 2);
    assert_eq!(json["x"]["extra"], "new");
}

// ============================================================
// Module extends
// ============================================================

#[tokio::test]
async fn module_extends_inherits_properties() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_module.pkl"
default_name = "extended"
extra = "new property"
"#;
    let path = base.join("test_extends.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    // default_name overridden
    assert_eq!(json["default_name"], "extended");
    // version inherited from base
    assert_eq!(json["version"], 1);
    // new property added
    assert_eq!(json["extra"], "new property");
}

#[tokio::test]
async fn module_extends_inherits_classes() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_module.pkl"
x = new Config {
    debug = true
}
"#;
    let path = base.join("test_extends_classes.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["x"]["debug"], true);
    assert_eq!(json["x"]["port"], 8080);
}

#[tokio::test]
async fn inherited_class_identity_uses_canonical_module_path() {
    let dir = TestTempDir::new("pklr_canonical_class_identity");
    std::fs::write(dir.path.join("base.pkl"), "class Item {}\n").unwrap();
    std::fs::write(
        dir.path.join("wrapper.pkl"),
        "extends \"./base.pkl\"\ninstance = new Item {}\n",
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "base.pkl"
import "wrapper.pkl"
same = wrapper.instance is base.Item
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&main).await.unwrap();
    assert_eq!(json["same"], true);
}

// ============================================================
// read() and read?()
// ============================================================

#[test]
fn read_env_variable() {
    unsafe { std::env::set_var("PKLR_TEST_VAR", "hello_pklr") };
    let json = eval(
        r#"
x = read("env:PKLR_TEST_VAR")
"#,
    );
    assert_eq!(json["x"], "hello_pklr");
    unsafe { std::env::remove_var("PKLR_TEST_VAR") };
}

#[test]
fn read_or_null_missing_env() {
    let json = eval(
        r#"
x = read?("env:DEFINITELY_NOT_SET_12345")
"#,
    );
    assert!(json["x"].is_null());
}

#[tokio::test]
async fn read_local_file() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
x = read("readme.txt")
"#;
    let path = base.join("test_read.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["x"], "Hello from pklr!\n");
}

#[tokio::test]
async fn read_file_uri() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let file_path = base.join("readme.txt");
    let src = format!(
        r#"
x = read("file://{}")
"#,
        file_path.display()
    );
    let path = base.join("test_read_file.pkl");
    let val = ev.eval_source(&src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["x"], "Hello from pklr!\n");
}

#[test]
fn read_or_null_missing_file() {
    let json = eval(
        r#"
x = read?("file:///nonexistent/path/to/file.txt")
"#,
    );
    assert!(json["x"].is_null());
}

#[test]
fn read_env_in_interpolation() {
    unsafe { std::env::set_var("PKLR_NAME", "world") };
    let json = eval(
        r#"
x = "hello \(read("env:PKLR_NAME"))"
"#,
    );
    assert_eq!(json["x"], "hello world");
    unsafe { std::env::remove_var("PKLR_NAME") };
}

// ============================================================
// Set() deduplication
// ============================================================

#[test]
fn set_deduplicates() {
    let json = eval(r#"x = Set(1, 2, 3, 2, 1)"#);
    assert_eq!(json["x"], serde_json::json!([1, 2, 3]));
}

#[test]
fn set_preserves_order() {
    let json = eval(r#"x = Set("b", "a", "c", "a")"#);
    assert_eq!(json["x"], serde_json::json!(["b", "a", "c"]));
}

#[test]
fn set_empty() {
    let json = eval(r#"x = Set()"#);
    assert_eq!(json["x"], serde_json::json!([]));
}

// ============================================================
// open modifier
// ============================================================

#[test]
fn open_class_allows_new_properties() {
    let json = eval(
        r#"
open class Config {
    port: Int = 8080
}
x = new Config {
    port = 9090
    host = "localhost"
}
"#,
    );
    assert_eq!(json["x"]["port"], 9090);
    assert_eq!(json["x"]["host"], "localhost");
}

#[test]
fn non_open_class_rejects_new_properties() {
    let msg = eval_fails(
        r#"
class Config {
    port: Int = 8080
}
x = new Config {
    port = 9090
    host = "localhost"
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("host"));
}

#[test]
fn non_open_class_rejects_dyn_property() {
    let msg = eval_fails(
        r#"
class Config {
    port: Int = 8080
}
x = new Config {
    ["host"] = "localhost"
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("host"));
}

#[test]
fn non_open_class_preserves_constraint_on_re_instantiation() {
    // After instantiating a non-open class, the is_open=false flag must be
    // preserved so that re-using the result as a base still enforces the constraint.
    let msg = eval_fails(
        r#"
class Config {
    port: Int = 8080
}
base = new Config { port = 9090 }
x = new Config {
    port = base.port
    host = "bad"
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("host"));
}

#[test]
fn non_open_class_allows_overrides() {
    let json = eval(
        r#"
class Config {
    port: Int = 8080
    debug: Boolean = false
}
x = new Config {
    port = 9090
    debug = true
}
"#,
    );
    assert_eq!(json["x"]["port"], 9090);
    assert_eq!(json["x"]["debug"], true);
}

// ============================================================
// Output block handling
// ============================================================

#[test]
fn output_block_is_skipped() {
    let json = eval(
        r#"
x = 1
output {
    renderer {
        converters {
            ["test"] = "hello"
        }
    }
}
"#,
    );
    assert_eq!(json["x"], 1);
    assert!(
        json.get("output").is_none(),
        "output should be skipped, got: {}",
        serde_json::to_string_pretty(&json).unwrap()
    );
}

// ============================================================
// new Dynamic
// ============================================================

#[test]
fn new_dynamic_creates_object() {
    let json = eval(
        r#"
x = new Dynamic {
    _type = "step"
    name = "test"
}
"#,
    );
    assert_eq!(json["x"]["_type"], "step");
    assert_eq!(json["x"]["name"], "test");
}

#[test]
fn new_dynamic_with_spread() {
    let json = eval(
        r#"
base {
    port = 8080
}
x = new Dynamic {
    _type = "config"
    ...base
}
"#,
    );
    assert_eq!(json["x"]["_type"], "config");
    assert_eq!(json["x"]["port"], 8080);
}

// ============================================================
// Class functions
// ============================================================

#[test]
fn class_function_basic() {
    let json = eval(
        r#"
class Greeter {
    name: String = "World"
    function greet(prefix: String): String = prefix + " " + name
}
g = new Greeter {}
result = g.greet("Hello")
"#,
    );
    assert_eq!(json["result"], "Hello World");
}

#[test]
fn null_safe_class_function_call() {
    let json = eval(
        r#"
class Greeter {
    name: String = "World"
    function greet(prefix: String): String = prefix + " " + name
}
g = new Greeter {}
a = g?.greet("Hello")
b = null?.greet("Hello")
"#,
    );
    assert_eq!(json["a"], "Hello World");
    assert_eq!(json["b"], serde_json::Value::Null);
}

#[test]
fn null_safe_unknown_method_errors_without_falling_through() {
    let error = eval_fails(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
result = g?.missing("Hello")
"#,
    );
    assert!(error.contains("unknown method 'missing' on Object"));
}

#[test]
fn null_safe_regex_constructor_emits_type_tag() {
    let json = eval(
        r##"
import "pkl:base" as base
glob = base?.Regex(#"^.*\.json$"#)
"##,
    );
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(json["glob"]["pattern"], r"^.*\.json$");
}

#[test]
fn null_safe_non_function_field_errors_like_regular_call() {
    let error = eval_fails(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
result = g?.name("Hello")
"#,
    );
    assert!(error.contains("cannot call non-function"));
}

#[test]
fn zero_arg_field_call_returns_field_value() {
    let json = eval(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
a = g.name()
b = g?.name()
"#,
    );
    assert_eq!(json["a"], "World");
    assert_eq!(json["b"], "World");
}

#[test]
fn regular_unknown_method_errors_without_falling_through() {
    let error = eval_fails(
        r#"
class Greeter {
    name: String = "World"
}
g = new Greeter {}
result = g.missing("Hello")
"#,
    );
    assert!(error.contains("unknown method 'missing' on Object"));
}

#[test]
fn class_lambda_valued_property_is_preserved() {
    let json = eval(
        r#"
class Transformer {
    transform = (x) -> x + 1
}
t = new Transformer {}
result = t.transform.apply(2)
"#,
    );
    assert_eq!(json["result"], 3);
    assert_eq!(json["t"]["transform"], "<lambda>");
}

#[test]
fn class_same_named_typed_property_is_preserved() {
    let json = eval(
        r#"
class Script {
    linux: String = "echo ok"
}

class Holder {
    Script = new Script {}
}

h = new Holder {}
"#,
    );
    assert_eq!(json["h"]["Script"]["linux"], "echo ok");
}

#[test]
fn class_function_testmaker_pattern() {
    // First check: does the class itself have checkFail?
    let json1 = eval(
        r#"
class TestMaker {
    filename: String = "file.txt"
    function checkFail(contents: String, code: Int): String = "check:" + filename
}
local testMaker = new TestMaker {}
result = testMaker.checkFail("bad", 1)
"#,
    );
    assert_eq!(json1["result"], "check:file.txt");

    // Second check: does it survive property override?
    let json2 = eval(
        r#"
class TestMaker {
    filename: String = "file.txt"
    function checkFail(contents: String, code: Int): String = "check:" + filename
}
local testMaker = new TestMaker { filename = "main.rs" }
result = testMaker.checkFail("bad", 1)
"#,
    );
    assert_eq!(json2["result"], "check:main.rs");
}

#[test]
fn class_function_with_local() {
    let json = eval(
        r#"
class Calc {
    base: Int = 10
    local function helper(x: Int): Int = x + base
    function compute(x: Int): Int = helper(x)
}
c = new Calc {}
result = c.compute(5)
"#,
    );
    assert_eq!(json["result"], 15);
}

// ============================================================
// hk.pkl compatibility
// ============================================================

#[test]
fn regex_constructor_emits_type_tag() {
    let json = eval(
        r##"
glob = Regex(#"^.*\.json$"#)
"##,
    );
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(json["glob"]["pattern"], r"^.*\.json$");
}

#[tokio::test]
async fn imported_regex_constructor_emits_type_tag() {
    let temp = TestTempDir::new("pklr_test_imported_regex_type_tag");
    let dir = temp.path();
    std::fs::write(
        dir.join("Types.pkl"),
        r#"
import "pkl:base"
function Regex(pattern: String) = base.Regex(pattern)
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("test.pkl"),
        r##"
import "Types.pkl"
glob = Types.Regex(#"^.*\.yaml$"#)
"##,
    )
    .unwrap();

    let mut ev = Evaluator::new_async();
    let path = dir.join("test.pkl");
    let val = ev
        .eval_source(&std::fs::read_to_string(&path).unwrap(), &path)
        .await
        .unwrap();
    let json = val.to_json();
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(json["glob"]["pattern"], r"^.*\.yaml$");
}

#[tokio::test]
async fn hk_step_regex_glob_emits_type_tag() {
    let temp = TestTempDir::new("pklr_test_hk_step_regex_glob");
    let dir = temp.path();
    std::fs::write(
        dir.join("Config.pkl"),
        r#"
import "pkl:base" as base
function Regex(pattern: String) = base.Regex(pattern)
class Step {
    glob: (String | List<String> | Regex)?
    check: String?
}
class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}
hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("hk.pkl"),
        r##"
amends "Config.pkl"
hooks {
    ["check"] {
        steps {
            ["regex-test"] {
                glob = Regex(#"^.*\.json$"#)
                check = "echo {{files}}"
            }
        }
    }
}
"##,
    )
    .unwrap();

    let mut ev = Evaluator::new_async();
    let path = dir.join("hk.pkl");
    let val = ev
        .eval_source(&std::fs::read_to_string(&path).unwrap(), &path)
        .await
        .unwrap();
    let json = val.to_json();
    let glob = &json["hooks"]["check"]["steps"]["regex-test"]["glob"];
    assert_eq!(glob["_type"], "regex");
    assert_eq!(glob["pattern"], r"^.*\.json$");
}

#[test]
fn hk_multiline_regex_pattern_is_normalized() {
    let json = eval(
        r####"
glob = Regex(#"""
    (?x)
    ^.*airflow\.template\.yaml$|
    ^chart/(?:templates|files)/.*\.yaml$
    """#)
"####,
    );
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(
        json["glob"]["pattern"],
        "(?x)\n^.*airflow\\.template\\.yaml$|\n^chart/(?:templates|files)/.*\\.yaml$\n"
    );
}

#[tokio::test]
async fn eval_amends_perf() {
    // Minimal amends test to check performance
    let temp = TestTempDir::new("pklr_test_perf");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
class Step {
    glob: (String | List<String>)?
    check: String?
    fix: String?
    check_first: Boolean = true
    batch: Boolean = false
}
class Hook {
    fix: Boolean?
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}
hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("test.pkl"),
        r#"
amends "Base.pkl"
hooks = new {
    ["pre-commit"] {
        fix = true
        steps = new {
            ["lint"] { check = "lint" }
        }
    }
}
"#,
    )
    .unwrap();
    let path = dir.join("test.pkl");
    let start = std::time::Instant::now();
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    let elapsed = start.elapsed();
    eprintln!("eval_amends_perf: {:?}", elapsed);
    assert!(
        elapsed.as_secs() < 5,
        "amends eval took too long: {:?}",
        elapsed
    );
    assert!(val["hooks"]["pre-commit"]["fix"] == true);
}

#[tokio::test]
async fn class_function_nested_in_new() {
    // Matches the hk builtin pattern: testMaker.checkFail() inside new Config.Step { tests { ... } }
    let temp = TestTempDir::new("pklr_test_nested");
    let dir = temp.path();
    std::fs::write(
        dir.join("helpers.pkl"),
        r#"
class TestMaker {
    filename: String = "file.txt"
    local function makeTest(runType: String, code: Int): String = runType + ":" + filename
    function checkFail(contents: String, code: Int): String = makeTest("check", code)
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "helpers.pkl"
local const testMaker = new helpers.TestMaker { filename = "src/main.rs" }
x {
    tests {
        ["check bad file"] = testMaker.checkFail("bad", 1)
    }
}
"#,
    )
    .unwrap();
    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["x"]["tests"]["check bad file"], "check:src/main.rs");
}

#[tokio::test]
async fn class_function_cross_module() {
    let temp = TestTempDir::new("pklr_test_cross_module");
    let dir = temp.path();
    std::fs::write(
        dir.join("helpers.pkl"),
        r#"
class TestMaker {
    filename: String = "file.txt"
    local function makeTest(runType: String, code: Int): String = runType + ":" + filename
    function checkFail(contents: String, code: Int): String = makeTest("check", code)
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "helpers.pkl"
local const testMaker = new helpers.TestMaker { filename = "main.rs" }
result = testMaker.checkFail("bad", 1)
"#,
    )
    .unwrap();
    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["result"], "check:main.rs");
}

#[test]
fn body_amendment_ignores_same_named_property_of_enclosing_object() {
    let val = eval(
        r#"
x {
  files = List("a")
  expect { files { ["a"] = "b" } }
}
"#,
    );
    assert_eq!(val["x"]["files"], serde_json::json!(["a"]));
    assert_eq!(val["x"]["expect"]["files"], serde_json::json!({"a": "b"}));
}

#[test]
fn body_amendment_ignores_inherited_property_of_enclosing_object() {
    let val = eval(
        r#"
class Test { files: List<String> = List("a"); expect: Dynamic = new Dynamic {} }
x = new Test {
  expect { files { ["a"] = "b" } }
}
"#,
    );
    assert_eq!(val["x"]["files"], serde_json::json!(["a"]));
    assert_eq!(val["x"]["expect"]["files"], serde_json::json!({"a": "b"}));
}

// ============================================================
// `module` inside class bodies
// ============================================================

#[test]
fn module_in_class_body_reads_module_property() {
    let val = eval(
        r#"
expected = "b"
class C { v = module.expected }
result = new C {}
"#,
    );
    assert_eq!(val["result"]["v"], "b");
}

#[test]
fn module_in_class_body_reads_property_declared_after_class() {
    let val = eval(
        r#"
class C { v = module.expected; w = "w" }
expected = "b"
result = new C {}
amended = new C { w = "x" }
overridden = new C { v = "o" }
"#,
    );
    assert_eq!(val["result"]["v"], "b");
    assert_eq!(val["amended"], serde_json::json!({"v": "b", "w": "x"}));
    assert_eq!(val["overridden"]["v"], "o");
    assert!(val.get("C").is_none());
}

#[test]
fn module_in_class_body_reaches_subclasses_and_local_functions() {
    let val = eval(
        r#"
open class C { v = module.expected; w = "w" }
class D extends C { u = module.other }
local function make(s) = new D { w = s }
local mk = (s) -> new C { w = s }
expected = "b"
other = "o"
d = new D {}
made = make("z")
lambda = mk("y")
"#,
    );
    assert_eq!(val["d"], serde_json::json!({"v": "b", "w": "w", "u": "o"}));
    assert_eq!(
        val["made"],
        serde_json::json!({"v": "b", "w": "z", "u": "o"})
    );
    assert_eq!(val["lambda"], serde_json::json!({"v": "b", "w": "y"}));
}

#[test]
fn module_in_class_body_before_property_is_evaluated_reports_error() {
    let err = eval_fails(
        r#"
class C { v = module.expected }
result = new C {}
expected = "b"
"#,
    );
    assert!(err.contains("expected"), "{err}");
}

#[test]
fn unused_class_reading_missing_module_property_is_not_an_error() {
    let val = eval("class C { v = module.missing }\nresult = 1\n");
    assert_eq!(val, serde_json::json!({"result": 1}));
}

#[test]
fn subclass_of_class_reading_missing_module_property_reports_error() {
    let err = eval_fails(
        r#"
class C { v = module.missing }
class D extends C { w = 1 }
result = new D {}
"#,
    );
    assert!(err.contains("missing"), "{err}");
}

#[test]
fn subclass_recovers_once_module_property_is_evaluated() {
    let val = eval(
        r#"
open class C { v = module.expected }
class D extends C { w = 1 }
expected = "b"
result = new D {}
"#,
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b", "w": 1}));
}

#[test]
fn type_alias_of_class_reading_module_is_refreshed() {
    let val = eval(
        r#"
class C { v = module.expected }
typealias A = C
expected = "b"
result = new A {}
"#,
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
}

#[test]
fn recovered_class_is_visible_through_module_and_this() {
    let val = eval(
        r#"
class C { v = module.expected }
expected = "b"
result = new module.C {}
"#,
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
    let val = eval(
        r#"
class C { v = module.expected }
function make() = new C {}
expected = "b"
viaModule = module.make()
viaThis = this.make()
"#,
    );
    assert_eq!(val["viaModule"], serde_json::json!({"v": "b"}));
    assert_eq!(val["viaThis"], serde_json::json!({"v": "b"}));
}

#[test]
fn class_reading_module_is_refreshed_only_where_used() {
    // Classes are refreshed lazily, before a property that can reach them:
    // by bare name, through a nested body, via a function, or `module.C`.
    let val = eval(
        r#"
open class C { v = module.expected }
function make() = new C {}
expected = "b"
unrelated = expected + "!"
holder { c = new C {} }
viaFunction = module.make()
later = "c"
direct = new C { w = module.later }
"#,
    );
    assert_eq!(val["unrelated"], "b!");
    assert_eq!(val["holder"]["c"], serde_json::json!({"v": "b"}));
    assert_eq!(val["viaFunction"], serde_json::json!({"v": "b"}));
    assert_eq!(val["direct"], serde_json::json!({"v": "b", "w": "c"}));
}

#[test]
fn class_read_in_type_constraint_is_refreshed() {
    let val = eval(
        r#"
class C { v = module.expected }
expected = "b"
result = 1 is Int(module.C.v == "b")
"#,
    );
    assert_eq!(val["result"], true);
    let val = eval(
        r#"
class C { v = module.expected }
typealias Ok = Int(module.C.v == "b")
expected = "b"
result = 1 is Ok
"#,
    );
    assert_eq!(val["result"], true);
}

#[test]
fn class_read_dynamically_through_module_is_refreshed() {
    let val = eval(
        r#"
local key = "C"
class C { v = module.expected }
class D { v = module[key].v }
expected = "b"
result = new D {}
"#,
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
}

#[test]
fn functions_reading_module_dynamically_are_refreshed() {
    for src in [
        "local key = \"C\"\nclass C { v = module.expected }\nlocal function make() = module[key].v\nexpected = \"b\"\nresult = make()\n",
        "local key = \"C\"\nclass C { v = module.expected }\nfunction make() = module[key].v\nexpected = \"b\"\nresult = make()\n",
    ] {
        assert_eq!(eval(src)["result"], "b", "{src}");
    }
}

#[test]
fn failed_class_reports_error_through_every_member_read() {
    for read in ["module[\"C\"]", "this[\"C\"]", "module?.C", "this?.C"] {
        let src = format!("class C {{ v = module.missing }}\nresult = {read}\n");
        let err = eval_fails(&src);
        assert!(err.contains("missing"), "{read}: {err}");
    }
}

#[test]
fn failed_module_local_is_not_a_member_of_module() {
    // Locals are not members of `module`/`this`, whether or not they failed,
    // even in a module whose classes are refreshed.
    let src = "class C { v = module.expected }\nlocal x = throw(\"boom\")\nexpected = \"b\"\n";
    let val = eval(&format!("{src}result = module?.x\nself = this?.x\n"));
    assert_eq!(val["result"], serde_json::Value::Null);
    assert_eq!(val["self"], serde_json::Value::Null);
    for read in ["module[\"x\"]", "this[\"x\"]"] {
        let err = eval_fails(&format!("{src}result = {read}\n"));
        assert!(err.contains("key not found: x"), "{read}: {err}");
    }
}

#[test]
fn members_declared_before_the_class_they_use_are_refreshed() {
    for (src, expected) in [
        (
            "function make() = new C {}\nclass C { v = module.expected }\nexpected = \"b\"\nresult = make()\n",
            serde_json::json!({"v": "b"}),
        ),
        (
            "local function make() = new C {}\nclass C { v = module.expected }\nexpected = \"b\"\nresult = make()\n",
            serde_json::json!({"v": "b"}),
        ),
        (
            "class D { c = new C {} }\nclass C { v = module.expected }\nexpected = \"b\"\nresult = new D {}\n",
            serde_json::json!({"c": {"v": "b"}}),
        ),
    ] {
        assert_eq!(eval(src)["result"], expected, "{src}");
    }
}

#[test]
fn dynamic_module_readers_are_refreshed_before_their_dependents() {
    // `C` reads `module[key]` and `D` reads `C`: `C` must be refreshed first.
    let val = eval(
        "local key = \"expected\"\nclass C { v = module[key] }\nclass D { v = module.C.v }\nexpected = \"b\"\nresult = new D {}\n",
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
    // A function using a class that reads `module` dynamically.
    let val = eval(
        "local key = \"expected\"\nfunction make() = new C {}\nclass C { v = module[key] }\nexpected = \"b\"\nresult = make()\n",
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
    // A dynamic reader of the module consumed by a function.
    let val = eval(
        "local key = \"C\"\nlocal function make() = new D {}\nclass D { c = module[key] }\nclass C { v = module.expected }\nexpected = \"b\"\nresult = make().c.v\n",
    );
    assert_eq!(val["result"], "b");
}

#[test]
fn refresh_repeats_for_members_read_before_they_recovered() {
    // Both classes read `module[...]`, so their order can't be derived and
    // `C` (declared first) is refreshed before `D`, which it reads. `D`
    // recovers in that pass, so `C` is refreshed again.
    let val = eval(
        "local k1 = \"D\"\nlocal k2 = \"expected\"\nclass C { v = module[k1].v }\nclass D { v = module[k2] }\nexpected = \"b\"\nresult = new C {}\n",
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
    // Functions re-bound in a repeated pass are changes too: `outerMake`
    // must see the `make` re-bound to the recovered `C`.
    let val = eval(
        "local k1 = \"D\"\nlocal k2 = \"expected\"\nlocal function make() = new C {}\nlocal function outerMake() = make()\nclass C { v = module[k1].v }\nclass D { v = module[k2] }\nexpected = \"b\"\nresult = outerMake().v\n",
    );
    assert_eq!(val["result"], "b");
}

#[test]
fn failed_class_reports_error_through_module_and_this() {
    for src in [
        "class C { v = module.missing }\nresult = new module.C {}\n",
        "class C { v = module.missing }\nresult = new this.C {}\n",
        "class C { v = module.missing }\nclass D extends module.C { w = 1 }\nresult = new D {}\n",
    ] {
        let err = eval_fails(src);
        assert!(err.contains("missing"), "{src}: {err}");
    }
}

#[test]
fn qualified_class_refs_are_refreshed_when_class_recovers() {
    for src in [
        "class C { v = module.expected }\nfunction make() = new module.C {}\nexpected = \"b\"\nresult = make()\n",
        "class C { v = module.expected }\nfunction make() = new this.C {}\nexpected = \"b\"\nresult = make()\n",
    ] {
        let val = eval(src);
        assert_eq!(val["result"], serde_json::json!({"v": "b"}), "{src}");
    }
    // A local holding a class is a value, not a type.
    for src in [
        "class C { v = module.expected }\nlocal x = module.C\nexpected = \"b\"\nresult = new x {}\n",
        "class C { v = module.expected }\nlocal x = this.C\nexpected = \"b\"\nresult = new x {}\n",
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains("Expected `x` to be a type, but it is not."),
            "{src}: {err}"
        );
    }
}

#[test]
fn module_function_building_class_reading_module_is_refreshed() {
    let val = eval(
        r#"
class C { v = module.expected }
function make() = new C {}
expected = "b"
result = make()
"#,
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
}

#[test]
fn outer_in_type_position_is_bound() {
    let json = eval(
        r#"
class Step { v = 1 }
obj {
  inner { s = new outer.Step {} }
  local x: outer.Step = new Step {}
  ok = x is outer.Step
}
"#,
    );
    assert_eq!(json["obj"]["inner"]["s"]["v"], 1);
    assert_eq!(json["obj"]["ok"], true);
}

#[test]
fn outer_is_bound_for_type_alias_constraints_checked_in_body() {
    let json = eval(
        r#"
limit = 5
typealias Checked = Int(this < outer.limit)
obj {
  ok = 3 is Checked
}
"#,
    );
    assert_eq!(json["obj"]["ok"], true);
}

#[test]
fn body_amendment_merges_member_from_when_generator() {
    let json = eval(
        r#"
base { when (true) { o { v = 1 } } }
x = (base) { o { w = 2 } }
y = (x) { o { z = 3 } }
"#,
    );
    assert_eq!(json["x"], serde_json::json!({"o": {"v": 1, "w": 2}}));
    assert_eq!(
        json["y"],
        serde_json::json!({"o": {"v": 1, "w": 2, "z": 3}})
    );
}

#[test]
fn body_amendment_merges_member_from_when_else_generator() {
    let json = eval(
        r#"
base { when (false) { o { v = 1 } } else { o { v = 2 } } }
x = (base) { o { w = 3 } }
"#,
    );
    assert_eq!(json["x"], serde_json::json!({"o": {"v": 2, "w": 3}}));
}

#[test]
fn class_instance_amendment_merges_member_from_generator() {
    let json = eval(
        r#"
class C { p: Dynamic = new { when (true) { o { v = 1 } } } }
c = new C { p { o { w = 2 } } }
nested = (c) { p { o { x = 3 } } }
class E { p: Dynamic = new { when (false) { o { v = 1 } } else { o { v = 2 } } } }
e = new E { p { o { w = 3 } } }
"#,
    );
    assert_eq!(json["c"], serde_json::json!({"p": {"o": {"v": 1, "w": 2}}}));
    assert_eq!(
        json["nested"],
        serde_json::json!({"p": {"o": {"v": 1, "w": 2, "x": 3}}})
    );
    assert_eq!(json["e"], serde_json::json!({"p": {"o": {"v": 2, "w": 3}}}));
}

#[test]
fn entry_amendment_merges_entry_from_for_generator() {
    let json = eval(
        r#"
base { for (k in List("a", "b")) { [k] { v = k } } }
x = (base) { ["a"] { w = 1 } }
class F { p: Dynamic = new { for (k in List("a", "b")) { [k] { v = k } } } }
f = new F { p { ["a"] { w = 1 } } }
"#,
    );
    let expected = serde_json::json!({"a": {"v": "a", "w": 1}, "b": {"v": "b"}});
    assert_eq!(json["x"], expected);
    assert_eq!(json["f"]["p"], expected);
// ============================================================
// Class extension and instantiation rules
// ============================================================

#[test]
fn class_without_body_is_an_empty_class() {
    let json = eval("open class Base\nclass Derived extends Base\nx = new Derived {}\n");
    assert_eq!(json["x"], serde_json::json!({}));
}

#[test]
fn invalid_supertypes_are_rejected() {
    for (src, message) in [
        (
            "class Base\nclass Derived extends Base\n",
            "Cannot extend non-open class `test#Base`.",
        ),
        (
            "open class Recurring extends Recurring {}\n",
            "Class `test#Recurring` cannot extend itself.",
        ),
        (
            "class Person extends Any\n",
            "Cannot extend external class `Any`.",
        ),
        (
            "class Person extends Dynamic\n",
            "Cannot extend non-open class `Dynamic`.",
        ),
        (
            "open class Foo\ntypealias Bar = Foo\nclass Baz extends Bar\n",
            "`Bar` is not a valid supertype.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
    // Open and abstract classes can be extended.
    let json = eval(
        "open class A { a = 1 }\nabstract class B extends A { b = 2 }\nclass C extends B\nx = new C {}\n",
    );
    assert_eq!(json["x"], serde_json::json!({"a": 1, "b": 2}));
}

#[test]
fn abstract_and_external_classes_cannot_be_instantiated() {
    for (src, message) in [
        (
            "abstract class Base\nres = new Base {}\n",
            "Cannot instantiate abstract class `test#Base`.",
        ),
        (
            "res = new String {}\n",
            "Cannot instantiate, or amend an instance of, external class `String`.",
        ),
        (
            "res = new Map {}\n",
            "Cannot instantiate, or amend an instance of, external class `Map`.",
        ),
        (
            "class Foo\nlocal Foo2 = Foo\nres = new Foo2 {}\n",
            "Expected `Foo2` to be a type, but it is not.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
}
