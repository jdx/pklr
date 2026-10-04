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

/// Set in the child process spawned by `import_glob_keys_are_relative_for_relative_entry_path`.
const RELATIVE_GLOB_CHILD_ENV: &str = "PKLR_TEST_RELATIVE_GLOB_CHILD";

#[tokio::test]
async fn import_glob_keys_are_relative_for_relative_entry_path() {
    if std::env::var_os(RELATIVE_GLOB_CHILD_ENV).is_some() {
        // Running in the child process, whose cwd is the temp dir.
        let val = pklr::eval_to_json_async(std::path::Path::new("main.pkl"))
            .await
            .unwrap();
        assert_eq!(val["result"], "used");
        return;
    }

    let temp = TestTempDir::new("pklr_test_import_glob_relative_entry");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("parts/used.pkl"), r#"value = "used""#).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "parts/*.pkl" as Parts
result = Parts["parts/used.pkl"].value
"#,
    )
    .unwrap();

    // The entry path must be bare (`main.pkl`) to hit the empty-parent case, which
    // needs the cwd to be the temp dir. Changing the cwd in-process would race
    // with other tests that use cwd-relative paths, so rerun just this test in a
    // child process instead.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "imports::import_glob_keys_are_relative_for_relative_entry_path",
            "--exact",
            "--nocapture",
        ])
        .env(RELATIVE_GLOB_CHILD_ENV, "1")
        .current_dir(dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
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

#[tokio::test]
async fn narrowed_import_that_read_a_cycle_placeholder_is_not_reused() {
    let temp = TestTempDir::new("pklr_test_cycle_narrowed_reuse");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        "import \"hub.pkl\"\nseen = hub?.title ?? \"placeholder\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("hub.pkl"),
        "import \"dep.pkl\"\ntitle = \"T\"\nduring = dep.seen\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"hub.pkl\"\nimport \"dep.pkl\"\nhubDuring = hub.during\nafter = dep.seen\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    // While hub is being imported, dep sees hub's placeholder. Once hub is
    // done, the same narrowed import of dep must see its real value rather
    // than a result cached during the cycle.
    assert_eq!(val["hubDuring"], "placeholder");
    assert_eq!(val["after"], "T");
}

#[tokio::test]
async fn import_glob_evaluates_only_referenced_modules() {
    let temp = TestTempDir::new("pklr_test_glob_referenced_only");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("parts/used.pkl"), "value = \"used\"\n").unwrap();
    // Evaluating this module fails, so it must not be evaluated unless read.
    std::fs::write(
        dir.join("parts/unused.pkl"),
        "value = throw(\"unused module was evaluated\")\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import* \"parts/*.pkl\" as Parts\nresult = Parts[\"parts/used.pkl\"].value\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"], "used");
}

#[tokio::test]
async fn import_glob_keys_still_see_every_module() {
    let temp = TestTempDir::new("pklr_test_glob_keys_all");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("parts/a.pkl"), "value = \"a\"\n").unwrap();
    std::fs::write(dir.join("parts/b.pkl"), "value = \"b\"\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import* \"parts/*.pkl\" as Parts\na = Parts[\"parts/a.pkl\"].value\ncount = Parts.length\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["a"], "a");
    assert_eq!(val["count"], 2);
}

#[tokio::test]
async fn import_glob_keeps_modules_an_amended_base_reads() {
    let temp = TestTempDir::new("pklr_test_glob_amended_base_reads");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("parts/a.pkl"), "value = \"a\"\n").unwrap();
    std::fs::write(dir.join("parts/b.pkl"), "value = \"b\"\n").unwrap();
    std::fs::write(
        dir.join("base.pkl"),
        "fromBase = Parts[\"parts/b.pkl\"].value\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "amends \"base.pkl\"\nimport* \"parts/*.pkl\" as Parts\nfromChild = Parts[\"parts/a.pkl\"].value\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["fromBase"], "b");
    assert_eq!(val["fromChild"], "a");
}

#[tokio::test]
async fn import_glob_field_read_named_like_a_module_sees_every_module() {
    let temp = TestTempDir::new("pklr_test_glob_field_named_like_module");
    let dir = temp.path();
    std::fs::write(dir.join("length"), "value = \"L\"\n").unwrap();
    std::fs::write(dir.join("other"), "value = \"O\"\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import* \"*\" as Parts\nl = Parts[\"length\"].value\ncount = Parts.length\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["l"], "L");
    assert_eq!(val["count"], 2);
}

#[tokio::test]
async fn import_glob_keeps_modules_read_by_type_alias_constraints() {
    let temp = TestTempDir::new("pklr_test_glob_type_alias_constraint");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("parts/a.pkl"), "value = \"a\"\n").unwrap();
    std::fs::write(dir.join("parts/b.pkl"), "value = \"b\"\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"import* "parts/*.pkl" as Parts
typealias IsB = String(this == Parts["parts/b.pkl"].value)
fromA = Parts["parts/a.pkl"].value
ok = "b" is IsB
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["fromA"], "a");
    assert_eq!(val["ok"], true);
}

#[tokio::test]
async fn indexed_ordinary_import_is_evaluated_whole() {
    let temp = TestTempDir::new("pklr_test_indexed_ordinary_import");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        "expected = \"b\"\ntypealias IsB = String(this == expected)\nresult = \"b\" is IsB\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\nout = Dep[\"result\"]\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["out"], true);
}

#[tokio::test]
async fn import_glob_in_a_base_keeps_modules_its_children_read() {
    let temp = TestTempDir::new("pklr_test_glob_base_children_read");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("parts/a.pkl"), "value = \"a\"\n").unwrap();
    std::fs::write(dir.join("parts/b.pkl"), "value = \"b\"\n").unwrap();
    std::fs::write(
        dir.join("A.pkl"),
        "open module A\nimport* \"parts/*.pkl\" as X\na = X[\"parts/a.pkl\"].value\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("B.pkl"),
        "extends \"A.pkl\"\nb = X[\"parts/b.pkl\"].value\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("C.pkl"),
        "amends \"A.pkl\"\nc = X[\"parts/b.pkl\"].value\n",
    )
    .unwrap();

    let extended = pklr::eval_to_json_async(&dir.join("B.pkl")).await.unwrap();
    assert_eq!(extended["a"], "a");
    assert_eq!(extended["b"], "b");
    let amended = pklr::eval_to_json_async(&dir.join("C.pkl")).await.unwrap();
    assert_eq!(amended["c"], "b");
}

#[tokio::test]
async fn narrowed_import_follows_type_alias_constraints() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_type_alias");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        "expected = \"b\"\ntypealias IsB = String(this == expected)\nresult = \"b\" is IsB\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\ntypealias IsTrue = Boolean(this == Dep.result)\nok = true is IsTrue\nout = Dep.result\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["ok"], true);
    assert_eq!(val["out"], true);
}

#[tokio::test]
async fn narrowed_import_follows_module_reads_in_class_defaults() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_class_default");
    let dir = temp.path();
    // `Bar`'s default reads the module property `min` without `module.`, so
    // building `Bar` from a requested property still needs `min`.
    std::fs::write(
        dir.join("dep.pkl"),
        "min = 1\nclass Bar {\n  a: Int = min\n}\nresult {\n  bar = new Bar {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as D\nout = D.result\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("class.pkl"),
        "import \"dep.pkl\" as D\nout = new D.Bar {}\n",
    )
    .unwrap();

    let dep = pklr::eval_to_json_async(&dir.join("dep.pkl"))
        .await
        .unwrap();
    assert_eq!(
        dep,
        serde_json::json!({"min": 1, "result": {"bar": {"a": 1}}})
    );
    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val, serde_json::json!({"out": {"bar": {"a": 1}}}));
    let class = pklr::eval_to_json_async(&dir.join("class.pkl"))
        .await
        .unwrap();
    assert_eq!(class, serde_json::json!({"out": {"a": 1}}));
}

#[tokio::test]
async fn narrowed_import_ignores_class_properties_named_like_module_properties() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_class_property_shadow");
    let dir = temp.path();
    // `b = a` reads the instance's own `a`, not the unused module property
    // `a`.
    std::fs::write(
        dir.join("dep.pkl"),
        "a = throw(\"unused\")\nclass D { a = 6; b = a }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\nd = new Dep.D {}\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val, serde_json::json!({"d": {"a": 6, "b": 6}}));
}

#[tokio::test]
async fn narrowed_import_follows_module_reads_named_like_inherited_properties() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_inherited_name");
    let dir = temp.path();
    // As in Pkl, `min` in `Child`'s body is the module property: names
    // declared around a class body win over members it inherits.
    std::fs::write(
        dir.join("dep.pkl"),
        "min = 1\nopen class Parent { min = 2 }\nclass Child extends Parent { a = min }\nres = new Child {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as D\nres = D.res\n",
    )
    .unwrap();

    let dep = pklr::eval_to_json_async(&dir.join("dep.pkl"))
        .await
        .unwrap();
    assert_eq!(dep["res"], serde_json::json!({"min": 2, "a": 1}));
    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val, serde_json::json!({"res": {"min": 2, "a": 1}}));
}

#[tokio::test]
async fn narrowed_import_follows_methods_called_by_class_defaults() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_default_method_call");
    let dir = temp.path();
    // A default that calls a method, by name or through `this`, an alias of
    // it or a nested object's `outer`, runs the method's body, which reads
    // the module's `min` (not the inherited one), so the narrowed import
    // must evaluate it.
    std::fs::write(
        dir.join("dep.pkl"),
        "min = 1\nopen class Parent { min = 2 }\nclass Child extends Parent {\n  function getMin() = min\n  a = getMin()\n}\nclass ThisChild extends Parent {\n  function getMin() = min\n  a = this.getMin()\n}\nclass AliasChild extends Parent {\n  function getMin() = min\n  a = let (self = this) self.getMin()\n}\nclass OuterChild extends Parent {\n  function getMin() = min\n  obj { a = outer.getMin() }\n  deep { inner { a = outer.outer.getMin() } }\n}\nchild = new Child {}\nthisChild = new ThisChild {}\naliasChild = new AliasChild {}\nouterChild = new OuterChild {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as D\nchild = D.child\nthisChild = D.thisChild\naliasChild = D.aliasChild\nouterChild = D.outerChild\n",
    )
    .unwrap();

    for file in ["dep.pkl", "main.pkl"] {
        let val = pklr::eval_to_json_async(&dir.join(file)).await.unwrap();
        assert_eq!(val["child"]["a"], 1, "{file}");
        assert_eq!(val["thisChild"]["a"], 1, "{file}");
        assert_eq!(val["aliasChild"]["a"], 1, "{file}");
        assert_eq!(val["outerChild"]["obj"]["a"], 1, "{file}");
        assert_eq!(val["outerChild"]["deep"]["inner"]["a"], 1, "{file}");
    }
}

#[tokio::test]
async fn narrowed_import_skips_methods_class_defaults_do_not_call() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_default_this_read");
    let dir = temp.path();
    // No default calls `getMin`, so the failing module `min` it reads is
    // never needed.
    std::fs::write(
        dir.join("dep.pkl"),
        "min = throw(\"unused\")\nopen class Parent { min = 2 }\nclass Child extends Parent {\n  x = 3\n  a = this.x\n  obj { y = 4; b = this.y; c = outer.x; inner { d = outer.y } }\n  function getMin() = min\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as D\nchild = new D.Child {}\na = child.a\nb = child.obj.b\nc = child.obj.c\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["a"], 3);
    assert_eq!(val["b"], 4);
    assert_eq!(val["c"], 3);
}

#[tokio::test]
async fn narrowed_import_follows_module_reads_but_not_checked_value_members() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_constraint_names");
    let dir = temp.path();
    // `length` in the constraint is the checked string's length, so the
    // unused module property of that name must not be evaluated.
    std::fs::write(
        dir.join("short.pkl"),
        "length = throw(\"unused\")\ntypealias IsShort = String(length == 1)\nresult = \"b\" is IsShort\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("module_read.pkl"),
        "expected = \"b\"\ntypealias IsB = String(this == module.expected)\nresult = \"b\" is IsB\n",
    )
    .unwrap();
    // A number's check binds no `length`, so here it is the module property.
    std::fs::write(
        dir.join("number.pkl"),
        "length = 1\ntypealias IsOne = Int(this == length)\nresult = 1 is IsOne\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"short.pkl\" as Short\nimport \"module_read.pkl\" as ModuleRead\nimport \"number.pkl\" as Number\nshort = Short.result\nmoduleRead = ModuleRead.result\nnumber = Number.result\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["short"], true);
    assert_eq!(val["moduleRead"], true);
    assert_eq!(val["number"], true);
}

#[tokio::test]
async fn narrowed_import_ignores_checked_value_names_in_is_expressions() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_is_constraint");
    let dir = temp.path();
    std::fs::write(
        dir.join("plain.pkl"),
        "length = throw(\"unused\")\nresult = \"b\" is String(length == 1)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("class.pkl"),
        "length = throw(\"unused\")\nclass C { ok = \"b\" is String(length == 1) }\nresult = new C {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("generic.pkl"),
        "length = throw(\"unused\")\nresult = List(1) is Listing<Int>(length == 1)\n",
    )
    .unwrap();
    // A nullable base runs its constraint on `null`, which binds no `length`.
    std::fs::write(
        dir.join("nullable.pkl"),
        "length = 1\nresult = null is Listing<Int>?(length == 1)\n",
    )
    .unwrap();
    // `N` resolves to `Int`, and a number's check doesn't bind `length`, so
    // it stays a module read.
    std::fs::write(
        dir.join("alias.pkl"),
        "length = 1\ntypealias N = Int\nresult = 1 is N(this == length)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"plain.pkl\" as Plain\nimport \"class.pkl\" as Class\nimport \"alias.pkl\" as Alias\nimport \"generic.pkl\" as Generic\nimport \"nullable.pkl\" as Nullable\nplain = Plain.result\nclassed = Class.result.ok\naliased = Alias.result\ngeneric = Generic.result\nnullable = Nullable.result\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["plain"], true);
    assert_eq!(val["classed"], true);
    assert_eq!(val["aliased"], true);
    assert_eq!(val["generic"], true);
    assert_eq!(val["nullable"], true);
}

#[tokio::test]
async fn narrowed_import_resolves_local_aliases_in_constraint_bases() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_alias_constraint_base");
    let dir = temp.path();
    // `S` is `String`, so `length` in the constraint is the string's own.
    std::fs::write(
        dir.join("string.pkl"),
        "length = throw(\"unused\")\ntypealias S = String\nresult = \"b\" is S(length == 1)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("chain.pkl"),
        "length = throw(\"unused\")\nisEmpty = throw(\"unused\")\ntypealias T = S\ntypealias S = NonEmpty\ntypealias NonEmpty = String(!isEmpty)\nresult = \"b\" is T(length == 1)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("listing.pkl"),
        "length = throw(\"unused\")\ntypealias L = Listing<Int>\nresult = List(1) is L(length == 1)\n",
    )
    .unwrap();
    // An alias of a number or nullable type binds no `length`, so it stays a
    // module read.
    std::fs::write(
        dir.join("number.pkl"),
        "length = 1\ntypealias M = N\ntypealias N = Int\nresult = 1 is M(this == length)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("nullable.pkl"),
        "length = 1\ntypealias L = Listing<Int>?\nresult = null is L(length == 1)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"string.pkl\" as Str\nimport \"chain.pkl\" as Chain\nimport \"listing.pkl\" as Lst\nimport \"number.pkl\" as Num\nimport \"nullable.pkl\" as Nullable\nstring = Str.result\nchain = Chain.result\nlisting = Lst.result\nnumber = Num.result\nnullable = Nullable.result\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["string"], true);
    assert_eq!(val["chain"], true);
    assert_eq!(val["listing"], true);
    assert_eq!(val["number"], true);
    assert_eq!(val["nullable"], true);
}

#[tokio::test]
async fn narrowed_import_respects_aliases_redeclared_in_nested_bodies() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_nested_alias");
    let dir = temp.path();
    // The nested `S` is `Int`, whose check binds no `length`, so the module's
    // `length` is still needed even though the module-level `S` is a String.
    std::fs::write(
        dir.join("dep.pkl"),
        "length = 1\ntypealias S = String\nresult {\n  typealias S = Int\n  ok = 1 is S(this == length)\n}\n",
    )
    .unwrap();
    // Redeclared as a String alias, the nested `S` binds the string's own
    // `length`, so the module's unused `length` must not be evaluated.
    std::fs::write(
        dir.join("dep_string.pkl"),
        "length = throw(\"unused\")\ntypealias S = String\nresult {\n  typealias S = String\n  ok = \"b\" is S(length == 1)\n}\n",
    )
    .unwrap();
    // A module alias `T` built on `S` is checked inside a body that
    // redeclares `S = Int`, so its constraint reads the module's `length`.
    std::fs::write(
        dir.join("dep_followed.pkl"),
        "length = 1\ntypealias S = String\ntypealias T = S(length == 1)\nresult {\n  typealias S = Int\n  ok = 1 is T\n}\n",
    )
    .unwrap();
    // A local evaluated before the nested alias still sees the module's
    // `S = Int`, whose check binds no `length`.
    std::fs::write(
        dir.join("dep_order.pkl"),
        "length = 1\ntypealias S = Int\nresult {\n  local checked = 1 is S(this == length)\n  typealias S = String\n  ok = checked\n}\n",
    )
    .unwrap();
    // A module alias `T` built on `S` is checked inside a body that
    // redeclares `S` identically, so `S` is still a String and binds the
    // string's own `length`; the module's unused `length` must not be read.
    std::fs::write(
        dir.join("dep_followed_same.pkl"),
        "length = throw(\"unused\")\ntypealias S = String\ntypealias T = S(length == 1)\nresult {\n  typealias S = String\n  ok = \"b\" is T\n}\n",
    )
    .unwrap();
    // An identical nested `S = String` still means `Int` when the body also
    // declares `String = Int`, so checks through `S` (followed via `T`, or
    // directly) read the module's `length`.
    std::fs::write(
        dir.join("dep_followed_shadowed.pkl"),
        "length = 1\ntypealias S = String\ntypealias T = S(length == 1)\nresult {\n  typealias String = Int\n  typealias S = String\n  ok = 1 is T\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dep_shadowed.pkl"),
        "length = 1\ntypealias S = String\nresult {\n  typealias String = Int\n  typealias S = String\n  ok = 1 is S(this == length)\n}\n",
    )
    .unwrap();
    // A nested alias that no module alias refers to leaves `S` resolved, so
    // the string's own `length` is used, directly or through `T`, and the
    // module's unused `length` is not evaluated.
    std::fs::write(
        dir.join("dep_unrelated.pkl"),
        "length = throw(\"unused\")\ntypealias S = String\nresult {\n  typealias U = Int\n  ok = \"b\" is S(length == 1)\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dep_followed_unrelated.pkl"),
        "length = throw(\"unused\")\ntypealias S = String\ntypealias T = S(length == 1)\nresult {\n  typealias U = Int\n  ok = \"b\" is T\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\nimport \"dep_string.pkl\" as DepString\nimport \"dep_order.pkl\" as DepOrder\nimport \"dep_followed.pkl\" as DepFollowed\nimport \"dep_followed_same.pkl\" as DepFollowedSame\nimport \"dep_followed_shadowed.pkl\" as DepFollowedShadowed\nimport \"dep_shadowed.pkl\" as DepShadowed\nimport \"dep_unrelated.pkl\" as DepUnrelated\nimport \"dep_followed_unrelated.pkl\" as DepFollowedUnrelated\nout = Dep.result.ok\noutString = DepString.result.ok\noutOrder = DepOrder.result.ok\noutFollowed = DepFollowed.result.ok\noutFollowedSame = DepFollowedSame.result.ok\noutFollowedShadowed = DepFollowedShadowed.result.ok\noutShadowed = DepShadowed.result.ok\noutUnrelated = DepUnrelated.result.ok\noutFollowedUnrelated = DepFollowedUnrelated.result.ok\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["out"], true);
    assert_eq!(val["outString"], true);
    assert_eq!(val["outOrder"], true);
    assert_eq!(val["outFollowed"], true);
    assert_eq!(val["outFollowedSame"], true);
    assert_eq!(val["outFollowedShadowed"], true);
    assert_eq!(val["outShadowed"], true);
    assert_eq!(val["outUnrelated"], true);
    assert_eq!(val["outFollowedUnrelated"], true);
}

#[tokio::test]
async fn narrowed_import_respects_builtins_redeclared_in_nested_bodies() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_nested_builtin");
    let dir = temp.path();
    // The module alias `T` is checked inside a body that redeclares `String`
    // as `Int`, whose check binds no `length`, so the module's `length` is
    // still needed.
    std::fs::write(
        dir.join("dep.pkl"),
        "length = 1\ntypealias T = String(length == 1)\nresult {\n  typealias String = Int\n  ok = 1 is T\n}\n",
    )
    .unwrap();
    // The same holds for a check on the redeclared name directly.
    std::fs::write(
        dir.join("dep_direct.pkl"),
        "length = 1\nresult {\n  typealias String = Int\n  ok = 1 is String(length == 1)\n}\n",
    )
    .unwrap();
    // Collections redeclared deeper inside the body, including as generics.
    std::fs::write(
        dir.join("dep_listing.pkl"),
        "length = 1\nresult {\n  inner {\n    typealias Listing = Int\n    ok = 1 is Listing<Int>(this == length)\n  }\n}\n",
    )
    .unwrap();
    // A class's member types are checked where the object is built, so a
    // class instantiated in that body checks `Int` and reads the module's
    // `length`, directly or through `T`.
    std::fs::write(
        dir.join("dep_class.pkl"),
        "length = 1\ntypealias T = String(length == 1)\nclass C {\n  a: String(length == 1) = 1\n  b: T = 1\n}\nresult {\n  typealias String = Int\n  ok = new C {}\n}\n",
    )
    .unwrap();
    // A redeclaration in a sibling body doesn't apply to `ok`, whose check
    // through `T` binds the string's own `length`, so the module's unused
    // `length` must not be evaluated.
    std::fs::write(
        dir.join("dep_sibling.pkl"),
        "length = throw(\"unused\")\ntypealias T = String(length == 1)\nresult {\n  inner {\n    typealias String = Int\n    value = 1\n  }\n  ok = \"b\" is T\n}\n",
    )
    .unwrap();
    // A local bound around the redeclaring body still shadows the module's
    // unused `x`.
    std::fs::write(
        dir.join("dep_local.pkl"),
        "x = throw(\"unused\")\nresult {\n  local x = 1\n  inner {\n    typealias String = Int\n    ok = x\n  }\n}\n",
    )
    .unwrap();
    // An identical redeclaration of the module's `String` reads the same, so
    // `T` still checks a List, which binds its own `length`.
    std::fs::write(
        dir.join("dep_identical.pkl"),
        "length = throw(\"unused\")\ntypealias String = List\ntypealias T = String(length == 1)\nresult {\n  typealias String = List\n  ok = List(1) is T\n}\n",
    )
    .unwrap();
    // An identical `String = List` still changes meaning when the body also
    // redeclares `List`, so checks on `String` (through `T`, or directly)
    // read the module's `length`.
    std::fs::write(
        dir.join("dep_identical_shadowed.pkl"),
        "length = 1\ntypealias String = List\ntypealias T = String(length == 1)\nresult {\n  typealias List = Int\n  typealias String = List\n  ok = 1 is T\n  okDirect = 1 is String(this == length)\n}\n",
    )
    .unwrap();
    // Redeclaring `List` in a sibling body doesn't change what `String`
    // means for `ok`, which checks a List with its own `length`.
    std::fs::write(
        dir.join("dep_alias_sibling.pkl"),
        "length = throw(\"unused\")\ntypealias String = List\ntypealias T = String(length == 1)\nresult {\n  inner {\n    typealias List = Int\n    value = 1\n  }\n  ok = List(1) is T\n}\n",
    )
    .unwrap();
    // Types and properties have separate namespaces, so a body that follows
    // the type `Foo` itself still reads the module property `Foo`.
    std::fs::write(
        dir.join("dep_same_name.pkl"),
        "typealias Foo = Int\nFoo = 5\nresult {\n  typealias String = Int\n  ok = Foo\n}\n",
    )
    .unwrap();
    // Redeclared as a collection, `String` still binds the value's own
    // `length`, directly or through `T`, so the module's unused `length` must
    // not be evaluated.
    std::fs::write(
        dir.join("dep_collection.pkl"),
        "length = throw(\"unused\")\ntypealias T = String(length == 1)\nresult {\n  typealias String = Listing<Int>\n  ok = List(1) is String(length == 1)\n  okFollowed = List(1) is T\n}\n",
    )
    .unwrap();
    // A deeper body that redeclares `List` changes what the outer
    // `String = List` means there, so its check reads the module's `length`.
    std::fs::write(
        dir.join("dep_deeper.pkl"),
        "length = 1\nresult {\n  typealias String = List\n  inner {\n    typealias List = Int\n    ok = 1 is String(length == 1)\n  }\n}\n",
    )
    .unwrap();
    // A base module's alias can redefine a built-in this module checks
    // against, directly or through a nested redeclaration.
    std::fs::write(dir.join("base.pkl"), "typealias List = Int\n").unwrap();
    std::fs::write(
        dir.join("dep_inherited.pkl"),
        "extends \"base.pkl\"\nlength = 1\nresult {\n  ok = 1 is List(length == 1)\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dep_inherited_nested.pkl"),
        "amends \"base.pkl\"\nlength = 1\nresult {\n  typealias String = List\n  ok = 1 is String(length == 1)\n}\n",
    )
    .unwrap();
    // Only the built-ins a base actually redefines count: `String` checks
    // under an empty base, or one redefining only `List`, still bind the
    // string's own `length`, so the module's unused `length` isn't read.
    std::fs::write(dir.join("base_empty.pkl"), "").unwrap();
    std::fs::write(
        dir.join("dep_inherited_empty.pkl"),
        "amends \"base_empty.pkl\"\nlength = throw(\"unused\")\nresult {\n  ok = \"b\" is String(length == 1)\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dep_inherited_other.pkl"),
        "extends \"base.pkl\"\nlength = throw(\"unused\")\nresult {\n  ok = \"b\" is String(length == 1)\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\nimport \"dep_direct.pkl\" as DepDirect\nimport \"dep_inherited_empty.pkl\" as DepInheritedEmpty\nimport \"dep_inherited_other.pkl\" as DepInheritedOther\nimport \"dep_deeper.pkl\" as DepDeeper\nimport \"dep_inherited.pkl\" as DepInherited\nimport \"dep_inherited_nested.pkl\" as DepInheritedNested\nimport \"dep_collection.pkl\" as DepCollection\nimport \"dep_same_name.pkl\" as DepSameName\nimport \"dep_alias_sibling.pkl\" as DepAliasSibling\nimport \"dep_identical_shadowed.pkl\" as DepIdenticalShadowed\nimport \"dep_local.pkl\" as DepLocal\nimport \"dep_identical.pkl\" as DepIdentical\nimport \"dep_listing.pkl\" as DepListing\nimport \"dep_sibling.pkl\" as DepSibling\nimport \"dep_class.pkl\" as DepClass\nout = Dep.result.ok\noutDirect = DepDirect.result.ok\noutListing = DepListing.result.inner.ok\noutSibling = DepSibling.result.ok\noutClass = DepClass.result.ok\noutLocal = DepLocal.result.inner.ok\noutIdentical = DepIdentical.result.ok\noutIdenticalShadowed = DepIdenticalShadowed.result.ok\noutIdenticalShadowedDirect = DepIdenticalShadowed.result.okDirect\noutAliasSibling = DepAliasSibling.result.ok\noutSameName = DepSameName.result.ok\noutCollection = DepCollection.result.ok\noutCollectionFollowed = DepCollection.result.okFollowed\noutDeeper = DepDeeper.result.inner.ok\noutInherited = DepInherited.result.ok\noutInheritedNested = DepInheritedNested.result.ok\noutInheritedEmpty = DepInheritedEmpty.result.ok\noutInheritedOther = DepInheritedOther.result.ok\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["out"], true);
    assert_eq!(val["outDirect"], true);
    assert_eq!(val["outListing"], true);
    assert_eq!(val["outSibling"], true);
    assert_eq!(val["outClass"], serde_json::json!({ "a": 1, "b": 1 }));
    assert_eq!(val["outLocal"], 1);
    assert_eq!(val["outIdentical"], true);
    assert_eq!(val["outIdenticalShadowed"], true);
    assert_eq!(val["outIdenticalShadowedDirect"], true);
    assert_eq!(val["outAliasSibling"], true);
    assert_eq!(val["outSameName"], 5);
    assert_eq!(val["outCollection"], true);
    assert_eq!(val["outCollectionFollowed"], true);
    assert_eq!(val["outDeeper"], true);
    assert_eq!(val["outInherited"], true);
    assert_eq!(val["outInheritedNested"], true);
    assert_eq!(val["outInheritedEmpty"], true);
    assert_eq!(val["outInheritedOther"], true);
}

#[tokio::test]
async fn narrowed_import_resolves_aliases_in_followed_class_bodies() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_class_alias");
    let dir = temp.path();
    // Following `C` analyses its property's constraint with the module's
    // aliases, so `S` is a String and `length` is the string's own.
    std::fs::write(
        dir.join("dep.pkl"),
        "length = throw(\"unused\")\ntypealias S = String\nclass C {\n  name: S(length == 1) = \"b\"\n}\nresult = new C {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\nout = Dep.result.name\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["out"], "b");
}

#[tokio::test]
async fn module_in_imported_class_body_means_the_class_module() {
    let temp = TestTempDir::new("pklr_test_module_in_imported_class");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep2.pkl"),
        "expected = \"b\"\nclass C { v = module.expected }\nresult = new C {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep2.pkl\"\nexpected = \"main\"\nr = dep2.result.v\nfresh = new dep2.C {}\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("dep2.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"]["v"], "b");
    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["r"], "b");
    assert_eq!(val["fresh"]["v"], "b");
}

#[tokio::test]
async fn imported_class_reading_missing_module_property_fails_when_instantiated() {
    let temp = TestTempDir::new("pklr_test_imported_poisoned_class");
    let dir = temp.path();
    std::fs::write(dir.join("dep.pkl"), "class C { v = module.missing }\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\"\nresult = new dep.C {}\n",
    )
    .unwrap();

    // The module defining the class still evaluates; the class is unused.
    let val = pklr::eval_to_json_async(&dir.join("dep.pkl"))
        .await
        .unwrap();
    assert_eq!(val, serde_json::json!({}));
    // As in Pkl, a class's defaults are evaluated for an instance, so only
    // building one reports the error; reading the class does not.
    let err = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing"), "{err}");
    for (name, read) in [
        ("read", "dep.C"),
        ("nullsafe", "dep?.C"),
        ("index", "dep[\"C\"]"),
    ] {
        let file = dir.join(format!("{name}.pkl"));
        std::fs::write(
            &file,
            format!("import \"dep.pkl\"\nlocal c = {read}\nresult = new c {{}}\n"),
        )
        .unwrap();
        let err = pklr::eval_to_json_async(&file)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing"), "{read}: {err}");
    }
    // Amending the module object keeps its evaluated members.
    std::fs::write(
        dir.join("depx.pkl"),
        "x = 1\nclass C { v = module.missing }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("amend.pkl"),
        "import \"depx.pkl\" as dep\nresult = (dep) { y = 2 }\nr2 = dep { y = 3 }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json_async(&dir.join("amend.pkl"))
        .await
        .unwrap();
    assert_eq!(val["result"]["x"], 1);
    assert_eq!(val["result"]["y"], 2);
    assert_eq!(val["r2"]["x"], 1);
    assert_eq!(val["r2"]["y"], 3);
}

#[tokio::test]
async fn narrowed_import_follows_module_reads_in_class_bodies() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_class_module_reads");
    let dir = temp.path();
    // The requested field reaches `C` only through a local; `C`'s body reads
    // `limit` through `module`, so the narrowed import must evaluate `limit`.
    std::fs::write(
        dir.join("direct.pkl"),
        "class C { v = module.limit }\nlocal ok = new C {}.v\nlimit = 2\nout = ok\n",
    )
    .unwrap();
    // At module level `this` is the module, so `this.C` names the class.
    std::fs::write(
        dir.join("this_read.pkl"),
        "class C { v = module.limit }\nlocal ok = new this.C {}.v\nlimit = 3\nout = ok\n",
    )
    .unwrap();
    // A dynamic `module[key]` read can reach any property.
    std::fs::write(
        dir.join("dynamic.pkl"),
        "class C { key = \"limit\"; v = module[key] }\nlocal ok = new C {}.v\nlimit = 4\nout = ok\n",
    )
    .unwrap();
    // A local function reads the module property when it is called.
    std::fs::write(
        dir.join("lambda.pkl"),
        "local f = () -> limit\nlimit = 5\nout = f()\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"direct.pkl\"\nimport \"this_read.pkl\"\nimport \"dynamic.pkl\"\nimport \"lambda.pkl\"\ndirect = direct.out\nthisRead = this_read.out\ndynamic = dynamic.out\nlambda = lambda.out\n",
    )
    .unwrap();

    for (file, expected) in [
        ("direct.pkl", 2),
        ("this_read.pkl", 3),
        ("dynamic.pkl", 4),
        ("lambda.pkl", 5),
    ] {
        let val = pklr::eval_to_json_async(&dir.join(file)).await.unwrap();
        assert_eq!(val["out"], expected, "{file}");
    }
    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["direct"], 2);
    assert_eq!(val["thisRead"], 3);
    assert_eq!(val["dynamic"], 4);
    assert_eq!(val["lambda"], 5);
}

#[tokio::test]
async fn narrowed_import_follows_locals_with_module_aliases() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_local_aliases");
    let dir = temp.path();
    // `ok` is evaluated at module level, where `S` is a String whose check
    // binds the string's own `length`, even though the body reading it
    // redeclares `S`; the module's unused `length` must not be evaluated.
    std::fs::write(
        dir.join("dep.pkl"),
        "length = throw(\"unused\")\ntypealias S = String\nlocal ok = \"b\" is S(length == 1)\nresult {\n  typealias S = Int\n  v = ok\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\"\nout = dep.result.v\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["out"], true);
}

#[tokio::test]
async fn narrowed_import_keeps_type_and_property_names_apart() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_type_property_names");
    let dir = temp.path();
    // Types and properties have separate namespaces: checking against the
    // alias `Foo` doesn't read the unused property `Foo`.
    std::fs::write(
        dir.join("dep.pkl"),
        "typealias Foo = Int\nFoo = throw(\"unused\")\nresult {\n  ok = 1 is Foo\n}\n",
    )
    .unwrap();
    // Reading `Foo` as a value reads the property.
    std::fs::write(
        dir.join("dep_value.pkl"),
        "typealias Foo = Int\nFoo = 5\nresult {\n  ok = Foo\n}\n",
    )
    .unwrap();
    // The alias is still followed, so what its constraint reads is kept, and
    // annotations and `new` name the type too.
    std::fs::write(
        dir.join("dep_followed.pkl"),
        "min = 1\ntypealias Foo = Int(this >= min)\nFoo = throw(\"unused\")\nclass Bar {\n  a: Int = 1\n}\nBar = throw(\"unused\")\nresult {\n  ok = 1 is Foo\n  typed: Foo = 2\n  bar = new Bar {}\n}\n",
    )
    .unwrap();
    // A body that follows definitions itself (it redeclares a type) drops the
    // type reference too.
    std::fs::write(
        dir.join("dep_narrowed.pkl"),
        "min = 1\ntypealias Foo = Int(this >= min)\nFoo = throw(\"unused\")\nresult {\n  typealias String = Int\n  ok = 1 is Foo\n}\n",
    )
    .unwrap();
    // Bindings shadow only their own namespace: a type declared in a body
    // doesn't hide the module property it reads, and a local doesn't hide
    // the module alias it checks against.
    std::fs::write(
        dir.join("dep_type_shadow.pkl"),
        "Foo = 5\nresult {\n  typealias Foo = Int\n  ok = Foo\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dep_local_shadow.pkl"),
        "min = 1\ntypealias Foo = Int(this >= min)\nresult {\n  local Foo = 1\n  ok = 1 is Foo\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\nimport \"dep_value.pkl\" as DepValue\nimport \"dep_followed.pkl\" as DepFollowed\nimport \"dep_narrowed.pkl\" as DepNarrowed\nimport \"dep_type_shadow.pkl\" as DepTypeShadow\nimport \"dep_local_shadow.pkl\" as DepLocalShadow\nout = Dep.result.ok\noutValue = DepValue.result.ok\noutFollowed = DepFollowed.result\noutNarrowed = DepNarrowed.result.ok\noutTypeShadow = DepTypeShadow.result.ok\noutLocalShadow = DepLocalShadow.result.ok\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["out"], true);
    assert_eq!(val["outValue"], 5);
    assert_eq!(
        val["outFollowed"],
        serde_json::json!({ "ok": true, "typed": 2, "bar": { "a": 1 } })
    );
    assert_eq!(val["outNarrowed"], true);
    assert_eq!(val["outTypeShadow"], 5);
    assert_eq!(val["outLocalShadow"], true);
}

#[tokio::test]
async fn narrowed_import_reads_qualified_type_roots_and_nested_classes() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_qualified_roots");
    let dir = temp.path();
    std::fs::write(dir.join("types.pkl"), "class Item {\n  a: Int = 1\n}\n").unwrap();
    // The root of `Dep.Item` is a property holding a module, so building one
    // reads the property.
    std::fs::write(
        dir.join("dep.pkl"),
        "Dep = import(\"types.pkl\")\nresult = new Dep.Item {}\n",
    )
    .unwrap();
    // A class declared in a body is bound as a value too, so reading it there
    // doesn't read the module property of its name.
    std::fs::write(
        dir.join("dep_class.pkl"),
        "C = throw(\"unused\")\nresult {\n  class C {\n    a: Int = 1\n  }\n  ok = C\n  inst = new C {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\nimport \"dep_class.pkl\" as DepClass\nout = Dep.result\noutClass = DepClass.result.inst\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["out"], serde_json::json!({ "a": 1 }));
    assert_eq!(val["outClass"], serde_json::json!({ "a": 1 }));
}

#[tokio::test]
async fn narrowed_import_reads_values_named_as_types() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_values_named_as_types");
    let dir = temp.path();
    // A type name that isn't one of the module's types can name a property
    // or local holding a class, which building or checking one reads.
    std::fs::write(
        dir.join("dep_property.pkl"),
        "Foo = Item\nclass Item {\n  a: Int = 1\n}\nresult: Foo = new Foo {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dep_local.pkl"),
        "class C {\n  v = module.expected\n}\nlocal x = module.C\nexpected = \"b\"\nresult = new x {}\n",
    )
    .unwrap();
    // A local in the body binds the name, so the module property it shadows
    // isn't read.
    std::fs::write(
        dir.join("dep_shadowed.pkl"),
        "class Item {\n  a: Int = 1\n}\nFoo = throw(\"unused\")\nresult {\n  local Foo = Item\n  x = new Foo {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep_property.pkl\" as DepProperty\nimport \"dep_local.pkl\" as DepLocal\nimport \"dep_shadowed.pkl\" as DepShadowed\noutProperty = DepProperty.result\noutLocal = DepLocal.result\noutShadowed = DepShadowed.result.x\n",
    )
    .unwrap();

    let val = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(val["outProperty"], serde_json::json!({ "a": 1 }));
    assert_eq!(val["outLocal"], serde_json::json!({ "v": "b" }));
    assert_eq!(val["outShadowed"], serde_json::json!({ "a": 1 }));
}
