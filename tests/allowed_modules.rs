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

/// A root module, the pointer to read from its result, and the value there.
const MODULEPATH_ROOTS: [(&str, &str, &str); 4] = [
    (
        "import",
        "import \"modulepath:/lib/a.pkl\"\ny = a.x\n",
        "/y",
    ),
    ("amends", "amends \"modulepath:/lib/a.pkl\"\n", "/x"),
    ("extends", "extends \"modulepath:/lib/a.pkl\"\n", "/x"),
    (
        "import_expr",
        "y = import(\"modulepath:/lib/a.pkl\").x\n",
        "/y",
    ),
];

/// Pkl matches the patterns against the `modulepath:` URI, not the file it
/// resolves to: `^file:` alone refuses the module, `^modulepath:` admits it.
#[test]
fn modulepath_modules_are_matched_by_their_uri() {
    for (kind, root, pointer) in MODULEPATH_ROOTS {
        let (_temp, dir) = modulepath_project(&format!("pklr_modules_mp_{kind}"), root);
        let refused = "Refusing to load module `modulepath:/lib/a.pkl` because it does not match any entry in the module allowlist";
        for patterns in [&["^file:"][..], &["^repl:", "^file:"], &["^pkl:", "^file:"]] {
            let err = eval_mp(&dir, patterns).unwrap_err();
            assert!(err.contains(refused), "{kind} {patterns:?}: {err}");
        }
        for patterns in [
            &["^file:", "^modulepath:"][..],
            &["^file:", "^modulepath:/lib/a\\.pkl$"],
        ] {
            let json = eval_mp(&dir, patterns).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(json.pointer(pointer), Some(&serde_json::json!(1)), "{kind}");
        }
    }
}

/// An unused `modulepath:` import does not admit a `file:` import of the same
/// file, and a denied `modulepath:` URI is refused even if nothing is there.
#[test]
fn modulepath_admission_is_per_import() {
    let root =
        "import \"modulepath:/lib/a.pkl\" as unused\nimport \"mp/lib/a.pkl\" as used\ny = used.x\n";
    let (_temp, dir) = modulepath_project("pklr_modules_mp_unused", root);
    let only_root_and_uri = [
        pat(&dir.join("root.pkl")),
        "^modulepath:/lib/a\\.pkl$".to_string(),
    ];
    let err = EvaluatorBuilder::new()
        .module_path([dir.join("mp")])
        .allowed_modules(only_root_and_uri)
        .eval_to_json(&dir.join("root.pkl"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Refusing to load module `file://"), "{err}");

    // Both imports used: the `modulepath:` admission does not cover the file.
    let both = "import \"modulepath:/lib/a.pkl\" as viaModulePath\nimport \"mp/lib/a.pkl\" as viaFile\ny = viaModulePath.x + viaFile.x\n";
    let (_temp, dir) = modulepath_project("pklr_modules_mp_both", both);
    let err = EvaluatorBuilder::new()
        .module_path([dir.join("mp")])
        .allowed_modules([pat(&dir.join("root.pkl")), "^modulepath:".to_string()])
        .eval_to_json(&dir.join("root.pkl"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Refusing to load module `file://"), "{err}");

    // The same through a glob that only its directory is allowed for.
    let glob = "import \"modulepath:/lib/a.pkl\" as viaModulePath\nimport* \"mp/lib/*.pkl\" as viaGlob\ny = viaModulePath.x\nz = viaGlob\n";
    let (_temp, dir) = modulepath_project("pklr_modules_mp_glob", glob);
    let err = EvaluatorBuilder::new()
        .module_path([dir.join("mp")])
        .allowed_modules([
            pat(&dir.join("root.pkl")),
            "^modulepath:".to_string(),
            pat(&dir.join("mp/lib")) + "/$",
        ])
        .eval_to_json(&dir.join("root.pkl"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Refusing to load module `file://"), "{err}");

    let missing = "import \"modulepath:/lib/none.pkl\"\ny = none.x\n";
    let (_temp, dir) = modulepath_project("pklr_modules_mp_missing", missing);
    let err = eval_mp(&dir, &["^file:"]).unwrap_err();
    assert!(
        err.contains("Refusing to load module `modulepath:/lib/none.pkl`"),
        "{err}"
    );
}

/// Reusing an evaluator does not carry one run's admissions into the next.
#[test]
fn modulepath_admission_does_not_outlive_the_run() {
    let (_temp, dir) = modulepath_project("pklr_modules_mp_reuse", MODULEPATH_ROOTS[0].1);
    let mut evaluator = EvaluatorBuilder::new()
        .module_path([dir.join("mp")])
        .allowed_modules([
            "^file:///.*/root\\.pkl$".to_string(),
            "^modulepath:".to_string(),
        ])
        .build();
    let value = evaluator.eval_file(&dir.join("root.pkl")).unwrap();
    assert_eq!(value.to_json()["y"], 1);
    let err = evaluator.eval_file(&dir.join("mp/lib/a.pkl")).unwrap_err();
    assert!(
        err.to_string().contains("Refusing to load module `file://"),
        "{err}"
    );
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

/// A module path whose modules import each other by relative path.
fn relative_project(name: &str, root: &str) -> (TestTempDir, std::path::PathBuf) {
    let temp = TestTempDir::new(name);
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    let files = [
        (
            "mp/x/a.pkl",
            "import \"b.pkl\"\nimport \"../y/c.pkl\"\nimport \"/y/c.pkl\" as abs\nr = b.v + c.v + abs.v\n",
        ),
        ("mp/x/b.pkl", "open module b\nv = 1\n"),
        ("mp/y/c.pkl", "v = 2\n"),
        (
            "mp/x/sub/d.pkl",
            "import \"../b.pkl\"\nimport \"../../y/c.pkl\"\nr = b.v + c.v\n",
        ),
        (
            "mp/x/esc.pkl",
            "import \"../../../outside.pkl\"\nr = outside.v\n",
        ),
        (
            "mp/x/trip.pkl",
            "import \".../outside.pkl\"\nr = outside.v\n",
        ),
        ("mp/x/ext.pkl", "extends \"b.pkl\"\nw = 5\n"),
        ("mp/x/amd.pkl", "amends \"b.pkl\"\n"),
        ("mp/x/expr.pkl", "r = import(\"b.pkl\").v\n"),
        (
            "mp/x/glob.pkl",
            "import* \"*.pkl\" as all\nr = all.keys.toList()\n",
        ),
        ("mp/x/query.pkl", "import \"b.pkl?x=1\"\nr = b.v\n"),
        ("mp/outside.pkl", "v = 7\n"),
        ("mp/x/outside.pkl", "v = 8\n"),
        ("mp/secret.pkl", "v = 99\n"),
        ("mp/x/bare.pkl", "open module bare\nv = 11\n"),
        ("mp/x/sub/bare.pkl", "amends \"...\"\n"),
        (
            "mp/x/sub/up.pkl",
            "import \".../../secret.pkl\"\nr = secret.v\n",
        ),
        ("root.pkl", root),
    ];
    for (file, source) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    (temp, dir)
}

/// Evaluate a root that imports `modulepath:/<module>`, allowing the root file
/// and the given patterns.
fn eval_relative(name: &str, module: &str, patterns: &[&str]) -> Result<serde_json::Value, String> {
    let root = format!("import \"modulepath:/{module}\" as m\nr = m\n");
    let (_temp, dir) = relative_project(name, &root);
    let mut allowed = vec![pat(&dir.join("root.pkl"))];
    allowed.extend(patterns.iter().map(|p| p.to_string()));
    EvaluatorBuilder::new()
        .module_path([dir.join("mp")])
        .allowed_modules(allowed)
        .eval_to_json(&dir.join("root.pkl"))
        .map_err(|e| e.to_string())
}

/// Pkl keeps a relative import made from a module path module a `modulepath:`
/// URI, so the allowlist matches that URI and never a `file:` one.
#[test]
fn relative_imports_from_module_path_modules_are_matched_as_modulepath_uris() {
    let refused = |uri: &str| format!("Refusing to load module `{uri}` because it does not match");
    let a = "^modulepath:/x/a\\.pkl$";
    let b = "^modulepath:/x/b\\.pkl$";
    let c = "^modulepath:/y/c\\.pkl$";
    type Expected<'a> = Result<(&'a str, serde_json::Value), &'a str>;
    let cases: [(&str, &[&str], Expected); 17] = [
        ("x/a.pkl", &[a, b, c], Ok(("/r/r", 5.into()))),
        (
            "x/a.pkl",
            &["^modulepath:/x/", "^modulepath:/y/"],
            Ok(("/r/r", 5.into())),
        ),
        ("x/a.pkl", &[a, c], Err("modulepath:/x/b.pkl")),
        ("x/a.pkl", &[a, b], Err("modulepath:/y/c.pkl")),
        // `..` and a leading `/` are matched in their normalized spelling.
        (
            "x/sub/d.pkl",
            &["^modulepath:/x/sub/d\\.pkl$", b, c],
            Ok(("/r/r", 3.into())),
        ),
        (
            "x/sub/d.pkl",
            &[
                "^modulepath:/x/sub/d\\.pkl$",
                "^modulepath:/x/sub/\\.\\./b\\.pkl$",
                c,
            ],
            Err("modulepath:/x/b.pkl"),
        ),
        // A `..` out of the module path root is matched as written and names nothing.
        (
            "x/esc.pkl",
            &["^modulepath:/x/esc\\.pkl$", "^modulepath:/outside\\.pkl$"],
            Err("modulepath:/../../outside.pkl"),
        ),
        (
            "x/esc.pkl",
            &["^modulepath:/x/esc\\.pkl$", "^modulepath:"],
            Err("Import not found: modulepath:/../../outside.pkl"),
        ),
        // A triple-dot import names the ancestor directory that holds the file.
        (
            "x/trip.pkl",
            &["^modulepath:/x/trip\\.pkl$", "^modulepath:/outside\\.pkl$"],
            Ok(("/r/r", 7.into())),
        ),
        (
            "x/trip.pkl",
            &["^modulepath:/x/trip\\.pkl$"],
            Err("modulepath:/outside.pkl"),
        ),
        // The search starts above the importer's directory, so `/x/outside.pkl`
        // is not a candidate; a bare `...` searches for the importer's own name.
        (
            "x/trip.pkl",
            &[
                "^modulepath:/x/trip\\.pkl$",
                "^modulepath:/x/outside\\.pkl$",
            ],
            Err("modulepath:/outside.pkl"),
        ),
        (
            "x/sub/bare.pkl",
            &[
                "^modulepath:/x/sub/bare\\.pkl$",
                "^modulepath:/x/bare\\.pkl$",
            ],
            Ok(("/r/v", 11.into())),
        ),
        // A triple-dot path cannot climb into a prefix the policy does not allow.
        (
            "x/sub/up.pkl",
            &[
                "^modulepath:/x/sub/up\\.pkl$",
                "^modulepath:/x/",
                "^modulepath:/secret\\.pkl$",
            ],
            Err("modulepath:/../../secret.pkl"),
        ),
        (
            "x/ext.pkl",
            &["^modulepath:/x/ext\\.pkl$", b],
            Ok(("/r/w", 5.into())),
        ),
        (
            "x/ext.pkl",
            &["^modulepath:/x/ext\\.pkl$"],
            Err("modulepath:/x/b.pkl"),
        ),
        (
            "x/amd.pkl",
            &["^modulepath:/x/amd\\.pkl$", b],
            Ok(("/r/v", 1.into())),
        ),
        (
            "x/expr.pkl",
            &["^modulepath:/x/expr\\.pkl$"],
            Err("modulepath:/x/b.pkl"),
        ),
    ];
    for (n, (module, patterns, expected)) in cases.into_iter().enumerate() {
        let result = eval_relative(&format!("pklr_modules_rel_{n}"), module, patterns);
        match expected {
            Ok((pointer, value)) => {
                let json = result.unwrap_or_else(|e| panic!("{module} {patterns:?}: {e}"));
                assert_eq!(
                    json.pointer(pointer),
                    Some(&value),
                    "{module} {patterns:?}: {json}"
                );
            }
            Err(message) => {
                let err = result.unwrap_err();
                let wanted = if message.starts_with("Import") {
                    message.to_string()
                } else {
                    refused(message)
                };
                assert!(err.contains(&wanted), "{module} {patterns:?}: {err}");
            }
        }
    }
}

/// A glob import cannot expand a `modulepath:` URI, which Pkl refuses even
/// when the allowlist admits it.
#[test]
fn glob_imports_from_module_path_modules_are_not_globbable() {
    let err = eval_relative("pklr_modules_rel_glob", "x/glob.pkl", &["^modulepath:"]).unwrap_err();
    assert!(
        err.contains("Cannot expand glob pattern `modulepath:/x/*.pkl` because scheme `modulepath` is not globbable"),
        "{err}"
    );
}

/// A query on a relative import names the same file; the allowlist sees it.
#[test]
fn relative_import_with_a_query_finds_the_module_path_file() {
    let query = "^modulepath:/x/query\\.pkl$";
    let ok = eval_relative(
        "rel-query",
        "x/query.pkl",
        &[query, "^modulepath:/x/b\\.pkl"],
    );
    assert_eq!(ok.unwrap()["r"]["r"], 1);
    let err = eval_relative("rel-query-denied", "x/query.pkl", &[query]).unwrap_err();
    assert!(err.contains("modulepath:/x/b.pkl?x=1"), "{err}");
}
