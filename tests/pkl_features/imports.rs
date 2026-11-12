use super::*;

fn encoded_file_uri(path: &std::path::Path) -> String {
    let path = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    if path.as_bytes().get(1) == Some(&b':') {
        format!("file:///{path}")
    } else {
        format!("file://{path}")
    }
}

// ============================================================
// Glob imports (import*)
// ============================================================

#[test]
fn encoded_file_uris_resolve_amends_imports_and_extends() {
    let temp = TestTempDir::new("pklr_test_encoded_file_uri");
    let dir = temp.path().join("uri target");
    std::fs::create_dir_all(&dir).unwrap();

    let amend_base = dir.join("Amend Base.pkl");
    std::fs::write(&amend_base, "answer = 7\n").unwrap();
    let amend_uri = encoded_file_uri(&amend_base);
    let amended = dir.join("Amended.pkl");
    std::fs::write(
        &amended,
        format!("amends \"{amend_uri}\"\nresult = answer\n"),
    )
    .unwrap();
    assert_eq!(pklr::eval_to_json(&amended).unwrap()["result"], 7);

    let import_base = dir.join("Import Base.pkl");
    std::fs::write(&import_base, "answer = 8\n").unwrap();
    let import_uri = encoded_file_uri(&import_base);
    let importer = dir.join("Importer.pkl");
    std::fs::write(
        &importer,
        format!("import \"{import_uri}\" as Base\nresult = Base.answer\n"),
    )
    .unwrap();
    assert_eq!(pklr::eval_to_json(&importer).unwrap()["result"], 8);

    let extend_base = dir.join("Extend Base.pkl");
    std::fs::write(&extend_base, "open module ExtendBase\nanswer = 9\n").unwrap();
    let extend_uri = encoded_file_uri(&extend_base);
    let extended = dir.join("Extended.pkl");
    std::fs::write(
        &extended,
        format!("extends \"{extend_uri}\"\nresult = answer\n"),
    )
    .unwrap();
    assert_eq!(pklr::eval_to_json(&extended).unwrap()["result"], 9);
}

#[test]
fn import_glob() {
    let mut ev = pklr::eval::Evaluator::new();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ev.set_base_path(&base);
    let src = r#"
import* "items/*.pkl" as Items
alpha_val = Items["items/alpha.pkl"].value
beta_val = Items["items/beta.pkl"].value
"#;
    let path = base.join("test_glob.pkl");
    let val = ev.eval_source(src, &path).unwrap();
    let json = val.to_json();
    assert_eq!(json["alpha_val"], "alpha");
    assert_eq!(json["beta_val"], "beta");
}

#[test]
fn import_glob_value_is_available_to_class_output() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["factory"]["step"]["check"], "prettier --check");
    assert_eq!(val["amended"]["step"]["check"], "prettier --stdin-filepath");
}

/// Set in the child process spawned by `import_glob_keys_are_relative_for_relative_entry_path`.
const RELATIVE_GLOB_CHILD_ENV: &str = "PKLR_TEST_RELATIVE_GLOB_CHILD";

#[test]
fn import_glob_keys_are_relative_for_relative_entry_path() {
    if std::env::var_os(RELATIVE_GLOB_CHILD_ENV).is_some() {
        // Running in the child process, whose cwd is the temp dir.
        let val = pklr::eval_to_json(std::path::Path::new("main.pkl")).unwrap();
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

#[test]
fn import_glob_double_star_crosses_directories() {
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

    let val = pklr::eval_to_json(&dir.join("hk.pkl")).unwrap();
    assert_eq!(val["value"], "foo");
    assert_eq!(val["has_self"], false);
}

#[test]
fn import_glob_star_matches_one_directory_segment() {
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

    let val = pklr::eval_to_json(&dir.join("hk.pkl")).unwrap();
    assert_eq!(val["value"], "foo");
    assert_eq!(val["has_nested"], false);
}

#[test]
fn import_glob_double_star_slash_matches_root_and_nested_files() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["root_value"], "root");
    assert_eq!(val["nested_value"], "nested");
}

#[cfg(unix)]
#[test]
fn import_glob_skips_symlinked_files() {
    let temp = TestTempDir::new("pklr_test_import_glob_symlinked_files");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("real")).unwrap();
    std::fs::write(dir.join("real/foo.pkl"), r#"value = "foo""#).unwrap();
    std::os::unix::fs::symlink(dir.join("real/foo.pkl"), dir.join("linked.pkl")).unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import* "*.pkl" as Index
value = Index.containsKey("linked.pkl")
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["value"], false);
}

#[cfg(unix)]
#[test]
fn import_glob_skips_broken_symlinks() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["value"], "good");
    assert_eq!(val["has_broken"], false);
}

#[test]
fn import_glob_expression_binds_matched_modules() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        val["keys"],
        serde_json::json!(["generated/alpha.pkl", "generated/beta.pkl"])
    );
    assert_eq!(val["values"], serde_json::json!(["alpha", "beta"]));
    assert_eq!(val["alpha"], "alpha");
}

#[test]
fn import_glob_expression_without_matches_is_empty() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["count"], 0);
}

#[test]
fn import_glob_expression_skips_the_enclosing_module() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["keys"], serde_json::json!(["other.pkl"]));
}

#[test]
fn import_glob_expression_resolves_against_its_own_module() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn inherited_import_glob_expression_resolves_against_the_base_module() {
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

    let val = pklr::eval_to_json(&dir.join("child.pkl")).unwrap();
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

#[test]
fn import_expressions_are_listing_elements() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn import_expression_evaluates_a_single_module() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["value"], "alpha");
}

#[test]
fn import_expression_field_access_only_evaluates_that_field() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["plain"], "h-ok");
    assert_eq!(val["derived"], "h-ok-derived");
    assert_eq!(val["chained"], "deep");
}

#[test]
fn import_expression_reports_missing_modules() {
    let temp = TestTempDir::new("pklr_test_import_expr_missing");
    let dir = temp.path();
    std::fs::write(
        dir.join("main.pkl"),
        r#"value = import("generated/alpha.pkl").value"#,
    )
    .unwrap();

    let err = pklr::eval_to_json(&dir.join("main.pkl"))
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
        err.contains("Unexpected token `uri`. Expected `\"`"),
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
        err.contains("Unexpected token `pattern`. Expected `\"`"),
        "{err}"
    );
}

#[test]
fn unused_import_is_not_evaluated() {
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
    let val = pklr::eval_to_json(&path).unwrap();
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

#[test]
fn import_used_by_inherited_class_default_is_loaded() {
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

    let mut ev = Evaluator::new();
    let val = ev.eval_source(src, &dir.join("child.pkl")).unwrap();
    let json = val.to_json();
    assert_eq!(json["result"]["name"], "hk");
}

#[test]
fn imported_amends_base_uses_inherited_scope() {
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

    let val = pklr::eval_to_json(&dir.join("child.pkl")).unwrap();
    assert_eq!(val["name"], "hk");
    assert_eq!(val["baseName"], "hk");
}

#[test]
fn amended_object_keeps_definition_site_import_scope() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "steps": {
                "a": {"check": {"command": "lint"}, "prefix": "x"},
                "b": {"check": "true"}
            }
        })
    );
}

#[test]
fn amended_object_keeps_definition_site_scope_when_local_shadows_import() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "steps": {
                "a": {"check": {"command": "lint"}, "prefix": "x"}
            }
        })
    );
}

#[test]
fn amended_object_uses_amendment_site_scope_for_overlay_entries() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({
            "inherited": "definition",
            "overlay": "amendment"
        })
    );
}

#[test]
fn amended_object_keeps_definition_site_type_alias_scope() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(json["result"], serde_json::json!({"value": "ok"}));
}

#[test]
fn amended_object_uses_amendment_scope_for_replaced_default() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"entry": {"selected": "amendment"}})
    );
}

#[test]
fn amended_object_applies_default_body_amendments() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn repeated_default_body_amendments_reject_external_lists() {
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

    let error = pklr::eval_to_json(&dir.join("main.pkl"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("Cannot instantiate, or amend an instance of, external class `List`."));
}

#[test]
fn amended_object_reconstructs_classes_in_definition_namespace() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(json["result"], serde_json::json!({"item": {"value": "ok"}}));
}

#[test]
fn amended_object_overlay_can_reference_later_inherited_sibling() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"selected": "inherited", "later": "inherited"})
    );
    let json = pklr::eval_to_json(&dir.join("shadowed.pkl")).unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"selected": "module", "later": "inherited"})
    );
}

#[test]
fn repeated_amendment_keeps_each_entries_lexical_scope() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"inherited": "definition", "overlay": "middle"})
    );
}

#[test]
fn derived_class_amendment_keeps_parent_and_child_lexical_scopes() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn derived_class_amendment_seeds_inherited_property_values() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json["result"],
        serde_json::json!({"later": "parent property", "selected": "child module"})
    );
}

#[test]
fn nested_class_inheritance_keeps_each_definition_scope() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn unassigned_inherited_property_does_not_shadow_amendment_scope() {
    let temp = TestTempDir::new("pklr_test_unassigned_inherited_property_scope");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        r#"
local collision = "definition"
open class Item {
    hidden collision: String
    selected: String?
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(json["result"], serde_json::json!({"selected": "amendment"}));
}

#[test]
fn amended_typed_property_keeps_definition_site_class_identity() {
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

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json["hooks"]["check"]["steps"]["demo"]["tests"],
        serde_json::json!({"case": {"expect": {"stdout": "ok"}}})
    );
}

#[test]
fn scoped_inherited_base_does_not_pollute_import_cache() {
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

    let mut ev = Evaluator::new();
    let child_val = ev.eval_file(&dir.join("child.pkl")).unwrap();
    assert_eq!(child_val.to_json()["result"], "hk");

    let err = ev.eval_file(&dir.join("Base.pkl")).unwrap_err();
    assert!(
        err.to_string().contains("Cannot find property `meta`."),
        "{err}"
    );
}

#[test]
fn imported_amends_and_extends_bases_keep_separate_values() {
    let temp = TestTempDir::new("pklr_test_imported_dual_inherited_bases");
    let dir = temp.path();
    std::fs::write(dir.join("AmendsBase.pkl"), r#"amendsName = meta.name"#).unwrap();
    std::fs::write(
        dir.join("ExtendsBase.pkl"),
        r#"open module ExtendsBase
extendsName = meta.name"#,
    )
    .unwrap();
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

    let val = pklr::eval_to_json(&dir.join("child.pkl")).unwrap();
    assert_eq!(val["extendsName"], "hk");
    assert_eq!(val["amended"], "hk");
    assert_eq!(val["extended"], "hk");
}

#[test]
fn import_used_only_by_annotation_does_not_create_builtin_cycle() {
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
    let val = pklr::eval_to_json(&path).unwrap();
    assert_eq!(val["result"], "ok");
    assert_eq!(val["amended"], "stdin");
}

#[test]
fn unused_import_glob_field_does_not_evaluate() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["result"], "ok");
}

#[test]
fn partial_import_expands_this_and_module_dependencies() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["y"], 42);
    assert_eq!(val["z"], 43);
}

#[test]
fn partial_import_includes_generic_param_fields() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["other"], "other");
    assert_eq!(val["enabled"], true);
}

#[test]
fn partial_import_ignores_imports_used_only_by_skipped_properties() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["result"], "ok");
}

#[test]
fn partial_import_treats_object_method_receiver_as_whole_import() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["result"]["first"], "one");
    assert_eq!(val["result"]["second"], "two");
}

#[test]
fn partial_import_treats_object_map_values_receiver_as_whole_import() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["result"]["first"], 2);
    assert_eq!(val["result"]["second"], 3);
}

#[test]
fn partial_import_keeps_user_defined_method_names_field_scoped() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["result"], 42);
}

#[test]
fn partial_import_includes_sibling_function_called_by_requested_function() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn imported_typed_mapping_does_not_leak_schema_classes() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["steps"]["original"]["check"], "echo original");
    assert!(val["steps"]["original"].get("Script").is_none(), "{val}");
}

#[test]
fn nested_imported_mapping_amendments_preserve_entries() {
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

    let val = pklr::eval_to_json(&dir.join("All.pkl")).unwrap();
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

#[test]
fn module_imported_narrowly_by_many_modules() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn reused_evaluator_rereads_changed_imports() {
    let temp = TestTempDir::new("pklr_test_reused_evaluator_imports");
    let dir = temp.path();
    std::fs::write(dir.join("dep.pkl"), "value = 1\nother = 2\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\"\nwhole = dep\nnarrow = dep.value\n",
    )
    .unwrap();

    let mut ev = pklr::eval::Evaluator::new();
    let first = ev.eval_file(&dir.join("main.pkl")).unwrap().to_json();
    assert_eq!(first["narrow"], 1);
    assert_eq!(first["whole"]["other"], 2);

    std::fs::write(dir.join("dep.pkl"), "value = 10\nother = 20\n").unwrap();
    let second = ev.eval_file(&dir.join("main.pkl")).unwrap().to_json();
    assert_eq!(second["narrow"], 10);
    assert_eq!(second["whole"]["other"], 20);
}

#[test]
fn narrowed_import_that_read_a_cycle_placeholder_is_not_reused() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    // While hub is being imported, dep sees hub's placeholder. Once hub is
    // done, the same narrowed import of dep must see its real value rather
    // than a result cached during the cycle.
    assert_eq!(val["hubDuring"], "placeholder");
    assert_eq!(val["after"], "T");
}

#[test]
fn import_glob_evaluates_only_referenced_modules() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["result"], "used");
}

#[test]
fn import_glob_keys_still_see_every_module() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["a"], "a");
    assert_eq!(val["count"], 2);
}

#[test]
fn import_glob_keeps_modules_an_amended_base_reads() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["fromBase"], "b");
    assert_eq!(val["fromChild"], "a");
}

#[test]
fn import_glob_field_read_named_like_a_module_sees_every_module() {
    let temp = TestTempDir::new("pklr_test_glob_field_named_like_module");
    let dir = temp.path();
    std::fs::write(dir.join("length"), "value = \"L\"\n").unwrap();
    std::fs::write(dir.join("other"), "value = \"O\"\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import* \"*\" as Parts\nl = Parts[\"length\"].value\ncount = Parts.length\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["l"], "L");
    assert_eq!(val["count"], 2);
}

#[test]
fn import_glob_keeps_modules_read_by_type_alias_constraints() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["fromA"], "a");
    assert_eq!(val["ok"], true);
}

#[test]
fn indexed_ordinary_import_is_evaluated_whole() {
    let temp = TestTempDir::new("pklr_test_indexed_ordinary_import");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        "expected = \"b\"\ntypealias IsB = String(this == expected)\nresult = \"b\" is IsB\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\n`out` = Dep[\"result\"]\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["out"], true);
}

#[test]
fn import_glob_in_a_base_keeps_modules_its_children_read() {
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

    let extended = pklr::eval_to_json(&dir.join("B.pkl")).unwrap();
    assert_eq!(extended["a"], "a");
    assert_eq!(extended["b"], "b");
    let amended = pklr::eval_to_json(&dir.join("C.pkl")).unwrap();
    assert_eq!(amended["c"], "b");
}

#[test]
fn narrowed_import_follows_type_alias_constraints() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_type_alias");
    let dir = temp.path();
    std::fs::write(
        dir.join("dep.pkl"),
        "expected = \"b\"\ntypealias IsB = String(this == expected)\nresult = \"b\" is IsB\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as Dep\ntypealias IsTrue = Boolean(this == Dep.result)\nok = true is IsTrue\n`out` = Dep.result\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["ok"], true);
    assert_eq!(val["out"], true);
}

#[test]
fn narrowed_import_follows_module_reads_in_class_defaults() {
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
        "import \"dep.pkl\" as D\n`out` = D.result\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("class.pkl"),
        "import \"dep.pkl\" as D\n`out` = new D.Bar {}\n",
    )
    .unwrap();

    let dep = pklr::eval_to_json(&dir.join("dep.pkl")).unwrap();
    assert_eq!(
        dep,
        serde_json::json!({"min": 1, "result": {"bar": {"a": 1}}})
    );
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val, serde_json::json!({"out": {"bar": {"a": 1}}}));
    let class = pklr::eval_to_json(&dir.join("class.pkl")).unwrap();
    assert_eq!(class, serde_json::json!({"out": {"a": 1}}));
}

#[test]
fn narrowed_import_ignores_shadowed_module_in_constraints() {
    let temp = TestTempDir::new("pklr_test_shadowed_module_constraint");
    let dir = temp.path();
    let main = dir.join("main.pkl");
    std::fs::write(
        &main,
        "import \"types.pkl\" as Dep\nresult = new Dep.C {}\n",
    )
    .unwrap();
    // A `module` bound to a fresh object, or by a `for` generator over fresh
    // values, is not the module, so `bad` (and its missing import) is never read.
    for constraint in [
        "let (`module` = new Dynamic { bad = true }) `module`.bad",
        "new Listing { for (`module` in new Listing { new Dynamic { bad = true } }) { `module`.bad } }.length == 1",
        "let (`module` = new Dynamic { bad = true }) new Listing { for (`module` in new Listing { `module` }) { `module`.bad } }.length == 1",
        "new Listing { for (`module` in new Mapping { [\"k\"] = new Dynamic { bad = true } }) { `module`.bad } }.length == 1",
    ] {
        std::fs::write(
            dir.join("types.pkl"),
            format!(
                "bad = import(\"missing.pkl\")\ntypealias Check = Boolean({constraint})\nclass C {{ ok: Check = true }}\n"
            ),
        )
        .unwrap();
        let value =
            pklr::eval_to_json(&main).unwrap_or_else(|error| panic!("{constraint}: {error}"));
        assert_eq!(
            value,
            serde_json::json!({"result": {"ok": true}}),
            "{constraint}"
        );
    }

    // A binding that may hold the module, or a collection read outside the
    // shadow, still reads module members.
    for constraint in [
        "module.bad != null",
        "let (`module` = module) `module`.bad != null",
        "new Listing { for (`module` in module.bad) { `module`.x } }.length == 0",
    ] {
        std::fs::write(
            dir.join("types.pkl"),
            format!(
                "bad = import(\"missing.pkl\")\ntypealias Check = Boolean({constraint})\nclass C {{ ok: Check = true }}\n"
            ),
        )
        .unwrap();
        assert!(pklr::eval_to_json(&main).is_err(), "{constraint}");
    }

    // An alias of the module keeps the module member it reads, including one
    // rebound after an outer shadow.
    for constraint in [
        "let (`module` = module) `module`.bad",
        "let (real = module) let (`module` = new Dynamic { bad = false }) let (`module` = real) `module`.bad",
        "let (real = module) let (`module` = new Dynamic { bad = false }) ((`module`) -> new Listing { for (`module` in new Listing { `module` }) { `module`.bad } }.length == 1).apply(real)",
    ] {
        std::fs::write(
            dir.join("types.pkl"),
            format!(
                "bad = true\ntypealias Check = Boolean({constraint})\nclass C {{ ok: Check = true }}\n"
            ),
        )
        .unwrap();
        let value =
            pklr::eval_to_json(&main).unwrap_or_else(|error| panic!("{constraint}: {error}"));
        assert_eq!(
            value,
            serde_json::json!({"result": {"ok": true}}),
            "{constraint}"
        );
    }
}

#[test]
fn narrowed_import_keeps_real_module_read_through_inner_object_shadow() {
    let temp = TestTempDir::new("pklr_test_inner_object_module_shadow");
    let dir = temp.path();
    // An object body that declares its own `module` property rebinds the name
    // for its entries, even though an outer binding proved `module` was not
    // the module. `real` holds the module, so `good` is a real read.
    for local in ["local ", ""] {
        std::fs::write(
            dir.join("dep.pkl"),
            format!(
                "good = true\nresult = let (real = module) let (`module` = new Dynamic {{ good = false }}) (new Dynamic {{\n  {local}`module` = real\n  ok = `module`.good\n}}).ok\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("main.pkl"),
            "import \"dep.pkl\" as Dep\nresult = Dep.result\n",
        )
        .unwrap();
        let value = pklr::eval_to_json(&dir.join("main.pkl"))
            .unwrap_or_else(|error| panic!("{local:?}: {error}"));
        assert_eq!(value, serde_json::json!({"result": true}), "{local:?}");
    }
}

#[test]
fn narrowed_import_keeps_shadow_when_inner_module_property_is_not_the_module() {
    let temp = TestTempDir::new("pklr_test_inner_object_non_module_shadow");
    let dir = temp.path();
    // Both bindings of `module` hold ordinary objects, so `bad` (and its
    // missing import) is never read for the requested `result`.
    for local in ["local ", ""] {
        std::fs::write(
            dir.join("dep.pkl"),
            format!(
                "import \"missing.pkl\" as Missing\nbad = Missing.value\nresult = let (`module` = new Dynamic {{ bad = false }}) (new Dynamic {{\n  {local}`module` = new Dynamic {{ bad = true }}\n  ok = `module`.bad\n}}).ok\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("main.pkl"),
            "import \"dep.pkl\" as Dep\nresult = Dep.result\n",
        )
        .unwrap();
        let value = pklr::eval_to_json(&dir.join("main.pkl"))
            .unwrap_or_else(|error| panic!("{local:?}: {error}"));
        assert_eq!(value, serde_json::json!({"result": true}), "{local:?}");
    }
}

#[test]
fn imported_class_retains_its_type_alias_constraint_bindings() {
    let temp = TestTempDir::new("pklr_test_imported_class_constraint_scope");
    let dir = temp.path();
    for vendors in ["vendors", "module.vendors"] {
        for default in ["", " = \"amd\""] {
            std::fs::write(
                dir.join("types.pkl"),
                format!(
                    "const vendors = new Listing {{ \"amd\"; \"intel\" }}\ntypealias VendorName = String({vendors}.contains(this))\ntypealias Vendor = VendorName\nclass Host {{ vendor: Vendor{default} }}\n"
                ),
            )
            .unwrap();
            let main = dir.join("main.pkl");
            for shadow in [
                "",
                "local vendors = new Listing { \"caller\" }\ntypealias Vendor = Int\ntypealias VendorName = Boolean\n",
            ] {
                for body in ["vendor = selected", ""] {
                    if body.is_empty() && default.is_empty() {
                        continue;
                    }
                    std::fs::write(
                        &main,
                        format!(
                            "import \"types.pkl\" as T\n{shadow}local selected = \"amd\"\nhost = new T.Host {{ {body} }}\n"
                        ),
                    )
                    .unwrap();
                    let value = pklr::eval_to_json(&main).unwrap_or_else(|error| {
                        panic!("{vendors}, {default:?}, {shadow:?}, {body:?}: {error}")
                    });
                    assert_eq!(value, serde_json::json!({"host": {"vendor": "amd"}}));
                }
            }

            std::fs::write(
                &main,
                "import \"types.pkl\" as T\nlocal vendors = new Listing { \"caller\" }\ntypealias Vendor = String\nhost = new T.Host { vendor = \"caller\" }\n",
            )
            .unwrap();
            let error = pklr::eval_to_json(&main).unwrap_err().to_string();
            assert!(error.contains("vendors.contains(this)"), "{error}");
        }
    }
}

#[test]
fn narrowed_import_ignores_class_properties_named_like_module_properties() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val, serde_json::json!({"d": {"a": 6, "b": 6}}));
}

#[test]
fn narrowed_import_follows_module_reads_named_like_inherited_properties() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_inherited_name");
    let dir = temp.path();
    // `min` in `Child` is the inherited property, but `Child`'s defaults are
    // first evaluated before `Parent`'s members are merged in, so the class
    // must still be refreshed once the module's `min` exists.
    std::fs::write(
        dir.join("dep.pkl"),
        "min = 1\nopen class Parent { min = 2 }\nclass Child extends Parent { a = min }\n`out` = new Child {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as D\n`out` = D.`out`\n",
    )
    .unwrap();

    let dep = pklr::eval_to_json(&dir.join("dep.pkl")).unwrap();
    assert_eq!(dep["out"], serde_json::json!({"min": 2, "a": 2}));
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val, serde_json::json!({"out": {"min": 2, "a": 2}}));
}

#[test]
fn narrowed_import_skips_module_properties_named_like_inherited_method_reads() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_inherited_method");
    let dir = temp.path();
    // A method body runs on a built instance, where `min` is the inherited
    // property, so the unused module `min` is not needed.
    std::fs::write(
        dir.join("inherited.pkl"),
        "min = throw(\"unused\")\nopen class Parent { min = 2 }\nclass Child extends Parent { function getMin() = min }\n",
    )
    .unwrap();
    // A module property the instance doesn't have is still read by a method.
    std::fs::write(
        dir.join("module.pkl"),
        "max = 3\nopen class Parent { min = 2 }\nclass Child extends Parent { function getMax() = max }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"inherited.pkl\" as I\nimport \"module.pkl\" as M\nmin = (new I.Child {}).getMin()\nmax = (new M.Child {}).getMax()\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["min"], 2);
    assert_eq!(val["max"], 3);
}

#[test]
fn narrowed_import_follows_methods_called_by_class_defaults() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_default_method_call");
    let dir = temp.path();
    // `a` calls `getMin` while `Child`'s defaults are first evaluated, before
    // `Parent`'s `min` is merged in, so the class must still be refreshed,
    // also when the method is called through an alias of `this` or through
    // a nested object's `outer` (also kept in a local from deeper down).
    std::fs::write(
        dir.join("dep.pkl"),
        "min = 1\nopen class Parent { min = 2 }\nclass Child extends Parent {\n  function getMin() = min\n  a = getMin()\n}\nclass ThisChild extends Parent {\n  function getMin() = min\n  a = this.getMin()\n}\nclass AliasChild extends Parent {\n  function getMin() = min\n  a = let (self = this) self.getMin()\n}\nclass OuterChild extends Parent {\n  function getMin() = min\n  obj { a = outer.getMin() }\n  deep { local up = outer\n    inner { a = up.getMin() } }\n}\nchild = new Child {}\nthisChild = new ThisChild {}\naliasChild = new AliasChild {}\nouterChild = new OuterChild {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as D\nchild = D.child\nthisChild = D.thisChild\naliasChild = D.aliasChild\nouterChild = D.outerChild\n",
    )
    .unwrap();

    let dep = pklr::eval_to_json(&dir.join("dep.pkl")).unwrap();
    assert_eq!(dep["child"]["a"], 2);
    assert_eq!(dep["thisChild"]["a"], 2);
    assert_eq!(dep["aliasChild"]["a"], 2);
    assert_eq!(dep["outerChild"]["obj"]["a"], 2);
    assert_eq!(dep["outerChild"]["deep"]["inner"]["a"], 2);
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["child"]["a"], 2);
    assert_eq!(val["thisChild"]["a"], 2);
    assert_eq!(val["aliasChild"]["a"], 2);
    assert_eq!(val["outerChild"]["obj"]["a"], 2);
    assert_eq!(val["outerChild"]["deep"]["inner"]["a"], 2);
}

#[test]
fn narrowed_import_skips_methods_class_defaults_do_not_call() {
    let temp = TestTempDir::new("pklr_test_narrowed_import_default_this_read");
    let dir = temp.path();
    // `a` reads `this.x` and `obj` reads `outer.x` but neither calls a
    // method, and `this` in `obj` (or `outer` deeper down) is not the
    // instance, so `getMin` only runs on the built instance, where `min` is
    // the inherited property.
    std::fs::write(
        dir.join("dep.pkl"),
        "min = throw(\"unused\")\nopen class Parent { min = 2 }\nclass Child extends Parent {\n  x = 3\n  a = this.x\n  obj { y = 4; b = this.y; c = outer.x; inner { d = outer.y; e = (outer) } }\n  function getMin() = min\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\" as D\nchild = new D.Child {}\na = child.a\nb = child.obj.b\nc = child.obj.c\nmin = child.getMin()\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["a"], 3);
    assert_eq!(val["b"], 4);
    assert_eq!(val["c"], 3);
    assert_eq!(val["min"], 2);
}

#[test]
fn narrowed_import_follows_module_reads_but_not_checked_value_members() {
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["short"], true);
    assert_eq!(val["moduleRead"], true);
    assert_eq!(val["number"], true);
}

#[test]
fn narrowed_import_ignores_checked_value_names_in_is_expressions() {
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
        "length = throw(\"unused\")\nresult = new Listing { 1 } is Listing<Int>(length == 1)\n",
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["plain"], true);
    assert_eq!(val["classed"], true);
    assert_eq!(val["aliased"], true);
    assert_eq!(val["generic"], true);
    assert_eq!(val["nullable"], true);
}

#[test]
fn narrowed_import_resolves_local_aliases_in_constraint_bases() {
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
        "length = throw(\"unused\")\ntypealias L = Listing<Int>\nresult = new Listing { 1 } is L(length == 1)\n",
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

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["string"], true);
    assert_eq!(val["chain"], true);
    assert_eq!(val["listing"], true);
    assert_eq!(val["number"], true);
    assert_eq!(val["nullable"], true);
}

#[test]
fn narrowed_import_respects_aliases_redeclared_in_nested_bodies() {
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
        "import \"dep.pkl\" as Dep\nimport \"dep_string.pkl\" as DepString\nimport \"dep_order.pkl\" as DepOrder\nimport \"dep_followed.pkl\" as DepFollowed\nimport \"dep_followed_same.pkl\" as DepFollowedSame\nimport \"dep_followed_shadowed.pkl\" as DepFollowedShadowed\nimport \"dep_shadowed.pkl\" as DepShadowed\nimport \"dep_unrelated.pkl\" as DepUnrelated\nimport \"dep_followed_unrelated.pkl\" as DepFollowedUnrelated\n`out` = Dep.result.ok\noutString = DepString.result.ok\noutOrder = DepOrder.result.ok\noutFollowed = DepFollowed.result.ok\noutFollowedSame = DepFollowedSame.result.ok\noutFollowedShadowed = DepFollowedShadowed.result.ok\noutShadowed = DepShadowed.result.ok\noutUnrelated = DepUnrelated.result.ok\noutFollowedUnrelated = DepFollowedUnrelated.result.ok\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
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

#[test]
fn narrowed_import_resolves_aliases_in_followed_class_bodies() {
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
        "import \"dep.pkl\" as Dep\n`out` = Dep.result.name\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["out"], "b");
}

#[test]
fn module_in_imported_class_body_means_the_class_module() {
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

    let val = pklr::eval_to_json(&dir.join("dep2.pkl")).unwrap();
    assert_eq!(val["result"]["v"], "b");
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["r"], "b");
    assert_eq!(val["fresh"]["v"], "b");
}

#[test]
fn imported_class_reading_missing_module_property_reports_error() {
    let temp = TestTempDir::new("pklr_test_imported_poisoned_class");
    let dir = temp.path();
    std::fs::write(dir.join("dep.pkl"), "class C { v = module.missing }\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"dep.pkl\"\nresult = new dep.C {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("read.pkl"), "import \"dep.pkl\"\nresult = dep.C\n").unwrap();

    // The module defining the class still evaluates; the class is unused.
    let val = pklr::eval_to_json(&dir.join("dep.pkl")).unwrap();
    assert_eq!(val, serde_json::json!({}));
    // The failed class's error metadata must not stop amending the module
    // object from keeping its evaluated members.
    std::fs::write(
        dir.join("depx.pkl"),
        "x = 1\nclass C { v = module.missing }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("amend.pkl"),
        "import \"depx.pkl\" as dep\nresult = (dep) { y = 2 }\nr2 = (dep) { y = 3 }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("amend.pkl")).unwrap();
    assert_eq!(val["result"], serde_json::json!({"x": 1, "y": 2}));
    assert_eq!(val["r2"], serde_json::json!({"x": 1, "y": 3}));
    // Null-safe and index reads report the saved error too.
    std::fs::write(
        dir.join("nullsafe.pkl"),
        "import \"dep.pkl\"\nresult = dep?.C\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("index.pkl"),
        "import \"dep.pkl\"\nresult = dep[\"C\"]\n",
    )
    .unwrap();
    for importer in ["main.pkl", "read.pkl", "nullsafe.pkl", "index.pkl"] {
        let err = pklr::eval_to_json(&dir.join(importer))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing"), "{importer}: {err}");
    }
}

#[test]
fn narrowed_import_follows_locals_with_module_aliases() {
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
        "import \"dep.pkl\"\n`out` = dep.result.v\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["out"], true);
}

#[test]
fn narrowed_import_reads_qualified_type_roots_and_nested_classes() {
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
        "import \"dep.pkl\" as Dep\nimport \"dep_class.pkl\" as DepClass\n`out` = Dep.result\noutClass = DepClass.result.inst\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["out"], serde_json::json!({ "a": 1 }));
    assert_eq!(val["outClass"], serde_json::json!({ "a": 1 }));
}
// ============================================================
// Triple-dot imports
// ============================================================

#[test]
fn triple_dot_import_searches_ancestor_directories() {
    let temp = TestTempDir::new("pklr_test_triple_dot_ancestors");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("helper")).unwrap();
    std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
    std::fs::write(dir.join("helper/Lib.pkl"), "name = \"root helper\"\n").unwrap();
    // Not found: the module's own directory is not searched.
    std::fs::create_dir_all(dir.join("a/b/c/helper")).unwrap();
    std::fs::write(dir.join("a/b/c/helper/Lib.pkl"), "name = \"own dir\"\n").unwrap();
    std::fs::write(
        dir.join("a/b/c/main.pkl"),
        "import \".../helper/Lib.pkl\"\nname = Lib.name\nviaExpr = import(\".../helper/Lib.pkl\").name\n",
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("a/b/c/main.pkl")).unwrap();
    assert_eq!(val["name"], "root helper");
    assert_eq!(val["viaExpr"], "root helper");

    // The nearest ancestor wins.
    std::fs::create_dir_all(dir.join("a/b/helper")).unwrap();
    std::fs::write(dir.join("a/b/helper/Lib.pkl"), "name = \"nearest\"\n").unwrap();
    let val = pklr::eval_to_json(&dir.join("a/b/c/main.pkl")).unwrap();
    assert_eq!(val["name"], "nearest");
}

#[test]
fn bare_triple_dot_amends_same_named_module_in_ancestor() {
    let temp = TestTempDir::new("pklr_test_triple_dot_bare");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("sub/inner")).unwrap();
    std::fs::write(dir.join("Config.pkl"), "a = 1\nb = 2\n").unwrap();
    std::fs::write(dir.join("sub/inner/Config.pkl"), "amends \"...\"\nb = 3\n").unwrap();

    let val = pklr::eval_to_json(&dir.join("sub/inner/Config.pkl")).unwrap();
    assert_eq!(val, serde_json::json!({ "a": 1, "b": 3 }));
}

#[test]
fn triple_dot_import_from_relative_entry_path() {
    let temp = TestTempDir::new("pklr_test_triple_dot_relative");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("input/api")).unwrap();
    std::fs::create_dir_all(dir.join("input-helper/api")).unwrap();
    std::fs::write(dir.join("input-helper/api/Lib.pkl"), "x = 42\n").unwrap();
    std::fs::write(
        dir.join("input/api/main.pkl"),
        "import \".../input-helper/api/Lib.pkl\"\nx = Lib.x\n",
    )
    .unwrap();

    // Evaluate through a relative path, as `pkl eval api/main.pkl` would
    // from the `input` directory.
    let mut ev = pklr::eval::Evaluator::new();
    let source = std::fs::read_to_string(dir.join("input/api/main.pkl")).unwrap();
    let relative = pathdiff(&dir.join("input/api/main.pkl"));
    let val = ev.eval_source(&source, &relative).unwrap().to_json();
    assert_eq!(val["x"], 42);
}

/// `path` relative to the current directory, through `..` components.
fn pathdiff(path: &std::path::Path) -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let Some(common) = cwd.ancestors().find(|ancestor| path.starts_with(ancestor)) else {
        return path.to_path_buf();
    };
    let ups = cwd.strip_prefix(common).unwrap().components().count();
    let mut relative: std::path::PathBuf = std::iter::repeat_n("..", ups).collect();
    relative.push(path.strip_prefix(common).unwrap());
    relative
}

#[test]
fn invalid_triple_dot_uri_is_rejected() {
    let temp = TestTempDir::new("pklr_test_triple_dot_invalid");
    let dir = temp.path();
    std::fs::write(dir.join("main.pkl"), "import \".../\"\nx = 1\n").unwrap();
    let err = pklr::eval_to_json(&dir.join("main.pkl")).unwrap_err();
    assert!(err.to_string().contains("has invalid syntax"), "{err}");
}

#[test]
fn analyze_imports_resolves_triple_dot_imports() {
    let temp = TestTempDir::new("pklr_test_triple_dot_analyze");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("a/b")).unwrap();
    std::fs::write(dir.join("Lib.pkl"), "x = 1\n").unwrap();
    std::fs::write(
        dir.join("a/b/main.pkl"),
        "import \".../Lib.pkl\"\ny = Lib.x\n",
    )
    .unwrap();
    let imports = pklr::analyze_imports(&dir.join("a/b/main.pkl")).unwrap();
    assert_eq!(imports, vec![dir.join("Lib.pkl")]);
}

#[test]
fn subclass_untyped_new_uses_imported_parent_type_scope() {
    let temp = TestTempDir::new("pklr_test_subclass_imported_parent_type");
    let dir = temp.path();
    std::fs::write(
        dir.join("lib.pkl"),
        r#"
class P { a = 1; b = 2 }
open class C { p: P = new { a = 5 } }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "lib.pkl"
class D extends lib.C { p = new { b = 3 } }
d = new D {}
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(json["d"]["p"], serde_json::json!({"a": 1, "b": 3}));
}

#[test]
fn untyped_new_amendment_resolves_imported_type_where_declared() {
    let temp = TestTempDir::new("pklr_test_untyped_new_imported_amendment_type");
    let dir = temp.path();
    std::fs::write(
        dir.join("Lib.pkl"),
        "class F { a = 1; b = 3 }\nclass H { f: F }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Lib.pkl"
class F { a = 99 }
h = new Lib.H { f = new { b = 10 } }
h2 = (h) { f = new { a = 7 } }
"#,
    )
    .unwrap();
    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(json["h"]["f"], serde_json::json!({"a": 1, "b": 10}));
    assert_eq!(json["h2"]["f"], serde_json::json!({"a": 7, "b": 3}));
}

// ============================================================
// Late binding through amending modules
// ============================================================

#[test]
fn late_binding_resolves_properties_declared_in_reverse_dependency_order() {
    let temp = TestTempDir::new("pklr_test_late_binding_reverse_order");
    let dir = temp.path();
    std::fs::write(
        dir.join("Template.pkl"),
        "c = b + 1\nb = a + 1\na = base + 1\nbase = 1\n",
    )
    .unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 10\n").unwrap();

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"base": 10, "a": 11, "b": 12, "c": 13})
    );
}

#[test]
fn late_binding_resolves_forward_references_between_inherited_objects() {
    let temp = TestTempDir::new("pklr_test_late_binding_forward_reference");
    let dir = temp.path();
    std::fs::write(
        dir.join("Template.pkl"),
        r#"
summary = new { first = items[0]; count = items.length }
items = List(base, base + 1)
base = 1
"#,
    )
    .unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 7\n").unwrap();

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(json["summary"], serde_json::json!({"first": 7, "count": 2}));
    assert_eq!(json["items"], serde_json::json!([7, 8]));
}

#[test]
fn late_binding_keeps_an_override_made_in_a_middle_layer() {
    let temp = TestTempDir::new("pklr_test_late_binding_middle_override");
    let dir = temp.path();
    std::fs::write(
        dir.join("Template.pkl"),
        r#"
forward = middle + 1
middle = base * 2
server = new { host = name + ".example"; port = base + 1 }
name = "template"
base = 1
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Middle.pkl"),
        "amends \"Template.pkl\"\nmiddle = base * 3\nname = \"middle\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("Leaf.pkl"), "amends \"Middle.pkl\"\nbase = 5\n").unwrap();

    let json = pklr::eval_to_json(&dir.join("Leaf.pkl")).unwrap();
    assert_eq!(json["middle"], 15);
    assert_eq!(json["forward"], 16);
    assert_eq!(
        json["server"],
        serde_json::json!({"host": "middle.example", "port": 6})
    );
}

#[test]
fn late_binding_recovers_from_a_property_that_fails_until_a_later_one_is_bound() {
    let temp = TestTempDir::new("pklr_test_late_binding_early_failure");
    let dir = temp.path();
    // With the template's own `count`, `per` divides by zero. `report` is
    // declared first, so it reads that failure before `per` is rebound.
    std::fs::write(
        dir.join("Template.pkl"),
        "total = 20\ncount = 0\nreport = per + 1\nper = total ~/ count\n",
    )
    .unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\ncount = 4\n").unwrap();

    let json = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"total": 20, "count": 4, "report": 6, "per": 5})
    );
}

#[test]
fn late_binding_reports_a_failure_no_pass_resolves_unless_it_is_overridden() {
    let temp = TestTempDir::new("pklr_test_late_binding_unresolved_failure");
    let dir = temp.path();
    std::fs::write(
        dir.join("Template.pkl"),
        "bad = throw(\"boom\")\nvalue = base + 1\nbase = 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("broken.pkl"),
        "amends \"Template.pkl\"\nbase = 2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("fixed.pkl"),
        "amends \"Template.pkl\"\nbase = 2\nbad = 0\n",
    )
    .unwrap();

    let error = pklr::eval_to_json(&dir.join("broken.pkl"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("boom"), "unexpected error: {error}");
    let json = pklr::eval_to_json(&dir.join("fixed.pkl")).unwrap();
    assert_eq!(json, serde_json::json!({"bad": 0, "value": 3, "base": 2}));
}

/// Evaluate `main` with room for a long chain: evaluation recurses once per
/// link, which a test thread's default stack cannot hold in a debug build.
fn eval_chain(main: std::path::PathBuf) -> serde_json::Value {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || pklr::eval_to_json(&main).unwrap())
        .unwrap()
        .join()
        .unwrap()
}

// Each of these reads a property declared after it, through a lambda, method
// or amended object that keeps the scope it was made in. Late binding runs in
// passes, and an override of `base` reaches `res` through such captures one
// pass per link, so the result depends on how many passes run.

#[test]
fn late_binding_reaches_through_a_long_chain_of_lambdas() {
    let temp = TestTempDir::new("pklr_test_late_binding_lambda_chain");
    let dir = temp.path();
    let depth = 16;
    let mut template = String::from("res = f1.apply(1)\n");
    for n in 1..depth {
        template.push_str(&format!("hidden f{n} = (x) -> f{}.apply(x)\n", n + 1));
    }
    template.push_str(&format!("hidden f{depth} = (x) -> x + r\n"));
    template.push_str("base = 1\nr = base + 1\n");
    std::fs::write(dir.join("Template.pkl"), template).unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 10\n").unwrap();

    assert_eq!(eval_chain(dir.join("main.pkl"))["res"], 12);
}

#[test]
fn late_binding_reaches_through_a_long_chain_of_methods() {
    let temp = TestTempDir::new("pklr_test_late_binding_method_chain");
    let dir = temp.path();
    let depth = 24;
    let mut template = String::from("res = f1(1)\n");
    for n in 1..depth {
        template.push_str(&format!("function f{n}(x) = f{}(x)\n", n + 1));
    }
    template.push_str(&format!("function f{depth}(x) = x + r\n"));
    template.push_str("base = 1\nr = base + 1\n");
    std::fs::write(dir.join("Template.pkl"), template).unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 10\n").unwrap();

    assert_eq!(eval_chain(dir.join("main.pkl"))["res"], 12);
}

#[test]
fn late_binding_reaches_through_a_long_chain_of_amended_objects() {
    let temp = TestTempDir::new("pklr_test_late_binding_amended_object_chain");
    let dir = temp.path();
    let depth = 10;
    let mut template = String::from("res = (o1) { go = true }.v\n");
    for n in 1..depth {
        template.push_str(&format!(
            "o{n} = new {{ go = false; v = if (go) (o{}) {{ go = true }}.v else 0 }}\n",
            n + 1
        ));
    }
    template.push_str(&format!(
        "o{depth} = new {{ go = false; v = if (go) r else 0 }}\n"
    ));
    template.push_str("base = 1\nr = base + 1\n");
    std::fs::write(dir.join("Template.pkl"), template).unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 10\n").unwrap();

    assert_eq!(eval_chain(dir.join("main.pkl"))["res"], 11);
}

#[test]
fn late_binding_reaches_through_a_recursive_lambda() {
    let temp = TestTempDir::new("pklr_test_late_binding_recursive_lambda");
    let dir = temp.path();
    // Each call goes back through the version of `sum` captured by an earlier
    // pass, so a call eight deep needs a chain of eight versions. The passes
    // run once per property of the template, plus one, so it has enough.
    let padding: String = (0..8).map(|n| format!("pad{n} = {n}\n")).collect();
    std::fs::write(
        dir.join("Template.pkl"),
        format!("hidden sum = (x) -> if (x <= 0) first else sum.apply(x - 1) + step\nres = sum.apply(8)\nfirst = 2\nstep = 1\n{padding}"),
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "amends \"Template.pkl\"\nfirst = 100\nstep = 4\n",
    )
    .unwrap();

    assert_eq!(eval_chain(dir.join("main.pkl"))["res"], 132);
}

#[test]
fn late_binding_reaches_through_a_long_chain_of_module_reads() {
    let temp = TestTempDir::new("pklr_test_late_binding_module_chain");
    let dir = temp.path();
    let depth = 8;
    let mut template = String::from("res = module.f1.apply(1)\n");
    for n in 1..depth {
        template.push_str(&format!(
            "hidden f{n} = (x) -> module.f{}.apply(x)\n",
            n + 1
        ));
    }
    template.push_str(&format!("hidden f{depth} = (x) -> x + module.r\n"));
    template.push_str("base = 1\nr = base + 1\n");
    std::fs::write(dir.join("Template.pkl"), template).unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 10\n").unwrap();

    assert_eq!(eval_chain(dir.join("main.pkl"))["res"], 12);
}

#[test]
fn late_binding_reaches_through_a_long_chain_of_this_reads() {
    let temp = TestTempDir::new("pklr_test_late_binding_this_chain");
    let dir = temp.path();
    let depth = 8;
    let mut template = String::from("res = this.f1.apply(1)\n");
    for n in 1..depth {
        template.push_str(&format!("hidden f{n} = (x) -> this.f{}.apply(x)\n", n + 1));
    }
    template.push_str(&format!("hidden f{depth} = (x) -> x + this.r\n"));
    template.push_str("base = 1\nr = base + 1\n");
    std::fs::write(dir.join("Template.pkl"), template).unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 10\n").unwrap();

    assert_eq!(eval_chain(dir.join("main.pkl"))["res"], 12);
}

#[test]
fn late_binding_reaches_through_a_long_chain_of_outer_reads() {
    let temp = TestTempDir::new("pklr_test_late_binding_outer_chain");
    let dir = temp.path();
    // pklr resolves `outer` inside a lambda to the enclosing module, which pkl
    // does not; the reads must still count as reads of the module's members.
    let depth = 8;
    let mut template = String::new();
    for n in 1..depth {
        template.push_str(&format!(
            "o{n} = new {{ f = (x) -> outer.o{}.f.apply(x) }}\n",
            n + 1
        ));
    }
    template.push_str(&format!("o{depth} = new {{ f = (x) -> x + outer.r }}\n"));
    template.push_str("res = o1.f.apply(1)\nbase = 1\nr = base + 1\n");
    std::fs::write(dir.join("Template.pkl"), template).unwrap();
    std::fs::write(dir.join("main.pkl"), "amends \"Template.pkl\"\nbase = 10\n").unwrap();

    assert_eq!(eval_chain(dir.join("main.pkl"))["res"], 12);
}

#[test]
fn typed_listing_elements_of_recursive_classes_in_an_imported_module() {
    let temp = TestTempDir::new("pklr_typed_listing_recursive_import");
    std::fs::write(
        temp.path().join("lib.pkl"),
        r#"
class Task { name: String; loopVar: String = "item"; block: Listing<Task>?; steps: Listing<Step>? }
class Step { id: String = "s"; sub: Listing<Task>? }
class Play { tasks: Listing<Task> }
"#,
    )
    .unwrap();
    let main = temp.path().join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "lib.pkl" as L
p = new L.Play { tasks { new { name = "t"; block { new { name = "c" } }; steps { new { sub { new { name = "z" } } } } } } }
"#,
    )
    .unwrap();
    let result = pklr::eval_to_json(&main).unwrap();
    let task = &result["p"]["tasks"][0];
    assert_eq!(task["loopVar"], "item");
    assert_eq!(task["block"][0]["name"], "c");
    assert_eq!(task["block"][0]["loopVar"], "item");
    assert_eq!(task["steps"][0]["id"], "s");
    assert_eq!(task["steps"][0]["sub"][0]["name"], "z");
    assert_eq!(task["steps"][0]["sub"][0]["loopVar"], "item");
}

#[test]
fn typed_listing_elements_through_a_transitive_import() {
    let temp = TestTempDir::new("pklr_typed_listing_transitive_import");
    std::fs::write(
        temp.path().join("types.pkl"),
        "class Item { n: String; d: Int = 1; kids: Listing<Item> }\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("lib.pkl"),
        "import \"types.pkl\"\nclass Holder { kids: Listing<types.Item>; opt: Listing<types.Item>? }\n",
    )
    .unwrap();
    let main = temp.path().join("main.pkl");
    std::fs::write(
        &main,
        r#"
import "lib.pkl" as L
h = new L.Holder { kids { new { n = "k"; kids { new { n = "kk" } } } }; opt { new { n = "o" } } }
"#,
    )
    .unwrap();
    let result = pklr::eval_to_json(&main).unwrap();
    assert_eq!(
        result["h"]["kids"],
        serde_json::json!([{"n": "k", "d": 1, "kids": [{"n": "kk", "d": 1, "kids": []}]}])
    );
    assert_eq!(result["h"]["opt"][0]["d"], 1);
}

// A mapping property's value type is a name written in the declaring module.
// Amending the mapping from another module must still resolve it, so entry
// bodies get the class's defaults and an empty `Listing` amend stays a list.
#[test]
fn empty_listing_amend_in_imported_class_mapping_entry_renders_list() {
    let temp = TestTempDir::new("pklr_test_empty_listing_amend_imported_mapping");
    let dir = temp.path();
    std::fs::write(
        dir.join("lib.pkl"),
        r#"
typealias Names = Listing<String>
typealias NodeAlias = Node
class Node {
    fast: Listing<String>
    bulk: Listing<String>
    maybe: Listing<String>?
    named: Names
    byKey: Mapping<String, Listing<String>>
}
open class Cluster {
    nodes: Mapping<String, Node>
    aliased: Mapping<String, NodeAlias>
    inner: Inner
}
class Inner { nodes: Mapping<String, Node> }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("wrap.pkl"),
        "import \"lib.pkl\"\nclass Wrapper { cluster: lib.Cluster }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "lib.pkl" as M
import "wrap.pkl" as W

class Local extends M.Cluster {}

cluster: M.Cluster = new {
    nodes {
        ["a"] {
            fast { "x" }
            bulk {}
            maybe {}
            named {}
            byKey { ["k"] {} }
        }
    }
    aliased { ["z"] { bulk {} } }
    inner { nodes { ["q"] { bulk {} } } }
}
wrapped: W.Wrapper = new { cluster { nodes { ["a"] { bulk {} } } } }
subclass: Local = new { nodes { ["a"] { bulk {} } } }
amended: M.Cluster = (cluster) { nodes { ["b"] { bulk {} } } }
"#,
    )
    .unwrap();

    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    let node = &val["cluster"]["nodes"]["a"];
    assert_eq!(node["fast"], serde_json::json!(["x"]));
    assert_eq!(node["bulk"], serde_json::json!([]));
    assert_eq!(node["maybe"], serde_json::json!([]));
    assert_eq!(node["named"], serde_json::json!([]));
    assert_eq!(node["byKey"], serde_json::json!({"k": []}));
    assert_eq!(
        val["cluster"]["aliased"]["z"]["bulk"],
        serde_json::json!([])
    );
    assert_eq!(
        val["cluster"]["inner"]["nodes"]["q"]["bulk"],
        serde_json::json!([])
    );
    // Properties the entry body does not mention take the class defaults.
    assert_eq!(
        val["cluster"]["inner"]["nodes"]["q"]["fast"],
        serde_json::json!([])
    );
    assert_eq!(
        val["wrapped"]["cluster"]["nodes"]["a"]["bulk"],
        serde_json::json!([])
    );
    assert_eq!(val["subclass"]["nodes"]["a"]["bulk"], serde_json::json!([]));
    assert_eq!(val["amended"]["nodes"]["a"]["bulk"], serde_json::json!([]));
    assert_eq!(val["amended"]["nodes"]["b"]["bulk"], serde_json::json!([]));
}

#[test]
fn empty_listing_amend_through_amends_of_imported_mapping_property() {
    let temp = TestTempDir::new("pklr_test_empty_listing_amend_amends_mapping");
    let dir = temp.path();
    std::fs::write(
        dir.join("base.pkl"),
        "open module base\nclass Node { bulk: Listing<String> }\nnodes: Mapping<String, Node>\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "amends \"base.pkl\"\nnodes { [\"a\"] { bulk {} } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["nodes"]["a"]["bulk"], serde_json::json!([]));
}

/// Writes the module `lib.pkl` (a class, an alias to it, a property) and the
/// module `main.pkl` with `main_body` below `import "lib.pkl" as lib`, then
/// evaluates `main.pkl`.
fn eval_with_class_module(main_body: &str) -> serde_json::Value {
    let temp = TestTempDir::new("pklr_test_imported_classes");
    let dir = temp.path();
    std::fs::write(
        dir.join("lib.pkl"),
        "class C { req: Int; opt: Int = 1 }\ntypealias A = C\nvalue = 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        format!("import \"lib.pkl\" as lib\n{main_body}\n"),
    )
    .unwrap();
    pklr::eval_to_json(&dir.join("main.pkl")).unwrap()
}

#[test]
fn imported_module_value_does_not_render_its_classes() {
    let val = eval_with_class_module("result = lib");
    assert_eq!(val, serde_json::json!({"result": {"value": 1}}));
}

#[test]
fn imported_module_nested_value_does_not_render_its_classes() {
    let val = eval_with_class_module(
        "a { b { m = lib } }\nl = new Listing { lib }\nm = new Mapping { [\"k\"] = lib }",
    );
    assert_eq!(
        val,
        serde_json::json!({
            "a": {"b": {"m": {"value": 1}}},
            "l": [{"value": 1}],
            "m": {"k": {"value": 1}},
        })
    );
}

#[test]
fn imported_module_converted_to_dynamic_or_map_does_not_carry_its_classes() {
    let val = eval_with_class_module(
        "d = lib.toDynamic()\nm = lib.toMap()\nk = lib.toMap().keys\nn = lib.toDynamic().toMap().keys",
    );
    assert_eq!(
        val,
        serde_json::json!({
            "d": {"value": 1},
            "m": {"value": 1},
            "k": ["value"],
            "n": ["value"],
        })
    );
}

#[test]
fn imported_module_as_output_value_does_not_render_its_classes() {
    let val = eval_with_class_module("output { value = lib }");
    assert_eq!(val, serde_json::json!({"value": 1}));
}

#[test]
fn imported_module_classes_stay_resolvable_through_dotted_access() {
    let val = eval_with_class_module(
        "c = new lib.C { req = 2 }\nd = new lib.A { req = 3; opt = 4 }\nf: lib.C = new lib.C { req = 5 }\nv = lib.value",
    );
    assert_eq!(
        val,
        serde_json::json!({
            "c": {"req": 2, "opt": 1},
            "d": {"req": 3, "opt": 4},
            "f": {"req": 5, "opt": 1},
            "v": 1,
        })
    );
}

#[test]
fn imported_module_rendered_by_the_stdlib_renderers_does_not_include_classes() {
    let val = eval_with_class_module(
        "j = new JsonRenderer { omitNullProperties = false }.renderValue(lib)\ny = new YamlRenderer {}.renderValue(lib)\np = new PcfRenderer {}.renderValue(lib)\nt = \"\\(lib)\"",
    );
    // `opt` is only reachable through class `C` (its default), so a leaked
    // class shows up as `opt` or as a `C`/`A` member, whatever the format.
    for key in ["j", "y", "p", "t"] {
        let text = val[key].as_str().unwrap();
        assert!(text.contains("value"), "{key}: {text}");
        assert!(!text.contains("opt"), "{key}: {text}");
        for class in ["C", "A"] {
            for leaked in [
                format!("\"{class}\""),
                format!("{class}:"),
                format!("{class} {{"),
                format!("{class} = "),
            ] {
                assert!(!text.contains(&leaked), "{key}: {text}");
            }
        }
    }
}

/// Evaluates `main.pkl` with `files` beside it, to JSON.
fn eval_files_to_json(files: &[(&str, &str)]) -> serde_json::Value {
    let temp = TestTempDir::new("pklr_test_class_member_files");
    for (name, source) in files {
        std::fs::write(temp.path().join(name), source).unwrap();
    }
    pklr::eval_to_json(&temp.path().join("main.pkl")).unwrap()
}

#[test]
fn class_held_by_a_property_is_a_value_not_a_member() {
    // Pkl renders `d.c` through the converter: only the classes a module
    // declares are left out, not every class-valued property.
    let val = eval_files_to_json(&[
        ("lib.pkl", "class C { req: Int; opt: Int = 1 }\n"),
        (
            "main.pkl",
            "import \"lib.pkl\" as lib\nd { c = lib.C }\noutput { renderer = new JsonRenderer { converters { [\"d.c\"] = (_) -> \"class\" } } }\n",
        ),
    ]);
    assert_eq!(val, serde_json::json!({"d": {"c": "class"}}));
}

#[test]
fn live_module_snapshots_do_not_render_declared_classes() {
    // `module`, `this` and their `toMap()`/`toDynamic()` views are built from the
    // module's live member map, which holds the class as a declared member just
    // like the exported object. Pkl never renders it.
    for value in ["module", "module.toMap()", "module.toDynamic()"] {
        let src = format!(
            "class C {{ a: Int = 1 }}\ntypealias A = C\nvalue = 1\noutput {{ value = new Dynamic {{ snap = {value} }} }}\n"
        );
        assert_eq!(
            eval_files_to_json(&[("main.pkl", src.as_str())]),
            serde_json::json!({"snap": {"value": 1}}),
            "{value}"
        );
    }
}

#[test]
fn live_module_snapshot_keeps_a_class_held_by_a_property() {
    // Only the declared member is skipped: `d` is an ordinary property that
    // happens to hold a class, so the snapshot still carries it.
    let val = eval_files_to_json(&[(
        "main.pkl",
        "class C { a: Int = 1 }\nd = C\noutput { value = new Dynamic { d = module.toMap().containsKey(\"d\"); c = module.toMap().containsKey(\"C\") } }\n",
    )]);
    assert_eq!(val, serde_json::json!({"d": true, "c": false}));
}

#[test]
fn imported_module_drops_a_property_named_like_an_inherited_typealias() {
    // As in Pkl, `A = 5` is not a rendered member of a module that inherits
    // `typealias A`, whether the module is evaluated or imported, while an
    // ordinary property still is.
    let files = [
        (
            "lib.pkl",
            "open module lib\ntypealias A = Int\nclass B {}\n",
        ),
        ("child.pkl", "extends \"lib.pkl\"\nA = 5\na = 1\n"),
        ("main.pkl", "import \"child.pkl\" as child\nr = child\n"),
    ];
    assert_eq!(
        eval_files_to_json(&files),
        serde_json::json!({"r": {"a": 1}})
    );
}

#[test]
fn imported_module_class_with_a_duration_default_still_renders_as_json() {
    // The class is never rendered, so its `Duration` default is not an
    // unrenderable value.
    let temp = TestTempDir::new("pklr_test_class_duration");
    let dir = temp.path();
    std::fs::write(
        dir.join("lib.pkl"),
        "class K { t: Duration = 5.min }\nx = 1\n",
    )
    .unwrap();
    std::fs::write(dir.join("main.pkl"), "import \"lib.pkl\" as lib\nr = lib\n").unwrap();
    let value = pklr::Evaluator::new()
        .eval_file(&dir.join("main.pkl"))
        .unwrap();
    assert_eq!(
        value.try_to_json().unwrap(),
        serde_json::json!({"r": {"x": 1}})
    );
}

#[test]
fn converted_collection_keeps_an_entry_holding_an_imported_class() {
    // A class held as a `Mapping`, `Map` or `Dynamic` entry is data, even when
    // its key is the class's name, so converting the container (repeatedly)
    // keeps it. Only the module's own declaration (`f`) is not a member.
    // Checked against pkl 0.32.2.
    let val = eval_files_to_json(&[
        ("lib.pkl", "class C { a: Int = 1 }\n"),
        (
            "main.pkl",
            "import \"lib.pkl\" as lib\n\
             local m = new Mapping { [\"C\"] = lib.C }\n\
             a = m.toMap().toMap().containsKey(\"C\")\n\
             b = m.toMap().mapValues((k, v) -> v).toMap().containsKey(\"C\")\n\
             c = m.toMap().filter((k, v) -> true).toMap().containsKey(\"C\")\n\
             d = new Dynamic { [\"C\"] = lib.C }.toMap().toMap().containsKey(\"C\")\n\
             e = Map(\"C\", lib.C).toMap().containsKey(\"C\")\n\
             f = lib.toMap().containsKey(\"C\")\n\
             g = m.toMap().toDynamic().toMap().keys.length\n\
             h = m.toMap().toMapping().toMap().keys.length\n",
        ),
    ]);
    assert_eq!(
        val,
        serde_json::json!({
            "a": true, "b": true, "c": true, "d": true, "e": true,
            "f": false, "g": 1, "h": 1,
        })
    );
}

#[test]
fn live_this_view_of_a_body_keeps_an_entry_holding_an_imported_class() {
    // Inside a `Dynamic`, `Mapping` or typed body, `this.toMap()`
    // sees the entry that holds a class (even under the class's own name),
    // matching the completed object. Only a module's own declaration is left
    // out of its `module`/`this` views. Checked against pkl 0.32.2.
    let val = eval_files_to_json(&[
        (
            "lib.pkl",
            "class C { a: Int = 1 }\n\
             function has() = module.toMap().containsKey(\"C\")\n\
             function hasThis() = this.toMap().containsKey(\"C\")\n",
        ),
        (
            "main.pkl",
            "import \"lib.pkl\" as lib\n\
             class T { c: Any = lib.C; f: Function }\n\
             local x_dyn = new Dynamic { [\"C\"] = lib.C; f = () -> this.toMap().containsKey(\"C\") }\n\
             local x_prop = new Dynamic { C = lib.C; f = () -> this.toMap().containsKey(\"C\") }\n\
             local x_outer = new Dynamic { [\"C\"] = lib.C; inner { f = () -> outer.toMap().containsKey(\"C\") } }\n\
             local x_map = new Mapping { [\"C\"] = lib.C; [\"f\"] = () -> this.toMap().containsKey(\"C\") }\n\
             local x_mapOuter = new Mapping { [\"C\"] = lib.C; [\"inner\"] { f = () -> outer.toMap().containsKey(\"C\") } }\n\
\
             local x_typed = new T { f = () -> this.toMap().containsKey(\"c\") }\n\
             r {\n\
               dyn = x_dyn.f.apply()\n\
               prop = x_prop.f.apply()\n\
               outerDyn = x_outer.inner.f.apply()\n\
               map = x_map[\"f\"].apply()\n\
               outerMap = x_mapOuter[\"inner\"].f.apply()\n\
\
               typed = x_typed.f.apply()\n\
             }\n\
             r2 { m = lib.has()\n t = lib.hasThis() }\n",
        ),
    ]);
    assert_eq!(
        val,
        serde_json::json!({
            "r": {
                "dyn": true, "prop": true, "outerDyn": false, "map": true,
                "outerMap": false, "typed": true,
            },
            "r2": {"m": false, "t": false},
        })
    );
}

#[test]
fn inherited_listing_property_elements_use_the_declaring_modules_class() {
    let temp = TestTempDir::new("pklr_inherited_listing_elem_class");
    let dir = temp.path();
    std::fs::write(
        dir.join("base.pkl"),
        "class T { x: Int = 1 }\nopen class Base { nested: Listing<T>? }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sub.pkl"),
        "import \"base.pkl\" as B\nclass T { y: Int = 2 }\nclass Sub extends B.Base {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"sub.pkl\" as S\nv = new S.Sub { nested { new {} } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["v"]["nested"], serde_json::json!([{"x": 1}]));
}

// A listing alias written in the declaring module keeps its meaning when the
// subclass's module binds the same name to a class. Checked against pkl 0.32.2.
#[test]
fn inherited_listing_alias_shadowed_by_a_class_in_the_subclass_module() {
    let temp = TestTempDir::new("pklr_inherited_listing_alias_shadowed");
    let dir = temp.path();
    std::fs::write(
        dir.join("a.pkl"),
        "class T { x: Int = 1 }\ntypealias L = Listing<T>\nopen class Base { nested: L? }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sub.pkl"),
        "import \"a.pkl\" as A\nclass T { y: Int = 2 }\nclass L {}\nclass Sub extends A.Base {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"sub.pkl\" as S\nv = new S.Sub { nested { new {} } }\nw = (v) { nested { new {} } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["v"]["nested"], serde_json::json!([{"x": 1}]));
    assert_eq!(val["w"]["nested"], serde_json::json!([{"x": 1}, {"x": 1}]));
}

// Amending an instance, or an instance of a subclass, resolves a new
// element's class where the property's type is declared, not where the
// instance was built. Checked against pkl 0.32.2.
#[test]
fn amended_instances_resolve_inherited_listing_elements_in_the_declaring_module() {
    let temp = TestTempDir::new("pklr_amended_instance_listing_elem_class");
    let dir = temp.path();
    std::fs::write(
        dir.join("a.pkl"),
        "class T { x: Int = 1 }\nopen class Base { nested: Listing<T>?; label: String = \"base\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sub.pkl"),
        "import \"a.pkl\" as A\nclass T { y: Int = 2 }\nclass Sub extends A.Base {}\n\
plain = new A.Base { label = \"plain\" }\n\
inst = new Sub { label = \"inst\" }\n\
grown = new A.Base { nested { new {} } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"sub.pkl\" as S\n\
a = (S.plain) { nested { new {} } }\n\
b = (S.inst) { nested { new {} } }\n\
c = (S.grown) { nested { new {} } }\n\
d = (S.inst) { label = \"again\"; nested { new {} } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["a"]["nested"], serde_json::json!([{"x": 1}]));
    assert_eq!(val["b"]["nested"], serde_json::json!([{"x": 1}]));
    assert_eq!(val["c"]["nested"], serde_json::json!([{"x": 1}, {"x": 1}]));
    assert_eq!(val["d"]["nested"], serde_json::json!([{"x": 1}]));
}

// An explicit `null` assigned to a `Listing<T>?` property is not a listing, so
// pkl rejects amending it with `new {}` whatever module declares `T`; pklr
// must not instead build an element of another module's `T`.
#[test]
fn assigned_null_listing_property_cannot_be_amended_with_elements() {
    let temp = TestTempDir::new("pklr_assigned_null_listing");
    let dir = temp.path();
    std::fs::write(
        dir.join("a.pkl"),
        "class T { x: Int = 1 }\nclass P { nested: Listing<T>? }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.pkl"),
        "import \"a.pkl\" as A\nclass T { y: Int = 2 }\np = new A.P { nested = null }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"b.pkl\" as B\nr = (B.p) { nested { new {} } }\n",
    )
    .unwrap();
    assert!(pklr::eval_to_json(&dir.join("main.pkl")).is_err());
}

// A listing alias keeps its constraints when a new element is added to an
// inherited property: the amended value is checked against the full alias in
// its declaring module, whether or not the amending module shadows a name.
// Checked against pkl 0.32.2.
#[test]
fn amended_listing_alias_keeps_its_constraints() {
    let temp = TestTempDir::new("pklr_amended_listing_alias_constraint");
    let dir = temp.path();
    let decls =
        "class T { x: Int = 1 }\ntypealias One = Listing<T>(length == 1)\nclass H { xs: One }\n";
    std::fs::write(
        dir.join("ok.pkl"),
        format!("{decls}h = new H {{ xs {{ new {{}} }} }}\n"),
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("ok.pkl")).unwrap();
    assert_eq!(val["h"]["xs"], serde_json::json!([{"x": 1}]));
    std::fs::write(
        dir.join("bad.pkl"),
        format!("{decls}h = new H {{ xs {{ new {{}}; new {{}} }} }}\n"),
    )
    .unwrap();
    let err = pklr::eval_to_json(&dir.join("bad.pkl")).unwrap_err();
    assert!(err.to_string().contains("length == 1"), "{err}");

    // The amending module binds the alias's name to a class.
    std::fs::write(
        dir.join("a.pkl"),
        "class T { x: Int = 1 }\ntypealias One = Listing<T>(length == 1)\nopen class H { xs: One }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.pkl"),
        "import \"a.pkl\" as A\nclass One { z: Int = 0 }\nh = new A.H { xs { new {} } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("shadowed.pkl"),
        "import \"b.pkl\" as B\nr = (B.h) { xs { new {} } }\n",
    )
    .unwrap();
    let err = pklr::eval_to_json(&dir.join("shadowed.pkl")).unwrap_err();
    assert!(err.to_string().contains("length == 1"), "{err}");
}

// A cyclic type alias is rejected, a `local` one included, instead of
// recursing until the stack overflows. Checked against pkl 0.32.2.
#[test]
fn cyclic_local_listing_alias_is_rejected() {
    let temp = TestTempDir::new("pklr_cyclic_local_listing_alias");
    let dir = temp.path();
    std::fs::write(
        dir.join("main.pkl"),
        "local typealias L = Listing<L>\nclass C { x: L? }\nc = new C { x { new {} } }\n",
    )
    .unwrap();
    let err = pklr::eval_to_json(&dir.join("main.pkl")).unwrap_err();
    assert!(err.to_string().contains("must not be cyclic"), "{err}");
}

// A long chain of aliases to a listing resolves without deep recursion.
#[test]
fn long_listing_alias_chain_resolves_elements() {
    let temp = TestTempDir::new("pklr_long_listing_alias_chain");
    let dir = temp.path();
    let mut source = String::from("class T { x: Int = 1 }\ntypealias A0 = Listing<T>\n");
    for index in 1..=5000 {
        source.push_str(&format!("typealias A{index} = A{}\n", index - 1));
    }
    source.push_str("class H { xs: A5000 }\nh = new H { xs { new {} } }\n");
    std::fs::write(dir.join("main.pkl"), source).unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["h"]["xs"], serde_json::json!([{"x": 1}]));
}

// A parenthesised `new { ... }` has no parent to amend, whatever type is
// expected where it stands, so it is an error in a typed listing's element,
// `if` branch and `let` body, and in a typed property. Without parentheses the
// same elements are built as the element class. Checked against pkl 0.32.2.
#[test]
fn parenthesised_untyped_new_cannot_tell_which_parent_to_amend() {
    let temp = TestTempDir::new("pklr_parenthesised_untyped_new");
    let dir = temp.path();
    let decl = "class T { x: Int = 1 }\n";
    for (index, source) in [
        "xs: Listing<T> = new { (new {}) }\n",
        "xs: Listing<T> = new { new {}; (new { x = 2 }) }\n",
        "xs: Listing<T> = new { if (true) (new {}) else new {} }\n",
        "xs: Listing<T> = new { let (a = 1) (new {}) }\n",
        "a: T = (new {})\n",
        "x = (new { a = 1 })\n",
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.join(format!("paren{index}.pkl"));
        std::fs::write(&path, format!("{decl}{source}")).unwrap();
        let err = pklr::eval_to_json(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot tell which parent to amend"),
            "{source}: {err}"
        );
    }
    std::fs::write(
        dir.join("plain.pkl"),
        format!(
            "{decl}xs: Listing<T> = new {{ let (a = 1) new {{}} }}\n\
ys: Listing<T> = new {{ if (true) new {{}} else new {{}} }}\n"
        ),
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("plain.pkl")).unwrap();
    assert_eq!(val["xs"], serde_json::json!([{"x": 1}]));
    assert_eq!(val["ys"], serde_json::json!([{"x": 1}]));
}

// Module-level aliases that name themselves are rejected whichever way they
// are used as a listing type. Checked against pkl 0.32.2.
#[test]
fn self_referential_module_aliases_are_rejected() {
    let temp = TestTempDir::new("pklr_self_referential_alias");
    let dir = temp.path();
    for (index, source) in [
        "typealias A = Listing<A>\nclass T { x: Int = 1 }\nxs: A = new {}\n",
        "typealias A = A\nxs: A = new {}\n",
        "local typealias A = A\nxs: A = new {}\n",
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.join(format!("cycle{index}.pkl"));
        std::fs::write(&path, source).unwrap();
        let err = pklr::eval_to_json(&path).unwrap_err();
        assert!(
            err.to_string().contains("must not be cyclic"),
            "{source}: {err}"
        );
    }
}

// A constraint on an inherited listing property reads the amended instance's
// final members (overrides, later amendments, hidden members), while its types
// and aliases come from the declaring scope. Checked against pkl 0.32.2.
#[test]
fn listing_constraints_read_the_amended_instance_members() {
    let temp = TestTempDir::new("pklr_listing_constraint_members");
    let dir = temp.path();
    let ok = [
        "class T {}\nclass H { limit: Int = 0; xs: Listing<T>(length <= limit) }\n\
h = new H { limit = 1; xs { new {} } }\n",
        "class T {}\nopen class H { limit: Int = 0; xs: Listing<T>(length <= limit) }\n\
class S extends H { limit = 1 }\nh = new S { xs { new {} } }\n",
        "class T {}\nclass H { limit: Int = 0; xs: Listing<T>(length <= limit) }\n\
base = new H { limit = 1; xs { new {} } }\nh = (base) { limit = 2; xs { new {} } }\n",
        "class T {}\ntypealias Lim = Listing<T>(this.length <= 1)\nclass H { limit: Int = 0; xs: Lim }\n\
h = new H { limit = 1; xs { new {} } }\n",
        "class T {}\nclass H { hidden limit: Int = 0; xs: Listing<T>(length <= limit) }\n\
h = new H { limit = 1; xs { new {} } }\n",
    ];
    for (index, source) in ok.into_iter().enumerate() {
        let path = dir.join(format!("ok{index}.pkl"));
        std::fs::write(&path, source).unwrap();
        let val = pklr::eval_to_json(&path).unwrap_or_else(|err| panic!("{source}: {err}"));
        // The amended-twice case appends a second element.
        let expected = if source.contains("limit = 2") {
            serde_json::json!([{}, {}])
        } else {
            serde_json::json!([{}])
        };
        assert_eq!(val["h"]["xs"], expected, "{source}");
    }
    let bad = [
        "class T {}\nclass H { limit: Int = 0; xs: Listing<T>(length <= limit) }\n\
h = new H { limit = 1; xs { new {}; new {} } }\n",
        "class T {}\ntypealias Lim = Listing<T>(length <= 2)\nclass H { limit: Int = 0; xs: Lim }\n\
h = new H { limit = 1; xs { new {}; new {}; new {} } }\n",
    ];
    for (index, source) in bad.into_iter().enumerate() {
        let path = dir.join(format!("bad{index}.pkl"));
        std::fs::write(&path, source).unwrap();
        assert!(pklr::eval_to_json(&path).is_err(), "{source}");
    }
}

// A cyclic `local` alias is an error only where it is used as a type. A
// property or variable of the same name is not a use. Checked against pkl
// 0.32.2.
#[test]
fn cyclic_local_alias_is_an_error_only_when_used_as_a_type() {
    let temp = TestTempDir::new("pklr_cyclic_local_alias_use");
    let dir = temp.path();
    let alias = "local typealias L = List<L>\n";
    let unused = [
        "class C { L: Int = 1 }\nx = 1\n",
        "local typealias M = L\nx = 1\n",
        "typealias P = Int(this is L)\nx = 1\n",
        "x = new Dynamic { L = 2 }\n",
    ];
    for (index, tail) in unused.into_iter().enumerate() {
        let path = dir.join(format!("unused{index}.pkl"));
        std::fs::write(&path, format!("{alias}{tail}")).unwrap();
        let val = pklr::eval_to_json(&path).unwrap_or_else(|err| panic!("{tail}: {err}"));
        assert!(val.get("x").is_some(), "{tail}");
    }
    let used = [
        "x = 1 is L\n",
        "x = 1 as L\n",
        "function f(a: L) = 1\nx = 1\n",
        "class C { y: Listing<L> }\nx = 1\n",
        "local typealias M = L\ny: M = List()\n",
        "x = new Listing<L> {}\n",
    ];
    for (index, tail) in used.into_iter().enumerate() {
        let path = dir.join(format!("used{index}.pkl"));
        std::fs::write(&path, format!("{alias}{tail}")).unwrap();
        let err = pklr::eval_to_json(&path).unwrap_err();
        assert!(
            err.to_string().contains("must not be cyclic"),
            "{tail}: {err}"
        );
    }
}

// A cycle between two `local` aliases is an error when one is used as a type,
// and not when only a parameter, generator variable or property shares their
// names. Checked against pkl 0.32.2.
#[test]
fn cyclic_local_aliases_follow_uses_through_each_other() {
    let temp = TestTempDir::new("pklr_cyclic_local_alias_through_alias");
    let dir = temp.path();
    let aliases = "local typealias A = B\nlocal typealias B = A\n";
    std::fs::write(dir.join("used.pkl"), format!("{aliases}x: A = 1\n")).unwrap();
    let err = pklr::eval_to_json(&dir.join("used.pkl")).unwrap_err();
    assert!(err.to_string().contains("must not be cyclic"), "{err}");
    std::fs::write(
        dir.join("unused.pkl"),
        format!(
            "{aliases}function f(A: Int, B: Int) = A + B\ny = f(1, 2)\n\
z = new Listing {{ for (A in List(1)) {{ A }} }}\n"
        ),
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("unused.pkl")).unwrap();
    assert_eq!(val["y"], 3);
    assert_eq!(val["z"], serde_json::json!([1]));
}

// A listing whose element type is a constrained alias, a nullable class or a
// nested listing builds `new { ... }` elements as the class. Checked against
// pkl 0.32.2.
#[test]
fn listing_elements_resolve_constrained_and_nullable_element_types() {
    let temp = TestTempDir::new("pklr_listing_elem_structural");
    let dir = temp.path();
    std::fs::write(
        dir.join("main.pkl"),
        "class T { x: Int = 1 }\ntypealias E = T(x > 0)\ntypealias LT = Listing<T?>\n\
class H { xs: Listing<E>; ys: Listing<Listing<T?>> }\n\
h = new H { xs { new {} }; ys { new { new {} } } }\n\
i: Listing<E> = new { new {} }\n\
a = new Listing<Listing<T?>> { new { new {} } }\n\
b: Listing<LT> = new { new { new {} } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["h"]["xs"], serde_json::json!([{"x": 1}]));
    assert_eq!(val["h"]["ys"], serde_json::json!([[{"x": 1}]]));
    assert_eq!(val["i"], serde_json::json!([{"x": 1}]));
    assert_eq!(val["a"], serde_json::json!([[{"x": 1}]]));
    assert_eq!(val["b"], serde_json::json!([[{"x": 1}]]));
}

// Amending an inherited listing of built-in elements resolves its alias where
// it was declared, even when the amending module binds the alias's name to a
// class. Checked against pkl 0.32.2.
#[test]
fn amended_builtin_listing_alias_resolves_in_the_declaring_module() {
    let temp = TestTempDir::new("pklr_amended_builtin_listing_alias");
    let dir = temp.path();
    std::fs::write(
        dir.join("a.pkl"),
        "typealias Two = Listing<String>(length <= 2)\nopen class H { xs: Two? }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ok.pkl"),
        "import \"a.pkl\" as A\nclass Two { z: Int = 0 }\nh = new A.H { xs { \"a\"; \"b\" } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("ok.pkl")).unwrap();
    assert_eq!(val["h"]["xs"], serde_json::json!(["a", "b"]));
    std::fs::write(
        dir.join("bad.pkl"),
        "import \"a.pkl\" as A\nclass Two { z: Int = 0 }\nh = new A.H { xs { \"a\"; \"b\"; \"c\" } }\n",
    )
    .unwrap();
    assert!(pklr::eval_to_json(&dir.join("bad.pkl")).is_err());
}

// A cyclic `local` alias named only in another alias's constraint is a use
// when that alias is used. Checked against pkl 0.32.2.
#[test]
fn cyclic_local_alias_named_in_a_constraint_is_rejected_when_used() {
    let temp = TestTempDir::new("pklr_cyclic_local_alias_constraint");
    let dir = temp.path();
    let aliases = "local typealias L = List<L>\ntypealias P = Int(this is L)\n";
    std::fs::write(dir.join("used.pkl"), format!("{aliases}x: P = 1\n")).unwrap();
    let err = pklr::eval_to_json(&dir.join("used.pkl")).unwrap_err();
    assert!(err.to_string().contains("must not be cyclic"), "{err}");
    std::fs::write(dir.join("unused.pkl"), format!("{aliases}x = 1\n")).unwrap();
    assert_eq!(pklr::eval_to_json(&dir.join("unused.pkl")).unwrap()["x"], 1);
}

// An empty body amending an inherited `Alias?` listing property gives an empty
// listing even when the amending module cannot see the alias, and elements of
// a listing of a constrained listing alias are built as the element class.
// Checked against pkl 0.32.2.
#[test]
fn nullable_listing_alias_amends_and_constrained_listing_alias_elements() {
    let temp = TestTempDir::new("pklr_nullable_listing_alias_amend");
    let dir = temp.path();
    std::fs::write(
        dir.join("b1.pkl"),
        "class T { x: Int = 1 }\nopen class H { xs: Alias? }\ntypealias Alias = Listing<T>\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        "import \"b1.pkl\" as B\nh = new B.H { xs {} }\ni = new B.H { xs { new {} } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["h"]["xs"], serde_json::json!([]));
    assert_eq!(val["i"]["xs"], serde_json::json!([{"x": 1}]));
    std::fs::write(
        dir.join("shadow.pkl"),
        "import \"b1.pkl\" as B\nclass Alias {}\nh = new B.H { xs {} }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("shadow.pkl")).unwrap();
    assert_eq!(val["h"]["xs"], serde_json::json!([]));
    std::fs::write(
        dir.join("cl.pkl"),
        "class T { x: Int = 1 }\ntypealias L = Listing<T>\ntypealias CL = L(length > 0)\n\
a = new Listing<CL> { new { new {} } }\nb: Listing<CL> = new { new { new {} } }\n\
class H { xs: Listing<CL> }\nh = new H { xs { new { new {} } } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("cl.pkl")).unwrap();
    for value in [&val["a"], &val["b"], &val["h"]["xs"]] {
        assert_eq!(*value, serde_json::json!([[{"x": 1}]]));
    }
}

// A constrained alias of a bare `Listing`, or of another constrained alias,
// is a listing: empty amends of a nullable one give an empty listing and the
// inner `new { ... }` elements of a `Listing` of it are listings. Checked
// against pkl 0.32.2.
#[test]
fn constrained_bare_listing_aliases_resolve_to_listings() {
    let temp = TestTempDir::new("pklr_constrained_bare_listing_alias");
    let dir = temp.path();
    std::fs::write(
        dir.join("main.pkl"),
        "class T { x: Int = 1 }\ntypealias Two = Listing(length <= 2)\n\
typealias TwoT = Listing<T>(length <= 2)\ntypealias Nest = TwoT(length >= 0)\n\
typealias NestB = Two(length >= 0)\n\
class H { a: Two?; b: TwoT?; c: Nest?; d: NestB? }\n\
h = new H { a {}; b { new {} }; c { new {} }; d {} }\n\
x: Listing<Two> = new { new {} }\ny: Listing<TwoT> = new { new { new {} } }\n\
z: Listing<Nest> = new { new { new {} } }\nw: Listing<NestB> = new { new {} }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(
        val["h"],
        serde_json::json!({"a": [], "b": [{"x": 1}], "c": [{"x": 1}], "d": []})
    );
    assert_eq!(val["x"], serde_json::json!([[]]));
    assert_eq!(val["y"], serde_json::json!([[{"x": 1}]]));
    assert_eq!(val["z"], serde_json::json!([[{"x": 1}]]));
    assert_eq!(val["w"], serde_json::json!([[]]));
}

// The element class of an amended `Listing<T>?` property belongs to that
// amendment only: a same-named property of a `Dynamic` nested in one of its
// elements stays untyped. Checked against pkl 0.32.2.
#[test]
fn listing_element_class_binding_is_not_reused_by_nested_same_named_properties() {
    let temp = TestTempDir::new("pklr_listing_binding_nested");
    let dir = temp.path();
    std::fs::write(
        dir.join("main.pkl"),
        "class T { x: Int = 1; inner: Dynamic = new Dynamic {} }\n\
typealias Al = Listing<String>(length >= 0)\nclass H { xs: Listing<T>?; ys: Al? }\n\
h = new H { xs { new { inner { xs { new {} } } } } }\n\
g = new H { ys { \"a\" }; xs { new T { inner { ys { new {} } } } } }\n",
    )
    .unwrap();
    let val = pklr::eval_to_json(&dir.join("main.pkl")).unwrap();
    assert_eq!(val["h"]["xs"][0]["inner"]["xs"], serde_json::json!([{}]));
    assert_eq!(val["h"]["xs"][0]["x"], 1);
    assert_eq!(val["g"]["xs"][0]["inner"]["ys"], serde_json::json!([{}]));
}

// ============================================================
// modulepath: imports
// ============================================================

fn eval_with_module_path(
    entry: &std::path::Path,
    dirs: &[&std::path::Path],
) -> Result<serde_json::Value, String> {
    pklr::EvaluatorBuilder::new()
        .module_path(dirs.iter().copied())
        .eval_to_json(entry)
        .map_err(|e| e.to_string())
}

#[test]
fn modulepath_import_found_in_first_matching_dir() {
    let temp = TestTempDir::new("pklr_test_modulepath_order");
    let (d1, d2) = (temp.path().join("d1"), temp.path().join("d2"));
    std::fs::create_dir_all(d1.join("lib")).unwrap();
    std::fs::create_dir_all(d2.join("lib")).unwrap();
    std::fs::write(d1.join("lib/a.pkl"), "v = \"d1\"\n").unwrap();
    std::fs::write(d2.join("lib/a.pkl"), "v = \"d2\"\n").unwrap();
    std::fs::write(d2.join("lib/b.pkl"), "v = \"d2b\"\n").unwrap();
    let main = temp.path().join("main.pkl");
    std::fs::write(
        &main,
        "import \"modulepath:/lib/a.pkl\" as A\nimport \"modulepath:/lib/b.pkl\" as B\na = A.v\nb = B.v\n",
    )
    .unwrap();
    let json = eval_with_module_path(&main, &[&d1, &d2]).unwrap();
    assert_eq!(json, serde_json::json!({"a": "d1", "b": "d2b"}));
    let json = eval_with_module_path(&main, &[&d2, &d1]).unwrap();
    assert_eq!(json["a"], "d2");
}

#[test]
fn modulepath_import_not_found() {
    let temp = TestTempDir::new("pklr_test_modulepath_missing");
    let main = temp.path().join("main.pkl");
    std::fs::write(&main, "x = import(\"modulepath:/nope.pkl\")\n").unwrap();
    let err = eval_with_module_path(&main, &[temp.path()]).unwrap_err();
    assert!(err.contains("modulepath:/nope.pkl"), "{err}");
    // Without a module path nothing resolves, and a file of the same name
    // next to the importer is not a fallback.
    std::fs::write(temp.path().join("nope.pkl"), "y = 1\n").unwrap();
    let err = eval_with_module_path(&main, &[]).unwrap_err();
    assert!(err.contains("modulepath:/nope.pkl"), "{err}");
}

#[test]
fn modulepath_module_relative_and_nested_imports() {
    let temp = TestTempDir::new("pklr_test_modulepath_nested");
    let mp = temp.path().join("mp");
    std::fs::create_dir_all(mp.join("lib/sub")).unwrap();
    std::fs::write(
        mp.join("lib/a.pkl"),
        "import \"sub/s.pkl\" as S\nimport \"modulepath:/lib/sub/s.pkl\" as S2\nv = \"a\"\nrel = S.x\nabs = S2.x\n",
    )
    .unwrap();
    std::fs::write(mp.join("lib/sub/s.pkl"), "x = \"sub\"\n").unwrap();
    let main = temp.path().join("main.pkl");
    std::fs::write(
        &main,
        "import \"modulepath:/lib/a.pkl\" as A\nr = A.rel\nb = A.abs\n",
    )
    .unwrap();
    let json = eval_with_module_path(&main, &[&mp]).unwrap();
    assert_eq!(json, serde_json::json!({"r": "sub", "b": "sub"}));

    let amending = temp.path().join("amending.pkl");
    std::fs::write(
        &amending,
        "amends \"modulepath:/lib/a.pkl\"\nv = \"over\"\n",
    )
    .unwrap();
    let json = eval_with_module_path(&amending, &[&mp]).unwrap();
    assert_eq!(json["v"], "over");
    assert_eq!(json["rel"], "sub");
}

#[test]
fn modulepath_uri_requires_slash_and_cannot_escape() {
    let temp = TestTempDir::new("pklr_test_modulepath_bad");
    let mp = temp.path().join("mp");
    std::fs::create_dir_all(&mp).unwrap();
    std::fs::write(temp.path().join("secret.pkl"), "s = 1\n").unwrap();
    let main = temp.path().join("main.pkl");
    std::fs::write(&main, "x = import(\"modulepath:lib/a.pkl\")\n").unwrap();
    assert!(eval_with_module_path(&main, &[&mp]).is_err());
    std::fs::write(&main, "x = import(\"modulepath:/../secret.pkl\")\n").unwrap();
    assert!(eval_with_module_path(&main, &[&mp]).is_err());
}

#[test]
fn modulepath_decodes_and_confines_uris() {
    let temp = TestTempDir::new("pklr_test_modulepath_decode");
    let (d1, d2) = (temp.path().join("d1"), temp.path().join("d2"));
    std::fs::create_dir_all(d1.join("lib/a.pkl")).unwrap();
    std::fs::create_dir_all(d1.join("other")).unwrap();
    std::fs::write(d1.join("file"), "").unwrap();
    std::fs::create_dir_all(&d2).unwrap();
    std::fs::create_dir_all(d2.join("lib")).unwrap();
    std::fs::write(d2.join("lib/a.pkl"), "v = \"d2\"\n").unwrap();
    std::fs::write(d2.join("a b.pkl"), "v = \"space\"\n").unwrap();
    std::fs::write(temp.path().join("secret.pkl"), "s = 1\n").unwrap();
    let main = temp.path().join("main.pkl");
    let eval = |uri: &str| {
        std::fs::write(&main, format!("x = import(\"{uri}\").v\n")).unwrap();
        eval_with_module_path(&main, &[&d1, &d2])
    };
    // A directory in an earlier entry does not hide a file in a later one.
    assert_eq!(eval("modulepath:/lib/a.pkl").unwrap()["x"], "d2");
    assert_eq!(eval("modulepath:/unused/../lib/a.pkl").unwrap()["x"], "d2");
    assert_eq!(eval("modulepath:/a%20b.pkl").unwrap()["x"], "space");
    // A plain file where an earlier entry would need a directory is a miss.
    std::fs::write(d2.join("file.pkl"), "v = \"d2file\"\n").unwrap();
    std::fs::create_dir_all(d2.join("file")).unwrap();
    std::fs::write(d2.join("file/x.pkl"), "v = \"deep\"\n").unwrap();
    assert_eq!(eval("modulepath:/file/x.pkl").unwrap()["x"], "deep");
    assert!(eval("modulepath:/../secret.pkl").is_err());
    assert!(eval("modulepath:/..\\\\secret.pkl").is_err());
    assert!(eval("modulepath:/%2e%2e/secret.pkl").is_err());
}

// `modulepath:` is not a valid directory name on Windows.
#[cfg(unix)]
#[test]
fn modulepath_does_not_fall_back_to_cwd_file() {
    let temp = TestTempDir::new("pklr_test_modulepath_cwd");
    let main = temp.path().join("main.pkl");
    std::fs::write(
        &main,
        "x = import(\"modulepath:/lib/zz_cwd_probe.pkl\").v\n",
    )
    .unwrap();
    let cwd_file = std::path::Path::new("modulepath:/lib/zz_cwd_probe.pkl");
    std::fs::create_dir_all(cwd_file.parent().unwrap()).unwrap();
    std::fs::write(cwd_file, "v = 1\n").unwrap();
    let result = eval_with_module_path(&main, &[temp.path()]);
    let _ = std::fs::remove_dir_all("modulepath:");
    assert!(result.is_err());
}

#[test]
fn modulepath_unused_missing_import_is_lazy() {
    let temp = TestTempDir::new("pklr_test_modulepath_lazy");
    let main = temp.path().join("main.pkl");
    std::fs::write(
        &main,
        "import \"modulepath:/missing.pkl\" as Unused\nimport \"modulepath:/other.pkl\"\nanswer = 42\n",
    )
    .unwrap();
    let json = eval_with_module_path(&main, &[temp.path()]).unwrap();
    assert_eq!(json, serde_json::json!({"answer": 42}));
    std::fs::write(
        &main,
        "import \"modulepath:/missing.pkl\" as Used\nanswer = Used.v\n",
    )
    .unwrap();
    let err = eval_with_module_path(&main, &[temp.path()]).unwrap_err();
    assert!(err.contains("modulepath:/missing.pkl"), "{err}");
}

#[test]
fn inherited_declaration_naming_a_non_type_is_rejected() {
    // `pkl eval` rejects the amending module with this message too.
    let temp = TestTempDir::new("pklr_test_inherited_non_type");
    std::fs::write(temp.path().join("p.pkl"), "foo = 1\nres: foo\n").unwrap();
    std::fs::write(temp.path().join("main.pkl"), "amends \"p.pkl\"\nx = 1\n").unwrap();
    let err = pklr::eval_to_json(&temp.path().join("main.pkl")).unwrap_err();
    assert!(
        err.to_string()
            .contains("Expected `foo` to be a type, but it is not."),
        "{err}"
    );
}

#[test]
fn inherited_declaration_resolves_types_in_its_own_module() {
    // A child's `local C = 1` does not change what the base's `C` names.
    let temp = TestTempDir::new("pklr_test_inherited_type_scope");
    std::fs::write(
        temp.path().join("p.pkl"),
        "local class C {}\nhidden item: C?\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("main.pkl"),
        "amends \"p.pkl\"\nlocal C = 1\n",
    )
    .unwrap();
    let v = pklr::eval_to_json(&temp.path().join("main.pkl")).unwrap();
    assert_eq!(v, serde_json::json!({}));
}

#[test]
fn inherited_annotation_is_not_checked_in_the_child_scope() {
    // The child redeclares `item` without a type, inheriting `C?`, while its
    // own `local C` shadows the class. `pkl eval` accepts it.
    let temp = TestTempDir::new("pklr_test_inherited_annotation_scope");
    std::fs::write(
        temp.path().join("p.pkl"),
        "local class C {}\nitem: C? = null\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("main.pkl"),
        "amends \"p.pkl\"\nlocal C = 1\nitem = if (false) new {} else null\n",
    )
    .unwrap();
    let v = pklr::eval_to_json(&temp.path().join("main.pkl")).unwrap();
    assert_eq!(v, serde_json::json!({}));
}
