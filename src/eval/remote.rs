use super::*;

pub(super) fn local_module_path(current_path: &Path, uri: &str) -> Option<PathBuf> {
    if uri.contains("://") && !uri.starts_with("file://") {
        return None;
    }
    // A relative reference inside a remote (http/https) module is not a local file.
    if !uri.starts_with("file://")
        && let Some(base) = current_path.to_str()
        && (base.starts_with("http://") || base.starts_with("https://"))
    {
        return None;
    }
    Some(if let Some(rel) = uri.strip_prefix("file://") {
        PathBuf::from(rel)
    } else {
        current_path.parent().unwrap_or(Path::new(".")).join(uri)
    })
}

/// If `current_path` is a remote (http/https) module URL and `uri` is a
/// relative reference (no scheme), resolve `uri` against that URL so the
/// referenced module is fetched over HTTP rather than from the local
/// filesystem. Returns the rewritten absolute URL, or `None` when no rewrite
/// applies (local base module, or `uri` already carries a scheme).
pub(super) fn resolve_remote_relative(current_path: &Path, uri: &str) -> Option<String> {
    if uri.contains("://") || uri.starts_with("pkl:") {
        return None;
    }
    let base = current_path.to_str()?;
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return None;
    }
    resolve_http_relative(base, uri)
}

pub(super) fn resolve_http_relative(base: &str, uri: &str) -> Option<String> {
    url::Url::parse(base)
        .ok()?
        .join(uri)
        .ok()
        .map(|url| url.to_string())
}

pub(super) fn canonical_remote_module_identity(uri: &str) -> String {
    url::Url::parse(uri)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| uri.to_string())
}

/// The path after the `...` of a triple-dot URI (`""` for a bare `...`), or
/// `None` for any other URI. Mirrors pkl's `IoUtils.parseTripleDotPath`.
pub(crate) fn parse_triple_dot_path(uri: &str) -> Result<Option<&str>> {
    let Some(rest) = uri.strip_prefix("...") else {
        return Ok(None);
    };
    if rest.is_empty() {
        return Ok(Some(""));
    }
    match rest.strip_prefix('/') {
        Some(path) if !path.is_empty() => Ok(Some(path)),
        _ => Err(Error::Eval(format!(
            "Module URI `{uri}` has invalid syntax: expected `...` or `.../path/to/my_module.pkl`"
        ))),
    }
}

/// Pkl's triple-dot resolution (`IoUtils.resolveTripleDotImport`): try `path`
/// in the parent of the module's directory, then in each ancestor up to
/// `root` (the file system root when `None`), and return the first candidate
/// that `exists` and is not the module itself. An empty `path` (from `...`)
/// looks for the module's own file name.
pub(crate) fn resolve_triple_dot(
    current_path: &Path,
    path: &str,
    root: Option<&Path>,
    mut exists: impl FnMut(&Path) -> Result<bool>,
) -> Result<Option<PathBuf>> {
    // Keep resolution in the evaluator's path namespace. In particular, do
    // not call `std::path::absolute`: capability-backed evaluators can use
    // relative virtual paths, and `absolute` would reinterpret them relative
    // to the host process. Normalize lexical dot segments so the ancestor
    // walk follows the module's actual path rather than a `.` or `..` alias.
    let current = normalize_lexical_path(current_path);
    let file_name;
    let path = if path.is_empty() {
        file_name = current
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        file_name.as_str()
    } else {
        path
    };
    let root = root.or_else(|| current.ancestors().last());
    let mut candidates = current
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .skip(1)
        .take_while(|dir| root.is_none_or(|root| dir.starts_with(root)))
        .collect::<Vec<_>>();
    // The root is tried last even when the module sits directly in it.
    if let Some(root) = root
        && candidates.last() != Some(&root)
    {
        candidates.push(root);
    }
    for dir in candidates {
        let candidate = normalize_lexical_path(&dir.join(path));
        if candidate != current && exists(&candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() && !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
