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
    /// Whether the `resource_*` hooks are overridden at all.
    override_resource: bool,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Split {
    fn new(deny: Deny, override_resource: bool) -> (Self, Arc<Mutex<Vec<&'static str>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                native: NativeCapabilities::new(),
                deny,
                override_resource,
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
        if !self.override_resource {
            return self.path_exists(path);
        }
        self.log.lock().unwrap().push("resource_path_exists");
        self.refuse(Deny::Resources, "resource existence")?;
        self.native.path_exists(path)
    }

    fn resource_read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        if !self.override_resource {
            return self.read_bytes(path);
        }
        self.log.lock().unwrap().push("resource_read_bytes");
        self.refuse(Deny::Resources, "resource read")?;
        self.native.read_bytes(path)
    }

    fn resource_glob(&mut self, base: &Path, pattern: &str) -> pklr::Result<Vec<PathBuf>> {
        if !self.override_resource {
            return self.glob(base, pattern);
        }
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
    let (caps, _) = Split::new(Deny::Resources, true);
    assert_eq!(project.eval("import.pkl", caps).unwrap()["value"], 42);
    let (caps, _) = Split::new(Deny::Resources, true);
    assert_eq!(
        project.eval("import_glob.pkl", caps).unwrap()["values"]["mod.pkl"]["value"],
        42
    );
    for file in ["read.pkl", "read_glob.pkl"] {
        let (caps, _) = Split::new(Deny::Resources, true);
        let error = project.eval(file, caps).unwrap_err();
        assert!(error.contains("resource"), "{file}: {error}");
        assert!(error.contains("denied"), "{file}: {error}");
    }
}

#[test]
fn denying_modules_still_allows_resources() {
    let project = Project::new("rescap_deny_mod");
    let (caps, _) = Split::new(Deny::Modules, true);
    assert_eq!(project.eval("read.pkl", caps).unwrap()["value"], "data\n");
    let (caps, _) = Split::new(Deny::Modules, true);
    assert!(
        project.eval("read_glob.pkl", caps).unwrap()["values"]["input.txt"]["text"] == "data\n"
    );
    for file in ["import.pkl", "import_glob.pkl"] {
        let (caps, _) = Split::new(Deny::Modules, true);
        let error = project.eval(file, caps).unwrap_err();
        assert!(error.contains("module"), "{file}: {error}");
        assert!(error.contains("denied"), "{file}: {error}");
    }
}

#[test]
fn resource_reads_call_only_resource_hooks() {
    let project = Project::new("rescap_hooks");
    let (caps, log) = Split::new(Deny::Nothing, true);
    project.eval("read.pkl", caps).unwrap();
    let log = log.lock().unwrap().clone();
    assert!(log.contains(&"resource_path_exists"), "{log:?}");
    assert!(log.contains(&"resource_read_bytes"), "{log:?}");
    assert!(!log.contains(&"read_bytes"), "{log:?}");
}

#[test]
fn defaults_delegate_to_the_module_hooks() {
    // With the `resource_*` methods left at their defaults, denying the
    // module hooks denies resources too, exactly as before the split.
    let project = Project::new("rescap_defaults");
    for file in ["read.pkl", "read_glob.pkl", "import.pkl"] {
        let (caps, _) = Split::new(Deny::Modules, false);
        assert!(project.eval(file, caps).is_err(), "{file}");
    }
    for (file, key) in [("read.pkl", "value"), ("import.pkl", "value")] {
        let (caps, _) = Split::new(Deny::Nothing, false);
        let value = project.eval(file, caps).unwrap();
        assert!(value.get(key).is_some(), "{file}");
    }
    let (caps, log) = Split::new(Deny::Nothing, false);
    project.eval("read_glob.pkl", caps).unwrap();
    assert!(log.lock().unwrap().contains(&"glob"));
}
