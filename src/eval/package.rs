use super::*;

/// Resolved package info: either a direct file URL or a zip archive + entry path.
#[derive(Debug)]
pub(super) enum PackageSource {
    /// Direct file download (pkg.pkl-lang.org format)
    Direct { url: String, root: String },
    /// Zip archive URL + path within the archive
    Zip(String, String),
}

pub(super) fn package_cache_paths(
    cache_dir: &Path,
    url: &str,
    extension: &str,
) -> (PathBuf, PathBuf) {
    // FNV-1a gives package URLs stable, filesystem-safe names without making
    // the cache layout depend on platform path rules. The adjacent URL file
    // detects the extraordinarily unlikely hash collision.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let base = cache_dir.join("packages").join(format!("{hash:016x}"));
    (base.with_extension(extension), base.with_extension("url"))
}

pub(super) fn validate_package_bytes(url: &str, extension: &str, bytes: &[u8]) -> Result<()> {
    if extension == "pkl" {
        std::str::from_utf8(bytes)
            .map_err(|error| Error::Eval(format!("package source is not UTF-8: {url}: {error}")))?;
    }
    #[cfg(feature = "package-zip")]
    if extension == "zip" {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|error| Error::Eval(format!("package archive is invalid: {url}: {error}")))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| {
                Error::Eval(format!("package archive is invalid: {url}: {error}"))
            })?;
            std::io::copy(&mut entry, &mut std::io::sink()).map_err(|error| {
                Error::Eval(format!(
                    "package archive entry is invalid: {url}: {}: {error}",
                    entry.name()
                ))
            })?;
        }
    }
    Ok(())
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cache");
    let temp_path = path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    let rename_result = match std::fs::rename(&temp_path, path) {
        Ok(()) => return Ok(()),
        // Windows rename does not replace an existing file. There is a brief
        // non-atomic gap on that platform, but the destination must not retain
        // stale bytes while its URL sidecar is updated.
        Err(_) if path.exists() => {
            std::fs::remove_file(path).and_then(|()| std::fs::rename(&temp_path, path))
        }
        Err(error) => Err(error),
    };
    if rename_result.is_err() {
        let _ = std::fs::remove_file(temp_path);
    }
    rename_result
}

/// Resolve a `package://` URI to a download source.
pub(super) fn resolve_package_uri(uri: &str) -> Result<PackageSource> {
    fn sanitize_package_entry(fragment: &str) -> Result<String> {
        if fragment.is_empty()
            || fragment.starts_with('/')
            || fragment.contains('\\')
            || fragment.split('/').any(|part| part == "..")
        {
            return Err(Error::Eval(format!(
                "invalid package entry path: {fragment}"
            )));
        }
        let path = Path::new(fragment);
        if path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return Err(Error::Eval(format!(
                "invalid package entry path: {fragment}"
            )));
        }
        Ok(fragment.to_string())
    }

    // Format 1: package://pkg.pkl-lang.org/github.com/owner/repo@version#/path.pkl
    // These resolve to direct file downloads from GitHub releases
    if let Some(rest) = uri.strip_prefix("package://pkg.pkl-lang.org/github.com/")
        && let Some((repo_ver, fragment)) = rest.split_once('#')
        && let Some((repo, version)) = repo_ver.split_once('@')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        let root = format!("https://github.com/{repo}/releases/download/{version}/");
        return Ok(PackageSource::Direct {
            url: format!("{root}{file_path}"),
            root,
        });
    }
    // Format 2: package://pkg.pkl-lang.org/pkl-pantry/package@version#/path.pkl
    // These are under https://github.com/apple/pkl-pantry's release named `package@version`
    if let Some(rest) = uri.strip_prefix("package://pkg.pkl-lang.org/pkl-pantry/")
        && let Some((package_ver, fragment)) = rest.split_once('#')
        && let Some((package, version)) = package_ver.split_once('@')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        return Ok(PackageSource::Zip(
            format!(
                "https://github.com/apple/pkl-pantry/releases/download/{package}@{version}/{package}@{version}.zip"
            ),
            file_path,
        ));
    }
    // Format 3: package://github.com/owner/repo/releases/download/v1.0/name@1.0#/path.pkl
    // These are zip archives; the fragment is a path within the zip
    if let Some(rest) = uri.strip_prefix("package://github.com/")
        && let Some((base, fragment)) = rest.split_once('#')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        let zip_url = format!("https://github.com/{base}.zip");
        return Ok(PackageSource::Zip(zip_url, file_path));
    }
    // Format 4: package://host/path/name@version#/path.pkl
    // Generic package hosts are zip archives and can be redirected with
    // HTTP rewrite rules after resolving to https://host/path/name@version.zip.
    if let Some(rest) = uri.strip_prefix("package://")
        && !rest.starts_with("pkg.pkl-lang.org/")
        && let Some((base, fragment)) = rest.split_once('#')
    {
        let file_path = sanitize_package_entry(fragment.strip_prefix('/').unwrap_or(fragment))?;
        let zip_url = format!("https://{base}.zip");
        return Ok(PackageSource::Zip(zip_url, file_path));
    }
    Err(Error::Eval(format!("unsupported package URI: {uri}")))
}
