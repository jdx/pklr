//! `read("modulepath:/...")`: resources served from the module path, like
//! `pkl eval --module-path`. Expected values were confirmed with the pkl CLI.

use pklr::EvaluatorBuilder;
use serde_json::json;

struct TestTempDir(std::path::PathBuf);

impl TestTempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(std::fs::canonicalize(&dir).unwrap())
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `mp1/Ruxel.pkl`, `mp1/sub/a.txt` and `mp2/sub.txt` beside `root.pkl`.
fn project(name: &str, root: &str) -> TestTempDir {
    let temp = TestTempDir::new(name);
    let dir = &temp.0;
    std::fs::create_dir_all(dir.join("mp1/sub")).unwrap();
    std::fs::create_dir_all(dir.join("mp2")).unwrap();
    std::fs::write(dir.join("mp1/Ruxel.pkl"), "value = 42\n").unwrap();
    std::fs::write(dir.join("mp1/sub/a.txt"), "hi\n").unwrap();
    std::fs::write(dir.join("mp2/sub.txt"), "z\n").unwrap();
    std::fs::write(dir.join("root.pkl"), root).unwrap();
    temp
}

fn eval(
    temp: &TestTempDir,
    module_path: bool,
    allowed: Option<&[&str]>,
) -> Result<serde_json::Value, String> {
    let mut builder = EvaluatorBuilder::new();
    if module_path {
        builder = builder.module_path([temp.0.join("mp1"), temp.0.join("mp2")]);
    }
    if let Some(allowed) = allowed {
        builder = builder.allowed_resources(allowed.iter().map(|s| s.to_string()));
    }
    builder
        .eval_to_json(&temp.0.join("root.pkl"))
        .map_err(|e| e.to_string())
}

#[test]
fn reads_a_module_path_resource() {
    let temp = project("mp_res_read", "a = read(\"modulepath:/Ruxel.pkl\")\n");
    assert_eq!(
        eval(&temp, true, None).unwrap(),
        json!({"a": {
            "uri": "modulepath:/Ruxel.pkl",
            "text": "value = 42\n",
            "base64": "dmFsdWUgPSA0Mgo=",
        }})
    );
}

#[test]
fn searches_every_module_path_directory_in_order() {
    let temp = project(
        "mp_res_order",
        "a = read(\"modulepath:/sub.txt\").text\nb = read(\"modulepath:/sub/a.txt\").text\n",
    );
    assert_eq!(
        eval(&temp, true, None).unwrap(),
        json!({"a": "z\n", "b": "hi\n"})
    );
}

#[test]
fn the_reported_uri_is_normalized() {
    let temp = project(
        "mp_res_norm",
        "a = read(\"modulepath:/sub//a.txt\").uri\nb = read(\"modulepath:/sub/../Ruxel.pkl\").uri\n",
    );
    assert_eq!(
        eval(&temp, true, None).unwrap(),
        json!({"a": "modulepath:/sub/a.txt", "b": "modulepath:/Ruxel.pkl"})
    );
}

#[test]
fn without_a_module_path_the_resource_is_missing() {
    let temp = project("mp_res_none", "a = read(\"modulepath:/Ruxel.pkl\")\n");
    let error = eval(&temp, false, None).unwrap_err();
    assert!(
        error.contains("Cannot find resource `modulepath:/Ruxel.pkl`."),
        "{error}"
    );
}

#[test]
fn a_missing_file_is_not_found() {
    let temp = project("mp_res_miss", "a = read(\"modulepath:/nope.pkl\")\n");
    let error = eval(&temp, true, None).unwrap_err();
    assert!(
        error.contains("Cannot find resource `modulepath:/nope.pkl`."),
        "{error}"
    );
}

#[test]
fn read_or_null_is_null_when_missing() {
    let temp = project(
        "mp_res_null",
        "a = read?(\"modulepath:/nope.pkl\")\nb = read?(\"modulepath:/Ruxel.pkl\")?.text\n",
    );
    assert_eq!(
        eval(&temp, true, None).unwrap(),
        json!({"b": "value = 42\n"})
    );
    assert_eq!(eval(&temp, false, None).unwrap(), json!({}));
}

#[test]
fn a_path_cannot_climb_out_of_the_module_path() {
    let temp = project(
        "mp_res_climb",
        "a = read?(\"modulepath:/../root.pkl\")\nb = read?(\"modulepath:/sub/../../root.pkl\")\n",
    );
    assert_eq!(eval(&temp, true, None).unwrap(), json!({}));
}

#[test]
fn a_uri_without_a_slash_is_invalid() {
    let temp = project("mp_res_slash", "a = read(\"modulepath:Ruxel.pkl\")\n");
    let error = eval(&temp, true, None).unwrap_err();
    assert!(error.contains("missing a `/`"), "{error}");
}

#[test]
fn globs_are_not_supported() {
    let temp = project("mp_res_glob", "a = read*(\"modulepath:/sub/*.txt\")\n");
    let error = eval(&temp, true, None).unwrap_err();
    assert!(
        error.contains("scheme `modulepath` is not globbable"),
        "{error}"
    );
}

#[test]
fn the_allowlist_matches_the_modulepath_uri() {
    let root = "a = read(\"modulepath:/sub/a.txt\").text\n";
    let temp = project("mp_res_allow", root);
    assert_eq!(
        eval(&temp, true, Some(&["modulepath:", "prop:"])).unwrap(),
        json!({"a": "hi\n"})
    );
    // The file the URI resolves to is not what is matched.
    let error = eval(&temp, true, Some(&["file:", "prop:"])).unwrap_err();
    assert!(
        error.contains("Refusing to read resource `modulepath:/sub/a.txt`"),
        "{error}"
    );
}

#[test]
fn the_allowlist_sees_dot_segments_removed() {
    // pkl: `--allowed-resources modulepath:/sub/` refuses this read, reporting
    // the normalized URI `modulepath:/Ruxel.pkl`.
    let temp = project(
        "mp_res_dots",
        "a = read(\"modulepath:/sub/../Ruxel.pkl\")\n",
    );
    let error = eval(&temp, true, Some(&["modulepath:/sub/", "prop:"])).unwrap_err();
    assert!(
        error.contains("Refusing to read resource `modulepath:/Ruxel.pkl`"),
        "{error}"
    );
}

#[test]
fn the_reported_uri_reads_the_same_file() {
    let temp = project(
        "mp_res_encoding",
        "a = read(\"modulepath:/a%20b.txt\")\nb = read(\"modulepath:/c%25d.txt\")\nc = read(a.uri).text\nd = read(b.uri).text\n",
    );
    std::fs::write(temp.0.join("mp1/a b.txt"), "spaced\n").unwrap();
    std::fs::write(temp.0.join("mp1/c%d.txt"), "percent\n").unwrap();
    let value = eval(&temp, true, None).unwrap();
    assert_eq!(value["a"]["uri"], "modulepath:/a%20b.txt");
    assert_eq!(value["b"]["uri"], "modulepath:/c%25d.txt");
    assert_eq!(value["c"], "spaced\n");
    assert_eq!(value["d"], "percent\n");
}
