//! `GlobAccess` and `expand_glob_with`: a host-provided file system view gets
//! Pkl's glob semantics from the library, for `read*` and `import*` alike.
//! The expected keys were confirmed with `pkl eval` (0.33) on the same tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pklr::{
    Error, EvalCapabilities, Evaluator, GlobAccess, NativeCapabilities, NativeGlobAccess,
    expand_glob, expand_glob_with,
};

/// An in-memory view of a directory tree. Listings come back in reverse
/// order, so the tests prove the library does the sorting.
#[derive(Default, Clone)]
struct Vfs {
    /// Absolute path to `is_dir`.
    nodes: BTreeMap<PathBuf, bool>,
    /// Paths that `list_dir` and `is_file` refuse.
    denied: Vec<PathBuf>,
    /// Paths asked about, to show the view is used.
    asked: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
}

impl Vfs {
    /// Mirror the real directory `root`, leaving out symbolic links.
    fn mirror(root: &Path) -> Self {
        let mut vfs = Self::default();
        vfs.nodes.insert(root.to_path_buf(), true);
        vfs.walk(root);
        vfs
    }

    fn walk(&mut self, dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_symlink() {
                continue;
            }
            self.nodes.insert(entry.path(), kind.is_dir());
            if kind.is_dir() {
                self.walk(&entry.path());
            }
        }
    }

    fn check(&self, path: &Path) -> pklr::Result<()> {
        self.asked.lock().unwrap().push(path.to_path_buf());
        if self.denied.iter().any(|denied| path.starts_with(denied)) {
            return Err(Error::Eval(format!("view denies {}", path.display())));
        }
        Ok(())
    }
}

impl GlobAccess for Vfs {
    fn is_file(&mut self, path: &Path) -> pklr::Result<bool> {
        self.check(path)?;
        Ok(self.nodes.get(path) == Some(&false))
    }

    fn list_dir(&mut self, path: &Path) -> pklr::Result<Vec<(String, bool)>> {
        self.check(path)?;
        Ok(self
            .nodes
            .iter()
            .rev()
            .filter(|(node, _)| node.parent() == Some(path))
            .map(|(node, is_dir)| {
                (
                    node.file_name().unwrap().to_string_lossy().into_owned(),
                    *is_dir,
                )
            })
            .collect())
    }
}

/// Native capabilities whose globs (module and resource) run over a view.
struct Hosted {
    native: NativeCapabilities,
    module_view: Vfs,
    /// A separate view for `read*`, when set.
    resource_view: Option<Vfs>,
}

impl EvalCapabilities for Hosted {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.native.read_to_string(path)
    }
    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        self.native.path_exists(path)
    }
    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        self.native.canonicalize(path)
    }
    fn read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        self.native.read_bytes(path)
    }
    fn read_env(&mut self, name: &str) -> pklr::Result<Option<String>> {
        self.native.read_env(name)
    }
    fn fetch_text(&mut self, url: &str) -> pklr::Result<String> {
        self.native.fetch_text(url)
    }
    fn fetch_bytes(&mut self, url: &str) -> pklr::Result<Vec<u8>> {
        self.native.fetch_bytes(url)
    }
    fn temp_dir(&mut self, prefix: &str) -> pklr::Result<PathBuf> {
        self.native.temp_dir(prefix)
    }
    // No `glob` override: the default uses the access hooks.
    fn glob_access(&mut self) -> Option<&mut dyn GlobAccess> {
        Some(&mut self.module_view)
    }
    fn resource_glob_access(&mut self) -> Option<&mut dyn GlobAccess> {
        match &mut self.resource_view {
            Some(view) => Some(view),
            None => Some(&mut self.module_view),
        }
    }
}

struct BoxCaps(Box<dyn EvalCapabilities>);

impl EvalCapabilities for BoxCaps {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.0.read_to_string(path)
    }
    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        self.0.path_exists(path)
    }
    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        self.0.canonicalize(path)
    }
    fn read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        self.0.read_bytes(path)
    }
    fn read_env(&mut self, name: &str) -> pklr::Result<Option<String>> {
        self.0.read_env(name)
    }
    fn fetch_text(&mut self, url: &str) -> pklr::Result<String> {
        self.0.fetch_text(url)
    }
    fn fetch_bytes(&mut self, url: &str) -> pklr::Result<Vec<u8>> {
        self.0.fetch_bytes(url)
    }
    fn temp_dir(&mut self, prefix: &str) -> pklr::Result<PathBuf> {
        self.0.temp_dir(prefix)
    }
    fn glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        self.0.glob(base, pattern)
    }
    fn resource_glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        self.0.resource_glob(base, pattern)
    }
}

struct Project(PathBuf);

impl Project {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        std::fs::create_dir_all(tree.join("sub/deep")).unwrap();
        std::fs::create_dir_all(tree.join("z")).unwrap();
        for (file, body) in [
            ("a.pkl", "x = 1\n"),
            ("b.txt", "b\n"),
            ("B.txt", "B\n"),
            (".hidden.pkl", "x = 7\n"),
            ("sub/c.pkl", "x = 3\n"),
            ("sub/d.txt", "d\n"),
            ("sub/deep/e.pkl", "x = 5\n"),
            ("z/f.pkl", "x = 6\n"),
        ] {
            std::fs::write(tree.join(file), body).unwrap();
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("a.pkl", tree.join("link.pkl")).unwrap();
            std::os::unix::fs::symlink("sub", tree.join("ln")).unwrap();
        }
        Self(std::fs::canonicalize(&dir).unwrap())
    }

    fn tree(&self) -> PathBuf {
        self.0.join("tree")
    }

    fn hosted(&self) -> Hosted {
        Hosted {
            native: NativeCapabilities::new(),
            module_view: Vfs::mirror(&self.tree()),
            resource_view: None,
        }
    }

    /// The keys of `import*` and `read*` of `tree/<pattern>`.
    fn keys(&self, pattern: &str, caps: impl EvalCapabilities + 'static) -> serde_json::Value {
        self.keys_of(pattern, caps, true)
    }

    fn keys_of(
        &self,
        pattern: &str,
        caps: impl EvalCapabilities + 'static,
        import: bool,
    ) -> serde_json::Value {
        let entry = self.0.join("entry.pkl");
        let import = if import {
            format!("i = import*(\"tree/{pattern}\").keys\n")
        } else {
            String::new()
        };
        std::fs::write(
            &entry,
            format!("{import}r = read*(\"tree/{pattern}\").keys\n"),
        )
        .unwrap();
        Evaluator::with_capabilities(caps)
            .eval_file(&entry)
            .unwrap()
            .to_json()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Pattern and the keys real pkl gives for both `import*` and `read*`.
const CASES: &[(&str, &[&str])] = &[
    ("*.pkl", &["tree/.hidden.pkl", "tree/a.pkl"]),
    (
        "**.pkl",
        &[
            "tree/.hidden.pkl",
            "tree/a.pkl",
            "tree/sub/c.pkl",
            "tree/sub/deep/e.pkl",
            "tree/z/f.pkl",
        ],
    ),
    ("sub/**.pkl", &["tree/sub/c.pkl", "tree/sub/deep/e.pkl"]),
    ("*/*.pkl", &["tree/sub/c.pkl", "tree/z/f.pkl"]),
    ("{a,.hidden}.pkl", &["tree/.hidden.pkl", "tree/a.pkl"]),
    ("nothing*", &[]),
    ("a.pkl", &["tree/a.pkl"]),
];

/// Patterns that match non-Pkl files, which `import*` would try to parse, so
/// only `read*` is compared.
const READ_ONLY_CASES: &[(&str, &[&str])] = &[
    ("{a,b}.*", &["tree/a.pkl", "tree/b.txt"]),
    ("**.txt", &["tree/B.txt", "tree/b.txt", "tree/sub/d.txt"]),
];

#[test]
fn virtual_view_matches_pkl_for_read_globs_matching_directories() {
    let project = Project::new("globaccess_dirs");
    for (pattern, expected) in READ_ONLY_CASES {
        let expected = serde_json::json!({ "r": expected });
        let views: [Box<dyn EvalCapabilities>; 2] = [
            Box::new(project.hosted()),
            Box::new(NativeCapabilities::new()),
        ];
        for caps in views {
            assert_eq!(
                project.keys_of(pattern, BoxCaps(caps), false),
                expected,
                "{pattern}"
            );
        }
    }
}

#[test]
fn virtual_view_matches_pkl_for_read_and_import_globs() {
    let project = Project::new("globaccess_keys");
    for (pattern, expected) in CASES {
        let expected = serde_json::json!({ "i": expected, "r": expected });
        assert_eq!(
            project.keys(pattern, project.hosted()),
            expected,
            "virtual view, {pattern}"
        );
        assert_eq!(
            project.keys(pattern, NativeCapabilities::new()),
            expected,
            "native, {pattern}"
        );
    }
}

#[test]
fn expand_glob_with_matches_expand_glob() {
    let project = Project::new("globaccess_expand");
    let tree = project.tree();
    let mut vfs = Vfs::mirror(&tree);
    // `**/` matching no directory is pklr's own rule, so these are only
    // compared with the native walker, not with `pkl`.
    let patterns = CASES.iter().map(|(pattern, _)| *pattern).chain([
        "**/*.pkl",
        "**/a.pkl",
        "sub/**/*.pkl",
        "sub/*",
        "**",
    ]);
    for pattern in patterns {
        let native = expand_glob(&tree, pattern).unwrap();
        assert_eq!(
            expand_glob_with(&tree, pattern, &mut vfs).unwrap(),
            native,
            "{pattern}"
        );
        assert_eq!(
            expand_glob_with(&tree, pattern, &mut NativeGlobAccess).unwrap(),
            native,
            "{pattern}"
        );
    }
}

#[test]
fn read_glob_returns_file_contents_through_the_view() {
    let project = Project::new("globaccess_read");
    let entry = project.0.join("entry.pkl");
    std::fs::write(&entry, "r = read*(\"tree/**.txt\")\n").unwrap();
    let json = Evaluator::with_capabilities(project.hosted())
        .eval_file(&entry)
        .unwrap()
        .to_json();
    assert_eq!(json["r"]["tree/sub/d.txt"]["text"], "d\n");
}

#[test]
fn the_view_decides_what_exists() {
    let project = Project::new("globaccess_hidden");
    let mut view = Vfs::mirror(&project.tree());
    view.nodes.remove(&project.tree().join("sub/c.pkl"));
    let mut caps = project.hosted();
    caps.module_view = view;
    assert_eq!(
        project.keys("**.pkl", caps)["i"],
        serde_json::json!([
            "tree/.hidden.pkl",
            "tree/a.pkl",
            "tree/sub/deep/e.pkl",
            "tree/z/f.pkl"
        ])
    );
}

#[test]
fn resource_view_is_separate_from_module_view() {
    let project = Project::new("globaccess_split");
    let mut caps = project.hosted();
    let mut resources = Vfs::mirror(&project.tree());
    resources.nodes.retain(|path, _| !path.ends_with("a.pkl"));
    caps.resource_view = Some(resources);
    let keys = project.keys("*.pkl", caps);
    assert_eq!(
        keys["i"],
        serde_json::json!(["tree/.hidden.pkl", "tree/a.pkl"])
    );
    assert_eq!(keys["r"], serde_json::json!(["tree/.hidden.pkl"]));
}

#[test]
fn view_errors_abort_the_glob() {
    let project = Project::new("globaccess_error");
    let mut vfs = Vfs::mirror(&project.tree());
    vfs.denied.push(project.tree().join("sub"));
    let error = expand_glob_with(&project.tree(), "**.pkl", &mut vfs).unwrap_err();
    assert!(error.to_string().contains("view denies"), "{error}");

    let mut caps = project.hosted();
    caps.module_view = vfs;
    let entry = project.0.join("entry.pkl");
    std::fs::write(&entry, "i = import*(\"tree/**.pkl\").keys\n").unwrap();
    let error = Evaluator::with_capabilities(caps)
        .eval_file(&entry)
        .unwrap_err();
    assert!(error.to_string().contains("view denies"), "{error}");
}

#[test]
fn walk_asks_the_view_not_the_disk() {
    let project = Project::new("globaccess_asked");
    let mut vfs = Vfs::mirror(&project.tree());
    let asked = vfs.asked.clone();
    expand_glob_with(&project.tree(), "sub/*.pkl", &mut vfs).unwrap();
    assert_eq!(*asked.lock().unwrap(), vec![project.tree().join("sub/")]);
}

#[test]
fn bare_relative_base_means_the_current_directory() {
    struct Record(Vec<PathBuf>);
    impl GlobAccess for Record {
        fn is_file(&mut self, path: &Path) -> pklr::Result<bool> {
            self.0.push(path.to_path_buf());
            Ok(true)
        }
        fn list_dir(&mut self, path: &Path) -> pklr::Result<Vec<(String, bool)>> {
            self.0.push(path.to_path_buf());
            Ok(vec![("m.pkl".into(), false)])
        }
    }
    let mut record = Record(Vec::new());
    let found = expand_glob_with(Path::new(""), "*.pkl", &mut record).unwrap();
    assert_eq!(found, vec![PathBuf::from("./m.pkl")]);
    assert_eq!(record.0, vec![PathBuf::from("./")]);
}

#[test]
fn default_glob_without_access_fails_instead_of_touching_the_disk() {
    struct Bare(NativeCapabilities);
    impl EvalCapabilities for Bare {
        fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
            self.0.read_to_string(path)
        }
        fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
            self.0.path_exists(path)
        }
        fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
            self.0.canonicalize(path)
        }
        fn read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
            self.0.read_bytes(path)
        }
        fn read_env(&mut self, name: &str) -> pklr::Result<Option<String>> {
            self.0.read_env(name)
        }
        fn fetch_text(&mut self, url: &str) -> pklr::Result<String> {
            self.0.fetch_text(url)
        }
        fn fetch_bytes(&mut self, url: &str) -> pklr::Result<Vec<u8>> {
            self.0.fetch_bytes(url)
        }
        fn temp_dir(&mut self, prefix: &str) -> pklr::Result<PathBuf> {
            self.0.temp_dir(prefix)
        }
    }
    let error = Bare(NativeCapabilities::new())
        .glob(Path::new("."), "*.pkl")
        .unwrap_err();
    assert!(error.to_string().contains("no glob access"), "{error}");
}
