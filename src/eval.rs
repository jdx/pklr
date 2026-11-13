use std::collections::BTreeMap;

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::rc::Rc;
use std::sync::{Arc, RwLock, Weak};

use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use crate::capabilities::EvalCapabilities;
use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{
    self, BinOp, Body, Entry, Expr, LocalBindings, Modifier, Module, Property, StringInterpPart,
    UnOp,
};
use crate::value::{
    CapturedScope, ListKind, ListValue, NameSet, ObjectKind, ObjectMap, ObjectSource,
    PoisonedMember, ScopeMap, TypeAliasMap, Value,
};

mod amend;
mod analysis;
mod calls;
mod classes;
mod converters;
mod entries;
mod expr;
mod failed;
mod glob;
mod lambda_guard;
mod listing;
mod loading;
mod mapping;
mod members;
mod module;
mod output;
mod package;
mod parsers;
mod prefetch;
mod reference;
mod reflect;
mod reflect_base;
mod relative_path;
mod remote;
pub(crate) mod render;
mod resource;
mod scope;
mod setup;
pub(crate) mod stdlib;
mod trace;
mod typecheck;
mod types;
mod yaml_parser;

use amend::*;
use analysis::*;
use calls::*;
use classes::*;
use entries::*;
use expr::*;
use glob::*;
pub use glob::{GlobAccess, NativeGlobAccess, expand_glob, expand_glob_with};
use lambda_guard::*;
use listing::*;
use loading::*;
use mapping::*;
use members::*;
use module::*;
pub(crate) use package::write_atomic;
use package::*;
use remote::*;
pub(crate) use remote::{parse_triple_dot_path, resolve_triple_dot};
pub(crate) use resource::file_uri_path;
use resource::{DEFAULT_ALLOWED_RESOURCES, absolute_clean, file_uri};
pub(crate) use scope::Scope as ListScope;
pub(crate) use scope::SourceScope;
use scope::*;
use typecheck::*;
use types::*;

/// The error a cancelled evaluation fails with.
fn cancelled() -> Error {
    Error::Eval("evaluation cancelled".to_string())
}

const DYNAMIC_SIBLING_REF: &str = "\0pklr:dynamic-sibling";
// Scoped context for lazily materializing an inherited `ModuleOutput.text`.
// It is an impossible user identifier and is captured by output-body lambdas.
const MODULE_OUTPUT_TEXT_CONTEXT: &str = "\0pklr:module-output-text-context";
/// How many more quiet late-binding passes than the longest chain of forward
/// references (`forward_reference_chain`) run before the passes stop. The
/// chain is what the model of how reads travel needs. This margin is slack,
/// not derived from it: it covers a read the model might miss. Margins of 0 and
/// 1 gave the same result on every valid program in about 50,000 fuzzed
/// programs each; the one program that differed at 0 is one pkl rejects. The
/// value is a judgement that two passes are cheap insurance. Each pass of
/// slack costs one pass over every late property, and only values that
/// capture a scope pay it.
const QUIET_PASS_MARGIN: usize = 2;

/// Evaluates pkl source files to [`Value`].
pub struct Evaluator {
    base_path: PathBuf,
    /// Maximum recursion depth, which also bounds import and amends chains
    max_depth: usize,
    /// Whether the host set `max_depth`, which then applies on any stack
    max_depth_set: bool,
    /// Cache for fetched HTTP sources (URL → source text)
    http_cache: HashMap<String, String>,
    /// The constraint (its source text) and the value that failed the last
    /// type check, for the error that reports it.
    type_violation: Option<(String, Value)>,
    /// How many constraint bodies are being evaluated, and the value a nested
    /// constraint last bound `this` to. pkl leaks that binding to the
    /// enclosing constraint, which shows when the enclosing one fails.
    constraint_nesting: u32,
    /// Lambda bodies (by address) whose const-scope violation pkl raises when
    /// the lambda runs, from the modules that declared them.
    deferred_const: HashMap<usize, (Arc<Expr>, Arc<str>)>,
    leaked_this: Option<Value>,
    /// Whether any [`Value::Failed`] member was made, so reads that look for
    /// failed members can be skipped until one exists.
    failed_seen: bool,
    /// The last declared property value whose members were coerced, with its
    /// type: the declaration's own check need not walk it again.
    last_coerced: Option<(Value, usize)>,
    /// Nonzero while a constraint predicate runs: its parameter checks are
    /// eager, as in pkl.
    eager_param_checks: u32,
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
    narrowed_import_cache: HashMap<(PathBuf, Vec<Name>), Value>,
    /// Narrowed evaluations of a module that read their own in-flight import.
    narrowed_in_flight: HashSet<(PathBuf, Vec<Name>)>,
    /// Parsed local modules (canonical path → AST)
    parse_cache: HashMap<PathBuf, Arc<Module>>,
    /// `referenced_roots` of object bodies amended in this run, keyed by the
    /// body's address. Each entry holds its body so the address stays unique.
    body_roots_cache: HashMap<usize, (crate::parser::Body, Arc<HashSet<Name>>)>,
    /// Dependency-order plans for module bodies evaluated in this run, keyed
    /// like `body_roots_cache`. A narrowed import can evaluate one body more
    /// than once, but its dependency graph is stable.
    plan_cache: HashMap<usize, (crate::parser::Body, Arc<EvaluationPlan>)>,
    /// Delayed errors for const-local lambdas, keyed by their capture map.
    lambda_guards: HashMap<usize, LambdaGuardEntry>,
    /// Guard registry size at which the next weak-entry cleanup runs.
    lambda_guard_prune_at: usize,
    #[cfg(test)]
    /// Total entries visited by guard-registry cleanup, for amortization tests.
    lambda_guard_prune_scans: usize,
    #[cfg(test)]
    /// Late-binding passes run, for convergence tests.
    late_passes_run: usize,
    /// Resources read in this run, by URI. As in Pkl, reading a resource again
    /// returns the first result, so reads are deterministic.
    resource_cache: HashMap<String, Value>,
    /// URI prefixes resources may be read from. Empty denies all resources.
    allowed_resources: Vec<String>,
    /// Directories searched, in order, for `modulepath:` module URIs.
    module_path: Vec<PathBuf>,
    /// Patterns module URIs must match to load; `None` allows every module.
    /// A pattern that fails to compile is kept as its error message, which
    /// every module load then reports.
    allowed_modules: Option<std::result::Result<Vec<fancy_regex::Regex>, String>>,
    /// The `package:` URI (without its `#` fragment) of each package
    /// loaded under a module policy, by the zip URL or HTTP root its modules
    /// are read from, so a module inside it is checked by its package URI.
    package_bases: HashMap<String, String>,
    /// The files loaded through a `modulepath:` URI in this evaluation, by
    /// absolute path, with that URI. A relative import made from such a file
    /// stays a `modulepath:` URI, and under a module policy the file is not
    /// checked again as a `file:` URI when it is loaded.
    module_path_admitted: HashMap<PathBuf, String>,
    environment: Option<BTreeMap<String, String>>,
    external_properties: BTreeMap<String, String>,
    /// Final scopes for modules evaluated in this run, used to preserve inherited locals.
    module_scopes: HashMap<PathBuf, ModuleScopeSnapshot>,
    /// The key of the saved scope of the module each module amends or extends,
    /// whether that module is a local file, a remote one or a package's.
    inherited_bases: HashMap<PathBuf, PathBuf>,
    /// The saved-scope key of each import identity looked up in this run.
    scope_keys: HashMap<String, PathBuf>,
    /// The scope each module's saved bindings make, built once per run.
    layered_scopes: HashMap<PathBuf, Scope>,
    /// Whether a module evaluated in this run has a failed property that
    /// rendering it would output (see `PoisonedMember::rendered`), so the
    /// output must be checked for one.
    rendered_member_failed: bool,
    /// Per module namespace, the properties that pass `module` to
    /// `relativePathTo` (with whether the module renders them). An amended
    /// copy of the module makes `module` the amending object, so reading one
    /// of them there fails.
    module_arg_members: HashMap<String, Vec<(Name, bool)>>,
    /// Set when `relativePathTo(module)` runs, so the module member being
    /// evaluated can be recorded in `module_arg_members`.
    module_arg_used: bool,
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
    /// Expected metadata SHA-256 per package zip URL, from `::sha256:` URIs.
    package_checksums: HashMap<String, String>,
    /// Per zip URL, the extracted directory and checksum it was verified against.
    #[cfg(feature = "package-zip")]
    verified_packages: HashMap<String, (PathBuf, String)>,
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
    /// Cached templates for the built-in classes used by module output.
    builtin_classes: HashMap<&'static str, Value>,
    /// Referent types of `pkl:ref` references.
    refs: reference::RefState,
    /// `output` declarations belonging to the module being evaluated.
    output_props: Vec<Arc<Property>>,
    /// Evaluated module output, when the module declares an `output` block.
    module_output: Option<Value>,
    /// Whether an output declaration explicitly assigned `output.value`.
    output_sets_value: bool,
    /// The default renderer used while evaluating module output.
    output_format: render::RendererKind,
    /// Set of (property_name, message) pairs already warned about.
    /// Used to deduplicate `@Deprecated` warnings so a deprecated property
    /// referenced inside a loop or template doesn't flood stderr.
    warned_deprecated: std::collections::HashSet<(String, Option<String>)>,
    /// Class identities currently being materialized as implicit typed
    /// defaults. This makes self- and mutually-recursive defaults fail
    /// cleanly instead of exhausting the process stack.
    active_typed_defaults: HashSet<Name>,
    /// Original values for Mapping keys created in this evaluation. Mapping
    /// storage keys preserve equality and rendering, but compound keys cannot
    /// be faithfully reconstructed from that string representation alone.
    mapping_key_values: HashMap<Arc<str>, Value>,
    /// Names declared by `module a.b.C` headers, by module identity (see
    /// `module_type_namespace`), for error messages.
    module_names: HashMap<String, String>,
    /// The classes and type aliases each module declares, by module identity,
    /// for `pkl:reflect`.
    module_declarations: HashMap<String, Arc<Vec<crate::parser::Declaration>>>,
    /// The declarations of parsed modules, by the address of the module's
    /// body, which the weak reference confirms is still that module's.
    parsed_declarations: HashMap<usize, ParsedDeclarations>,
    /// Whether an evaluation moved to the large stack, so what the evaluator
    /// holds may be nested too deeply to free on a small one.
    grew: bool,
}

type ParsedDeclarations = (
    std::sync::Weak<Vec<crate::parser::Entry>>,
    Arc<Vec<crate::parser::Declaration>>,
);

#[derive(Clone, Default)]
struct MappingInheritedDefault {
    value: Option<Value>,
    entries: Option<crate::parser::Body>,
}

#[derive(Clone, Default)]
struct ModuleScopeSnapshot {
    values: Arc<ScopeMap>,
    type_aliases: Arc<TypeAliasMap>,
    module_identities: IndexMap<String, String>,
    /// The module that declares each inherited type alias, when it is not
    /// the module itself. Its imports resolve the names in the alias.
    alias_definers: HashMap<Arc<str>, PathBuf>,
    /// The classes and type aliases an importer can name as `m.Name`.
    exported_types: HashSet<Arc<str>>,
    late_properties: Vec<Arc<Property>>,
}

#[cfg(feature = "native-io")]
impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
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
