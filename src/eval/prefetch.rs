//! Concurrent prefetching of remote modules.
//!
//! Before a module is evaluated, its remote dependencies (`http(s)://` and
//! `package://` imports, amends and extends) are collected without evaluating
//! anything, resolved exactly as evaluation resolves them, and fetched in one
//! batch through [`EvalCapabilities::fetch_text_many`] and
//! [`EvalCapabilities::fetch_bytes_many`]. The fetched modules are scanned the
//! same way, level by level, until no new URLs turn up or the per-evaluation
//! budget (levels, requests and bytes) is spent.
//!
//! Prefetching only fills the caches evaluation already reads (`http_cache`,
//! the persistent package cache and the extracted package directories). A
//! failed prefetch is simply not cached, so the fetch at evaluation time runs
//! as it would without prefetching and reports the same error.
//!
//! [`EvalCapabilities::fetch_text_many`]: crate::EvalCapabilities::fetch_text_many
//! [`EvalCapabilities::fetch_bytes_many`]: crate::EvalCapabilities::fetch_bytes_many

use super::*;
use crate::capabilities::FetchBudget;

/// The most levels of remote imports one prefetch follows.
const MAX_LEVELS: usize = 8;
/// The most requests prefetching makes in one evaluation.
const MAX_REQUESTS: usize = 256;
/// The most response bytes prefetching downloads in one evaluation.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// What prefetching has done so far in one evaluation. Prefetching follows
/// every remote import, used or not, so it runs on a budget: once that is
/// spent, the remaining imports are left to evaluation, which fetches only
/// the ones it uses.
pub(super) struct PrefetchState {
    /// Cache keys of the remote modules prefetched or attempted.
    attempted: HashSet<String>,
    /// Requests left in the budget.
    requests: usize,
    /// Response bytes left in the budget, charged by the capabilities as
    /// they download.
    bytes: FetchBudget,
}

impl Default for PrefetchState {
    fn default() -> Self {
        Self::new(None)
    }
}

impl PrefetchState {
    /// A fresh budget, spent at once when `cancel` is set, so a cancelled
    /// evaluation starts no further prefetch requests and drops the bodies
    /// still downloading.
    pub(super) fn new(cancel: Option<Arc<std::sync::atomic::AtomicBool>>) -> Self {
        Self {
            attempted: HashSet::default(),
            requests: MAX_REQUESTS,
            bytes: FetchBudget::new(MAX_BYTES).cancelled_by(cancel),
        }
    }
}

impl PrefetchState {
    fn exhausted(&self) -> bool {
        self.requests == 0 || self.bytes.is_spent()
    }
}

/// Direct-download package roots that are certain to be registered in
/// `package_http_roots` by the time evaluation resolves a module's imports:
/// those of the packages the module itself belongs to. Evaluation registers a
/// root when it loads a file of that package, so a file's own imports always
/// see it, while an unrelated import elsewhere may be evaluated without it.
type Roots = Rc<Vec<String>>;

/// The remote imports of each module prefetching loaded, with the path
/// evaluation gives that module and its [`Roots`], to queue for the next
/// level.
type Scanned = Vec<(Vec<String>, String, Roots)>;

/// Record the non-glob import URIs of `source` (loaded from `path`) for the
/// next level, scanning its tokens for the same import forms
/// `module_import_uris` reads from a parsed module, without parsing a module
/// evaluation may never use. A module that does not lex is skipped;
/// evaluation reports its error if it is used.
fn scan(scanned: &mut Scanned, source: &str, path: String, roots: Roots) {
    let Ok(tokens) = lexer::lex_named(source, &path) else {
        return;
    };
    let imports = parser::collect_imports_with_kind(&tokens)
        .into_iter()
        // Glob imports are expanded at evaluation time.
        .filter_map(|(uri, is_glob)| (!is_glob).then_some(uri))
        .collect();
    scanned.push((imports, path, roots));
}

/// The URIs of the modules `module` imports, amends or extends, other than
/// glob imports, which are expanded at evaluation time.
fn module_import_uris(module: &Module) -> impl Iterator<Item = &str> {
    module
        .amends
        .iter()
        .chain(module.extends.iter())
        .chain(
            module
                .imports
                .iter()
                .filter(|import| !import.is_glob)
                .map(|import| &import.uri),
        )
        .chain(module.import_exprs.iter())
        .map(String::as_str)
}

/// A remote module to prefetch. `roots` are the [`Roots`] of the module once
/// loaded, for resolving its own imports.
enum Prefetch {
    /// A plain HTTP module, cached in `http_cache` under its rewritten URL.
    Http {
        url: String,
        fetch_url: String,
        roots: Roots,
    },
    /// A direct-download package source, cached in `http_cache` under its URL
    /// and in the persistent package cache.
    PackageFile { url: String, roots: Roots },
    /// A package archive, cached in the persistent package cache and
    /// extracted into `package_dirs`.
    /// `entries` are the archive paths imported from it, each scanned after
    /// the single extraction.
    #[cfg(feature = "package-zip")]
    PackageZip {
        url: String,
        entries: Vec<String>,
        roots: Roots,
    },
}

impl Prefetch {
    /// Identifies this target's cache work, for skipping targets already
    /// attempted in this evaluation.
    fn attempt_key(&self) -> String {
        match self {
            Prefetch::Http { fetch_url, .. } => format!("http {fetch_url}"),
            Prefetch::PackageFile { url, .. } => format!("pkl {url}"),
            #[cfg(feature = "package-zip")]
            Prefetch::PackageZip { url, .. } => format!("zip {url}"),
        }
    }

    fn is_http(&self) -> bool {
        matches!(self, Prefetch::Http { .. })
    }
}

/// One request and every target that needs its response. Targets that share
/// a URL are fetched once, and each still does its own cache work.
struct Download {
    fetch_url: String,
    targets: Vec<Prefetch>,
}

impl Download {
    /// Plain HTTP modules are fetched as text, as evaluation fetches them;
    /// anything involving a package as bytes.
    fn is_text(&self) -> bool {
        self.targets.iter().all(Prefetch::is_http)
    }
}

impl Evaluator {
    /// Prefetch the remote modules `module` (at `path`) depends on.
    ///
    /// Does nothing beyond scanning the module's import URIs when none of
    /// them is remote, or when the evaluator is offline.
    pub(super) fn prefetch_remote_imports(&mut self, module: &Module, path: &Path) {
        if self.offline || self.prefetch.exhausted() {
            return;
        }
        // Only a remote module's relative imports and absolute remote URIs
        // can name remote modules; skip resolving anything else.
        let base_is_remote = path
            .to_str()
            .is_some_and(|base| base.starts_with("http://") || base.starts_with("https://"));
        let roots = Roots::default();
        let mut level = Vec::new();
        for uri in module_import_uris(module) {
            if base_is_remote
                || uri.starts_with("https://")
                || uri.starts_with("http://")
                || uri.starts_with("package://")
            {
                self.prefetch_target(uri, path, &roots, &mut level);
            }
        }
        if !level.is_empty() {
            self.prefetch_levels(level);
        }
    }

    /// Queue `uri`, referenced from the module at `base` (whose [`Roots`] are
    /// `roots`), when it names a remote module that is not cached yet.
    ///
    /// The cache key is the one `fetch_source` will use: an HTTP URL under a
    /// package root registered now, or under one of `roots`, is a package
    /// file keyed by its URL; any other is keyed by its rewritten URL. Roots
    /// of other prefetched packages are not consulted, since evaluation may
    /// never load those packages.
    fn prefetch_target(&self, uri: &str, base: &Path, roots: &Roots, out: &mut Vec<Prefetch>) {
        let resolved = resolve_remote_relative(base, uri);
        let uri = resolved.as_deref().unwrap_or(uri);
        if uri.starts_with("https://") || uri.starts_with("http://") {
            let in_package = self
                .package_http_roots
                .iter()
                .chain(roots.iter())
                .any(|root| uri.starts_with(root.as_str()));
            if in_package {
                if !self.http_cache.contains_key(uri) {
                    out.push(Prefetch::PackageFile {
                        url: uri.to_string(),
                        roots: roots.clone(),
                    });
                }
                return;
            }
            let fetch_url = self.rewrite_url(uri).into_owned();
            if !self.http_cache.contains_key(&fetch_url) {
                out.push(Prefetch::Http {
                    url: uri.to_string(),
                    fetch_url,
                    roots: roots.clone(),
                });
            }
        } else if uri.starts_with("package://") {
            match resolve_package_uri(uri) {
                Ok(PackageSource::Direct { url, root }) => {
                    if !self.http_cache.contains_key(&url) {
                        // Loading this file registers its root, so the file's
                        // own imports resolve under it.
                        let roots = if roots.contains(&root) {
                            roots.clone()
                        } else {
                            let mut with_root = (**roots).clone();
                            with_root.push(root);
                            Rc::new(with_root)
                        };
                        out.push(Prefetch::PackageFile { url, roots });
                    }
                }
                #[cfg(feature = "package-zip")]
                Ok(PackageSource::Zip(url, entry)) => {
                    if !self.package_dirs.contains_key(&url) {
                        out.push(Prefetch::PackageZip {
                            url,
                            entries: vec![entry],
                            roots: roots.clone(),
                        });
                    }
                }
                #[cfg(not(feature = "package-zip"))]
                Ok(PackageSource::Zip(..)) => {}
                Err(_) => {}
            }
        }
    }

    fn prefetch_levels(&mut self, mut level: Vec<Prefetch>) {
        for _ in 0..MAX_LEVELS {
            level.retain(|target| !self.prefetch.attempted.contains(&target.attempt_key()));
            let mut downloads = self.group_downloads(level);
            // Downloads past the request budget are left to evaluation.
            downloads.truncate(self.prefetch.requests);
            if downloads.is_empty() {
                break;
            }
            // Sources fetched (or found in the package cache) at this level,
            // with the path evaluation gives them, to scan for the next level.
            let mut sources = Scanned::new();
            for download in &mut downloads {
                for target in &download.targets {
                    self.prefetch.attempted.insert(target.attempt_key());
                }
                // A package already in the persistent cache needs no network.
                let targets = std::mem::take(&mut download.targets);
                for target in targets {
                    if !self.load_cached_package(&target, &mut sources) {
                        download.targets.push(target);
                    }
                }
            }
            downloads.retain(|download| !download.targets.is_empty());
            let (text, bytes): (Vec<Download>, Vec<Download>) =
                downloads.into_iter().partition(Download::is_text);

            if !text.is_empty() {
                let urls: Vec<String> = text.iter().map(|d| d.fetch_url.clone()).collect();
                self.prefetch.requests -= urls.len();
                let results = self
                    .capabilities
                    .fetch_text_many(&urls, &self.prefetch.bytes);
                for (download, result) in text.into_iter().zip(results) {
                    if let Ok(body) = result {
                        let mut targets = download.targets;
                        let last = targets.pop();
                        for target in targets {
                            self.store_text(target, body.clone(), &mut sources);
                        }
                        if let Some(target) = last {
                            self.store_text(target, body, &mut sources);
                        }
                    }
                }
            }

            if !bytes.is_empty() {
                let urls: Vec<String> = bytes.iter().map(|d| d.fetch_url.clone()).collect();
                self.prefetch.requests -= urls.len();
                let results = self
                    .capabilities
                    .fetch_bytes_many(&urls, &self.prefetch.bytes);
                for (download, result) in bytes.into_iter().zip(results) {
                    if let Ok(fetched) = result {
                        for target in download.targets {
                            self.store_bytes(target, &fetched, &mut sources);
                        }
                    }
                }
            }

            if self.prefetch.exhausted() {
                break;
            }
            let mut next = Vec::new();
            for (imports, source_path, roots) in &sources {
                for uri in imports {
                    self.prefetch_target(uri, Path::new(source_path), roots, &mut next);
                }
            }
            level = next;
        }
    }

    /// Group targets by the URL actually requested, merging the entries of
    /// one package archive, so each URL is downloaded once per level.
    fn group_downloads(&self, level: Vec<Prefetch>) -> Vec<Download> {
        let mut index_by_url: HashMap<String, usize> = HashMap::default();
        let mut downloads: Vec<Download> = Vec::new();
        for target in level {
            let fetch_url = match &target {
                Prefetch::Http { fetch_url, .. } => fetch_url.clone(),
                Prefetch::PackageFile { url, .. } => self.rewrite_url(url).into_owned(),
                #[cfg(feature = "package-zip")]
                Prefetch::PackageZip { url, .. } => self.rewrite_url(url).into_owned(),
            };
            let index = *index_by_url.entry(fetch_url.clone()).or_insert_with(|| {
                downloads.push(Download {
                    fetch_url,
                    targets: Vec::new(),
                });
                downloads.len() - 1
            });
            let targets = &mut downloads[index].targets;
            if let Some(existing) = targets
                .iter_mut()
                .find(|existing| existing.attempt_key() == target.attempt_key())
            {
                #[cfg(feature = "package-zip")]
                if let (
                    Prefetch::PackageZip { entries, .. },
                    Prefetch::PackageZip { entries: more, .. },
                ) = (existing, target)
                {
                    for entry in more {
                        if !entries.contains(&entry) {
                            entries.push(entry);
                        }
                    }
                }
                #[cfg(not(feature = "package-zip"))]
                let _ = existing;
            } else {
                targets.push(target);
            }
        }
        downloads
    }

    /// Load `target` from the persistent package cache. Returns whether it
    /// was a cached package, which then needs no request.
    fn load_cached_package(&mut self, target: &Prefetch, sources: &mut Scanned) -> bool {
        match target {
            Prefetch::Http { .. } => false,
            Prefetch::PackageFile { url, roots } => match self.cached_package(url, "pkl") {
                Some(cached) => {
                    if let Ok(source) = std::str::from_utf8(&cached) {
                        scan(sources, source, url.clone(), roots.clone());
                    }
                    true
                }
                None => false,
            },
            #[cfg(feature = "package-zip")]
            Prefetch::PackageZip {
                url,
                entries,
                roots,
            } => {
                if self.cached_package(url, "zip").is_none() {
                    return false;
                }
                // Extracting a cached archive needs no network.
                if let Ok(dir) = self.extract_package_zip(url) {
                    self.push_package_entries(&dir, entries, roots, sources);
                }
                true
            }
        }
    }

    /// Cache a plain HTTP module's text, as `fetch_source` would.
    fn store_text(&mut self, target: Prefetch, body: String, sources: &mut Scanned) {
        if let Prefetch::Http {
            url,
            fetch_url,
            roots,
        } = target
        {
            // Scan first so the body can move into the cache uncopied.
            scan(sources, &body, url, roots);
            self.http_cache.insert(fetch_url, body);
        }
    }

    /// Do `target`'s cache work with downloaded `bytes`, as evaluation would
    /// after fetching it.
    fn store_bytes(&mut self, target: Prefetch, bytes: &[u8], sources: &mut Scanned) {
        match target {
            Prefetch::Http { .. } => {
                // `fetch_text` rejects bodies that are not UTF-8; leave those
                // to evaluation so it reports the same error.
                if let Ok(body) = std::str::from_utf8(bytes) {
                    self.store_text(target, body.to_string(), sources);
                }
            }
            Prefetch::PackageFile { url, roots } => {
                if validate_package_bytes(&url, "pkl", bytes).is_err() {
                    return;
                }
                let _ = self.write_package_cache(&url, "pkl", bytes);
                if let Ok(source) = std::str::from_utf8(bytes) {
                    scan(sources, source, url.clone(), roots);
                    self.http_cache.insert(url, source.to_string());
                }
            }
            #[cfg(feature = "package-zip")]
            Prefetch::PackageZip {
                url,
                entries,
                roots,
            } => {
                if validate_package_bytes(&url, "zip", bytes).is_err() {
                    return;
                }
                let _ = self.write_package_cache(&url, "zip", bytes);
                let prefix = format!("pklr-pkg-{}", self.package_dirs.len());
                let Ok(dir) = self.temp_dir_io(&prefix) else {
                    return;
                };
                if self.extract_zip_io(bytes.to_vec(), &dir).is_err() {
                    return;
                }
                self.package_dirs.insert(url, dir.clone());
                self.push_package_entries(&dir, &entries, &roots, sources);
            }
        }
    }

    /// The valid persistent-cache copy of a package, if there is one.
    fn cached_package(&mut self, url: &str, extension: &str) -> Option<Vec<u8>> {
        let cached = self.read_package_cache(url, extension).ok()??;
        validate_package_bytes(url, extension, &cached).ok()?;
        Some(cached)
    }

    #[cfg(feature = "package-zip")]
    fn push_package_entries(
        &mut self,
        dir: &Path,
        entries: &[String],
        roots: &Roots,
        sources: &mut Scanned,
    ) {
        for entry in entries {
            let path = dir.join(entry);
            if let Ok(source) = self.read_to_string_io(&path) {
                scan(sources, &source, path.display().to_string(), roots.clone());
            }
        }
    }
}
