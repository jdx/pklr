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
                    String::from_utf8_lossy(entry.name_raw())
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

/// Split the optional `::sha256:<hex>` checksum off a package URI's
/// `name@version` part, returning the URI without it and the checksum.
pub(super) fn split_package_checksum(uri: &str) -> Result<(String, Option<&str>)> {
    let (location, fragment) = match uri.split_once('#') {
        Some((location, fragment)) => (location, Some(fragment)),
        None => (uri, None),
    };
    // The checksum follows the version, so search only after the last `@`;
    // `::` can appear earlier in the host (IPv6 literals) or path.
    let version_start = location.rfind('@').map_or(0, |at| at + 1);
    let Some((version, checksum)) = location[version_start..].split_once("::") else {
        return Ok((uri.to_string(), None));
    };
    let location = &location[..version_start + version.len()];
    let valid = checksum
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if !valid {
        return Err(Error::Eval(format!(
            "invalid package checksum (expected `::sha256:<64 hex digits>`): {uri}"
        )));
    }
    let stripped = match fragment {
        Some(fragment) => format!("{location}#{fragment}"),
        None => location.to_string(),
    };
    Ok((stripped, Some(checksum)))
}

/// Resolve a `package://` URI to a download source.
///
/// A `::sha256:<hex>` checksum after the version is ignored when locating the
/// package; see [`split_package_checksum`] and `Evaluator::resolve_package`.
pub(super) fn resolve_package_uri(uri: &str) -> Result<PackageSource> {
    let (stripped, _checksum) = split_package_checksum(uri)?;
    let uri = stripped.as_str();
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

/// Lowercase hex SHA-256 of `bytes`.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Evaluator {
    /// Resolve a `package://` URI, recording the checksum it requires (if any)
    /// for the archive extraction that follows. A URI without a checksum clears
    /// any earlier requirement, so each import is checked against its own.
    pub(super) fn resolve_package(&mut self, uri: &str) -> Result<PackageSource> {
        let source = resolve_package_uri(uri)?;
        let checksum = split_package_checksum(uri)?.1;
        match (&source, checksum) {
            (PackageSource::Zip(zip_url, _), Some(checksum)) => {
                let expected = checksum.trim_start_matches("sha256:").to_ascii_lowercase();
                self.package_checksums.insert(zip_url.clone(), expected);
            }
            (PackageSource::Zip(zip_url, _), None) => {
                self.package_checksums.remove(zip_url);
            }
            (PackageSource::Direct { .. }, Some(_)) => {
                return Err(Error::Unsupported(format!(
                    "package checksums are not supported for this package host: {uri}"
                )));
            }
            (PackageSource::Direct { .. }, None) => {}
        }
        Ok(source)
    }

    /// Check the package zip at `zip_url` against its `::sha256:` checksum:
    /// the metadata next to the archive must hash to the checksum, and the
    /// archive to the checksum the metadata lists, as the Pkl CLI does.
    #[cfg(feature = "package-zip")]
    pub(super) fn verify_package_zip(&mut self, zip_url: &str, expected: &str) -> Result<Vec<u8>> {
        let metadata_url = zip_url.strip_suffix(".zip").unwrap_or(zip_url);
        let metadata = self.fetch_package_bytes(metadata_url, "json")?;
        if sha256_hex(&metadata) != expected {
            self.remove_package_cache(metadata_url, "json");
            return Err(Error::Eval(format!(
                "package metadata checksum mismatch: {metadata_url} (expected sha256:{expected})"
            )));
        }
        let zip_checksum = serde_json::from_slice::<serde_json::Value>(&metadata)
            .ok()
            .and_then(|json| {
                json.get("packageZipChecksums")?
                    .get("sha256")?
                    .as_str()
                    .map(str::to_ascii_lowercase)
            })
            .ok_or_else(|| {
                Error::Eval(format!(
                    "package metadata has no packageZipChecksums.sha256: {metadata_url}"
                ))
            })?;
        let bytes = self.fetch_package_bytes(zip_url, "zip")?;
        if sha256_hex(&bytes) != zip_checksum {
            self.remove_package_cache(zip_url, "zip");
            return Err(Error::Eval(format!(
                "package archive checksum mismatch: {zip_url} (expected sha256:{zip_checksum})"
            )));
        }
        Ok(bytes)
    }

    pub(super) fn fetch_package_source(&mut self, url: &str) -> Result<String> {
        if let Some(cached) = self.http_cache.get(url) {
            return Ok(cached.clone());
        }
        let bytes = self.fetch_package_bytes(url, "pkl")?;
        let source = String::from_utf8(bytes)
            .map_err(|error| Error::Eval(format!("package source is not UTF-8: {url}: {error}")))?;
        self.http_cache.insert(url.to_string(), source.clone());
        Ok(source)
    }

    pub(super) fn fetch_direct_package_source(&mut self, url: &str, root: &str) -> Result<String> {
        self.package_http_roots.insert(root.to_string());
        self.fetch_package_source(url)
    }

    pub(super) fn fetch_package_bytes(&mut self, url: &str, extension: &str) -> Result<Vec<u8>> {
        match self.read_package_cache(url, extension) {
            Ok(Some(bytes)) => match validate_package_bytes(url, extension, &bytes) {
                Ok(()) => return Ok(bytes),
                Err(error) if self.offline => return Err(error),
                Err(_) => self.remove_package_cache(url, extension),
            },
            Ok(None) => {}
            Err(error) if self.offline => return Err(error),
            Err(_) => {
                // The persistent cache is an optimization while online. If it
                // is unreadable, fetch from the network and continue without it.
            }
        }
        if self.offline {
            let cache = self
                .package_cache_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<disabled>".to_string());
            return Err(Error::Eval(format!(
                "package is not cached and offline mode is enabled: {url} (cache: {cache})"
            )));
        }
        let fetch_url = self.rewrite_url(url).into_owned();
        let bytes = self.fetch_bytes_io(&fetch_url)?;
        validate_package_bytes(url, extension, &bytes)?;
        // Best-effort: the bytes are already in hand.
        let _ = self.write_package_cache(url, extension, &bytes);
        Ok(bytes)
    }

    pub(super) fn read_package_cache(
        &mut self,
        url: &str,
        extension: &str,
    ) -> Result<Option<Vec<u8>>> {
        let Some(cache_dir) = &self.package_cache_dir else {
            return Ok(None);
        };
        let (data_path, url_path) = package_cache_paths(cache_dir, url, extension);
        let cached_url = match self.read_to_string_io(&url_path) {
            Ok(cached_url) => cached_url,
            Err(Error::Io(_, error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if cached_url != url {
            return Ok(None);
        }
        match self.read_bytes_io(&data_path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::Io(_, error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Cache `bytes` for `url`. The fetch path ignores the error; preloading does not.
    pub(super) fn write_package_cache(
        &mut self,
        url: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let Some(cache_dir) = &self.package_cache_dir else {
            return Ok(());
        };
        let (data_path, url_path) = package_cache_paths(cache_dir, url, extension);
        let Some(parent) = data_path.parent() else {
            return Ok(());
        };
        self.create_dir_all_io(parent)?;
        self.write_atomic_io(&data_path, bytes)?;
        self.write_atomic_io(&url_path, url.as_bytes())
    }

    /// Seed the persistent package cache with `bytes` for `url`.
    ///
    /// `extension` is `"zip"` for archive packages and `"pkl"` for direct file
    /// downloads. A valid cache entry already present wins, so preloading never
    /// overrides content fetched from the network. Does nothing when no package
    /// cache directory is configured.
    pub fn preload_package(&mut self, url: &str, extension: &str, bytes: &[u8]) -> Result<()> {
        if self.package_cache_dir.is_none() {
            return Ok(());
        }
        if let Ok(Some(cached)) = self.read_package_cache(url, extension)
            && validate_package_bytes(url, extension, &cached).is_ok()
        {
            return Ok(());
        }
        validate_package_bytes(url, extension, bytes)?;
        self.write_package_cache(url, extension, bytes)
    }

    pub(super) fn remove_package_cache(&mut self, url: &str, extension: &str) {
        let Some(cache_dir) = &self.package_cache_dir else {
            return;
        };
        let (data_path, url_path) = package_cache_paths(cache_dir, url, extension);
        let _ = self.remove_file_io(&data_path);
        let _ = self.remove_file_io(&url_path);
    }

    /// Download a package zip and extract it to a temp directory.
    /// Returns the path to the extracted directory. Caches by zip URL.
    #[cfg(feature = "package-zip")]
    pub(super) fn extract_package_zip(&mut self, zip_url: &str) -> Result<PathBuf> {
        let expected = self.package_checksums.get(zip_url).cloned();
        // An archive extracted earlier (by prefetch or an unchecked import)
        // is reused only if this exact directory passed this checksum.
        if let Some(dir) = self.package_dirs.get(zip_url)
            && expected.as_ref().is_none_or(|expected| {
                self.verified_packages
                    .get(zip_url)
                    .is_some_and(|(verified_dir, checksum)| {
                        verified_dir == dir && checksum == expected
                    })
            })
        {
            return Ok(dir.clone());
        }
        let bytes = match &expected {
            Some(expected) => self.verify_package_zip(zip_url, expected)?,
            None => self.fetch_package_bytes(zip_url, "zip")?,
        };
        // Extract the verified bytes afresh, replacing any earlier unchecked copy.
        let prefix = format!("pklr-pkg-{}", self.package_dirs.len());
        let dir = self.temp_dir_io(&prefix)?;
        self.extract_zip_io(bytes, &dir)?;
        self.package_dirs.insert(zip_url.to_string(), dir.clone());
        match expected {
            Some(expected) => {
                self.verified_packages
                    .insert(zip_url.to_string(), (dir.clone(), expected));
            }
            None => {
                self.verified_packages.remove(zip_url);
            }
        }
        Ok(dir)
    }

    #[cfg(all(test, feature = "package-zip"))]
    pub(super) fn package_dir_for_zip(&self, zip_url: &str) -> Option<&PathBuf> {
        self.package_dirs.get(zip_url)
    }
}

#[cfg(test)]
mod package_uri_tests {
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    use std::path::PathBuf;

    #[cfg(feature = "native-io")]
    use super::Evaluator;
    use super::{PackageSource, resolve_package_uri};

    #[test]
    fn atomic_write_replaces_existing_file() {
        let test_dir = std::env::temp_dir().join(format!(
            "pklr-write-atomic-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        let path = test_dir.join("package.pkl");

        super::write_atomic(&path, b"old").unwrap();
        super::write_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        std::fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn generic_package_uri_resolves_to_zip_url() {
        let pkg =
            resolve_package_uri("package://example.com/v1.26.0/hk@1.26.0#/Config.pkl").unwrap();
        match pkg {
            PackageSource::Zip(zip_url, entry) => {
                assert_eq!(zip_url, "https://example.com/v1.26.0/hk@1.26.0.zip");
                assert_eq!(entry, "Config.pkl");
            }
            PackageSource::Direct { .. } => panic!("expected zip package source"),
        }
    }

    #[test]
    #[cfg(feature = "package-zip")]
    fn package_archive_validation_reads_entry_payloads() {
        use std::io::Write;

        let payload = b"payload whose checksum must be validated";
        let mut bytes = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut bytes);
            let mut archive = zip::ZipWriter::new(cursor);
            archive
                .start_file(
                    "Config.pkl",
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            archive.write_all(payload).unwrap();
            archive.finish().unwrap();
        }
        let payload_start = bytes
            .windows(payload.len())
            .position(|window| window == payload)
            .unwrap();
        bytes[payload_start] ^= 0xff;

        let error = super::validate_package_bytes("https://example.com/package.zip", "zip", &bytes)
            .unwrap_err()
            .to_string();
        assert!(error.contains("package archive entry is invalid"));
    }

    #[test]
    #[cfg(feature = "native-io")]
    fn preload_writes_the_package_cache() {
        let cache_dir = std::env::temp_dir().join(format!(
            "pklr-preload-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&cache_dir);
        let mut evaluator = Evaluator::default();
        evaluator.set_package_cache_dir(&cache_dir);

        evaluator
            .preload_package(
                "https://example.com/package@1.0.0.pkl",
                "pkl",
                b"answer = 42\n",
            )
            .unwrap();

        assert!(std::fs::read_dir(&cache_dir).unwrap().next().is_some());
        std::fs::remove_dir_all(cache_dir).unwrap();
    }

    /// Seed a cache dir with a package zip and its metadata, returning the
    /// evaluator and the metadata checksum. `zip_checksum` overrides the
    /// archive checksum the metadata lists.
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    fn checksummed_package(name: &str, zip_checksum: Option<&str>) -> (Evaluator, String, String) {
        use std::io::Write;

        let mut zip = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut zip));
            archive
                .start_file("Config.pkl", zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(b"answer = 42\n").unwrap();
            archive.finish().unwrap();
        }
        let listed = zip_checksum.map_or_else(|| super::sha256_hex(&zip), str::to_string);
        let metadata = format!(r#"{{"packageZipChecksums":{{"sha256":"{listed}"}}}}"#);
        let cache_dir =
            std::env::temp_dir().join(format!("pklr-checksum-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache_dir);
        let mut evaluator = Evaluator::default();
        evaluator.set_package_cache_dir(&cache_dir);
        evaluator.set_offline(true);
        evaluator
            .preload_package("https://example.com/p@1.0.zip", "zip", &zip)
            .unwrap();
        evaluator
            .preload_package("https://example.com/p@1.0", "json", metadata.as_bytes())
            .unwrap();
        let checksum = super::sha256_hex(metadata.as_bytes());
        (evaluator, checksum, metadata)
    }

    #[test]
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    fn checksummed_package_is_verified() {
        let (mut evaluator, checksum, _) = checksummed_package("ok", None);
        let source = format!(
            r#"import "package://example.com/p@1.0::sha256:{checksum}#/Config.pkl" as c
result = c.answer"#
        );
        let value = evaluator
            .eval_source(&source, std::path::Path::new("main.pkl"))
            .unwrap();
        assert_eq!(value.to_json()["result"], 42);
    }

    #[test]
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    fn checksummed_package_rejects_wrong_metadata_checksum() {
        let (mut evaluator, _, _) = checksummed_package("bad-meta", None);
        let wrong = "0".repeat(64);
        let source = format!(
            r#"import "package://example.com/p@1.0::sha256:{wrong}#/Config.pkl" as c
result = c.answer"#
        );
        let error = evaluator
            .eval_source(&source, std::path::Path::new("main.pkl"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("metadata checksum mismatch"), "{error}");
    }

    #[test]
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    fn checksummed_package_rejects_wrong_archive_checksum() {
        let (mut evaluator, checksum, _) = checksummed_package("bad-zip", Some(&"1".repeat(64)));
        let source = format!(
            r#"import "package://example.com/p@1.0::sha256:{checksum}#/Config.pkl" as c
result = c.answer"#
        );
        let error = evaluator
            .eval_source(&source, std::path::Path::new("main.pkl"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("archive checksum mismatch"), "{error}");
    }

    #[test]
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    fn checksum_is_enforced_after_an_unchecked_import_extracted_the_archive() {
        let (mut evaluator, checksum, metadata) = checksummed_package("reuse", None);
        let path = std::path::Path::new("main.pkl");
        evaluator
            .eval_source(
                r#"import "package://example.com/p@1.0#/Config.pkl" as c
result = c.answer"#,
                path,
            )
            .unwrap();
        let wrong = "0".repeat(64);
        let error = evaluator
            .eval_source(
                &format!(
                    r#"import "package://example.com/p@1.0::sha256:{wrong}#/Config.pkl" as c
result = c.answer"#
                ),
                path,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("metadata checksum mismatch"), "{error}");
        // The mistyped checksum does not poison a corrected retry. A mismatch
        // evicts the cached metadata, which this offline test re-seeds.
        evaluator
            .preload_package("https://example.com/p@1.0", "json", metadata.as_bytes())
            .unwrap();
        let value = evaluator
            .eval_source(
                &format!(
                    r#"import "package://example.com/p@1.0::sha256:{checksum}#/Config.pkl" as c
result = c.answer"#
                ),
                path,
            )
            .unwrap();
        assert_eq!(value.to_json()["result"], 42);
    }

    #[test]
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    fn package_dir_lookup_uses_source_zip_url() {
        let mut evaluator = Evaluator::default();
        evaluator.set_http_rewrites(&["https://example.com/=https://mirror.local/".to_string()]);
        let dir = PathBuf::from("/tmp/pklr-test-package");
        evaluator
            .package_dirs
            .insert("https://example.com/pkg@1.0.zip".to_string(), dir.clone());

        assert_eq!(
            evaluator
                .package_dir_for_zip("https://example.com/pkg@1.0.zip")
                .cloned(),
            Some(dir)
        );
    }

    #[test]
    fn package_uri_checksum_is_not_part_of_the_version() {
        let hash = "52298901b489e45a8713b7354ecaeeb603364ccf0e2db5bf17feb1c52f1f4863";
        let uri = format!("package://example.com/v1/hk@1.0::sha256:{hash}#/Config.pkl");
        match resolve_package_uri(&uri).unwrap() {
            PackageSource::Zip(zip_url, entry) => {
                assert_eq!(zip_url, "https://example.com/v1/hk@1.0.zip");
                assert_eq!(entry, "Config.pkl");
            }
            PackageSource::Direct { .. } => panic!("expected zip package source"),
        }
        match resolve_package_uri(&format!(
            "package://pkg.pkl-lang.org/github.com/o/r@1.0::sha256:{hash}#/a.pkl"
        ))
        .unwrap()
        {
            PackageSource::Direct { url, .. } => {
                assert_eq!(url, "https://github.com/o/r/releases/download/1.0/a.pkl");
            }
            PackageSource::Zip(..) => panic!("expected direct package source"),
        }
        assert!(resolve_package_uri("package://example.com/hk@1.0::md5:abcd#/a.pkl").is_err());
        match resolve_package_uri("package://[::1]:8080/pkg@1.0#/a.pkl").unwrap() {
            PackageSource::Zip(zip_url, _) => assert_eq!(zip_url, "https://[::1]:8080/pkg@1.0.zip"),
            PackageSource::Direct { .. } => panic!("expected zip package source"),
        }
    }

    #[test]
    fn generic_package_uri_rejects_path_traversal_entries() {
        let err = resolve_package_uri("package://example.com/pkg@1.0#/../secret.pkl")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid package entry path"));
    }

    #[test]
    fn malformed_registry_uri_does_not_fall_back_to_generic_zip() {
        let err =
            resolve_package_uri("package://pkg.pkl-lang.org/github.com/owner/repo#/Config.pkl")
                .unwrap_err()
                .to_string();
        assert!(err.contains("unsupported package URI"));
    }
}
