//! Resource IO hooks on `EvalCapabilities`: `resource_path_exists`,
//! `resource_read_bytes` and `resource_glob` are separate from the module-level
//! `path_exists`, `read_bytes` and `glob`, and default to them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pklr::{Error, EvalCapabilities, Evaluator, NativeCapabilities};

#[derive(Clone, Copy, PartialEq)]
enum Deny {
    /// Delegate everything: only the default `resource_*` methods.
    Nothing,
    /// Refuse resource IO but allow module IO.
    Resources,
    /// Refuse module IO but allow resource IO.
    Modules,
}

/// Native capabilities that refuse one side of file IO, and log which hooks ran.
struct Split {
    native: NativeCapabilities,
    deny: Deny,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Split {
    fn new(deny: Deny) -> (Self, Arc<Mutex<Vec<&'static str>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                native: NativeCapabilities::new(),
                deny,
                log: log.clone(),
            },
            log,
        )
    }

    fn refuse(&self, side: Deny, what: &str) -> pklr::Result<()> {
        if self.deny == side {
            Err(Error::Eval(format!("{what} denied")))
        } else {
            Ok(())
        }
    }
}

impl EvalCapabilities for Split {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.native.read_to_string(path)
    }

    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        self.log.lock().unwrap().push("path_exists");
        self.refuse(Deny::Modules, "module existence")?;
        self.native.path_exists(path)
    }

    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        self.native.canonicalize(path)
    }

    fn read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        self.log.lock().unwrap().push("read_bytes");
        self.refuse(Deny::Modules, "module read")?;
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

    fn glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        self.log.lock().unwrap().push("glob");
        self.refuse(Deny::Modules, "module glob")?;
        self.native.glob(base, pattern)
    }

    fn resource_path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        self.log.lock().unwrap().push("resource_path_exists");
        self.refuse(Deny::Resources, "resource existence")?;
        self.native.path_exists(path)
    }

    fn resource_read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        self.log.lock().unwrap().push("resource_read_bytes");
        self.refuse(Deny::Resources, "resource read")?;
        self.native.read_bytes(path)
    }

    fn resource_glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        self.log.lock().unwrap().push("resource_glob");
        self.refuse(Deny::Resources, "resource glob")?;
        self.native.glob(base, pattern)
    }
}

struct Project(PathBuf);

impl Project {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("mod.pkl"), "value = 42\n").unwrap();
        std::fs::write(dir.join("input.txt"), "data\n").unwrap();
        std::fs::write(
            dir.join("import.pkl"),
            "import \"mod.pkl\" as m\nvalue = m.value\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("import_glob.pkl"),
            "values = import*(\"mod*.pkl\")\n",
        )
        .unwrap();
        std::fs::write(dir.join("read.pkl"), "value = read(\"input.txt\").text\n").unwrap();
        std::fs::write(
            dir.join("read_glob.pkl"),
            "values = read*(\"input*.txt\")\n",
        )
        .unwrap();
        Self(dir)
    }

    fn eval(&self, file: &str, capabilities: Split) -> Result<serde_json::Value, String> {
        Evaluator::with_capabilities(capabilities)
            .eval_file(&self.0.join(file))
            .map(|value| value.to_json())
            .map_err(|error| error.to_string())
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn denying_resources_still_allows_imports() {
    let project = Project::new("rescap_deny_res");
    let (caps, _) = Split::new(Deny::Resources);
    assert_eq!(project.eval("import.pkl", caps).unwrap()["value"], 42);
    let (caps, _) = Split::new(Deny::Resources);
    assert_eq!(
        project.eval("import_glob.pkl", caps).unwrap()["values"]["mod.pkl"]["value"],
        42
    );
    for file in ["read.pkl", "read_glob.pkl"] {
        let (caps, _) = Split::new(Deny::Resources);
        let error = project.eval(file, caps).unwrap_err();
        assert!(error.contains("resource"), "{file}: {error}");
        assert!(error.contains("denied"), "{file}: {error}");
    }
}

#[test]
fn denying_modules_still_allows_resources() {
    let project = Project::new("rescap_deny_mod");
    let (caps, _) = Split::new(Deny::Modules);
    assert_eq!(project.eval("read.pkl", caps).unwrap()["value"], "data\n");
    let (caps, _) = Split::new(Deny::Modules);
    assert!(
        project.eval("read_glob.pkl", caps).unwrap()["values"]["input.txt"]["text"] == "data\n"
    );
    for file in ["import.pkl", "import_glob.pkl"] {
        let (caps, _) = Split::new(Deny::Modules);
        let error = project.eval(file, caps).unwrap_err();
        assert!(error.contains("module"), "{file}: {error}");
        assert!(error.contains("denied"), "{file}: {error}");
    }
}

#[test]
fn resource_reads_call_only_resource_hooks() {
    let project = Project::new("rescap_hooks");
    let (caps, log) = Split::new(Deny::Nothing);
    project.eval("read.pkl", caps).unwrap();
    let log = log.lock().unwrap().clone();
    assert!(log.contains(&"resource_path_exists"), "{log:?}");
    assert!(log.contains(&"resource_read_bytes"), "{log:?}");
    assert!(!log.contains(&"read_bytes"), "{log:?}");
}

/// Only the module-level hooks; the `resource_*` methods are the trait defaults.
struct Plain {
    native: NativeCapabilities,
    deny: bool,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl EvalCapabilities for Plain {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.native.read_to_string(path)
    }

    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        self.log.lock().unwrap().push("path_exists");
        if self.deny {
            return Err(Error::Eval("existence denied".into()));
        }
        self.native.path_exists(path)
    }

    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        self.native.canonicalize(path)
    }

    fn read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        self.log.lock().unwrap().push("read_bytes");
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

    fn glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        self.log.lock().unwrap().push("glob");
        self.native.glob(base, pattern)
    }
}

#[test]
fn defaults_delegate_to_the_module_hooks() {
    let project = Project::new("rescap_defaults");
    for file in ["read.pkl", "read_glob.pkl", "import.pkl"] {
        let log = Arc::new(Mutex::new(Vec::new()));
        let caps = Plain {
            native: NativeCapabilities::new(),
            deny: false,
            log: log.clone(),
        };
        let value = Evaluator::with_capabilities(caps)
            .eval_file(&project.0.join(file))
            .unwrap()
            .to_json();
        assert!(value.as_object().is_some_and(|o| !o.is_empty()), "{file}");
        let log = log.lock().unwrap();
        match file {
            "read.pkl" => assert!(log.contains(&"read_bytes") && log.contains(&"path_exists")),
            "read_glob.pkl" => assert!(log.contains(&"glob") && log.contains(&"read_bytes")),
            _ => assert!(log.contains(&"path_exists")),
        }
    }
    // Denying the module-level hook denies resources too, as before the split.
    let caps = Plain {
        native: NativeCapabilities::new(),
        deny: true,
        log: Arc::default(),
    };
    let error = Evaluator::with_capabilities(caps)
        .eval_file(&project.0.join("read.pkl"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("existence denied"), "{error}");
}

#[test]
fn modulepath_resources_use_the_resource_hooks() {
    let project = Project::new("rescap_modulepath");
    std::fs::create_dir_all(project.0.join("mp")).unwrap();
    std::fs::write(project.0.join("mp/data.txt"), "mp\n").unwrap();
    std::fs::write(
        project.0.join("mpread.pkl"),
        "value = read(\"modulepath:/data.txt\").text\n",
    )
    .unwrap();
    let (caps, log) = Split::new(Deny::Modules);
    let mut evaluator = Evaluator::with_capabilities(caps);
    evaluator.set_module_path(vec![project.0.join("mp")]);
    let value = evaluator.eval_file(&project.0.join("mpread.pkl")).unwrap();
    assert_eq!(value.to_json()["value"], "mp\n");
    let log = log.lock().unwrap();
    assert!(log.contains(&"resource_path_exists"), "{log:?}");
    assert!(!log.contains(&"path_exists"), "{log:?}");
}
