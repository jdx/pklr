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
                Ok(Some(resource_value(&uri, &self.read_bytes_io(&path)?)))
            }
            Target::Remote(uri) => Ok(Some(resource_value(
                &uri,
                &self.read_remote_resource(&uri)?,
            ))),
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
            let path = absolute_clean(&file_uri_path(uri)?);
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
                    let path = absolute_clean(&self.resolve_local_path(module, uri)?);
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
        if self
            .allowed_resources
            .iter()
            .any(|prefix| uri.starts_with(prefix))
        {
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

    pub(super) fn eval_read_glob(&mut self, pattern: &str, module: &Path) -> Result<Value> {
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
                    if glob_matches(pattern, &key) {
                        out.insert(key.into(), Value::String(value.into()));
                    }
                }
            }
            None | Some("file") => {
                let (base, glob, file_keys) = if uri_scheme(pattern) == Some("file") {
                    let path = file_uri_path(pattern)?;
                    let normalized = file_uri(&absolute_clean(&path));
                    self.check_resource_allowed(&normalized)?;
                    (
                        PathBuf::from("/"),
                        path.strip_prefix("/")
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string(),
                        true,
                    )
                } else {
                    let base = module_dir(module.parent().unwrap_or(Path::new("."))).to_path_buf();
                    // Validate the resolved, normalized pattern before globbing.
                    self.check_resource_allowed(&file_uri(&absolute_clean(&base.join(pattern))))?;
                    (base, pattern.to_string(), false)
                };
                for path in self.glob_io(&base, &glob)? {
                    let path = absolute_clean(&path);
                    let uri = file_uri(&path);
                    self.check_resource_allowed(&uri)?;
                    // Pkl keys explicit file globs by the same normalized,
                    // percent-encoded URI exposed by the resource itself.
                    let key = if file_keys {
                        uri.clone()
                    } else {
                        pathdiff_or_full(&path, &base)
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

fn file_uri_path(uri: &str) -> Result<PathBuf> {
    let rest = uri
        .strip_prefix("file:")
        .ok_or_else(|| Error::Eval(format!("Resource URI `{uri}` has invalid syntax.")))?;
    let path = rest
        .strip_prefix("//")
        .and_then(|s| s.find('/').map(|i| &s[i..]))
        .unwrap_or(rest);
    if !path.starts_with('/') {
        return Err(Error::Eval(format!(
            "Resource URI `{uri}` has invalid syntax. File URIs must have a path that starts with `/` (e.g. file:/path/to/my_resource)."
        )));
    }
    Ok(PathBuf::from(percent_decode(path)))
}

fn file_uri(path: &Path) -> String {
    format!(
        "file://{}",
        percent_encode(&normalize_pkl_path(&path.to_string_lossy()))
    )
}
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_' | b'~' | b'*') {
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
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            _ => out.push(component),
        }
    }
    out
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
