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

#[test]
fn invalid_module_body_reports_a_parse_error() {
    let msg = eval_fails("this is not valid pkl");
    assert!(msg.contains("Keyword `this` is not allowed here"), "{msg}");
    assert!(!msg.contains("Invalid property definition"), "{msg}");
}

// ============================================================
// pkl:test
// ============================================================

#[test]
fn test_catch_returns_error_message() {
    let json = eval(
        r#"
import "pkl:test"
class Bird { name = "Pigeon" }
local bird = new Bird {}
thrown = test.catch(() -> throw("boom"))
dynamic = test.catch(() -> new Dynamic { x = 1 }.y)
typed = test.catch(() -> bird.age)
caught = test.catchOrNull(() -> throw("boom"))
notThrown = test.catchOrNull(() -> 1) == null
"#,
    );
    assert_eq!(json["thrown"], "boom");
    assert_eq!(
        json["dynamic"],
        "Cannot find property `y` in object of type `Dynamic`."
    );
    assert_eq!(
        json["typed"],
        "Cannot find property `age` in object of type `test#Bird`."
    );
    assert_eq!(json["caught"], "boom");
    assert_eq!(json["notThrown"], true);
}

#[test]
fn test_catch_fails_without_an_error() {
    let msg = eval_fails(
        r#"
import "pkl:test"
x = test.catch(() -> 1)
"#,
    );
    assert!(msg.contains("Expected an exception, but none was thrown."));
}

#[test]
fn modules_extending_pkl_test_inherit_catch() {
    let json = eval(
        r#"
extends "pkl:test"
x = module.catch(() -> throw("boom"))
"#,
    );
    assert_eq!(json, serde_json::json!({ "x": "boom" }));
}

#[test]
fn modules_amending_pkl_test_inherit_catch() {
    let json = eval(
        r#"
amends "pkl:test"
local x = module.catch(() -> throw("boom"))
local y = catch(() -> throw("bare"))
examples { [x] = new Listing {}; [y] = new Listing {} }
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({ "examples": { "boom": [], "bare": [] } })
    );
}

#[test]
fn modules_amending_a_pkl_test_module_inherit_catch() {
    let temp = TestTempDir::new("pklr_test_amend_pkl_test_base");
    let dir = temp.path();
    std::fs::write(
        dir.join("base.pkl"),
        "open module base\nextends \"pkl:test\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
amends "base.pkl"
local x = module.catch(() -> throw("boom"))
local y = catch(() -> throw("bare"))
examples { [x] = new Listing {}; [y] = new Listing {} }
"#,
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        val,
        serde_json::json!({ "examples": { "boom": [], "bare": [] } })
    );
}

#[test]
fn generated_mapping_keys_cannot_repeat_in_one_body() {
    let json = eval(
        r#"
import "pkl:test"
local m = new Mapping { ["a"] = 0 }
forLoop = test.catch(() -> new Mapping { for (i in List(1, 2)) { ["a"] = i } })
direct = test.catch(() -> new Mapping { ["a"] = 1; when (true) { ["a"] = 2 } })
generatorAfterDirect = test.catch(() -> new Dynamic { ["a"] = 1; for (i in List(1)) { ["a"] = i } })
twoLoops = test.catch(() -> new Mapping { for (i in List(1)) { ["a"] = i } for (i in List(1)) { ["a"] = i } })
amendedTwice = test.catch(() -> (m) { for (i in List(1, 2)) { ["a"] = i } })
amendsParent = (m) { for (i in List(1)) { ["a"] = i } }
nextLayer = new Mapping { ["a"] = 1 } { ["a"] = 2 }
differentTypes = test.catchOrNull(() -> new Mapping<Any, Int> { [1] = 10; ["1"] = 20 })
local differentTypeKeys = new Mapping<Any, Int> { [1] = 10; ["1"] = 20 }
numberKey = differentTypeKeys[1]
stringKey = differentTypeKeys["1"]
differentTypeKeyCount = differentTypeKeys.length
typedKeys = differentTypeKeys.keys
"#,
    );
    let duplicate = "Duplicate definition of member `\"a\"`.";
    for key in [
        "forLoop",
        "direct",
        "generatorAfterDirect",
        "twoLoops",
        "amendedTwice",
    ] {
        assert_eq!(json[key], duplicate, "{key}");
    }
    assert_eq!(json["amendsParent"]["a"], 1);
    assert_eq!(json["nextLayer"]["a"], 2);
    assert!(json["differentTypes"].is_null());
    assert_eq!(json["numberKey"], 10);
    assert_eq!(json["stringKey"], 20);
    assert_eq!(json["differentTypeKeyCount"], 2);
    assert_eq!(json["typedKeys"], serde_json::json!([1, "1"]));
}

#[test]
fn amended_mapping_cannot_define_a_key_twice() {
    let json = eval(
        r#"
import "pkl:test"
local m = new Mapping { ["z"] = 0 }
direct = test.catch(() -> (m) { ["k"] = 1; ["" + "k"] = 2 }.length)
intAndFloat = test.catchOrNull(() -> new Mapping<Any, Int> { [1] = 10; [1.0] = 20 })
"#,
    );
    assert_eq!(json["direct"], "Duplicate definition of member `\"k\"`.");
    assert!(json["intAndFloat"].is_null());
}

#[test]
fn amended_mapping_cannot_amend_a_key_twice_in_one_body() {
    let msg = eval_fails(
        r#"
local m = new Mapping { ["nested"] = new Dynamic {} }
x = (m) { ["nested"] { a = 1 }; ["nested"] { b = 2 } }
"#,
    );
    assert!(
        msg.contains("Duplicate definition of member `\"nested\"`."),
        "{msg}"
    );
}

#[test]
fn declared_module_names_do_not_outlive_an_evaluation() {
    let temp = TestTempDir::new("pklr_test_module_names_reset");
    let dir = temp.path();
    let settings = dir.join("settings.pkl");
    let main = dir.join("main.pkl");
    std::fs::write(&main, "import \"settings.pkl\"\nx = settings.nope\n").unwrap();
    let mut ev = Evaluator::new();
    std::fs::write(&settings, "module company.Settings\na = 1\n").unwrap();
    let first = ev.eval_file(&main).unwrap_err().to_string();
    assert!(first.contains("in module `company.Settings`"), "{first}");
    std::fs::write(&settings, "a = 1\n").unwrap();
    let second = ev.eval_file(&main).unwrap_err().to_string();
    assert!(second.contains("in module `settings`"), "{second}");
}

#[test]
fn missing_property_messages_use_declared_module_names() {
    let temp = TestTempDir::new("pklr_test_declared_module_name");
    let dir = temp.path();
    std::fs::write(
        dir.join("settings.pkl"),
        "module company.Settings\nclass Bird { name = \"x\" }\nbird = new Bird {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "pkl:test"
import "settings.pkl"
typed = test.catch(() -> settings.bird.age)
"#,
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        val["typed"],
        "Cannot find property `age` in object of type `company.Settings#Bird`."
    );
    std::fs::write(
        dir.join("main.pkl"),
        "import \"settings.pkl\"\nx = settings.nope\n",
    )
    .unwrap();
    let err = pklr::eval_to_json(&dir.join("main.pkl"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Cannot find property `nope` in module `company.Settings`."));
}

#[test]
fn listing_index_amendments_must_name_a_parent_element() {
    let json = eval(
        r#"
import "pkl:test"
local x = new Listing { "one" }
amended = (x) { [0] = "uno" }
past = test.catch(() -> (x) { [1] = "two" })
added = test.catch(() -> (x) { "two"; [1] = "dos" })
negative = test.catch(() -> (x) { [-1] = "two" })
wrongType = test.catch(() -> (x) { ["0"] = "two" })
"#,
    );
    assert_eq!(json["amended"], serde_json::json!(["uno"]));
    assert_eq!(json["past"], "Element index `1` is out of range `0`..`0`.");
    assert_eq!(json["added"], "Element index `1` is out of range `0`..`0`.");
    assert_eq!(
        json["negative"],
        "Element index `-1` is out of range `0`..`0`."
    );
    assert_eq!(
        json["wrongType"],
        "Expected key of type `Int`, but got type `String`."
    );
}

#[test]
fn mapping_body_cannot_define_a_key_twice() {
    let json = eval(
        r#"
import "pkl:test"
local m = new Mapping { ["a"] = 1 }
amended = (m) { ["a"] = 2 }
duplicate = test.catch(() -> new Mapping { ["a"] = 1; ["" + "a"] = 2 })
"#,
    );
    assert_eq!(json["amended"]["a"], 2);
    assert_eq!(json["duplicate"], "Duplicate definition of member `\"a\"`.");
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
    let json = eval("local d = 5.min\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 5);
    assert_eq!(json["x"]["unit"], "min");
}

#[test]
fn duration_seconds() {
    let json = eval("local d = 3.s\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 3);
    assert_eq!(json["x"]["unit"], "s");
}

#[test]
fn duration_hours() {
    let json = eval("local d = 2.h\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 2);
    assert_eq!(json["x"]["unit"], "h");
}

#[test]
fn duration_days() {
    let json = eval("local d = 7.d\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 7);
    assert_eq!(json["x"]["unit"], "d");
}

#[test]
fn duration_milliseconds() {
    let json = eval("local d = 100.ms\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 100);
    assert_eq!(json["x"]["unit"], "ms");
}

#[test]
fn duration_nanoseconds() {
    let json = eval("local d = 50.ns\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 50);
    assert_eq!(json["x"]["unit"], "ns");
}

#[test]
fn duration_microseconds() {
    let json = eval("local d = 10.us\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 10);
    assert_eq!(json["x"]["unit"], "us");
}

#[test]
fn duration_float_value() {
    let json = eval("local d = 5.5.min\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 5.5);
    assert_eq!(json["x"]["unit"], "min");
}

// ============================================================
// Data sizes
// ============================================================

#[test]
fn datasize_bytes() {
    let json = eval("local d = 512.b\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 512);
    assert_eq!(json["x"]["unit"], "b");
}

#[test]
fn datasize_kilobytes() {
    let json = eval("local d = 10.kb\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 10);
    assert_eq!(json["x"]["unit"], "kb");
}

#[test]
fn datasize_megabytes() {
    let json = eval("local d = 256.mb\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 256);
    assert_eq!(json["x"]["unit"], "mb");
}

#[test]
fn datasize_gigabytes() {
    let json = eval("local d = 4.gb\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 4);
    assert_eq!(json["x"]["unit"], "gb");
}

#[test]
fn datasize_terabytes() {
    let json = eval("local d = 1.tb\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 1);
    assert_eq!(json["x"]["unit"], "tb");
}

#[test]
fn datasize_petabytes() {
    let json = eval("local d = 2.pb\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 2);
    assert_eq!(json["x"]["unit"], "pb");
}

#[test]
fn datasize_gibibytes() {
    let json = eval("local d = 8.gib\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 8);
    assert_eq!(json["x"]["unit"], "gib");
}

#[test]
fn datasize_mebibytes() {
    let json = eval("local d = 16.mib\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 16);
    assert_eq!(json["x"]["unit"], "mib");
}

#[test]
fn datasize_tebibytes() {
    let json = eval("local d = 1.tib\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 1);
    assert_eq!(json["x"]["unit"], "tib");
}

#[test]
fn datasize_pebibytes() {
    let json = eval("local d = 1.pib\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 1);
    assert_eq!(json["x"]["unit"], "pib");
}

#[test]
fn datasize_kibibytes() {
    let json = eval("local d = 64.kib\nx { value = d.value; unit = d.unit }");
    assert_eq!(json["x"]["value"], 64);
    assert_eq!(json["x"]["unit"], "kib");
}

#[test]
fn unicode_escape_without_braces_errors() {
    let msg = eval_fails(r#"x = "\u0041""#);
    assert!(msg.contains("Did you mean `{`?"));
}

#[test]
fn unicode_escape_empty_braces_errors() {
    let msg = eval_fails(r#"x = "\u{}""#);
    assert!(msg.contains("Invalid Unicode escape sequence"));
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

#[test]
fn const_cannot_override_in_amends() {
    let mut ev = pklr::eval::Evaluator::new();
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
    let result = ev.eval_source(src, &path);
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
    assert_eq!(json["mapping"], serde_json::json!({}));
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
fn list_body_amendment_rejects_external_list() {
    let error = eval_fails(
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
    assert!(error.contains("Cannot instantiate, or amend an instance of, external class `List`."));
}

#[test]
fn list_index_body_amendment_rejects_external_list() {
    let error = eval_fails(
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
    assert!(error.contains("Cannot instantiate, or amend an instance of, external class `List`."));
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
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
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
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
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
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
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
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
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
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
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
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
    assert_eq!(json["result"], 2);
}

#[test]
fn amended_typed_defaults_render_as_in_pkl() {
    let temp = TestTempDir::new("pklr_amended_scope_default");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "implicit: Mapping<String, String>\nvisible = implicit.length\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "amends \"Base.pkl\"\n").unwrap();
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
    assert_eq!(json["implicit"], serde_json::json!({}));
    assert_eq!(json["visible"], 0);
}

#[test]
fn inherited_late_binding_propagates_errors() {
    let temp = TestTempDir::new("pklr_inherited_error");
    std::fs::write(
        temp.path().join("Base.pkl"),
        "abstract module Base\nderived = 1 ~/ denominator\ndenominator = 1\n",
    )
    .unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "extends \"Base.pkl\"\ndenominator = 0\n").unwrap();
    let error = pklr::EvaluatorBuilder::new()
        .eval_to_json(&child)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Division by zero."), "{error}");
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
    let error = pklr::EvaluatorBuilder::new()
        .eval_to_json(&child)
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
    let error = pklr::EvaluatorBuilder::new()
        .eval_to_json(&child)
        .unwrap_err()
        .to_string();
    assert!(error.contains("abstract property 'required'"));

    std::fs::write(
        &child,
        "extends \"Base.pkl\"\nrequired = List(\"implemented\")\n",
    )
    .unwrap();
    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
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
local empty = null
typealias WholeNumber = Int
genericMatches = items is List<String>(this.length > 0)
nullableMatches = empty is String?(this == null)
innerNullableRejectsNull = empty is List<String?>(this == null)
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

#[test]
fn is_operator_distinguishes_qualified_classes_with_the_same_name() {
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

    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["same"], true);
    assert_eq!(json["different"], false);
}

#[test]
fn is_operator_distinguishes_local_and_imported_classes_with_the_same_name() {
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

    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["localIsImported"], false);
    assert_eq!(json["directIsImported"], true);
    assert_eq!(json["valueIsImported"], true);
    assert_eq!(json["derivedIsImportedBase"], true);
    assert_eq!(json["functionValueIsImported"], true);
    assert_eq!(json["valueIsLocal"], false);
}

#[test]
fn imported_class_identity_survives_reexports_and_captured_imports() {
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

    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["reexportedMatches"], true);
    assert_eq!(json["amendedMatches"], true);
    assert_eq!(json["functionResultMatches"], true);
    assert_eq!(json["capturedImportMatches"], true);
}

#[test]
fn imported_helper_preserves_nested_class_identity_in_returned_instance() {
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

    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["marker"], 1);
    assert_eq!(json["result"]["test"]["expect"]["code"], 1);
}

#[test]
fn ordinary_scope_objects_are_not_merged_as_partial_modules() {
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

    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["result"]["selected"], "base");
    assert_eq!(json["result"]["hasStale"], true);
}

#[test]
fn partial_views_of_wrapper_with_distinct_reexports_are_merged() {
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

    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["result"]["left"], "left");
    assert_eq!(json["result"]["right"], "right");
}

#[test]
fn partial_views_of_distinct_wrappers_are_not_merged() {
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

    let json = pklr::eval_to_json(&main).unwrap();
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

#[test]
fn module_extends_inherits_properties() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_module.pkl"
default_name = "extended"
extra = "new property"
"#;
    let path = base.join("test_extends.pkl");
    let val = ev.eval_source(src, &path).unwrap();
    let json = val.to_json();
    // default_name overridden
    assert_eq!(json["default_name"], "extended");
    // version inherited from base
    assert_eq!(json["version"], 1);
    // new property added
    assert_eq!(json["extra"], "new property");
}

#[test]
fn module_extends_inherits_classes() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
extends "base_module.pkl"
x = new Config {
    debug = true
}
"#;
    let path = base.join("test_extends_classes.pkl");
    let val = ev.eval_source(src, &path).unwrap();
    let json = val.to_json();
    assert_eq!(json["x"]["debug"], true);
    assert_eq!(json["x"]["port"], 8080);
}

#[test]
fn inherited_class_identity_uses_canonical_module_path() {
    let dir = TestTempDir::new("pklr_canonical_class_identity");
    std::fs::write(
        dir.path.join("base.pkl"),
        "open module base\nclass Item {}\n",
    )
    .unwrap();
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

    let json = pklr::eval_to_json(&main).unwrap();
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

#[test]
fn read_local_file() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
x = read("readme.txt")
"#;
    let path = base.join("test_read.pkl");
    let val = ev.eval_source(src, &path).unwrap();
    let json = val.to_json();
    assert_eq!(json["x"]["text"], "Hello from pklr!\n");
}

#[test]
fn read_file_uri() {
    let mut ev = pklr::eval::Evaluator::new();
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
    let val = ev.eval_source(&src, &path).unwrap();
    let json = val.to_json();
    assert_eq!(json["x"]["text"], "Hello from pklr!\n");
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

// ============================================================
// Resources
// ============================================================

#[test]
fn resources_are_module_relative_and_preserve_bytes() {
    let temp = TestTempDir::new("pklr_test_resources_relative");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("nested/data.bin"), [0, 255, b'x']).unwrap();
    std::fs::write(dir.join("nested/main.pkl"), "value = read(\"data.bin\")\n").unwrap();
    let value = pklr::eval_to_json(&dir.join("nested/main.pkl")).unwrap();
    assert_eq!(value["value"]["base64"], "AP94");
    assert!(
        value["value"]["uri"]
            .as_str()
            .unwrap()
            .ends_with("nested/data.bin")
    );
}

#[test]
fn resource_read_question_only_suppresses_missing_resources() {
    let temp = TestTempDir::new("pklr_test_resources_denied");
    let path = temp.path().join("main.pkl");
    std::fs::write(
        &path,
        "missing = read?(\"missing.txt\")\ndenied = read?(\"env:HOME\")\n",
    )
    .unwrap();
    let error = pklr::EvaluatorBuilder::new()
        .allowed_resources(vec!["file:".to_string()])
        .eval_to_json(&path)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Refusing to read resource `env:HOME`"),
        "{error}"
    );
}

#[test]
fn resource_allowlist_normalizes_paths_before_io() {
    let temp = TestTempDir::new("pklr_test_resources_allowlist");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("allowed")).unwrap();
    std::fs::write(dir.join("secret.txt"), "nope").unwrap();
    let path = dir.join("allowed/main.pkl");
    std::fs::write(&path, "x = read(\"../secret.txt\")\n").unwrap();
    let allowed = format!("file://{}/allowed/", dir.display());
    let error = pklr::EvaluatorBuilder::new()
        .allowed_resources(vec![allowed])
        .eval_to_json(&path)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Refusing to read resource"), "{error}");

    // A non-directory URI prefix remains a prefix, as documented by the
    // builder, while a trailing slash above keeps directory boundaries.
    std::fs::write(dir.join("allowed/config-current.txt"), "prefix").unwrap();
    let path = dir.join("allowed/main.pkl");
    std::fs::write(&path, "value = read(\"config-current.txt\")\n").unwrap();
    let allowed = format!("file:{}/allowed/config-", dir.display());
    let value = pklr::EvaluatorBuilder::new()
        .allowed_resources(vec![allowed])
        .eval_to_json(&path)
        .unwrap();
    assert_eq!(value["value"]["text"], "prefix");

    let value = pklr::EvaluatorBuilder::new()
        .allowed_resources(vec!["file:///".to_string()])
        .eval_to_json(&path)
        .unwrap();
    assert_eq!(value["value"]["text"], "prefix");
}

#[test]
fn resource_env_prop_and_file_globs() {
    let temp = TestTempDir::new("pklr_test_resources_glob");
    let dir = temp.path();
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    std::fs::write(dir.join("b.txt"), "b").unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(
        &path,
        "envs = read*(\"env:APP_*\")\nprops = read*(\"prop:app*\")\nfiles = read*(\"*.txt\")\n",
    )
    .unwrap();
    let value = pklr::EvaluatorBuilder::new()
        .environment_variables(vec![("APP_MODE".to_string(), "test".to_string())])
        .external_properties(vec![("apple".to_string(), "pie".to_string())])
        .eval_to_json(&path)
        .unwrap();
    assert_eq!(value["envs"]["env:APP_MODE"], "test");
    assert_eq!(value["props"]["prop:apple"], "pie");
    assert_eq!(value["files"]["a.txt"]["text"], "a");
    assert_eq!(value["files"]["b.txt"]["text"], "b");
}

#[test]
fn resource_glob_keys_stay_module_relative_and_braces_do_not_expand() {
    let temp = TestTempDir::new("pklr_test_resources_glob_keys");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::create_dir_all(dir.join("literal")).unwrap();
    std::fs::write(dir.join("literal/value.txt"), "value").unwrap();
    std::fs::write(
        dir.join("nested/main.pkl"),
        "values = read*(\"../literal/*.txt\")\n",
    )
    .unwrap();
    let value = pklr::eval_to_json(&dir.join("nested/main.pkl")).unwrap();
    assert_eq!(value["values"]["../literal/value.txt"]["text"], "value");

    // This is deliberately invoked through read* so the production resolver,
    // not a test-only matcher, compiles the adjacent groups.
    let pattern = std::iter::repeat_n("{a,b}", 30).collect::<String>();
    let name = "a".repeat(30);
    std::fs::write(dir.join(&name), "safe").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        format!("values = read*(\"{pattern}\")\n"),
    )
    .unwrap();
    let value = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(value["values"][name]["text"], "safe");
}

#[test]
fn resource_file_allowlist_accepts_equivalent_uri_spellings() {
    let temp = TestTempDir::new("pklr_test_resources_uri_spellings");
    let dir = temp.path();
    std::fs::write(dir.join("data.txt"), "allowed").unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(&path, "value = read(\"data.txt\")\n").unwrap();
    let allowed = format!("file:{}", dir.display());
    let value = pklr::EvaluatorBuilder::new()
        .allowed_resources(vec![allowed])
        .eval_to_json(&path)
        .unwrap();
    assert_eq!(value["value"]["text"], "allowed");

    std::fs::create_dir_all(dir.join("allowed")).unwrap();
    std::fs::create_dir_all(dir.join("allowed-sibling")).unwrap();
    std::fs::write(dir.join("allowed-sibling/secret.txt"), "secret").unwrap();
    let path = dir.join("allowed/main.pkl");
    std::fs::write(&path, "value = read(\"../allowed-sibling/secret.txt\")\n").unwrap();
    let allowed = format!("file:{}/allowed/", dir.display());
    let error = pklr::EvaluatorBuilder::new()
        .allowed_resources(vec![allowed])
        .eval_to_json(&path)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Refusing to read resource"), "{error}");
}

#[test]
fn narrow_native_allowlist_keeps_missing_relative_resources_nullable() {
    let temp = TestTempDir::new("pklr_test_resources_missing_narrow");
    let dir = temp.path().join("safe");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(&path, "value = read?(\"missing.txt\")\n").unwrap();
    let allowed = format!("file:{}/", dir.display());
    let value = pklr::EvaluatorBuilder::new()
        .allowed_resources(vec![allowed])
        .eval_to_json(&path)
        .unwrap();
    assert!(value["value"].is_null());

    // The module itself need not exist for eval_source. Its existing relative
    // parent still establishes the native allowlist base.
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let allowed = format!("file:{}/", fixture.display());
    let mut evaluator = pklr::eval::Evaluator::new();
    evaluator.set_allowed_resources(vec![allowed]);
    let value = evaluator
        .eval_source(
            "value = read?(\"missing-from-unsaved.txt\")\n",
            std::path::Path::new("tests/fixtures/unsaved-resource-module.pkl"),
        )
        .unwrap()
        .to_json();
    assert!(value["value"].is_null());

    let cwd = std::env::current_dir().unwrap();
    let allowed = format!("file:{}/", cwd.display());
    let mut evaluator = pklr::eval::Evaluator::new();
    evaluator.set_allowed_resources(vec![allowed]);
    let value = evaluator
        .eval_source(
            "value = read?(\"missing-from-bare-unsaved.txt\")\n",
            std::path::Path::new("unsaved-resource-module.pkl"),
        )
        .unwrap()
        .to_json();
    assert!(value["value"].is_null());
}

#[test]
fn resource_glob_rejects_triple_dot_before_traversal() {
    let error = eval_fails(r#"x = read*(".../secret/*.txt")"#);
    assert!(
        error.contains("Cannot combine resource globs with triple-dot"),
        "{error}"
    );
}

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

#[test]
fn sets_cannot_be_indexed() {
    let error = eval_fails(r#"x = Set(1, 2)[0]"#);
    assert!(error.contains("cannot index Set"), "{error}");
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
fn class_instance_rejects_entries() {
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
    assert!(
        msg.contains("Object of type `test#Config` cannot have an entry."),
        "{msg}"
    );
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

#[test]
fn imported_regex_constructor_emits_type_tag() {
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

    let mut ev = Evaluator::new();
    let path = dir.join("test.pkl");
    let val = ev
        .eval_source(&std::fs::read_to_string(&path).unwrap(), &path)
        .unwrap();
    let json = val.to_json();
    assert_eq!(json["glob"]["_type"], "regex");
    assert_eq!(json["glob"]["pattern"], r"^.*\.yaml$");
}

#[test]
fn hk_step_regex_glob_emits_type_tag() {
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

    let mut ev = Evaluator::new();
    let path = dir.join("hk.pkl");
    let val = ev
        .eval_source(&std::fs::read_to_string(&path).unwrap(), &path)
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
        "(?x)\n^.*airflow\\.template\\.yaml$|\n^chart/(?:templates|files)/.*\\.yaml$"
    );
}

#[test]
fn eval_amends_perf() {
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
    let val = pklr::eval_to_json(&path).unwrap();
    let elapsed = start.elapsed();
    eprintln!("eval_amends_perf: {elapsed:?}");
    assert!(
        elapsed.as_secs() < 5,
        "amends eval took too long: {elapsed:?}"
    );
    assert!(val["hooks"]["pre-commit"]["fix"] == true);
}

#[test]
fn class_function_nested_in_new() {
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
    let val = pklr::eval_to_json(&path).unwrap();
    assert_eq!(val["x"]["tests"]["check bad file"], "check:src/main.rs");
}

#[test]
fn class_function_cross_module() {
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
    let val = pklr::eval_to_json(&path).unwrap();
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
fn module_qualified_class_read_is_not_shadowed_by_class_member() {
    let val = eval(
        r#"
class C { expected = "class"; v = module.expected }
expected = "module"
result = new C {}
"#,
    );
    assert_eq!(val["result"]["v"], "module");
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
fn module_in_class_body_reads_property_declared_after_use() {
    let val = eval(
        r#"
class C { v = module.expected }
result = new C {}
expected = "b"
"#,
    );
    assert_eq!(val["result"], serde_json::json!({"v": "b"}));
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
        (
            "local Foo = new Dynamic {\n  @Deprecated { message = \"old\" }\n  value = 1\n}\nres = new Foo {}\n",
            "Expected `Foo` to be a type, but it is not.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
}

#[test]
fn instantiation_checks_follow_type_aliases() {
    let err = eval_fails("typealias R = Regex\nres = new R {}\n");
    assert!(
        err.contains("Cannot instantiate, or amend an instance of, external class `Regex`."),
        "{err}"
    );
    let json =
        eval("class Foo { x = 1 }\ntypealias A = Foo\nlocal f = () -> new A {}\nr = f.apply()\n");
    assert_eq!(json["r"], serde_json::json!({"x": 1}));
    // A parameter named like the class doesn't change what the alias names.
    let json = eval(
        "class Foo { x = 1 }\ntypealias A = Foo\nlocal f = (Foo) -> new A {}\nr = f.apply(5)\n",
    );
    assert_eq!(json["r"], serde_json::json!({"x": 1}));
    let err = eval_fails(
        "typealias A1 = A2\ntypealias A2 = A3\ntypealias A3 = A4\ntypealias A4 = A5\ntypealias A5 = A6\ntypealias A6 = A7\ntypealias A7 = A8\ntypealias A8 = A9\ntypealias A9 = Regex\nres = new A1 {}\n",
    );
    assert!(
        err.contains("Cannot instantiate, or amend an instance of, external class `Regex`."),
        "{err}"
    );
}

#[test]
fn min_pkl_version_folds_constant_strings() {
    let err =
        eval_fails("@ModuleInfo { minPklVersion = \"99.\" + \"9.9\" }\nmodule future\nx = 1\n");
    assert!(
        err.contains("Module `future` requires Pkl version 99.9.9 or higher"),
        "{err}"
    );
}

// ============================================================
// Iteration and element rules
// ============================================================

#[test]
fn generators_and_spreads_reject_non_iterable_values() {
    for (src, message) in [
        (
            "foo {\n  for (n in 5) { n }\n}\n",
            "Cannot iterate over value of type `Int`.",
        ),
        (
            "foo = new Listing {\n  for (c in \"abc\") { c }\n}\n",
            "Cannot iterate over value of type `String`.",
        ),
        (
            "class Person\nfoo {\n  for (_ in new Person {}) { 42 }\n}\n",
            "Cannot iterate over value of type `test#Person`.",
        ),
        (
            "source = null\nres { ...source }\n",
            "Cannot iterate over value of type `Null`.",
        ),
        (
            "res = new Listing { ...1 }\n",
            "Cannot iterate over value of type `Int`.",
        ),
        (
            "class Person { name = \"Bob\" }\nres { ...new Person {} }\n",
            "Cannot iterate over value of type `test#Person`.",
        ),
        (
            "res = new Mapping { ...List(1, 2) }\n",
            "Cannot spread value of type `List` into object of type `Mapping`.",
        ),
        (
            "local d = new Dynamic { b = 2 }\nres = new Listing { ...d }\n",
            "Cannot spread object containing properties into object of type `Listing`.",
        ),
        (
            "local m = new Mapping { [\"a\"] = 1 }\nres = new Listing { ...m }\n",
            "Cannot spread object containing entries into object of type `Listing`.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
    // Collections, Listings, Mappings and Dynamic objects can be iterated.
    let json = eval(
        r#"
local m = new Mapping { ["a"] = 1 }
res = new Listing {
  for (x in List(1)) { x }
  for (x in new Listing { 2 }) { x }
  for (_, v in m) { v }
  ...List(2)
}
"#,
    );
    assert_eq!(json["res"], serde_json::json!([1, 2, 1, 2]));
}

#[test]
fn typed_objects_cannot_have_elements() {
    for src in [
        "class Foo { names: Listing<String> }\nfoo = new Foo {\n  (names) { \"x\" }\n}\n",
        "class Foo { names: Listing<String> }\nfoo = new Foo {}\nbar = (foo) {\n  when (true) { \"x\" }\n}\n",
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains("Object of type `test#Foo` cannot have an element."),
            "{src}: {err}"
        );
    }
    // Amending a property that is a Listing can add elements.
    let json =
        eval("class Foo { names: Listing<String> }\nfoo = new Foo {\n  names { \"x\" }\n}\n");
    assert_eq!(json["foo"]["names"], serde_json::json!(["x"]));
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
}

#[test]
fn amending_the_wrong_kind_of_parent_is_rejected() {
    for (src, message) in [
        (
            "res = (5) { \"pigeon\" }\n",
            "Cannot instantiate, or amend an instance of, external class `Int`.",
        ),
        (
            "res = (\"s\") { [\"pigeon\"] = true }\n",
            "Cannot instantiate, or amend an instance of, external class `String`.",
        ),
        (
            "class Person {}\nres = new Person { \"pigeon\" }\n",
            "Object of type `test#Person` cannot have an element.",
        ),
        (
            "class Person {}\nres = new Person { [\"pigeon\"] = true }\n",
            "Object of type `test#Person` cannot have an entry.",
        ),
        (
            "res = new ValueRenderer { \"pigeon\" }\n",
            "Cannot instantiate abstract class `ValueRenderer`.",
        ),
        (
            "typealias R = ValueRenderer\nres = new R {}\n",
            "Cannot instantiate abstract class `ValueRenderer`.",
        ),
        (
            "res = new Mapping { \"pigeon\" }\n",
            "Object of type `Mapping` cannot have an element.",
        ),
        (
            "res = (new Mapping {}) { \"pigeon\" }\n",
            "Object of type `Mapping` cannot have an element.",
        ),
        (
            "res = (new Mapping {}) { ...List(1, 2) }\n",
            "Cannot spread value of type `List` into object of type `Mapping`.",
        ),
        (
            "res = (null) { pigeon = true }\n",
            "Cannot instantiate, or amend an instance of, external class `Null`.",
        ),
    ] {
        let err = eval_fails(src);
        assert!(err.contains(message), "{src}: {err}");
    }
}

#[test]
fn bare_mapping_amendment_preserves_default_for_later_amendments() {
    let json = eval(
        r#"
local base = (new Mapping {}) { default { enabled = true } }
result = (base) { ["example"] {} }
"#,
    );
    assert_eq!(
        json["result"],
        serde_json::json!({"example": {"enabled": true}})
    );
}
#[test]
fn nested_amendments_of_class_instances_reject_elements() {
    for src in [
        "class Foo { a = 1 }\nm = new Mapping<String, Foo> { [\"a\"] { \"ignored\" } }\n",
        "class Foo { a = 1 }\nclass Bar { f: Foo }\nb = new Bar { f { \"x\" } }\n",
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains("Object of type `test#Foo` cannot have an element."),
            "{src}: {err}"
        );
    }
}

#[test]
fn declared_property_types_check_aliases_constraints_and_late_members() {
    let alias = eval_fails(
        r#"
typealias IsB = String(this == "b")
checked: IsB = "x"
"#,
    );
    assert!(alias.contains("property 'checked'"), "{alias}");

    let later = eval_fails(
        r#"
checked: Int(this < limit) = 1
limit = 0
"#,
    );
    assert!(later.contains("property 'checked'"), "{later}");
}

#[test]
fn declared_property_type_checks_wait_for_instances_and_cover_body_forms() {
    let json = eval(
        r#"
class C { value: Int = "bad" }
instance = new C { value = 1 }
fn: Function1<Int, Int> = (x) -> x
"#,
    );
    assert_eq!(json["instance"]["value"], 1);

    let class_default = eval_fails("class C { value: Int = \"bad\" }\ninstance = new C {}\n");
    assert!(
        class_default.contains("property 'value'"),
        "{class_default}"
    );

    let body = eval_fails("items: Listing(this.length == 1) = new Listing { 1; 2 }\n");
    assert!(body.contains("property 'items'"), "{body}");
}

#[test]
fn amended_module_checks_the_final_declared_property_value() {
    let temp = TestTempDir::new("pklr_amended_declared_property_type");
    std::fs::write(temp.path().join("Base.pkl"), "foo: Int(this > 0) = -1\n").unwrap();
    let child = temp.path().join("Child.pkl");
    std::fs::write(&child, "amends \"Base.pkl\"\nfoo = 5\n").unwrap();

    let json = pklr::EvaluatorBuilder::new().eval_to_json(&child).unwrap();
    assert_eq!(json["foo"], 5);
}

#[test]
fn generator_body_amends_the_receiver_member() {
    let json = eval(
        r#"
base { o { v = 1 } }
property = (base) { when (true) { o { w = 2 } } }
entryBase { ["k"] { v = 1 } }
entry = (entryBase) { for (_ in List(1)) { ["k"] { w = 2 } } }
"#,
    );
    assert_eq!(json["property"], serde_json::json!({"o": {"v": 1, "w": 2}}));
    assert_eq!(json["entry"], serde_json::json!({"k": {"v": 1, "w": 2}}));
}

#[test]
fn inherited_body_listings_are_not_applied_twice_when_amended() {
    let result = eval(
        r#"
b { l { 1 } }
b2 = (b) { l { 2 } }
b3 = (b2) { l { 3 } }
class D { l: Listing<Int> }
d = new D { l { 1 } }
d2 = (d) { l { 2 } }
"#,
    );
    assert_eq!(result["b2"]["l"], serde_json::json!([1, 2]));
    assert_eq!(result["b3"]["l"], serde_json::json!([1, 2, 3]));
    assert_eq!(result["d2"]["l"], serde_json::json!([1, 2]));
}

#[test]
fn untyped_new_uses_declared_and_inherited_property_types() {
    let json = eval(
        r#"
class P { a = 1; b = 2 }
open class C { l: Listing<Int> = new { 1 }; p: P = new { a = 5 } }
open class D extends C { l = new { 2 }; p = new { b = 3 } }
class E extends D { p = new { b = 9 } }
typed: Mapping<String, Int> = new { ["k"] = 1 }
d = new D { l = new { 3 } }
e = new E {}
"#,
    );
    assert_eq!(json["typed"], serde_json::json!({"k": 1}));
    assert_eq!(json["d"]["l"], serde_json::json!([2, 3]));
    assert_eq!(json["d"]["p"], serde_json::json!({"a": 1, "b": 3}));
    assert_eq!(json["e"]["p"], serde_json::json!({"a": 1, "b": 9}));
}

#[test]
fn dynamic_generator_kind_follows_taken_branch() {
    let json = eval(
        r#"
a = new { when (true) { 1 } else { p = 2 } }
b = new { when (false) { 1 } else { p = 2 } }
"#,
    );
    assert_eq!(json["a"], serde_json::json!([1]));
    assert_eq!(json["b"], serde_json::json!({"p": 2}));
}

#[test]
fn typed_untyped_new_covers_alias_nullable_and_mapping_values() {
    let json = eval(
        r#"
class F { a = 1; b = 3 }
x: F = new { a = 2 }
v: F? = new { a = 7 }
typealias L = Listing<String>
u: L = new { "s" }
m: Mapping<String, F> = new { ["k"] { a = 0 } }
"#,
    );
    assert_eq!(json["x"], serde_json::json!({"a": 2, "b": 3}));
    assert_eq!(json["v"], serde_json::json!({"a": 7, "b": 3}));
    assert_eq!(json["u"], serde_json::json!(["s"]));
    assert_eq!(json["m"], serde_json::json!({"k": {"a": 0, "b": 3}}));
}

#[test]
fn untyped_new_amendment_uses_declared_type_default() {
    let json = eval(
        r#"
class F { a = 1; b = 3 }
class H { f: F = new { a = 5 } }
class H2 { f: F }
h = new H { f = new { b = 10 } }
h2 = new H2 { f = new { b = 10 } }
h3 = (h) { f = new { a = 7 } }
"#,
    );
    assert_eq!(json["h"]["f"], serde_json::json!({"a": 5, "b": 10}));
    assert_eq!(json["h2"]["f"], serde_json::json!({"a": 1, "b": 10}));
    assert_eq!(json["h3"]["f"], serde_json::json!({"a": 7, "b": 3}));
}

#[test]
fn object_locals_retry_after_body_members_bind() {
    let json = eval("bar = 5\nfoo { bar = 1; local loc = bar; qux = loc }");
    assert_eq!(json["foo"], serde_json::json!({"bar": 1, "qux": 1}));

    // Both locals recover once `b` binds, then the earlier property that
    // depended on them is retried.
    let json = eval("foo { local a = b; local c = a + 1; d = c; b = 2 }");
    assert_eq!(json["foo"], serde_json::json!({"d": 3, "b": 2}));

    // Retry until the chain reaches a fixed point, not just once after `c`
    // binds. The property that depended on the chain is then retried too.
    let json = eval("foo { local a = b; local b = c; d = a; c = 2 }");
    assert_eq!(json["foo"], serde_json::json!({"d": 2, "c": 2}));

    // A deferred property shadows an outer binding while it waits for its
    // local. Its dependent must retry rather than capture that outer value.
    let json = eval("d = 1\nfoo { local a = c; d = a; e = d; c = 2 }");
    assert_eq!(json["foo"], serde_json::json!({"d": 2, "e": 2, "c": 2}));

    // The same deferred dependency can appear in a generated body. Once the
    // local resolves, the generated member must retry rather than surfacing
    // the temporary poison from the earlier property.
    let json = eval("foo { local a = c; d = a; when (true) { e = d }; c = 2 }");
    assert_eq!(json["foo"], serde_json::json!({"d": 2, "e": 2, "c": 2}));

    // Dynamic entries also bind their keys into `this`, so a local can retry
    // after a later key becomes available.
    let json = eval(r#"foo { local a = this["c"]; ["d"] = a; ["c"] = 2 }"#);
    assert_eq!(json["foo"], serde_json::json!({"d": 2, "c": 2}));

    // When the deferred property recovers, it returns to source order rather
    // than remaining appended after members that bound while it was pending.
    let json = eval(
        "foo { local a = c; first = a; second = 2; c = 1 }\nrendered = new PcfRenderer {}.renderDocument(foo)",
    );
    let rendered = json["rendered"].as_str().unwrap();
    let first = rendered.find("first = 1").unwrap();
    let second = rendered.find("second = 2").unwrap();
    let c = rendered.find("c = 1").unwrap();
    assert!(first < second && second < c, "{rendered}");

    // A child entry may overwrite an inherited member without moving that
    // member's existing slot. A recovered property belongs after that slot,
    // even though the overwrite itself appears later in the child body.
    let json = eval(
        "base = new { old = 1 }\nfoo = (base) { local a = c; deferred = a; old = 2; c = 3 }\nrendered = new PcfRenderer {}.renderDocument(foo)",
    );
    let rendered = json["rendered"].as_str().unwrap();
    let old = rendered.find("old = 2").unwrap();
    let deferred = rendered.find("deferred = 3").unwrap();
    let c = rendered.find("c = 3").unwrap();
    assert!(old < deferred && deferred < c, "{rendered}");

    // A local that never resolves remains lazy and fails only when read.
    let err = eval_fails("foo { local a = missing; b = a }");
    assert!(err.contains("missing"), "{err}");
}

#[test]
fn const_locals_reject_non_const_members_that_are_actually_bound() {
    for src in [
        r#"
foo {
  res1 = 15
  const local qux = this.res1
  res2 = qux
}
"#,
        r#"
foo {
  when (true) {
    res1 = 15
  }
  const local qux = res1
  res2 = qux
}
"#,
        r#"
open class Parent { res1 = 15 }
class Child extends Parent {
  const local qux = res1
  res2 = qux
}
foo = new Child {}
"#,
        r#"
open class Parent { res1 = 15 }
class Child extends Parent {
  const local qux = super.res1
  res2 = qux
}
foo = new Child {}
"#,
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains("Cannot reference property `res1` from here because it is not `const`"),
            "{src}: {err}"
        );
    }
}

#[test]
fn const_local_lambdas_stay_lazy_but_reject_non_const_members_when_called() {
    let json = eval(
        r#"
foo {
  res1 = 15
  const local f = () -> res1
}
"#,
    );
    assert_eq!(json["foo"], serde_json::json!({"res1": 15}));

    let err = eval_fails(
        r#"
foo {
  res1 = 15
  const local f = () -> res1
  res2 = f.apply()
}
"#,
    );
    assert!(
        err.contains("Cannot reference property `res1` from here because it is not `const`"),
        "{err}"
    );

    let err = eval_fails(
        r#"
foo {
  const local f = () -> res1
  when (true) {
    res1 = 15
  }
  res2 = f.apply()
}
"#,
    );
    assert!(
        err.contains("Cannot reference property `res1` from here because it is not `const`"),
        "{err}"
    );
}

#[test]
fn class_const_local_lambdas_defer_inherited_non_const_validation() {
    let json = eval(
        r#"
open class Parent { res1 = 15 }
class Child extends Parent {
  const local f = () -> res1
}
result = 1
"#,
    );
    assert_eq!(json, serde_json::json!({"result": 1}));

    let json = eval(
        r#"
open class Parent { res1 = 15 }
class Child extends Parent {
  const local f = () -> res1
}
result = new Child {}
"#,
    );
    assert_eq!(json, serde_json::json!({"result": {"res1": 15}}));

    let err = eval_fails(
        r#"
open class Parent { res1 = 15 }
class Child extends Parent {
  const local f = () -> res1
  result = f.apply()
}
instance = new Child {}
"#,
    );
    assert!(
        err.contains("Cannot reference property `res1` from here because it is not `const`"),
        "{err}"
    );
}

#[test]
fn const_locals_are_lazy_and_validate_on_access_across_receivers_and_bodies() {
    for receiver in ["res1", "this.res1", "super.res1"] {
        for is_lambda in [false, true] {
            let value = if is_lambda {
                format!("() -> {receiver}")
            } else {
                receiver.to_string()
            };
            let use_value = if is_lambda { "qux.apply()" } else { "qux" };
            let object_source = |used: bool| {
                let use_line = if used {
                    format!("result = {use_value}")
                } else {
                    String::new()
                };
                if receiver == "super.res1" {
                    format!(
                        "base = new {{ res1 = 15 }}\nfoo = (base) {{\n  const local qux = {value}\n  {use_line}\n}}"
                    )
                } else {
                    format!("foo {{\n  res1 = 15\n  const local qux = {value}\n  {use_line}\n}}")
                }
            };
            let class_source = |used: bool| {
                let use_line = if used {
                    format!("result = {use_value}")
                } else {
                    String::new()
                };
                format!(
                    "open class Parent {{ res1 = 15 }}\nclass Child extends Parent {{\n  const local qux = {value}\n  {use_line}\n}}\ninstance = new Child {{}}"
                )
            };

            eval(&object_source(false));
            eval(&class_source(false));
            for src in [object_source(true), class_source(true)] {
                let err = eval_fails(&src);
                assert!(
                    err.contains(
                        "Cannot reference property `res1` from here because it is not `const`"
                    ),
                    "{src}: {err}"
                );
            }
        }
    }
}

#[test]
fn const_local_lambda_copies_keep_late_member_validation() {
    for src in [
        r#"
foo {
  const local f = () -> res1
  result = f.apply()
  res1 = 15
}
"#,
        r#"
res1 = 5
foo {
  const local f = () -> res1
  local g = f
  res1 = 15
  result = g.apply()
}
"#,
        r#"
res1 = 5
foo {
  const local f = () -> res1
  local g = f
  when (true) {
    res1 = 15
  }
  result = g.apply()
}
"#,
    ] {
        let err = eval_fails(src);
        assert!(
            err.contains("Cannot reference property `res1` from here because it is not `const`"),
            "{src}: {err}"
        );
    }
}

#[test]
fn const_local_lambda_ignores_untaken_generator_members() {
    let err = eval_fails(
        r#"
foo {
  const local f = () -> res1
  when (false) {
    res1 = 15
  }
  result = f.apply()
}
"#,
    );
    assert!(err.contains("res1"), "{err}");
    assert!(
        !err.contains("Cannot reference property `res1` from here because it is not `const`"),
        "{err}"
    );
}

#[test]
fn const_local_lambda_guard_is_not_exposed_through_outer() {
    let json = eval(
        r#"
foo {
  const local f = () -> new {
    copied = outer
  }
  result = f.apply()
}
"#,
    );
    assert!(
        !json.to_string().contains("pklr:lambda-guard"),
        "internal lambda guard leaked through outer: {json}"
    );
}

#[test]
fn super_in_an_extending_module_reads_the_base_module() {
    let temp = TestTempDir::new("pklr_module_super");
    std::fs::write(
        temp.path().join("base.pkl"),
        "open module base\nhidden secret = 42\nfunction say(msg) = \"Hi \" + msg\nsameProp = \"a\"\npigeon { name = \"Pigeon\" }\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("mid.pkl"),
        "open module mid\nextends \"base.pkl\"\nsameProp = super.sameProp + \"b\"\nhello = super.say(\"there\")\nsecretCopy = super.secret\npigeon = (super.pigeon) { name = \"PIGEON\" }\n",
    )
    .unwrap();
    let child = temp.path().join("child.pkl");
    std::fs::write(
        &child,
        "extends \"mid.pkl\"\nsameProp = super.sameProp + \"c\"\n",
    )
    .unwrap();
    let json = pklr::eval_to_json(&temp.path().join("mid.pkl")).unwrap();
    assert_eq!(json["sameProp"], "ab");
    assert_eq!(json["hello"], "Hi there");
    assert_eq!(json["secretCopy"], 42);
    assert_eq!(json["pigeon"]["name"], "PIGEON");
    // A base module's property reading `super` keeps reading its own base.
    let json = pklr::eval_to_json(&child).unwrap();
    assert_eq!(json["sameProp"], "abc");
}

#[test]
fn nested_object_super_does_not_freeze_inherited_module_properties() {
    let temp = TestTempDir::new("pklr_nested_super");
    std::fs::write(
        temp.path().join("base.pkl"),
        "open module base\np = 1\ntemplate { x = 10 }\nobj = (template) { x = super.x + p }\n",
    )
    .unwrap();
    let child = temp.path().join("child.pkl");
    std::fs::write(&child, "extends \"base.pkl\"\np = 2\n").unwrap();

    let json = pklr::eval_to_json(&child).unwrap();
    assert_eq!(json["obj"]["x"], 12);
}

#[test]
fn inherited_method_keeps_its_defining_module_super() {
    let temp = TestTempDir::new("pklr_method_module_super");
    std::fs::write(temp.path().join("root.pkl"), "open module root\np = 1\n").unwrap();
    std::fs::write(
        temp.path().join("base.pkl"),
        "open module base\nextends \"root.pkl\"\nfunction baseP() = super.p\n",
    )
    .unwrap();
    let child = temp.path().join("child.pkl");
    std::fs::write(
        &child,
        "extends \"base.pkl\"\np = 2\nresult = super.baseP()\n",
    )
    .unwrap();

    assert_eq!(pklr::eval_to_json(&child).unwrap()["result"], 1);
}

#[test]
fn amending_module_super_reads_hidden_properties_but_not_methods() {
    let temp = TestTempDir::new("pklr_amending_module_super");
    std::fs::write(
        temp.path().join("base.pkl"),
        "hidden secret = 42\ncopy = 0\nresult = 0\nfunction say(msg) = \"Hi \" + msg\n",
    )
    .unwrap();
    let child = temp.path().join("child.pkl");
    std::fs::write(&child, "amends \"base.pkl\"\ncopy = super.secret\n").unwrap();
    assert_eq!(pklr::eval_to_json(&child).unwrap()["copy"], 42);

    std::fs::write(
        &child,
        "amends \"base.pkl\"\nresult = super.say(\"there\")\n",
    )
    .unwrap();
    assert!(pklr::eval_to_json(&child).is_err());
}

#[test]
fn only_open_or_abstract_modules_can_be_extended() {
    let temp = TestTempDir::new("pklr_extend_non_open");
    std::fs::write(temp.path().join("closed.pkl"), "a = 1\n").unwrap();
    let child = temp.path().join("child.pkl");
    std::fs::write(&child, "extends \"closed.pkl\"\nb = 2\n").unwrap();
    let err = pklr::eval_to_json(&child).unwrap_err().to_string();
    assert!(
        err.contains("Cannot extend non-open module `closed`"),
        "{err}"
    );
}

#[test]
fn constraint_reads_unbound_names_from_the_checked_value() {
    let source = "class Address { street: String }\nclass Person { address: Address(street.endsWith(\"St.\")) }\n";
    let json = eval(&format!(
        "{source}result = new Person {{ address {{ street = \"Hampton St.\" }} }}"
    ));
    assert_eq!(json["result"]["address"]["street"], "Hampton St.");

    let error = eval_fails(&format!(
        "{source}result = new Person {{ address {{ street = \"Garlic Blvd.\" }} }}"
    ));
    assert!(error.contains("property 'address'"), "{error}");

    // A bare call in a constraint is a call on the checked value. Lambda
    // parameters still resolve lexically instead of becoming `this.it`.
    let json = eval("values: List(every((it) -> it < 0)) = List(-1, -2)");
    assert_eq!(json["values"], serde_json::json!([-1, -2]));
    let error = eval_fails("values: List(every((it) -> it < 0)) = List(1)");
    assert!(error.contains("property 'values'"), "{error}");

    // `read?` also receives a rewritten URI expression; the missing resource
    // is deliberately nullable.
    let json = eval(
        "class Resource { path: String }\nresult: Resource(read?(path) == null) = new Resource { path = \"missing-resource.txt\" }",
    );
    assert_eq!(json["result"]["path"], "missing-resource.txt");
}

#[test]
fn constraint_reads_the_instance_members_around_it() {
    let source = "class Gauge { min: Int; max: Int(this >= min) }\n";
    let json = eval(&format!(
        "{source}result = new Gauge {{ min = 4; max = 6 }}"
    ));
    assert_eq!(json["result"], serde_json::json!({"min": 4, "max": 6}));

    let error = eval_fails(&format!(
        "{source}result = new Gauge {{ min = 4; max = 3 }}"
    ));
    assert!(error.contains("property 'max'"), "{error}");

    // A finished object's saved scope also retains hidden members, which are
    // not present in its rendered map but remain visible to the constraint.
    let json = eval(
        "class HiddenGauge { hidden min: Int; max: Int(this >= min) }\nresult = new HiddenGauge { min = 4; max = 6 }",
    );
    assert_eq!(json["result"], serde_json::json!({"max": 6}));
}

#[test]
fn typed_defaults_render_and_constraints_apply_predicates() {
    let json = eval(
        r#"
class Person
person: Person
mapping: Mapping<String, Person>
choice: *Person | String
items: Listing(isShort) = new Listing { 1 }
hidden isShort = (it) -> it.length < 2
listItem = List(1, 2)[1]
listingItem = (new Listing { 3; 4 })[0]
"#,
    );
    assert_eq!(json["person"], serde_json::json!({}));
    assert_eq!(json["mapping"], serde_json::json!({}));
    assert_eq!(json["choice"], serde_json::json!({}));
    assert_eq!(json["items"], serde_json::json!([1]));
    assert_eq!(json["listItem"], 2);
    assert_eq!(json["listingItem"], 3);
}

#[test]
fn inherited_fixed_members_reject_direct_and_generated_overrides() {
    let error = eval_fails(
        "class Bird { fixed name: String = \"Hawk\" }\nvalue = new Bird { name = \"Eagle\" }",
    );
    assert!(
        error.contains("Cannot assign to fixed property `name`."),
        "{error}"
    );

    let error = eval_fails(
        "class Bird { fixed name: String = \"Hawk\" }\nvalue = new Bird { when (true) { name = \"Eagle\" } }",
    );
    assert!(
        error.contains("Cannot assign to fixed property `name`."),
        "{error}"
    );

    let error = eval_fails(
        "class Bird { fixed name: String = \"Hawk\" }\nvalue = new Bird { ...new Dynamic { name = \"Eagle\" } }",
    );
    assert!(
        error.contains("Cannot assign to fixed property `name`."),
        "{error}"
    );

    let json = eval(
        "class Bird { fixed name: String = \"Hawk\" }\nvalue = new Bird { when (false) { name = \"Eagle\" } }",
    );
    assert_eq!(json["value"]["name"], "Hawk");
}

#[test]
fn typed_class_defaults_preserve_lazy_errors_until_requested() {
    let json = eval(
        r#"
class C { value = throw("boom") }
answer = 1
"#,
    );
    assert_eq!(json["answer"], 1);

    for src in [
        "class C { value = throw(\"boom\") }\nc: C",
        "class C { value = throw(\"boom\") }\ntypealias Alias = C\nc: Alias",
        "class C { value = throw(\"boom\") }\nc: *C|String",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("boom"), "{error}");
    }
    let error = eval_fails("class C { value: Int = \"wrong\" }\nc: C");
    assert!(error.contains("property 'value' expected Int"), "{error}");

    let json = eval("class Int { value = 1 }\nc: Int");
    assert_eq!(json["c"], serde_json::json!({ "value": 1 }));
    let error = eval_fails("class Int { value = throw(\"boom\") }\nc: Int");
    assert!(error.contains("boom"), "{error}");

    let json = eval(
        r#"
class C { value = throw("boom") }
nullable: C?
items: Listing<Int>
mapping: Mapping<String, C>
"#,
    );
    assert!(json.get("nullable").is_none());
    assert_eq!(json["items"], serde_json::json!([]));
    assert_eq!(json["mapping"], serde_json::json!({}));
}

#[test]
fn qualified_imported_typed_defaults_materialize_deferred_class_errors() {
    let dir = TestTempDir::new("pklr_qualified_typed_default");
    std::fs::write(
        dir.path().join("lib.pkl"),
        "class C { value = throw(\"boom\") }\n",
    )
    .unwrap();
    let main = dir.path().join("main.pkl");

    for source in [
        "import \"lib.pkl\" as lib\nresult: lib.C\n",
        "import \"lib.pkl\" as lib\ntypealias Imported = lib.C\nresult: Imported\n",
    ] {
        std::fs::write(&main, source).unwrap();
        let error = pklr::eval_to_json(&main).unwrap_err().to_string();
        assert!(error.contains("boom"), "{error}");
    }

    std::fs::write(&main, "import \"lib.pkl\" as lib\nanswer = 1\n").unwrap();
    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["answer"], 1);
}

#[test]
fn recursive_typed_defaults_fail_cleanly() {
    let error = eval_fails(
        r#"
class Node {
  child: Node
}
x: Node
"#,
    );
    assert!(error.contains("recursive typed default"), "{error}");
}

#[test]
fn typed_instance_member_errors_remain_lazy() {
    let json = eval("class C { bad: Int = \"wrong\"; ok = 1 }\nlocal c = new C {}\nres = c.ok");
    assert_eq!(json["res"], 1);

    for src in [
        "class C { bad: Int = \"wrong\"; ok = 1 }\nlocal c = new C {}\nres = c.bad",
        "class C { bad: Int = \"wrong\"; ok = 1 }\nc = new C {}",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("property 'bad' expected Int"), "{error}");
    }

    let json = eval("class C { hidden bad: Int = \"wrong\"; ok = 1 }\nc: C");
    assert_eq!(json["c"], serde_json::json!({ "ok": 1 }));
}

#[test]
fn imported_amending_module_exports_inherited_classes() {
    let dir = TestTempDir::new("pklr_import_inherited_class");
    std::fs::write(
        dir.path.join("base.pkl"),
        "open module base\nclass Person\n",
    )
    .unwrap();
    std::fs::write(
        dir.path.join("child.pkl"),
        "extends \"base.pkl\"\nchildValue = 1\n",
    )
    .unwrap();
    let main = dir.path.join("main.pkl");
    std::fs::write(&main, "import \"child.pkl\"\nres = new child.Person {}\n").unwrap();

    let json = pklr::eval_to_json(&main).unwrap();
    assert_eq!(json["res"], serde_json::json!({}));
}

#[test]
fn amending_module_rejects_invalid_fixed_and_const_before_member_evaluation() {
    let dir = TestTempDir::new("pklr_amending_modifier_timing");
    std::fs::write(dir.path.join("base.pkl"), "name = \"base\"\n").unwrap();
    let main = dir.path.join("main.pkl");

    for (member, expected) in [
        (
            "fixed unused = throw(\"boom\")",
            "Modifier `fixed` is not applicable to object members.",
        ),
        (
            "const unused = throw(\"boom\")",
            "Modifier `const` can only be applied to object members that are also `local`.",
        ),
    ] {
        std::fs::write(&main, format!("amends \"base.pkl\"\n{member}\n")).unwrap();
        let error = pklr::eval_to_json(&main).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
}
