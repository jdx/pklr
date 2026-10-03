use super::*;

// ============================================================
// Glob imports (import*)
// ============================================================

#[tokio::test]
async fn import_glob() {
    let mut ev = pklr::eval::Evaluator::new_async();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
import* "items/*.pkl" as Items
alpha_val = Items["items/alpha.pkl"].value
beta_val = Items["items/beta.pkl"].value
"#;
    let path = base.join("test_glob.pkl");
    let val = ev.eval_source(src, &path).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["alpha_val"], "alpha");
    assert_eq!(json["beta_val"], "beta");
}

#[tokio::test]
async fn import_glob_value_is_available_to_class_output() {
    let temp = TestTempDir::new("pklr_test_import_glob_class_output");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("builtins")).unwrap();
    std::fs::write(
        dir.join("builtins/prettier.pkl"),
        r#"
prettier { check = "prettier --check" }
prettier_stdin { check = "prettier --stdin-filepath" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "builtins/*.pkl" as Raw

class Factory {
    stdin: Boolean = false
    fixed step = if (stdin)
        Raw["builtins/prettier.pkl"].prettier_stdin
    else
        Raw["builtins/prettier.pkl"].prettier
}

factory = new Factory {}
amended = (factory) { stdin = true }
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["factory"]["step"]["check"], "prettier --check");
    assert_eq!(val["amended"]["step"]["check"], "prettier --stdin-filepath");
}

#[tokio::test]
async fn import_glob_double_star_crosses_directories() {
    let temp = TestTempDir::new("pklr_test_import_glob_double_star");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("config/foo.pkl"), r#"value = "foo""#).unwrap();
    std::fs::write(
        dir.join("hk.pkl"),
        r#"
import* "**.pkl" as Index
value = Index["config/foo.pkl"].value
has_self = Index.containsKey("hk.pkl")
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("hk.pkl")).await.unwrap();
    assert_eq!(val["value"], "foo");
    assert_eq!(val["has_self"], false);
}

#[tokio::test]
async fn import_glob_star_matches_one_directory_segment() {
    let temp = TestTempDir::new("pklr_test_import_glob_star_directory_segment");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("nested/config")).unwrap();
    std::fs::write(dir.join("config/foo.pkl"), r#"value = "foo""#).unwrap();
    std::fs::write(dir.join("nested/config/bar.pkl"), r#"value = "bar""#).unwrap();
    std::fs::write(
        dir.join("hk.pkl"),
        r#"
import* "*/*.pkl" as Index
value = Index["config/foo.pkl"].value
has_nested = Index.containsKey("nested/config/bar.pkl")
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("hk.pkl")).await.unwrap();
    assert_eq!(val["value"], "foo");
    assert_eq!(val["has_nested"], false);
}

#[tokio::test]
async fn import_glob_double_star_slash_matches_root_and_nested_files() {
    let temp = TestTempDir::new("pklr_test_import_glob_double_star_slash");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("foo.pkl"), r#"value = "root""#).unwrap();
    std::fs::write(dir.join("nested/foo.pkl"), r#"value = "nested""#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "**/foo.pkl" as Index
root_value = Index["foo.pkl"].value
nested_value = Index["nested/foo.pkl"].value
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["root_value"], "root");
    assert_eq!(val["nested_value"], "nested");
}

#[cfg(unix)]
#[tokio::test]
async fn import_glob_matches_symlinked_files() {
    let temp = TestTempDir::new("pklr_test_import_glob_symlinked_files");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("real")).unwrap();
    std::fs::write(dir.join("real/foo.pkl"), r#"value = "foo""#).unwrap();
    std::os::unix::fs::symlink(dir.join("real/foo.pkl"), dir.join("linked.pkl")).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "*.pkl" as Index
value = Index["linked.pkl"].value
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["value"], "foo");
}

#[cfg(unix)]
#[tokio::test]
async fn import_glob_skips_broken_symlinks() {
    let temp = TestTempDir::new("pklr_test_import_glob_broken_symlinks");
    let dir = temp.path();
    std::fs::write(dir.join("good.pkl"), r#"value = "good""#).unwrap();
    std::os::unix::fs::symlink(dir.join("missing.pkl"), dir.join("broken.pkl")).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "*.pkl" as Index
value = Index["good.pkl"].value
has_broken = Index.containsKey("broken.pkl")
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["value"], "good");
    assert_eq!(val["has_broken"], false);
}

#[tokio::test]
async fn import_glob_expression_binds_matched_modules() {
    let temp = TestTempDir::new("pklr_test_import_glob_expr");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("generated")).unwrap();
    std::fs::write(dir.join("generated/alpha.pkl"), r#"value = "alpha""#).unwrap();
    std::fs::write(dir.join("generated/beta.pkl"), r#"value = "beta""#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
local generated = import*("generated/*.pkl")
keys = generated.keys.toList()
values = generated.toMap().values.map((m) -> m.value)
alpha = generated["generated/alpha.pkl"].value
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        val["keys"],
        serde_json::json!(["generated/alpha.pkl", "generated/beta.pkl"])
    );
    assert_eq!(val["values"], serde_json::json!(["alpha", "beta"]));
    assert_eq!(val["alpha"], "alpha");
}

#[tokio::test]
async fn import_glob_expression_without_matches_is_empty() {
    let temp = TestTempDir::new("pklr_test_import_glob_expr_empty");
    let dir = temp.path();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
local generated = import*("generated/*.pkl")
count = generated.length
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["count"], 0);
}

#[tokio::test]
async fn import_glob_expression_skips_the_enclosing_module() {
    let temp = TestTempDir::new("pklr_test_import_glob_expr_self");
    let dir = temp.path();
    std::fs::write(dir.join("other.pkl"), r#"value = "other""#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
local siblings = import*("*.pkl")
keys = siblings.keys.toList()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["keys"], serde_json::json!(["other.pkl"]));
}

#[tokio::test]
async fn import_glob_expression_resolves_against_its_own_module() {
    let temp = TestTempDir::new("pklr_test_import_glob_expr_own_module");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("lib/generated")).unwrap();
    std::fs::write(dir.join("lib/generated/one.pkl"), r#"value = "one""#).unwrap();
    std::fs::write(
        dir.join("lib/index.pkl"),
        r#"
matched = import*("generated/*.pkl").keys.toList()
nested {
    matched = import*("generated/*.pkl").keys.toList()
}
fromLambda = ((n) -> import*("generated/*.pkl").length).apply(0)
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "lib/index.pkl" as Index
fromModule = Index.matched
fromObjectBody = Index.nested.matched
fromLambda = Index.fromLambda
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        val["fromModule"],
        serde_json::json!(["generated/one.pkl"]),
        "{val}"
    );
    assert_eq!(
        val["fromObjectBody"],
        serde_json::json!(["generated/one.pkl"]),
        "{val}"
    );
    assert_eq!(val["fromLambda"], 1, "{val}");
}

#[tokio::test]
async fn inherited_import_glob_expression_resolves_against_the_base_module() {
    let temp = TestTempDir::new("pklr_test_import_glob_expr_amends");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("base/generated")).unwrap();
    std::fs::create_dir_all(dir.join("own")).unwrap();
    std::fs::write(dir.join("base/generated/one.pkl"), r#"value = "base""#).unwrap();
    std::fs::write(dir.join("own/one.pkl"), r#"value = "own""#).unwrap();
    std::fs::write(
        dir.join("base/Base.pkl"),
        r#"
fromBase = import*("generated/*.pkl").keys.toList()
fromChild = List()
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("child.pkl"),
        r#"
amends "base/Base.pkl"
fromChild = import*("own/*.pkl").keys.toList()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("child.pkl"))
        .await
        .unwrap();
    assert_eq!(
        val["fromBase"],
        serde_json::json!(["generated/one.pkl"]),
        "{val}"
    );
    assert_eq!(
        val["fromChild"],
        serde_json::json!(["own/one.pkl"]),
        "{val}"
    );
}

#[tokio::test]
async fn import_expressions_are_listing_elements() {
    let temp = TestTempDir::new("pklr_test_import_expr_listing");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("generated")).unwrap();
    std::fs::write(dir.join("generated/alpha.pkl"), r#"value = "alpha""#).unwrap();
    std::fs::write(dir.join("generated/beta.pkl"), r#"value = "beta""#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
single = new Listing { import("generated/alpha.pkl") }
globbed = new Listing { ...import*("generated/*.pkl").toMap().values }
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        val["single"],
        serde_json::json!([{"value": "alpha"}]),
        "{val}"
    );
    assert_eq!(
        val["globbed"],
        serde_json::json!([{"value": "alpha"}, {"value": "beta"}]),
        "{val}"
    );
}

#[tokio::test]
async fn import_expression_evaluates_a_single_module() {
    let temp = TestTempDir::new("pklr_test_import_expr");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("generated")).unwrap();
    std::fs::write(dir.join("generated/alpha.pkl"), r#"value = "alpha""#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
value = import("generated/alpha.pkl").value
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["value"], "alpha");
}

#[tokio::test]
async fn import_expression_field_access_only_evaluates_that_field() {
    let temp = TestTempDir::new("pklr_test_import_expr_narrowing");
    let dir = temp.path();
    std::fs::write(
        dir.join("mod.pkl"),
        r#"
local helper = "h"
working = "\(helper)-ok"
derived = working + "-derived"
nested { inner = "deep" }
broken = missingThing.x
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
plain = import("mod.pkl").working
derived = import("mod.pkl").derived
chained = import("mod.pkl").nested.inner
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["plain"], "h-ok");
    assert_eq!(val["derived"], "h-ok-derived");
    assert_eq!(val["chained"], "deep");
}

#[tokio::test]
async fn import_expression_reports_missing_modules() {
    let temp = TestTempDir::new("pklr_test_import_expr_missing");
    let dir = temp.path();
    std::fs::write(
        dir.join("main.pkl"),
        r#"value = import("generated/alpha.pkl").value"#,
    )
    .unwrap();

    let err = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("alpha.pkl"), "{err}");
}

#[test]
fn import_expression_imports_the_standard_library() {
    let json = eval(r#"regex = import("pkl:base").Regex"#);
    assert_eq!(json["regex"], "Regex");
}

#[test]
fn import_expression_requires_a_string_literal() {
    let err = eval_fails(
        r#"
local uri = "generated/alpha.pkl"
value = import(uri)
"#,
    );
    assert!(
        err.contains("import() requires a string literal URI"),
        "{err}"
    );
}

#[test]
fn import_glob_expression_requires_a_string_literal() {
    let err = eval_fails(
        r#"
local pattern = "generated/*.pkl"
value = import*(pattern)
"#,
    );
    assert!(
        err.contains("import*() requires a string literal URI"),
        "{err}"
    );
}

#[tokio::test]
async fn unused_import_is_not_evaluated() {
    let temp = TestTempDir::new("pklr_test_unused_import");
    let dir = temp.path();
    std::fs::write(
        dir.join("broken.pkl"),
        r#"
value = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "broken.pkl"
result = "ok"
"#,
    )
    .unwrap();

    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["result"], "ok");
}

#[test]
fn missing_unused_import_is_not_evaluated() {
    // Unused imports are intentionally lazy, so missing paths only fail once
    // the imported binding is referenced.
    let json = eval(
        r#"
import "does-not-exist.pkl"
result = "ok"
"#,
    );
    assert_eq!(json["result"], "ok");
}

#[test]
fn shadowed_unused_import_is_not_evaluated() {
    let json = eval(
        r#"
import "does-not-exist.pkl" as Foo
class Foo {}
result = new Foo {}
"#,
    );
    assert!(json["result"].is_object());
}

#[test]
fn nested_shadowed_unused_import_is_not_evaluated() {
    let json = eval(
        r#"
import "does-not-exist.pkl" as Foo
class Outer {
    class Foo {}
    x = new Foo {}
}
result = new Outer {}
"#,
    );
    assert!(json["result"]["x"].is_object());
}

#[test]
fn unused_import_glob_without_alias_is_still_invalid() {
    let err = eval_fails(r#"import* "items/*.pkl""#);
    assert!(err.contains("import* requires an alias"), "{err}");
}

#[tokio::test]
async fn import_used_by_inherited_class_default_is_loaded() {
    let temp = TestTempDir::new("pklr_test_inherited_import_ref");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
class Project {
    name = meta.name
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    let src = r#"
amends "Base.pkl"
import "meta.pkl"
result = new Project {}
"#;

    let mut ev = Evaluator::new_async();
    let val = ev.eval_source(src, &dir.join("child.pkl")).await.unwrap();
    let json = val.to_json();
    assert_eq!(json["result"]["name"], "hk");
}

#[tokio::test]
async fn imported_amends_base_uses_inherited_scope() {
    let temp = TestTempDir::new("pklr_test_imported_amends_base_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
name = meta.name
"#,
    )
    .unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    std::fs::write(
        dir.join("child.pkl"),
        r#"
amends "Base.pkl"
import "meta.pkl"
import "Base.pkl" as Base
baseName = Base.name
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("child.pkl"))
        .await
        .unwrap();
    assert_eq!(val["name"], "hk");
    assert_eq!(val["baseName"], "hk");
}

#[tokio::test]
async fn amended_object_keeps_definition_site_import_scope() {
    let temp = TestTempDir::new("pklr_test_amended_object_import_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
class Spec {
    abstract command: String
}
open class Step {
    check: (String | Spec)?
    prefix: String?
}
steps: Mapping<String, Step> = new Mapping<String, Step> {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
import "Base.pkl" as Config
step = new Config.Step {
    check = new Config.Spec { command = "lint" }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("Config.pkl"), "amends \"Base.pkl\"\n").unwrap();
    std::fs::write(
        dir.join("Shared.pkl"),
        r#"
import "Lib.pkl"
import "Config.pkl"
a = (Lib.step) { prefix = "x" }
b = new Config.Step { check = "true" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
amends "Config.pkl"
import "Shared.pkl"
steps {
    ["a"] = Shared.a
    ["b"] = Shared.b
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "steps": {
                "a": {"check": {"command": "lint"}, "prefix": "x"},
                "b": {"check": "true", "prefix": null}
            }
        })
    );
}

#[tokio::test]
async fn amended_object_keeps_definition_site_scope_when_local_shadows_import() {
    let temp = TestTempDir::new("pklr_test_amended_object_local_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
class Spec {
    abstract command: String
}
open class Step {
    check: (String | Spec)?
    prefix: String?
}
steps: Mapping<String, Step> = new Mapping<String, Step> {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
import "Base.pkl" as Config
step = new Config.Step {
    check = new Config.Spec { command = "lint" }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Shared.pkl"),
        r#"
import "Lib.pkl"
local Config = 1
a = (Lib.step) { prefix = "x" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
amends "Base.pkl"
import "Shared.pkl"
steps { ["a"] = Shared.a }
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "steps": {
                "a": {"check": {"command": "lint"}, "prefix": "x"}
            }
        })
    );
}

#[tokio::test]
async fn amended_object_uses_amendment_site_scope_for_overlay_entries() {
    let temp = TestTempDir::new("pklr_test_amended_object_overlay_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
local collision = "definition"
open class Item {
    inherited = collision
}
value = new Item {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
local collision = "amendment"
result = (Lib.value) {
    overlay = collision
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({
            "inherited": "definition",
            "overlay": "amendment"
        })
    );
}

#[tokio::test]
async fn amended_object_keeps_definition_site_type_alias_scope() {
    let temp = TestTempDir::new("pklr_test_amended_object_type_alias_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
typealias Check = String
open class Item {
    value: Check = "ok"
}
item = new Item {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
typealias Check = Int
result = (Lib.item) {}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(json["result"], serde_json::json!({"value": "ok"}));
}

#[tokio::test]
async fn amended_object_uses_amendment_scope_for_replaced_default() {
    let temp = TestTempDir::new("pklr_test_amended_object_default_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
local collision = "definition"
value = new {
    default = new { selected = collision }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
local collision = "amendment"
result = (Lib.value) {
    default = new { selected = collision }
    ["entry"] {}
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"entry": {"selected": "amendment"}})
    );
}

#[tokio::test]
async fn amended_object_applies_default_body_amendments() {
    let temp = TestTempDir::new("pklr_test_amended_object_default_body");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
value = new {
    default = new {
        inherited = "base"
        changed = "base"
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
result = (Lib.value) {
    default {
        changed = "amendment"
        added = "amendment"
    }
    ["entry"] {}
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({
            "entry": {
                "inherited": "base",
                "changed": "amendment",
                "added": "amendment"
            }
        })
    );
}

#[tokio::test]
async fn repeated_default_body_amendments_apply_once() {
    let temp = TestTempDir::new("pklr_test_repeated_default_body_amendments");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
value = new {
    default = new {
        items = List()
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Middle.pkl"),
        r#"
import "Lib.pkl"
value = (Lib.value) {
    default {
        items { "middle" }
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Middle.pkl"
result = (Middle.value) {
    default {
        items { "main" }
    }
    ["entry"] {}
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"]["entry"]["items"],
        serde_json::json!(["middle", "main"])
    );
}

#[tokio::test]
async fn amendment_type_alias_uses_amendment_scope() {
    let temp = TestTempDir::new("pklr_test_amendment_type_alias_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
open class Foo {
    origin = "definition"
}
value = new {
    inherited: Foo = new Foo {}
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
open class Foo {
    origin = "amendment"
}
result = (Lib.value) {
    typealias Alias = Foo
    selected = new Alias {}
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({
            "inherited": {"origin": "definition"},
            "selected": {"origin": "amendment"}
        })
    );
}

#[tokio::test]
async fn amended_object_reconstructs_classes_in_definition_namespace() {
    let temp = TestTempDir::new("pklr_test_amended_object_class_namespace");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
open class Container {
    class Inner {
        value = "ok"
    }
    item: Inner = new Inner {}
}
item = new Container {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
result = (Lib.item) {}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(json["result"], serde_json::json!({"item": {"value": "ok"}}));
}

#[tokio::test]
async fn amended_object_overlay_can_reference_later_inherited_sibling() {
    let temp = TestTempDir::new("pklr_test_amended_object_later_sibling");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
open class Item {
    selected = "initial"
    later = "inherited"
}
item = new Item {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
result = (Lib.item) { selected = later }
"#,
    )
    .unwrap();
    // A local of the amending module is resolved before the object's
    // inherited members, as in Pkl.
    std::fs::write(
        dir.join("shadowed.pkl"),
        r#"
import "Lib.pkl"
local later = "module"
result = (Lib.item) { selected = later }
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"selected": "inherited", "later": "inherited"})
    );
    let json = pklr::eval_to_json_async(&dir.join("shadowed.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"selected": "module", "later": "inherited"})
    );
}

#[tokio::test]
async fn repeated_amendment_keeps_each_entries_lexical_scope() {
    let temp = TestTempDir::new("pklr_test_repeated_amendment_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
local collision = "definition"
open class Item {
    inherited = collision
}
item = new Item {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Middle.pkl"),
        r#"
import "Lib.pkl"
local collision = "middle"
item = (Lib.item) { overlay = collision }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Middle.pkl"
local collision = "main"
result = (Middle.item) {}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"inherited": "definition", "overlay": "middle"})
    );
}

#[tokio::test]
async fn derived_class_amendment_keeps_parent_and_child_lexical_scopes() {
    let temp = TestTempDir::new("pklr_test_derived_class_amendment_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
local collision = "parent"
open class Parent {
    fromParent = collision
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Middle.pkl"),
        r#"
import "Base.pkl"
local collision = "child"
open class Child extends Base.Parent {
    fromChild = collision
}
item = (new Child {}) {
    fromMiddle = collision
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Middle.pkl"
local collision = "main"
result = (Middle.item) {
    fromMain = collision
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({
            "fromParent": "parent",
            "fromChild": "child",
            "fromMiddle": "child",
            "fromMain": "main"
        })
    );
}

#[tokio::test]
async fn derived_class_amendment_seeds_inherited_property_values() {
    let temp = TestTempDir::new("pklr_test_derived_class_inherited_property_values");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
open class Parent {
    selected = "parent default"
    later = "parent property"
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Middle.pkl"),
        r#"
import "Base.pkl"
local later = "child module"
open class Child extends Base.Parent {}
item = new Child { selected = later }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Middle.pkl"
result = Middle.item
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"later": "parent property", "selected": "child module"})
    );
}

#[tokio::test]
async fn nested_class_inheritance_keeps_each_definition_scope() {
    let temp = TestTempDir::new("pklr_test_nested_class_inheritance_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
local collision = "parent"
open class Parent {
    fromParent = collision
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Middle.pkl"),
        r#"
import "Base.pkl"
local collision = "middle"
open class Middle extends Base.Parent {
    fromMiddle = collision
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Leaf.pkl"),
        r#"
import "Middle.pkl"
local collision = "leaf"
open class Leaf extends Middle.Middle {
    fromLeaf = collision
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Leaf.pkl"
local collision = "main"
result = (new Leaf.Leaf {}) { fromMain = collision }
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({
            "fromParent": "parent",
            "fromMiddle": "middle",
            "fromLeaf": "leaf",
            "fromMain": "main"
        })
    );
}

#[tokio::test]
async fn unassigned_inherited_property_does_not_shadow_amendment_scope() {
    let temp = TestTempDir::new("pklr_test_unassigned_inherited_property_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
local collision = "definition"
open class Item {
    collision: String
}
item = new Item {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
local collision = "amendment"
result = (Lib.item) {
    selected = collision
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(json["result"], serde_json::json!({"selected": "amendment"}));
}

#[tokio::test]
async fn amended_typed_property_keeps_definition_site_class_identity() {
    let temp = TestTempDir::new("pklr_test_amended_typed_property_identity");
    let dir = temp.path();
    std::fs::write(
        dir.join("Config.pkl"),
        r#"
class Test {
    expect: Expect = new Expect {}
}
class Expect {
    stdout: String?
}
open class Step {
    tests: Mapping<String, Test> = new Mapping<String, Test> {}
}
open class Group {}
class Hook {
    steps: Mapping<String, Step | Group> = new Mapping<String, Step> {}
}
hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
amends "Config.pkl"
hooks {
    ["check"] {
        steps {
            ["demo"] {
                tests {
                    ["case"] {
                        expect { stdout = "ok" }
                    }
                }
            }
        }
    }
}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(
        json["hooks"]["check"]["steps"]["demo"]["tests"],
        serde_json::json!({"case": {"expect": {"stdout": "ok"}}})
    );
}

#[tokio::test]
async fn scoped_inherited_base_does_not_pollute_import_cache() {
    let temp = TestTempDir::new("pklr_test_scoped_base_cache");
    let dir = temp.path();
    std::fs::write(
        dir.join("Base.pkl"),
        r#"
name = meta.name
"#,
    )
    .unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    std::fs::write(
        dir.join("child.pkl"),
        r#"
amends "Base.pkl"
import "meta.pkl"
result = name
"#,
    )
    .unwrap();

    let mut ev = Evaluator::new_async();
    let child_val = ev.eval_file_pub(&dir.join("child.pkl")).await.unwrap();
    assert_eq!(child_val.to_json()["result"], "hk");

    let err = ev.eval_file_pub(&dir.join("Base.pkl")).await.unwrap_err();
    assert!(
        err.to_string().contains("undefined variable: meta"),
        "{err}"
    );
}

#[tokio::test]
async fn imported_amends_and_extends_bases_keep_separate_values() {
    let temp = TestTempDir::new("pklr_test_imported_dual_inherited_bases");
    let dir = temp.path();
    std::fs::write(dir.join("AmendsBase.pkl"), r#"amendsName = meta.name"#).unwrap();
    std::fs::write(dir.join("ExtendsBase.pkl"), r#"extendsName = meta.name"#).unwrap();
    std::fs::write(dir.join("meta.pkl"), r#"name = "hk""#).unwrap();
    std::fs::write(
        dir.join("child.pkl"),
        r#"
amends "AmendsBase.pkl"
extends "ExtendsBase.pkl"
import "meta.pkl"
import "AmendsBase.pkl" as AmendsBase
import "ExtendsBase.pkl" as ExtendsBase
amended = AmendsBase.amendsName
extended = ExtendsBase.extendsName
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("child.pkl"))
        .await
        .unwrap();
    assert_eq!(val["extendsName"], "hk");
    assert_eq!(val["amended"], "hk");
    assert_eq!(val["extended"], "hk");
}

#[tokio::test]
async fn import_used_only_by_annotation_does_not_create_builtin_cycle() {
    let temp = TestTempDir::new("pklr_test_annotation_import_cycle");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("builtins")).unwrap();
    std::fs::write(
        dir.join("Builtins.pkl"),
        r#"
import* "builtins/*.pkl" as RawBuiltins

class meta extends Annotation {
    description: String?
}

class PrettierFactory {
    stdin: Boolean = false
    fixed step = if (stdin)
        RawBuiltins["builtins/prettier.pkl"].prettier_stdin
    else
        RawBuiltins["builtins/prettier.pkl"].prettier
}
prettier = new PrettierFactory {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("builtins").join("prettier.pkl"),
        r#"
import "../Builtins.pkl"

@Builtins.meta { description = "formatter" }
prettier = "ok"
prettier_stdin = "stdin"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Builtins.pkl"
result = Builtins.prettier.step
amended = ((Builtins.prettier) { stdin = true }).step
"#,
    )
    .unwrap();

    let path = dir.join("main.pkl");
    let val = pklr::eval_to_json_async(&path).await.unwrap();
    assert_eq!(val["result"], "ok");
    assert_eq!(val["amended"], "stdin");
}

#[tokio::test]
async fn unused_import_glob_field_does_not_evaluate() {
    let temp = TestTempDir::new("pklr_test_unused_import_glob_field");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("builtins")).unwrap();
    std::fs::write(
        dir.join("Builtins.pkl"),
        r#"
import* "builtins/*.pkl" as Builtins
prettier = Builtins["builtins/prettier.pkl"].prettier
staticcheck = Builtins["builtins/staticcheck.pkl"].staticcheck
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("builtins").join("prettier.pkl"),
        r#"prettier = "ok""#,
    )
    .unwrap();
    std::fs::write(
        dir.join("builtins").join("staticcheck.pkl"),
        r#"
static_check = "misspelled"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Builtins.pkl"
result = Builtins.prettier
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "ok");
}

#[tokio::test]
async fn partial_import_expands_this_and_module_dependencies() {
    let temp = TestTempDir::new("pklr_test_partial_import_this_deps");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
x = 41
y = this.x + 1
z = module.x + 2
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
y = Dep.y
z = Dep.z
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["y"], 42);
    assert_eq!(val["z"], 43);
}

#[tokio::test]
async fn partial_import_includes_type_annotation_fields() {
    let temp = TestTempDir::new("pklr_test_partial_import_type_ann");
    let dir = temp.path();
    std::fs::write(
        dir.join("types.pkl"),
        r#"
Step = new Dynamic {
    enabled = true
}
Other = "other"
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "types.pkl" as Types
other = Types.Other
steps: Mapping<String, Types.Step> = new Mapping {}
steps {
    ["a"] {
        name = "alpha"
    }
}
enabled = steps["a"].enabled
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["other"], "other");
    assert_eq!(val["enabled"], true);
}

#[tokio::test]
async fn partial_import_includes_generic_param_fields() {
    let temp = TestTempDir::new("pklr_test_partial_import_generic_param");
    let dir = temp.path();
    std::fs::write(
        dir.join("types.pkl"),
        r#"
Step = new Dynamic {
    enabled = true
}
Other = "other"
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "types.pkl" as Types
other = Types.Other
steps = new Mapping<String, Types.Step> {
    ["a"] {
        name = "alpha"
    }
}
enabled = steps["a"].enabled
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["other"], "other");
    assert_eq!(val["enabled"], true);
}

#[tokio::test]
async fn partial_import_ignores_imports_used_only_by_skipped_properties() {
    let temp = TestTempDir::new("pklr_test_partial_import_skipped_import");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
import "broken.pkl" as Broken
wanted = "ok"
unused = Broken.value
"#,
    )
    .unwrap();
    std::fs::write(dir.join("broken.pkl"), r#"value = missing.field"#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.wanted
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "ok");
}

#[tokio::test]
async fn partial_import_treats_object_method_receiver_as_whole_import() {
    let temp = TestTempDir::new("pklr_test_partial_import_object_method");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
first = "one"
second = "two"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.toMap().toMapping()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"]["first"], "one");
    assert_eq!(val["result"]["second"], "two");
}

#[tokio::test]
async fn partial_import_treats_object_map_values_receiver_as_whole_import() {
    let temp = TestTempDir::new("pklr_test_partial_import_map_values");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
first = 1
second = 2
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.mapValues((k, v) -> v + 1).toMapping()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"]["first"], 2);
    assert_eq!(val["result"]["second"], 3);
}

#[tokio::test]
async fn partial_import_keeps_user_defined_method_names_field_scoped() {
    let temp = TestTempDir::new("pklr_test_partial_import_user_method");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
map = (n) -> n + 1
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.map(41)
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], 42);
}

#[tokio::test]
async fn partial_import_includes_sibling_function_called_by_requested_function() {
    let temp = TestTempDir::new("pklr_test_partial_import_sibling_function");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        r#"
function helper(): String = "ok"
function picked(): String = helper()
broken = missing.field
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "dep.pkl" as Dep
result = Dep.picked()
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "ok");
}

#[test]
fn object_entries_can_be_separated_by_semicolons() {
    let json = eval(
        r#"
x {
  ["FOO"] = "foo"; ["BAR"] = "bar"
}
"#,
    );
    assert_eq!(json["x"]["FOO"], "foo");
    assert_eq!(json["x"]["BAR"], "bar");
}

#[test]
fn object_to_mapping_returns_mapping_like_object() {
    let json = eval(
        r#"
local Builtins = new Mapping {
  ["one"] = "1"
}
x = Builtins.toMap().toMapping()
"#,
    );
    assert_eq!(json["x"]["one"], "1");
}

#[test]
fn top_level_bare_elements_are_invalid() {
    let err = eval_fails("BROKEN SYNTAX");
    assert!(err.contains("Invalid property definition"), "{err}");
}

#[tokio::test]
async fn imported_typed_mapping_does_not_leak_schema_classes() {
    let temp = TestTempDir::new("pklr_test_imported_typed_mapping");
    let dir = temp.path();
    std::fs::write(
        dir.join("Config.pkl"),
        r#"
class Script {
  linux: String?
}

class Step {
  check: (String | Script)?
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("other.pkl"),
        r#"
import "./Config.pkl"
STEPS = new Mapping<String, Config.Step> {
  ["original"] { check = "echo original" }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "./Config.pkl"
import "./other.pkl"
steps = other.STEPS
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["steps"]["original"]["check"], "echo original");
    assert!(val["steps"]["original"].get("Script").is_none(), "{val}");
}

#[tokio::test]
async fn nested_imported_mapping_amendments_preserve_entries() {
    let temp = TestTempDir::new("pklr_test_nested_imported_mapping_amendments");
    let dir = temp.path();
    std::fs::write(
        dir.join("Config.pkl"),
        r#"
class Step {
  check: String?
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Shared.pkl"),
        r#"
import "./Config.pkl"
prettier = new Config.Step { check = "prettier" }
extra = new Config.Step { check = "extra" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Core.pkl"),
        r#"
import "./Config.pkl"
import "./Shared.pkl"
steps = new Mapping<String, Config.Step> {
  ["prettier"] = Shared.prettier
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Default.pkl"),
        r#"
import "./Config.pkl"
import "./Core.pkl"
steps = (Core.steps) {
  ["terraform"] = new Config.Step { check = "terraform" }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("All.pkl"),
        r#"
import "./Config.pkl"
import "./Shared.pkl"
import "./Default.pkl"
steps = (Default.steps) {
  ["extra"] = Shared.extra
}
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("All.pkl"))
        .await
        .unwrap();
    assert_eq!(
        val["steps"],
        serde_json::json!({
            "prettier": {"check": "prettier"},
            "terraform": {"check": "terraform"},
            "extra": {"check": "extra"},
        })
    );
}

#[test]
fn typed_mapping_amendment_preserves_existing_keyed_entries() {
    let json = eval(
        r#"
class Step {
  check: String?
  env: Mapping<String, String> = new Mapping<String, String> {}
}

class Hook {
  steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

local hooks = new Mapping<String, Hook> {
  ["check"] {
    steps {
      ["echo"] { check = "env" }
    }
  }
}

result = (hooks) {
  ["check"] {
    steps {
      ["echo"] {
        env {
          ["STEP_VAR"] = "step_value"
        }
      }
      ["new step"] {
        check = "echo hello"
      }
    }
  }
}
"#,
    );

    assert_eq!(json["result"]["check"]["steps"]["echo"]["check"], "env");
    assert_eq!(
        json["result"]["check"]["steps"]["echo"]["env"]["STEP_VAR"],
        "step_value"
    );
    assert_eq!(
        json["result"]["check"]["steps"]["new step"]["check"],
        "echo hello"
    );
}

#[tokio::test]
async fn module_imported_narrowly_by_many_modules() {
    let temp = TestTempDir::new("pklr_test_shared_narrowed_import");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(
        dir.join("shared.pkl"),
        r#"
class Step {
  name: String
  cmd: String = "run"
}
greeting = "hi"
count = 3
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("hub.pkl"),
        r#"
import* "parts/*.pkl" as Parts
label = "hub"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("parts/one.pkl"),
        r#"
import "../shared.pkl"
import "../hub.pkl"
step = new shared.Step { name = "one" }
hello = shared.greeting
hubSeen = hub
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("parts/two.pkl"),
        r#"
import "../shared.pkl"
step = new shared.Step { name = "two"; cmd = "go" }
n = shared.count
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "hub.pkl"
import "parts/one.pkl"
import "parts/two.pkl"
import "shared.pkl"
oneHub = one.hubSeen.label
oneStep = one.step
oneHello = one.hello
twoStep = two.step
twoN = two.n
direct = shared.greeting
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["oneHub"], "hub");
    assert_eq!(
        val["oneStep"],
        serde_json::json!({"name": "one", "cmd": "run"})
    );
    assert_eq!(val["oneHello"], "hi");
    assert_eq!(
        val["twoStep"],
        serde_json::json!({"name": "two", "cmd": "go"})
    );
    assert_eq!(val["twoN"], 3);
    assert_eq!(val["direct"], "hi");
}

#[tokio::test]
async fn reused_evaluator_rereads_changed_imports() {
    let temp = TestTempDir::new("pklr_test_reused_evaluator_imports");
    let dir = temp.path();
    std::fs::write(dir.join("dep.pkl"), "value = 1\nother = 2\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\"\nwhole = dep\nnarrow = dep.value\n",
    )
    .unwrap();

    let mut ev = pklr::eval::Evaluator::new_async();
    let first = ev
        .eval_file_pub(&dir.join("main.pkl"))
        .await
        .unwrap()
        .to_json();
    assert_eq!(first["narrow"], 1);
    assert_eq!(first["whole"]["other"], 2);

    std::fs::write(dir.join("dep.pkl"), "value = 10\nother = 20\n").unwrap();
    let second = ev
        .eval_file_pub(&dir.join("main.pkl"))
        .await
        .unwrap()
        .to_json();
    assert_eq!(second["narrow"], 10);
    assert_eq!(second["whole"]["other"], 20);
}
