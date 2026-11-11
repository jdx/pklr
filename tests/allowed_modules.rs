//! The module policy: `EvaluatorBuilder::allowed_modules`, like
//! `pkl --allowed-modules`.

use pklr::EvaluatorBuilder;

struct TestTempDir(std::path::PathBuf);

impl TestTempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn uri(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

fn project(name: &str) -> (TestTempDir, std::path::PathBuf) {
    let temp = TestTempDir::new(name);
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/b.pkl"), "x = 1\n").unwrap();
    std::fs::write(dir.join("sub/c.pkl"), "import \"b.pkl\"\ny = b.x + 1\n").unwrap();
    (temp, dir)
}

fn eval(path: &std::path::Path, patterns: &[String]) -> Result<serde_json::Value, String> {
    EvaluatorBuilder::new()
        .allowed_modules(patterns.iter().cloned())
        .eval_to_json(path)
        .map_err(|e| e.to_string())
}

#[test]
fn default_allows_every_module() {
    let (_temp, dir) = project("pklr_modules_default");
    std::fs::write(dir.join("a.pkl"), "import \"sub/b.pkl\"\nv = b.x\n").unwrap();
    let json = EvaluatorBuilder::new()
        .eval_to_json(&dir.join("a.pkl"))
        .unwrap();
    assert_eq!(json["v"], 1);
}

#[test]
fn allowed_and_denied_imports() {
    let (_temp, dir) = project("pklr_modules_import");
    std::fs::write(dir.join("a.pkl"), "import \"sub/b.pkl\"\nv = b.x\n").unwrap();
    let a = dir.join("a.pkl");

    assert_eq!(eval(&a, &["file:".into()]).unwrap()["v"], 1);
    assert_eq!(eval(&a, &[uri(&dir) + "/"]).unwrap()["v"], 1);
    // A pattern is a regular expression anchored at the start of the URI.
    assert_eq!(eval(&a, &["file:.*\\.pkl".into()]).unwrap()["v"], 1);

    let only_entry = vec![uri(&a)];
    let error = eval(&a, &only_entry).unwrap_err();
    assert!(
        error.contains(&format!(
            "Refusing to load module `{}/sub/b.pkl` because it does not match any entry in the module allowlist",
            uri(&dir)
        )),
        "{error}"
    );

    // The entry module is checked too.
    let error = eval(&a, &["https:".into()]).unwrap_err();
    assert!(
        error.contains(&format!("Refusing to load module `{}`", uri(&a))),
        "{error}"
    );
    assert!(eval(&a, &[]).is_err());

    // Matching is case-sensitive and does not float to the middle of a URI.
    assert!(eval(&a, &["FILE:".into()]).is_err());
    assert!(eval(&a, &["tmp".into()]).is_err());
}

#[test]
fn stdlib_modules_are_always_allowed() {
    let (_temp, dir) = project("pklr_modules_stdlib");
    let a = dir.join("a.pkl");
    std::fs::write(&a, "import \"pkl:math\"\nv = math.pi > 3\n").unwrap();
    assert_eq!(eval(&a, &[uri(&a)]).unwrap()["v"], true);
}

#[test]
fn reads_are_not_gated_by_the_module_policy() {
    let (_temp, dir) = project("pklr_modules_read");
    let a = dir.join("a.pkl");
    std::fs::write(&a, "v = read(\"sub/b.pkl\").text\n").unwrap();
    assert_eq!(eval(&a, &[uri(&a)]).unwrap()["v"], "x = 1\n");
}

#[test]
fn amends_and_extends_are_checked() {
    let (_temp, dir) = project("pklr_modules_amends");
    std::fs::write(dir.join("sub/open.pkl"), "open module base\nx = 1\n").unwrap();
    for keyword in ["amends", "extends"] {
        let a = dir.join(format!("{keyword}.pkl"));
        std::fs::write(&a, format!("{keyword} \"sub/open.pkl\"\n")).unwrap();
        assert_eq!(eval(&a, &["file:".into()]).unwrap()["x"], 1);
        let error = eval(&a, &[uri(&a)]).unwrap_err();
        assert!(
            error.contains("Refusing to load module") && error.contains("sub/open.pkl"),
            "{keyword}: {error}"
        );
    }
}

#[test]
fn import_expressions_and_globs_are_checked() {
    let (_temp, dir) = project("pklr_modules_expr");
    let a = dir.join("a.pkl");
    std::fs::write(&a, "v = import(\"sub/b.pkl\").x\n").unwrap();
    assert_eq!(eval(&a, &["file:".into()]).unwrap()["v"], 1);
    assert!(eval(&a, &[uri(&a)]).is_err());

    let g = dir.join("g.pkl");
    std::fs::write(&g, "import* \"sub/*.pkl\" as m\nv = m.keys.toList()\n").unwrap();
    let json = eval(&g, &[uri(&g), uri(&dir) + "/sub/"]).unwrap();
    assert_eq!(json["v"], serde_json::json!(["sub/b.pkl", "sub/c.pkl"]));
    // Allowing the matches alone is not enough: the searched directory is checked.
    let error = eval(
        &g,
        &[uri(&g), uri(&dir) + "/sub/b.pkl", uri(&dir) + "/sub/c.pkl"],
    )
    .unwrap_err();
    assert!(error.contains("Refusing to load module"), "{error}");
}

#[test]
fn transitive_imports_are_checked() {
    let (_temp, dir) = project("pklr_modules_transitive");
    let a = dir.join("a.pkl");
    std::fs::write(&a, "import \"sub/c.pkl\"\nv = c.y\n").unwrap();
    // c.pkl is allowed but the module it imports, sub/b.pkl, is not.
    let error = eval(&a, &[uri(&a), uri(&dir.join("sub/c.pkl"))]).unwrap_err();
    assert!(
        error.contains(&format!("`{}/sub/b.pkl`", uri(&dir))),
        "{error}"
    );
    let all = vec![
        uri(&a),
        uri(&dir.join("sub/c.pkl")),
        uri(&dir.join("sub/b.pkl")),
    ];
    assert_eq!(eval(&a, &all).unwrap()["v"], 2);
}

#[test]
fn relative_paths_are_normalized_before_matching() {
    let (_temp, dir) = project("pklr_modules_relative");
    std::fs::create_dir_all(dir.join("allowed")).unwrap();
    let a = dir.join("allowed/a.pkl");
    std::fs::write(&a, "import \"../sub/b.pkl\"\nv = b.x\n").unwrap();
    let error = eval(&a, &[uri(&dir) + "/allowed/"]).unwrap_err();
    assert!(error.contains("Refusing to load module"), "{error}");
    assert_eq!(eval(&a, &[uri(&dir) + "/"]).unwrap()["v"], 1);
}

#[test]
fn invalid_pattern_is_an_error() {
    let (_temp, dir) = project("pklr_modules_invalid");
    let a = dir.join("sub/b.pkl");
    let error = eval(&a, &["[".into()]).unwrap_err();
    assert!(error.contains("invalid allowed-modules pattern"), "{error}");
}
