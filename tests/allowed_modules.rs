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

/// The `file:` URI of `path`, as pklr spells it.
fn uri(path: &std::path::Path) -> String {
    let path = path
        .display()
        .to_string()
        .replace('\\', "/")
        .replace(' ', "%20");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

/// A pattern matching exactly the URI of `path` (patterns are regular expressions).
fn pat(path: &std::path::Path) -> String {
    fancy_regex::escape(&uri(path)).into_owned()
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
    assert_eq!(eval(&a, &[pat(&dir) + "/"]).unwrap()["v"], 1);
    // A pattern is a regular expression anchored at the start of the URI.
    assert_eq!(eval(&a, &["file:.*\\.pkl".into()]).unwrap()["v"], 1);

    let only_entry = vec![pat(&a)];
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
    assert_eq!(eval(&a, &[pat(&a)]).unwrap()["v"], true);
}

#[test]
fn reads_are_not_gated_by_the_module_policy() {
    let (_temp, dir) = project("pklr_modules_read");
    let a = dir.join("a.pkl");
    std::fs::write(&a, "v = read(\"sub/b.pkl\").text\n").unwrap();
    assert_eq!(eval(&a, &[pat(&a)]).unwrap()["v"], "x = 1\n");
}

#[test]
fn amends_and_extends_are_checked() {
    let (_temp, dir) = project("pklr_modules_amends");
    std::fs::write(dir.join("sub/open.pkl"), "open module base\nx = 1\n").unwrap();
    for keyword in ["amends", "extends"] {
        let a = dir.join(format!("{keyword}.pkl"));
        std::fs::write(&a, format!("{keyword} \"sub/open.pkl\"\n")).unwrap();
        assert_eq!(eval(&a, &["file:".into()]).unwrap()["x"], 1);
        let error = eval(&a, &[pat(&a)]).unwrap_err();
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
    assert!(eval(&a, &[pat(&a)]).is_err());

    let g = dir.join("g.pkl");
    std::fs::write(&g, "import* \"sub/*.pkl\" as m\nv = m.keys.toList()\n").unwrap();
    let json = eval(&g, &[pat(&g), pat(&dir) + "/sub/"]).unwrap();
    assert_eq!(json["v"], serde_json::json!(["sub/b.pkl", "sub/c.pkl"]));
    // Allowing the matches alone is not enough: the searched directory is checked.
    let error = eval(
        &g,
        &[pat(&g), pat(&dir) + "/sub/b.pkl", pat(&dir) + "/sub/c.pkl"],
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
    let error = eval(&a, &[pat(&a), pat(&dir.join("sub/c.pkl"))]).unwrap_err();
    assert!(
        error.contains(&format!("`{}/sub/b.pkl`", uri(&dir))),
        "{error}"
    );
    let all = vec![
        pat(&a),
        pat(&dir.join("sub/c.pkl")),
        pat(&dir.join("sub/b.pkl")),
    ];
    assert_eq!(eval(&a, &all).unwrap()["v"], 2);
}

#[test]
fn relative_paths_are_normalized_before_matching() {
    let (_temp, dir) = project("pklr_modules_relative");
    std::fs::create_dir_all(dir.join("allowed")).unwrap();
    let a = dir.join("allowed/a.pkl");
    std::fs::write(&a, "import \"../sub/b.pkl\"\nv = b.x\n").unwrap();
    let error = eval(&a, &[pat(&dir) + "/allowed/"]).unwrap_err();
    assert!(error.contains("Refusing to load module"), "{error}");
    assert_eq!(eval(&a, &[pat(&dir) + "/"]).unwrap()["v"], 1);
}

#[test]
fn invalid_pattern_is_an_error() {
    let (_temp, dir) = project("pklr_modules_invalid");
    let a = dir.join("sub/b.pkl");
    let error = eval(&a, &["[".into()]).unwrap_err();
    assert!(error.contains("invalid allowed-modules pattern"), "{error}");
}

#[test]
fn denied_modules_are_refused_before_any_io() {
    let (_temp, dir) = project("pklr_modules_before_io");
    let a = dir.join("a.pkl");

    // A denied module that does not exist is refused, not reported missing.
    std::fs::write(&a, "import \"sub/missing.pkl\" as m\nv = m.x\n").unwrap();
    let error = eval(&a, &[pat(&a)]).unwrap_err();
    assert!(error.contains("Refusing to load module"), "{error}");

    // A denied HTTP import is refused without a request (nothing listens there).
    std::fs::write(&a, "import \"http://127.0.0.1:1/m.pkl\" as m\nv = m.x\n").unwrap();
    let error = eval(&a, &["file:".into()]).unwrap_err();
    assert!(
        error.contains("Refusing to load module `http://127.0.0.1:1/m.pkl`"),
        "{error}"
    );

    // As in pkl, an import that is never used is never loaded.
    std::fs::write(&a, "import \"sub/b.pkl\" as b\nv = 1\n").unwrap();
    assert_eq!(eval(&a, &[pat(&a)]).unwrap()["v"], 1);
}

#[cfg(unix)]
#[test]
fn a_denied_symlink_is_not_served_from_the_cache() {
    let (_temp, dir) = project("pklr_modules_symlink");
    std::os::unix::fs::symlink(dir.join("sub/b.pkl"), dir.join("link.pkl")).unwrap();
    let a = dir.join("a.pkl");
    std::fs::write(
        &a,
        "import \"sub/b.pkl\" as b\nimport \"link.pkl\" as l\nv = b.x + l.x\n",
    )
    .unwrap();
    let error = eval(&a, &[pat(&a), pat(&dir.join("sub/b.pkl"))]).unwrap_err();
    assert!(error.contains("link.pkl"), "{error}");
}

#[test]
fn a_denied_entry_is_refused_before_it_is_read() {
    let (_temp, dir) = project("pklr_modules_entry");
    // The entry does not exist: a read would report that instead.
    let error = eval(&dir.join("missing.pkl"), &["https:".into()]).unwrap_err();
    assert!(error.contains("Refusing to load module"), "{error}");
}

/// A project whose `lib/a.pkl` is found through the module path `dir/mp`.
fn modulepath_project(name: &str, root: &str) -> (TestTempDir, std::path::PathBuf) {
    let temp = TestTempDir::new(name);
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::create_dir_all(dir.join("mp/lib")).unwrap();
    std::fs::write(dir.join("mp/lib/a.pkl"), "open module a\nx = 1\n").unwrap();
    std::fs::write(dir.join("root.pkl"), root).unwrap();
    (temp, dir)
}

fn eval_mp(dir: &std::path::Path, patterns: &[&str]) -> Result<serde_json::Value, String> {
    EvaluatorBuilder::new()
        .module_path([dir.join("mp")])
        .allowed_modules(patterns.iter().map(|p| p.to_string()))
        .eval_to_json(&dir.join("root.pkl"))
        .map_err(|e| e.to_string())
}

const MODULEPATH_ROOTS: [(&str, &str); 4] = [
    ("import", "import \"modulepath:/lib/a.pkl\"\ny = a.x\n"),
    ("amends", "amends \"modulepath:/lib/a.pkl\"\n"),
    ("extends", "extends \"modulepath:/lib/a.pkl\"\n"),
    ("import_expr", "y = import(\"modulepath:/lib/a.pkl\").x\n"),
];

/// Pkl matches the patterns against the `modulepath:` URI, not the file it
/// resolves to: `^file:` alone refuses the module, `^modulepath:` admits it.
#[test]
fn modulepath_modules_are_matched_by_their_uri() {
    for (kind, root) in MODULEPATH_ROOTS {
        let (_temp, dir) = modulepath_project(&format!("pklr_modules_mp_{kind}"), root);
        let refused = "Refusing to load module `modulepath:/lib/a.pkl` because it does not match any entry in the module allowlist";
        for patterns in [&["^file:"][..], &["^repl:", "^file:"], &["^pkl:", "^file:"]] {
            let err = eval_mp(&dir, patterns).unwrap_err();
            assert!(err.contains(refused), "{kind} {patterns:?}: {err}");
        }
        let ok = eval_mp(&dir, &["^file:", "^modulepath:"]);
        // The URI is allowed; any failure left is not the policy.
        if let Err(err) = ok {
            assert!(!err.contains("Refusing"), "{kind}: {err}");
        }
        let ok = eval_mp(&dir, &["^file:", "^modulepath:/lib/a\\.pkl$"]);
        if let Err(err) = ok {
            assert!(!err.contains("Refusing"), "{kind}: {err}");
        }
    }
}

/// A pattern naming only `modulepath:` does not admit the importing file.
#[test]
fn modulepath_only_policy_refuses_the_file_root() {
    let (_temp, dir) = modulepath_project("pklr_modules_mp_root", MODULEPATH_ROOTS[0].1);
    let err = eval_mp(&dir, &["^modulepath:"]).unwrap_err();
    assert!(
        err.contains("Refusing to load module `file://") && err.contains("root.pkl"),
        "{err}"
    );
}
