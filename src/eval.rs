use std::collections::BTreeMap;

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use crate::capabilities::EvalCapabilities;
use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{
    self, BinOp, Body, Entry, Expr, Modifier, Module, Property, StringInterpPart, UnOp,
};
use crate::value::{
    CapturedScope, ListKind, ListValue, NameSet, ObjectKind, ObjectMap, ObjectSource,
    PoisonedMember, ScopeMap, TypeAliasMap, Value,
};

mod analysis;
mod glob;
mod mapping;
mod package;
mod prefetch;
mod remote;
mod scope;
mod types;

use analysis::*;
pub use glob::expand_glob;
use glob::*;
use mapping::*;
pub(crate) use package::write_atomic;
use package::*;
use remote::*;
pub(crate) use remote::{parse_triple_dot_path, resolve_triple_dot};
pub(crate) use scope::SourceScope;
use scope::*;
use types::*;

/// The error a cancelled evaluation fails with.
fn cancelled() -> Error {
    Error::Eval("evaluation cancelled".to_string())
}

const DYNAMIC_SIBLING_REF: &str = "\0pklr:dynamic-sibling";

/// Evaluates pkl source files to [`Value`].
pub struct Evaluator {
    base_path: PathBuf,
    /// Maximum import depth to prevent infinite recursion
    max_depth: usize,
    /// Cache for fetched HTTP sources (URL → source text)
    http_cache: HashMap<String, String>,
    /// Cache for evaluated local imports (canonical path → Value)
    import_cache: HashMap<PathBuf, Value>,
    /// Local imports whose `import_cache` entry is still the placeholder that
    /// breaks circular imports.
    imports_in_flight: HashSet<PathBuf>,
    /// Number of times evaluation saw an in-flight placeholder instead of a
    /// module's real value. A result computed while this moved may be
    /// incomplete and is not cached.
    placeholder_reads: u64,
    /// Narrowed evaluations of local imports, keyed by canonical path and the
    /// sorted requested fields.
    narrowed_import_cache: HashMap<(PathBuf, Vec<String>), Value>,
    /// Parsed local modules (canonical path → AST)
    parse_cache: HashMap<PathBuf, Arc<Module>>,
    /// `referenced_roots` of object bodies amended in this run, keyed by the
    /// body's address. Each entry holds its body so the address stays unique.
    body_roots_cache: HashMap<usize, (crate::parser::Body, Arc<HashSet<String>>)>,
    /// Dependency-order plans for module bodies evaluated in this run, keyed
    /// like `body_roots_cache`. A narrowed import can evaluate one body more
    /// than once, but its dependency graph is stable.
    plan_cache: HashMap<usize, (crate::parser::Body, Arc<EvaluationPlan>)>,
    /// Resources read in this run, by URI. As in Pkl, reading a resource again
    /// returns the first result, so reads are deterministic.
    resource_cache: HashMap<String, Value>,
    /// Final scopes for modules evaluated in this run, used to preserve inherited locals.
    module_scopes: HashMap<PathBuf, ModuleScopeSnapshot>,
    /// Whether a module evaluated in this run has a failed property that
    /// rendering it would output (see `PoisonedMember::rendered`), so the
    /// output must be checked for one.
    rendered_member_failed: bool,
    /// Environment variables read during evaluation (name → observed value).
    env_reads: BTreeMap<String, Option<String>>,
    /// Local files currently being evaluated with inherited scope.
    scoped_imports_in_flight: HashSet<PathBuf>,
    /// Host-provided IO for files, environment, HTTP, packages, and globs.
    capabilities: Box<dyn EvalCapabilities>,
    /// Set from another thread to stop the evaluation (see
    /// [`Evaluator::set_cancel_flag`]).
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Extracted package zip directories (zip URL → temp dir path)
    #[cfg(feature = "package-zip")]
    package_dirs: HashMap<String, PathBuf>,
    /// Directory used to persist downloaded package content across evaluators.
    package_cache_dir: Option<PathBuf>,
    /// HTTP URL roots whose sources belong to direct-download packages.
    package_http_roots: HashSet<String>,
    /// Prefetch progress and budget for the current evaluation.
    prefetch: prefetch::PrefetchState,
    /// Whether network access is disabled for this evaluator.
    offline: bool,
    /// HTTP URL rewrite rules (source_prefix → target_prefix).
    /// Longest matching prefix wins.
    http_rewrites: Vec<(String, String)>,
    /// Converters extracted from `output.renderer.converters`.
    /// Each entry maps a class name to a converter lambda.
    converters: Vec<(String, Value)>,
    /// Set of (property_name, message) pairs already warned about.
    /// Used to deduplicate `@Deprecated` warnings so a deprecated property
    /// referenced inside a loop or template doesn't flood stderr.
    warned_deprecated: std::collections::HashSet<(String, Option<String>)>,
}

#[derive(Clone, Default)]
struct MappingInheritedDefault {
    value: Option<Value>,
    entries: Option<crate::parser::Body>,
}

#[derive(Clone, Default)]
struct ModuleScopeSnapshot {
    values: ScopeMap,
    type_aliases: TypeAliasMap,
    late_properties: Vec<Arc<Property>>,
}

/// The error of the first failed member rendering `value` would output: a
/// failed property of a module object it contains.
fn rendered_member_failure(value: &Value) -> Option<&str> {
    match value {
        Value::Object(map, source) => source
            .as_ref()
            .and_then(|source| source.poisoned_members.as_ref())
            .and_then(|members| members.values().find(|member| member.rendered))
            .map(|member| member.message.as_str())
            .or_else(|| map.values().find_map(rendered_member_failure)),
        Value::List(items) => items.iter().find_map(rendered_member_failure),
        _ => None,
    }
}

/// The value of a literal or a plain name, or `None` for any expression that
/// needs the full evaluator. Must agree with `Evaluator::eval_expr_boxed`.
fn eval_simple_expr(
    expr: &Expr,
    scope: &Scope,
    depth: usize,
    max_depth: usize,
) -> Option<Result<Value>> {
    if depth > max_depth {
        return None;
    }
    Some(match expr {
        Expr::Null => Ok(Value::Null),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Int(n) => Ok(Value::Int(*n)),
        Expr::Float(f) => Ok(Value::Float(*f)),
        Expr::String(s) => Ok(Value::String(Arc::clone(s))),
        Expr::Ident(name) => scope.get(name).cloned().ok_or_else(|| {
            Error::Eval(
                scope
                    .poison_of(name)
                    .cloned()
                    .unwrap_or_else(|| format!("undefined variable: {name}")),
            )
        }),
        // An operator over simple operands, other than `|>` (a call) and an
        // object-body amendment, which need the evaluator. `&&`, `||` keep
        // short-circuiting; the depth mirrors `eval_binop`'s `depth + 1`.
        Expr::Binop(op, left, right)
            if !matches!(op, BinOp::Pipe)
                && !(matches!(op, BinOp::Add) && matches!(right.as_ref(), Expr::ObjectBody(_))) =>
        {
            if !is_simple_expr(left) || !is_simple_expr(right) {
                return None;
            }
            let l = match eval_simple_expr(left, scope, depth + 1, max_depth)? {
                Ok(l) => l,
                Err(error) => return Some(Err(error)),
            };
            if matches!(op, BinOp::And | BinOp::Or) {
                let left_truthy = is_truthy(&l);
                let short_circuit = match op {
                    BinOp::And => !left_truthy,
                    _ => left_truthy,
                };
                if short_circuit {
                    return Some(Ok(Value::Bool(left_truthy)));
                }
                return Some(
                    eval_simple_expr(right, scope, depth + 1, max_depth)?
                        .map(|r| Value::Bool(is_truthy(&r))),
                );
            }
            let r = match eval_simple_expr(right, scope, depth + 1, max_depth)? {
                Ok(r) => r,
                Err(error) => return Some(Err(error)),
            };
            apply_binop(*op, l, r)
        }
        _ => return None,
    })
}

/// Whether `eval_simple_expr` can evaluate `expr` without the evaluator.
fn is_simple_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Ident(_) => true,
        Expr::Binop(op, left, right) => {
            !matches!(op, BinOp::Pipe)
                && !(matches!(op, BinOp::Add) && matches!(right.as_ref(), Expr::ObjectBody(_)))
                && is_simple_expr(left)
                && is_simple_expr(right)
        }
        _ => false,
    }
}

/// Apply a binary operator other than `|>` to evaluated operands.
fn apply_binop(op: BinOp, l: Value, r: Value) -> Result<Value> {
    match op {
        BinOp::Add => add_values(l, r),
        BinOp::Sub => arithmetic(l, r, |a, b| Ok(a - b), |a, b| Ok(a - b)),
        BinOp::Mul => arithmetic(l, r, |a, b| Ok(a * b), |a, b| Ok(a * b)),
        BinOp::Div => arithmetic(
            l,
            r,
            |a, b| {
                if b == 0 {
                    Err(Error::Eval("division by zero".into()))
                } else {
                    Ok(a / b)
                }
            },
            |a, b| Ok(a / b),
        ),
        BinOp::Mod => arithmetic(
            l,
            r,
            |a, b| {
                if b == 0 {
                    Err(Error::Eval("modulo by zero".into()))
                } else {
                    Ok(a % b)
                }
            },
            |a, b| Ok(a % b),
        ),
        BinOp::Eq => Ok(Value::Bool(values_eq(&l, &r))),
        BinOp::Ne => Ok(Value::Bool(!values_eq(&l, &r))),
        BinOp::Lt => compare(l, r, std::cmp::Ordering::Less),
        BinOp::Le => compare_or_eq(l, r, std::cmp::Ordering::Less),
        BinOp::Gt => compare(l, r, std::cmp::Ordering::Greater),
        BinOp::Ge => compare_or_eq(l, r, std::cmp::Ordering::Greater),
        BinOp::And => Ok(Value::Bool(is_truthy(&l) && is_truthy(&r))),
        BinOp::Or => Ok(Value::Bool(is_truthy(&l) || is_truthy(&r))),
        BinOp::IntDiv => arithmetic(
            l,
            r,
            |a, b| {
                if b == 0 {
                    Err(Error::Eval("division by zero".into()))
                } else {
                    Ok(a / b)
                }
            },
            |a, b| Ok((a / b).floor()),
        ),
        BinOp::Pow => arithmetic(
            l,
            r,
            |a, b| {
                if b < 0 {
                    Err(Error::Eval(
                        "integer exponentiation with negative exponent is not supported".into(),
                    ))
                } else {
                    Ok(a.pow(b as u32))
                }
            },
            |a, b| Ok(a.powf(b)),
        ),
        BinOp::NullCoalesce => {
            if is_null_value(&l) {
                Ok(r)
            } else {
                Ok(l)
            }
        }
        BinOp::Pipe => unreachable!("`|>` calls a function; see eval_binop"),
    }
}

fn regex_value(pattern: Value) -> Value {
    let mut map = ObjectMap::default();
    map.insert("_type".into(), Value::String("regex".into()));
    map.insert("pattern".into(), pattern);
    Value::Object(Arc::new(map), None)
}

#[cfg(feature = "native-io")]
impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

/// Access to the host capabilities.
impl Evaluator {
    fn read_to_string_io(&mut self, path: &Path) -> Result<String> {
        self.check_cancelled()?;
        self.capabilities.read_to_string(path)
    }

    fn path_exists_io(&mut self, path: &Path) -> Result<bool> {
        self.capabilities.path_exists(path)
    }

    fn canonicalize_io(&mut self, path: &Path) -> Result<PathBuf> {
        self.capabilities.canonicalize(path)
    }

    fn read_bytes_io(&mut self, path: &Path) -> Result<Vec<u8>> {
        self.capabilities.read_bytes(path)
    }

    fn create_dir_all_io(&mut self, path: &Path) -> Result<()> {
        self.capabilities.create_dir_all(path)
    }

    fn write_atomic_io(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.capabilities.write_atomic(path, bytes)
    }

    fn remove_file_io(&mut self, path: &Path) -> Result<()> {
        self.capabilities.remove_file(path)
    }

    #[cfg(feature = "package-zip")]
    fn extract_zip_io(&mut self, bytes: Vec<u8>, destination: &Path) -> Result<()> {
        self.capabilities.extract_zip(bytes, destination)
    }

    fn read_env_io(&mut self, name: &str) -> Result<Option<String>> {
        self.capabilities.read_env(name)
    }

    fn fetch_text_io(&mut self, url: &str) -> Result<String> {
        self.check_cancelled()?;
        self.capabilities.fetch_text(url)
    }

    fn fetch_bytes_io(&mut self, url: &str) -> Result<Vec<u8>> {
        self.check_cancelled()?;
        self.capabilities.fetch_bytes(url)
    }

    #[cfg(feature = "package-zip")]
    fn temp_dir_io(&mut self, prefix: &str) -> Result<PathBuf> {
        self.capabilities.temp_dir(prefix)
    }

    fn glob_io(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
        self.capabilities.glob(base, pattern)
    }
}

impl Evaluator {
    /// Construct an evaluator with the native host capabilities.
    #[cfg(feature = "native-io")]
    pub fn new() -> Self {
        Self::with_capabilities(crate::capabilities::NativeCapabilities::new())
    }

    pub fn with_capabilities(capabilities: impl EvalCapabilities + 'static) -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::default(),
            import_cache: HashMap::default(),
            imports_in_flight: HashSet::default(),
            placeholder_reads: 0,
            narrowed_import_cache: HashMap::default(),
            parse_cache: HashMap::default(),
            body_roots_cache: HashMap::default(),
            plan_cache: HashMap::default(),
            resource_cache: HashMap::default(),
            module_scopes: HashMap::default(),
            rendered_member_failed: false,
            env_reads: BTreeMap::new(),
            scoped_imports_in_flight: HashSet::default(),
            capabilities: Box::new(capabilities),
            cancel: None,
            #[cfg(feature = "package-zip")]
            package_dirs: HashMap::default(),
            package_cache_dir: None,
            package_http_roots: HashSet::default(),
            prefetch: prefetch::PrefetchState::default(),
            offline: false,
            http_rewrites: Vec::new(),
            converters: Vec::new(),
            warned_deprecated: std::collections::HashSet::default(),
        }
    }

    pub fn set_base_path(&mut self, path: &Path) {
        self.base_path = path.to_path_buf();
    }

    /// The local file a `file:` or relative module URI in the module at
    /// `current_path` names.
    fn local_file_path(&mut self, current_path: &Path, uri: &str) -> Result<PathBuf> {
        match uri.strip_prefix("file://") {
            Some(path) => Ok(PathBuf::from(path)),
            None => self.resolve_local_path(current_path, uri),
        }
    }

    /// Resolve a local module or resource URI against the module at
    /// `current_path`. A triple-dot URI is searched for upward from the
    /// module's directory (see [`Self::resolve_triple_dot`]); a path that
    /// cannot be found resolves to a path that does not exist.
    fn resolve_local_path(&mut self, current_path: &Path, uri: &str) -> Result<PathBuf> {
        if let Some(triple_dot) = parse_triple_dot_path(uri)? {
            return Ok(self
                .resolve_triple_dot(current_path, triple_dot)?
                .unwrap_or_else(|| PathBuf::from(uri)));
        }
        Ok(current_path.parent().unwrap_or(Path::new(".")).join(uri))
    }

    /// Resolve the triple-dot `path` for the module at `current_path`.
    /// Inside an extracted package the search stops at the package root.
    fn resolve_triple_dot(&mut self, current_path: &Path, path: &str) -> Result<Option<PathBuf>> {
        #[cfg(feature = "package-zip")]
        let root = self
            .package_dirs
            .values()
            .find(|root| current_path.starts_with(root))
            .cloned();
        #[cfg(not(feature = "package-zip"))]
        let root: Option<PathBuf> = None;
        // Canonicalize through the evaluator's capabilities, which resolves
        // native relative entry paths and supports virtual path namespaces.
        // Unlike `std::path::absolute`, this never substitutes the host CWD
        // for a capability-backed evaluator.
        let current_path = self
            .canonicalize_io(current_path)
            .unwrap_or_else(|_| current_path.to_path_buf());
        resolve_triple_dot(&current_path, path, root.as_deref(), |candidate| {
            self.path_exists_io(candidate)
        })
    }

    fn module_type_namespace(&mut self, path: &Path) -> String {
        if path.to_string_lossy().contains("://") {
            return path.display().to_string();
        }
        self.canonicalize_io(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()
    }

    /// Persist downloaded package content under `path`.
    pub fn set_package_cache_dir(&mut self, path: impl Into<PathBuf>) {
        self.package_cache_dir = Some(path.into());
    }

    /// Disable network access. Package imports can still use the persistent cache.
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    /// Stop evaluating, with an "evaluation cancelled" error, once `flag` is
    /// set, for example from another thread after a timeout. The evaluator
    /// checks it before each expression, module read and fetch, and starts
    /// no further prefetch requests once it is set.
    pub fn set_cancel_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.cancel = Some(flag);
    }

    /// Fail if the cancel flag is set. A single `Option` test when there is
    /// none.
    #[inline]
    fn check_cancelled(&self) -> Result<()> {
        match &self.cancel {
            Some(flag) if flag.load(std::sync::atomic::Ordering::Relaxed) => Err(cancelled()),
            _ => Ok(()),
        }
    }

    /// Return the environment variables observed by the latest evaluation.
    ///
    /// Missing variables are included with a `None` value. Entries are ordered
    /// by variable name so callers can serialize or hash them deterministically.
    pub fn env_reads(&self) -> &BTreeMap<String, Option<String>> {
        &self.env_reads
    }

    #[cfg(feature = "native-io")]
    pub(crate) fn take_env_reads(&mut self) -> BTreeMap<String, Option<String>> {
        std::mem::take(&mut self.env_reads)
    }

    fn begin_evaluation(&mut self) {
        self.env_reads.clear();
        self.import_cache.clear();
        self.imports_in_flight.clear();
        self.placeholder_reads = 0;
        self.narrowed_import_cache.clear();
        self.parse_cache.clear();
        self.body_roots_cache.clear();
        self.plan_cache.clear();
        self.resource_cache.clear();
        clear_names();
        self.module_scopes.clear();
        self.rendered_member_failed = false;
        self.scoped_imports_in_flight.clear();
        self.converters.clear();
        self.prefetch = prefetch::PrefetchState::new(self.cancel.clone());
    }

    /// Add HTTP URL rewrite rules. Each rule is a `"source_prefix=target_prefix"` string
    /// (matching pkl CLI's `--http-rewrite` format). When a URL matches a source prefix,
    /// the prefix is replaced with the target. Longest matching prefix wins.
    pub fn set_http_rewrites(&mut self, rules: &[String]) {
        self.http_rewrites = rules
            .iter()
            .filter_map(|rule| {
                let Some((src, tgt)) = rule.split_once('=') else {
                    eprintln!("pklr: ignoring malformed rewrite rule (missing '='): {rule}");
                    return None;
                };
                if src.is_empty() {
                    eprintln!("pklr: ignoring rewrite rule with empty source prefix: {rule}");
                    return None;
                }
                Some((src.to_string(), tgt.to_string()))
            })
            .collect();
    }

    /// If `field` is marked `@Deprecated` in `source`, emit the warning at
    /// most once per (field, message) pair. Called from field-access
    /// expressions so the warning fires when a deprecated property is *used*,
    /// not when its containing module loads. Per-call dedup mirrors pkl-jvm,
    /// which avoids flooding stderr when a deprecated property is referenced
    /// inside a loop or template.
    fn warn_if_deprecated_access(&mut self, source: &Option<Arc<ObjectSource>>, field: &str) {
        let Some(src) = source else { return };
        let Some(message) = src.deprecated.get(field) else {
            return;
        };
        let key = (field.to_string(), message.clone());
        if !self.warned_deprecated.insert(key) {
            return;
        }
        if let Some(msg) = message {
            eprintln!("[pklr] WARNING: property '{field}' is deprecated: {msg}");
        } else {
            eprintln!("[pklr] WARNING: property '{field}' is deprecated");
        }
    }

    /// Apply rewrite rules to a URL. Returns the rewritten URL or the original.
    pub fn rewrite_url<'a>(&self, url: &'a str) -> std::borrow::Cow<'a, str> {
        if self.http_rewrites.is_empty() {
            return std::borrow::Cow::Borrowed(url);
        }
        // Find the longest matching prefix
        let best = self
            .http_rewrites
            .iter()
            .filter(|(src, _)| url.starts_with(src.as_str()))
            .max_by_key(|(src, _)| src.len());
        match best {
            Some((src, tgt)) => std::borrow::Cow::Owned(format!("{}{}", tgt, &url[src.len()..])),
            None => std::borrow::Cow::Borrowed(url),
        }
    }

    /// Read a resource by URI, reusing an earlier read of the same URI.
    fn read_resource(&mut self, uri: &str) -> Result<Value> {
        if let Some(value) = self.resource_cache.get(uri) {
            return Ok(value.clone());
        }
        let value = self.read_resource_uncached(uri)?;
        self.resource_cache.insert(uri.to_string(), value.clone());
        Ok(value)
    }

    /// Read a resource by URI scheme.
    fn read_resource_uncached(&mut self, uri: &str) -> Result<Value> {
        if let Some(path) = uri.strip_prefix("file://") {
            // file:// — read local file
            let content = self.read_to_string_io(Path::new(path))?;
            Ok(Value::String(content.into()))
        } else if let Some(var_name) = uri.strip_prefix("env:") {
            // env: — read environment variable
            let value = self.read_env_io(var_name)?;
            self.env_reads.insert(var_name.to_string(), value.clone());
            let Some(val) = value else {
                return Err(Error::Eval(format!(
                    "environment variable not found: {var_name}"
                )));
            };
            Ok(Value::String(val.into()))
        } else if let Some(prop_name) = uri.strip_prefix("prop:") {
            // prop: — system properties (not standard in Rust, return empty)
            Err(Error::Eval(format!(
                "system property not available: {prop_name}"
            )))
        } else if uri.starts_with("https://") || uri.starts_with("http://") {
            // HTTP/HTTPS
            let content = self.fetch_source(uri)?;
            Ok(Value::String(content.into()))
        } else {
            // Bare path — treat as file relative to base_path
            let file_path = self.base_path.join(uri);
            let content = self.read_to_string_io(&file_path)?;
            Ok(Value::String(content.into()))
        }
    }

    fn fetch_source(&mut self, url: &str) -> Result<String> {
        if self
            .package_http_roots
            .iter()
            .any(|root| url.starts_with(root))
        {
            return self.fetch_package_source(url);
        }
        let rewritten = self.rewrite_url(url);
        let fetch_url = rewritten.as_ref();
        if let Some(cached) = self.http_cache.get(fetch_url) {
            return Ok(cached.clone());
        }
        if self.offline {
            return Err(Error::Eval(format!(
                "offline mode prevented HTTP fetch for {url}"
            )));
        }
        let body = self.fetch_text_io(fetch_url)?;
        self.http_cache.insert(fetch_url.to_string(), body.clone());
        Ok(body)
    }

    /// The names the modules `module` amends or extends (transitively) read,
    /// adding to `type_names` the type aliases and classes they declare.
    fn inherited_reference_roots(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
        type_names: &mut HashSet<String>,
    ) -> Result<HashSet<String>> {
        let mut refs = HashSet::default();
        if depth > self.max_depth {
            return Ok(refs);
        }
        for uri in [module.amends.as_deref(), module.extends.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some((base_module, source_path)) = self.load_parsed_module(uri, path)? {
                refs.extend(referenced_roots(&base_module.body));
                type_names.extend(base_module.body.iter().filter_map(|entry| match entry {
                    Entry::TypeAlias(name, _) | Entry::ClassDef(name, ..) => Some(name.clone()),
                    _ => None,
                }));
                refs.extend(self.inherited_reference_roots(
                    &base_module,
                    Path::new(&source_path),
                    depth + 1,
                    type_names,
                )?);
            }
        }
        Ok(refs)
    }

    fn load_module_source(&mut self, uri: &str, path: &Path) -> Result<Option<(String, String)>> {
        let resolved = resolve_remote_relative(path, uri);
        let uri = resolved.as_deref().unwrap_or(uri);
        if uri.starts_with("https://") || uri.starts_with("http://") {
            let source = self.fetch_source(uri)?;
            return Ok(Some((source, uri.to_string())));
        }
        if uri.starts_with("package://") {
            let pkg = resolve_package_uri(uri)?;
            match &pkg {
                PackageSource::Direct { url, root } => {
                    let source = self.fetch_direct_package_source(url, root)?;
                    return Ok(Some((source, url.clone())));
                }
                PackageSource::Zip(zip_url, entry) => {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(entry);
                        let source = self.read_to_string_io(&local_path)?;
                        return Ok(Some((source, local_path.display().to_string())));
                    }
                    #[cfg(not(feature = "package-zip"))]
                    {
                        let _ = entry;
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                }
            }
        }
        if uri.starts_with("pkl:") || (uri.contains("://") && !uri.starts_with("file://")) {
            return Ok(None);
        }
        let import_path = self.local_file_path(path, uri)?;
        if !self.path_exists_io(&import_path)? {
            return Ok(None);
        }
        let source = self.read_to_string_io(&import_path)?;
        Ok(Some((source, import_path.display().to_string())))
    }

    /// Load and parse the module `uri` names, sharing the parse cache with
    /// evaluation so a base module is not lexed and parsed again for each
    /// analysis. `None` when the module is absent or does not parse.
    fn load_parsed_module(
        &mut self,
        uri: &str,
        path: &Path,
    ) -> Result<Option<(Arc<Module>, String)>> {
        let Some((source, source_path)) = self.load_module_source(uri, path)? else {
            return Ok(None);
        };
        // Remote sources are keyed by URL, which never collides with an
        // absolute local path.
        let key = if source_path.contains("://") {
            Some(PathBuf::from(&source_path))
        } else {
            self.canonicalize_io(Path::new(&source_path)).ok()
        };
        if let Some(module) = key.as_ref().and_then(|key| self.parse_cache.get(key)) {
            return Ok(Some((Arc::clone(module), source_path)));
        }
        let Ok(tokens) = lexer::lex_named(&source, &source_path) else {
            return Ok(None);
        };
        let Ok(module) = parser::parse_named(&tokens, &source, &source_path) else {
            return Ok(None);
        };
        let module = Arc::new(module);
        if let Some(key) = key {
            self.parse_cache.insert(key, Arc::clone(&module));
        }
        Ok(Some((module, source_path)))
    }

    fn fetch_package_source(&mut self, url: &str) -> Result<String> {
        if let Some(cached) = self.http_cache.get(url) {
            return Ok(cached.clone());
        }
        let bytes = self.fetch_package_bytes(url, "pkl")?;
        let source = String::from_utf8(bytes)
            .map_err(|error| Error::Eval(format!("package source is not UTF-8: {url}: {error}")))?;
        self.http_cache.insert(url.to_string(), source.clone());
        Ok(source)
    }

    fn fetch_direct_package_source(&mut self, url: &str, root: &str) -> Result<String> {
        self.package_http_roots.insert(root.to_string());
        self.fetch_package_source(url)
    }

    fn fetch_package_bytes(&mut self, url: &str, extension: &str) -> Result<Vec<u8>> {
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

    fn read_package_cache(&mut self, url: &str, extension: &str) -> Result<Option<Vec<u8>>> {
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
    fn write_package_cache(&mut self, url: &str, extension: &str, bytes: &[u8]) -> Result<()> {
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

    fn remove_package_cache(&mut self, url: &str, extension: &str) {
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
    fn extract_package_zip(&mut self, zip_url: &str) -> Result<PathBuf> {
        // Check if already extracted
        if let Some(dir) = self.package_dirs.get(zip_url) {
            return Ok(dir.clone());
        }
        let bytes = self.fetch_package_bytes(zip_url, "zip")?;
        let prefix = format!("pklr-pkg-{}", self.package_dirs.len());
        let dir = self.temp_dir_io(&prefix)?;
        self.extract_zip_io(bytes, &dir)?;
        self.package_dirs.insert(zip_url.to_string(), dir.clone());
        Ok(dir)
    }

    #[cfg(all(test, feature = "package-zip"))]
    fn package_dir_for_zip(&self, zip_url: &str) -> Option<&PathBuf> {
        self.package_dirs.get(zip_url)
    }

    /// Evaluate `source` as the module at `path`.
    pub fn eval_source(&mut self, source: &str, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        self.eval_source_inner(source, path)
    }

    fn eval_source_inner(&mut self, source: &str, path: &Path) -> Result<Value> {
        // Seed import cache for the entry file so circular back-references work.
        // Mark it in flight too, so a narrowed import that reads this
        // placeholder is not cached.
        let canonical = self.canonicalize_io(path).ok();
        if let Some(canonical) = &canonical {
            self.import_cache
                .insert(canonical.clone(), Value::Object(Arc::default(), None));
            self.imports_in_flight.insert(canonical.clone());
        }
        let result = self.eval_entry_module(source, path);
        if let Some(canonical) = canonical {
            self.imports_in_flight.remove(&canonical);
            // Update cache with real value
            if let Ok(val) = &result {
                self.import_cache.insert(canonical, val.clone());
            }
        }
        result
    }

    fn eval_entry_module(&mut self, source: &str, path: &Path) -> Result<Value> {
        let name = path.display().to_string();
        let tokens = lexer::lex_named(source, &name)?;
        let module = parser::parse_named(&tokens, source, &name)?;
        self.eval_module(&module, path, 0)
    }

    /// Evaluate a local pkl file and apply its output converters.
    #[cfg(feature = "native-io")]
    pub(crate) fn eval_file_converted(&mut self, path: &Path) -> Result<Value> {
        let value = self.eval_file(path)?;
        self.apply_converters(value)
    }

    /// Evaluate a local pkl file by path.
    pub fn eval_file(&mut self, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        let source = self.read_to_string_io(path)?;
        self.eval_source_inner(&source, path)
    }

    /// Read, lex, parse, and evaluate a local file (with caching).
    /// Inserts a placeholder before evaluation to break circular imports.
    fn eval_import_file(&mut self, path: &Path, depth: usize) -> Result<Value> {
        let canonical = self.canonicalize_io(path)?;
        if let Some(cached) = self.cached_import(&canonical) {
            return Ok(cached);
        }
        // Insert empty placeholder to break circular imports
        self.import_cache
            .insert(canonical.clone(), Value::Object(Arc::default(), None));
        self.imports_in_flight.insert(canonical.clone());
        let result = self.eval_file_inner(path, &canonical, depth);
        self.imports_in_flight.remove(&canonical);
        if result.is_err() {
            // Remove stale placeholder on failure so retries can re-evaluate
            self.import_cache.remove(&canonical);
        }
        result
    }

    fn eval_file_with_requested_fields(
        &mut self,
        path: &Path,
        depth: usize,
        requested_fields: Option<HashSet<String>>,
    ) -> Result<Value> {
        if requested_fields.is_none() {
            return self.eval_import_file(path, depth);
        }
        let canonical = self.canonicalize_io(path)?;
        if let Some(cached) = self.cached_import(&canonical) {
            return Ok(cached);
        }
        let mut fields: Vec<String> = requested_fields.iter().flatten().cloned().collect();
        fields.sort_unstable();
        let key = (canonical.clone(), fields);
        if let Some(cached) = self.narrowed_import_cache.get(&key) {
            return Ok(cached.clone());
        }
        self.import_cache
            .insert(canonical.clone(), Value::Object(Arc::default(), None));
        self.imports_in_flight.insert(canonical.clone());
        let placeholder_reads = self.placeholder_reads;
        let result = self.eval_file_requested_fields_inner(path, depth, requested_fields);
        self.imports_in_flight.remove(&canonical);
        self.import_cache.remove(&canonical);
        // A module evaluated while one of its imports was still in flight may
        // have seen that import's placeholder, so only cache self-contained
        // results.
        if let Ok(value) = &result
            && self.placeholder_reads == placeholder_reads
        {
            self.narrowed_import_cache.insert(key, value.clone());
        }
        result
    }

    /// The cached value of a local import, counting reads of an in-flight
    /// placeholder.
    fn cached_import(&mut self, canonical: &Path) -> Option<Value> {
        let cached = self.import_cache.get(canonical)?.clone();
        if self.imports_in_flight.contains(canonical) {
            self.placeholder_reads += 1;
        }
        Some(cached)
    }

    fn eval_file_requested_fields_inner(
        &mut self,
        path: &Path,
        depth: usize,
        requested_fields: Option<HashSet<String>>,
    ) -> Result<Value> {
        let module = self.parse_file(path)?;
        self.eval_module_with_scope(&module, path, depth, None, requested_fields)
    }

    fn eval_file_inner(&mut self, path: &Path, canonical: &Path, depth: usize) -> Result<Value> {
        let val = self.eval_file_inner_with_scope(path, depth, None)?;
        self.import_cache
            .insert(canonical.to_path_buf(), val.clone());
        Ok(val)
    }

    fn eval_file_with_scope(
        &mut self,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
    ) -> Result<Value> {
        if inherited_scope.is_none() {
            return self.eval_import_file(path, depth);
        }
        let canonical = self.canonicalize_io(path)?;
        if !self.scoped_imports_in_flight.insert(canonical.clone()) {
            self.placeholder_reads += 1;
            return Ok(Value::Object(Arc::default(), None));
        }
        let result = self.eval_file_inner_with_scope(path, depth, inherited_scope);
        self.scoped_imports_in_flight.remove(&canonical);
        result
    }

    fn eval_file_inner_with_scope(
        &mut self,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
    ) -> Result<Value> {
        let module = self.parse_file(path)?;
        let val = self.eval_module_with_scope(&module, path, depth, inherited_scope, None)?;
        Ok(val)
    }

    fn parse_file(&mut self, path: &Path) -> Result<Arc<Module>> {
        let canonical = self.canonicalize_io(path).ok();
        if let Some(module) = canonical.as_ref().and_then(|c| self.parse_cache.get(c)) {
            return Ok(Arc::clone(module));
        }
        let source = self.read_to_string_io(path)?;
        let name = path.display().to_string();
        let tokens = lexer::lex_named(&source, &name)?;
        let module = Arc::new(parser::parse_named(&tokens, &source, &name)?);
        if let Some(canonical) = canonical {
            self.parse_cache.insert(canonical, Arc::clone(&module));
        }
        Ok(module)
    }

    /// The name pkl gives the class of a module that amends another: the
    /// name of the module at the root of its amends chain.
    fn amended_module_name(&mut self, module: &Module, path: &Path) -> String {
        let mut name = module_display_name(module, path);
        let mut current = (module.amends.clone(), path.to_path_buf());
        for _ in 0..self.max_depth {
            let (Some(uri), from) = current else { break };
            if uri.contains("://") {
                break;
            }
            let Ok(base_path) = self.resolve_local_path(&from, &uri) else {
                break;
            };
            let Ok(base) = self.parse_file(&base_path) else {
                break;
            };
            name = module_display_name(&base, &base_path);
            current = (base.amends.clone(), base_path);
        }
        name
    }

    /// Reject a module that amends or extends itself.
    fn check_not_self(
        &mut self,
        module: &Module,
        path: &Path,
        base_path: &Path,
        verb: &str,
    ) -> Result<()> {
        let same = match (self.canonicalize_io(path), self.canonicalize_io(base_path)) {
            (Ok(a), Ok(b)) => a == b,
            _ => path == base_path,
        };
        if same {
            return Err(Error::Eval(format!(
                "Module `{}` cannot {verb} itself.",
                module_display_name(module, path)
            )));
        }
        Ok(())
    }

    fn eval_module(&mut self, module: &Module, path: &Path, depth: usize) -> Result<Value> {
        self.eval_module_with_scope(module, path, depth, None, None)
    }

    fn layer_evaluated_module_scope(&self, path: &Path, scope: &mut Scope) {
        let Some(inherited) = self.module_scopes.get(path) else {
            return;
        };
        for (name, value) in &inherited.values {
            if &**name != "this" && &**name != "module" {
                scope.set_name(name.clone(), value.clone());
            }
        }
        for (name, ty) in &inherited.type_aliases {
            scope.set_type_alias(name.clone(), ty.clone());
        }
    }

    fn inherited_late_properties(&self, path: &Path) -> Vec<Arc<Property>> {
        self.module_scopes
            .get(path)
            .map(|snapshot| snapshot.late_properties.clone())
            .unwrap_or_default()
    }

    /// Resolve a glob import pattern to a mapping of matched module paths to
    /// their evaluated values. Shared by `import* "glob" as Alias` declarations
    /// and `import*("glob")` expressions.
    ///
    /// Keys are the matched paths relative to `path`'s directory, matching pkl.
    /// The enclosing module is skipped: pklr evaluates matched modules eagerly,
    /// so including it would recurse until the import depth limit.
    ///
    /// `requested` is the set of keys the importing code reads, when it only
    /// reads the mapping as `Alias["key"]`. Then only those modules are
    /// evaluated. A requested name that is not a matched key (such as `keys`
    /// or `length`) needs the whole mapping, so every module is evaluated.
    fn eval_glob_import(
        &mut self,
        uri: &str,
        path: &Path,
        depth: usize,
        requested: Option<&HashSet<String>>,
    ) -> Result<Value> {
        // Non-local glob imports resolve to an empty mapping.
        if uri.contains("://") {
            return Ok(Value::Object(Arc::default(), None));
        }
        // Match `expand_glob`, which walks `.` for a bare entry path, so the
        // matched paths share this prefix and the keys come out relative.
        let base_dir = module_dir(path.parent().unwrap_or(Path::new(".")));
        let matched = self.glob_io(base_dir, uri)?;
        let matched = matched
            .into_iter()
            .map(|matched_path| {
                let rel_key = pathdiff_or_full(&matched_path, base_dir);
                (matched_path, rel_key)
            })
            .collect::<Vec<_>>();
        let requested = requested.filter(|requested| {
            requested
                .iter()
                .all(|key| matched.iter().any(|(_, rel_key)| rel_key == key))
        });
        let mut mapping = ObjectMap::default();
        for (matched_path, rel_key) in matched {
            if requested.is_some_and(|requested| !requested.contains(&rel_key)) {
                continue;
            }
            if self.same_local_path(&matched_path, path)? {
                continue;
            }
            let val = self.eval_file_with_requested_fields(&matched_path, depth + 1, None)?;
            mapping.insert(rel_key.into(), val);
        }
        Ok(Value::Object(Arc::new(mapping), None))
    }

    /// Evaluate a single `import("uri")` expression, resolving `uri` the same way
    /// an `import "uri"` declaration in `path` would.
    ///
    /// `requested` narrows evaluation to the module properties the expression
    /// actually reads, matching how an import alias used as `Alias.field` is
    /// narrowed. `None` evaluates the whole module.
    fn eval_import_expr(
        &mut self,
        uri: &str,
        path: &Path,
        depth: usize,
        requested: Option<HashSet<String>>,
    ) -> Result<Value> {
        let resolved = resolve_remote_relative(path, uri);
        let uri: &str = resolved.as_deref().unwrap_or(uri);

        if let Some(module_name) = uri.strip_prefix("pkl:") {
            return Ok(stdlib_module(module_name));
        }

        if !uri.contains("://") || uri.starts_with("file://") {
            let import_path = self.local_file_path(path, uri)?;
            if !self.path_exists_io(&import_path)? {
                return Err(Error::ImportNotFound(import_path.display().to_string()));
            }
            return self.eval_file_with_requested_fields(&import_path, depth + 1, requested);
        }

        let Some((source, name)) = self.load_module_source(uri, path)? else {
            return Err(Error::ImportNotFound(uri.to_string()));
        };
        // Package zips extract to a local file, so evaluate those through the
        // shared import cache like any other local import.
        if !name.contains("://") {
            return self.eval_file_with_requested_fields(Path::new(&name), depth + 1, requested);
        }
        let tokens = lexer::lex_named(&source, &name)?;
        let imported = parser::parse_named(&tokens, &source, &name)?;
        self.eval_module_with_scope(&imported, Path::new(&name), depth + 1, None, requested)
    }

    fn eval_super_member(
        &mut self,
        field: &str,
        scope: &Scope,
        depth: usize,
        property_access: bool,
    ) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        if let Some(Value::List(items)) = scope.get("super") {
            let mut length = scope.receiver_list_base.unwrap_or(items.len());
            if let Some(entries) = &scope.receiver_entries {
                self.eval_listing_length(entries, scope, depth + 1, &mut length)?;
            }
            return match field {
                "length" => Ok(Value::Int(length as i64)),
                "isEmpty" => Ok(Value::Bool(length == 0)),
                "isNotEmpty" => Ok(Value::Bool(length != 0)),
                "first" | "last" => {
                    if length == 0 {
                        return Err(Error::Eval(format!("{field} called on an empty Listing")));
                    }
                    let target = if field == "first" { 0 } else { length - 1 };
                    let mut value = items.get(target).cloned();
                    if let Some(entries) = &scope.receiver_entries {
                        let mut position = scope.receiver_list_base.unwrap_or(items.len());
                        self.eval_listing_member(
                            entries,
                            scope,
                            depth + 1,
                            target,
                            &mut position,
                            &mut value,
                            &[],
                        )?;
                    }
                    value.ok_or_else(|| {
                        Error::Eval(format!("listing index {target} is out of bounds"))
                    })
                }
                _ => Err(Error::Eval(format!("field not found: {field}"))),
            };
        }
        let Some(Value::Object(map, source)) = scope.get("super") else {
            return Err(Error::Eval("undefined variable: super".into()));
        };
        if let Some(source) = source {
            let entry = source
                .entries
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, entry)| match entry {
                    Entry::Property(prop)
                        if prop.name == field
                            && !has_modifier(&prop.modifiers, Modifier::Local) =>
                    {
                        Some((index, prop))
                    }
                    _ => None,
                });
            if let Some((index, prop)) = entry {
                let definition = restore_scope(&capture_object_source_scope(source));
                let owners =
                    entry_scope_owners(&source.entries, Some(&source.entry_scopes), Some(source));
                let mut receiver = Scope {
                    receiver_entries: scope.receiver_entries.clone(),
                    ..Scope::default()
                };
                if let Some(Value::Object(members, _)) = scope.get("this") {
                    for (name, value) in members.iter() {
                        receiver.set(name, value.clone());
                    }
                }
                if let Some(this) = scope.get("this") {
                    receiver.set("this", this.clone());
                }
                let mut active = scope_for_object_entry(
                    index,
                    &receiver,
                    Some(&source.entry_scopes),
                    &owners,
                    Some((&definition, &source.body_members)),
                );
                // Reconstruct a body amendment from its own parent, rather
                // than reapplying it to the already-amended cached member.
                if prop.body.is_some() {
                    let has_parent_member = matches!(active.get("super"),
                        Some(Value::Object(parent, _)) if parent.contains_key(field));
                    let inherited = if has_parent_member {
                        self.eval_super_member(field, &active, depth + 1, true)?
                    } else {
                        Value::Null
                    };
                    active.set(field, inherited);
                }
                if let Some(value) = self.eval_property(prop, &active, depth + 1)? {
                    return Ok(value);
                }
            }
        }
        match field {
            "length" | "keys" | "isEmpty" | "isNotEmpty"
                if property_access
                    && source
                        .as_ref()
                        .is_none_or(|source| source.type_name.is_none()) =>
            {
                let mut keys = IndexMap::new();
                if let Some(entries) = &scope.receiver_entries {
                    self.eval_receiver_keys(entries, scope, depth + 1, &mut keys)?;
                } else {
                    keys.extend(map.keys().map(|key| (key.clone(), ())));
                }
                return Ok(match field {
                    "length" => Value::Int(keys.len() as i64),
                    "keys" => Value::List(ListValue::new(
                        ListKind::Set,
                        keys.into_keys().map(Value::String).collect::<Vec<_>>(),
                    )),
                    "isEmpty" => Value::Bool(keys.is_empty()),
                    _ => Value::Bool(!keys.is_empty()),
                });
            }
            _ => {}
        }
        if let Some(value) = map.get(field) {
            return Ok(value.clone());
        }
        // An untyped object's prototype supplies an empty Dynamic default
        // for a newly declared property, never a member of an outer object.
        if property_access
            && source
                .as_ref()
                .is_none_or(|source| source.type_name.is_none())
        {
            return Ok(Value::Object(Arc::default(), None));
        }
        Err(Error::Eval(format!("field not found: {field}")))
    }

    /// Enumerate receiver members without evaluating their values. Mapping
    /// metadata such as super.length includes entries after the current one.
    fn eval_receiver_keys(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        keys: &mut IndexMap<Arc<str>, ()>,
    ) -> Result<()> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        let scope = scope.child();
        for entry in entries {
            match entry {
                // Locals are values, not members. Evaluating a local that
                // reads super.length while counting would re-enter this walk.
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Property(prop)
                    if prop.name != "default"
                        && !has_modifier(&prop.modifiers, Modifier::Hidden) =>
                {
                    keys.insert(prop.name.as_str().into(), ());
                }
                Entry::DynProperty(key, _) => {
                    let key = self.eval_expr(key, &scope, depth + 1)?;
                    keys.insert(value_to_key(&key)?, ());
                }
                Entry::Spread(expr) => {
                    let value = self.eval_expr(expr, &scope, depth + 1)?;
                    check_iterable(&value)?;
                    if let Value::Object(map, _) = value {
                        keys.extend(map.keys().map(|key| (key.clone(), ())));
                    }
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection)? {
                        let mut iter = scope.child();
                        iter.set(&generator.val_var, value);
                        if let Some(name) = &generator.key_var {
                            iter.set(name, key);
                        }
                        self.eval_receiver_keys(&generator.body, &iter, depth + 1, keys)?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition = self.eval_expr(&generator.condition, &scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(body) = selected {
                        self.eval_receiver_keys(body, &scope, depth + 1, keys)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Count listing members without evaluating element values, so metadata
    /// can be read by an element itself without recursing into that element.
    fn eval_listing_length(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        length: &mut usize,
    ) -> Result<()> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        let scope = scope.child();
        for entry in entries {
            match entry {
                Entry::Elem(_) => *length += 1,
                Entry::DynProperty(index, _) => {
                    let index = self.eval_expr(index, &scope, depth + 1)?;
                    if let Value::Int(index) = index
                        && usize::try_from(index).ok() == Some(*length)
                    {
                        *length += 1;
                    }
                }
                // Locals are values, not members. Evaluating a local that
                // reads super.length while counting would re-enter this walk.
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Spread(expr) => {
                    let value = self.eval_expr(expr, &scope, depth + 1)?;
                    *length += listing_spread_values(value)?.len();
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection)? {
                        let mut iter = scope.child();
                        iter.set(&generator.val_var, value);
                        if let Some(name) = &generator.key_var {
                            iter.set(name, key);
                        }
                        self.eval_listing_length(&generator.body, &iter, depth + 1, length)?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition = self.eval_expr(&generator.condition, &scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(body) = selected {
                        self.eval_listing_length(body, &scope, depth + 1, length)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Evaluate the object an `expr.field` / `expr?.field` access reads from.
    ///
    /// An `import("uri")` base is narrowed to the single field being read, so
    /// `import("mod.pkl").field` leaves the module's other properties
    /// unevaluated just as `import "mod.pkl" as Mod` + `Mod.field` does.
    fn eval_field_base(
        &mut self,
        obj_expr: &Expr,
        field: &str,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        if let Expr::Import(uri, module_path) = obj_expr {
            let requested = HashSet::from_iter([field.to_string()]);
            return self.eval_import_expr(uri, Path::new(module_path), depth, Some(requested));
        }
        self.eval_expr(obj_expr, scope, depth + 1)
    }

    fn eval_module_with_scope(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
        inherited_scope: Option<Scope>,
        requested_fields: Option<HashSet<String>>,
    ) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval(format!(
                "max import depth {} exceeded",
                self.max_depth
            )));
        }
        self.check_cancelled()?;
        self.prefetch_remote_imports(module, path);
        if module
            .body
            .iter()
            .any(|entry| matches!(entry, Entry::Elem(_)))
        {
            return Err(Error::Eval("Invalid property definition".into()));
        }
        let type_namespace = self.module_type_namespace(path);
        let mut scope = Scope {
            type_namespace: Some(type_namespace),
            ..Scope::default()
        };
        seed_builtins(&mut scope);
        // Evaluated as the base of an amending or extending module, whose
        // reads of this module's bindings are not analyzed here.
        let evaluated_as_base = inherited_scope.is_some();
        if let Some(inherited_scope) = inherited_scope {
            for (key, value) in inherited_scope.flatten() {
                scope.set_name(key, value);
            }
            for (key, ty) in inherited_scope.flatten_type_aliases() {
                scope.set_type_alias(key, ty);
            }
            for (key, identity) in inherited_scope.flatten_module_identities() {
                scope.set_module_identity(key, identity);
            }
        }
        let mut inherited_type_names = HashSet::default();
        let inherited_references =
            self.inherited_reference_roots(module, path, depth + 1, &mut inherited_type_names)?;
        // Checks here also resolve the type aliases of the modules this one
        // amends or extends, which may redefine a built-in.
        let inherited_builtins: Vec<&str> = BINDING_BUILTIN_TYPES
            .iter()
            .copied()
            .filter(|name| inherited_type_names.contains(*name))
            .collect();
        let requested_output_fields = requested_fields
            .as_ref()
            .map(|fields| expand_requested_fields(&module.body, fields, &inherited_builtins));
        let analysis_entries =
            analysis_entries_for_requested_fields(&module.body, requested_output_fields.as_ref());
        let mut referenced_imports = referenced_roots(&analysis_entries);
        referenced_imports.extend(inherited_references);
        let import_field_uses = import_field_uses(&analysis_entries);

        let inherited_local_paths: Vec<_> = module
            .amends
            .iter()
            .chain(module.extends.iter())
            .filter(|uri| local_module_path(path, uri).is_some())
            .map(|uri| self.local_file_path(path, uri))
            .collect::<Result<_>>()?;
        let mut deferred_inherited_imports = Vec::new();

        // Process imports
        for import in &module.imports {
            // A relative import inside a remote module resolves against that URL.
            let resolved_uri = resolve_remote_relative(path, &import.uri);
            let uri: &str = resolved_uri.as_deref().unwrap_or(&import.uri);

            // Handle glob imports: import* "dir/*.pkl" as Alias
            if import.is_glob {
                let alias = import
                    .alias
                    .clone()
                    .ok_or_else(|| Error::Eval("import* requires an alias".into()))?;
                if !referenced_imports.contains(&alias) {
                    continue;
                }

                // Code in other modules can read the alias without being analyzed
                // here: an amended or extended base reads it from this module's
                // scope, and a module amending or extending this one inherits it.
                let requested =
                    if module.amends.is_none() && module.extends.is_none() && !evaluated_as_base {
                        glob_index_keys(&import_field_uses, &alias)
                    } else {
                        None
                    };
                let mapping = self.eval_glob_import(uri, path, depth, requested.as_ref())?;
                scope.declare(alias, mapping);
                continue;
            }

            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP import
                let alias = import.alias.clone().unwrap_or_else(|| {
                    uri.rsplit('/')
                        .next()
                        .unwrap_or(uri)
                        .strip_suffix(".pkl")
                        .unwrap_or(uri)
                        .to_string()
                });
                if !referenced_imports.contains(&alias) {
                    continue;
                }
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                let source = self.fetch_source(uri)?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, uri)?;
                    let imp_module = parser::parse_named(&tokens, &source, uri)?;
                    self.eval_module_with_scope(
                        &imp_module,
                        Path::new(uri),
                        depth + 1,
                        None,
                        requested,
                    )?
                };
                scope.declare(&alias, imported_val);
                scope.set_module_identity(alias, canonical_remote_module_identity(uri));
                continue;
            }

            if uri.starts_with("package://") {
                let pkg = resolve_package_uri(uri)?;
                let fragment = uri.split_once('#').map(|(_, f)| f).unwrap_or("");
                let file_path = fragment.strip_prefix('/').unwrap_or(fragment);
                let alias = import.alias.clone().unwrap_or_else(|| {
                    file_path
                        .rsplit('/')
                        .next()
                        .unwrap_or(file_path)
                        .strip_suffix(".pkl")
                        .unwrap_or(file_path)
                        .to_string()
                });
                if !referenced_imports.contains(&alias) {
                    continue;
                }
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                // For zip packages, extract to temp dir and eval as local file
                if let PackageSource::Zip(zip_url, _) = &pkg {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(file_path);
                        let imported_val = self.eval_file_with_requested_fields(
                            &local_path,
                            depth + 1,
                            requested,
                        )?;
                        let identity = self.module_type_namespace(&local_path);
                        scope.declare(&alias, imported_val);
                        scope.set_module_identity(alias, identity);
                        continue;
                    }
                    #[cfg(not(feature = "package-zip"))]
                    {
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                }
                let url = match &pkg {
                    PackageSource::Direct { url, .. } => url.clone(),
                    PackageSource::Zip(..) => unreachable!(),
                };
                let root = match &pkg {
                    PackageSource::Direct { root, .. } => root,
                    PackageSource::Zip(..) => unreachable!(),
                };
                let source = self.fetch_direct_package_source(&url, root)?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, &url)?;
                    let imp_module = parser::parse_named(&tokens, &source, &url)?;
                    self.eval_module_with_scope(
                        &imp_module,
                        Path::new(&url),
                        depth + 1,
                        None,
                        requested,
                    )?
                };
                scope.declare(&alias, imported_val);
                scope.set_module_identity(alias, url);
                continue;
            }

            // Handle pkl: standard library imports
            if let Some(module_name) = uri.strip_prefix("pkl:") {
                let stdlib_val = stdlib_module(module_name);
                let alias = import
                    .alias
                    .clone()
                    .unwrap_or_else(|| module_name.to_string());
                if !referenced_imports.contains(&alias) {
                    continue;
                }
                scope.declare(alias, stdlib_val);
                continue;
            }

            // Skip other non-local imports
            if uri.contains("://") && !uri.starts_with("file://") {
                continue;
            }

            let import_path = self.local_file_path(path, uri)?;
            let alias = import.alias.clone().unwrap_or_else(|| {
                import_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });
            if !referenced_imports.contains(&alias) {
                // Unused imports are intentionally lazy: missing local paths are
                // reported only if the imported binding is actually referenced.
                continue;
            }
            if !self.path_exists_io(&import_path)? {
                return Err(Error::ImportNotFound(import_path.display().to_string()));
            }
            let mut inherited_path = None;
            for candidate in &inherited_local_paths {
                if self.same_local_path(candidate, &import_path)? {
                    inherited_path = Some(candidate);
                    break;
                }
            }
            if let Some(inherited_path) = inherited_path {
                deferred_inherited_imports.push((alias, inherited_path.clone()));
                continue;
            }
            {
                let requested = requested_fields_for_import(&import_field_uses, &alias);
                let imported_val =
                    self.eval_file_with_requested_fields(&import_path, depth + 1, requested)?;
                let identity = self.module_type_namespace(&import_path);
                scope.declare(&alias, imported_val);
                scope.set_module_identity(alias, identity);
            }
        }

        // Process amends: load base module as starting values
        let mut base_obj = ObjectMap::default();
        let mut late_inherited_properties = Vec::new();
        if let Some(amends_uri) = &module.amends {
            // A relative amends inside a remote module resolves against that URL.
            let resolved_amends = resolve_remote_relative(path, amends_uri);
            let uri: &str = resolved_amends.as_deref().unwrap_or(amends_uri);
            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP amends
                self.check_not_self(module, path, Path::new(uri), "amend")?;
                let source = self.fetch_source(uri)?;
                let tokens = lexer::lex_named(&source, uri)?;
                let base_module = parser::parse_named(&tokens, &source, uri)?;
                let base_val = self.eval_module_with_scope(
                    &base_module,
                    Path::new(uri),
                    depth + 1,
                    Some(scope.clone()),
                    None,
                )?;
                self.layer_evaluated_module_scope(Path::new(uri), &mut scope);
                late_inherited_properties.extend(self.inherited_late_properties(Path::new(uri)));
                if let Value::Object(m, _) = base_val {
                    base_obj = (*m).clone();
                }
            } else if uri.starts_with("package://") {
                let pkg = resolve_package_uri(uri)?;
                if let PackageSource::Zip(zip_url, entry) = &pkg {
                    #[cfg(feature = "package-zip")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(entry);
                        self.check_not_self(module, path, &local_path, "amend")?;
                        let source = self.read_to_string_io(&local_path)?;
                        let name = local_path.display().to_string();
                        let tokens = lexer::lex_named(&source, &name)?;
                        let base_module = parser::parse_named(&tokens, &source, &name)?;
                        let base_val = self.eval_module_with_scope(
                            &base_module,
                            &local_path,
                            depth + 1,
                            Some(scope.clone()),
                            None,
                        )?;
                        self.layer_evaluated_module_scope(&local_path, &mut scope);
                        late_inherited_properties
                            .extend(self.inherited_late_properties(&local_path));
                        if let Value::Object(m, _) = base_val {
                            base_obj = (*m).clone();
                        }
                    }
                    #[cfg(not(feature = "package-zip"))]
                    {
                        let _ = entry;
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                } else if let PackageSource::Direct { url, root } = &pkg {
                    self.check_not_self(module, path, Path::new(url.as_str()), "amend")?;
                    let source = self.fetch_direct_package_source(url, root)?;
                    let tokens = lexer::lex_named(&source, url)?;
                    let base_module = parser::parse_named(&tokens, &source, url)?;
                    let base_val = self.eval_module_with_scope(
                        &base_module,
                        Path::new(url.as_str()),
                        depth + 1,
                        Some(scope.clone()),
                        None,
                    )?;
                    self.layer_evaluated_module_scope(Path::new(url.as_str()), &mut scope);
                    late_inherited_properties
                        .extend(self.inherited_late_properties(Path::new(url.as_str())));
                    if let Value::Object(m, _) = base_val {
                        base_obj = (*m).clone();
                    }
                }
            } else if !uri.starts_with("pkl:")
                && (!uri.contains("://") || uri.starts_with("file://"))
            {
                let amends_path = self.local_file_path(path, uri)?;
                if self.path_exists_io(&amends_path)? {
                    self.check_not_self(module, path, &amends_path, "amend")?;
                    let base_val =
                        self.eval_file_with_scope(&amends_path, depth + 1, Some(scope.clone()))?;
                    self.layer_evaluated_module_scope(&amends_path, &mut scope);
                    late_inherited_properties.extend(self.inherited_late_properties(&amends_path));
                    if let Value::Object(m, _) = &base_val {
                        self.bind_deferred_inherited_imports(
                            &deferred_inherited_imports,
                            &amends_path,
                            &base_val,
                            &mut scope,
                        )?;
                        base_obj = (**m).clone();
                    }
                }
            }
        }

        // Inject class definitions from amends base into scope so the amending
        // module can reference them (e.g., `new Step { ... }`).
        // We re-parse the base module to find ClassDef entries, then evaluate
        // them directly (similar to extends handling), because eval_module strips
        // class definitions from its return value.
        // Also remove inherited class definitions from base_obj so they don't
        // appear in the amending module's data output.
        if let Some(amends_uri) = &module.amends {
            let resolved_amends = resolve_remote_relative(path, amends_uri);
            let uri: &str = resolved_amends.as_deref().unwrap_or(amends_uri);
            if let Some((base_module, source_path)) = self.load_parsed_module(uri, path)? {
                let mut base_scope = scope.clone();
                base_scope.type_namespace =
                    Some(self.module_type_namespace(Path::new(&source_path)));
                for entry in base_module.body.iter() {
                    if let Entry::ClassDef(name, class_mods, parent, body) = entry {
                        let defaults = self.eval_class_def(
                            name,
                            class_mods,
                            parent.as_deref(),
                            body,
                            &base_scope,
                            depth,
                        )?;
                        scope.set(name, defaults);
                        // Remove inherited class definitions from base output —
                        // they were included at depth > 0 for dotted access but
                        // should not appear in the amending module's data output.
                        base_obj.shift_remove(name.as_str());
                    }
                    // Extract converters from the base module's output block
                    // (the amending module inherits them; child overrides if present).
                    if let Entry::Property(prop) = entry
                        && prop.name == "output"
                        && depth == 0
                    {
                        self.extract_converters_from_ast(prop, &scope, depth);
                    }
                    if let Entry::Property(prop) = entry
                        && !has_modifier(&prop.modifiers, Modifier::Local)
                        && prop.name != "output"
                    {
                        late_inherited_properties.push(prop.clone());
                    }
                }
            }
            // Remove function values from base output (not data)
            base_obj.retain(|_, v| !matches!(v, Value::Lambda(..)));
        }

        // Process extends: load base module, inherit all members and scope
        if let Some(extends_uri) = &module.extends {
            // A relative extends inside a remote module resolves against that URL.
            let resolved_extends = resolve_remote_relative(path, extends_uri);
            let uri: &str = resolved_extends.as_deref().unwrap_or(extends_uri);
            if !uri.contains("://") || uri.starts_with("file://") {
                let extends_path = self.local_file_path(path, uri)?;
                if self.path_exists_io(&extends_path)? {
                    self.check_not_self(module, path, &extends_path, "extend")?;
                    let base_module = self.parse_file(&extends_path)?;
                    if base_module.amends.is_some() {
                        return Err(Error::Eval(format!(
                            "Module `{}` cannot be extended or used as type because it amends another module.",
                            self.amended_module_name(&base_module, &extends_path)
                        )));
                    }
                    let ext_val =
                        self.eval_file_with_scope(&extends_path, depth + 1, Some(scope.clone()))?;
                    self.layer_evaluated_module_scope(&extends_path, &mut scope);
                    late_inherited_properties.extend(self.inherited_late_properties(&extends_path));
                    let name = extends_path.display().to_string();
                    let source = self.read_to_string_io(&extends_path)?;
                    let tokens = lexer::lex_named(&source, &name)?;
                    let ext_module = parser::parse_named(&tokens, &source, &name)?;
                    if let Value::Object(m, _) = &ext_val {
                        self.bind_deferred_inherited_imports(
                            &deferred_inherited_imports,
                            &extends_path,
                            &ext_val,
                            &mut scope,
                        )?;
                        base_obj = (**m).clone();
                    }
                    let mut base_scope = scope.clone();
                    base_scope.type_namespace = Some(self.module_type_namespace(&extends_path));
                    // Also evaluate the base module's scope (classes, locals) into our scope
                    // by re-processing its body entries
                    for entry in ext_module.body.iter() {
                        match entry {
                            Entry::ClassDef(cls_name, cls_mods, parent, body) => {
                                let defaults = self.eval_class_def(
                                    cls_name,
                                    cls_mods,
                                    parent.as_deref(),
                                    body,
                                    &base_scope,
                                    depth,
                                )?;
                                scope.set(cls_name, defaults);
                                base_obj.shift_remove(cls_name.as_str());
                            }
                            Entry::TypeAlias(name, ty) => {
                                self.eval_type_alias(name, ty, &mut scope);
                            }
                            Entry::Property(prop) if prop.name == "output" && depth == 0 => {
                                self.extract_converters_from_ast(prop, &scope, depth);
                            }
                            Entry::Property(prop)
                                if !has_modifier(&prop.modifiers, Modifier::Local)
                                    && prop.name != "output" =>
                            {
                                late_inherited_properties.push(prop.clone());
                                if let Ok(Some(value)) = self.eval_property(prop, &scope, depth) {
                                    scope.set(&prop.name, value);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            } else if uri.starts_with("https://") || uri.starts_with("http://") {
                self.check_not_self(module, path, Path::new(uri), "extend")?;
                let source = self.fetch_source(uri)?;
                let tokens = lexer::lex_named(&source, uri)?;
                let ext_module = parser::parse_named(&tokens, &source, uri)?;
                if ext_module.amends.is_some() {
                    return Err(Error::Eval(format!(
                        "Module `{}` cannot be extended or used as type because it amends another module.",
                        self.amended_module_name(&ext_module, Path::new(uri))
                    )));
                }
                let ext_val = self.eval_module_with_scope(
                    &ext_module,
                    Path::new(uri),
                    depth + 1,
                    Some(scope.clone()),
                    None,
                )?;
                self.layer_evaluated_module_scope(Path::new(uri), &mut scope);
                late_inherited_properties.extend(self.inherited_late_properties(Path::new(uri)));
                if let Value::Object(m, _) = ext_val {
                    base_obj = (*m).clone();
                }
                let mut base_scope = scope.clone();
                base_scope.type_namespace = Some(self.module_type_namespace(Path::new(uri)));
                // Inject class definitions from HTTP base into scope
                for entry in ext_module.body.iter() {
                    if let Entry::ClassDef(cls_name, cls_mods, parent, body) = entry {
                        let defaults = self.eval_class_def(
                            cls_name,
                            cls_mods,
                            parent.as_deref(),
                            body,
                            &base_scope,
                            depth,
                        )?;
                        scope.set(cls_name, defaults);
                        base_obj.shift_remove(cls_name.as_str());
                    }
                    if let Entry::Property(prop) = entry
                        && !has_modifier(&prop.modifiers, Modifier::Local)
                        && prop.name != "output"
                    {
                        late_inherited_properties.push(prop.clone());
                        if let Ok(Some(value)) = self.eval_property(prop, &scope, depth) {
                            scope.set(&prop.name, value);
                        }
                    }
                }
            }
        }

        let requested_eval_fields = requested_output_fields.as_ref().map(|fields| {
            let mut dependency_entries = late_inherited_properties
                .iter()
                .cloned()
                .map(Entry::Property)
                .collect::<Vec<_>>();
            dependency_entries.extend(module.body.iter().cloned());
            expand_requested_fields(&dependency_entries, fields, &inherited_builtins)
        });

        // Inherited members are visible by name and through `this`/`module`
        // from the start; this module's members join them as they evaluate.
        let mut all_props = Arc::new(base_obj.clone());
        for (k, v) in &base_obj {
            scope.set(k, v.clone());
        }
        scope.set("this", Value::Object(Arc::clone(&all_props), None));
        scope.set("module", Value::Object(Arc::clone(&all_props), None));

        // Evaluate locals, classes, type aliases and properties in dependency
        // order, so a member can read one declared after it. A member that
        // fails is poisoned rather than failing the module: as in Pkl, the
        // error surfaces only where the member is used (or rendered, below).
        let mut class_names: std::collections::HashSet<String> =
            std::collections::HashSet::default();
        let mut evaluated: HashMap<&str, Value> = HashMap::default();
        let mut failed: HashMap<&str, String> = HashMap::default();
        let mut failed_indices: HashSet<usize> = HashSet::default();
        let plan = Arc::clone(
            &self
                .plan_cache
                .entry(Arc::as_ptr(&module.body) as usize)
                .or_insert_with(|| {
                    (
                        Arc::clone(&module.body),
                        Arc::new(module_evaluation_plan(&module.body)),
                    )
                })
                .1,
        );
        let mut todo = plan.order.clone();
        // A member can fail because it read another before that one could be
        // evaluated: through a dynamic `module[key]`, which the order can't
        // account for. Members that failed are evaluated again, along with
        // the members that read them, until a pass changes nothing.
        let mut previous_failures: Option<HashMap<&str, String>> = None;
        for _ in 0..=plan.order.len() {
            for index in todo {
                let entry = &module.body[index];
                if let Some(name) = module_member_name(entry) {
                    failed.remove(name);
                    failed_indices.remove(&index);
                }
                match entry {
                    Entry::Property(prop)
                        if has_modifier(&prop.modifiers, Modifier::Local)
                            && prop.value.is_some() =>
                    {
                        match self.eval_expr(prop.value.as_ref().unwrap(), &scope, depth) {
                            Ok(val) => scope.declare(&prop.name, val),
                            Err(Error::Eval(message)) => {
                                scope.declare_poisoned(prop.name.clone(), message.clone());
                                failed.insert(&prop.name, message);
                                failed_indices.insert(index);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Entry::ClassDef(name, class_mods, parent, body) => {
                        class_names.insert(name.clone());
                        // Reject invalid inheritance eagerly using the same
                        // structural validation as `eval_class_def`. Other
                        // evaluation errors can depend on members declared
                        // later in the module and remain retryable.
                        if let Some(parent_name) = parent.as_deref() {
                            let parent_val = resolve_dotted(&scope, parent_name);
                            check_supertype(&scope, name, parent_name, parent_val.as_ref())?;
                        }
                        match self.eval_class_def(
                            name,
                            class_mods,
                            parent.as_deref(),
                            body,
                            &scope,
                            depth,
                        ) {
                            Ok(defaults) => {
                                scope.set_member_poison(name, None);
                                scope.declare(name, defaults.clone());
                                module_props_insert(
                                    &mut scope,
                                    &mut all_props,
                                    name.clone(),
                                    defaults,
                                );
                                let snapshot = Value::Object(Arc::clone(&all_props), None);
                                scope.set("this", snapshot.clone());
                                scope.set("module", snapshot);
                            }
                            Err(Error::Eval(message)) => {
                                scope.set_member_poison(name, Some(message.clone()));
                                scope.declare_poisoned(name.clone(), message.clone());
                                failed.insert(name, message);
                                failed_indices.insert(index);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Entry::TypeAlias(name, ty) => {
                        self.eval_type_alias(name, ty, &mut scope);
                    }
                    Entry::Property(prop) => {
                        let mods = &prop.modifiers;
                        if has_modifier(mods, Modifier::Local) {
                            continue;
                        }
                        // Extract renderer converters from the `output` block AST,
                        // then skip it (it's not included in the output).
                        // Clear any base-inherited converters so child overrides take precedence.
                        if prop.name == "output" {
                            if depth == 0 {
                                self.converters.clear();
                                self.extract_converters_from_ast(prop, &scope, depth);
                            }
                            continue;
                        }
                        if let Some(fields) = &requested_eval_fields
                            && !fields.contains(&prop.name)
                        {
                            continue;
                        }
                        // abstract/external properties must have a value (or be overridden)
                        if (has_modifier(mods, Modifier::Abstract)
                            || has_modifier(mods, Modifier::External))
                            && prop.value.is_none()
                            && prop.body.is_none()
                        {
                            if let Some(v) = base_obj.get(prop.name.as_str()) {
                                // Satisfied by base — add to scope so other properties can reference it
                                scope.set(&prop.name, v.clone());
                            } else if has_modifier(mods, Modifier::External) {
                                return Err(Error::Eval(format!(
                                    "external property '{}' must be assigned a value in {}",
                                    prop.name,
                                    path.display()
                                )));
                            } else if depth == 0 && !module_is_abstract(module) {
                                return Err(Error::Eval(format!(
                                    "abstract property '{}' must be assigned a value in {}",
                                    prop.name,
                                    path.display()
                                )));
                            }
                            continue;
                        }
                        let val = match self.eval_property(prop, &scope, depth) {
                            Ok(value) => value,
                            // Module properties are late-bound. Keep an unresolved
                            // template expression deferred until a consumer actually
                            // requires it; poisoned locals still surface that error.
                            Err(Error::Eval(message))
                                if is_unresolved_template_error(&message)
                                    && (module_is_abstract(module)
                                        || property_reference_names(prop).iter().any(|name| {
                                            late_inherited_properties
                                                .iter()
                                                .any(|inherited| inherited.name == *name)
                                        })) =>
                            {
                                None
                            }
                            Err(Error::Eval(message)) => {
                                scope.declare_poisoned(prop.name.clone(), message.clone());
                                failed.insert(&prop.name, message);
                                failed_indices.insert(index);
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        if let Some(v) = val {
                            // const/fixed: error if overriding an immutable property from base
                            if (has_modifier(mods, Modifier::Const)
                                || has_modifier(mods, Modifier::Fixed))
                                && base_obj.contains_key(prop.name.as_str())
                            {
                                let kind = if has_modifier(mods, Modifier::Const) {
                                    "const"
                                } else {
                                    "fixed"
                                };
                                return Err(Error::Eval(format!(
                                    "cannot override {kind} property '{}'",
                                    prop.name
                                )));
                            }
                            // Always add to scope so other properties can reference it
                            scope.declare(&prop.name, v.clone());
                            // Track in all_props (including hidden) for `this`/`module`
                            module_props_insert(
                                &mut scope,
                                &mut all_props,
                                prop.name.clone(),
                                v.clone(),
                            );
                            // Update `this` and `module` with all properties (including hidden)
                            let snapshot = Value::Object(Arc::clone(&all_props), None);
                            scope.set("this", snapshot.clone());
                            scope.set("module", snapshot);
                            evaluated.insert(&prop.name, v);
                        }
                    }
                    _ => {}
                }
            }
            if failed_indices.is_empty() || previous_failures.as_ref() == Some(&failed) {
                break;
            }
            previous_failures = Some(failed.clone());
            todo = plan.affected_by(&failed_indices);
        }

        // The planner may evaluate an annotation class or alias after the
        // annotated property. Validate once the final module scope is ready.
        check_module_annotations(module, &scope)?;

        // Assemble the module object in declaration order: inherited members,
        // then classes (exported so they're accessible via dotted paths, e.g.
        // `import "helpers.pkl"` → `helpers.ClassName`), then properties. A
        // failed property that would be rendered fails the module.
        let mut out = base_obj;
        let mut poisoned_members = IndexMap::new();
        for entry in module.body.iter() {
            if let Entry::ClassDef(name, ..) = entry {
                if let Some(value) = scope.get(name) {
                    out.insert(name.as_str().into(), value.clone());
                } else if let Some(message) = failed.get(name.as_str()) {
                    poisoned_members.insert(
                        name.clone(),
                        PoisonedMember {
                            message: message.clone(),
                            rendered: false,
                        },
                    );
                }
            }
        }
        for entry in module.body.iter() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            // Locals participate in failure convergence but are never module
            // members or rendered output.
            if has_modifier(&prop.modifiers, Modifier::Local) {
                continue;
            }
            let rendered = !has_modifier(&prop.modifiers, Modifier::Hidden)
                && requested_output_fields
                    .as_ref()
                    .is_none_or(|fields| fields.contains(&prop.name));
            if let Some(message) = failed.get(prop.name.as_str()) {
                if rendered && depth == 0 {
                    return Err(Error::Eval(message.clone()));
                }
                self.rendered_member_failed |= rendered;
                poisoned_members.insert(
                    prop.name.clone(),
                    PoisonedMember {
                        message: message.clone(),
                        rendered,
                    },
                );
                continue;
            }
            if let Some(value) = evaluated.remove(prop.name.as_str())
                && rendered
                && (depth > 0 || should_render_property_value(prop, &value))
            {
                out.insert(prop.name.as_str().into(), value);
            }
        }

        // Pkl properties are late-bound. Re-evaluate inherited expressions after
        // child overrides have populated the scope (for example `uses` derived
        // from an action module's overridden `action` and `version`).
        let child_property_names: HashSet<&str> = module
            .body
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some(prop.name.as_str()),
                _ => None,
            })
            .collect();
        if depth == 0
            && !module_is_abstract(module)
            && let Some(prop) = late_inherited_properties.iter().find(|prop| {
                has_modifier(&prop.modifiers, Modifier::Abstract)
                    && prop.value.is_none()
                    && prop.body.is_none()
                    && !child_property_names.contains(prop.name.as_str())
            })
        {
            return Err(Error::Eval(format!(
                "abstract property '{}' must be assigned a value in {}",
                prop.name,
                path.display()
            )));
        }
        let child_candidates = module
            .body
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop)
                    if prop.name != "output" && (prop.value.is_some() || prop.body.is_some()) =>
                {
                    Some(prop)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut late_bound_names = late_inherited_properties
            .iter()
            .map(|prop| prop.name.clone())
            .collect::<HashSet<_>>();
        let mut late_child_properties = Vec::new();
        loop {
            let mut added = false;
            for prop in &child_candidates {
                if late_child_properties
                    .iter()
                    .any(|selected: &&Property| selected.name == prop.name)
                {
                    continue;
                }
                let refs = property_reference_names(prop);
                if refs.iter().any(|name| late_bound_names.contains(name))
                    || (refs.contains(DYNAMIC_SIBLING_REF) && !late_bound_names.is_empty())
                {
                    late_bound_names.insert(prop.name.clone());
                    late_child_properties.push(*prop);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        let late_passes = late_inherited_properties.len() + late_child_properties.len();
        for pass in 0..=late_passes {
            for prop in &late_inherited_properties {
                if child_property_names.contains(prop.name.as_str()) {
                    continue;
                }
                match self.eval_property(prop, &scope, depth) {
                    Ok(Some(value)) => {
                        scope.set(&prop.name, value.clone());
                        module_props_insert(
                            &mut scope,
                            &mut all_props,
                            prop.name.clone(),
                            value.clone(),
                        );
                        if !has_modifier(&prop.modifiers, Modifier::Hidden)
                            && (depth > 0 || should_render_property_value(prop, &value))
                            && requested_output_fields
                                .as_ref()
                                .is_none_or(|fields| fields.contains(&prop.name))
                        {
                            out.insert(prop.name.as_str().into(), value);
                        }
                        let snapshot = Value::Object(Arc::clone(&all_props), None);
                        scope.set("this", snapshot.clone());
                        scope.set("module", snapshot);
                    }
                    Ok(None) => {}
                    Err(Error::Eval(message))
                        if pass < late_passes
                            || (module_is_abstract(module)
                                && is_unresolved_template_error(&message)) => {}
                    Err(error) => return Err(error),
                }
            }
            for prop in &late_child_properties {
                match self.eval_property(prop, &scope, depth) {
                    Ok(Some(value)) => {
                        scope.set(&prop.name, value.clone());
                        if has_modifier(&prop.modifiers, Modifier::Local) {
                            continue;
                        }
                        module_props_insert(
                            &mut scope,
                            &mut all_props,
                            prop.name.clone(),
                            value.clone(),
                        );
                        if !has_modifier(&prop.modifiers, Modifier::Hidden)
                            && (depth > 0 || should_render_property_value(prop, &value))
                            && requested_output_fields
                                .as_ref()
                                .is_none_or(|fields| fields.contains(&prop.name))
                        {
                            out.insert(prop.name.as_str().into(), value);
                        }
                        let snapshot = Value::Object(Arc::clone(&all_props), None);
                        scope.set("this", snapshot.clone());
                        scope.set("module", snapshot);
                    }
                    Ok(None) => {}
                    Err(Error::Eval(message))
                        if pass < late_passes
                            || (module_is_abstract(module)
                                && is_unresolved_template_error(&message)) => {}
                    Err(error) => return Err(error),
                }
            }
        }

        if depth == 0 {
            for prop in &late_inherited_properties {
                if let Some(value) = all_props.get(prop.name.as_str())
                    && !should_render_property_value(prop, value)
                    && !child_property_names.contains(prop.name.as_str())
                {
                    out.shift_remove(prop.name.as_str());
                }
            }
        }

        // Check declarations only after this module's final scope is complete:
        // constraints and aliases may refer to members declared later.
        if !evaluated_as_base {
            for entry in module.body.iter() {
                if let Entry::Property(prop) = entry
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                    && (prop.value.is_some() || prop.body.is_some())
                    && prop.type_ann.is_some()
                    && scope.is_declared(&prop.name)
                    && let Some(value) = scope.get(&prop.name)
                {
                    self.check_declared_property_type(prop, value, &scope, depth)?;
                }
            }
        }

        // At the top level (depth 0), strip class definitions and lambdas from
        // the serialized output — they're schema/functions, not data.
        // Imported modules (depth > 0) keep them so dotted access works
        // (e.g., `helpers.ClassName`).
        if depth == 0 {
            for name in &class_names {
                out.shift_remove(name.as_str());
            }
            out.retain(|_, v| !matches!(v, Value::Lambda(..)));
        }
        if let Some(fields) = &requested_output_fields {
            out.retain(|name, _| fields.contains(&**name));
        }
        // If the module declares any `@Deprecated` properties, attach a
        // minimal ObjectSource carrying just the deprecation map so field
        // access can warn lazily. Modules without @Deprecated keep `None`
        // source to avoid changing amend behavior in the common case.
        // An abstract module also keeps a source, so `new` can reject it.
        let deprecated = collect_deprecated(&module.body);
        let is_abstract = module_is_abstract(module);
        let source = if deprecated.is_empty() && poisoned_members.is_empty() && !is_abstract {
            None
        } else {
            Some(Arc::new(ObjectSource {
                entries: Vec::new().into(),
                captured: SourceScope::default(),
                body_members: HashSet::default(),
                is_open: true,
                is_abstract,
                type_name: None,
                // An abstract module records its name for the `new` error.
                type_identity: is_abstract.then(|| module_display_name(module, path)),
                parent_type_names: Vec::new(),
                parent_type_identities: Vec::new(),
                entry_scopes: Vec::new(),
                evaluated_properties: Vec::new(),
                mapping_value_types: Vec::new(),
                deprecated,
                poisoned_members: (!poisoned_members.is_empty())
                    .then(|| Arc::new(poisoned_members)),
                kind: ObjectKind::Object,
            }))
        };
        let mut effective_late_properties = IndexMap::new();
        for prop in late_inherited_properties
            .iter()
            .chain(module.body.iter().filter_map(|entry| match entry {
                Entry::Property(prop)
                    if !has_modifier(&prop.modifiers, Modifier::Local) && prop.name != "output" =>
                {
                    Some(prop)
                }
                _ => None,
            }))
        {
            effective_late_properties.insert(prop.name.clone(), prop.clone());
        }
        self.module_scopes.insert(
            path.to_path_buf(),
            ModuleScopeSnapshot {
                values: scope.flatten(),
                type_aliases: scope.flatten_type_aliases(),
                late_properties: effective_late_properties.into_values().collect(),
            },
        );
        // An imported module whose failed property this output renders fails
        // the output, as rendering that property directly would.
        if depth == 0
            && self.rendered_member_failed
            && let Some(message) = out.values().find_map(rendered_member_failure)
        {
            return Err(Error::Eval(message.to_string()));
        }
        Ok(Value::Object(Arc::new(out), source))
    }

    fn eval_property(
        &mut self,
        prop: &Property,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Some(expr) = &prop.value {
            let mut value = self.eval_expr(expr, scope, depth)?;
            apply_mapping_type_annotation(&mut value, prop.type_ann.as_ref());
            return Ok(Some(value));
        }
        if let Some(body) = &prop.body {
            let mut nullable_default = prop
                .type_ann
                .as_ref()
                .and_then(|ty| nullable_inner_default(ty, scope));
            if let Some(default) = &mut nullable_default {
                apply_mapping_type_annotation(default, prop.type_ann.as_ref());
            }
            // A body amendment amends a member the receiver itself holds. A
            // same-named property of an enclosing object, declared or
            // inherited, is a different member and must not become the base.
            let scoped = scope
                .get(&prop.name)
                .filter(|_| scope.vars.contains_key(prop.name.as_str()));
            let amendment_base = scoped
                .filter(|value| !is_null_value(value))
                .or(nullable_default.as_ref())
                .or(scoped);
            if matches!(amendment_base, Some(Value::List(_)))
                || (!matches!(amendment_base, Some(Value::Object(..)))
                    && (prop.type_ann.as_ref().is_some_and(type_is_listing)
                        || entries_are_listing_amendment(body)))
            {
                let mut items = match amendment_base {
                    Some(Value::List(existing)) => existing.clone(),
                    _ => ListValue::new(ListKind::Listing, Vec::new()),
                };
                let mut amendment_scope = scope.child();
                amendment_scope.set("super", Value::List(items.clone()));
                amendment_scope.receiver_entries = Some(body.clone());
                amendment_scope.receiver_list_base = Some(items.len());
                self.eval_listing_entries(body, &amendment_scope, depth, items.make_mut())?;
                return Ok(Some(Value::List(items)));
            }
            // `foo { ... }` — object body amendment.
            // If the property already has a value in scope (e.g., from a base class),
            // amend that value so its ObjectSource (type info, default template) is preserved.
            if let Some(Value::Object(existing_map, Some(src))) = amendment_base {
                if !src.mapping_value_types.is_empty() {
                    // Mapping ObjectSource entries are mapping body entries such as
                    // `default` and dynamic keys. Rebuild the entry map with the
                    // type-aware evaluator so single-type and union mappings both keep
                    // mapping defaults plus converter type metadata after amendment.
                    let (inherited_scope, mut amendment_scope) =
                        mapping_amendment_scopes(src.scope(), src.scope_declared(), scope);
                    amendment_scope.set(
                        "super",
                        Value::Object(Arc::clone(existing_map), Some(Arc::clone(src))),
                    );
                    let mut receiver_entries = existing_map
                        .keys()
                        .map(|key| Entry::DynProperty(Expr::String(Arc::clone(key)), Expr::Null))
                        .collect::<Vec<_>>();
                    receiver_entries.extend_from_slice(body);
                    amendment_scope.receiver_entries = Some(Arc::new(receiver_entries));
                    let value_type_defaults = src
                        .mapping_value_types
                        .iter()
                        .filter_map(|name| {
                            resolve_dotted(&inherited_scope, name)
                                .map(|value| (name.clone(), value))
                        })
                        .collect::<Vec<_>>();
                    let inherited_default =
                        self.find_default_template(&src.entries, &inherited_scope, depth)?;
                    let mut amended = ObjectMap::default();
                    self.eval_mapping_entries_with_type_default(
                        &src.entries,
                        &inherited_scope,
                        depth,
                        &mut amended,
                        &value_type_defaults,
                        &src.mapping_value_types,
                        MappingInheritedDefault::default(),
                    )?;
                    amended.extend(existing_map.iter().map(|(k, v)| (k.clone(), v.clone())));
                    self.eval_mapping_entries_with_type_default(
                        body,
                        &amendment_scope,
                        depth,
                        &mut amended,
                        &value_type_defaults,
                        &src.mapping_value_types,
                        MappingInheritedDefault {
                            value: inherited_default,
                            entries: find_default_body_entries(&src.entries),
                        },
                    )?;
                    return Ok(Some(Value::Object(
                        Arc::new(amended),
                        Some(Arc::clone(src)),
                    )));
                }
                return Ok(Some(self.eval_amended_object(
                    existing_map,
                    src,
                    body,
                    scope,
                    depth,
                )?));
            }
            let mut body_scope = scope.child();
            body_scope.set("super", Value::Object(Arc::default(), None));
            let val = self.eval_entries(body, &body_scope, depth)?;
            return Ok(Some(val));
        }
        if let Some(ty) = &prop.type_ann {
            let mut default = type_default_value(ty, scope);
            if let Some(default) = &mut default {
                apply_mapping_type_annotation(default, Some(ty));
            }
            if default.is_none()
                && matches!(ty, crate::parser::TypeExpr::Union(variants) if !variants.iter().any(is_default_type))
            {
                return Err(Error::Eval(format!(
                    "union property '{}' has no selected default",
                    prop.name
                )));
            }
            return Ok(default);
        }
        Ok(None)
    }

    fn eval_entries(&mut self, entries: &Body, scope: &Scope, depth: usize) -> Result<Value> {
        let mut receiver_scope = scope.clone();
        receiver_scope.receiver_entries = Some(entries.clone());
        receiver_scope.receiver_list_base = None;
        self.eval_entries_with_lexical_scopes(entries, &receiver_scope, depth, None, None)
    }

    fn eval_entries_with_lexical_scopes(
        &mut self,
        entries: &Body,
        scope: &Scope,
        depth: usize,
        entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
        inherited_source: Option<&ObjectSource>,
    ) -> Result<Value> {
        let mut child_scope = scope.child();
        let entry_owners = entry_scope_owners(entries, entry_scopes, inherited_source);
        let own_body = own_body_names(entries, entry_scopes, inherited_source);
        // Entries without a captured scope belong to the object's own
        // definition body. Bindings they see as declared are that body's
        // members; members the object inherits from elsewhere (a parent class
        // or an amendment) are not, so a nested object's inherited member of
        // the same name still resolves through its own `this`.
        let binds_declared = |name: &str| own_body.as_ref().is_none_or(|own| own.contains(name));
        let own_body_scope = own_body.as_ref().map(|own| (scope, own));
        if let Some(source) = inherited_source {
            for entry in source.entries.iter() {
                if let Entry::Property(prop) = entry
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                    && source.evaluated_properties.contains(&prop.name)
                    && let Some(value) = source.scope().get(prop.name.as_str())
                {
                    child_scope.set(&prop.name, value.clone());
                }
            }
        }
        // Locals of an enclosing object that alias its `this` (`local self =
        // this`) hold the same snapshot. Drop the ones this body never names so
        // they are not captured by the object built here; `outer` keeps them
        // when the body uses `outer`, since `outer.self` would still reach them.
        let unused_this_aliases = {
            let aliases = scope.visible_this_aliases();
            if aliases.is_empty() {
                aliases
            } else {
                let refs = self.body_referenced_roots(entries);
                if refs.contains("outer") {
                    Vec::new()
                } else {
                    aliases
                        .into_iter()
                        .filter(|name| !refs.contains(name))
                        .collect()
                }
            }
        };
        // `outer` is only reachable by name, so a body that never mentions it
        // (the common case) skips flattening the enclosing scope for it.
        if entries_mention(entries, "outer") || scope.type_aliases_mention("outer") {
            // Set `outer` to a snapshot of the parent scope's variables as an object.
            // Also insert Null for any nullable-no-default properties declared in these
            // entries but absent from the parent scope, so that `outer.optionalProp`
            // resolves to Null rather than failing with "field not found".
            let mut outer_map = scope.flatten();
            // `this` inside the body is rebound to the new object, so the parent's
            // `this` snapshot is unreachable through `outer`. Leaving it out keeps
            // nested objects from holding a reference to the parent's property map,
            // which would otherwise force a full copy on every parent insert.
            outer_map.shift_remove("this");
            for name in &unused_this_aliases {
                outer_map.shift_remove(name.as_str());
            }
            for entry in entries.iter() {
                if let Entry::Property(prop) = entry
                    && prop.value.is_none()
                    && prop.body.is_none()
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                    && !outer_map.contains_key(prop.name.as_str())
                    && matches!(prop.type_ann, Some(crate::parser::TypeExpr::Nullable(_)))
                {
                    outer_map.insert(prop.name.as_str().into(), Value::Null);
                }
            }
            let outer_obj = Value::Object(Arc::new(outer_map), None);
            child_scope.set("outer", outer_obj);
        }
        // Class-as-a-function definitions commonly use `local self = this` so
        // output properties can close over the amended instance. Bind `this`
        // before locals are evaluated, then keep direct aliases synchronized as
        // properties populate the instance.
        let mut all_props: Arc<ObjectMap> = Arc::default();
        let mut this_aliases = Vec::new();
        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
        // First pass: collect locals, class definitions, and type aliases in
        // declaration order so they can reference each other correctly.
        // Non-lambda locals are evaluated eagerly; lambda locals are deferred
        // to a second pass so they capture the fully-populated scope.
        let mut deferred_lambdas: Vec<(String, &crate::parser::Expr, usize)> = Vec::new();
        for (entry_index, entry) in entries.iter().enumerate() {
            // Only locals, classes and type aliases are handled in this pass,
            // so build the entry's scope only for those.
            if !matches!(
                entry,
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some()
            ) && !matches!(entry, Entry::ClassDef(..) | Entry::TypeAlias(..))
            {
                continue;
            }
            let active_scope = scope_for_object_entry(
                entry_index,
                &child_scope,
                entry_scopes,
                &entry_owners,
                own_body_scope,
            );
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    let expr = prop.value.as_ref().unwrap();
                    // Bind every local in declaration order so later locals and
                    // entries can reference it (e.g. a non-lambda local that
                    // calls a lambda local defined just above it).
                    let result = self.eval_expr(expr, &active_scope, depth);
                    // Release the entry scope before binding, as for properties.
                    drop(active_scope);
                    match result {
                        Ok(val) => {
                            if binds_declared(&prop.name) {
                                child_scope.declare(&prop.name, val);
                            } else {
                                child_scope.set(&prop.name, val);
                            }
                            if matches!(expr, Expr::Ident(name) if name == "this" || this_aliases.contains(name))
                            {
                                this_aliases.push(prop.name.clone());
                                child_scope.mark_this_alias(&prop.name);
                            }
                        }
                        Err(Error::Eval(message)) if binds_declared(&prop.name) => {
                            child_scope.declare_poisoned(prop.name.clone(), message)
                        }
                        Err(Error::Eval(message)) => child_scope.poison(prop.name.clone(), message),
                        Err(error) => return Err(error),
                    }
                    if matches!(expr, crate::parser::Expr::Lambda(..)) {
                        // Lambda evaluation only captures the current scope; it
                        // does not run the body. Bind once for declaration-order
                        // visibility, then re-bind after properties for late
                        // binding of overrides.
                        deferred_lambdas.push((prop.name.clone(), expr, entry_index));
                    }
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self.eval_class_def(
                        name,
                        class_mods,
                        parent.as_deref(),
                        body,
                        &active_scope,
                        depth,
                    )?;
                    if binds_declared(name) {
                        child_scope.declare(name, defaults);
                    } else {
                        child_scope.set(name, defaults);
                    }
                }
                Entry::TypeAlias(name, ty) => {
                    let mut resolved_scope = active_scope;
                    self.eval_type_alias(name, ty, &mut resolved_scope);
                    child_scope.set_type_alias(name.clone(), ty.clone());
                    if let Some(value) = resolved_scope.vars.get(name.as_str()) {
                        child_scope.set(name, value.clone());
                    }
                }
                _ => {}
            }
        }

        let mut default_template: Option<Value> = None;
        for (entry_index, entry) in entries.iter().enumerate() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if prop.name != "default" || has_modifier(&prop.modifiers, Modifier::Local) {
                continue;
            }
            let mut active_scope = scope_for_object_entry(
                entry_index,
                &child_scope,
                entry_scopes,
                &entry_owners,
                own_body_scope,
            );
            if let Some(template) = &default_template {
                active_scope.set("default", template.clone());
            }
            default_template = self.eval_property(prop, &active_scope, depth)?;
        }

        let mut map: ObjectMap = ObjectMap::default();
        // Names of members produced by generators and not since rebound by a
        // property entry.
        let mut generated: HashSet<Arc<str>> = HashSet::default();
        for (entry_index, entry) in entries.iter().enumerate() {
            match entry {
                Entry::Property(prop) => {
                    let mods = &prop.modifiers;
                    if has_modifier(mods, Modifier::Local) {
                        continue;
                    }
                    // Skip the `default` property — it's a template, not an output entry
                    if prop.name == "default" && default_template.is_some() {
                        continue;
                    }
                    if has_modifier(mods, Modifier::Abstract)
                        && prop.value.is_none()
                        && prop.body.is_none()
                    {
                        continue; // abstract without value — skip (must be overridden)
                    }
                    refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                    let mut active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    // `o { ... }` amends a member an earlier generator produced,
                    // which is in the object but not bound in scope.
                    if prop.value.is_none()
                        && prop.body.is_some()
                        && generated.contains(prop.name.as_str())
                        && let Some(existing) = all_props.get(prop.name.as_str())
                    {
                        active_scope.set(&prop.name, existing.clone());
                    }
                    let value = self.eval_property(prop, &active_scope, depth)?;
                    // Release the entry scope first: it may share the object
                    // scope's bindings, which binding the value would then copy.
                    drop(active_scope);
                    if let Some(v) = value {
                        generated.remove(prop.name.as_str());
                        if binds_declared(&prop.name) {
                            child_scope.declare(&prop.name, v.clone());
                        } else {
                            child_scope.set(&prop.name, v.clone());
                        }
                        entry_owners.release_this(&this_aliases);
                        props_insert(
                            &mut child_scope,
                            &this_aliases,
                            &mut all_props,
                            prop.name.clone(),
                            v.clone(),
                        );
                        if !has_modifier(mods, Modifier::Hidden) {
                            map.insert(prop.name.as_str().into(), v);
                        }
                        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                    }
                }
                Entry::DynProperty(key_expr, val_expr) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let key = self.eval_expr(key_expr, &active_scope, depth)?;
                    let key_str = value_to_key(&key)?;
                    // `["key"] { ... }` amends an entry inherited from the parent
                    // (for example when amending an untyped `Mapping`) rather than
                    // replacing it.
                    if let Expr::ObjectBody(body) = val_expr
                        && let Some(existing @ (Value::Object(..) | Value::List(_))) =
                            map.get(&key_str).cloned()
                    {
                        // A listing amendment only takes elements, so a property
                        // would otherwise be dropped silently. Reject it as Pkl does.
                        if matches!(existing, Value::List(_))
                            && let Some(name) = find_listing_body_property(body)
                        {
                            return Err(Error::Eval(format!(
                                "cannot amend listing entry '{key_str}' with property '{name}': \
                                 object of type Listing cannot have a property (other than default)"
                            )));
                        }
                        let val =
                            self.eval_value_amendment(existing, body, &active_scope, depth)?;
                        drop(active_scope);
                        entry_owners.release_this(&this_aliases);
                        props_insert(
                            &mut child_scope,
                            &this_aliases,
                            &mut all_props,
                            key_str.clone(),
                            val.clone(),
                        );
                        map.insert(key_str, val);
                        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        continue;
                    }
                    let val = if let Some(Value::Object(template_map, Some(src))) =
                        &default_template
                        && let Expr::ObjectBody(body) = val_expr
                    {
                        // Default template has ObjectSource — use eval_amended_object
                        // so nested property amendments work properly.
                        let mut result = self.eval_amended_object(
                            template_map,
                            src,
                            body,
                            &active_scope,
                            depth,
                        )?;
                        // Propagate the template's type_name so converters can match.
                        if let Some(ref tn) = src.type_name
                            && let Value::Object(_, ref mut result_src) = result
                        {
                            let new_src = match result_src.take() {
                                Some(s) => {
                                    let mut ns = Arc::unwrap_or_clone(s);
                                    ns.type_name = Some(tn.clone());
                                    ns
                                }
                                None => ObjectSource {
                                    entries: vec![].into(),
                                    captured: SourceScope::default(),
                                    body_members: HashSet::default(),
                                    is_open: true,
                                    is_abstract: false,
                                    type_name: Some(tn.clone()),
                                    type_identity: src.type_identity.clone(),
                                    parent_type_names: src.parent_type_names.clone(),
                                    parent_type_identities: src.parent_type_identities.clone(),
                                    entry_scopes: Vec::new(),
                                    evaluated_properties: Vec::new(),
                                    mapping_value_types: Vec::new(),
                                    deprecated: merge_deprecated(&src.deprecated, body),
                                    poisoned_members: None,
                                    kind: ObjectKind::Object,
                                },
                            };
                            *result_src = Some(std::sync::Arc::new(new_src));
                        }
                        result
                    } else {
                        let mut val = self.eval_expr(val_expr, &active_scope, depth)?;
                        if let Some(ref tpl) = default_template {
                            val = merge_values(tpl.clone(), val);
                        }
                        val
                    };
                    drop(active_scope);
                    entry_owners.release_this(&this_aliases);
                    props_insert(
                        &mut child_scope,
                        &this_aliases,
                        &mut all_props,
                        key_str.clone(),
                        val.clone(),
                    );
                    map.insert(key_str, val);
                    refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                }
                Entry::Spread(expr) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let val = self.eval_expr(expr, &active_scope, depth)?;
                    check_iterable(&val)?;
                    if let Value::Object(m, _) = val {
                        drop(active_scope);
                        entry_owners.release_this(&this_aliases);
                        props_extend(
                            &mut child_scope,
                            &this_aliases,
                            &mut all_props,
                            m.iter().map(|(k, v)| (k.clone(), v.clone())),
                        );
                        map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let collection = self.eval_expr(&fgen.collection, &active_scope, depth)?;
                    let items = collection_to_items(collection)?;
                    for (k, v) in items {
                        let mut iter_scope = active_scope.child();
                        iter_scope.set(&fgen.val_var, v);
                        if let Some(key_var) = &fgen.key_var {
                            iter_scope.set(key_var, k);
                        }
                        let body_val = self.eval_entries_with_lexical_scopes(
                            &fgen.body,
                            &iter_scope,
                            depth,
                            None,
                            None,
                        )?;
                        if let Value::Object(m, _) = body_val {
                            entry_owners.release_this(&this_aliases);
                            props_extend(
                                &mut child_scope,
                                &this_aliases,
                                &mut all_props,
                                m.iter().map(|(k, v)| (k.clone(), v.clone())),
                            );
                            generated.extend(m.keys().cloned());
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        }
                    }
                }
                Entry::WhenGenerator(wgen) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let cond = self.eval_expr(&wgen.condition, &active_scope, depth)?;
                    if is_truthy(&cond) {
                        let body_val = self.eval_entries_with_lexical_scopes(
                            &wgen.body,
                            &active_scope,
                            depth,
                            None,
                            None,
                        )?;
                        if let Value::Object(m, _) = body_val {
                            entry_owners.release_this(&this_aliases);
                            props_extend(
                                &mut child_scope,
                                &this_aliases,
                                &mut all_props,
                                m.iter().map(|(k, v)| (k.clone(), v.clone())),
                            );
                            generated.extend(m.keys().cloned());
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        }
                    } else if let Some(else_body) = &wgen.else_body {
                        let else_val = self.eval_entries_with_lexical_scopes(
                            else_body,
                            &active_scope,
                            depth,
                            None,
                            None,
                        )?;
                        if let Value::Object(m, _) = else_val {
                            entry_owners.release_this(&this_aliases);
                            props_extend(
                                &mut child_scope,
                                &this_aliases,
                                &mut all_props,
                                m.iter().map(|(k, v)| (k.clone(), v.clone())),
                            );
                            generated.extend(m.keys().cloned());
                            map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        }
                    }
                }
                Entry::Elem(_) => {} // bare elements only valid in Listing bodies
                Entry::ClassDef(..) | Entry::TypeAlias(..) => {} // handled in scope setup
            }
        }
        // Evaluate deferred local lambdas (function definitions) AFTER all
        // properties so they capture overridden values (late binding).
        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
        for (name, expr, entry_index) in deferred_lambdas {
            let active_scope = scope_for_object_entry(
                entry_index,
                &child_scope,
                entry_scopes,
                &entry_owners,
                own_body_scope,
            );
            let val = self.eval_expr(expr, &active_scope, depth)?;
            drop(active_scope);
            child_scope.set(name, val);
        }
        let hidden_aliases = unused_this_aliases
            .iter()
            // A binding of the same name made by this body shadows the alias.
            .filter(|name| {
                !child_scope.vars.contains_key(name.as_str())
                    && !child_scope.poisoned.contains_key(name.as_str())
            })
            .map(|name| name_of(name))
            .collect();
        let source = ObjectSource {
            entries: entries.clone(),
            captured: SourceScope::lazy(&child_scope, hidden_aliases, Vec::new()),
            body_members: own_body.clone().unwrap_or_else(|| {
                entries
                    .iter()
                    .filter_map(entry_member_name)
                    .cloned()
                    .collect()
            }),
            is_open: true, // default: allow new properties
            is_abstract: false,
            type_name: None,
            type_identity: None,
            parent_type_names: Vec::new(),
            parent_type_identities: Vec::new(),
            entry_scopes: entry_scopes.map(<[_]>::to_vec).unwrap_or_default(),
            evaluated_properties: all_props.keys().map(|k| k.to_string()).collect(),
            mapping_value_types: Vec::new(),
            deprecated: collect_deprecated(entries),
            poisoned_members: None,
            kind: ObjectKind::Object,
        };
        Ok(Value::Object(Arc::new(map), Some(Arc::new(source))))
    }

    /// Evaluate a class definition, optionally inheriting from a parent class.
    ///
    /// If `parent_name` is provided, the parent class is looked up in scope,
    /// its defaults are used as a base, and `super` is bound to the parent
    /// value so the child class body can reference it.
    fn eval_class_def(
        &mut self,
        class_name: &str,
        class_mods: &[Modifier],
        parent_name: Option<&str>,
        body: &Body,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let parent_val = parent_name.and_then(|name| resolve_dotted(scope, name));
        if let Some(parent_name) = parent_name {
            check_supertype(scope, class_name, parent_name, parent_val.as_ref())?;
        }
        // A parent class that failed to evaluate fails its subclasses too,
        // rather than leaving them without the inherited members.
        if parent_val.is_none()
            && let Some(message) = parent_name.and_then(|name| poisoned_member(scope, name))
        {
            return Err(Error::Eval(message));
        }
        let (parent_type_names, parent_type_identities) = match &parent_val {
            Some(Value::Object(_, Some(source))) => {
                let names = source
                    .type_name
                    .iter()
                    .cloned()
                    .chain(source.parent_type_names.iter().cloned())
                    .collect();
                let identities = source
                    .type_identity
                    .iter()
                    .cloned()
                    .chain(source.parent_type_identities.iter().cloned())
                    .collect();
                (names, identities)
            }
            _ => (Vec::new(), Vec::new()),
        };

        let mut child_scope = scope.child();
        if let Some(ref pv) = parent_val {
            child_scope.set("super", pv.clone());
        }

        let child_defaults = self.eval_entries(body, &child_scope, depth + 1)?;
        if let Some(Value::Object(parent_map, parent_src)) = parent_val {
            // Merge: parent defaults first, child overrides on top
            let mut merged: ObjectMap = (*parent_map).clone();
            if let Value::Object(child_map, child_src) = child_defaults {
                for (k, v) in child_map.iter() {
                    merged.insert(k.clone(), v.clone());
                }
                // Preserve the child's ObjectSource for late binding,
                // but prepend parent entries so inherited props are available
                let source = if let Some(child_arc) = child_src {
                    let mut src = (*child_arc).clone();
                    // Collect child property names (including dynamic string-key entries)
                    let child_names: std::collections::HashSet<String> = body
                        .iter()
                        .filter_map(|e| match e {
                            Entry::Property(p) => Some(p.name.clone()),
                            Entry::DynProperty(Expr::String(s), _) => Some(s.to_string()),
                            _ => None,
                        })
                        .collect();
                    if let Some(psrc) = parent_src {
                        let mut combined_entries = Vec::new();
                        let mut combined_entry_scopes = Vec::new();
                        let mut combined_evaluated_properties = Vec::new();
                        let mut inherited_property_values = IndexMap::new();
                        // One snapshot for every entry of the parent's body, so
                        // they resolve each other as members of the same body.
                        let parent_scope = Arc::new({
                            let mut captured = capture_object_source_scope(&psrc);
                            captured.body_members.extend(
                                psrc.entries
                                    .iter()
                                    .enumerate()
                                    .filter(|(index, _)| {
                                        psrc.entry_scopes.get(*index).is_none_or(Option::is_none)
                                    })
                                    .filter_map(|(_, entry)| entry_member_name(entry).cloned()),
                            );
                            captured
                        });
                        for (entry_index, pe) in psrc.entries.iter().enumerate() {
                            if let Entry::Property(p) = pe
                                && !child_names.contains(&p.name)
                            {
                                combined_entries.push(pe.clone());
                                combined_entry_scopes.push(
                                    psrc.entry_scopes
                                        .get(entry_index)
                                        .and_then(Clone::clone)
                                        .or_else(|| Some(Arc::clone(&parent_scope))),
                                );
                                if psrc.evaluated_properties.contains(&p.name) {
                                    combined_evaluated_properties.push(p.name.clone());
                                    if let Some(value) = psrc.scope().get(p.name.as_str()) {
                                        inherited_property_values
                                            .insert(p.name.clone(), value.clone());
                                    }
                                }
                            }
                        }
                        let child_entries = std::mem::take(&mut src.entries);
                        let child_entry_scopes = std::mem::take(&mut src.entry_scopes);
                        for (entry_index, entry) in
                            Arc::unwrap_or_clone(child_entries).into_iter().enumerate()
                        {
                            combined_entries.push(entry);
                            combined_entry_scopes
                                .push(child_entry_scopes.get(entry_index).cloned().unwrap_or(None));
                        }
                        combined_evaluated_properties.extend(src.evaluated_properties);
                        // Keep the ObjectSource invariant used by amendment
                        // seeding: every evaluated property name resolves to
                        // that property's value in `scope`, even when a child
                        // module has a same-named lexical binding.
                        for name in inherited_property_values.keys() {
                            src.captured.parts_mut().declared.remove(name.as_str());
                        }
                        src.captured.parts_mut().values.extend(
                            inherited_property_values
                                .into_iter()
                                .map(|(k, v)| (Arc::from(k), v)),
                        );
                        src.entries = combined_entries.into();
                        src.entry_scopes = combined_entry_scopes;
                        src.evaluated_properties = combined_evaluated_properties;
                    }
                    Some(Arc::new(src))
                } else {
                    None
                };
                Ok(Value::Object(Arc::new(merged), source))
            } else {
                Ok(Value::Object(Arc::new(merged), None))
            }
        } else {
            Ok(child_defaults)
        }
        .map(|val| {
            // Set is_open flag and class_name on the result's ObjectSource
            let is_open = has_modifier(class_mods, Modifier::Open);
            if let Value::Object(map, Some(src)) = val {
                let data_property_names = body
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::Property(prop)
                            if !has_modifier(&prop.modifiers, Modifier::Local) =>
                        {
                            Some(prop.name.as_str())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let schema_member_names = body
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::ClassDef(name, ..)
                            if !data_property_names.contains(name.as_str()) =>
                        {
                            Some(name.as_str())
                        }
                        Entry::Property(prop)
                            if matches!(prop.value, Some(Expr::Lambda(..)))
                                && has_modifier(&prop.modifiers, Modifier::Local)
                                && !data_property_names.contains(prop.name.as_str()) =>
                        {
                            Some(prop.name.as_str())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let mut map = map;
                Arc::make_mut(&mut map).retain(|key, _| !schema_member_names.contains(&**key));
                let mut new_src = Arc::unwrap_or_clone(src);
                new_src.is_open = is_open;
                new_src.is_abstract = has_modifier(class_mods, Modifier::Abstract);
                new_src.type_name = Some(class_name.to_string());
                new_src.type_identity = Some(scope.runtime_type_identity(class_name));
                new_src.parent_type_names = parent_type_names;
                new_src.parent_type_identities = parent_type_identities;
                new_src.kind = ObjectKind::Class;
                Value::Object(map, Some(Arc::new(new_src)))
            } else {
                val
            }
        })
    }

    /// Evaluate a type alias declaration.
    ///
    /// If the aliased type is a named type that exists in scope (e.g. a class),
    /// the alias name is bound to the same value so `new AliasName { ... }` works.
    fn eval_type_alias(&self, name: &str, ty: &crate::parser::TypeExpr, scope: &mut Scope) {
        // Store the TypeExpr so `is`/`as` can resolve alias names to their definitions
        scope.set_type_alias(name.to_string(), ty.clone());
        // Also mark the name as a type among the bindings, which a function
        // captures (its alias map is not), so `new Alias {}` inside one is
        // still known to name a type.
        scope.declare(type_alias_marker(name), Value::Bool(true));
        match ty {
            crate::parser::TypeExpr::Named(target) => {
                // Alias to a class or another alias already in scope
                if let Some(val) = scope.get(target) {
                    scope.declare(name, val.clone());
                }
            }
            crate::parser::TypeExpr::Nullable(inner) => {
                // typealias Foo = Bar? -- alias to the inner type
                if let crate::parser::TypeExpr::Named(target) = inner.as_ref()
                    && let Some(val) = scope.get(target)
                {
                    scope.declare(name, val.clone());
                }
            }
            crate::parser::TypeExpr::Constrained(base, _) => {
                self.bind_type_alias_value(name, base, scope);
            }
            // Union types, generics, etc. -- no runtime representation needed
            _ => {}
        }
    }

    fn bind_type_alias_value(&self, name: &str, target: &str, scope: &mut Scope) {
        if let Some(val) = scope.get(target.trim_end_matches('?')).cloned() {
            scope.declare(name, val);
        }
    }

    /// Check a concrete property value after its defining scope is complete.
    /// Defaults and declarations without values are checked when they are
    /// materialized, not merely declared.
    fn check_declared_property_type(
        &mut self,
        prop: &Property,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        if prop.value.is_none() && prop.body.is_none() {
            return Ok(());
        }
        let Some(ty) = &prop.type_ann else {
            return Ok(());
        };
        if !type_is_runtime_checkable(ty, scope)
            || self.eval_type_check(value, ty, scope, depth + 1)?
        {
            return Ok(());
        }
        Err(Error::Eval(format!(
            "property '{}' expected {}, got {}",
            prop.name,
            display_type_expr(ty),
            value_type_name(value)
        )))
    }

    /// Check if a value matches a type expression, including constraint evaluation.
    fn eval_type_check(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<bool> {
        self.eval_type_check_in(val, ty, scope, depth, &mut Vec::new())
    }

    /// Resolve aliases with a guard: malformed or recursively-expanded
    /// aliases must report an evaluation error instead of recursing until the
    /// process stack overflows.
    fn eval_alias_check(
        &mut self,
        val: &Value,
        name: &str,
        resolved: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        if resolving.iter().any(|seen| seen == name) {
            return Err(Error::Eval(format!("type alias '{name}' refers to itself")));
        }
        resolving.push(name.to_string());
        // Long chains (`A0 = A1`, …) are valid Pkl and must not consume the
        // Rust call stack once per alias.  Flatten the simple-name part here;
        // compound targets still go through the regular checker below.
        let mut target = resolved.clone();
        while let crate::parser::TypeExpr::Named(next) = &target {
            let Some(next_target) = scope.get_type_alias(next) else {
                break;
            };
            if resolving.iter().any(|seen| seen == next) {
                while resolving.pop().as_deref() != Some(name) {}
                return Err(Error::Eval(format!("type alias '{next}' refers to itself")));
            }
            resolving.push(next.clone());
            target = next_target.clone();
        }
        let result = self.eval_type_check_in(val, &target, scope, depth + 1, resolving);
        while resolving.pop().as_deref() != Some(name) {}
        result
    }

    fn eval_type_check_in(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        use crate::parser::TypeExpr;
        match ty {
            TypeExpr::Named(name) => {
                if let Some(expected) = string_literal_type_value(name) {
                    return Ok(matches!(val, Value::String(actual) if &**actual == expected));
                }
                // Check if name is a type alias; if so, resolve to the aliased type
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                if let Some(matches) = value_is_class_type(val, name, scope) {
                    return Ok(matches);
                }
                // Otherwise, plain type check
                Ok(value_is_type(val, ty))
            }
            TypeExpr::Constrained(base, constraint) => {
                // First check the base type
                let class_name = base.trim_end_matches('?').split('<').next().unwrap_or(base);
                let base_matches = if base.ends_with('?') && is_null_value(val) {
                    true
                } else if let Some(resolved) = scope.get_type_alias(class_name) {
                    let resolved = resolved.clone();
                    self.eval_alias_check(val, class_name, &resolved, scope, depth, resolving)?
                } else {
                    value_is_class_type(val, class_name, scope)
                        .unwrap_or_else(|| value_is_named_type(val, base))
                };
                if !base_matches {
                    return Ok(false);
                }
                // Evaluate the constraint with `this` bound to the value
                let mut constraint_scope = scope.child();
                constraint_scope.set("this", val.clone());
                // Also bind common properties directly so `length`, `isEmpty` etc. work
                match val {
                    Value::String(s) => {
                        constraint_scope.set("length", Value::Int(s.chars().count() as i64));
                        constraint_scope.set("isEmpty", Value::Bool(s.is_empty()));
                    }
                    Value::Int(_) | Value::Float(_) => {}
                    Value::List(items) => {
                        constraint_scope.set("length", Value::Int(items.len() as i64));
                        constraint_scope.set("isEmpty", Value::Bool(items.is_empty()));
                    }
                    Value::Object(items, _) => {
                        constraint_scope.set("length", Value::Int(items.len() as i64));
                        constraint_scope.set("isEmpty", Value::Bool(items.is_empty()));
                    }
                    _ => {}
                }
                let result = self.eval_expr(constraint, &constraint_scope, depth + 1)?;
                Ok(is_truthy(&result))
            }
            TypeExpr::Nullable(inner) => {
                if is_null_value(val) {
                    return Ok(true);
                }
                self.eval_type_check_in(val, inner, scope, depth, resolving)
            }
            TypeExpr::Union(variants) => {
                // A type pklr does not model cannot be disproved at runtime.
                // Check every modeled variant first, but preserve that
                // conservative acceptance only if none of them matches.
                let mut has_uncheckable_variant = false;
                for v in variants {
                    if !type_is_runtime_checkable(v, scope) {
                        has_uncheckable_variant = true;
                    } else if self.eval_type_check_in(val, v, scope, depth, resolving)? {
                        return Ok(true);
                    }
                }
                Ok(has_uncheckable_variant)
            }
            TypeExpr::Generic(name, _) => {
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                Ok(value_is_type(val, ty))
            }
        }
    }

    /// Evaluate an amended object with late binding.
    ///
    /// Merges the base object's original entries with the overlay entries,
    /// then re-evaluates everything so that dependent properties pick up
    /// overridden values.
    /// `referenced_roots` of an object body, computed once per body: a class
    /// or object amended many times would otherwise be analyzed every time.
    fn body_referenced_roots(&mut self, body: &crate::parser::Body) -> Arc<HashSet<String>> {
        let key = Arc::as_ptr(body) as usize;
        if let Some((_, roots)) = self.body_roots_cache.get(&key) {
            return Arc::clone(roots);
        }
        let roots = Arc::new(referenced_roots(body));
        self.body_roots_cache
            .insert(key, (Arc::clone(body), Arc::clone(&roots)));
        roots
    }

    fn eval_amended_object(
        &mut self,
        base_map: &Arc<ObjectMap>,
        base_source: &Arc<ObjectSource>,
        overlay_entries: &[Entry],
        current_scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // A module object's source can carry only error metadata. It has no
        // entries to rebuild the object from, so amend the evaluated members.
        if base_source.is_metadata_only() {
            return self.eval_value_amendment(
                Value::Object(Arc::clone(base_map), Some(Arc::clone(base_source))),
                &Arc::new(overlay_entries.to_vec()),
                current_scope,
                depth,
            );
        }
        // Every amendment of a class instance goes through here: `new C {}`,
        // property bodies, and entries of typed Mappings and Listings.
        check_no_elements(base_source, overlay_entries)?;
        let base_entries = &base_source.entries;
        let base_scope = base_source.scope();
        // Build merged entry list preserving base order.
        // Overridden properties are replaced in-place so that later
        // properties that reference them see the new value.
        let mut merged: Vec<Entry> = Vec::new();
        let mut merged_entry_scopes = Vec::new();
        let mut amendment_scope = capture_scope(current_scope);
        let mut parent_members = (**base_map).clone();
        // Hidden properties are absent from the rendered map but still
        // accessible through super. Mapping keys, in contrast, need the map:
        // they need not have a binding in the lexical scope.
        for name in &base_source.evaluated_properties {
            if !parent_members.contains_key(name.as_str())
                && let Some(value) = base_source.scope().get(name.as_str())
            {
                parent_members.insert(name.as_str().into(), value.clone());
            }
        }
        amendment_scope.values.insert(
            "super".into(),
            Value::Object(Arc::new(parent_members), Some(Arc::clone(base_source))),
        );
        let amendment_captured = Arc::new(CapturedScope {
            body_members: overlay_entries
                .iter()
                .filter_map(entry_member_name)
                .cloned()
                .collect(),
            ..amendment_scope
        });
        let amendment_entry_scope = Some(Arc::clone(&amendment_captured));
        let mut overlay_by_name: FxIndexMap<&str, &Entry> = FxIndexMap::default();
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry {
                overlay_by_name.insert(prop.name.as_str(), entry);
            }
        }
        let last_base_property_index = base_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                Entry::Property(prop) => Some((prop.name.as_str(), index)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        // Walk base entries: substitute overridden properties in-place.
        // If the overlay has a body amendment (no `=`), keep the base entry first
        // so its value is in scope, then add the overlay body entry after.
        let mut used_overlay: HashSet<&str> = HashSet::default();
        for (entry_index, entry) in base_entries.iter().enumerate() {
            let inherited_entry_scope = base_source
                .entry_scopes
                .get(entry_index)
                .cloned()
                .unwrap_or(None);
            if let Entry::Property(prop) = entry
                && let Some(replacement) = overlay_by_name.get(prop.name.as_str())
            {
                if let Entry::Property(overlay_prop) = replacement
                    && overlay_prop.body.is_some()
                    && overlay_prop.value.is_none()
                    && (prop.value.is_some() || prop.body.is_some() || prop.type_ann.is_some())
                {
                    // Body amendment: keep base entry (for its value) AND overlay
                    // entry (for body amendment). eval_property will see the base
                    // value in scope and amend it.
                    merged.push(entry.clone());
                    merged_entry_scopes.push(inherited_entry_scope.clone());
                    // A prior amendment leaves the original property and its
                    // body entry in ObjectSource. Preserve that chain, but add
                    // this amendment only once after its final entry.
                    if last_base_property_index.get(prop.name.as_str()) != Some(&entry_index) {
                        continue;
                    }
                }
                let mut replacement = (*replacement).clone();
                if let Entry::Property(overlay_prop) = &mut replacement {
                    let overlay_prop = Arc::make_mut(overlay_prop);
                    if overlay_prop.type_ann.is_none() {
                        overlay_prop.type_ann = prop.type_ann.clone();
                    }
                    // `hidden` is declared on the class property. An overlay
                    // that assigns it (`new Step { staged = true }` or
                    // `(step) { staged = true }`) keeps it out of the output.
                    if has_modifier(&prop.modifiers, Modifier::Hidden)
                        && !has_modifier(&overlay_prop.modifiers, Modifier::Hidden)
                    {
                        overlay_prop.modifiers.push(Modifier::Hidden);
                    }
                }
                merged.push(replacement);
                merged_entry_scopes.push(amendment_entry_scope.clone());
                used_overlay.insert(prop.name.as_str());
                continue;
            }
            merged.push(entry.clone());
            merged_entry_scopes.push(inherited_entry_scope);
        }

        // Append overlay entries that are genuinely new (not replacing a base entry)
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry
                && used_overlay.contains(prop.name.as_str())
            {
                continue; // already placed in-order above
            }
            merged.push(entry.clone());
            merged_entry_scopes.push(amendment_entry_scope.clone());
        }

        // Build scope: start with the base's captured scope, then layer current scope
        // The new scope starts empty, so the base's maps are copied whole
        // rather than rebinding each name.
        let mut eval_scope = Scope {
            type_namespace: object_source_type_namespace(base_source),
            ..Scope::default()
        };
        if !base_scope.is_empty() {
            eval_scope.vars = Arc::new(base_scope.clone());
            let base_declared = base_source.scope_declared();
            if !base_declared.is_empty() {
                eval_scope.declared = Arc::new(
                    base_declared
                        .iter()
                        .filter(|name| base_scope.contains_key(&***name))
                        .cloned()
                        .collect(),
                );
            }
        }
        for (name, identity) in base_source.scope_module_identities() {
            eval_scope.set_module_identity(name.clone(), identity.clone());
        }
        let base_type_aliases = base_source.scope_type_aliases();
        if !base_type_aliases.is_empty() {
            eval_scope.type_aliases = Arc::new(base_type_aliases.clone());
        }
        // Layer in current scope values (imports, module-level locals, etc.).
        // The same imported module can be field-pruned differently at its
        // definition and use sites. Preserve both partial views so methods
        // retain the classes captured by their definition-site import.
        // A different binding with the same name must not replace a free name
        // used by an inherited entry: those names are lexically bound where
        // the base object was defined, not where it is amended.
        let inherited_references = self.body_referenced_roots(base_entries);
        let mut preserved_inherited_bindings = HashSet::default();
        // The amendment's captured scope is `current_scope` flattened, plus
        // the `super` binding skipped here.
        for (k, v) in &amendment_captured.values {
            // Inherited entries retain their original parent. The overlay's
            // parent is captured separately in amendment_entry_scope.
            if &**k == "super" {
                continue;
            }
            let (k, v) = (k.clone(), v.clone());
            let same_module = eval_scope
                .module_identity(&k)
                .zip(current_scope.module_identity(&k))
                .is_some_and(|(base, current)| base == current);
            let value = if same_module {
                // Still the base's own import, so it keeps the base's mark.
                eval_scope
                    .get(&k)
                    .and_then(|base| merge_partial_module_values(base, &v))
                    .unwrap_or(v)
            } else if inherited_references.contains(&*k) && eval_scope.get(&k).is_some() {
                preserved_inherited_bindings.insert(k.clone());
                continue;
            } else {
                // A use-site binding is not declared in the base's body.
                Arc::make_mut(&mut eval_scope.declared).remove(&*k);
                v
            };
            eval_scope.set_name(k, value);
        }
        for (name, identity) in &amendment_captured.module_identities {
            if !preserved_inherited_bindings.contains(name.as_str()) {
                eval_scope.set_module_identity(name.clone(), identity.clone());
            }
        }
        // Preserve definition-site aliases used by inherited entries. Overlay
        // entries are evaluated against `current_scope` separately below.
        for (k, ty) in &amendment_captured.type_aliases {
            if !inherited_references.contains(&**k) || eval_scope.get_type_alias(k).is_none() {
                eval_scope.set_type_alias(k.clone(), Arc::clone(ty));
            }
        }
        // Seed Null for nullable-no-default base properties absent from eval_scope.
        // This ensures `outer.optProp` resolves to Null rather than "field not found"
        // when the property was never assigned a value in the base class or any overlay.
        for entry in base_entries.iter() {
            if let Entry::Property(prop) = entry
                && prop.value.is_none()
                && prop.body.is_none()
                && !has_modifier(&prop.modifiers, Modifier::Local)
                && eval_scope.get(&prop.name).is_none()
                && matches!(prop.type_ann, Some(crate::parser::TypeExpr::Nullable(_)))
            {
                eval_scope.set(&prop.name, Value::Null);
            }
        }

        // Evaluate the merged entries (eval_entries handles locals, classes,
        // and evaluates properties in order with each added to scope)
        let merged: Body = Arc::new(merged);
        eval_scope.receiver_entries = Some(merged.clone());
        let mut result = self.eval_entries_with_lexical_scopes(
            &merged,
            &eval_scope,
            depth + 1,
            Some(&merged_entry_scopes),
            Some(base_source),
        )?;
        if let Value::Object(map, Some(source)) = result {
            let mut source = Arc::unwrap_or_clone(source);
            source.entry_scopes = merged_entry_scopes;
            result = Value::Object(map, Some(Arc::new(source)));
        }
        let assigned_property_names = merged
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) if prop.value.is_some() || prop.body.is_some() => {
                    Some(prop.name.as_str())
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        if let Value::Object(map, source) = &result {
            for entry in base_entries.iter() {
                let Entry::Property(prop) = entry else {
                    continue;
                };
                let Some(type_ann) = &prop.type_ann else {
                    continue;
                };
                let value = map.get(prop.name.as_str()).or_else(|| {
                    assigned_property_names
                        .contains(prop.name.as_str())
                        .then(|| {
                            source
                                .as_ref()
                                .and_then(|source| source.scope().get(prop.name.as_str()))
                        })
                        .flatten()
                });
                let Some(value) = value else {
                    continue;
                };
                if type_is_runtime_checkable(type_ann, &eval_scope)
                    && !self.eval_type_check(value, type_ann, &eval_scope, depth + 1)?
                {
                    return Err(Error::Eval(format!(
                        "property '{}' expected {}, got {}",
                        prop.name,
                        display_type_expr(type_ann),
                        value_type_name(value)
                    )));
                }
            }
        }
        // Amending an object preserves its class identity (so `is Foo` and
        // output converters still match). eval_entries does not know the base
        // type, so re-tag the result here.
        match (object_type_metadata(base_source), result) {
            (Some(base_type), Value::Object(map, Some(src))) => {
                let mut new_src = Arc::unwrap_or_clone(src);
                new_src.type_name = Some(base_type.name);
                new_src.type_identity = base_type.identity;
                new_src.parent_type_names = base_type.parent_names;
                new_src.parent_type_identities = base_type.parent_identities;
                Ok(Value::Object(map, Some(Arc::new(new_src))))
            }
            // An amended mapping is still a mapping.
            (None, Value::Object(map, Some(src)))
                if base_source.kind == ObjectKind::Mapping && src.kind != ObjectKind::Mapping =>
            {
                let mut new_src = Arc::unwrap_or_clone(src);
                new_src.kind = ObjectKind::Mapping;
                Ok(Value::Object(map, Some(Arc::new(new_src))))
            }
            (_, other) => Ok(other),
        }
    }

    fn eval_object_body_over_template(
        &mut self,
        template_map: &Arc<ObjectMap>,
        template_src: &Arc<ObjectSource>,
        explicit_map: &ObjectMap,
        body: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // This template combines already-evaluated type and explicit defaults.
        // Its original source describes only the type default, so retain the
        // explicit values as source bindings before applying the entry body.
        // Keep untouched class expressions for late binding to entry overrides.
        let mut source = (**template_src).clone();
        for key in explicit_map.keys() {
            let Some(value) = template_map.get(key) else {
                continue;
            };
            let binding = format!("\0mapping_template:{key}");
            source
                .captured
                .parts_mut()
                .values
                .insert(binding.as_str().into(), value.clone());
            let mut replaced = false;
            for (index, entry) in Arc::make_mut(&mut source.entries).iter_mut().enumerate() {
                if let Entry::Property(prop) = entry
                    && *prop.name == **key
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                {
                    let prop = Arc::make_mut(prop);
                    prop.value = Some(Expr::Ident(binding.clone()));
                    prop.body = None;
                    if let Some(entry_scope) = source.entry_scopes.get_mut(index) {
                        *entry_scope = None;
                    }
                    replaced = true;
                }
            }
            if !replaced {
                Arc::make_mut(&mut source.entries).push(Entry::Property(Arc::new(Property {
                    annotations: Vec::new(),
                    modifiers: Vec::new(),
                    name: key.to_string(),
                    type_ann: None,
                    value: Some(Expr::Ident(binding)),
                    body: None,
                    is_method: false,
                })));
                source.entry_scopes.resize(source.entries.len(), None);
            }
            source.body_members.insert(key.to_string());
        }
        self.eval_amended_object(template_map, &Arc::new(source), body, scope, depth)
    }

    /// Read one member of the amended receiver without forcing unrelated
    /// elements (which may themselves reference super.first or super.last).
    #[allow(clippy::too_many_arguments)]
    fn eval_listing_member(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        target: usize,
        position: &mut usize,
        value: &mut Option<Value>,
        inherited_locals: &[(String, Expr)],
    ) -> Result<()> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        let scope = scope.child();
        let mut locals = inherited_locals.to_vec();
        for entry in entries {
            match entry {
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {
                    if let Some(expr) = &prop.value {
                        locals.push((prop.name.clone(), expr.clone()));
                    }
                }
                Entry::Elem(expr) => {
                    if *position == target {
                        *value = Some(self.eval_expr(
                            &with_listing_locals(expr, &locals),
                            &scope,
                            depth + 1,
                        )?);
                    }
                    *position += 1;
                }
                Entry::DynProperty(index, expr) => {
                    let index = self.eval_expr(index, &scope, depth + 1)?;
                    let Value::Int(index) = index else {
                        return Err(Error::Eval(
                            "listing index amendment requires an Int index".into(),
                        ));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| Error::Eval("listing index cannot be negative".into()))?;
                    if index == target {
                        *value = Some(
                            if let (Some(base), Expr::ObjectBody(body)) = (value.as_ref(), expr) {
                                let mut amendment_scope = scope.child();
                                let base_name = "\0listing_endpoint_base".to_string();
                                amendment_scope.set(&base_name, base.clone());
                                let amendment = Expr::Binop(
                                    BinOp::Add,
                                    Box::new(Expr::Ident(base_name)),
                                    Box::new(Expr::ObjectBody(body.clone())),
                                );
                                self.eval_expr(
                                    &with_listing_locals(&amendment, &locals),
                                    &amendment_scope,
                                    depth + 1,
                                )?
                            } else {
                                self.eval_expr(
                                    &with_listing_locals(expr, &locals),
                                    &scope,
                                    depth + 1,
                                )?
                            },
                        );
                    }
                    *position = (*position).max(index + 1);
                }
                Entry::Spread(expr) => {
                    let values = listing_spread_values(self.eval_expr(expr, &scope, depth + 1)?)?;
                    if target >= *position && target - *position < values.len() {
                        *value = Some(values[target - *position].clone());
                    }
                    *position += values.len();
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, item) in collection_to_items(collection)? {
                        let mut iter_scope = scope.child();
                        let mut iter_locals = locals.clone();
                        // Bind generator variables after enclosing locals so
                        // shadowing does not change those locals' definitions.
                        let value_binding = format!("\0listing_generator_value:{depth}");
                        iter_scope.set(&value_binding, item.clone());
                        iter_scope.set(&generator.val_var, item);
                        iter_locals.push((generator.val_var.clone(), Expr::Ident(value_binding)));
                        if let Some(key_var) = &generator.key_var {
                            let key_binding = format!("\0listing_generator_key:{depth}");
                            iter_scope.set(&key_binding, key.clone());
                            iter_scope.set(key_var, key);
                            iter_locals.push((key_var.clone(), Expr::Ident(key_binding)));
                        }
                        self.eval_listing_member(
                            &generator.body,
                            &iter_scope,
                            depth + 1,
                            target,
                            position,
                            value,
                            &iter_locals,
                        )?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition = self.eval_expr(&generator.condition, &scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(selected) = selected {
                        self.eval_listing_member(
                            selected,
                            &scope,
                            depth + 1,
                            target,
                            position,
                            value,
                            &locals,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn eval_listing_entries(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        items: &mut Vec<Value>,
    ) -> Result<()> {
        let mut listing_scope = scope.child();
        // A body's locals and properties (`default`) are visible to all of
        // its entries, including ones written before them, and to each other
        // regardless of order. Bind each once the members of this body it
        // reads are bound, so a function captures them too.
        let mut pending: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some((prop, prop.value.as_ref()?)),
                _ => None,
            })
            .collect();
        let mut unbound: HashSet<String> =
            pending.iter().map(|(prop, _)| prop.name.clone()).collect();
        while !pending.is_empty() {
            let ready = pending
                .iter()
                .position(|(prop, expr)| {
                    let mut refs = HashSet::default();
                    collect_expr_refs(expr, &mut refs, &HashSet::default());
                    !refs
                        .iter()
                        .any(|name| *name != prop.name && unbound.contains(name))
                })
                // A cycle: bind in declaration order.
                .unwrap_or(0);
            let (prop, expr) = pending.remove(ready);
            let value = self.eval_expr(expr, &listing_scope, depth + 1)?;
            bind_listing_member(&mut listing_scope, prop, value);
            unbound.remove(&prop.name);
        }
        for entry in entries {
            match entry {
                Entry::Property(_) => {}
                Entry::Elem(expr) => items.push(self.eval_expr(expr, &listing_scope, depth + 1)?),
                Entry::DynProperty(index, value) => {
                    let index = self.eval_expr(index, &listing_scope, depth + 1)?;
                    let Value::Int(index) = index else {
                        return Err(Error::Eval(
                            "listing index amendment requires an Int index".into(),
                        ));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| Error::Eval("listing index cannot be negative".into()))?;
                    let value = if index < items.len()
                        && let Expr::ObjectBody(entries) = value
                    {
                        self.eval_value_amendment(
                            items[index].clone(),
                            entries,
                            &listing_scope,
                            depth + 1,
                        )?
                    } else {
                        self.eval_expr(value, &listing_scope, depth + 1)?
                    };
                    if index < items.len() {
                        items[index] = value;
                    } else if index == items.len() {
                        items.push(value);
                    } else {
                        return Err(Error::Eval(format!(
                            "listing index {index} is out of bounds for length {}",
                            items.len()
                        )));
                    }
                }
                Entry::Spread(expr) => {
                    let value = self.eval_expr(expr, &listing_scope, depth + 1)?;
                    items.extend(listing_spread_values(value)?.iter().cloned());
                }
                Entry::ForGenerator(generator) => {
                    let collection =
                        self.eval_expr(&generator.collection, &listing_scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection)? {
                        let mut iter_scope = listing_scope.child();
                        iter_scope.set(&generator.val_var, value);
                        if let Some(key_var) = &generator.key_var {
                            iter_scope.set(key_var, key);
                        }
                        self.eval_listing_entries(&generator.body, &iter_scope, depth + 1, items)?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition =
                        self.eval_expr(&generator.condition, &listing_scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(selected) = selected {
                        self.eval_listing_entries(selected, &listing_scope, depth + 1, items)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Evaluate `expr`. Literals and plain name lookups, which are most of the
    /// expressions evaluated, are answered here without allocating the boxed
    /// future that a recursive evaluation needs.
    fn eval_expr(&mut self, expr: &Expr, scope: &Scope, depth: usize) -> Result<Value> {
        if depth <= self.max_depth
            && let Some(result) = eval_simple_expr(expr, scope, depth, self.max_depth)
        {
            return result;
        }
        self.eval_expr_boxed(expr, scope, depth)
    }

    fn eval_expr_boxed(&mut self, expr: &Expr, scope: &Scope, depth: usize) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        self.check_cancelled()?;
        match expr {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::String(s) => Ok(Value::String(Arc::clone(s))),
            Expr::StringInterpolation(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        StringInterpPart::Literal(s) => result.push_str(s),
                        StringInterpPart::Expr(e) => {
                            let val = self.eval_expr(e, scope, depth + 1)?;
                            result.push_str(&value_to_display(&val));
                        }
                    }
                }
                Ok(Value::String(result.into()))
            }
            Expr::Ident(name) => scope.get(name).cloned().ok_or_else(|| {
                Error::Eval(
                    scope
                        .poison_of(name)
                        .cloned()
                        .unwrap_or_else(|| format!("undefined variable: {name}")),
                )
            }),
            Expr::Lambda(params, body) => {
                // The body is shared with the AST unless one of the rewrites
                // below applies to it.
                let mut body = Arc::clone(body);
                if needs_method_result_types(&body) {
                    capture_method_result_types(Arc::make_mut(&mut body), scope);
                }
                let mut names = HashSet::default();
                collect_unshadowed_names(&body, &mut names);
                // A body that names a type captures the whole scope (below),
                // so resolving its aliases leaves `names` as it is.
                if names.contains(NAMES_A_TYPE) && scope.has_type_aliases() {
                    capture_type_aliases(Arc::make_mut(&mut body), scope);
                }
                let mut refs = HashSet::default();
                let shadows = params.iter().cloned().collect::<HashSet<_>>();
                collect_expr_refs(&body, &mut refs, &shadows);
                // Capture only the bindings the body can reach: every name it
                // mentions, ignoring shadowing, plus the implicit receivers.
                // Flattening the whole scope for every lambda value, and
                // restoring all of it on every call, dominated evaluation. An
                // object built in the body sees its enclosing bindings through
                // `outer`, so a body that mentions `outer` keeps everything, as
                // does a body that names a type (see `NAMES_A_TYPE`).
                let captured =
                    Arc::new(if names.contains("outer") || names.contains(NAMES_A_TYPE) {
                        scope.flatten()
                    } else {
                        scope.flatten_names(
                            names
                                .iter()
                                .map(String::as_str)
                                .chain(["this", "module", "super"]),
                        )
                    });
                let captured_body = refs
                    .iter()
                    .filter(|name| scope.get(name).is_none())
                    .find_map(|name| scope.poison_of(name))
                    .map(|message| {
                        Arc::new(Expr::Throw(Box::new(Expr::String(message.clone().into()))))
                    })
                    .unwrap_or(body);
                Ok(Value::Lambda(Arc::clone(params), captured_body, captured))
            }
            Expr::InferredNew(ty, entries) => {
                let (name, params) = inferred_new_type(ty, scope, 0)?;
                self.eval_expr(
                    &Expr::New(Some(name), entries.clone(), params),
                    scope,
                    depth + 1,
                )
            }
            Expr::New(type_name, entries, generic_params) => {
                let mut constructor_scope = scope.child();
                constructor_scope.set("super", Value::Object(Arc::default(), None));
                constructor_scope.receiver_entries = Some(entries.clone());
                constructor_scope.receiver_list_base = None;
                let scope = &constructor_scope;
                match type_name.as_deref() {
                    Some("Listing") => {
                        let mut listing_scope = scope.child();
                        listing_scope.set(
                            "super",
                            Value::List(ListValue::new(ListKind::Listing, Vec::new())),
                        );
                        listing_scope.receiver_list_base = Some(0);
                        let mut items = Vec::new();
                        self.eval_listing_entries(entries, &listing_scope, depth + 1, &mut items)?;
                        Ok(Value::List(ListValue::new(ListKind::Listing, items)))
                    }
                    Some("Mapping") => {
                        check_member_kinds("Mapping", entries, false)?;
                        // If the Mapping has a value type param (e.g., Mapping<String, Step>),
                        // resolve it as a default template so entries inherit the class type.
                        let value_type_defaults = generic_params
                            .iter()
                            .skip(1)
                            .filter_map(|name| {
                                resolve_dotted(scope, name).map(|value| (name.clone(), value))
                            })
                            .collect::<Vec<_>>();
                        let mut map = ObjectMap::default();
                        self.eval_mapping_entries_with_type_default(
                            entries,
                            scope,
                            depth,
                            &mut map,
                            &value_type_defaults,
                            generic_params.get(1..).unwrap_or(&[]),
                            MappingInheritedDefault::default(),
                        )?;
                        // Build ObjectSource with a synthetic `default` entry so that
                        // body amendments (`steps { ["x"] { ... } }`) merge new entries
                        // with the value type class, preserving type_name for converters.
                        let mut src_entries = entries.to_vec();
                        if value_type_defaults.len() == 1
                            && !entries
                                .iter()
                                .any(|e| matches!(e, Entry::Property(p) if p.name == "default"))
                        {
                            // Inject a synthetic default property referencing the value type
                            let vt_name = generic_params[1].clone();
                            src_entries.push(Entry::Property(Arc::new(Property {
                                annotations: vec![],
                                modifiers: vec![],
                                name: "default".into(),
                                type_ann: None,
                                value: Some(Expr::New(Some(vt_name), vec![].into(), vec![])),
                                body: None,
                                is_method: false,
                            })));
                        }
                        let source_body_members = src_entries
                            .iter()
                            .filter_map(entry_member_name)
                            .cloned()
                            .collect();
                        let deprecated = collect_deprecated(&src_entries);
                        let source = ObjectSource {
                            entries: src_entries.into(),
                            captured: SourceScope::lazy(
                                scope,
                                vec![name_of("outer"), name_of("this")],
                                vec!["outer", "this"],
                            ),
                            body_members: source_body_members,
                            is_open: true,
                            is_abstract: false,
                            type_name: None,
                            type_identity: None,
                            parent_type_names: Vec::new(),
                            parent_type_identities: Vec::new(),
                            entry_scopes: Vec::new(),
                            evaluated_properties: map.keys().map(|k| k.to_string()).collect(),
                            mapping_value_types: generic_params.iter().skip(1).cloned().collect(),
                            deprecated,
                            poisoned_members: None,
                            kind: ObjectKind::Mapping,
                        };
                        Ok(Value::Object(Arc::new(map), Some(Arc::new(source))))
                    }
                    Some("Dynamic") => self.eval_entries(entries, scope, depth + 1),
                    _ => {
                        // Check if type name matches a class in scope (supports dotted names)
                        let base = type_name.as_ref().and_then(|name| {
                            let parts: Vec<&str> = name.split('.').collect();
                            let mut val = scope.get(parts[0])?.clone();
                            for part in &parts[1..] {
                                val = match val {
                                    Value::Object(ref map, _) => map.get(*part)?.clone(),
                                    _ => return None,
                                };
                            }
                            Some(val)
                        });
                        // A class whose definition failed (for example one
                        // reading a `module` property not evaluated yet) is
                        // poisoned; report why instead of building a bare object.
                        if base.is_none()
                            && let Some(message) = type_name
                                .as_deref()
                                .and_then(|name| poisoned_member(scope, name))
                        {
                            return Err(Error::Eval(message));
                        }
                        check_instantiable(scope, type_name.as_deref(), base.as_ref())?;
                        if let Some(Value::Object(ref base_map, Some(ref base_src))) = base {
                            check_no_elements(base_src, entries)?;
                            // Enforce open modifier: non-open classes reject new properties
                            if !base_src.is_open {
                                // Collect all declared property names from the base class
                                // (includes those with no default value)
                                let base_names: std::collections::HashSet<String> = base_src
                                    .entries
                                    .iter()
                                    .filter_map(|e| {
                                        if let Entry::Property(p) = e {
                                            Some(p.name.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .chain(base_map.keys().map(|k| k.to_string()))
                                    .collect();
                                for entry in entries.iter() {
                                    match entry {
                                        Entry::Property(p)
                                            if !has_modifier(&p.modifiers, Modifier::Local)
                                                && !base_names.contains(&p.name) =>
                                        {
                                            return Err(Error::Eval(format!(
                                                "cannot add property '{}' to non-open class",
                                                p.name
                                            )));
                                        }
                                        Entry::DynProperty(Expr::String(key), _)
                                            if !base_names.contains(&**key) =>
                                        {
                                            return Err(Error::Eval(format!(
                                                "cannot add property '{}' to non-open class",
                                                key
                                            )));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            let is_open = base_src.is_open;
                            // Late binding: re-evaluate merged base + overlay entries
                            let mut result = self
                                .eval_amended_object(base_map, base_src, entries, scope, depth)?;
                            // Preserve the base class's is_open flag and tag the
                            // type_name so output.renderer.converters can match it.
                            if let Value::Object(_, ref mut src_slot) = result {
                                // Keep the identity resolved from the class value, including
                                // when the constructor expression uses an imported class.
                                let tn = base_src.type_name.clone().or_else(|| type_name.clone());
                                let new_src = if let Some(src) = src_slot.take() {
                                    let mut s = Arc::unwrap_or_clone(src);
                                    if s.is_open != is_open {
                                        s.is_open = is_open;
                                    }
                                    s.type_name = tn;
                                    s.parent_type_names = base_src.parent_type_names.clone();
                                    s
                                } else {
                                    ObjectSource {
                                        entries: Vec::new().into(),
                                        captured: SourceScope::default(),
                                        body_members: HashSet::default(),
                                        is_open,
                                        is_abstract: false,
                                        type_name: tn,
                                        type_identity: base_src.type_identity.clone(),
                                        parent_type_names: base_src.parent_type_names.clone(),
                                        parent_type_identities: base_src
                                            .parent_type_identities
                                            .clone(),
                                        entry_scopes: Vec::new(),
                                        evaluated_properties: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&base_src.deprecated, entries),
                                        poisoned_members: None,
                                        kind: ObjectKind::Object,
                                    }
                                };
                                *src_slot = Some(Arc::new(new_src));
                            }
                            Ok(result)
                        } else if let Some(Value::Object(base_map, base_src)) = base {
                            // Fallback: eager merge
                            let overlay = self.eval_entries(entries, scope, depth + 1)?;
                            let mut merged: ObjectMap = (*base_map).clone();
                            let mut deprecated = base_src
                                .as_ref()
                                .map(|s| s.deprecated.clone())
                                .unwrap_or_default();
                            if let Value::Object(overlay_map, overlay_src) = &overlay {
                                merged.extend(
                                    overlay_map.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                                if let Some(os) = overlay_src.as_ref() {
                                    for (k, v) in &os.deprecated {
                                        deprecated.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                            let src = ObjectSource {
                                entries: Vec::new().into(),
                                captured: SourceScope::default(),
                                body_members: HashSet::default(),
                                is_open: true,
                                is_abstract: false,
                                type_name: type_name.clone(),
                                type_identity: None,
                                parent_type_names: Vec::new(),
                                parent_type_identities: Vec::new(),
                                entry_scopes: Vec::new(),
                                evaluated_properties: Vec::new(),
                                mapping_value_types: Vec::new(),
                                deprecated,
                                poisoned_members: None,
                                kind: ObjectKind::Object,
                            };
                            Ok(Value::Object(Arc::new(merged), Some(Arc::new(src))))
                        } else {
                            self.eval_entries(entries, scope, depth + 1)
                        }
                    }
                }
            }
            Expr::ObjectBody(entries) => {
                let mut body_scope = scope.child();
                body_scope.set("super", Value::Object(Arc::default(), None));
                self.eval_entries(entries, &body_scope, depth + 1)
            }
            Expr::Field(obj_expr, field) => {
                if matches!(obj_expr.as_ref(), Expr::Ident(name) if name == "super") {
                    let value = self.eval_super_member(field, scope, depth + 1, true)?;
                    if let Some(Value::Object(_, source)) = scope.get("super") {
                        self.warn_if_deprecated_access(source, field);
                    }
                    return Ok(value);
                }
                let obj = self.eval_field_base(obj_expr, field, scope, depth)?;
                // Built-in properties
                match (&obj, field.as_str()) {
                    (Value::List(items), "length") => return Ok(Value::Int(items.len() as i64)),
                    (Value::List(items), "isEmpty") => return Ok(Value::Bool(items.is_empty())),
                    (Value::List(items), "first") => {
                        return items
                            .first()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::List(items), "last") => {
                        return items
                            .last()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::String(s), "length") => {
                        return Ok(Value::Int(s.chars().count() as i64));
                    }
                    (Value::String(s), "isEmpty") => return Ok(Value::Bool(s.is_empty())),
                    (Value::Object(map, _), "length") => return Ok(Value::Int(map.len() as i64)),
                    (Value::Object(map, _), "isEmpty") => return Ok(Value::Bool(map.is_empty())),
                    (Value::Object(map, _), "keys") => {
                        return Ok(Value::List(ListValue::new(
                            ListKind::Set,
                            map.keys()
                                .map(|k| Value::String(k.clone()))
                                .collect::<Vec<_>>(),
                        )));
                    }
                    (Value::Object(map, _), "values") => {
                        return Ok(Value::List(
                            map.values().cloned().collect::<Vec<_>>().into(),
                        ));
                    }
                    // Duration and DataSize units on numbers
                    (
                        Value::Int(_) | Value::Float(_),
                        "ns" | "us" | "ms" | "s" | "min" | "h" | "d" | "b" | "kb" | "mb" | "gb"
                        | "tb" | "pb" | "kib" | "mib" | "gib" | "tib" | "pib",
                    ) => {
                        return Ok(make_unit_object(obj, field));
                    }
                    _ => {}
                }
                match &obj {
                    Value::Object(map, source) => {
                        let val = map.get(field.as_str()).cloned().ok_or_else(|| {
                            Error::Eval(
                                missing_member_error(source, obj_expr, field, scope)
                                    .unwrap_or_else(|| format!("field not found: {field}")),
                            )
                        })?;
                        self.warn_if_deprecated_access(source, field);
                        Ok(val)
                    }
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::NullSafeField(obj_expr, field) => {
                let obj = self.eval_field_base(obj_expr, field, scope, depth)?;
                match &obj {
                    Value::Null => Ok(Value::Null),
                    Value::Object(map, source) => {
                        if !map.contains_key(field.as_str())
                            && let Some(message) =
                                missing_member_error(source, obj_expr, field, scope)
                        {
                            return Err(Error::Eval(message));
                        }
                        let val = map.get(field.as_str()).cloned().unwrap_or(Value::Null);
                        if !is_null_value(&val) {
                            self.warn_if_deprecated_access(source, field);
                        }
                        Ok(val)
                    }
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::Index(obj_expr, key_expr) => {
                if matches!(obj_expr.as_ref(), Expr::Ident(name) if name == "super") {
                    let key = self.eval_expr(key_expr, scope, depth + 1)?;
                    if let Some(Value::List(items)) = scope.get("super") {
                        return match key {
                            Value::Int(index) => usize::try_from(index)
                                .ok()
                                .and_then(|index| items.get(index))
                                .cloned()
                                .ok_or_else(|| {
                                    Error::Eval(format!("index out of bounds: {index}"))
                                }),
                            _ => Err(Error::Eval("listing index must be an Int".into())),
                        };
                    }
                    return self.eval_super_member(&value_to_key(&key)?, scope, depth + 1, false);
                }
                let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
                let key = self.eval_expr(key_expr, scope, depth + 1)?;
                let key_str = value_to_key(&key)?;
                match obj {
                    Value::Object(map, source) => map.get(&key_str).cloned().ok_or_else(|| {
                        Error::Eval(
                            missing_member_error(&source, obj_expr, &key_str, scope)
                                .unwrap_or_else(|| format!("key not found: {key_str}")),
                        )
                    }),
                    _ => Err(Error::Eval("cannot index non-object".into())),
                }
            }
            Expr::Call(func_expr, args) => self.eval_call(func_expr, args, scope, depth),
            Expr::If(cond, then_expr, else_expr) => {
                let c = self.eval_expr(cond, scope, depth + 1)?;
                if is_truthy(&c) {
                    self.eval_expr(then_expr, scope, depth + 1)
                } else {
                    self.eval_expr(else_expr, scope, depth + 1)
                }
            }
            Expr::Let(name, val_expr, body_expr) => {
                let val = self.eval_expr(val_expr, scope, depth + 1)?;
                let mut child = scope.child();
                child.set(name, val);
                self.eval_expr(body_expr, &child, depth + 1)
            }
            Expr::Binop(op, left, right) => self.eval_binop(*op, left, right, scope, depth),
            Expr::Unop(op, operand) => {
                let v = self.eval_expr(operand, scope, depth + 1)?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => Err(Error::Eval("cannot negate non-number".into())),
                    },
                    UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
                    UnOp::NonNull => {
                        if is_null_value(&v) {
                            Err(Error::Eval(
                                "non-null assertion failed: value is null".into(),
                            ))
                        } else {
                            Ok(v)
                        }
                    }
                }
            }
            Expr::Is(expr, ty) => {
                let val = self.eval_expr(expr, scope, depth + 1)?;
                let matches = self.eval_type_check(&val, ty, scope, depth)?;
                Ok(Value::Bool(matches))
            }
            Expr::As(expr, ty) => {
                let val = self.eval_expr(expr, scope, depth + 1)?;
                let matches = self.eval_type_check(&val, ty, scope, depth)?;
                if matches {
                    Ok(val)
                } else {
                    Err(Error::Eval(format!(
                        "cannot cast {} to {}",
                        value_type_name(&val),
                        display_type_expr(ty)
                    )))
                }
            }
            Expr::Throw(msg_expr) => {
                let msg = self.eval_expr(msg_expr, scope, depth + 1)?;
                Err(Error::Eval(format!("throw: {}", value_to_display(&msg))))
            }
            Expr::Trace(expr) => {
                let v = self.eval_expr(expr, scope, depth + 1)?;
                eprintln!("[pklr trace] {}", value_to_display(&v));
                Ok(v)
            }
            Expr::Read(uri_expr) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1)?;
                let uri_str = value_to_display(&uri);
                self.read_resource(&uri_str)
            }
            Expr::Import(uri, module_path) => {
                self.eval_import_expr(uri, Path::new(module_path), depth, None)
            }
            Expr::ImportGlob(pattern, module_path) => {
                let module_path = Path::new(module_path);
                let resolved = resolve_remote_relative(module_path, pattern);
                let pattern: &str = resolved.as_deref().unwrap_or(pattern);
                self.eval_glob_import(pattern, module_path, depth, None)
            }
            Expr::ReadOrNull(uri_expr) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1)?;
                let uri_str = value_to_display(&uri);
                match self.read_resource(&uri_str) {
                    Ok(v) => Ok(v),
                    Err(_) => Ok(Value::Null),
                }
            }
        }
    }

    /// Evaluate the `(start, end)` arguments of an `IntSeq(start, end)` call.
    fn eval_int_seq_bounds(
        &mut self,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<(i64, i64)> {
        let [start_expr, end_expr] = args else {
            return Err(Error::Eval(format!(
                "IntSeq() expects 2 arguments (start, end), got {}",
                args.len()
            )));
        };
        let mut bounds = [0i64; 2];
        for (slot, expr) in bounds.iter_mut().zip([start_expr, end_expr]) {
            match self.eval_expr(expr, scope, depth + 1)? {
                Value::Int(n) => *slot = n,
                other => {
                    return Err(Error::Eval(format!(
                        "IntSeq() expects Int arguments, got {}",
                        value_type_name(&other)
                    )));
                }
            }
        }
        Ok((bounds[0], bounds[1]))
    }

    fn eval_call(
        &mut self,
        func_expr: &Expr,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // `IntSeq(start, end).step(n)`: IntSeq evaluates to a plain list, so
        // the step is applied while the range bounds are still known.
        if let Expr::Field(obj_expr, method) = func_expr
            && method == "step"
            && let Expr::Call(seq_func, seq_args) = obj_expr.as_ref()
            && matches!(seq_func.as_ref(), Expr::Ident(name) if name == "IntSeq")
            && int_seq_is_builtin(scope)
        {
            let (start, end) = self.eval_int_seq_bounds(seq_args, scope, depth)?;
            let [step_expr] = args else {
                return Err(Error::Eval(
                    "IntSeq.step() expects exactly one argument".into(),
                ));
            };
            let step = match self.eval_expr(step_expr, scope, depth + 1)? {
                Value::Int(n) => n,
                other => {
                    return Err(Error::Eval(format!(
                        "IntSeq.step() expects an Int, got {}",
                        value_type_name(&other)
                    )));
                }
            };
            return int_seq(start, end, step);
        }
        // Handle method calls: obj.method(args)
        if let Expr::Field(obj_expr, method) = func_expr {
            let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
            }
            if let Some(result) = self.eval_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_field_call(&obj, method, &evaled_args)? {
                return Ok(result);
            }
            return Err(Error::Eval(format!(
                "unknown method '{method}' on {}",
                value_type_name(&obj)
            )));
        }
        // Handle null-safe method calls: obj?.method(args)
        if let Expr::NullSafeField(obj_expr, method) = func_expr {
            let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
            if is_null_value(&obj) {
                return Ok(Value::Null);
            }
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
            }
            if let Some(result) = self.eval_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_field_call(&obj, method, &evaled_args)? {
                return Ok(result);
            }
            return Err(Error::Eval(format!(
                "unknown method '{method}' on {}",
                value_type_name(&obj)
            )));
        }

        // Handle built-in functions: List(), Listing(), Map()
        if let Expr::Ident(name) = func_expr {
            match name.as_str() {
                "List" | "Listing" => {
                    let mut items = Vec::new();
                    for a in args {
                        items.push(self.eval_expr(a, scope, depth + 1)?);
                    }
                    return Ok(Value::List(items.into()));
                }
                "Set" => {
                    let mut items = Vec::new();
                    for a in args {
                        let val = self.eval_expr(a, scope, depth + 1)?;
                        if !set_contains(&items, &val) {
                            items.push(val);
                        }
                    }
                    return Ok(Value::List(ListValue::new(ListKind::Set, items)));
                }
                "IntSeq" if int_seq_is_builtin(scope) => {
                    let (start, end) = self.eval_int_seq_bounds(args, scope, depth)?;
                    return int_seq(start, end, 1);
                }
                "Regex" => {
                    if let Some(arg) = args.first() {
                        let val = self.eval_expr(arg, scope, depth + 1)?;
                        return Ok(regex_value(val));
                    }
                    return Err(Error::Eval("Regex() requires a pattern argument".into()));
                }
                "Map" => {
                    // Map(k1, v1, k2, v2, ...)
                    let mut map = ObjectMap::default();
                    let mut evaled = Vec::new();
                    for a in args {
                        evaled.push(self.eval_expr(a, scope, depth + 1)?);
                    }
                    for pair in evaled.chunks(2) {
                        if let [k, v] = pair {
                            map.insert(value_to_key(k)?, v.clone());
                        }
                    }
                    return Ok(Value::Object(Arc::new(map), None));
                }
                _ => {}
            }
        }

        // Evaluate the function expression
        let func_val = self.eval_expr(func_expr, scope, depth + 1)?;

        // Lambda call
        if let Value::Lambda(params, body, captured) = func_val {
            let mut call_scope = Scope::for_call(&captured);
            // If we're inside a method call context (scope has `this` as an Object),
            // layer the instance's properties so local functions see overridden values
            if let Some(Value::Object(this_map, _)) = scope.get("this") {
                for (k, v) in this_map.iter() {
                    call_scope.set(k, v.clone());
                }
            }
            // Bind arguments to parameters
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
            }
            for (param, arg) in params.iter().zip(evaled_args) {
                call_scope.declare(param, arg);
            }
            return self.eval_expr(&body, &call_scope, depth + 1);
        }

        // Built-in type constructors resolved from scope (e.g. base.Regex)
        if let Value::String(ref name) = func_val
            && &**name == "Regex"
            && let Some(arg) = args.first()
        {
            let val = self.eval_expr(arg, scope, depth + 1)?;
            return Ok(regex_value(val));
        }

        // Plain call with no args on an object — return the object
        if args.is_empty() {
            return Ok(func_val);
        }
        Err(Error::Eval("cannot call non-function".into()))
    }

    fn eval_object_field_call(
        &mut self,
        obj: &Value,
        method: &str,
        evaled_args: &[Value],
    ) -> Result<Option<Value>> {
        if let Value::Object(map, source) = obj
            && let Some(func_val) = map.get(method).cloned()
        {
            self.warn_if_deprecated_access(source, method);
            if let Value::String(ref name) = func_val
                && &**name == "Regex"
                && let Some(arg) = evaled_args.first()
            {
                return Ok(Some(regex_value(arg.clone())));
            }
            if evaled_args.is_empty() {
                return Ok(Some(func_val));
            }
            return Err(Error::Eval("cannot call non-function".into()));
        }
        Ok(None)
    }

    fn eval_object_method_call(
        &mut self,
        obj: &Value,
        method: &str,
        evaled_args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Value::Object(map, _) = obj
            && let Some(Value::Lambda(params, body, captured)) = map.get(method)
        {
            let mut call_scope = Scope::for_call(captured);
            // Layer in all instance properties, including lambdas, so local
            // functions called by this method see overrides.
            for (k, v) in map.iter() {
                call_scope.set(k, v.clone());
            }
            call_scope.set("this", obj.clone());
            for (i, param) in params.iter().enumerate() {
                if let Some(arg) = evaled_args.get(i) {
                    call_scope.declare(param, arg.clone());
                }
            }
            return Ok(Some(self.eval_expr(body, &call_scope, depth + 1)?));
        }
        Ok(None)
    }

    fn eval_method_call(
        &mut self,
        obj: &Value,
        method: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        match (obj, method) {
            // String methods
            (Value::String(s), "contains") => {
                let arg = require_str_arg(args, 0, "contains")?;
                Ok(Some(Value::Bool(s.contains(arg))))
            }
            (Value::String(s), "startsWith") => {
                let arg = require_str_arg(args, 0, "startsWith")?;
                Ok(Some(Value::Bool(s.starts_with(arg))))
            }
            (Value::String(s), "endsWith") => {
                let arg = require_str_arg(args, 0, "endsWith")?;
                Ok(Some(Value::Bool(s.ends_with(arg))))
            }
            (Value::String(s), "replaceLast") => {
                let from = require_str_arg(args, 0, "replaceLast")?;
                let to = require_str_arg(args, 1, "replaceLast")?;
                let mut result = s.to_string();
                if let Some(start) = s.rfind(from) {
                    result.replace_range(start..start + from.len(), to);
                }
                Ok(Some(Value::String(result.into())))
            }
            (Value::String(s), "replaceAll") => {
                let from = require_str_arg(args, 0, "replaceAll")?;
                let to = require_str_arg(args, 1, "replaceAll")?;
                Ok(Some(Value::String(s.replace(from, to).into())))
            }
            (Value::String(s), "split") => {
                let sep = require_str_arg(args, 0, "split")?;
                Ok(Some(Value::List(
                    s.split(sep)
                        .map(|p| Value::String(p.into()))
                        .collect::<Vec<_>>()
                        .into(),
                )))
            }
            (Value::String(s), "trim") => Ok(Some(Value::String(s.trim().into()))),
            (Value::String(s), "trimStart") => Ok(Some(Value::String(s.trim_start().into()))),
            (Value::String(s), "trimEnd") => Ok(Some(Value::String(s.trim_end().into()))),
            (Value::String(s), "toUpperCase") => Ok(Some(Value::String(s.to_uppercase().into()))),
            (Value::String(s), "toLowerCase") => Ok(Some(Value::String(s.to_lowercase().into()))),
            (Value::String(s), "toInt") => s
                .parse::<i64>()
                .map(|n| Some(Value::Int(n)))
                .map_err(|_| Error::Eval(format!("cannot convert '{s}' to Int"))),
            (Value::String(s), "toBoolean") => match s.to_ascii_lowercase().as_str() {
                "true" => Ok(Some(Value::Bool(true))),
                "false" => Ok(Some(Value::Bool(false))),
                _ => Err(Error::Eval(format!("cannot convert '{s}' to Boolean"))),
            },

            // List methods
            (Value::List(items), "contains") => {
                let arg = args.first().cloned().unwrap_or(Value::Null);
                Ok(Some(Value::Bool(set_contains(items, &arg))))
            }
            (Value::List(items), "toList") => {
                Ok(Some(Value::List(items.clone().with_kind(ListKind::List))))
            }
            (Value::List(items), "toSet") => {
                if items.kind() == ListKind::Set {
                    return Ok(Some(obj.clone()));
                }
                let mut seen = Vec::new();
                for item in items.iter() {
                    if !set_contains(&seen, item) {
                        seen.push(item.clone());
                    }
                }
                Ok(Some(Value::List(ListValue::new(ListKind::Set, seen))))
            }
            (Value::List(items), "map") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("map requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items.iter() {
                    result.push(self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?);
                }
                Ok(Some(collection_result(items.kind(), result)))
            }
            (Value::List(items), "flatMap") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("flatMap requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items.iter() {
                    let val = self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?;
                    if let Value::List(inner) = val {
                        result.extend(inner.iter().cloned());
                    } else {
                        result.push(val);
                    }
                }
                Ok(Some(collection_result(items.kind(), result)))
            }
            (Value::List(items), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items.iter() {
                    let cond = self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?;
                    if is_truthy(&cond) {
                        result.push(item.clone());
                    }
                }
                Ok(Some(collection_result(items.kind(), result)))
            }
            (Value::List(items), "filterNonNull") => Ok(Some(collection_result(
                items.kind(),
                items
                    .iter()
                    .filter(|item| !is_null_value(item))
                    .cloned()
                    .collect(),
            ))),
            (Value::List(items), "fold") => {
                let init = args
                    .first()
                    .ok_or_else(|| Error::Eval("fold requires initial value".into()))?
                    .clone();
                let lambda = args
                    .get(1)
                    .ok_or_else(|| Error::Eval("fold requires a function argument".into()))?;
                let mut acc = init;
                for item in items.iter() {
                    acc = self.invoke_lambda(lambda, &[acc, item.clone()], depth)?;
                }
                Ok(Some(acc))
            }
            (Value::List(items), "any") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("any requires a function argument".into()))?;
                for item in items.iter() {
                    if is_truthy(&self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?) {
                        return Ok(Some(Value::Bool(true)));
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            (Value::List(items), "every") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("every requires a function argument".into()))?;
                for item in items.iter() {
                    if !is_truthy(&self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?) {
                        return Ok(Some(Value::Bool(false)));
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            (Value::List(items), "join") => {
                let sep = args.first().and_then(|v| v.as_str()).unwrap_or(",");
                let s: Vec<String> = items.iter().map(value_to_display).collect();
                Ok(Some(Value::String(s.join(sep).into())))
            }
            (Value::List(items), "reverse") => {
                let mut rev = items.clone();
                rev.make_mut().reverse();
                Ok(Some(Value::List(rev)))
            }

            // Object/Mapping methods
            (Value::Object(map, _), "containsKey") => {
                let key = args.first().and_then(|v| v.as_str()).unwrap_or("");
                Ok(Some(Value::Bool(map.contains_key(key))))
            }
            (Value::Object(map, _), "toMap" | "toMapping") => {
                Ok(Some(Value::Object(map.clone(), None)))
            }
            (Value::Object(map, _), "mapValues") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("mapValues requires a function".into()))?;
                let mut result = ObjectMap::default();
                for (k, v) in map.iter() {
                    let new_v =
                        self.invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)?;
                    result.insert(k.clone(), new_v);
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
            }
            (Value::Object(map, _), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function".into()))?;
                let mut result = ObjectMap::default();
                for (k, v) in map.iter() {
                    let keep =
                        self.invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)?;
                    if is_truthy(&keep) {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
            }
            (Value::Object(..), "toList") => Ok(Some(obj.clone())),
            (Value::Object(map, _), "toDynamic") => Ok(Some(Value::Object(map.clone(), None))),

            // Int/Float methods
            (Value::Int(n), "toString") => Ok(Some(Value::String(n.to_string().into()))),
            (Value::Float(f), "toString") => Ok(Some(Value::String(f.to_string().into()))),
            (Value::Bool(b), "toString") => Ok(Some(Value::String(b.to_string().into()))),

            // Lambda.apply()
            (Value::Lambda(params, body, captured), "apply") => {
                let mut call_scope = Scope::for_call(captured);
                for (param, arg) in params.iter().zip(args.iter()) {
                    call_scope.declare(param, arg.clone());
                }
                Ok(Some(self.eval_expr(body, &call_scope, depth + 1)?))
            }

            _ => Ok(None), // not a known method
        }
    }

    fn invoke_lambda(&mut self, lambda: &Value, args: &[Value], depth: usize) -> Result<Value> {
        if let Value::Lambda(params, body, captured) = lambda {
            let mut scope = Scope::for_call(captured);
            for (param, arg) in params.iter().zip(args.iter()) {
                scope.declare(param, arg.clone());
            }
            self.eval_expr(body, &scope, depth + 1)
        } else {
            Err(Error::Eval("expected a function".into()))
        }
    }

    fn eval_value_amendment(
        &mut self,
        base: Value,
        overlay_entries: &Body,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Amending an abstract module's object instantiates it.
        if let Value::Object(_, Some(source)) = &base
            && source.is_abstract
            && source.type_name.is_none()
        {
            return Err(Error::Eval(format!(
                "Cannot instantiate abstract class `{}`.",
                source.type_identity.as_deref().unwrap_or_default()
            )));
        }
        if let Value::Null
        | Value::Int(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::String(_)
        | Value::Lambda(..) = base
        {
            return Err(Error::Eval(format!(
                "Cannot instantiate, or amend an instance of, external class `{}`.",
                value_type_name(&base)
            )));
        }
        if let Value::Object(base_map, Some(base_src)) = &base
            && base_src.kind == ObjectKind::Mapping
        {
            check_no_elements(base_src, overlay_entries)?;
            let needs_mapping_evaluation = body_has(overlay_entries, &|entry| {
                matches!(entry, Entry::Spread(_))
                    || matches!(entry, Entry::Property(prop) if prop.name == "default")
            });
            if !base_src.is_metadata_only() || !needs_mapping_evaluation {
                // Unannotated mapping entries with bodies still use the general
                // amendment path, which preserves Listing-shaped entry bodies.
            } else {
                let (_, mut amendment_scope) =
                    mapping_amendment_scopes(base_src.scope(), base_src.scope_declared(), scope);
                amendment_scope.set("super", base.clone());
                let mut receiver_entries = base_map
                    .keys()
                    .map(|key| Entry::DynProperty(Expr::String(Arc::clone(key)), Expr::Null))
                    .collect::<Vec<_>>();
                receiver_entries.extend_from_slice(overlay_entries);
                amendment_scope.receiver_entries = Some(Arc::new(receiver_entries));
                let mut amended = ObjectMap::default();
                amended.extend(
                    base_map
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                self.eval_mapping_entries_with_type_default(
                    overlay_entries,
                    &amendment_scope,
                    depth,
                    &mut amended,
                    &[],
                    &[],
                    MappingInheritedDefault::default(),
                )?;
                let mut source = Arc::unwrap_or_clone(Arc::clone(base_src));
                source.entries = overlay_entries.to_vec().into();
                source.captured = SourceScope::lazy(scope, Vec::new(), Vec::new());
                return Ok(Value::Object(Arc::new(amended), Some(Arc::new(source))));
            }
        }
        if let Value::List(existing) = base {
            let mut amended = existing;
            let mut amendment_scope = scope.child();
            amendment_scope.set("super", Value::List(amended.clone()));
            amendment_scope.receiver_entries = Some(overlay_entries.clone());
            amendment_scope.receiver_list_base = Some(amended.len());
            self.eval_listing_entries(
                overlay_entries,
                &amendment_scope,
                depth + 1,
                amended.make_mut(),
            )?;
            return Ok(Value::List(amended));
        }
        if let Value::Object(base_map, Some(base_src)) = &base
            && !base_src.is_metadata_only()
        {
            check_no_elements(base_src, overlay_entries)?;
            let has_mapping_default = base_src
                .entries
                .iter()
                .any(|entry| matches!(entry, Entry::Property(prop) if prop.name == "default"));
            if !base_src.mapping_value_types.is_empty()
                || (base_src.kind == ObjectKind::Mapping && has_mapping_default)
            {
                let (inherited_scope, mut amendment_scope) =
                    mapping_amendment_scopes(base_src.scope(), base_src.scope_declared(), scope);
                amendment_scope.set("super", base.clone());
                let mut receiver_entries = base_map
                    .keys()
                    .map(|key| Entry::DynProperty(Expr::String(Arc::clone(key)), Expr::Null))
                    .collect::<Vec<_>>();
                receiver_entries.extend_from_slice(overlay_entries);
                amendment_scope.receiver_entries = Some(Arc::new(receiver_entries));
                let value_type_defaults = base_src
                    .mapping_value_types
                    .iter()
                    .filter_map(|name| {
                        resolve_dotted(&inherited_scope, name).map(|value| (name.clone(), value))
                    })
                    .collect::<Vec<_>>();
                let inherited_default =
                    self.find_default_template(&base_src.entries, &inherited_scope, depth)?;
                let mut amended = ObjectMap::default();
                self.eval_mapping_entries_with_type_default(
                    &base_src.entries,
                    &inherited_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    &base_src.mapping_value_types,
                    MappingInheritedDefault::default(),
                )?;
                if let Value::Object(existing_map, _) = &base {
                    amended.extend(existing_map.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                self.eval_mapping_entries_with_type_default(
                    overlay_entries,
                    &amendment_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    &base_src.mapping_value_types,
                    MappingInheritedDefault {
                        value: inherited_default,
                        entries: find_default_body_entries(&base_src.entries),
                    },
                )?;
                return Ok(Value::Object(Arc::new(amended), Some(Arc::clone(base_src))));
            }
            return self.eval_amended_object(base_map, base_src, overlay_entries, scope, depth);
        }
        let mut amendment_scope = scope.child();
        if let Value::Object(existing, _) = &base {
            for (name, value) in existing.iter() {
                amendment_scope.set(name, value.clone());
            }
        }
        let overlay = self.eval_entries(overlay_entries, &amendment_scope, depth + 1)?;
        Ok(merge_values(base, overlay))
    }

    fn eval_binop(
        &mut self,
        op: BinOp,
        left: &Expr,
        right: &Expr,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Special case: object amendment `base + ObjectBody(entries)`
        if let BinOp::Add = op
            && let Expr::ObjectBody(overlay_entries) = right
        {
            let base = self.eval_expr(left, scope, depth + 1)?;
            return self.eval_value_amendment(base, overlay_entries, scope, depth);
        }

        // Logical `&&` and `||` short-circuit: the right operand must not be
        // evaluated when the left already determines the result (e.g.
        // `x is Foo && x.fooField` must not touch `fooField` when `x` is not a
        // `Foo`).
        if matches!(op, BinOp::And | BinOp::Or) {
            let left_truthy = is_truthy(&self.eval_expr(left, scope, depth + 1)?);
            let short_circuit = match op {
                BinOp::And => !left_truthy,
                _ => left_truthy,
            };
            if short_circuit {
                return Ok(Value::Bool(left_truthy));
            }
            let right_truthy = is_truthy(&self.eval_expr(right, scope, depth + 1)?);
            return Ok(Value::Bool(right_truthy));
        }

        let l = self.eval_expr(left, scope, depth + 1)?;
        let r = self.eval_expr(right, scope, depth + 1)?;
        match op {
            BinOp::Pipe => {
                // x |> f  is equivalent to  f(x)
                match r {
                    Value::Lambda(params, body, captured) => {
                        if params.len() != 1 {
                            return Err(Error::Eval(format!(
                                "pipe operator requires a single-parameter function, got {}",
                                params.len()
                            )));
                        }
                        let mut call_scope = Scope::for_call(&captured);
                        call_scope.declare(params[0].clone(), l);
                        self.eval_expr(&body, &call_scope, depth + 1)
                    }
                    _ => Err(Error::Eval(
                        "pipe operator requires a function on the right side".into(),
                    )),
                }
            }
            _ => apply_binop(op, l, r),
        }
    }

    /// Find and evaluate a `default { ... }` property in an entry list.
    fn find_default_template(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        for entry in entries {
            if let Entry::Property(prop) = entry
                && prop.name == "default"
                && !has_modifier(&prop.modifiers, Modifier::Local)
            {
                return self.eval_property(prop, scope, depth);
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_mapping_entries_with_type_default(
        &mut self,
        entries: &[crate::parser::Entry],
        scope: &Scope,
        depth: usize,
        map: &mut ObjectMap,
        type_defaults: &[(String, Value)],
        value_type_names: &[String],
        inherited_default: MappingInheritedDefault,
    ) -> Result<()> {
        let mut entry_scope = scope.child();
        let mut deferred_lambdas: Vec<(String, &crate::parser::Expr)> = Vec::new();
        for entry in entries {
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    let expr = prop.value.as_ref().unwrap();
                    // Bind every local in declaration order so later locals and
                    // entries can reference it (e.g. a non-lambda local that
                    // calls a lambda local defined just above it).
                    let val = self.eval_expr(expr, &entry_scope, depth)?;
                    entry_scope.declare(&prop.name, val);
                    if matches!(expr, crate::parser::Expr::Lambda(..)) {
                        // Lambda evaluation only captures the current scope; it
                        // does not run the body. Bind once for declaration-order
                        // visibility, then re-bind after properties for late
                        // binding of overrides.
                        deferred_lambdas.push((prop.name.clone(), expr));
                    }
                }
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.body.is_some() =>
                {
                    let val =
                        self.eval_entries(prop.body.as_ref().unwrap(), &entry_scope, depth)?;
                    entry_scope.declare(&prop.name, val);
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self.eval_class_def(
                        name,
                        class_mods,
                        parent.as_deref(),
                        body,
                        &entry_scope,
                        depth,
                    )?;
                    entry_scope.set(name, defaults);
                }
                Entry::TypeAlias(name, ty) => {
                    self.eval_type_alias(name, ty, &mut entry_scope);
                }
                _ => {}
            }
        }
        for (name, expr) in deferred_lambdas {
            let val = self.eval_expr(expr, &entry_scope, depth)?;
            entry_scope.set(name, val);
        }

        let explicit_default = self
            .find_default_template(entries, &entry_scope, depth)?
            .or(inherited_default.value);
        let explicit_default_entries =
            find_default_body_entries(entries).or(inherited_default.entries);

        for entry in entries {
            match entry {
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, &entry_scope, depth + 1)?;
                    let key_str = value_to_key(&key)?;
                    if let Some(Value::Object(existing_map, Some(existing_src))) = map.get(&key_str)
                        && let Expr::ObjectBody(body) = val_expr
                    {
                        let val = self.eval_amended_object(
                            existing_map,
                            existing_src,
                            body,
                            &entry_scope,
                            depth,
                        )?;
                        map.insert(key_str, val);
                        continue;
                    }
                    let type_default = match val_expr {
                        Expr::ObjectBody(body) => select_mapping_type_default(type_defaults, body)
                            .map(|(name, value)| (Some(name.as_str()), value)),
                        Expr::New(Some(type_name), _, _) => {
                            select_mapping_type_default_for_new(type_defaults, type_name)
                                .map(|(name, value)| (Some(name.as_str()), value))
                        }
                        Expr::New(None, _, _) => None,
                        _ => type_defaults
                            .first()
                            .map(|(name, value)| (Some(name.as_str()), value)),
                    };
                    // `new T { ... }` constructs a fresh T. It never amends the
                    // mapping's `default`, so only T's own defaults apply. In
                    // particular a `default` of another class (the synthetic
                    // `new Step {}` of `new Mapping<String, Step> {}`) must not
                    // turn a `new Group {}` entry into a Step.
                    let is_typed_new = matches!(val_expr, Expr::New(Some(_), _, _));
                    let default_template = match (type_default, explicit_default.as_ref()) {
                        (Some((type_name, type_default)), _) if is_typed_new => {
                            Some((type_name, type_default.clone()))
                        }
                        (None, _) if is_typed_new => None,
                        (Some((type_name, type_default)), Some(explicit_default)) => Some((
                            type_name,
                            merge_values(type_default.clone(), explicit_default.clone()),
                        )),
                        (Some((type_name, type_default)), None) => {
                            Some((type_name, type_default.clone()))
                        }
                        (None, Some(explicit_default)) => Some((None, explicit_default.clone())),
                        (None, None) => None,
                    };
                    // If the default template has ObjectSource, amend its source entries
                    // so late-bound class properties are recomputed after overrides.
                    let val =
                        if let Some((default_type_name, Value::Object(template_map, Some(src)))) =
                            default_template.as_ref()
                            && let Some(body) = mapping_entry_body(val_expr)
                        {
                            if let Expr::New(Some(type_name), _, _) = val_expr {
                                validate_new_object_body(type_name, body, src)?;
                            }
                            let mut result =
                                if let Some(default_entries) = explicit_default_entries.as_ref() {
                                    let mut overlay_entries = default_entries.to_vec();
                                    overlay_entries.extend(body.iter().cloned());
                                    self.eval_amended_object(
                                        template_map,
                                        src,
                                        &overlay_entries,
                                        &entry_scope,
                                        depth,
                                    )?
                                } else if !is_typed_new
                                    && let Some(Value::Object(explicit_map, Some(explicit_src))) =
                                        explicit_default.as_ref()
                                    && type_default.is_some()
                                    && default_type_name.is_some_and(|expected| {
                                        let chain = explicit_src
                                            .type_name
                                            .iter()
                                            .chain(explicit_src.parent_type_names.iter())
                                            .cloned()
                                            .collect::<Vec<_>>();
                                        let expected = expand_type_alias_names(
                                            std::slice::from_ref(&expected.to_string()),
                                            &entry_scope,
                                        );
                                        expand_type_alias_names(&chain, &entry_scope).iter().any(
                                            |actual| {
                                                expected.iter().any(|expected| {
                                                    type_names_match(actual, expected)
                                                })
                                            },
                                        )
                                    })
                                {
                                    // The `default` is itself an instance of the selected
                                    // value type (for example the synthetic `new Step {}`
                                    // of a typed mapping literal). Amend its entries so the
                                    // body's assignments late-bind sibling properties.
                                    self.eval_amended_object(
                                        explicit_map,
                                        explicit_src,
                                        body,
                                        &entry_scope,
                                        depth,
                                    )?
                                } else if let Some(Value::Object(explicit_map, _)) =
                                    explicit_default.as_ref()
                                    && type_default.is_some()
                                {
                                    self.eval_object_body_over_template(
                                        template_map,
                                        src,
                                        explicit_map,
                                        body,
                                        &entry_scope,
                                        depth,
                                    )?
                                } else {
                                    self.eval_amended_object(
                                        template_map,
                                        src,
                                        body,
                                        &entry_scope,
                                        depth,
                                    )?
                                };
                            let type_name = src.type_name.as_deref().or(*default_type_name);
                            if let Some(tn) = type_name
                                && let Value::Object(_, ref mut result_src) = result
                            {
                                let new_src = match result_src.take() {
                                    Some(s) => {
                                        let mut ns = Arc::unwrap_or_clone(s);
                                        ns.type_name = Some(tn.to_string());
                                        ns
                                    }
                                    None => ObjectSource {
                                        entries: vec![].into(),
                                        captured: SourceScope::default(),
                                        body_members: HashSet::default(),
                                        is_open: true,
                                        is_abstract: false,
                                        type_name: Some(tn.to_string()),
                                        type_identity: src.type_identity.clone(),
                                        parent_type_names: src.parent_type_names.clone(),
                                        parent_type_identities: src.parent_type_identities.clone(),
                                        entry_scopes: Vec::new(),
                                        evaluated_properties: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&src.deprecated, body),
                                        poisoned_members: None,
                                        kind: ObjectKind::Object,
                                    },
                                };
                                *result_src = Some(std::sync::Arc::new(new_src));
                            }
                            result
                        } else {
                            let val = self.eval_expr(val_expr, &entry_scope, depth + 1)?;
                            apply_mapping_entry_template(
                                default_template.map(|(_, template)| template),
                                val,
                                value_type_names,
                                &entry_scope,
                            )?
                        };
                    map.insert(key_str, val);
                }
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Property(prop)
                    if prop.name == "default"
                        && (explicit_default.is_some() || !type_defaults.is_empty()) => {}
                Entry::Spread(e) => {
                    let val = self.eval_expr(e, &entry_scope, depth + 1)?;
                    check_iterable(&val)?;
                    if let Value::List(_) = val {
                        return Err(Error::Eval(
                            "Cannot spread value of type `List` into object of type `Mapping`."
                                .into(),
                        ));
                    }
                    if let Value::Object(m, _) = val {
                        map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self.eval_expr(&fgen.collection, &entry_scope, depth + 1)?;
                    for (k, v) in collection_to_items(collection)? {
                        let mut iter_scope = entry_scope.child();
                        iter_scope.set(&fgen.val_var, v);
                        if let Some(kv) = &fgen.key_var {
                            iter_scope.set(kv, k);
                        }
                        self.eval_mapping_entries_with_type_default(
                            &fgen.body,
                            &iter_scope,
                            depth + 1,
                            map,
                            type_defaults,
                            value_type_names,
                            MappingInheritedDefault {
                                value: explicit_default.clone(),
                                entries: explicit_default_entries.clone(),
                            },
                        )?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition =
                        self.eval_expr(&generator.condition, &entry_scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(selected) = selected {
                        self.eval_mapping_entries_with_type_default(
                            selected,
                            &entry_scope,
                            depth + 1,
                            map,
                            type_defaults,
                            value_type_names,
                            MappingInheritedDefault {
                                value: explicit_default.clone(),
                                entries: explicit_default_entries.clone(),
                            },
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Walk the AST of the `output` property to extract converter lambdas.
    /// Looks for the structure: `output { renderer { converters { [Type] = (x) -> ... } } }`.
    /// Converter keys are type identifiers (e.g., `[Regex]`), which we preserve as
    /// strings for matching against `ObjectSource.type_name`.
    fn extract_converters_from_ast(&mut self, output_prop: &Property, scope: &Scope, depth: usize) {
        let Some(output_body) = &output_prop.body else {
            return;
        };
        let mut converter_scope = scope.child();
        for entry in output_body.iter() {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
                && let Ok(value) = self.eval_expr(expr, &converter_scope, depth)
            {
                converter_scope.set(&prop.name, value);
            }
        }
        // Find `renderer { converters { ... } }` inside output
        let Some(renderer_body) = output_body.iter().find_map(|entry| {
            if let Entry::Property(p) = entry
                && p.name == "renderer"
            {
                p.body.as_ref()
            } else {
                None
            }
        }) else {
            return;
        };
        for entry in renderer_body.iter() {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
                && let Ok(value) = self.eval_expr(expr, &converter_scope, depth)
            {
                converter_scope.set(&prop.name, value);
            }
        }
        let Some(converters_body) = renderer_body.iter().find_map(|entry| {
            if let Entry::Property(p) = entry
                && p.name == "converters"
            {
                p.body.as_ref()
            } else {
                None
            }
        }) else {
            return;
        };
        // Each converter is a DynProperty: [ClassName] = (x) -> expr
        for centry in converters_body.iter() {
            if let Entry::DynProperty(key_expr, val_expr) = centry {
                // Extract the class name from the key expression
                let class_name = match key_expr {
                    Expr::Ident(name) => name.clone(),
                    Expr::Field(_, name) => name.clone(),
                    Expr::String(s) => s.to_string(),
                    _ => continue,
                };
                // Evaluate the lambda value
                if let Ok(lambda) = self.eval_expr(val_expr, &converter_scope, depth)
                    && matches!(lambda, Value::Lambda(..))
                {
                    self.converters.push((class_name, lambda));
                }
            }
        }
    }

    /// Apply `output.renderer.converters` to a value tree.
    /// Walks recursively, replacing typed objects with their converter output.
    pub fn apply_converters(&mut self, value: Value) -> Result<Value> {
        if self.converters.is_empty() {
            return Ok(value);
        }
        let converters = self.converters.clone();
        let mut memo = ConverterMemo::default();
        Ok(self
            .apply_converters_recursive(&value, &converters, Vec::new(), &mut memo)?
            .unwrap_or(value))
    }

    /// Apply converters to `value` and everything under it. `None` means no
    /// converter applied anywhere, so the caller can keep `value` (and share
    /// its maps) instead of rebuilding an identical copy.
    fn apply_converters_recursive(
        &mut self,
        value: &Value,
        converters: &[(String, Value)],
        blocked_root_converters: Vec<String>,
        memo: &mut ConverterMemo,
    ) -> Result<Option<Value>> {
        // A shared object or list (one mapping referenced from several places,
        // like the same steps under every hook) converts the same way each
        // time, so it is converted once.
        let key = match value {
            Value::Object(map, src) if blocked_root_converters.is_empty() => Some((
                Arc::as_ptr(map) as usize,
                src.as_ref().map_or(0, |src| Arc::as_ptr(src) as usize),
            )),
            Value::List(items) if blocked_root_converters.is_empty() => {
                Some((items.items_ptr() as usize, items.kind() as usize))
            }
            _ => None,
        };
        if let Some(key) = key
            && let Some((_, converted)) = memo.get(&key)
        {
            return Ok(converted.clone());
        }
        let converted =
            self.apply_converters_uncached(value, converters, blocked_root_converters, memo)?;
        if let Some(key) = key {
            // The original is kept so its address is not reused by another
            // value while the memo is alive.
            memo.insert(key, (value.clone(), converted.clone()));
        }
        Ok(converted)
    }

    fn apply_converters_uncached(
        &mut self,
        value: &Value,
        converters: &[(String, Value)],
        blocked_root_converters: Vec<String>,
        memo: &mut ConverterMemo,
    ) -> Result<Option<Value>> {
        match value {
            Value::Object(map, src) => {
                // Check if this object has a type_name that matches a converter
                let type_names = src
                    .iter()
                    .flat_map(|source| {
                        source
                            .type_name
                            .iter()
                            .chain(source.parent_type_names.iter())
                    })
                    .collect::<Vec<_>>();

                if !type_names.is_empty() {
                    for type_name in type_names {
                        for (conv_name, lambda) in converters {
                            if type_names_match(conv_name, type_name)
                                && !blocked_root_converters.contains(conv_name)
                                && let Value::Lambda(params, body, captured) = lambda
                            {
                                let mut call_scope = Scope::for_call(captured);
                                // Bind the object as the first parameter
                                if let Some(param) = params.first() {
                                    call_scope.declare(param, value.clone());
                                }
                                let result = self.eval_expr(body, &call_scope, 0)?;
                                let mut blocked = blocked_root_converters;
                                blocked.push(conv_name.clone());
                                let converted = self.apply_converters_recursive(
                                    &result, converters, blocked, memo,
                                )?;
                                return Ok(Some(converted.unwrap_or(result)));
                            }
                        }
                    }
                }

                // No converter matched — recurse into children
                let mut new_map: Option<ObjectMap> = None;
                for (index, (k, v)) in map.iter().enumerate() {
                    let converted =
                        self.apply_converters_recursive(v, converters, Vec::new(), memo)?;
                    match (&mut new_map, converted) {
                        (Some(new_map), converted) => {
                            new_map.insert(k.clone(), converted.unwrap_or_else(|| v.clone()));
                        }
                        (None, Some(converted)) => {
                            let mut changed =
                                ObjectMap::with_capacity_and_hasher(map.len(), Default::default());
                            changed.extend(
                                map.iter().take(index).map(|(k, v)| (k.clone(), v.clone())),
                            );
                            changed.insert(k.clone(), converted);
                            new_map = Some(changed);
                        }
                        (None, None) => {}
                    }
                }
                Ok(new_map.map(|new_map| Value::Object(Arc::new(new_map), src.clone())))
            }
            Value::List(items) => {
                let mut new_items: Option<Vec<Value>> = None;
                for (index, item) in items.iter().enumerate() {
                    let converted =
                        self.apply_converters_recursive(item, converters, Vec::new(), memo)?;
                    match (&mut new_items, converted) {
                        (Some(new_items), converted) => {
                            new_items.push(converted.unwrap_or_else(|| item.clone()));
                        }
                        (None, Some(converted)) => {
                            let mut changed = Vec::with_capacity(items.len());
                            changed.extend_from_slice(&items[..index]);
                            changed.push(converted);
                            new_items = Some(changed);
                        }
                        (None, None) => {}
                    }
                }
                Ok(new_items.map(|new_items| Value::List(ListValue::new(items.kind(), new_items))))
            }
            _ => Ok(None),
        }
    }
}

/// Converted values by the address of the object or list they came from (see
/// `apply_converters_recursive`), each kept with its original.
type ConverterMemo = HashMap<(usize, usize), (Value, Option<Value>)>;

#[cfg(test)]
mod requested_field_tests {
    use std::collections::HashSet;

    use super::Evaluator;
    use crate::{Value, lexer, parser};

    #[test]
    fn inherited_late_properties_respect_requested_fields() {
        let test_dir = std::env::temp_dir().join(format!(
            "pklr-requested-fields-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(
            test_dir.join("Base.pkl"),
            "abstract module Base\nwanted = source + 1\nextra = source + 2\nsource = 1\n",
        )
        .unwrap();
        let child_path = test_dir.join("Child.pkl");
        let source = "extends \"Base.pkl\"\nsource = 2\n";
        std::fs::write(&child_path, source).unwrap();
        let tokens = lexer::lex(source).unwrap();
        let module = parser::parse(&tokens).unwrap();

        let value = Evaluator::default()
            .eval_module_with_scope(
                &module,
                &child_path,
                1,
                None,
                Some(HashSet::from_iter(["wanted".to_string()])),
            )
            .unwrap();
        let Value::Object(fields, _) = value else {
            panic!("expected an object");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields["wanted"], Value::Int(3));

        std::fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn mapping_source_captures_import_identities() {
        let test_dir = std::env::temp_dir().join(format!(
            "pklr-mapping-source-imports-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("Config.pkl"), "class Item {}\n").unwrap();
        let main_path = test_dir.join("main.pkl");
        std::fs::write(
            &main_path,
            "import \"Config.pkl\"\nresult = new Mapping { [\"item\"] = new Config.Item {} }\n",
        )
        .unwrap();

        let value = Evaluator::default().eval_file(&main_path).unwrap();
        let Value::Object(fields, _) = value else {
            panic!("expected module object");
        };
        let Value::Object(_, Some(source)) = &fields["result"] else {
            panic!("expected mapping object source");
        };
        assert!(source.scope().contains_key("Config"));
        assert!(source.scope_module_identities().contains_key("Config"));

        std::fs::remove_dir_all(test_dir).unwrap();
    }
}

#[cfg(test)]
mod glob_tests {
    use super::{glob_matches, max_glob_depth};

    #[test]
    fn double_star_crosses_directories() {
        assert!(glob_matches("**.pkl", "config/foo.pkl"));
        assert!(glob_matches("a/**/b.pkl", "a/x/y/b.pkl"));
    }

    #[test]
    fn double_star_slash_keeps_literal_separator() {
        assert!(glob_matches("**/foo.pkl", "foo.pkl"));
        assert!(glob_matches("**/foo.pkl", "config/foo.pkl"));
    }

    #[test]
    fn star_stays_in_one_directory_segment() {
        assert!(glob_matches("*/*.pkl", "config/foo.pkl"));
        assert!(!glob_matches("*/*.pkl", "nested/config/foo.pkl"));
    }

    #[test]
    fn non_recursive_patterns_have_bounded_depth() {
        assert_eq!(max_glob_depth("*.pkl"), Some(0));
        assert_eq!(max_glob_depth("*/*.pkl"), Some(1));
        assert_eq!(max_glob_depth("**.pkl"), None);
    }
}

#[cfg(test)]
mod remote_relative_tests {
    use super::{canonical_remote_module_identity, resolve_http_relative};

    #[test]
    fn http_relative_resolves_against_base_directory() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "../Lib.pkl").as_deref(),
            Some("https://example.com/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_ignores_base_query_and_fragment() {
        assert_eq!(
            resolve_http_relative(
                "https://example.com/cfg/Main.pkl?version=2#part",
                "../Lib.pkl"
            )
            .as_deref(),
            Some("https://example.com/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_keeps_absolute_path_references_absolute() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "/shared/Lib.pkl").as_deref(),
            Some("https://example.com/shared/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_clamps_above_root_segments() {
        assert_eq!(
            resolve_http_relative("https://example.com/Main.pkl", "../../Lib.pkl").as_deref(),
            Some("https://example.com/Lib.pkl")
        );
    }

    #[test]
    fn http_relative_resolves_query_only_references() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "?v=2").as_deref(),
            Some("https://example.com/cfg/Main.pkl?v=2")
        );
    }

    #[test]
    fn http_relative_resolves_fragment_only_references() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "#frag").as_deref(),
            Some("https://example.com/cfg/Main.pkl#frag")
        );
    }

    #[test]
    fn http_relative_resolves_network_path_references() {
        assert_eq!(
            resolve_http_relative("https://example.com/cfg/Main.pkl", "//cdn.example/Lib.pkl")
                .as_deref(),
            Some("https://cdn.example/Lib.pkl")
        );
    }

    #[test]
    fn remote_module_identity_normalizes_dot_segments() {
        let absolute =
            canonical_remote_module_identity("https://example.com/cfg/../shared/Lib.pkl");
        let relative =
            resolve_http_relative("https://example.com/cfg/Main.pkl", "../shared/Lib.pkl").unwrap();

        assert_eq!(absolute, relative);
        assert_eq!(absolute, "https://example.com/shared/Lib.pkl");
    }
}

impl Evaluator {
    fn same_local_path(&mut self, left: &Path, right: &Path) -> Result<bool> {
        if left == right {
            return Ok(true);
        }
        let left_key = self
            .canonicalize_io(left)
            .unwrap_or_else(|_| left.to_path_buf());
        let right_key = self
            .canonicalize_io(right)
            .unwrap_or_else(|_| right.to_path_buf());
        Ok(left_key == right_key)
    }

    fn bind_deferred_inherited_imports(
        &mut self,
        deferred: &[(String, PathBuf)],
        inherited_path: &Path,
        inherited_val: &Value,
        scope: &mut Scope,
    ) -> Result<()> {
        for (alias, alias_path) in deferred {
            if self.same_local_path(alias_path, inherited_path)? {
                scope.declare(alias, inherited_val.clone());
                let identity = self.module_type_namespace(alias_path);
                scope.set_module_identity(alias.clone(), identity);
            }
        }
        Ok(())
    }
}

/// Bind a local or property of a listing body for the body's entries.
fn bind_listing_member(scope: &mut Scope, prop: &Property, value: Value) {
    if has_modifier(&prop.modifiers, Modifier::Local) {
        scope.declare(&prop.name, value);
    } else {
        scope.set(&prop.name, value);
    }
}

/// Standard library classes declared `external`. User code can neither
/// instantiate nor extend them.
const EXTERNAL_CLASSES: &[&str] = &[
    "Any",
    "Null",
    "Class",
    "TypeAlias",
    "Module",
    "Number",
    "Int",
    "Float",
    "Boolean",
    "String",
    "Regex",
    "Duration",
    "DataSize",
    "Object",
    "Function",
    "Function0",
    "Function1",
    "Function2",
    "Function3",
    "Function4",
    "Function5",
    "Pair",
    "Collection",
    "IntSeq",
    "VarArgs",
    "List",
    "Set",
    "Map",
    "Bytes",
];

/// Abstract standard library classes that aren't external.
const ABSTRACT_STDLIB_CLASSES: &[&str] = &[
    "Annotation",
    "Typed",
    "BaseValueRenderer",
    "ValueRenderer",
    "BytesRenderer",
];

/// Standard library classes that are neither `open` nor `abstract`.
const CLOSED_STDLIB_CLASSES: &[&str] = &["Dynamic", "Listing", "Mapping"];

/// A class name as pkl reports it, `module#Class`, from the class's
/// definition-site identity (`<module path>.<Class>`).
fn qualified_class_name(identity: Option<&str>, name: &str) -> String {
    let simple = name.rsplit('.').next().unwrap_or(name);
    let module = identity
        .and_then(|identity| identity.strip_suffix(simple))
        .and_then(|path| path.strip_suffix('.'))
        .and_then(|path| Path::new(path).file_stem())
        .map(|stem| stem.to_string_lossy().into_owned());
    match module {
        Some(module) => format!("{module}#{simple}"),
        None => simple.to_string(),
    }
}

/// Check that `class class_name extends parent_name` names a class pkl lets
/// user code extend.
fn check_supertype(
    scope: &Scope,
    class_name: &str,
    parent_name: &str,
    parent: Option<&Value>,
) -> Result<()> {
    if parent_name == class_name {
        return Err(Error::Eval(format!(
            "Class `{}` cannot extend itself.",
            qualified_class_name(Some(&scope.runtime_type_identity(class_name)), class_name)
        )));
    }
    if scope.get_type_alias(parent_name).is_some() {
        return Err(Error::Eval(format!(
            "`{parent_name}` is not a valid supertype."
        )));
    }
    match parent {
        Some(Value::Object(_, Some(source)))
            if source.type_name.is_some() && !source.is_open && !source.is_abstract =>
        {
            Err(Error::Eval(format!(
                "Cannot extend non-open class `{}`.",
                qualified_class_name(source.type_identity.as_deref(), parent_name)
            )))
        }
        // Built-ins are bound to a marker string of their own name.
        None | Some(Value::String(_)) => {
            if EXTERNAL_CLASSES.contains(&parent_name) {
                Err(Error::Eval(format!(
                    "Cannot extend external class `{parent_name}`."
                )))
            } else if CLOSED_STDLIB_CLASSES.contains(&parent_name) {
                Err(Error::Eval(format!(
                    "Cannot extend non-open class `{parent_name}`."
                )))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// Check that `new type_name { ... }` names a class user code can
/// instantiate.
fn check_instantiable(scope: &Scope, type_name: Option<&str>, class: Option<&Value>) -> Result<()> {
    let Some(type_name) = type_name else {
        return Ok(());
    };
    // The class a type alias names (`typealias R = Regex`).
    let mut resolved = type_name;
    let mut seen = HashSet::default();
    while seen.insert(resolved) {
        match scope.get_type_alias(resolved) {
            Some(crate::parser::TypeExpr::Named(target)) if !target.starts_with('*') => {
                resolved = target;
            }
            _ => break,
        }
    }
    match class {
        Some(Value::Object(_, Some(source))) if source.is_abstract => Err(Error::Eval(format!(
            "Cannot instantiate abstract class `{}`.",
            qualified_class_name(source.type_identity.as_deref(), type_name)
        ))),
        // A class is bound under its own name, or under a type alias of it.
        // Any other binding of an object (`local Foo2 = Foo`) is a value.
        Some(Value::Object(_, Some(source)))
            if !type_name.contains('.')
                && source.type_name.as_deref() != Some(type_name)
                && scope.get_type_alias(type_name).is_none()
                && scope.get(&type_alias_marker(type_name)).is_none()
                // Module metadata is carried on a source without entries,
                // whereas an ordinary object has its declaration body here.
                && !(source.entries.is_empty()
                    && (source.poisoned_members.is_some() || !source.deprecated.is_empty())) =>
        {
            Err(Error::Eval(format!(
                "Expected `{type_name}` to be a type, but it is not."
            )))
        }
        // Built-ins are bound to a marker string of their own name.
        None if EXTERNAL_CLASSES.contains(&resolved) => Err(Error::Eval(format!(
            "Cannot instantiate, or amend an instance of, external class `{resolved}`."
        ))),
        Some(Value::String(name)) if EXTERNAL_CLASSES.contains(&name.as_ref()) => Err(Error::Eval(
            format!("Cannot instantiate, or amend an instance of, external class `{name}`."),
        )),
        None | Some(Value::String(_)) if ABSTRACT_STDLIB_CLASSES.contains(&resolved) => Err(
            Error::Eval(format!("Cannot instantiate abstract class `{resolved}`.")),
        ),
        _ => Ok(()),
    }
}

/// The binding that marks `name` as a type alias (see `eval_type_alias`).
/// The NUL prefix keeps it apart from any pkl identifier.
fn type_alias_marker(name: &str) -> String {
    format!("\0typealias:{name}")
}

/// A module's name as pkl reports it: the name from its `module`
/// declaration, else its file name without the extension.
fn module_display_name(module: &Module, path: &Path) -> String {
    module.name.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    })
}

/// Standard library types that are not annotation classes.
const STDLIB_NON_ANNOTATION_TYPES: &[&str] = &[
    "Any",
    "Null",
    "Boolean",
    "Number",
    "Int",
    "Float",
    "String",
    "Duration",
    "DataSize",
    "Regex",
    "Object",
    "Dynamic",
    "Typed",
    "Listing",
    "Mapping",
    "List",
    "Set",
    "Map",
    "Pair",
    "IntSeq",
    "Function",
    "Function0",
    "Function1",
    "Function2",
    "Function3",
    "Function4",
    "Function5",
    "VarArgs",
    "Bytes",
    "Collection",
    "Module",
    "Class",
    "TypeAlias",
];

/// Check that each annotation on a module property names an annotation
/// class: a subclass of `Annotation`, not another class or a value.
fn check_module_annotations(module: &Module, scope: &Scope) -> Result<()> {
    let annotated = module
        .body
        .iter()
        .any(|entry| matches!(entry, Entry::Property(prop) if !prop.annotations.is_empty()));
    if !annotated {
        return Ok(());
    }
    let classes: HashMap<&str, Option<&str>> = module
        .body
        .iter()
        .filter_map(|entry| match entry {
            Entry::ClassDef(name, _, parent, _) => Some((name.as_str(), parent.as_deref())),
            _ => None,
        })
        .collect();
    let properties: HashSet<&str> = module
        .body
        .iter()
        .filter_map(|entry| match entry {
            Entry::Property(prop) => Some(prop.name.as_str()),
            _ => None,
        })
        .collect();
    let resolve_alias = |name: &str| -> Option<String> {
        let mut current = name.to_string();
        let mut seen = HashSet::default();
        while let Some(ty) = scope.get_type_alias(&current) {
            match ty {
                crate::parser::TypeExpr::Named(target)
                    if !target.starts_with('*') && seen.insert(current.clone()) =>
                {
                    current = target.clone();
                }
                _ => return None,
            }
        }
        Some(current)
    };
    let is_annotation_class = |name: &str| {
        let mut current = name.to_string();
        for _ in 0..=classes.len() {
            match classes.get(current.as_str()) {
                Some(Some(parent)) => match resolve_alias(parent) {
                    Some(parent) => current = parent,
                    None => return false,
                },
                Some(None) => return false,
                None => return !STDLIB_NON_ANNOTATION_TYPES.contains(&current.as_str()),
            }
        }
        false
    };
    for entry in module.body.iter() {
        let Entry::Property(prop) = entry else {
            continue;
        };
        for annotation in &prop.annotations {
            if annotation.name.contains('.') {
                continue;
            }
            let Some(resolved) = resolve_alias(&annotation.name) else {
                return Err(Error::Eval("Expected an annotation class.".into()));
            };
            let name = resolved.as_str();
            if classes.contains_key(name) {
                if !is_annotation_class(name) {
                    return Err(Error::Eval("Expected an annotation class.".into()));
                }
                continue;
            }
            if scope.get_type_alias(name).is_some() {
                continue;
            }
            if name == annotation.name && properties.contains(name) {
                return Err(Error::Eval(format!(
                    "Expected `{name}` to be a type, but it is not."
                )));
            }
            match scope.get(name) {
                Some(Value::String(marker)) if &**marker == name => {
                    if name != "Annotation" && STDLIB_NON_ANNOTATION_TYPES.contains(&name) {
                        return Err(Error::Eval("Expected an annotation class.".into()));
                    }
                }
                Some(Value::Object(_, Some(source)))
                    if source.type_name.as_deref() == Some(name) => {}
                Some(_) => {
                    return Err(Error::Eval(format!(
                        "Expected `{name}` to be a type, but it is not."
                    )));
                }
                None if STDLIB_NON_ANNOTATION_TYPES.contains(&name) => {
                    return Err(Error::Eval("Expected an annotation class.".into()));
                }
                None => {}
            }
        }
    }
    Ok(())
}

fn stdlib_module(name: &str) -> Value {
    let mut map = ObjectMap::default();
    if name == "base" {
        map.insert("Regex".into(), Value::String("Regex".into()));
    }
    Value::Object(Arc::new(map), None)
}

fn seed_builtins(scope: &mut Scope) {
    for name in [
        "Regex",
        "Dynamic",
        "Annotation",
        "Duration",
        "DataSize",
        "IntSeq",
        "Pair",
    ] {
        scope.set(name, Value::String(name.into()));
    }
}

/// Whether `IntSeq` in `scope` is still the built-in, which `seed_builtins`
/// binds to a marker string, rather than a user binding of that name.
fn int_seq_is_builtin(scope: &Scope) -> bool {
    matches!(scope.get("IntSeq"), Some(Value::String(name)) if &**name == "IntSeq")
}

/// Largest number of elements an `IntSeq` may produce. IntSeq is
/// materialized as a list, so an unbounded range would exhaust memory.
const MAX_INT_SEQ_LEN: i128 = 1_000_000;

/// Materialize `IntSeq(start, end).step(step)` as a list of ints. The range is
/// inclusive of `end` when a step lands on it, and empty when `step` points
/// away from `end` (e.g. `IntSeq(5, 1)` with the default step of 1).
fn int_seq(start: i64, end: i64, step: i64) -> Result<Value> {
    if step == 0 {
        return Err(Error::Eval("IntSeq step must not be 0".into()));
    }
    let (start_w, end_w, step_w) = (start as i128, end as i128, step as i128);
    let len = if (step > 0 && start <= end) || (step < 0 && start >= end) {
        (end_w - start_w) / step_w + 1
    } else {
        0
    };
    if len > MAX_INT_SEQ_LEN {
        return Err(Error::Eval(format!(
            "IntSeq({start}, {end}) with step {step} has {len} elements, more than the supported maximum of {MAX_INT_SEQ_LEN}"
        )));
    }
    Ok(Value::List(
        (0..len)
            .map(|i| Value::Int((start_w + i * step_w) as i64))
            .collect::<Vec<_>>()
            .into(),
    ))
}

/// The key/value pairs a for-generator iterates over.
fn collection_to_items(v: Value) -> Result<Vec<(Value, Value)>> {
    check_iterable(&v)?;
    Ok(match v {
        Value::List(items) => items
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, v)| (Value::Int(i as i64), v))
            .collect(),
        Value::Object(map, _) => map
            .iter()
            .map(|(k, v)| (Value::String(k.clone()), v.clone()))
            .collect(),
        _ => vec![],
    })
}

/// A value's type as pkl names it in error messages: `module#Class` for a
/// class instance.
fn value_type_display(value: &Value) -> String {
    match value {
        Value::Object(_, Some(source)) if source.type_name.is_some() => qualified_class_name(
            source.type_identity.as_deref(),
            source.type_name.as_deref().unwrap_or_default(),
        ),
        Value::Object(..) => "Dynamic".to_string(),
        value => value_type_name(value).to_string(),
    }
}

/// The elements `...value` adds to a Listing. Only collections and Listings
/// can be spread into one: objects have properties or entries instead.
fn listing_spread_values(value: Value) -> Result<Arc<Vec<Value>>> {
    check_iterable(&value)?;
    match value {
        Value::List(values) => Ok(Arc::new(values.to_vec())),
        Value::Object(members, _) if members.is_empty() => Ok(Arc::default()),
        Value::Object(_, source) => {
            let members = if source.is_some_and(|s| s.kind == ObjectKind::Mapping) {
                "entries"
            } else {
                "properties"
            };
            Err(Error::Eval(format!(
                "Cannot spread object containing {members} into object of type `Listing`."
            )))
        }
        _ => Ok(Arc::default()),
    }
}

/// Reject elements in a body that amends or instantiates a class: only
/// Listings and Dynamic objects can have elements.
fn check_no_elements(source: &ObjectSource, entries: &[Entry]) -> Result<()> {
    match &source.type_name {
        Some(type_name) => check_member_kinds(
            &qualified_class_name(source.type_identity.as_deref(), type_name),
            entries,
            true,
        ),
        None if source.kind == ObjectKind::Mapping => check_member_kinds("Mapping", entries, false),
        None => Ok(()),
    }
}

/// Whether a body (or a generator in it) has a member matching `is_kind`.
fn body_has(entries: &[Entry], is_kind: &dyn Fn(&Entry) -> bool) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::ForGenerator(generator) => body_has(&generator.body, is_kind),
        Entry::WhenGenerator(generator) => {
            body_has(&generator.body, is_kind)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(|body| body_has(body, is_kind))
        }
        entry => is_kind(entry),
    })
}

/// Reject elements in the body of an object of type `type_display`, and
/// entries too when `typed` (a class instance has only properties).
fn check_member_kinds(type_display: &str, entries: &[Entry], typed: bool) -> Result<()> {
    if body_has(entries, &|entry| matches!(entry, Entry::Elem(_))) {
        return Err(Error::Eval(format!(
            "Object of type `{type_display}` cannot have an element."
        )));
    }
    if typed && body_has(entries, &|entry| matches!(entry, Entry::DynProperty(..))) {
        return Err(Error::Eval(format!(
            "Object of type `{type_display}` cannot have an entry."
        )));
    }
    Ok(())
}

/// Reject a value a generator or spread can't iterate over: anything but a
/// collection, Listing, Mapping or Dynamic object. Instances of classes are
/// typed objects and can't be iterated either.
fn check_iterable(value: &Value) -> Result<()> {
    match value {
        Value::List(_) => Ok(()),
        Value::Object(_, source) if source.as_ref().is_none_or(|s| s.type_name.is_none()) => Ok(()),
        value => Err(Error::Eval(format!(
            "Cannot iterate over value of type `{}`.",
            value_type_display(value)
        ))),
    }
}

#[cfg(test)]
mod auto_trait_tests {
    use super::Evaluator;

    #[test]
    fn evaluator_is_sync() {
        fn assert_sync<T: Sync>() {}

        assert_sync::<Evaluator>();
    }
}

#[cfg(test)]
mod package_uri_tests {
    #[cfg(all(feature = "native-io", feature = "package-zip"))]
    use std::path::PathBuf;

    #[cfg(all(feature = "native-io", feature = "package-zip"))]
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

#[cfg(all(test, feature = "native-io"))]
mod super_deprecation_tests {
    #[test]
    fn super_property_access_warns_once() {
        let mut evaluator = super::Evaluator::default();
        let source = r#"
local base = new {
  @Deprecated { message = "use replacement" }
  old = 1
}
result = (base) { old = super.old + super.old }
"#;
        let value = evaluator
            .eval_source(source, std::path::Path::new("super.pkl"))
            .unwrap();
        assert_eq!(value.to_json()["result"]["old"], 2);
        assert_eq!(evaluator.warned_deprecated.len(), 1);
        assert!(
            evaluator
                .warned_deprecated
                .contains(&("old".into(), Some("use replacement".into())))
        );
    }
}
