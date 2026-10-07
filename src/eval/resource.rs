//! Resource readers.  Resolution and authorization happen before any host IO.

use super::*;

pub(super) const DEFAULT_ALLOWED_RESOURCES: &[&str] = &[
    "prop:",
    "env:",
    "file:",
    "modulepath:",
    "package:",
    "projectpackage:",
    "https:",
];

impl Evaluator {
    pub(super) fn eval_read(&mut self, uri: &str, module: &Path, nullable: bool) -> Result<Value> {
        match self.read_resource(uri, module)? {
            Some(value) => Ok(value),
            None if nullable => Ok(Value::Null),
            None => Err(Error::Eval(format!("Cannot find resource `{uri}`."))),
        }
    }

    fn read_resource(&mut self, uri: &str, module: &Path) -> Result<Option<Value>> {
        let target = self.resolve_resource(uri, module)?;
        let cache_key = target.cache_key();
        if let Some(value) = self.resource_cache.get(&cache_key) {
            return Ok(Some(value.clone()));
        }
        let value = match target {
            Target::Env(name) => Ok(self.env_var(&name)?.map(|v| Value::String(v.into()))),
            Target::Prop(name) => Ok(self
                .external_properties
                .get(&name)
                .cloned()
                .map(|v| Value::String(v.into()))),
            Target::File { path, uri } => {
                if !self.path_exists_io(&path)? {
                    return Ok(None);
                }
                match self.read_bytes_io(&path) {
                    Ok(bytes) => Ok(Some(resource_value(&uri, &bytes))),
                    Err(Error::Io(_, error)) if error.kind() == std::io::ErrorKind::NotFound => {
                        Ok(None)
                    }
                    Err(error) => Err(error),
                }
            }
            Target::Remote(uri) => match self.read_remote_resource(&uri) {
                Ok(bytes) => Ok(Some(resource_value(&uri, &bytes))),
                Err(Error::ImportNotFound(_)) => Ok(None),
                Err(Error::Io(_, error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(None)
                }
                Err(error) => Err(error),
            },
            Target::Unreadable => Ok(None),
        };
        if let Some(value) = value? {
            self.resource_cache.insert(cache_key, value.clone());
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    fn read_remote_resource(&mut self, uri: &str) -> Result<Vec<u8>> {
        if !uri.starts_with("package://") {
            if self.offline {
                return Err(Error::Eval(format!(
                    "offline mode prevented HTTP fetch for {uri}"
                )));
            }
            return self.fetch_bytes_io(self.rewrite_url(uri).as_ref());
        }
        match resolve_package_uri(uri)? {
            PackageSource::Direct { url, .. } => self.fetch_package_bytes(&url, "pkl"),
            PackageSource::Zip(zip_url, entry) => {
                #[cfg(feature = "package-zip")]
                {
                    let dir = self.extract_package_zip(&zip_url)?;
                    self.read_bytes_io(&dir.join(entry))
                }
                #[cfg(not(feature = "package-zip"))]
                {
                    let _ = (zip_url, entry);
                    Err(Error::Unsupported(
                        "package zip resources require pklr's 'package-zip' feature".into(),
                    ))
                }
            }
        }
    }

    fn resolve_resource(&mut self, uri: &str, module: &Path) -> Result<Target> {
        // Parse and normalize explicit file URIs before their allowlist check.
        // Checking the spelling first would let `/safe/../secret` (including
        // percent-encoded dot segments) escape an allowed directory.
        if uri_scheme(uri) == Some("file") {
            let path = absolute_clean(&self.host_absolute_path(file_uri_path(uri)?));
            let normalized = file_uri(&path);
            self.check_resource_allowed(&normalized)?;
            return Ok(Target::File {
                path,
                uri: normalized,
            });
        }
        let absolute = match uri_scheme(uri) {
            Some(_) => uri.to_string(),
            None => match resolve_remote_relative(module, uri) {
                Some(remote) => remote,
                None => {
                    // When no file URI can possibly be allowed, fail before
                    // resolving a relative path through a capability.
                    if !self
                        .allowed_resources
                        .iter()
                        .any(|prefix| uri_scheme(prefix) == Some("file"))
                    {
                        self.check_resource_allowed("file:")?;
                    }
                    let path = self.resolve_resource_local_path(module, uri)?;
                    let path = absolute_clean(&self.host_absolute_path(path));
                    let normalized = file_uri(&path);
                    self.check_resource_allowed(&normalized)?;
                    return Ok(Target::File {
                        path,
                        uri: normalized,
                    });
                }
            },
        };
        self.check_resource_allowed(&absolute)?;
        let scheme = uri_scheme(&absolute).unwrap_or_default();
        let value = &absolute[scheme.len() + 1..];
        Ok(match scheme {
            "env" => Target::Env(percent_decode(value)),
            "prop" => Target::Prop(percent_decode(value)),
            "file" => Target::File {
                path: file_uri_path(&absolute)?,
                uri: absolute,
            },
            "http" | "https" | "package" => Target::Remote(absolute),
            _ => Target::Unreadable,
        })
    }

    fn check_resource_allowed(&self, uri: &str) -> Result<()> {
        // Schemes are case-sensitive in Pkl.  A prefix is compared after a
        // file URI has been normalized; this prevents `../` allowlist escapes.
        if self.allowed_resources.iter().any(|prefix| {
            if uri_scheme(uri) == Some("file") && uri_scheme(prefix) == Some("file") {
                return file_allowlist_matches(uri, prefix);
            }
            if uri.starts_with(prefix) {
                return true;
            }
            false
        }) {
            return Ok(());
        }
        Err(Error::Eval(format!(
            "Refusing to read resource `{uri}` because it does not match any entry in the resource allowlist."
        )))
    }

    fn env_var(&mut self, name: &str) -> Result<Option<String>> {
        let value = match &self.environment {
            Some(env) => env.get(name).cloned(),
            None => self.read_env_io(name)?,
        };
        self.env_reads.insert(name.to_string(), value.clone());
        Ok(value)
    }

    /// Give native paths their canonical absolute spelling while leaving a
    /// capability's virtual relative namespace untouched. Native
    /// canonicalization produces an absolute path; sandbox capabilities can
    /// return their own virtual path unchanged.
    pub(super) fn host_absolute_path(&mut self, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            path
        } else {
            self.canonicalize_io(&path).unwrap_or(path)
        }
    }

    /// Resolve a triple-dot resource while checking each actual candidate
    /// before asking the host whether that candidate exists.
    fn resolve_resource_local_path(&mut self, module: &Path, uri: &str) -> Result<PathBuf> {
        let Some(triple_dot) = parse_triple_dot_path(uri)? else {
            // Resolve the module identity, not the target: canonicalizing a
            // missing resource would fail and lose a valid native base path.
            let parent = module_dir(module.parent().unwrap_or(Path::new(".")));
            let parent = self
                .canonicalize_io(parent)
                .unwrap_or_else(|_| parent.to_path_buf());
            return Ok(parent.join(uri));
        };
        let module = self
            .canonicalize_io(module)
            .unwrap_or_else(|_| module.to_path_buf());
        #[cfg(feature = "package-zip")]
        let roots: Vec<PathBuf> = self.package_dirs.values().cloned().collect();
        #[cfg(feature = "package-zip")]
        let root = roots.into_iter().find_map(|root| {
            let root = self.canonicalize_io(&root).unwrap_or(root);
            module.starts_with(&root).then_some(root)
        });
        #[cfg(not(feature = "package-zip"))]
        let root: Option<PathBuf> = None;
        let found = resolve_triple_dot(&module, triple_dot, root.as_deref(), |candidate| {
            self.check_resource_allowed(&file_uri(&absolute_clean(candidate)))?;
            self.path_exists_io(candidate)
        })?;
        Ok(found.unwrap_or_else(|| module.parent().unwrap_or(Path::new(".")).join(uri)))
    }

    pub(super) fn eval_read_glob(&mut self, pattern: &str, module: &Path) -> Result<Value> {
        // Resource globs never support triple-dot lookup; reject it before
        // resolving a path or listing any ancestor directory.
        if parse_triple_dot_path(pattern)?.is_some() {
            return Err(Error::Eval(
                "Cannot combine resource globs with triple-dot module URIs.".into(),
            ));
        }
        // Relative resources in an HTTP/package module are remote-relative,
        // never paths in the evaluator process' filesystem namespace.
        let resolved_remote = uri_scheme(pattern)
            .is_none()
            .then(|| resolve_remote_relative(module, pattern))
            .flatten();
        let pattern = resolved_remote.as_deref().unwrap_or(pattern);
        let mut out = ObjectMap::default();
        match uri_scheme(pattern) {
            Some("env") | Some("prop") => {
                self.check_resource_allowed(pattern)?;
                let glob = GlobPart::compile(pattern, pattern)?;
                let env = uri_scheme(pattern) == Some("env");
                let entries: Vec<(String, String)> = if env {
                    match &self.environment {
                        Some(values) => {
                            values.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                        }
                        None => self.capabilities.env_vars()?,
                    }
                } else {
                    self.external_properties
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                };
                for (name, value) in entries {
                    if env {
                        self.env_reads.insert(name.clone(), Some(value.clone()));
                    }
                    let key = format!(
                        "{}:{}",
                        if env { "env" } else { "prop" },
                        percent_encode(&name)
                    );
                    if glob.matches(&key) {
                        out.insert(key.into(), Value::String(value.into()));
                    }
                }
            }
            None | Some("file") => {
                let (base, glob, file_keys) = if uri_scheme(pattern) == Some("file") {
                    let path = file_uri_path(pattern)?;
                    let normalized = file_uri(&absolute_clean(&path));
                    self.check_resource_allowed(&normalized)?;
                    let (base, glob) = file_glob_base_and_pattern(&path);
                    (base, glob, true)
                } else {
                    let base = absolute_clean(&self.host_absolute_path(
                        module_dir(module.parent().unwrap_or(Path::new("."))).to_path_buf(),
                    ));
                    // Validate the resolved, normalized pattern before globbing.
                    self.check_resource_allowed(&file_uri(&absolute_clean(&base.join(pattern))))?;
                    (base, pattern.to_string(), false)
                };
                for path in self.glob_io(&base, &glob)? {
                    let path = absolute_clean(&self.host_absolute_path(path));
                    let uri = file_uri(&path);
                    self.check_resource_allowed(&uri)?;
                    // Pkl keys explicit file globs by the same normalized,
                    // percent-encoded URI exposed by the resource itself.
                    let key = if file_keys {
                        uri.clone()
                    } else {
                        relative_glob_key(&path, &base)
                    };
                    out.insert(
                        key.into(),
                        resource_value(&uri, &self.read_bytes_io(&path)?),
                    );
                }
            }
            Some(scheme) => {
                self.check_resource_allowed(pattern)?;
                return Err(Error::Eval(format!(
                    "Cannot expand glob pattern `{pattern}` because scheme `{scheme}` is not globbable."
                )));
            }
        }
        Ok(Value::Object(Arc::new(out), None))
    }
}

enum Target {
    Env(String),
    Prop(String),
    File { path: PathBuf, uri: String },
    Remote(String),
    Unreadable,
}

impl Target {
    fn cache_key(&self) -> String {
        match self {
            Self::Env(name) => format!("env:{name}"),
            Self::Prop(name) => format!("prop:{name}"),
            Self::File { uri, .. } | Self::Remote(uri) => uri.clone(),
            Self::Unreadable => "unreadable:".to_string(),
        }
    }
}

pub(super) fn uri_scheme(uri: &str) -> Option<&str> {
    let colon = uri.find(':')?;
    let scheme = &uri[..colon];
    (scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// Convert a Pkl `file:` URI to its host path.
///
/// Module resolution uses the same conversion as resources so percent escapes,
/// Windows drive URIs, and file authorities have one consistent meaning.
pub(crate) fn file_uri_path(uri: &str) -> Result<PathBuf> {
    let rest = uri
        .strip_prefix("file:")
        .ok_or_else(|| Error::Eval(format!("Resource URI `{uri}` has invalid syntax.")))?;
    let path = rest
        .strip_prefix("//")
        .and_then(|s| s.find('/').map(|i| &s[i..]))
        .unwrap_or(rest);
    // `file://server/share` identifies a UNC path on Windows. Keep the
    // authority in that case; `localhost` remains the local host and keeps
    // the pre-existing absolute-path interpretation. Other platforms retain
    // their existing resource URI behavior.
    #[cfg(windows)]
    let path = rest
        .strip_prefix("//")
        .and_then(|authority_and_path| authority_and_path.split_once('/'))
        .filter(|(authority, _)| {
            !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost")
        })
        .map(|(authority, path)| format!("//{authority}/{path}"))
        .unwrap_or_else(|| path.to_owned());
    if !path.starts_with('/') {
        return Err(Error::Eval(format!(
            "Resource URI `{uri}` has invalid syntax. File URIs must have a path that starts with `/` (e.g. file:/path/to/my_resource)."
        )));
    }
    #[cfg(windows)]
    let path = percent_decode(&path);
    #[cfg(not(windows))]
    let path = percent_decode(path);
    #[cfg(windows)]
    let path = path
        .strip_prefix('/')
        .filter(|path| {
            path.as_bytes().len() >= 2
                && path.as_bytes()[0].is_ascii_alphabetic()
                && path.as_bytes()[1] == b':'
        })
        .unwrap_or(&path);
    Ok(PathBuf::from(path))
}

pub(super) fn file_uri(path: &Path) -> String {
    let path = percent_encode(&normalize_pkl_path(&path.to_string_lossy()));
    if path.len() >= 2 && path.as_bytes()[0].is_ascii_alphabetic() && path.as_bytes()[1] == b':' {
        format!("file:///{path}")
    } else {
        format!("file://{path}")
    }
}

fn file_glob_base_and_pattern(path: &Path) -> (PathBuf, String) {
    #[cfg(windows)]
    {
        use std::path::Component;

        let mut base = PathBuf::new();
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            base.push(prefix.as_os_str());
        }
        if let Some(Component::RootDir) = components.next() {
            base.push(Path::new("\\"));
        }
        return (
            base,
            normalize_pkl_path(&components.as_path().to_string_lossy()),
        );
    }
    #[cfg(not(windows))]
    {
        (
            PathBuf::from("/"),
            path.strip_prefix("/")
                .unwrap_or(path)
                .to_string_lossy()
                .to_string(),
        )
    }
}
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric()
                || matches!(b, b'/' | b'.' | b'-' | b'_' | b'~' | b'*' | b':')
            {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn percent_decode(text: &str) -> String {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            let b = high << 4 | low;
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
fn absolute_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if out.components().next_back().is_some_and(|component| {
                    !matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                }) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(component);
                }
            }
            std::path::Component::CurDir => {}
            _ => out.push(component),
        }
    }
    out
}

/// A resource glob key remains relative to its importing module, even when a
/// literal `..` prefix walks above that directory.
fn relative_glob_key(path: &Path, base: &Path) -> String {
    let path: Vec<_> = path.components().collect();
    let base: Vec<_> = base.components().collect();
    let shared = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut key = PathBuf::new();
    for _ in shared..base.len() {
        key.push("..");
    }
    for component in &path[shared..] {
        key.push(component.as_os_str());
    }
    normalize_pkl_path(&key.to_string_lossy())
}

/// Match normalized file allowlist entries. A trailing slash declares a
/// directory prefix; other prefixes retain the builder's documented URI
/// prefix semantics.
fn file_allowlist_matches(uri: &str, prefix: &str) -> bool {
    if prefix == "file:" {
        return true;
    }
    let Ok(path) = file_uri_path(prefix) else {
        return false;
    };
    let directory = prefix.ends_with('/');
    let prefix = file_uri(&absolute_clean(&path));
    if !directory || prefix == "file:///" || prefix.ends_with(":/") {
        return uri.starts_with(&prefix);
    }
    uri == prefix
        || uri
            .strip_prefix(&prefix)
            .is_some_and(|remaining| remaining.starts_with('/'))
}
fn resource_value(uri: &str, bytes: &[u8]) -> Value {
    let mut map = ObjectMap::default();
    map.insert("uri".into(), Value::String(uri.into()));
    map.insert(
        "text".into(),
        Value::String(String::from_utf8_lossy(bytes).into()),
    );
    map.insert("base64".into(), Value::String(base64(bytes).into()));
    Value::Object(Arc::new(map), None)
}
fn base64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | ((c.get(1).copied().unwrap_or(0) as u32) << 8)
            | c.get(2).copied().unwrap_or(0) as u32;
        o.push(A[(n >> 18) as usize & 63] as char);
        o.push(A[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        o.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    o
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn file_uri_path_preserves_unc_authority() {
        let path = super::file_uri_path("file://server/share/Config%20One.pkl").unwrap();
        assert_eq!(
            path,
            std::path::PathBuf::from(r"\\server\share\Config One.pkl")
        );
    }
}
