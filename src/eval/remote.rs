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
