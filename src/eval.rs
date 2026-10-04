use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

use crate::capabilities::EvalCapabilities;
use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{
    self, BinOp, Body, Entry, Expr, Modifier, Module, Property, StringInterpPart, UnOp,
};
use crate::value::{CapturedScope, NameSet, ObjectSource, ScopeMap, Value};

mod analysis;
mod bridge;
mod glob;
mod mapping;
mod package;
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
use scope::*;
use types::*;

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
    /// Final scopes for modules evaluated in this run, used to preserve inherited locals.
    module_scopes: HashMap<PathBuf, ModuleScopeSnapshot>,
    /// Environment variables read during evaluation (name → observed value).
    env_reads: BTreeMap<String, Option<String>>,
    /// Local files currently being evaluated with inherited scope.
    scoped_imports_in_flight: HashSet<PathBuf>,
    /// Host-provided IO for files, environment, HTTP, packages, and globs.
    capabilities: Box<dyn EvalCapabilities>,
    /// Set while the evaluator runs on a worker thread for the async API;
    /// capability calls then go to the async caller.
    bridge: Option<bridge::Bridge>,
    /// The capabilities' blocking counterpart, used for calls other than HTTP
    /// fetches while evaluating for an async caller.
    local_capabilities: Option<Box<dyn EvalCapabilities>>,
    /// Extracted package zip directories (zip URL → temp dir path)
    #[cfg(feature = "package-zip-core")]
    package_dirs: HashMap<String, PathBuf>,
    /// Directory used to persist downloaded package content across evaluators.
    package_cache_dir: Option<PathBuf>,
    /// HTTP URL roots whose sources belong to direct-download packages.
    package_http_roots: HashSet<String>,
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
    /// Number of poison-and-retry passes in progress (see `BodyOutcome`).
    /// Output with side effects, such as `trace`, is not repeated in them.
    retry_passes: usize,
}

/// A declared-type check of a generator-produced property. It runs in the
/// iteration scope it was evaluated in, with the finished enclosing object's
/// members layered over it, except names the iteration declares itself.
struct PendingTypeCheck {
    /// The property's name and declared type.
    name: String,
    ty: crate::parser::TypeExpr,
    value: Value,
    /// The iteration scope, and the names in it whose binding is poisoned.
    /// Saved only when the type has a constraint or names a type the
    /// iteration declares; any other type resolves in the generator entry's
    /// scope.
    scope: Option<(CapturedScope, IndexMap<String, String>)>,
    /// Whether the type has a constraint, which may read the object's
    /// members: those are then resolved lexically first, then on the
    /// finished object.
    constrained: bool,
    /// Loop variables and the locals, classes and type aliases of the
    /// generator bodies around the property: these win over object members.
    iteration_names: FxHashSet<String>,
    /// The enclosing object's generator entry that produced it, set once
    /// that entry finishes.
    entry_index: Option<usize>,
}

/// What the deferred type checks of a finished object body read.
struct FinishedBody<'a> {
    entries: &'a [Entry],
    child_scope: &'a Scope,
    entry_scopes: Option<&'a [Option<Arc<CapturedScope>>]>,
    entry_owners: &'a EntryOwners,
    own_body_scope: Option<(&'a Scope, &'a HashSet<String>)>,
    local_bindings: &'a FxHashMap<usize, std::result::Result<Value, String>>,
    all_props: &'a IndexMap<String, Value>,
}

impl FinishedBody<'_> {
    fn entry_scope(&self, entry_index: usize) -> Scope {
        scope_for_object_entry(
            entry_index,
            self.child_scope,
            self.entry_scopes,
            self.entry_owners,
            self.own_body_scope,
        )
    }

    fn owner(&self, entry_index: usize) -> Option<*const CapturedScope> {
        self.entry_scopes
            .and_then(|scopes| scopes.get(entry_index))
            .and_then(Option::as_ref)
            .map(Arc::as_ptr)
    }

    fn hides_member(&self, entry_index: usize, name: &str) -> bool {
        self.entry_owners
            .hides_member(entry_index, self.entry_scopes, self.own_body_scope, name)
    }

    /// The locals of the body that wrote the entry at `entry_index`, with
    /// their bindings.
    fn own_bindings(
        &self,
        entry_index: usize,
    ) -> Vec<(String, std::result::Result<Value, String>)> {
        same_body_local_bindings(
            self.entries,
            entry_index,
            &|index| self.owner(index),
            self.local_bindings,
        )
    }

    /// The scope a deferred check of the entry at `entry_index` runs in: its
    /// entry scope, with the finished members it does not resolve lexically.
    fn check_scope(&self, entry_index: usize) -> Scope {
        let mut scope = self.entry_scope(entry_index).child();
        layer_finished_members(
            &mut scope,
            self.all_props,
            |name| self.hides_member(entry_index, name),
            &self.own_bindings(entry_index),
        );
        scope
    }
}

/// A typed local of an object body whose check waits for the finished body:
/// the local, its entry index, and the type aliases its type resolved to
/// where it was bound.
type DeferredLocal<'a> = (&'a Property, usize, Vec<(String, crate::parser::TypeExpr)>);

/// The result of checking a typed local.
enum LocalCheck {
    Passed,
    /// Reading the local must fail with this error.
    Failed(String),
    /// The constraint reads a name that is not bound yet. When the local is
    /// bound, its check waits for the finished body. If the name is unbound
    /// even then (a member of an enclosing body bound later), the check
    /// cannot be decided and the local is left unchecked.
    Unresolved,
}

/// The result of evaluating a body whose typed locals may need another pass.
enum BodyOutcome<T> {
    Done(T),
    /// These typed locals (name, type error) failed their check against the
    /// finished body. Evaluate the body again with them poisoned, so only
    /// an actual read fails.
    PoisonAndRetry(Vec<(String, String)>),
}

/// Which entries of an object body have their declared types checked once
/// the body is evaluated.
#[derive(Clone)]
enum TypeChecks {
    /// Every entry: an object's own body.
    All,
    /// None: a class body, whose defaults an instance checks when built.
    Nothing,
    /// The entries at these indices: the ones an amendment wrote itself.
    Entries(Rc<FxHashSet<usize>>),
    /// Every entry of a `for`/`when` body, handed to the enclosing object to
    /// check once that object is complete. Carries the names the iterations
    /// around it declare (loop variables, enclosing generator bodies'
    /// declarations).
    Generator(Rc<FxHashSet<String>>),
}

impl TypeChecks {
    fn includes(&self, entry_index: usize) -> bool {
        match self {
            TypeChecks::All | TypeChecks::Generator(_) => true,
            TypeChecks::Nothing => false,
            TypeChecks::Entries(indices) => indices.contains(&entry_index),
        }
    }

    /// Checks for the body of the generator at `entry_index`. `names` are
    /// what this iteration declares around that body: its loop variables,
    /// plus, inside a generator body, that body's own declarations.
    ///
    /// `body_typed` says whether the generator's body (or a generator nested
    /// in it) has a typed entry; without one there is nothing to check, and
    /// no per-iteration bookkeeping is built.
    fn for_generator(
        &self,
        entry_index: usize,
        body_typed: bool,
        names: impl FnOnce() -> Vec<String>,
    ) -> TypeChecks {
        if !body_typed || !self.includes(entry_index) {
            return TypeChecks::Nothing;
        }
        let names = names();
        match self {
            TypeChecks::Generator(outer) if names.is_empty() => {
                TypeChecks::Generator(Rc::clone(outer))
            }
            TypeChecks::Generator(outer) => {
                let mut all = (**outer).clone();
                all.extend(names);
                TypeChecks::Generator(Rc::new(all))
            }
            _ => TypeChecks::Generator(Rc::new(names.into_iter().collect())),
        }
    }

    fn is_generator(&self) -> bool {
        matches!(self, TypeChecks::Generator(_))
    }
}

#[derive(Clone, Default)]
struct MappingInheritedDefault {
    value: Option<Value>,
    entries: Option<crate::parser::Body>,
}

#[derive(Clone, Default)]
struct ModuleScopeSnapshot {
    values: ScopeMap,
    type_aliases: IndexMap<String, crate::parser::TypeExpr>,
    late_properties: Vec<Property>,
}

/// Lay a finished object's members over `scope`, the scope a deferred type
/// check of one of its entries runs in, following Pkl's lookup order: a name
/// the entry resolves lexically keeps that binding, and any other member
/// takes its final value from the finished object map.
///
/// `lexical(name)` says whether the entry resolves `name` lexically (to an
/// enclosing body's or module's binding, see `EntryOwners::hides_member`).
/// `own_bindings` are the locals of the entry's own body (and, for a
/// generator, the iteration's own declarations), with their bindings: they
/// are lexical too, even where a member has the same name, and are applied
/// last. Returns the names set or poisoned on `scope`.
fn layer_finished_members(
    scope: &mut Scope,
    members: &IndexMap<String, Value>,
    lexical: impl Fn(&str) -> bool,
    own_bindings: &[(String, std::result::Result<Value, String>)],
) -> FxHashSet<String> {
    let mut layered = FxHashSet::default();
    for (name, value) in members.iter() {
        if own_bindings.iter().any(|(own, _)| own == name) || lexical(name) {
            continue;
        }
        scope.set(name.clone(), value.clone());
        layered.insert(name.clone());
    }
    for (name, binding) in own_bindings {
        match binding {
            Ok(value) => scope.set(name.clone(), value.clone()),
            Err(message) => scope.poison(name.clone(), message.clone()),
        }
        layered.insert(name.clone());
    }
    layered
}

/// The closure of what checking a value against `ty` depends on, following
/// alias chains with `resolve`: the type aliases reached, with the definitions
/// `resolve` gave them, and every name reached. Names include the type names
/// themselves (a class is a value in scope, a dotted name also counts by its
/// root), the names read by constraints (of `ty` and of the aliases), and the
/// types named inside those constraints (`this is Small`, `as`), whose
/// aliases are followed too.
fn type_closure(
    ty: &crate::parser::TypeExpr,
    resolve: &dyn Fn(&str) -> Option<crate::parser::TypeExpr>,
) -> (Vec<(String, crate::parser::TypeExpr)>, FxHashSet<String>) {
    struct Closure<'a> {
        resolve: &'a dyn Fn(&str) -> Option<crate::parser::TypeExpr>,
        aliases: Vec<(String, crate::parser::TypeExpr)>,
        names: FxHashSet<String>,
    }
    impl Closure<'_> {
        fn name(&mut self, name: &str) {
            let name = name
                .trim_start_matches('*')
                .trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(name);
            if !self.names.insert(name.to_string()) {
                // Already visited, which also ends alias cycles.
                return;
            }
            if let Some(root) = name.split('.').next() {
                self.names.insert(root.to_string());
            }
            if let Some(alias) = (self.resolve)(name) {
                self.ty(&alias);
                self.aliases.push((name.to_string(), alias));
            }
        }
        fn ty(&mut self, ty: &crate::parser::TypeExpr) {
            use crate::parser::TypeExpr;
            match ty {
                TypeExpr::Named(name) => self.name(name),
                TypeExpr::Constrained(base, constraint) => {
                    // Everything the constraint reads, including the types it
                    // names; a name that is an alias is followed like any
                    // other.
                    let mut refs = HashSet::new();
                    collect_expr_refs(constraint, &mut refs, &HashSet::new());
                    for name in refs {
                        self.name(&name);
                    }
                    self.name(base);
                }
                TypeExpr::Nullable(inner) => self.ty(inner),
                TypeExpr::Union(variants) => {
                    for variant in variants {
                        self.ty(variant);
                    }
                }
                TypeExpr::Generic(name, args) => {
                    self.name(name);
                    for arg in args {
                        self.ty(arg);
                    }
                }
            }
        }
    }
    let mut closure = Closure {
        resolve,
        aliases: Vec::new(),
        names: FxHashSet::default(),
    };
    closure.ty(ty);
    (closure.aliases, closure.names)
}

/// `type_closure` with the aliases `scope` resolves.
fn type_check_dependencies(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
) -> (Vec<(String, crate::parser::TypeExpr)>, FxHashSet<String>) {
    type_closure(ty, &|name| scope.get_type_alias(name).cloned())
}

/// The type aliases a typed local's declared type resolves to where it is
/// bound. Its deferred check keeps these, so an alias of the same name
/// declared later in the body does not change what the local was declared
/// as.
fn bound_aliases(prop: &Property, scope: &Scope) -> Vec<(String, crate::parser::TypeExpr)> {
    prop.type_ann
        .as_ref()
        .map(|ty| type_check_dependencies(ty, scope).0)
        .unwrap_or_default()
}

/// The part of `scope` a deferred check against `ty` needs, saved for later:
/// the bindings of the names it can read (with failed ones), and the type
/// aliases it reaches as `scope` resolves them. Much cheaper than capturing
/// the whole scope.
fn capture_for_type_check(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
) -> (CapturedScope, IndexMap<String, String>) {
    let (aliases, names) = type_check_dependencies(ty, scope);
    let values = scope.flatten_names(names.iter().map(String::as_str));
    let poisoned = names
        .iter()
        .filter(|name| scope.get(name).is_none())
        .filter_map(|name| {
            scope
                .poison_of(name)
                .map(|message| (name.clone(), message.clone()))
        })
        .collect();
    let captured = CapturedScope {
        values,
        declared: NameSet::default(),
        body_members: HashSet::new(),
        module_identities: IndexMap::new(),
        type_aliases: aliases.into_iter().collect(),
        type_namespace: scope.type_namespace.clone(),
    };
    (captured, poisoned)
}

/// Whether a generator body, or a generator nested in it, has a typed entry.
fn generator_body_has_typed_entries(entries: &[Entry]) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::Property(prop) => prop.type_ann.is_some(),
        Entry::ForGenerator(fgen) => generator_body_has_typed_entries(&fgen.body),
        Entry::WhenGenerator(wgen) => {
            generator_body_has_typed_entries(&wgen.body)
                || wgen
                    .else_body
                    .as_ref()
                    .is_some_and(|body| generator_body_has_typed_entries(body))
        }
        _ => false,
    })
}

/// Whether a deferred check of a generator-produced property of type `ty`
/// needs the iteration scope it was produced in: anything the type reaches
/// (following alias chains as `scope`, the generator body's, resolves them,
/// and through its constraints) is a name the iteration declares
/// (`iteration_names`). Any other type resolves the same from the enclosing
/// object's generator entry.
fn type_needs_iteration_scope(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    iteration_names: &FxHashSet<String>,
) -> bool {
    if iteration_names.is_empty() {
        return false;
    }
    type_reaches(ty, &|name| scope.get_type_alias(name), &|name| {
        iteration_names.contains(name)
    })
}

/// Whether any name in the closure `type_closure` computes for `ty` (with
/// aliases from `resolve`) satisfies `found`. Stops at the first match and
/// allocates only to follow aliases and constraints, so the common case of a
/// plain type name is cheap.
fn type_reaches<'a>(
    ty: &'a crate::parser::TypeExpr,
    resolve: &dyn Fn(&str) -> Option<&'a crate::parser::TypeExpr>,
    found: &dyn Fn(&str) -> bool,
) -> bool {
    fn name<'a>(
        name: &str,
        resolve: &dyn Fn(&str) -> Option<&'a crate::parser::TypeExpr>,
        found: &dyn Fn(&str) -> bool,
        followed: &mut Vec<String>,
    ) -> bool {
        let name = name
            .trim_start_matches('*')
            .trim_end_matches('?')
            .split('<')
            .next()
            .unwrap_or(name);
        if found(name) || name.split('.').next().is_some_and(found) {
            return true;
        }
        let Some(alias) = resolve(name) else {
            return false;
        };
        if followed.iter().any(|seen| seen == name) {
            return false;
        }
        followed.push(name.to_string());
        visit(alias, resolve, found, followed)
    }
    fn visit<'a>(
        ty: &'a crate::parser::TypeExpr,
        resolve: &dyn Fn(&str) -> Option<&'a crate::parser::TypeExpr>,
        found: &dyn Fn(&str) -> bool,
        followed: &mut Vec<String>,
    ) -> bool {
        use crate::parser::TypeExpr;
        match ty {
            TypeExpr::Named(type_name) => name(type_name, resolve, found, followed),
            TypeExpr::Constrained(base, constraint) => {
                let mut refs = HashSet::new();
                collect_expr_refs(constraint, &mut refs, &HashSet::new());
                refs.iter().any(|read| name(read, resolve, found, followed))
                    || name(base, resolve, found, followed)
            }
            TypeExpr::Nullable(inner) => visit(inner, resolve, found, followed),
            TypeExpr::Union(variants) => variants
                .iter()
                .any(|variant| visit(variant, resolve, found, followed)),
            TypeExpr::Generic(type_name, args) => {
                name(type_name, resolve, found, followed)
                    || args.iter().any(|arg| visit(arg, resolve, found, followed))
            }
        }
    }
    visit(ty, resolve, found, &mut Vec::new())
}

/// Whether `ty`, or a type alias it names (following alias chains), has a
/// constraint, which may read names.
fn type_has_constraint(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    fn visit(ty: &crate::parser::TypeExpr, scope: &Scope, resolving: &mut Vec<String>) -> bool {
        use crate::parser::TypeExpr;
        match ty {
            TypeExpr::Constrained(..) => true,
            TypeExpr::Named(name) => {
                let name = name.trim_start_matches('*').trim_end_matches('?');
                let Some(alias) = scope.get_type_alias(name) else {
                    return false;
                };
                if resolving.iter().any(|seen| seen == name) {
                    return false;
                }
                resolving.push(name.to_string());
                let found = visit(alias, scope, resolving);
                resolving.pop();
                found
            }
            TypeExpr::Nullable(inner) => visit(inner, scope, resolving),
            TypeExpr::Union(variants) | TypeExpr::Generic(_, variants) => variants
                .iter()
                .any(|variant| visit(variant, scope, resolving)),
        }
    }
    visit(ty, scope, &mut Vec::new())
}

/// Whether `ty` names a type that is not resolvable at the entry at
/// `entry_index` but is a type alias declared later in the same body. A
/// body's alias applies only after its declaration, so a name that already
/// resolves (to an earlier or enclosing alias, or a class) keeps that
/// meaning. A name that does not resolve yet means the later alias, so the
/// check waits for the finished body, where it is registered.
fn names_later_alias(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    entries: &[Entry],
    entry_index: usize,
) -> bool {
    let later = |name: &str| {
        entries[entry_index + 1..]
            .iter()
            .find_map(|entry| match entry {
                Entry::TypeAlias(alias, ty) if alias == name => Some(ty),
                _ => None,
            })
    };
    // Cheap common case: the body declares no type alias after this entry.
    if !entries[entry_index + 1..]
        .iter()
        .any(|entry| matches!(entry, Entry::TypeAlias(..)))
    {
        return false;
    }
    // A name that resolves to something where the local is bound (an alias
    // in scope, a class or other bound value, or a built-in type) keeps that
    // meaning; only a name that resolves to nothing yet means its later
    // declaration.
    let unresolved = |name: &str| {
        scope.get_type_alias(name).is_none()
            && !is_builtin_type_name(name)
            && resolve_dotted(scope, name).is_none()
    };
    // Follow alias chains, through aliases in scope and, for an unresolved
    // name, the later declaration: `typealias A = B` then a local of type
    // `A`, then `typealias B = Int`, reaches the later `B`.
    let (_, names) = type_closure(ty, &|name| match scope.get_type_alias(name) {
        Some(alias) => Some(alias.clone()),
        None if unresolved(name) => later(name).cloned(),
        None => None,
    });
    names
        .iter()
        .any(|name| unresolved(name) && later(name).is_some())
}

/// The bindings of the locals written in the same body as the entry at
/// `entry_index` (entries sharing its captured scope, per `owner`).
fn same_body_local_bindings(
    entries: &[Entry],
    entry_index: usize,
    owner: &impl Fn(usize) -> Option<*const CapturedScope>,
    local_bindings: &FxHashMap<usize, std::result::Result<Value, String>>,
) -> Vec<(String, std::result::Result<Value, String>)> {
    let entry_owner = owner(entry_index);
    entries
        .iter()
        .enumerate()
        .filter(|(index, _)| owner(*index) == entry_owner)
        .filter_map(|(index, entry)| match entry {
            Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {
                local_bindings
                    .get(&index)
                    .map(|binding| (prop.name.clone(), binding.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The names of a body's members, including properties its `for`/`when`
/// generators produce.
fn body_member_names(entries: &[Entry]) -> FxHashSet<String> {
    fn generated(entries: &[Entry], names: &mut FxHashSet<String>) {
        for entry in entries {
            match entry {
                Entry::ForGenerator(fgen) => collect(&fgen.body, names),
                Entry::WhenGenerator(wgen) => {
                    collect(&wgen.body, names);
                    if let Some(else_body) = &wgen.else_body {
                        collect(else_body, names);
                    }
                }
                _ => {}
            }
        }
    }
    fn collect(entries: &[Entry], names: &mut FxHashSet<String>) {
        for entry in entries {
            if let Entry::Property(prop) = entry
                && !has_modifier(&prop.modifiers, Modifier::Local)
            {
                names.insert(prop.name.clone());
            }
        }
        generated(entries, names);
    }
    let mut names = entries
        .iter()
        .filter_map(entry_member_name)
        .cloned()
        .collect::<FxHashSet<_>>();
    generated(entries, &mut names);
    names
}

/// Whether a constraint of `ty`, or of a type alias it names (following alias
/// chains), can read a member of the body declaring a typed local: a name in
/// `members`, or anything through `module`, `outer` or `super`. Such a local
/// is checked against the finished body, since a member may not be bound yet
/// when the local is, or may replace an outer binding of the same name.
fn constraint_reads_members(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    members: &FxHashSet<String>,
) -> bool {
    fn visit(
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        members: &FxHashSet<String>,
        resolving: &mut Vec<String>,
    ) -> bool {
        use crate::parser::TypeExpr;
        let alias_reads = |name: &str, resolving: &mut Vec<String>| {
            let name = name
                .trim_start_matches('*')
                .trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(name);
            let Some(alias) = scope.get_type_alias(name) else {
                return false;
            };
            if resolving.iter().any(|seen| seen == name) {
                return false;
            }
            resolving.push(name.to_string());
            let reads = visit(alias, scope, members, resolving);
            resolving.pop();
            reads
        };
        match ty {
            TypeExpr::Named(name) => alias_reads(name, resolving),
            TypeExpr::Nullable(inner) => visit(inner, scope, members, resolving),
            TypeExpr::Union(variants) | TypeExpr::Generic(_, variants) => variants
                .iter()
                .any(|variant| visit(variant, scope, members, resolving)),
            TypeExpr::Constrained(base, constraint) => {
                let mut refs = HashSet::new();
                collect_expr_refs(constraint, &mut refs, &HashSet::new());
                refs.iter().any(|name| {
                    members.contains(name) || matches!(name.as_str(), "module" | "outer" | "super")
                }) || alias_reads(base, resolving)
            }
        }
    }
    visit(ty, scope, members, &mut Vec::new())
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
        Expr::String(s) => Ok(Value::String(s.clone())),
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
    let mut map = IndexMap::new();
    map.insert("_type".to_string(), Value::String("regex".to_string()));
    map.insert("pattern".to_string(), pattern);
    Value::Object(Arc::new(map), None)
}

#[cfg(feature = "native-io")]
impl Default for Evaluator {
    fn default() -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::new(),
            import_cache: HashMap::new(),
            imports_in_flight: HashSet::new(),
            placeholder_reads: 0,
            narrowed_import_cache: HashMap::new(),
            parse_cache: HashMap::new(),
            module_scopes: HashMap::new(),
            env_reads: BTreeMap::new(),
            scoped_imports_in_flight: HashSet::new(),
            #[cfg(feature = "blocking")]
            capabilities: Box::new(crate::capabilities::BlockingCapabilities::new()),
            #[cfg(not(feature = "blocking"))]
            capabilities: Box::new(crate::capabilities::NativeCapabilities::new()),
            bridge: None,
            local_capabilities: None,
            #[cfg(feature = "package-zip-core")]
            package_dirs: HashMap::new(),
            package_cache_dir: None,
            package_http_roots: HashSet::new(),
            offline: false,
            http_rewrites: Vec::new(),
            converters: Vec::new(),
            warned_deprecated: std::collections::HashSet::new(),
            retry_passes: 0,
        }
    }
}

/// Synchronous access to the host capabilities. Each call blocks on the
/// capability's future, or, while the evaluator runs on a worker thread for
/// the async API, waits for the async caller to serve it.
impl Evaluator {
    /// Run a capability call other than an HTTP fetch, using the
    /// capabilities' blocking counterpart while one is installed.
    fn io<T, F>(&mut self, call: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(
                &'c mut dyn EvalCapabilities,
            ) -> crate::capabilities::BoxFuture<'c, Result<T>>
            + Send
            + 'static,
    {
        match self.local_capabilities.as_deref_mut() {
            Some(local) => pollster::block_on(call(local)),
            None => self.http_io(call),
        }
    }

    /// Run a capability call with the host capabilities themselves.
    fn http_io<T, F>(&mut self, call: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(
                &'c mut dyn EvalCapabilities,
            ) -> crate::capabilities::BoxFuture<'c, Result<T>>
            + Send
            + 'static,
    {
        match &self.bridge {
            Some(bridge) => bridge.call(call),
            None => pollster::block_on(call(&mut *self.capabilities)),
        }
    }

    fn read_to_string_io(&mut self, path: &Path) -> Result<String> {
        let path = path.to_path_buf();
        self.io(move |c| Box::pin(async move { c.read_to_string(&path).await }))
    }

    fn path_exists_io(&mut self, path: &Path) -> Result<bool> {
        let path = path.to_path_buf();
        self.io(move |c| Box::pin(async move { c.path_exists(&path).await }))
    }

    fn canonicalize_io(&mut self, path: &Path) -> Result<PathBuf> {
        let path = path.to_path_buf();
        self.io(move |c| Box::pin(async move { c.canonicalize(&path).await }))
    }

    fn read_bytes_io(&mut self, path: &Path) -> Result<Vec<u8>> {
        let path = path.to_path_buf();
        self.io(move |c| Box::pin(async move { c.read_bytes(&path).await }))
    }

    fn create_dir_all_io(&mut self, path: &Path) -> Result<()> {
        let path = path.to_path_buf();
        self.io(move |c| Box::pin(async move { c.create_dir_all(&path).await }))
    }

    fn write_atomic_io(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        let path = path.to_path_buf();
        let bytes = bytes.to_vec();
        self.io(move |c| Box::pin(async move { c.write_atomic(&path, &bytes).await }))
    }

    fn remove_file_io(&mut self, path: &Path) -> Result<()> {
        let path = path.to_path_buf();
        self.io(move |c| Box::pin(async move { c.remove_file(&path).await }))
    }

    #[cfg(feature = "package-zip-core")]
    fn extract_zip_io(&mut self, bytes: Vec<u8>, destination: &Path) -> Result<()> {
        let destination = destination.to_path_buf();
        self.io(move |c| Box::pin(async move { c.extract_zip(bytes, &destination).await }))
    }

    fn read_env_io(&mut self, name: &str) -> Result<Option<String>> {
        let name = name.to_string();
        self.io(move |c| Box::pin(async move { c.read_env(&name).await }))
    }

    fn fetch_text_io(&mut self, url: &str) -> Result<String> {
        let url = url.to_string();
        self.http_io(move |c| Box::pin(async move { c.fetch_text(&url).await }))
    }

    fn fetch_bytes_io(&mut self, url: &str) -> Result<Vec<u8>> {
        let url = url.to_string();
        self.http_io(move |c| Box::pin(async move { c.fetch_bytes(&url).await }))
    }

    #[cfg(feature = "package-zip-core")]
    fn temp_dir_io(&mut self, prefix: &str) -> Result<PathBuf> {
        let prefix = prefix.to_string();
        self.io(move |c| Box::pin(async move { c.temp_dir(&prefix).await }))
    }

    fn glob_io(&mut self, base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
        let base = base.to_path_buf();
        let pattern = pattern.to_string();
        self.io(move |c| Box::pin(async move { c.glob(&base, &pattern).await }))
    }

    /// Run the synchronous `work` for an async caller.
    ///
    /// On a multi-threaded tokio runtime, with capabilities that have a
    /// blocking counterpart, it runs in place under `block_in_place`, with HTTP
    /// fetches blocking on the host capabilities while the runtime's other
    /// workers drive them. Anywhere else it runs on a worker thread (see
    /// [`Evaluator::run_on_worker`]).
    async fn run_async<A, R>(&mut self, arg: A, work: fn(&mut Evaluator, A) -> R) -> R
    where
        A: Send + 'static,
        R: Send + 'static,
    {
        // Only capabilities with a blocking counterpart run in place: their
        // remaining calls are HTTP fetches the runtime drives by itself, while
        // other capabilities may wait on tasks that blocking here would stall
        // (for example a sibling in the caller's `join!`).
        #[cfg(feature = "async")]
        if let Ok(handle) = tokio::runtime::Handle::try_current()
            && handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
            && let Some(local) = self.capabilities.blocking_capabilities()
        {
            self.local_capabilities = Some(local);
            let result = tokio::task::block_in_place(|| work(self, arg));
            self.local_capabilities = None;
            return result;
        }
        self.run_on_worker(arg, work).await
    }

    /// Run `work` on a worker thread with HTTP fetches, and any capability
    /// calls without a blocking counterpart, served by this task, so async
    /// capabilities run on the caller's executor.
    ///
    /// If the returned future is dropped before it finishes, the worker stops
    /// at its next expression, and the evaluator keeps its capabilities,
    /// configuration and extracted package directories. It loses its other
    /// caches; copying them up front would cost every call for the sake of a
    /// rare cancellation.
    async fn run_on_worker<A, R>(&mut self, arg: A, work: fn(&mut Evaluator, A) -> R) -> R
    where
        A: Send + 'static,
        R: Send + 'static,
    {
        struct Restore<'a> {
            evaluator: &'a mut Evaluator,
            capabilities: Option<Box<dyn EvalCapabilities>>,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                if let Some(capabilities) = self.capabilities.take() {
                    self.evaluator.capabilities = capabilities;
                }
            }
        }

        let local_capabilities = self.capabilities.blocking_capabilities();
        let capabilities = std::mem::replace(&mut self.capabilities, Box::new(bridge::Detached));
        let placeholder = self.detached_copy();
        let evaluator = std::mem::replace(self, placeholder);
        let mut restore = Restore {
            evaluator: self,
            capabilities: Some(capabilities),
        };
        let capabilities = restore
            .capabilities
            .as_deref_mut()
            .expect("capabilities present");
        let (evaluator, result) =
            bridge::run(evaluator, capabilities, move |mut evaluator, bridge| {
                evaluator.bridge = Some(bridge);
                evaluator.local_capabilities = local_capabilities;
                let result = work(&mut evaluator, arg);
                evaluator.bridge = None;
                evaluator.local_capabilities = None;
                (evaluator, result)
            })
            .await;
        *restore.evaluator = evaluator;
        result
    }

    /// Fail if the async caller this evaluation runs for has gone away.
    #[inline]
    fn check_cancelled(&self) -> Result<()> {
        match &self.bridge {
            Some(bridge) if bridge.is_cancelled() => Err(bridge::cancelled()),
            _ => Ok(()),
        }
    }

    /// An evaluator with this one's configuration and no capabilities.
    fn detached_copy(&self) -> Evaluator {
        let mut copy = Evaluator::with_capabilities(bridge::Detached);
        copy.base_path = self.base_path.clone();
        copy.max_depth = self.max_depth;
        copy.package_cache_dir = self.package_cache_dir.clone();
        copy.package_http_roots = self.package_http_roots.clone();
        copy.offline = self.offline;
        copy.http_rewrites = self.http_rewrites.clone();
        #[cfg(feature = "package-zip-core")]
        {
            copy.package_dirs = self.package_dirs.clone();
        }
        copy
    }
}

impl Evaluator {
    /// Construct an evaluator with synchronous host capabilities.
    #[cfg(feature = "blocking")]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct an evaluator with asynchronous host capabilities.
    #[cfg(feature = "async")]
    pub fn new_async() -> Self {
        Self::with_capabilities(crate::capabilities::NativeCapabilities::new())
    }

    pub fn with_capabilities(capabilities: impl EvalCapabilities + 'static) -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::new(),
            import_cache: HashMap::new(),
            imports_in_flight: HashSet::new(),
            placeholder_reads: 0,
            narrowed_import_cache: HashMap::new(),
            parse_cache: HashMap::new(),
            module_scopes: HashMap::new(),
            env_reads: BTreeMap::new(),
            scoped_imports_in_flight: HashSet::new(),
            capabilities: Box::new(capabilities),
            bridge: None,
            local_capabilities: None,
            #[cfg(feature = "package-zip-core")]
            package_dirs: HashMap::new(),
            package_cache_dir: None,
            package_http_roots: HashSet::new(),
            offline: false,
            http_rewrites: Vec::new(),
            converters: Vec::new(),
            warned_deprecated: std::collections::HashSet::new(),
            retry_passes: 0,
        }
    }

    pub fn set_base_path(&mut self, path: &Path) {
        self.base_path = path.to_path_buf();
    }

    fn resolve_local_path(&self, current_path: &Path, uri: &str) -> PathBuf {
        #[cfg(feature = "package-zip-core")]
        if let Some(from_root) = uri.strip_prefix(".../")
            && let Some(root) = self
                .package_dirs
                .values()
                .find(|root| current_path.starts_with(root))
        {
            return root.join(from_root);
        }
        if let Some(from_root) = uri.strip_prefix(".../") {
            return self.base_path.join(from_root);
        }
        current_path.parent().unwrap_or(Path::new(".")).join(uri)
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
        self.module_scopes.clear();
        self.scoped_imports_in_flight.clear();
        self.converters.clear();
    }

    /// Set a custom HTTP client for fetching remote imports and packages.
    /// Use this to configure proxy settings, CA certificates, timeouts, etc.
    /// Returns an error when the installed capabilities use another HTTP backend.
    #[cfg(feature = "http")]
    pub fn set_http_client(&mut self, client: reqwest::Client) -> Result<()> {
        self.capabilities.set_http_client(client)
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

    /// Read a resource by URI scheme.
    fn read_resource(&mut self, uri: &str) -> Result<Value> {
        if let Some(path) = uri.strip_prefix("file://") {
            // file:// — read local file
            let content = self.read_to_string_io(Path::new(path))?;
            Ok(Value::String(content))
        } else if let Some(var_name) = uri.strip_prefix("env:") {
            // env: — read environment variable
            let value = self.read_env_io(var_name)?;
            self.env_reads.insert(var_name.to_string(), value.clone());
            let Some(val) = value else {
                return Err(Error::Eval(format!(
                    "environment variable not found: {var_name}"
                )));
            };
            Ok(Value::String(val))
        } else if let Some(prop_name) = uri.strip_prefix("prop:") {
            // prop: — system properties (not standard in Rust, return empty)
            Err(Error::Eval(format!(
                "system property not available: {prop_name}"
            )))
        } else if uri.starts_with("https://") || uri.starts_with("http://") {
            // HTTP/HTTPS
            let content = self.fetch_source(uri)?;
            Ok(Value::String(content))
        } else {
            // Bare path — treat as file relative to base_path
            let file_path = self.base_path.join(uri);
            let content = self.read_to_string_io(&file_path)?;
            Ok(Value::String(content))
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

    fn inherited_reference_roots(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
    ) -> Result<HashSet<String>> {
        let mut refs = HashSet::new();
        if depth > self.max_depth {
            return Ok(refs);
        }
        for uri in [module.amends.as_deref(), module.extends.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some((source, source_path)) = self.load_module_source(uri, path)?
                && let Ok(tokens) = lexer::lex_named(&source, &source_path)
                && let Ok(base_module) = parser::parse_named(&tokens, &source, &source_path)
            {
                refs.extend(referenced_roots(&base_module.body));
                refs.extend(self.inherited_reference_roots(
                    &base_module,
                    Path::new(&source_path),
                    depth + 1,
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
                    #[cfg(feature = "package-zip-core")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(entry);
                        let source = self.read_to_string_io(&local_path)?;
                        return Ok(Some((source, local_path.display().to_string())));
                    }
                    #[cfg(not(feature = "package-zip-core"))]
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
        let import_path = if let Some(rel) = uri.strip_prefix("file://") {
            PathBuf::from(rel)
        } else {
            self.resolve_local_path(path, uri)
        };
        if !self.path_exists_io(&import_path)? {
            return Ok(None);
        }
        let source = self.read_to_string_io(&import_path)?;
        Ok(Some((source, import_path.display().to_string())))
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
    pub async fn preload_package_async(
        &mut self,
        url: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<()> {
        if self.package_cache_dir.is_none() {
            return Ok(());
        }
        let arg = (url.to_string(), extension.to_string(), bytes.to_vec());
        self.run_async(arg, |evaluator, (url, extension, bytes)| {
            evaluator.preload_package(&url, &extension, &bytes)
        })
        .await
    }

    /// Seed the persistent package cache with `bytes` for `url`, blocking on
    /// host IO.
    ///
    /// See [`Evaluator::preload_package_async`].
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
    #[cfg(feature = "package-zip-core")]
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

    #[cfg(all(test, feature = "package-zip-core"))]
    fn package_dir_for_zip(&self, zip_url: &str) -> Option<&PathBuf> {
        self.package_dirs.get(zip_url)
    }

    /// Evaluate `source` as the module at `path`.
    ///
    /// Evaluation runs on a worker thread while this task serves its host IO,
    /// so async capabilities run on the caller's runtime.
    pub async fn eval_source(&mut self, source: &str, path: &Path) -> Result<Value> {
        let arg = (source.to_string(), path.to_path_buf());
        self.run_async(arg, |evaluator, (source, path)| {
            evaluator.eval_source_blocking(&source, &path)
        })
        .await
    }

    /// Evaluate `source` as the module at `path`, blocking on host IO.
    pub fn eval_source_blocking(&mut self, source: &str, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        self.eval_source_inner(source, path)
    }

    fn eval_source_inner(&mut self, source: &str, path: &Path) -> Result<Value> {
        // Seed import cache for the entry file so circular back-references work.
        // Mark it in flight too, so a narrowed import that reads this
        // placeholder is not cached.
        let canonical = self.canonicalize_io(path).ok();
        if let Some(canonical) = &canonical {
            self.import_cache.insert(
                canonical.clone(),
                Value::Object(Arc::new(IndexMap::new()), None),
            );
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

    /// Evaluate a local pkl file by path (public entry point).
    pub async fn eval_file_pub(&mut self, path: &Path) -> Result<Value> {
        self.run_async(path.to_path_buf(), |evaluator, path| {
            evaluator.eval_file_blocking(&path)
        })
        .await
    }

    /// Evaluate a local pkl file and apply its output converters in one
    /// worker run.
    #[cfg(feature = "async")]
    pub(crate) async fn eval_file_converted(&mut self, path: &Path) -> Result<Value> {
        self.run_async(path.to_path_buf(), |evaluator, path| {
            evaluator.eval_file_converted_blocking(&path)
        })
        .await
    }

    #[cfg(feature = "native-io")]
    pub(crate) fn eval_file_converted_blocking(&mut self, path: &Path) -> Result<Value> {
        let value = self.eval_file_blocking(path)?;
        self.apply_converters_blocking(value)
    }

    /// Evaluate a local pkl file by path, blocking on host IO.
    pub fn eval_file_blocking(&mut self, path: &Path) -> Result<Value> {
        self.begin_evaluation();
        let source = self.read_to_string_io(path)?;
        self.eval_source_inner(&source, path)
    }

    /// Read, lex, parse, and evaluate a local file (with caching).
    /// Inserts a placeholder before evaluation to break circular imports.
    fn eval_file(&mut self, path: &Path, depth: usize) -> Result<Value> {
        let canonical = self.canonicalize_io(path)?;
        if let Some(cached) = self.cached_import(&canonical) {
            return Ok(cached);
        }
        // Insert empty placeholder to break circular imports
        self.import_cache.insert(
            canonical.clone(),
            Value::Object(Arc::new(IndexMap::new()), None),
        );
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
            return self.eval_file(path, depth);
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
        self.import_cache.insert(
            canonical.clone(),
            Value::Object(Arc::new(IndexMap::new()), None),
        );
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
            return self.eval_file(path, depth);
        }
        let canonical = self.canonicalize_io(path)?;
        if !self.scoped_imports_in_flight.insert(canonical.clone()) {
            self.placeholder_reads += 1;
            return Ok(Value::Object(Arc::new(IndexMap::new()), None));
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

    fn inherited_late_properties(&self, path: &Path) -> Vec<Property> {
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
            return Ok(Value::Object(Arc::new(IndexMap::new()), None));
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
        let mut mapping = IndexMap::new();
        for (matched_path, rel_key) in matched {
            if requested.is_some_and(|requested| !requested.contains(&rel_key)) {
                continue;
            }
            if self.same_local_path(&matched_path, path)? {
                continue;
            }
            let val = self.eval_file_with_requested_fields(&matched_path, depth + 1, None)?;
            mapping.insert(rel_key, val);
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
            let import_path = if let Some(rel) = uri.strip_prefix("file://") {
                PathBuf::from(rel)
            } else {
                self.resolve_local_path(path, uri)
            };
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
                        receiver.set(name.clone(), value.clone());
                    }
                }
                if let Some(this) = scope.get("this") {
                    receiver.set("this".into(), this.clone());
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
                    active.set(field.to_string(), inherited);
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
                    "keys" => Value::List(keys.into_keys().map(Value::String).collect()),
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
            return Ok(Value::Object(Arc::new(IndexMap::new()), None));
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
        keys: &mut IndexMap<String, ()>,
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
                    keys.insert(prop.name.clone(), ());
                }
                Entry::DynProperty(key, _) => {
                    let key = self.eval_expr(key, &scope, depth + 1)?;
                    keys.insert(value_to_key(&key)?, ());
                }
                Entry::Spread(expr) => {
                    if let Value::Object(map, _) = self.eval_expr(expr, &scope, depth + 1)? {
                        keys.extend(map.keys().map(|key| (key.clone(), ())));
                    }
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection) {
                        let mut iter = scope.child();
                        iter.set(generator.val_var.clone(), value);
                        if let Some(name) = &generator.key_var {
                            iter.set(name.clone(), key);
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
                    *length += match self.eval_expr(expr, &scope, depth + 1)? {
                        Value::List(items) => items.len(),
                        Value::Object(items, _) => items.len(),
                        _ => 1,
                    };
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection) {
                        let mut iter = scope.child();
                        iter.set(generator.val_var.clone(), value);
                        if let Some(name) = &generator.key_var {
                            iter.set(name.clone(), key);
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
            let requested = HashSet::from([field.to_string()]);
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
        // Typed locals that failed their check against the finished module
        // are poisoned in another pass, so only an actual read fails. Each
        // pass poisons at least one more local, so this terminates.
        let mut poisoned_locals = FxHashMap::default();
        loop {
            let retrying = !poisoned_locals.is_empty();
            if retrying {
                self.retry_passes += 1;
            }
            let outcome = self.eval_module_pass(
                module,
                path,
                depth,
                inherited_scope.as_ref(),
                requested_fields.as_ref(),
                &poisoned_locals,
            );
            if retrying {
                self.retry_passes -= 1;
            }
            match outcome? {
                BodyOutcome::Done(value) => return Ok(value),
                BodyOutcome::PoisonAndRetry(failures) => poisoned_locals.extend(failures),
            }
        }
    }

    fn eval_module_pass(
        &mut self,
        module: &Module,
        path: &Path,
        depth: usize,
        inherited_scope: Option<&Scope>,
        requested_fields: Option<&HashSet<String>>,
        poisoned_locals: &FxHashMap<String, String>,
    ) -> Result<BodyOutcome<Value>> {
        if depth > self.max_depth {
            return Err(Error::Eval(format!(
                "max import depth {} exceeded",
                self.max_depth
            )));
        }
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
        let requested_output_fields =
            requested_fields.map(|fields| expand_requested_fields(&module.body, fields));
        let analysis_entries =
            analysis_entries_for_requested_fields(&module.body, requested_output_fields.as_ref());
        let mut referenced_imports = referenced_roots(&analysis_entries);
        referenced_imports.extend(self.inherited_reference_roots(module, path, depth + 1)?);
        let import_field_uses = import_field_uses(&analysis_entries);

        let inherited_local_paths: Vec<_> = module
            .amends
            .iter()
            .chain(module.extends.iter())
            .filter_map(|uri| local_module_path(path, uri))
            .collect();
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
                scope.declare(alias.clone(), imported_val);
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
                    #[cfg(feature = "package-zip-core")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(file_path);
                        let imported_val = self.eval_file_with_requested_fields(
                            &local_path,
                            depth + 1,
                            requested,
                        )?;
                        let identity = self.module_type_namespace(&local_path);
                        scope.declare(alias.clone(), imported_val);
                        scope.set_module_identity(alias, identity);
                        continue;
                    }
                    #[cfg(not(feature = "package-zip-core"))]
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
                scope.declare(alias.clone(), imported_val);
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

            let import_path = if let Some(rel) = uri.strip_prefix("file://") {
                PathBuf::from(rel)
            } else {
                self.resolve_local_path(path, uri)
            };
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
                scope.declare(alias.clone(), imported_val);
                scope.set_module_identity(alias, identity);
            }
        }

        // Process amends: load base module as starting values
        let mut base_obj = IndexMap::new();
        let mut late_inherited_properties = Vec::new();
        if let Some(amends_uri) = &module.amends {
            // A relative amends inside a remote module resolves against that URL.
            let resolved_amends = resolve_remote_relative(path, amends_uri);
            let uri: &str = resolved_amends.as_deref().unwrap_or(amends_uri);
            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP amends
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
                    #[cfg(feature = "package-zip-core")]
                    {
                        let pkg_dir = self.extract_package_zip(zip_url)?;
                        let local_path = pkg_dir.join(entry);
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
                    #[cfg(not(feature = "package-zip-core"))]
                    {
                        let _ = entry;
                        return Err(Error::Unsupported(format!(
                            "package zip imports require pklr's 'package-zip' feature: {zip_url}"
                        )));
                    }
                } else if let PackageSource::Direct { url, root } = &pkg {
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
                let amends_path = if let Some(rel) = uri.strip_prefix("file://") {
                    PathBuf::from(rel)
                } else {
                    self.resolve_local_path(path, uri)
                };
                if self.path_exists_io(&amends_path)? {
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
            let base_source = self.load_module_source(uri, path)?;
            if let Some((src, source_path)) = base_source
                && let Ok(tokens) = lexer::lex_named(&src, &source_path)
                && let Ok(base_module) = parser::parse_named(&tokens, &src, &source_path)
            {
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
                        scope.set(name.clone(), defaults);
                        // Remove inherited class definitions from base output —
                        // they were included at depth > 0 for dotted access but
                        // should not appear in the amending module's data output.
                        base_obj.shift_remove(name);
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
                let extends_path = if let Some(rel) = uri.strip_prefix("file://") {
                    PathBuf::from(rel)
                } else {
                    self.resolve_local_path(path, uri)
                };
                if self.path_exists_io(&extends_path)? {
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
                                scope.set(cls_name.clone(), defaults);
                                base_obj.shift_remove(cls_name);
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
                                    scope.set(prop.name.clone(), value);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            } else if uri.starts_with("https://") || uri.starts_with("http://") {
                let source = self.fetch_source(uri)?;
                let tokens = lexer::lex_named(&source, uri)?;
                let ext_module = parser::parse_named(&tokens, &source, uri)?;
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
                        scope.set(cls_name.clone(), defaults);
                        base_obj.shift_remove(cls_name);
                    }
                    if let Entry::Property(prop) = entry
                        && !has_modifier(&prop.modifiers, Modifier::Local)
                        && prop.name != "output"
                    {
                        late_inherited_properties.push(prop.clone());
                        if let Ok(Some(value)) = self.eval_property(prop, &scope, depth) {
                            scope.set(prop.name.clone(), value);
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
            expand_requested_fields(&dependency_entries, fields)
        });

        // Locals are evaluated before the main property pass, but `this` and
        // `module` must already expose inherited members at that point.
        let inherited_snapshot = Value::Object(Arc::new(base_obj.clone()), None);
        scope.set("this".into(), inherited_snapshot.clone());
        scope.set("module".into(), inherited_snapshot);

        // Typed locals whose check waits for the module to be complete: their
        // constraint reads a module member, or their type is an alias declared
        // later (type aliases are registered in declaration order).
        // Built only when the module has a typed local.
        let mut module_members: Option<FxHashSet<String>> = None;
        let mut deferred_locals = Vec::new();

        // First pass: collect locals, class definitions, and type aliases in
        // declaration order so they can reference each other
        for (entry_index, entry) in module.body.iter().enumerate() {
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    if let Some(message) = poisoned_locals.get(&prop.name) {
                        // Failed its check in an earlier pass.
                        scope.declare_poisoned(prop.name.clone(), message.clone());
                        continue;
                    }
                    match self.eval_expr(prop.value.as_ref().unwrap(), &scope, depth) {
                        Ok(val) => match &prop.type_ann {
                            Some(ty)
                                if names_later_alias(ty, &scope, &module.body, entry_index)
                                    || constraint_reads_members(
                                        ty,
                                        &scope,
                                        module_members.get_or_insert_with(|| {
                                            let mut members = body_member_names(&module.body);
                                            members.extend(
                                                late_inherited_properties
                                                    .iter()
                                                    .map(|prop| prop.name.clone()),
                                            );
                                            members
                                        }),
                                    ) =>
                            {
                                deferred_locals.push(prop);
                                scope.declare(prop.name.clone(), val);
                            }
                            Some(_) => match self.typed_local_failure(prop, &val, &scope, depth)? {
                                LocalCheck::Failed(message) => {
                                    scope.declare_poisoned(prop.name.clone(), message)
                                }
                                LocalCheck::Passed => scope.declare(prop.name.clone(), val),
                                LocalCheck::Unresolved => {
                                    deferred_locals.push(prop);
                                    scope.declare(prop.name.clone(), val);
                                }
                            },
                            None => scope.declare(prop.name.clone(), val),
                        },
                        Err(Error::Eval(message)) => {
                            scope.declare_poisoned(prop.name.clone(), message)
                        }
                        Err(error) => return Err(error),
                    }
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self.eval_class_def(
                        name,
                        class_mods,
                        parent.as_deref(),
                        body,
                        &scope,
                        depth,
                    )?;
                    scope.declare(name.clone(), defaults);
                }
                Entry::TypeAlias(name, ty) => {
                    self.eval_type_alias(name, ty, &mut scope);
                }
                _ => {}
            }
        }

        // Export class definitions so they're accessible via dotted paths
        // (e.g., `import "helpers.pkl"` → `helpers.ClassName`).
        // Track class names to exclude from serialized output.
        let mut class_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in module.body.iter() {
            if let Entry::ClassDef(name, ..) = entry
                && let Some(cls_val) = scope.get(name)
            {
                base_obj.insert(name.clone(), cls_val.clone());
                class_names.insert(name.clone());
            }
        }

        // Second pass: evaluate non-local entries into output object
        let mut out = base_obj;
        // all_props includes hidden properties — used for `this`/`module`
        // snapshots. It is shared with those snapshots and grown in place, as
        // in `eval_entries_with_lexical_scopes`, rather than copied per property.
        let mut all_props = Arc::new(out.clone());
        // Seed scope with base properties so body amendments can find them
        // (e.g., `hooks { ... }` needs to find the base hooks Mapping in scope
        // to properly amend it with type-aware merging).
        for (k, v) in &out {
            scope.set(k.clone(), v.clone());
        }
        // Bind `this` at module level so properties can reference the module object
        scope.set("this".into(), Value::Object(Arc::clone(&all_props), None));
        // Also bind `module` to the same value
        scope.set("module".into(), Value::Object(Arc::clone(&all_props), None));
        for entry in module.body.iter() {
            if let Entry::Property(prop) = entry {
                let mods = &prop.modifiers;
                if has_modifier(mods, Modifier::Local) {
                    continue; // already collected
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
                    if let Some(v) = out.get(&prop.name) {
                        // Satisfied by base — add to scope so other properties can reference it
                        scope.set(prop.name.clone(), v.clone());
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
                    Err(error) => return Err(error),
                };
                if let Some(v) = val {
                    // const/fixed: error if overriding an immutable property from base
                    if (has_modifier(mods, Modifier::Const) || has_modifier(mods, Modifier::Fixed))
                        && out.contains_key(&prop.name)
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
                    scope.declare(prop.name.clone(), v.clone());
                    // Track in all_props (including hidden) for `this`/`module`
                    module_props_insert(&mut scope, &mut all_props, prop.name.clone(), v.clone());
                    if !has_modifier(mods, Modifier::Hidden)
                        && (depth > 0 || should_render_property_value(prop, &v))
                        && requested_output_fields
                            .as_ref()
                            .is_none_or(|fields| fields.contains(&prop.name))
                    {
                        out.insert(prop.name.clone(), v);
                    }
                    // Update `this` and `module` with all properties (including hidden)
                    let snapshot = Value::Object(Arc::clone(&all_props), None);
                    scope.set("this".into(), snapshot.clone());
                    scope.set("module".into(), snapshot);
                }
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
                        scope.set(prop.name.clone(), value.clone());
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
                            out.insert(prop.name.clone(), value);
                        }
                        let snapshot = Value::Object(Arc::clone(&all_props), None);
                        scope.set("this".into(), snapshot.clone());
                        scope.set("module".into(), snapshot);
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
                        scope.set(prop.name.clone(), value.clone());
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
                            out.insert(prop.name.clone(), value);
                        }
                        let snapshot = Value::Object(Arc::clone(&all_props), None);
                        scope.set("this".into(), snapshot.clone());
                        scope.set("module".into(), snapshot);
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

        // Check typed locals whose constraint reads module members against
        // the finished module. One that fails is poisoned in another pass.
        let mut failed_locals = Vec::new();
        for prop in deferred_locals {
            let Some(value) = scope.get(&prop.name).cloned() else {
                continue;
            };
            if let LocalCheck::Failed(message) =
                self.typed_local_failure(prop, &value, &scope, depth)?
            {
                failed_locals.push((prop.name.clone(), message));
            }
        }
        if !failed_locals.is_empty() {
            return Ok(BodyOutcome::PoisonAndRetry(failed_locals));
        }

        // Check declared types once every property is bound, so a constraint
        // can read properties declared after the one it checks.
        for entry in module.body.iter() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if prop.name == "output"
                || prop.value.is_none()
                || prop.type_ann.is_none()
                || has_modifier(&prop.modifiers, Modifier::Local)
            {
                continue;
            }
            let Some(value) = all_props.get(&prop.name).cloned() else {
                continue;
            };
            self.check_declared_property_type(prop, &value, &scope, depth)?;
        }

        if depth == 0 {
            for prop in &late_inherited_properties {
                if let Some(value) = all_props.get(&prop.name)
                    && !should_render_property_value(prop, value)
                    && !child_property_names.contains(prop.name.as_str())
                {
                    out.shift_remove(&prop.name);
                }
            }
        }

        // At the top level (depth 0), strip class definitions and lambdas from
        // the serialized output — they're schema/functions, not data.
        // Imported modules (depth > 0) keep them so dotted access works
        // (e.g., `helpers.ClassName`).
        if depth == 0 {
            for name in &class_names {
                out.shift_remove(name);
            }
            out.retain(|_, v| !matches!(v, Value::Lambda(..)));
        }
        if let Some(fields) = &requested_output_fields {
            out.retain(|name, _| fields.contains(name));
        }
        // If the module declares any `@Deprecated` properties, attach a
        // minimal ObjectSource carrying just the deprecation map so field
        // access can warn lazily. Modules without @Deprecated keep `None`
        // source to avoid changing amend behavior in the common case.
        let deprecated = collect_deprecated(&module.body);
        let source = if deprecated.is_empty() {
            None
        } else {
            Some(Arc::new(ObjectSource {
                entries: Vec::new().into(),
                scope: ScopeMap::default(),
                scope_declared: NameSet::default(),
                body_members: HashSet::new(),
                is_open: true,
                type_name: None,
                type_identity: None,
                parent_type_names: Vec::new(),
                parent_type_identities: Vec::new(),
                scope_module_identities: IndexMap::new(),
                scope_type_aliases: IndexMap::new(),
                entry_scopes: Vec::new(),
                evaluated_properties: Vec::new(),
                mapping_value_types: Vec::new(),
                deprecated,
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
        Ok(BodyOutcome::Done(Value::Object(Arc::new(out), source)))
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
                    _ => Vec::new(),
                };
                let mut amendment_scope = scope.child();
                amendment_scope.set("super".into(), Value::List(items.clone()));
                amendment_scope.receiver_entries = Some(body.clone());
                amendment_scope.receiver_list_base = Some(items.len());
                self.eval_listing_entries(body, &amendment_scope, depth, &mut items)?;
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
                        mapping_amendment_scopes(&src.scope, &src.scope_declared, scope);
                    amendment_scope.set(
                        "super".into(),
                        Value::Object(Arc::clone(existing_map), Some(Arc::clone(src))),
                    );
                    let mut receiver_entries = existing_map
                        .keys()
                        .map(|key| Entry::DynProperty(Expr::String(key.clone()), Expr::Null))
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
                    let mut amended = IndexMap::new();
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
            body_scope.set(
                "super".into(),
                Value::Object(Arc::new(IndexMap::new()), None),
            );
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
        self.eval_entries_checked(entries, scope, depth, TypeChecks::All)
    }

    /// Evaluate an object body. The entries `checks` selects have their
    /// declared types checked once the whole body is evaluated.
    fn eval_entries_checked(
        &mut self,
        entries: &Body,
        scope: &Scope,
        depth: usize,
        checks: TypeChecks,
    ) -> Result<Value> {
        let mut receiver_scope = scope.clone();
        receiver_scope.receiver_entries = Some(entries.clone());
        receiver_scope.receiver_list_base = None;
        self.eval_entries_with_lexical_scopes(entries, &receiver_scope, depth, None, None, checks)
    }

    fn eval_entries_with_lexical_scopes(
        &mut self,
        entries: &Body,
        scope: &Scope,
        depth: usize,
        entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
        inherited_source: Option<&ObjectSource>,
        checks: TypeChecks,
    ) -> Result<Value> {
        // An object body checks what its `for`/`when` bodies hand up itself,
        // so nothing is left in this list when it returns.
        let mut pending = Vec::new();
        self.eval_entries_pending(
            entries,
            scope,
            depth,
            entry_scopes,
            inherited_source,
            checks,
            &mut pending,
        )
    }

    /// Evaluate a body, adding the checks a `for`/`when` body hands up to
    /// its enclosing object to `pending`.
    #[allow(clippy::too_many_arguments)]
    fn eval_entries_pending(
        &mut self,
        entries: &Body,
        scope: &Scope,
        depth: usize,
        entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
        inherited_source: Option<&ObjectSource>,
        checks: TypeChecks,
        pending: &mut Vec<PendingTypeCheck>,
    ) -> Result<Value> {
        // Typed locals that failed their check against the finished body are
        // poisoned in another pass, so only an actual read fails. Each pass
        // poisons at least one more local, so this terminates.
        let mut poisoned_locals = FxHashMap::default();
        loop {
            let generator_mark = pending.len();
            let retrying = !poisoned_locals.is_empty();
            if retrying {
                self.retry_passes += 1;
            }
            let result = self.eval_body_entries(
                entries,
                scope,
                depth,
                entry_scopes,
                inherited_source,
                checks.clone(),
                &poisoned_locals,
                pending,
            );
            if retrying {
                self.retry_passes -= 1;
            }
            match result {
                Ok(BodyOutcome::Done(value)) => return Ok(value),
                Ok(BodyOutcome::PoisonAndRetry(failures)) => {
                    pending.truncate(generator_mark);
                    poisoned_locals.extend(failures);
                }
                Err(error) => {
                    // A caller may recover from the error (a failed local is
                    // poisoned); it must not inherit this body's pending checks.
                    pending.truncate(generator_mark);
                    return Err(error);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_body_entries(
        &mut self,
        entries: &Body,
        scope: &Scope,
        depth: usize,
        entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
        inherited_source: Option<&ObjectSource>,
        checks: TypeChecks,
        poisoned_locals: &FxHashMap<String, String>,
        pending: &mut Vec<PendingTypeCheck>,
    ) -> Result<BodyOutcome<Value>> {
        let generator_mark = pending.len();
        // Typed locals whose constraint reads a member of this body, with
        // their entry index: checked once the body is complete.
        let mut deferred_locals: Vec<DeferredLocal> = Vec::new();
        // Whether this body has anything to type-check: a typed entry, or a
        // generator whose body may hold one. Bodies without (most of them)
        // skip the bookkeeping below.
        let tracks_types = entries.iter().any(|entry| match entry {
            Entry::Property(prop) => prop.type_ann.is_some(),
            Entry::ForGenerator(_) | Entry::WhenGenerator(_) => true,
            _ => false,
        });
        // Each local's binding (value or error) by entry index. The body's
        // scope can later rebind the name to a member of the same name, but
        // deferred checks resolve a body's own locals lexically.
        let mut local_bindings: FxHashMap<usize, std::result::Result<Value, String>> =
            FxHashMap::default();
        // The body's member names, built when a check first needs them.
        let mut body_members: Option<FxHashSet<String>> = None;
        // What a generator body declares for itself rather than as object
        // members belongs to the iteration, and so does a nested generator's.
        // An object's own declarations are its members.
        let generator_names = if checks.is_generator() && tracks_types {
            entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {
                        Some(prop.name.clone())
                    }
                    Entry::ClassDef(name, ..) | Entry::TypeAlias(name, _) => Some(name.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
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
                    && let Some(value) = source.scope.get(prop.name.as_str())
                {
                    child_scope.set(prop.name.clone(), value.clone());
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
                let refs = referenced_roots(entries);
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
            let outer_obj = Value::Object(
                Arc::new(
                    outer_map
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v))
                        .collect(),
                ),
                None,
            );
            child_scope.set("outer".into(), outer_obj);
        }
        // Class-as-a-function definitions commonly use `local self = this` so
        // output properties can close over the amended instance. Bind `this`
        // before locals are evaluated, then keep direct aliases synchronized as
        // properties populate the instance.
        let mut all_props: Arc<IndexMap<String, Value>> = Arc::default();
        let mut this_aliases = Vec::new();
        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
        // First pass: collect locals, class definitions, and type aliases in
        // declaration order so they can reference each other correctly.
        // Non-lambda locals are evaluated eagerly; lambda locals are deferred
        // to a second pass so they capture the fully-populated scope.
        let mut deferred_lambdas: Vec<(String, &crate::parser::Expr, usize)> = Vec::new();
        // Unlike a module's, a type alias declared in an object body is
        // visible only to entries after it (as narrowed-import analysis
        // assumes), so it is registered in declaration order below.
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
                    let mut type_failed = false;
                    // Typed locals are checked lazily (a failure poisons the
                    // binding) in every body, whichever property entries it
                    // checks, so an unread local never fails. A class
                    // definition (`Nothing`) binds them unchecked: its
                    // defaults may read a local that an instance overrides
                    // the reader of, and each instance re-evaluates and
                    // checks the class's locals.
                    let checked = prop.type_ann.is_some() && !matches!(checks, TypeChecks::Nothing);
                    let evaluated = if checked {
                        let (evaluated, failed) = self.bind_typed_local(
                            prop,
                            entries,
                            entry_index,
                            &active_scope,
                            depth,
                            poisoned_locals,
                            &mut body_members,
                            &mut deferred_locals,
                        )?;
                        type_failed = failed;
                        evaluated
                    } else {
                        match self.eval_expr(expr, &active_scope, depth) {
                            Ok(val) => Ok(val),
                            Err(Error::Eval(message)) => Err(message),
                            Err(error) => return Err(error),
                        }
                    };
                    // Release the entry scope before binding, as for properties.
                    drop(active_scope);
                    if tracks_types {
                        local_bindings.insert(entry_index, evaluated.clone());
                    }
                    match evaluated {
                        Ok(val) => {
                            if binds_declared(&prop.name) {
                                child_scope.declare(prop.name.clone(), val);
                            } else {
                                child_scope.set(prop.name.clone(), val);
                            }
                            if matches!(expr, Expr::Ident(name) if name == "this" || this_aliases.contains(name))
                            {
                                this_aliases.push(prop.name.clone());
                                child_scope.mark_this_alias(&prop.name);
                            }
                        }
                        Err(message) if binds_declared(&prop.name) => {
                            child_scope.declare_poisoned(prop.name.clone(), message)
                        }
                        Err(message) => child_scope.poison(prop.name.clone(), message),
                    }
                    if !type_failed && matches!(expr, crate::parser::Expr::Lambda(..)) {
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
                        child_scope.declare(name.clone(), defaults);
                    } else {
                        child_scope.set(name.clone(), defaults);
                    }
                }
                Entry::TypeAlias(name, ty) => {
                    let mut resolved_scope = active_scope;
                    self.eval_type_alias(name, ty, &mut resolved_scope);
                    child_scope.set_type_alias(name.clone(), ty.clone());
                    if let Some(value) = resolved_scope.vars.get(name.as_str()) {
                        child_scope.set(name.clone(), value.clone());
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
                active_scope.set("default".into(), template.clone());
            }
            default_template = self.eval_property(prop, &active_scope, depth)?;
        }

        let mut map: IndexMap<String, Value> = IndexMap::new();
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
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let value = self.eval_property(prop, &active_scope, depth)?;
                    // Release the entry scope first: it may share the object
                    // scope's bindings, which binding the value would then copy.
                    drop(active_scope);
                    if let Some(v) = value {
                        if binds_declared(&prop.name) {
                            child_scope.declare(prop.name.clone(), v.clone());
                        } else {
                            child_scope.set(prop.name.clone(), v.clone());
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
                            map.insert(prop.name.clone(), v);
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
                                    scope: ScopeMap::default(),
                                    scope_declared: NameSet::default(),
                                    body_members: HashSet::new(),
                                    is_open: true,
                                    type_name: Some(tn.clone()),
                                    type_identity: src.type_identity.clone(),
                                    parent_type_names: src.parent_type_names.clone(),
                                    parent_type_identities: src.parent_type_identities.clone(),
                                    scope_module_identities: IndexMap::new(),
                                    scope_type_aliases: IndexMap::new(),
                                    entry_scopes: Vec::new(),
                                    evaluated_properties: Vec::new(),
                                    mapping_value_types: Vec::new(),
                                    deprecated: merge_deprecated(&src.deprecated, body),
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
                    let items = collection_to_items(collection);
                    let body_typed = generator_body_has_typed_entries(&fgen.body);
                    for (k, v) in items {
                        let mut iter_scope = active_scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(key_var) = &fgen.key_var {
                            iter_scope.set(key_var.clone(), k);
                        }
                        let body_checks = checks.for_generator(entry_index, body_typed, || {
                            let mut names = generator_names.clone();
                            names.push(fgen.val_var.clone());
                            names.extend(fgen.key_var.iter().cloned());
                            names
                        });
                        let body_val = self.eval_entries_pending(
                            &fgen.body,
                            &iter_scope,
                            depth,
                            None,
                            None,
                            body_checks,
                            pending,
                        )?;
                        if let Value::Object(m, _) = body_val {
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
                        let body_val = self.eval_entries_pending(
                            &wgen.body,
                            &active_scope,
                            depth,
                            None,
                            None,
                            checks.for_generator(
                                entry_index,
                                generator_body_has_typed_entries(&wgen.body),
                                || generator_names.clone(),
                            ),
                            pending,
                        )?;
                        if let Value::Object(m, _) = body_val {
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
                    } else if let Some(else_body) = &wgen.else_body {
                        let else_val = self.eval_entries_pending(
                            else_body,
                            &active_scope,
                            depth,
                            None,
                            None,
                            checks.for_generator(
                                entry_index,
                                generator_body_has_typed_entries(else_body),
                                || generator_names.clone(),
                            ),
                            pending,
                        )?;
                        if let Value::Object(m, _) = else_val {
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
                }
                Entry::Elem(_) => {} // bare elements only valid in Listing bodies
                Entry::ClassDef(..) | Entry::TypeAlias(..) => {} // handled in scope setup
            }
            // Checks a generator entry handed up resolve names the way that
            // entry does.
            if !checks.is_generator() {
                for check in &mut pending[generator_mark..] {
                    check.entry_index.get_or_insert(entry_index);
                }
            }
        }
        // Check typed locals whose constraint reads members of this body
        // against the finished body. One that fails is poisoned in another
        // pass, so only an actual read fails.
        if !deferred_locals.is_empty() {
            let body = FinishedBody {
                entries,
                child_scope: &child_scope,
                entry_scopes,
                entry_owners: &entry_owners,
                own_body_scope,
                local_bindings: &local_bindings,
                all_props: &all_props,
            };
            let failed_locals = self.settle_deferred_locals(&body, deferred_locals, depth)?;
            if !failed_locals.is_empty() {
                return Ok(BodyOutcome::PoisonAndRetry(failed_locals));
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
        // Check declared types once every member is bound, so a constraint
        // can read members declared after the property it checks. Each entry
        // is checked in the scope it was evaluated in, so an amendment's
        // constraints see the amendment's own locals and type aliases. A body
        // without a typed entry or generator has nothing to check.
        if tracks_types {
            let body = FinishedBody {
                entries,
                child_scope: &child_scope,
                entry_scopes,
                entry_owners: &entry_owners,
                own_body_scope,
                local_bindings: &local_bindings,
                all_props: &all_props,
            };
            self.check_finished_body(
                &body,
                &checks,
                pending,
                generator_mark,
                &generator_names,
                &mut body_members,
                depth,
            )?;
        }
        let mut source_scope = child_scope.flatten();
        for name in &unused_this_aliases {
            // A binding of the same name made by this body shadows the alias.
            if !child_scope.vars.contains_key(name.as_str())
                && !child_scope.poisoned.contains_key(name.as_str())
            {
                source_scope.shift_remove(name.as_str());
            }
        }
        let source = ObjectSource {
            entries: entries.clone(),
            scope: source_scope,
            scope_declared: child_scope.flatten_declared(),
            body_members: own_body.clone().unwrap_or_else(|| {
                entries
                    .iter()
                    .filter_map(entry_member_name)
                    .cloned()
                    .collect()
            }),
            is_open: true, // default: allow new properties
            type_name: None,
            type_identity: None,
            parent_type_names: Vec::new(),
            parent_type_identities: Vec::new(),
            scope_module_identities: child_scope.flatten_module_identities(),
            scope_type_aliases: child_scope.flatten_type_aliases(),
            entry_scopes: entry_scopes.map(<[_]>::to_vec).unwrap_or_default(),
            evaluated_properties: all_props.keys().cloned().collect(),
            mapping_value_types: Vec::new(),
            deprecated: collect_deprecated(entries),
        };
        Ok(BodyOutcome::Done(Value::Object(
            Arc::new(map),
            Some(Arc::new(source)),
        )))
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
            child_scope.set("super".into(), pv.clone());
        }

        // Class property defaults are checked against their declared types
        // when an instance is built, not when the class is defined.
        let child_defaults =
            self.eval_entries_checked(body, &child_scope, depth + 1, TypeChecks::Nothing)?;
        if let Some(Value::Object(parent_map, parent_src)) = parent_val {
            // Merge: parent defaults first, child overrides on top
            let mut merged: IndexMap<String, Value> = (*parent_map).clone();
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
                            Entry::DynProperty(Expr::String(s), _) => Some(s.clone()),
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
                                    if let Some(value) = psrc.scope.get(p.name.as_str()) {
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
                            src.scope_declared.remove(name.as_str());
                        }
                        src.scope.extend(
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
                Arc::make_mut(&mut map)
                    .retain(|key, _| !schema_member_names.contains(key.as_str()));
                let mut new_src = Arc::unwrap_or_clone(src);
                new_src.is_open = is_open;
                new_src.type_name = Some(class_name.to_string());
                new_src.type_identity = Some(scope.runtime_type_identity(class_name));
                new_src.parent_type_names = parent_type_names;
                new_src.parent_type_identities = parent_type_identities;
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
        match ty {
            crate::parser::TypeExpr::Named(target) => {
                // Alias to a class or another alias already in scope
                if let Some(val) = scope.get(target) {
                    scope.declare(name.to_string(), val.clone());
                }
            }
            crate::parser::TypeExpr::Nullable(inner) => {
                // typealias Foo = Bar? -- alias to the inner type
                if let crate::parser::TypeExpr::Named(target) = inner.as_ref()
                    && let Some(val) = scope.get(target)
                {
                    scope.declare(name.to_string(), val.clone());
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
            scope.declare(name.to_string(), val);
        }
    }

    /// Check a property's assigned value against its declared type, running
    /// the constraints of the type and of any type aliases it names.
    ///
    /// Only `name: Type = value` declarations are checked; amendments and
    /// type defaults carry no value of their own to check. `scope` should be
    /// the finished scope of the declaring module or object, so constraints
    /// can read members declared after the property.
    fn check_declared_property_type(
        &mut self,
        prop: &Property,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        match self.declared_type_mismatch(prop, value, scope, depth)? {
            Some(message) => Err(Error::Eval(message)),
            None => Ok(()),
        }
    }

    /// `check_declared_property_type` for a property of an object body,
    /// checked when the object is complete. A constraint that reads a name
    /// still unbound then (a member of an enclosing body bound later) cannot
    /// be decided by this eager evaluator and is left unchecked, as for typed
    /// locals; a violation, or any other error, still fails.
    fn check_object_property_type(
        &mut self,
        prop: &Property,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        if prop.value.is_none() {
            return Ok(());
        }
        let Some(ty) = &prop.type_ann else {
            return Ok(());
        };
        self.check_object_member_type(&prop.name, ty, value, scope, depth)
    }

    /// `check_object_property_type` for a member with declared type `ty`.
    fn check_object_member_type(
        &mut self,
        name: &str,
        ty: &crate::parser::TypeExpr,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        match self.type_mismatch(name, ty, value, scope, depth) {
            Ok(Some(message)) => Err(Error::Eval(message)),
            Ok(None) => Ok(()),
            Err(Error::Eval(message)) if message.starts_with("undefined variable: ") => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The error message when `value` does not match the type `prop`
    /// declares, or `None` when it matches or nothing is checked. An error
    /// evaluating a constraint is returned as is.
    fn declared_type_mismatch(
        &mut self,
        prop: &Property,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<String>> {
        if prop.value.is_none() {
            return Ok(None);
        }
        let Some(type_ann) = &prop.type_ann else {
            return Ok(None);
        };
        self.type_mismatch(&prop.name, type_ann, value, scope, depth)
    }

    /// The error message when the value of member `name` does not match its
    /// declared type `ty`, or `None` when it matches or the type is not
    /// checked at runtime.
    fn type_mismatch(
        &mut self,
        name: &str,
        ty: &crate::parser::TypeExpr,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<String>> {
        if type_is_runtime_checkable(ty, scope)
            && !self.eval_type_check(value, ty, scope, depth + 1)?
        {
            return Ok(Some(format!(
                "property '{}' expected {}, got {}",
                name,
                display_type_expr(ty),
                value_type_name(value)
            )));
        }
        Ok(None)
    }

    /// Check a typed local's value against its declared type. Pkl checks a
    /// local when it is read, so the caller poisons a failing local (as a
    /// failed evaluation does) rather than failing: any actual read then
    /// fails, through functions and constraints alike, and an unread local
    /// never does.
    ///
    /// Evaluate a typed local of an object body and check it, returning its
    /// binding (a value, or the error a read fails with) and whether it
    /// failed its type check. A check that needs the finished body (its
    /// constraint reads a member, or its type is an alias declared later) is
    /// added to `deferred_locals` instead. Kept out of `eval_body_entries`,
    /// whose hot loop most bodies run without a typed local.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn bind_typed_local<'a>(
        &mut self,
        prop: &'a Property,
        entries: &[Entry],
        entry_index: usize,
        scope: &Scope,
        depth: usize,
        poisoned_locals: &FxHashMap<String, String>,
        body_members: &mut Option<FxHashSet<String>>,
        deferred_locals: &mut Vec<DeferredLocal<'a>>,
    ) -> Result<(std::result::Result<Value, String>, bool)> {
        if let Some(message) = poisoned_locals.get(&prop.name) {
            // Failed its check in an earlier pass.
            return Ok((Err(message.clone()), true));
        }
        let Some(expr) = &prop.value else {
            return Ok((Ok(Value::Null), false));
        };
        let val = match self.eval_expr(expr, scope, depth) {
            Ok(val) => val,
            Err(Error::Eval(message)) => return Ok((Err(message), false)),
            Err(error) => return Err(error),
        };
        let Some(ty) = &prop.type_ann else {
            return Ok((Ok(val), false));
        };
        let members = body_members.get_or_insert_with(|| body_member_names(entries));
        if constraint_reads_members(ty, scope, members)
            || names_later_alias(ty, scope, entries, entry_index)
        {
            deferred_locals.push((prop, entry_index, bound_aliases(prop, scope)));
            return Ok((Ok(val), false));
        }
        Ok(match self.typed_local_failure(prop, &val, scope, depth)? {
            // Re-binding it after the properties would drop the poison, so
            // the caller is told it failed.
            LocalCheck::Failed(message) => (Err(message), true),
            LocalCheck::Passed => (Ok(val), false),
            LocalCheck::Unresolved => {
                deferred_locals.push((prop, entry_index, bound_aliases(prop, scope)));
                (Ok(val), false)
            }
        })
    }

    /// Check the typed locals whose check waited for the finished body.
    /// Returns those that fail (name, error), to poison in another pass.
    #[inline(never)]
    fn settle_deferred_locals(
        &mut self,
        body: &FinishedBody,
        deferred_locals: Vec<DeferredLocal>,
        depth: usize,
    ) -> Result<Vec<(String, String)>> {
        let mut failed_locals = Vec::new();
        for (prop, entry_index, aliases) in deferred_locals {
            let Some(Ok(value)) = body.local_bindings.get(&entry_index).cloned() else {
                continue;
            };
            let mut check_scope = body.check_scope(entry_index);
            // The local keeps the aliases it was declared with; only member
            // values come from the finished body.
            for (name, ty) in aliases {
                check_scope.set_type_alias(name, ty);
            }
            if let LocalCheck::Failed(message) =
                self.typed_local_failure(prop, &value, &check_scope, depth)?
            {
                failed_locals.push((prop.name.clone(), message));
            }
        }
        Ok(failed_locals)
    }

    /// Check the declared types of a finished object body's properties, and
    /// of those its `for`/`when` bodies handed up (`pending` from
    /// `generator_mark` on). In a `for`/`when` body itself (`checks` is
    /// `Generator`), its checks are handed up to the enclosing object instead.
    ///
    /// A constraint resolves the names it reads as Pkl does: lexically first,
    /// otherwise on the finished object, whose member may be one a generator
    /// produced (not bound by name in the body's scope) or replaced.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn check_finished_body(
        &mut self,
        body: &FinishedBody,
        checks: &TypeChecks,
        pending: &mut Vec<PendingTypeCheck>,
        generator_mark: usize,
        generator_names: &[String],
        body_members: &mut Option<FxHashSet<String>>,
        depth: usize,
    ) -> Result<()> {
        // Built when a generator body first has a check to hand up.
        let mut iteration_names: Option<FxHashSet<String>> = None;
        for (entry_index, entry) in body.entries.iter().enumerate() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            // Typed locals were checked when bound.
            let Some(ty) = &prop.type_ann else {
                continue;
            };
            if prop.value.is_none()
                || has_modifier(&prop.modifiers, Modifier::Local)
                || !checks.includes(entry_index)
            {
                continue;
            }
            let Some(value) = body.all_props.get(&prop.name).cloned() else {
                continue;
            };
            let active_scope = body.entry_scope(entry_index);
            if let TypeChecks::Generator(outer_names) = checks {
                let names = iteration_names.get_or_insert_with(|| {
                    let mut names = (**outer_names).clone();
                    names.extend(generator_names.iter().cloned());
                    names
                });
                // Only a type that reads a name the iteration declares (a
                // loop variable, or a generator body's local, class or type
                // alias) needs the iteration scope; any other type resolves
                // the same from the enclosing object's entry.
                let constrained = type_has_constraint(ty, &active_scope);
                let saved_scope = type_needs_iteration_scope(ty, &active_scope, names)
                    .then(|| capture_for_type_check(ty, &active_scope));
                let iteration_names = if saved_scope.is_some() {
                    names.clone()
                } else {
                    FxHashSet::default()
                };
                pending.push(PendingTypeCheck {
                    name: prop.name.clone(),
                    ty: ty.clone(),
                    value,
                    scope: saved_scope,
                    constrained,
                    iteration_names,
                    entry_index: None,
                });
                continue;
            }
            let members = body_members.get_or_insert_with(|| body_member_names(body.entries));
            if constraint_reads_members(ty, &active_scope, members) {
                drop(active_scope);
                let check_scope = body.check_scope(entry_index);
                self.check_object_property_type(prop, &value, &check_scope, depth)?;
            } else {
                self.check_object_property_type(prop, &value, &active_scope, depth)?;
            }
        }
        if checks.is_generator() {
            return Ok(());
        }
        // Check what this object's `for`/`when` bodies produced, in their
        // iteration scope with this object's finished members layered over
        // it, except names the iteration declares itself. Failed bindings of
        // the iteration stay failed.
        for check in pending.split_off(generator_mark) {
            // Final value: a later entry (such as an instance's override of a
            // class default) may have replaced the generator's.
            let value = body
                .all_props
                .get(&check.name)
                .cloned()
                .unwrap_or_else(|| check.value.clone());
            let Some((saved_scope, saved_poisoned)) = &check.scope else {
                // The type reads nothing the iteration declares, so it
                // resolves the same from the generator entry, with the
                // finished members layered as for a direct property when it
                // has a constraint.
                let check_scope = match check.entry_index {
                    Some(entry_index) if check.constrained => body.check_scope(entry_index),
                    Some(entry_index) => body.entry_scope(entry_index),
                    None => body.child_scope.clone(),
                };
                self.check_object_member_type(&check.name, &check.ty, &value, &check_scope, depth)?;
                continue;
            };
            // The iteration's own declarations are lexical and already bound
            // in its saved scope; the locals of the body holding the generator
            // are lexical too. Other members resolve as the generator entry
            // resolves them.
            let own_bindings = check
                .entry_index
                .map(|entry_index| body.own_bindings(entry_index))
                .unwrap_or_default()
                .into_iter()
                .filter(|(name, _)| !check.iteration_names.contains(name))
                .collect::<Vec<_>>();
            let mut check_scope = restore_scope(saved_scope).child();
            let layered = layer_finished_members(
                &mut check_scope,
                body.all_props,
                |name| {
                    check.iteration_names.contains(name)
                        || check
                            .entry_index
                            .is_some_and(|entry_index| body.hides_member(entry_index, name))
                },
                &own_bindings,
            );
            for (name, message) in saved_poisoned {
                if !layered.contains(name) {
                    check_scope.poison(name.clone(), message.clone());
                }
            }
            self.check_object_member_type(&check.name, &check.ty, &value, &check_scope, depth)?;
        }
        Ok(())
    }

    /// A constraint that reads a poisoned binding fails with that binding's
    /// error. One that reads a name not bound yet is `Unresolved`.
    fn typed_local_failure(
        &mut self,
        prop: &Property,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<LocalCheck> {
        match self.declared_type_mismatch(prop, value, scope, depth) {
            Ok(None) => Ok(LocalCheck::Passed),
            Ok(Some(message)) => Ok(LocalCheck::Failed(message)),
            Err(Error::Eval(message)) if message.starts_with("undefined variable: ") => {
                Ok(LocalCheck::Unresolved)
            }
            Err(Error::Eval(message)) => Ok(LocalCheck::Failed(message)),
            Err(error) => Err(error),
        }
    }

    /// Check if a value matches a type expression, including constraint evaluation.
    fn eval_type_check(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<bool> {
        use crate::parser::TypeExpr;
        match ty {
            TypeExpr::Named(name) => {
                if let Some(expected) = string_literal_type_value(name) {
                    return Ok(matches!(val, Value::String(actual) if actual == expected));
                }
                // Check if name is a type alias; if so, resolve to the aliased type
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_type_check(val, &resolved, scope, depth + 1);
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
                    self.eval_type_check(val, &resolved, scope, depth + 1)?
                } else {
                    value_is_class_type(val, class_name, scope)
                        .unwrap_or_else(|| value_is_named_type(val, base))
                };
                if !base_matches {
                    return Ok(false);
                }
                // Evaluate the constraint with `this` bound to the value
                let mut constraint_scope = scope.child();
                constraint_scope.set("this".into(), val.clone());
                // Also bind common properties directly so `length`, `isEmpty` etc. work
                match val {
                    Value::String(s) => {
                        constraint_scope.set("length".into(), Value::Int(s.chars().count() as i64));
                        constraint_scope.set("isEmpty".into(), Value::Bool(s.is_empty()));
                    }
                    Value::Int(_) | Value::Float(_) => {}
                    Value::List(items) => {
                        constraint_scope.set("length".into(), Value::Int(items.len() as i64));
                        constraint_scope.set("isEmpty".into(), Value::Bool(items.is_empty()));
                    }
                    Value::Object(items, _) => {
                        constraint_scope.set("length".into(), Value::Int(items.len() as i64));
                        constraint_scope.set("isEmpty".into(), Value::Bool(items.is_empty()));
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
                self.eval_type_check(val, inner, scope, depth)
            }
            TypeExpr::Union(variants) => {
                for v in variants {
                    if self.eval_type_check(val, v, scope, depth)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            // Non-constrained types: delegate to the simple check
            _ => Ok(value_is_type(val, ty)),
        }
    }

    /// Evaluate an amended object with late binding.
    ///
    /// Merges the base object's original entries with the overlay entries,
    /// then re-evaluates everything so that dependent properties pick up
    /// overridden values.
    fn eval_amended_object(
        &mut self,
        base_map: &Arc<IndexMap<String, Value>>,
        base_source: &Arc<ObjectSource>,
        overlay_entries: &[Entry],
        current_scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let base_entries = &base_source.entries;
        let base_scope = &base_source.scope;
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
            if !parent_members.contains_key(name)
                && let Some(value) = base_source.scope.get(name.as_str())
            {
                parent_members.insert(name.clone(), value.clone());
            }
        }
        amendment_scope.values.insert(
            "super".into(),
            Value::Object(Arc::new(parent_members), Some(Arc::clone(base_source))),
        );
        let amendment_entry_scope = Some(Arc::new(CapturedScope {
            body_members: overlay_entries
                .iter()
                .filter_map(entry_member_name)
                .cloned()
                .collect(),
            ..amendment_scope
        }));
        let mut overlay_by_name: IndexMap<String, &Entry> = IndexMap::new();
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry {
                overlay_by_name.insert(prop.name.clone(), entry);
            }
        }
        let last_base_property_index = base_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                Entry::Property(prop) => Some((prop.name.clone(), index)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        // Walk base entries: substitute overridden properties in-place.
        // If the overlay has a body amendment (no `=`), keep the base entry first
        // so its value is in scope, then add the overlay body entry after.
        let mut used_overlay: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Merged entries the amendment wrote with its own declared types (and
        // its generators, whose bodies may declare some). The base's entries
        // were checked where they were written, or are class defaults, which
        // the post-build check below covers.
        let mut overlay_checked = FxHashSet::default();
        for (entry_index, entry) in base_entries.iter().enumerate() {
            let inherited_entry_scope = base_source
                .entry_scopes
                .get(entry_index)
                .cloned()
                .unwrap_or(None);
            if let Entry::Property(prop) = entry
                && let Some(replacement) = overlay_by_name.get(&prop.name)
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
                    if last_base_property_index.get(&prop.name) != Some(&entry_index) {
                        continue;
                    }
                }
                let mut replacement = (*replacement).clone();
                if let Entry::Property(overlay_prop) = &mut replacement {
                    if overlay_prop.type_ann.is_some() {
                        // The amendment retypes this property itself.
                        overlay_checked.insert(merged.len());
                    } else {
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
                used_overlay.insert(prop.name.clone());
                continue;
            }
            if matches!(entry, Entry::ForGenerator(_) | Entry::WhenGenerator(_)) {
                // The post-build check below sees only the base's direct
                // properties, and a class body's generators were not checked
                // when the class was defined. Re-checking a generator from an
                // earlier amendment gives the same result.
                overlay_checked.insert(merged.len());
            }
            merged.push(entry.clone());
            merged_entry_scopes.push(inherited_entry_scope);
        }

        // Append overlay entries that are genuinely new (not replacing a base entry)
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry
                && used_overlay.contains(&prop.name)
            {
                continue; // already placed in-order above
            }
            if matches!(entry, Entry::Property(prop) if prop.type_ann.is_some())
                || matches!(entry, Entry::ForGenerator(_) | Entry::WhenGenerator(_))
            {
                overlay_checked.insert(merged.len());
            }
            merged.push(entry.clone());
            merged_entry_scopes.push(amendment_entry_scope.clone());
        }

        // Build scope: start with the base's captured scope, then layer current scope
        let mut eval_scope = Scope {
            type_namespace: capture_object_source_scope(base_source).type_namespace,
            ..Scope::default()
        };
        for (k, v) in base_scope {
            if base_source.scope_declared.contains(&**k) {
                eval_scope.declare_name(k.clone(), v.clone());
            } else {
                eval_scope.set_name(k.clone(), v.clone());
            }
        }
        for (name, identity) in &base_source.scope_module_identities {
            eval_scope.set_module_identity(name.clone(), identity.clone());
        }
        for (name, ty) in &base_source.scope_type_aliases {
            eval_scope.set_type_alias(name.clone(), ty.clone());
        }
        // Layer in current scope values (imports, module-level locals, etc.).
        // The same imported module can be field-pruned differently at its
        // definition and use sites. Preserve both partial views so methods
        // retain the classes captured by their definition-site import.
        // A different binding with the same name must not replace a free name
        // used by an inherited entry: those names are lexically bound where
        // the base object was defined, not where it is amended.
        let inherited_references = referenced_roots(base_entries);
        let mut preserved_inherited_bindings = HashSet::new();
        for (k, v) in current_scope.flatten() {
            // Inherited entries retain their original parent. The overlay's
            // parent is captured separately in amendment_entry_scope.
            if &*k == "super" {
                continue;
            }
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
                Rc::make_mut(&mut eval_scope.declared).remove(&*k);
                v
            };
            eval_scope.set_name(k, value);
        }
        for (name, identity) in current_scope.flatten_module_identities() {
            if !preserved_inherited_bindings.contains(&*name) {
                eval_scope.set_module_identity(name, identity);
            }
        }
        // Preserve definition-site aliases used by inherited entries. Overlay
        // entries are evaluated against `current_scope` separately below.
        for (k, ty) in current_scope.flatten_type_aliases() {
            if !inherited_references.contains(&k) || eval_scope.get_type_alias(&k).is_none() {
                eval_scope.set_type_alias(k, ty);
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
                eval_scope.set(prop.name.clone(), Value::Null);
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
            // Entries the amendment wrote are checked in their own scope once
            // the body is done; the base's declared properties are checked
            // below.
            TypeChecks::Entries(Rc::new(overlay_checked)),
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
                // Typed locals are checked lazily, when bound: a failure
                // poisons the local, so only a read fails.
                if has_modifier(&prop.modifiers, Modifier::Local) {
                    continue;
                }
                let Some(type_ann) = &prop.type_ann else {
                    continue;
                };
                let value = map.get(&prop.name).or_else(|| {
                    assigned_property_names
                        .contains(prop.name.as_str())
                        .then(|| {
                            source
                                .as_ref()
                                .and_then(|source| source.scope.get(prop.name.as_str()))
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
            (_, other) => Ok(other),
        }
    }

    fn eval_object_body_over_template(
        &mut self,
        template_map: &Arc<IndexMap<String, Value>>,
        template_src: &Arc<ObjectSource>,
        explicit_map: &IndexMap<String, Value>,
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
            source.scope.insert(binding.as_str().into(), value.clone());
            let mut replaced = false;
            for (index, entry) in Arc::make_mut(&mut source.entries).iter_mut().enumerate() {
                if let Entry::Property(prop) = entry
                    && prop.name == *key
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                {
                    prop.value = Some(Expr::Ident(binding.clone()));
                    prop.body = None;
                    if let Some(entry_scope) = source.entry_scopes.get_mut(index) {
                        *entry_scope = None;
                    }
                    replaced = true;
                }
            }
            if !replaced {
                Arc::make_mut(&mut source.entries).push(Entry::Property(Property {
                    annotations: Vec::new(),
                    modifiers: Vec::new(),
                    name: key.clone(),
                    type_ann: None,
                    value: Some(Expr::Ident(binding)),
                    body: None,
                }));
                source.entry_scopes.resize(source.entries.len(), None);
            }
            source.body_members.insert(key.clone());
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
                                amendment_scope.set(base_name.clone(), base.clone());
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
                    let values = match self.eval_expr(expr, &scope, depth + 1)? {
                        Value::List(values) => values,
                        Value::Object(values, _) => values.values().cloned().collect(),
                        value => vec![value],
                    };
                    if target >= *position && target - *position < values.len() {
                        *value = Some(values[target - *position].clone());
                    }
                    *position += values.len();
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, item) in collection_to_items(collection) {
                        let mut iter_scope = scope.child();
                        let mut iter_locals = locals.clone();
                        // Bind generator variables after enclosing locals so
                        // shadowing does not change those locals' definitions.
                        let value_binding = format!("\0listing_generator_value:{depth}");
                        iter_scope.set(value_binding.clone(), item.clone());
                        iter_scope.set(generator.val_var.clone(), item);
                        iter_locals.push((generator.val_var.clone(), Expr::Ident(value_binding)));
                        if let Some(key_var) = &generator.key_var {
                            let key_binding = format!("\0listing_generator_key:{depth}");
                            iter_scope.set(key_binding.clone(), key.clone());
                            iter_scope.set(key_var.clone(), key);
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
        for entry in entries {
            match entry {
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {
                    if let Some(expr) = &prop.value {
                        let value = self.eval_expr(expr, &listing_scope, depth + 1)?;
                        listing_scope.declare(prop.name.clone(), value);
                    }
                }
                Entry::Elem(expr) => items.push(self.eval_expr(expr, &listing_scope, depth + 1)?),
                Entry::Property(prop) if prop.value.is_some() => {
                    let value =
                        self.eval_expr(prop.value.as_ref().unwrap(), &listing_scope, depth + 1)?;
                    listing_scope.set(prop.name.clone(), value);
                }
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
                Entry::Spread(expr) => match self.eval_expr(expr, &listing_scope, depth + 1)? {
                    Value::List(values) => items.extend(values),
                    Value::Object(values, _) => items.extend(values.values().cloned()),
                    value => items.push(value),
                },
                Entry::ForGenerator(generator) => {
                    let collection =
                        self.eval_expr(&generator.collection, &listing_scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection) {
                        let mut iter_scope = listing_scope.child();
                        iter_scope.set(generator.val_var.clone(), value);
                        if let Some(key_var) = &generator.key_var {
                            iter_scope.set(key_var.clone(), key);
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
            Expr::String(s) => Ok(Value::String(s.clone())),
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
                Ok(Value::String(result))
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
                let mut body = (**body).clone();
                capture_method_result_types(&mut body, scope);
                let mut refs = HashSet::new();
                let shadows = params.iter().cloned().collect::<HashSet<_>>();
                collect_expr_refs(&body, &mut refs, &shadows);
                // Capture only the bindings the body can reach: every name it
                // mentions, ignoring shadowing, plus the implicit receivers.
                // Flattening the whole scope for every lambda value, and
                // restoring all of it on every call, dominated evaluation. An
                // object built in the body sees its enclosing bindings through
                // `outer`, so a body that mentions `outer` keeps everything, as
                // does a body that names a type (see `NAMES_A_TYPE`).
                let mut names = HashSet::new();
                collect_unshadowed_names(&body, &mut names);
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
                    .map(|message| Expr::Throw(Box::new(Expr::String(message.clone()))))
                    .unwrap_or(body);
                Ok(Value::Lambda(
                    params.as_slice().into(),
                    Arc::new(captured_body),
                    captured,
                ))
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
                constructor_scope.set(
                    "super".into(),
                    Value::Object(Arc::new(IndexMap::new()), None),
                );
                constructor_scope.receiver_entries = Some(entries.clone());
                constructor_scope.receiver_list_base = None;
                let scope = &constructor_scope;
                match type_name.as_deref() {
                    Some("Listing") => {
                        let mut listing_scope = scope.child();
                        listing_scope.set("super".into(), Value::List(Vec::new()));
                        listing_scope.receiver_list_base = Some(0);
                        let mut items = Vec::new();
                        self.eval_listing_entries(entries, &listing_scope, depth + 1, &mut items)?;
                        Ok(Value::List(items))
                    }
                    Some("Mapping") | Some("Map") => {
                        // If the Mapping has a value type param (e.g., Mapping<String, Step>),
                        // resolve it as a default template so entries inherit the class type.
                        let value_type_defaults = generic_params
                            .iter()
                            .skip(1)
                            .filter_map(|name| {
                                resolve_dotted(scope, name).map(|value| (name.clone(), value))
                            })
                            .collect::<Vec<_>>();
                        let mut map = IndexMap::new();
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
                            src_entries.push(Entry::Property(Property {
                                annotations: vec![],
                                modifiers: vec![],
                                name: "default".into(),
                                type_ann: None,
                                value: Some(Expr::New(Some(vt_name), vec![].into(), vec![])),
                                body: None,
                            }));
                        }
                        let mut source_scope = scope.flatten();
                        source_scope.shift_remove("outer");
                        source_scope.shift_remove("this");
                        let source_declared = scope.flatten_declared();
                        let source_body_members = src_entries
                            .iter()
                            .filter_map(entry_member_name)
                            .cloned()
                            .collect();
                        let mut source_module_identities = scope.flatten_module_identities();
                        source_module_identities.shift_remove("outer");
                        source_module_identities.shift_remove("this");
                        let deprecated = collect_deprecated(&src_entries);
                        let source = ObjectSource {
                            entries: src_entries.into(),
                            scope: source_scope,
                            scope_declared: source_declared,
                            body_members: source_body_members,
                            is_open: true,
                            type_name: None,
                            type_identity: None,
                            parent_type_names: Vec::new(),
                            parent_type_identities: Vec::new(),
                            scope_module_identities: source_module_identities,
                            scope_type_aliases: scope.flatten_type_aliases(),
                            entry_scopes: Vec::new(),
                            evaluated_properties: map.keys().cloned().collect(),
                            mapping_value_types: generic_params.iter().skip(1).cloned().collect(),
                            deprecated,
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
                        if let Some(Value::Object(ref base_map, Some(ref base_src))) = base {
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
                                    .chain(base_map.keys().cloned())
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
                                            if !base_names.contains(key) =>
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
                                        scope: ScopeMap::default(),
                                        scope_declared: NameSet::default(),
                                        body_members: HashSet::new(),
                                        is_open,
                                        type_name: tn,
                                        type_identity: base_src.type_identity.clone(),
                                        parent_type_names: base_src.parent_type_names.clone(),
                                        parent_type_identities: base_src
                                            .parent_type_identities
                                            .clone(),
                                        scope_module_identities: IndexMap::new(),
                                        scope_type_aliases: IndexMap::new(),
                                        entry_scopes: Vec::new(),
                                        evaluated_properties: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&base_src.deprecated, entries),
                                    }
                                };
                                *src_slot = Some(Arc::new(new_src));
                            }
                            Ok(result)
                        } else if let Some(Value::Object(base_map, base_src)) = base {
                            // Fallback: eager merge
                            let overlay = self.eval_entries(entries, scope, depth + 1)?;
                            let mut merged: IndexMap<String, Value> = (*base_map).clone();
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
                                scope: ScopeMap::default(),
                                scope_declared: NameSet::default(),
                                body_members: HashSet::new(),
                                is_open: true,
                                type_name: type_name.clone(),
                                type_identity: None,
                                parent_type_names: Vec::new(),
                                parent_type_identities: Vec::new(),
                                scope_module_identities: IndexMap::new(),
                                scope_type_aliases: IndexMap::new(),
                                entry_scopes: Vec::new(),
                                evaluated_properties: Vec::new(),
                                mapping_value_types: Vec::new(),
                                deprecated,
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
                body_scope.set(
                    "super".into(),
                    Value::Object(Arc::new(IndexMap::new()), None),
                );
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
                        return Ok(Value::List(
                            map.keys().map(|k| Value::String(k.clone())).collect(),
                        ));
                    }
                    (Value::Object(map, _), "values") => {
                        return Ok(Value::List(map.values().cloned().collect()));
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
                        let val = map
                            .get(field)
                            .cloned()
                            .ok_or_else(|| Error::Eval(format!("field not found: {field}")))?;
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
                        let val = map.get(field).cloned().unwrap_or(Value::Null);
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
                    Value::Object(map, _) => map
                        .get(&key_str)
                        .cloned()
                        .ok_or_else(|| Error::Eval(format!("key not found: {key_str}"))),
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
                child.set(name.clone(), val);
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
                // A retry pass re-evaluates a body whose first pass already
                // ran to completion and printed its traces.
                if self.retry_passes == 0 {
                    eprintln!("[pklr trace] {}", value_to_display(&v));
                }
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
                    return Ok(Value::List(items));
                }
                "Set" => {
                    let mut items = Vec::new();
                    for a in args {
                        let val = self.eval_expr(a, scope, depth + 1)?;
                        if !items.contains(&val) {
                            items.push(val);
                        }
                    }
                    return Ok(Value::List(items)); // deduplicated
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
                    let mut map = IndexMap::new();
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
                    call_scope.set(k.clone(), v.clone());
                }
            }
            // Bind arguments to parameters
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
            }
            for (param, arg) in params.iter().zip(evaled_args) {
                call_scope.declare(param.clone(), arg);
            }
            return self.eval_expr(&body, &call_scope, depth + 1);
        }

        // Built-in type constructors resolved from scope (e.g. base.Regex)
        if let Value::String(ref name) = func_val
            && name == "Regex"
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
                && name == "Regex"
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
                call_scope.set(k.clone(), v.clone());
            }
            call_scope.set("this".into(), obj.clone());
            for (i, param) in params.iter().enumerate() {
                if let Some(arg) = evaled_args.get(i) {
                    call_scope.declare(param.clone(), arg.clone());
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
                let mut result = s.clone();
                if let Some(start) = s.rfind(from) {
                    result.replace_range(start..start + from.len(), to);
                }
                Ok(Some(Value::String(result)))
            }
            (Value::String(s), "replaceAll") => {
                let from = require_str_arg(args, 0, "replaceAll")?;
                let to = require_str_arg(args, 1, "replaceAll")?;
                Ok(Some(Value::String(s.replace(from, to))))
            }
            (Value::String(s), "split") => {
                let sep = require_str_arg(args, 0, "split")?;
                Ok(Some(Value::List(
                    s.split(sep).map(|p| Value::String(p.to_string())).collect(),
                )))
            }
            (Value::String(s), "trim") => Ok(Some(Value::String(s.trim().to_string()))),
            (Value::String(s), "trimStart") => Ok(Some(Value::String(s.trim_start().to_string()))),
            (Value::String(s), "trimEnd") => Ok(Some(Value::String(s.trim_end().to_string()))),
            (Value::String(s), "toUpperCase") => Ok(Some(Value::String(s.to_uppercase()))),
            (Value::String(s), "toLowerCase") => Ok(Some(Value::String(s.to_lowercase()))),
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
                Ok(Some(Value::Bool(items.contains(&arg))))
            }
            (Value::List(items), "toList") => Ok(Some(Value::List(items.clone()))),
            (Value::List(items), "toSet") => {
                let mut seen = Vec::new();
                for item in items {
                    if !seen.contains(item) {
                        seen.push(item.clone());
                    }
                }
                Ok(Some(Value::List(seen)))
            }
            (Value::List(items), "map") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("map requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    result.push(self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?);
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "flatMap") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("flatMap requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    let val = self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?;
                    if let Value::List(inner) = val {
                        result.extend(inner);
                    } else {
                        result.push(val);
                    }
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    let cond = self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?;
                    if is_truthy(&cond) {
                        result.push(item.clone());
                    }
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "filterNonNull") => Ok(Some(Value::List(
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
                for item in items {
                    acc = self.invoke_lambda(lambda, &[acc, item.clone()], depth)?;
                }
                Ok(Some(acc))
            }
            (Value::List(items), "any") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("any requires a function argument".into()))?;
                for item in items {
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
                for item in items {
                    if !is_truthy(&self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?) {
                        return Ok(Some(Value::Bool(false)));
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            (Value::List(items), "join") => {
                let sep = args.first().and_then(|v| v.as_str()).unwrap_or(",");
                let s: Vec<String> = items.iter().map(value_to_display).collect();
                Ok(Some(Value::String(s.join(sep))))
            }
            (Value::List(items), "reverse") => {
                let mut rev = items.clone();
                rev.reverse();
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
                let mut result = IndexMap::new();
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
                let mut result = IndexMap::new();
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
            (Value::Int(n), "toString") => Ok(Some(Value::String(n.to_string()))),
            (Value::Float(f), "toString") => Ok(Some(Value::String(f.to_string()))),
            (Value::Bool(b), "toString") => Ok(Some(Value::String(b.to_string()))),

            // Lambda.apply()
            (Value::Lambda(params, body, captured), "apply") => {
                let mut call_scope = Scope::for_call(captured);
                for (param, arg) in params.iter().zip(args.iter()) {
                    call_scope.declare(param.clone(), arg.clone());
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
                scope.declare(param.clone(), arg.clone());
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
        if let Value::List(existing) = base {
            let mut amended = existing;
            let mut amendment_scope = scope.child();
            amendment_scope.set("super".into(), Value::List(amended.clone()));
            amendment_scope.receiver_entries = Some(overlay_entries.clone());
            amendment_scope.receiver_list_base = Some(amended.len());
            self.eval_listing_entries(overlay_entries, &amendment_scope, depth + 1, &mut amended)?;
            return Ok(Value::List(amended));
        }
        if let Value::Object(base_map, Some(base_src)) = &base {
            if !base_src.mapping_value_types.is_empty() {
                let (inherited_scope, mut amendment_scope) =
                    mapping_amendment_scopes(&base_src.scope, &base_src.scope_declared, scope);
                amendment_scope.set("super".into(), base.clone());
                let mut receiver_entries = base_map
                    .keys()
                    .map(|key| Entry::DynProperty(Expr::String(key.clone()), Expr::Null))
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
                let mut amended = IndexMap::new();
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
                amendment_scope.set(name.clone(), value.clone());
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
        map: &mut IndexMap<String, Value>,
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
                    entry_scope.declare(prop.name.clone(), val);
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
                    entry_scope.declare(prop.name.clone(), val);
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
                    entry_scope.set(name.clone(), defaults);
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
                                        scope: ScopeMap::default(),
                                        scope_declared: NameSet::default(),
                                        body_members: HashSet::new(),
                                        is_open: true,
                                        type_name: Some(tn.to_string()),
                                        type_identity: src.type_identity.clone(),
                                        parent_type_names: src.parent_type_names.clone(),
                                        parent_type_identities: src.parent_type_identities.clone(),
                                        scope_module_identities: IndexMap::new(),
                                        scope_type_aliases: IndexMap::new(),
                                        entry_scopes: Vec::new(),
                                        evaluated_properties: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&src.deprecated, body),
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
                    if let Value::Object(m, _) = val {
                        map.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self.eval_expr(&fgen.collection, &entry_scope, depth + 1)?;
                    for (k, v) in collection_to_items(collection) {
                        let mut iter_scope = entry_scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(kv) = &fgen.key_var {
                            iter_scope.set(kv.clone(), k);
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
                converter_scope.set(prop.name.clone(), value);
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
                converter_scope.set(prop.name.clone(), value);
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
                    Expr::String(s) => s.clone(),
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
    pub async fn apply_converters(&mut self, value: Value) -> Result<Value> {
        if self.converters.is_empty() {
            return Ok(value);
        }
        self.run_async(value, |evaluator, value| {
            evaluator.apply_converters_blocking(value)
        })
        .await
    }

    /// Apply `output.renderer.converters` to a value tree, blocking on host IO.
    pub fn apply_converters_blocking(&mut self, value: Value) -> Result<Value> {
        if self.converters.is_empty() {
            return Ok(value);
        }
        let converters = self.converters.clone();
        self.apply_converters_recursive(value, &converters, Vec::new())
    }

    fn apply_converters_recursive(
        &mut self,
        value: Value,
        converters: &[(String, Value)],
        blocked_root_converters: Vec<String>,
    ) -> Result<Value> {
        match value {
            Value::Object(map, ref src) => {
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
                                    call_scope.declare(
                                        param.clone(),
                                        Value::Object(map.clone(), src.clone()),
                                    );
                                }
                                let result = self.eval_expr(body, &call_scope, 0)?;
                                let mut blocked = blocked_root_converters;
                                blocked.push(conv_name.clone());
                                return self
                                    .apply_converters_recursive(result, converters, blocked);
                            }
                        }
                    }
                }

                // No converter matched — recurse into children
                let mut new_map = IndexMap::new();
                for (k, v) in map.iter() {
                    new_map.insert(
                        k.clone(),
                        self.apply_converters_recursive(v.clone(), converters, Vec::new())?,
                    );
                }
                Ok(Value::Object(Arc::new(new_map), src.clone()))
            }
            Value::List(items) => {
                let mut new_items = Vec::with_capacity(items.len());
                for item in items {
                    new_items.push(self.apply_converters_recursive(
                        item,
                        converters,
                        Vec::new(),
                    )?);
                }
                Ok(Value::List(new_items))
            }
            other => Ok(other),
        }
    }
}

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
                Some(HashSet::from(["wanted".to_string()])),
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

        let value = Evaluator::default().eval_file_blocking(&main_path).unwrap();
        let Value::Object(fields, _) = value else {
            panic!("expected module object");
        };
        let Value::Object(_, Some(source)) = &fields["result"] else {
            panic!("expected mapping object source");
        };
        assert!(source.scope.contains_key("Config"));
        assert!(source.scope_module_identities.contains_key("Config"));

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
                scope.declare(alias.clone(), inherited_val.clone());
                let identity = self.module_type_namespace(alias_path);
                scope.set_module_identity(alias.clone(), identity);
            }
        }
        Ok(())
    }
}

fn stdlib_module(name: &str) -> Value {
    let mut map = IndexMap::new();
    if name == "base" {
        map.insert("Regex".to_string(), Value::String("Regex".to_string()));
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
        scope.set(name.to_string(), Value::String(name.to_string()));
    }
}

/// Whether `IntSeq` in `scope` is still the built-in, which `seed_builtins`
/// binds to a marker string, rather than a user binding of that name.
fn int_seq_is_builtin(scope: &Scope) -> bool {
    matches!(scope.get("IntSeq"), Some(Value::String(name)) if name == "IntSeq")
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
            .collect(),
    ))
}

fn collection_to_items(v: Value) -> Vec<(Value, Value)> {
    match v {
        Value::List(items) => items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Value::Int(i as i64), v))
            .collect(),
        Value::Object(map, _) => map
            .iter()
            .map(|(k, v)| (Value::String(k.clone()), v.clone()))
            .collect(),
        _ => vec![],
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
    #[cfg(all(feature = "native-io", feature = "package-zip-core"))]
    use std::path::PathBuf;

    #[cfg(all(feature = "native-io", feature = "package-zip-core"))]
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
    #[cfg(feature = "package-zip-core")]
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
    #[cfg(all(feature = "blocking", feature = "native-io"))]
    fn blocking_preload_does_not_require_a_tokio_runtime() {
        let cache_dir = std::env::temp_dir().join(format!(
            "pklr-blocking-preload-{}-{:?}",
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
    #[cfg(all(feature = "native-io", feature = "package-zip-core"))]
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

#[cfg(all(test, feature = "blocking"))]
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
            .eval_source_blocking(source, std::path::Path::new("super.pkl"))
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
