use super::*;
use std::cell::RefCell;

use rustc_hash::{FxBuildHasher, FxHashSet};

pub(super) type FxIndexMap<K, V> = IndexMap<K, V, FxBuildHasher>;

/// A binding name in a [`Scope`]. Reference-counted so copying bindings
/// between scopes, which happens for every evaluated entry, does not
/// allocate.
pub(super) type Name = Arc<str>;

// --- Scope ---

#[derive(Clone)]
pub(super) struct ObjectTypeMetadata {
    pub(super) name: String,
    pub(super) identity: Option<String>,
    pub(super) parent_names: Vec<String>,
    pub(super) parent_identities: Vec<String>,
}

pub(super) fn object_type_metadata(source: &ObjectSource) -> Option<ObjectTypeMetadata> {
    Some(ObjectTypeMetadata {
        name: source.type_name.clone()?,
        identity: source.type_identity.clone(),
        parent_names: source.parent_type_names.clone(),
        parent_identities: source.parent_type_identities.clone(),
    })
}

#[derive(Debug, Default, Clone)]
pub(super) struct Scope {
    // The maps are shared copy-on-write so cloning a scope, which `child`
    // does for every nested body, does not copy its bindings.
    // They also use a fast non-cryptographic hasher: lookups and inserts on
    // these maps dominate evaluation, and the keys come from trusted source.
    // `vars` is an `Arc` so a lambda's captured bindings (a `ScopeMap`, the
    // same map type) can be used as a scope without copying them.
    pub(super) vars: Arc<FxIndexMap<Name, Value>>,
    pub(super) type_aliases: Rc<FxIndexMap<Name, crate::parser::TypeExpr>>,
    /// Whether a type alias in this scope, or in a scope it was built over,
    /// may have a built-in type's name (`typealias String = ...`). Without
    /// one, a built-in type name needs no alias lookup. May over-approximate.
    pub(super) shadows_builtin_type: bool,
    /// Whether this scope belongs to a class definition's body, or to a
    /// value an amendment amends with a body of its own (or an object body
    /// nested in either). Declared types there are checked when an instance
    /// is built, or by that amendment, against the final values. Not
    /// captured: an instance's scopes, and modules imported meanwhile, start
    /// without it.
    pub(super) defining_class: bool,
    pub(super) module_identities: Rc<FxIndexMap<Name, String>>,
    pub(super) poisoned: Rc<FxIndexMap<Name, String>>,
    /// Names in `vars` or `poisoned` declared by an entry written in the body
    /// that owns this scope, as opposed to members an object inherits from its
    /// class or parent. Pkl resolves a name in lexically enclosing bodies
    /// before falling back to the object's inherited members, so only these
    /// declared names may win over an inherited member of an inner object.
    pub(super) declared: Rc<FxHashSet<Name>>,
    /// Names in `vars` bound by a `local` that aliases the `this` of the
    /// object owning this scope (`local self = this`). Unlike a property such
    /// as `me = this`, such a local is not a member, so a nested object whose
    /// body never names it can leave it out of what it captures.
    pub(super) this_aliases: Rc<FxHashSet<String>>,
    pub(super) type_namespace: Option<String>,
    pub(super) receiver_entries: Option<Arc<Vec<Entry>>>,
    pub(super) receiver_list_base: Option<usize>,
    pub(super) parent: Option<Rc<Scope>>,
}

impl Scope {
    /// A scope for calling a lambda: its captured bindings, shared rather
    /// than copied, under a child layer for the call's own bindings
    /// (parameters, and anything the caller layers over the capture).
    pub(super) fn for_call(captured: &Arc<ScopeMap>) -> Self {
        Scope {
            vars: Arc::clone(captured),
            ..Scope::default()
        }
        .child()
    }

    pub(super) fn child(&self) -> Self {
        Self {
            vars: Arc::default(),
            type_aliases: Rc::default(),
            shadows_builtin_type: self.shadows_builtin_type,
            defining_class: self.defining_class,
            module_identities: Rc::default(),
            poisoned: Rc::default(),
            declared: Rc::default(),
            this_aliases: Rc::default(),
            type_namespace: self.type_namespace.clone(),
            receiver_entries: self.receiver_entries.clone(),
            receiver_list_base: self.receiver_list_base,
            parent: Some(Rc::new(self.clone())),
        }
    }

    pub(super) fn runtime_type_identity(&self, name: &str) -> String {
        self.type_namespace
            .as_ref()
            .map(|namespace| format!("{namespace}.{name}"))
            .unwrap_or_else(|| name.to_string())
    }

    pub(super) fn set(&mut self, name: String, val: Value) {
        self.set_name(name.into(), val);
    }

    pub(super) fn set_name(&mut self, name: Name, val: Value) {
        if self.poisoned.contains_key(&*name) {
            Rc::make_mut(&mut self.poisoned).shift_remove(&*name);
        }
        if self.module_identities.contains_key(&*name) {
            Rc::make_mut(&mut self.module_identities).shift_remove(&*name);
        }
        if self.this_aliases.contains(&*name) {
            Rc::make_mut(&mut self.this_aliases).remove(&*name);
        }
        Arc::make_mut(&mut self.vars).insert(name, val);
    }

    /// Mark the binding of `name` in this scope as a local alias of `this`.
    pub(super) fn mark_this_alias(&mut self, name: &str) {
        if !self.this_aliases.contains(name) {
            Rc::make_mut(&mut self.this_aliases).insert(name.to_string());
        }
    }

    /// Local `this` aliases visible from this scope: names whose innermost
    /// binding is marked by [`Scope::mark_this_alias`].
    pub(super) fn visible_this_aliases(&self) -> Vec<String> {
        // Walk the chain once, innermost first. A marked name counts only if
        // no inner level binds it; marks are few, so checking the inner levels
        // per mark stays cheap however many bindings each level holds.
        let mut names = Vec::new();
        let mut seen = FxHashSet::default();
        let mut inner: Vec<&Scope> = Vec::new();
        let mut level = Some(self);
        while let Some(scope) = level {
            for name in scope.this_aliases.iter() {
                if scope.binds(name)
                    && seen.insert(name.as_str())
                    && !inner.iter().any(|inner| inner.binds(name))
                {
                    names.push(name.clone());
                }
            }
            inner.push(scope);
            level = scope.parent.as_deref();
        }
        names
    }

    fn binds(&self, name: &str) -> bool {
        self.vars.contains_key(name) || self.poisoned.contains_key(name)
    }

    /// Bind a name declared in the body that owns this scope.
    pub(super) fn declare(&mut self, name: String, val: Value) {
        self.declare_name(name.into(), val);
    }

    pub(super) fn declare_name(&mut self, name: Name, val: Value) {
        if !self.declared.contains(&*name) {
            Rc::make_mut(&mut self.declared).insert(name.clone());
        }
        self.set_name(name, val);
    }

    /// Poison a local declared in the body that owns this scope.
    pub(super) fn declare_poisoned(&mut self, name: String, message: String) {
        let name = Name::from(name);
        if !self.declared.contains(&*name) {
            Rc::make_mut(&mut self.declared).insert(name.clone());
        }
        Rc::make_mut(&mut self.poisoned).insert(name, message);
    }

    /// Whether the innermost binding of `name` was declared in a body rather
    /// than inherited from a class or parent object.
    pub(super) fn is_declared(&self, name: &str) -> bool {
        if self.vars.contains_key(name) || self.poisoned.contains_key(name) {
            self.declared.contains(name)
        } else {
            self.parent
                .as_ref()
                .is_some_and(|parent| parent.is_declared(name))
        }
    }

    pub(super) fn flatten_declared(&self) -> NameSet {
        let mut declared = self
            .parent
            .as_ref()
            .map(|parent| parent.flatten_declared())
            .unwrap_or_default();
        for name in self.vars.keys().chain(self.poisoned.keys()) {
            if self.declared.contains(name) {
                declared.insert(name.clone());
            } else {
                declared.remove(name);
            }
        }
        declared
    }

    pub(super) fn set_module_identity(&mut self, name: String, identity: String) {
        Rc::make_mut(&mut self.module_identities).insert(name.into(), identity);
    }

    pub(super) fn module_identity(&self, name: &str) -> Option<&String> {
        if self.vars.contains_key(name) {
            self.module_identities.get(name)
        } else {
            self.parent.as_ref()?.module_identity(name)
        }
    }

    pub(super) fn flatten_module_identities(&self) -> IndexMap<String, String> {
        let mut identities = self
            .parent
            .as_ref()
            .map(|parent| parent.flatten_module_identities())
            .unwrap_or_default();
        for name in self.vars.keys() {
            identities.shift_remove(&**name);
        }
        identities.extend(
            self.module_identities
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        );
        identities
    }

    /// Replace a binding declared in the body that owns this scope with a
    /// poisoned one, dropping any value it held.
    pub(super) fn redeclare_poisoned(&mut self, name: String, message: String) {
        if self.vars.contains_key(name.as_str()) {
            Arc::make_mut(&mut self.vars).shift_remove(name.as_str());
        }
        self.declare_poisoned(name, message);
    }

    /// Key recording the error of a module member (a class, type alias or
    /// module function, never a local) that failed to evaluate. It contains a
    /// dot, so it never collides with a binding.
    pub(super) fn member_poison_key(name: &str) -> String {
        format!("module.{name}")
    }

    /// Record (`Some`) or clear (`None`) the error of module member `name`.
    pub(super) fn set_member_poison(&mut self, name: &str, message: Option<String>) {
        let key = Self::member_poison_key(name);
        match message {
            Some(message) => self.poison(key, message),
            None if self.poisoned.contains_key(key.as_str()) => {
                Rc::make_mut(&mut self.poisoned).shift_remove(key.as_str());
            }
            None => {}
        }
    }

    pub(super) fn poison(&mut self, name: String, message: String) {
        Rc::make_mut(&mut self.poisoned).insert(name.into(), message);
    }

    pub(super) fn poison_of(&self, name: &str) -> Option<&String> {
        self.poisoned.get(name).or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.poison_of(name))
        })
    }

    pub(super) fn set_type_alias(&mut self, name: String, ty: crate::parser::TypeExpr) {
        if super::types::is_builtin_type_name(&name) {
            self.shadows_builtin_type = true;
        }
        Rc::make_mut(&mut self.type_aliases).insert(name.into(), ty);
    }

    /// Whether a type alias visible from this scope mentions `name`. A type
    /// alias's constraint runs in the scope of the value being checked, so it
    /// can read bindings such as `outer` from wherever the check happens.
    pub(super) fn type_aliases_mention(&self, name: &str) -> bool {
        self.type_aliases.values().any(|ty| type_mentions(ty, name))
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.type_aliases_mention(name))
    }

    /// Whether any type alias is visible from this scope.
    pub(super) fn has_type_aliases(&self) -> bool {
        !self.type_aliases.is_empty()
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.has_type_aliases())
    }

    pub(super) fn get_type_alias(&self, name: &str) -> Option<&crate::parser::TypeExpr> {
        self.type_aliases
            .get(name)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_type_alias(name)))
    }

    pub(super) fn get(&self, name: &str) -> Option<&Value> {
        if self.poisoned.contains_key(name) {
            return None;
        }
        self.vars
            .get(name)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get(name)))
    }

    pub(super) fn flatten(&self) -> ScopeMap {
        let mut result = self
            .parent
            .as_ref()
            .map(|p| p.flatten())
            .unwrap_or_default();
        for name in self.poisoned.keys() {
            result.shift_remove(&**name);
        }
        result.extend(self.vars.iter().map(|(k, v)| (k.clone(), v.clone())));
        result
    }

    /// The subset of `flatten` for `names`, sorted by name.
    pub(super) fn flatten_names<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> ScopeMap {
        let mut names = names.into_iter().collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| {
                let (key, value) = self.flattened(name)?;
                Some((key.clone(), value.clone()))
            })
            .collect()
    }

    /// The binding `flatten` would hold for `name`, with the scope's stored
    /// key: an inner binding wins, and a name poisoned at a level hides outer
    /// bindings unless that same level also binds it.
    fn flattened(&self, name: &str) -> Option<(&Name, &Value)> {
        if let Some(binding) = self.vars.get_key_value(name) {
            return Some(binding);
        }
        if self.poisoned.contains_key(name) {
            return None;
        }
        self.parent.as_ref()?.flattened(name)
    }

    pub(super) fn flatten_type_aliases(&self) -> IndexMap<String, crate::parser::TypeExpr> {
        let mut result = self
            .parent
            .as_ref()
            .map(|p| p.flatten_type_aliases())
            .unwrap_or_default();
        result.extend(
            self.type_aliases
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        );
        result
    }
}

/// The saved error for member `name` that object `obj_expr` (with `source`)
/// does not have: a failed class of an imported module object, or, inside
/// the defining module, one read through `module`/`this`. Used by every
/// by-name member read (`a.C`, `a?.C`, `a["C"]`) before reporting a missing
/// member.
pub(super) fn missing_member_error(
    source: &Option<Arc<ObjectSource>>,
    obj_expr: &Expr,
    name: &str,
    scope: &Scope,
) -> Option<String> {
    if let Some(message) = source
        .as_ref()
        .and_then(|source| source.poisoned_members.as_ref()?.get(name))
    {
        return Some(message.clone());
    }
    match obj_expr {
        Expr::Ident(root) if root == "module" || root == "this" => {
            poisoned_member(scope, &format!("{root}.{name}"))
        }
        _ => None,
    }
}

/// The error of a poisoned member a dotted name refers to: either the root
/// binding itself, or a member of a module object that failed to evaluate
/// (`dep.C` where `dep`'s class `C` could not be built). `None` when the name
/// resolves or is simply absent.
pub(super) fn poisoned_member(scope: &Scope, name: &str) -> Option<String> {
    let mut parts = name.trim_end_matches('?').split('.');
    let root = parts.next()?;
    let Some(mut value) = scope.get(root) else {
        return scope.poison_of(root).cloned();
    };
    // Inside the defining module, `module` (and `this` while it is the module
    // object) is a snapshot of the module's members. A member that failed to
    // evaluate is absent from it; its error is kept in the module scope under
    // `Scope::member_poison_key`. Locals are not members, so a failed local
    // is never reported here.
    let names_current_module = root == "module"
        || (root == "this"
            && matches!(
                (value, scope.get("module")),
                (Value::Object(this, _), Some(Value::Object(module, _))) if Arc::ptr_eq(this, module)
            ));
    for (index, part) in parts.enumerate() {
        let Value::Object(map, source) = value else {
            return None;
        };
        match map.get(part) {
            Some(member) => value = member,
            None => {
                if let Some(message) = source
                    .as_ref()
                    .and_then(|source| source.poisoned_members.as_ref()?.get(part))
                {
                    return Some(message.clone());
                }
                return (index == 0 && names_current_module)
                    .then(|| scope.poison_of(&Scope::member_poison_key(part)).cloned())
                    .flatten();
            }
        }
    }
    None
}

pub(super) fn capture_scope(scope: &Scope) -> CapturedScope {
    CapturedScope {
        values: scope.flatten(),
        declared: scope.flatten_declared(),
        body_members: HashSet::new(),
        module_identities: scope.flatten_module_identities(),
        type_aliases: scope.flatten_type_aliases(),
        type_namespace: scope.type_namespace.clone(),
    }
}

pub(super) fn capture_object_source_scope(source: &ObjectSource) -> CapturedScope {
    let type_namespace = source
        .type_name
        .as_deref()
        .zip(source.type_identity.as_deref())
        .and_then(|(name, identity)| identity.strip_suffix(&format!(".{name}")))
        .map(str::to_owned);
    CapturedScope {
        values: source.scope.clone(),
        declared: source.scope_declared.clone(),
        body_members: source.body_members.clone(),
        module_identities: source.scope_module_identities.clone(),
        type_aliases: source.scope_type_aliases.clone(),
        type_namespace,
    }
}

pub(super) fn restore_scope(captured: &CapturedScope) -> Scope {
    let mut scope = Scope {
        type_namespace: captured.type_namespace.clone(),
        ..Scope::default()
    };
    for (name, value) in &captured.values {
        scope.set_name(name.clone(), value.clone());
    }
    scope.declared = Rc::new(captured.declared.clone());
    for (name, identity) in &captured.module_identities {
        scope.set_module_identity(name.clone(), identity.clone());
    }
    for (name, ty) in &captured.type_aliases {
        scope.set_type_alias(name.clone(), ty.clone());
    }
    scope
}

/// `own_body` is the scope the object was defined in and its own body's member
/// names, when some entries come from elsewhere (a parent class or an
/// amendment). Entries of the object's own body then also resolve names
/// declared around that definition before members the object got elsewhere.
pub(super) fn scope_for_object_entry(
    entry_index: usize,
    object: &Scope,
    entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
    entry_owners: &EntryOwners,
    own_body: Option<(&Scope, &HashSet<String>)>,
) -> Scope {
    if let Some(captured) = entry_scopes
        .and_then(|scopes| scopes.get(entry_index))
        .and_then(Option::as_ref)
    {
        let lexical = entry_owners.restored(captured);
        let none = HashSet::new();
        let owned = entry_owners
            .owners
            .get(entry_index)
            .map_or(&none, Rc::as_ref);
        return entry_owners.bindings(Arc::as_ptr(captured) as usize, &lexical, object, owned);
    }
    match own_body {
        Some((definition, owned)) => entry_owners.bindings(
            definition as *const Scope as usize,
            definition,
            object,
            owned,
        ),
        None => object.clone(),
    }
}

/// Per-entry lexical context for evaluating an object's entries.
#[derive(Default)]
pub(super) struct EntryOwners {
    /// For each entry, the names declared by its body; see
    /// [`entry_scope_owners`].
    owners: Vec<Rc<HashSet<String>>>,
    /// Captured scopes already restored, keyed by their address. Entries from
    /// one body share a captured scope, so each is restored once rather than
    /// once per entry.
    restored: RefCell<HashMap<*const CapturedScope, Scope>>,
    /// The last entry scope built for each lexical scope, keyed by the
    /// lexical scope's address.
    /// The object only gains or rebinds members between entries, so the next
    /// entry's scope is that one updated in place rather than rebuilt.
    bindings: RefCell<HashMap<usize, ObjectBindings>>,
}

/// An entry scope built over an object, kept for updating.
struct ObjectBindings {
    scope: Scope,
    /// Object members hidden because the lexical scope declares the name.
    hidden: FxHashSet<Name>,
}

impl EntryOwners {
    /// The entry scope over `object` for the lexical scope identified by
    /// `key` (see [`update_object_bindings`]), updated from the previous
    /// call's result.
    fn bindings(
        &self,
        key: usize,
        lexical: &Scope,
        object: &Scope,
        owned: &HashSet<String>,
    ) -> Scope {
        let mut cache = self.bindings.borrow_mut();
        let cached = cache.entry(key).or_insert_with(|| ObjectBindings {
            scope: lexical.child(),
            hidden: FxHashSet::default(),
        });
        update_object_bindings(cached, lexical, object, owned);
        cached.scope.clone()
    }

    /// Drop the cached entry scopes' references to the object's current
    /// `this` snapshot, like `release_this_aliases` does for the object scope,
    /// so the property map can grow in place. The next entry rebinds them.
    ///
    /// A cached scope still shared with a live entry scope (as in a `for`
    /// generator, whose scope outlives each iteration) is left alone: the live
    /// scope holds the snapshot regardless, and releasing would only copy the
    /// cached bindings.
    pub(super) fn release_this(&self, aliases: &[String]) {
        for cached in self.bindings.borrow_mut().values_mut() {
            if Arc::strong_count(&cached.scope.vars) == 1 {
                release_this_aliases(&mut cached.scope, aliases);
            }
        }
    }

    /// Whether the scope `scope_for_object_entry` builds for this entry
    /// resolves `name` lexically rather than to the object's member: the
    /// entry's lexical scope declares it and the entry's own body does not.
    /// This is the rule `update_object_bindings` applies.
    pub(super) fn hides_member(
        &self,
        entry_index: usize,
        entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
        own_body: Option<(&Scope, &HashSet<String>)>,
        name: &str,
    ) -> bool {
        if let Some(captured) = entry_scopes
            .and_then(|scopes| scopes.get(entry_index))
            .and_then(Option::as_ref)
        {
            let owned = self
                .owners
                .get(entry_index)
                .is_some_and(|owned| owned.contains(name));
            return !owned && self.restored(captured).is_declared(name);
        }
        match own_body {
            Some((definition, owned)) => !owned.contains(name) && definition.is_declared(name),
            None => false,
        }
    }

    fn restored(&self, captured: &Arc<CapturedScope>) -> Scope {
        self.restored
            .borrow_mut()
            .entry(Arc::as_ptr(captured))
            .or_insert_with(|| restore_scope(captured))
            .clone()
    }
}

/// For each entry evaluated in a captured lexical scope, the names declared by
/// the entries written in the same body (those sharing that captured scope).
/// `inherited` is the object being amended: its entries count too, since a body
/// still declares a member that a later amendment replaces.
pub(super) fn entry_scope_owners(
    entries: &[Entry],
    entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
    inherited: Option<&ObjectSource>,
) -> EntryOwners {
    let Some(entry_scopes) = entry_scopes else {
        return EntryOwners::default();
    };
    let inherited_entries = inherited
        .map(|source| source.entries.iter().zip(&source.entry_scopes))
        .into_iter()
        .flatten();
    let mut groups: HashMap<*const CapturedScope, HashSet<String>> = HashMap::new();
    for (entry, captured) in entries.iter().zip(entry_scopes).chain(inherited_entries) {
        let Some(captured) = captured else {
            continue;
        };
        let names = groups.entry(Arc::as_ptr(captured)).or_insert_with(|| {
            // Members of this body that a later amendment replaced.
            captured.body_members.clone()
        });
        if let Some(name) = entry_member_name(entry) {
            names.insert(name.clone());
        }
    }
    let groups = groups
        .into_iter()
        .map(|(ptr, names)| (ptr, Rc::new(names)))
        .collect::<HashMap<_, _>>();
    let owners = entry_scopes
        .iter()
        .map(|captured| {
            captured
                .as_ref()
                .and_then(|captured| groups.get(&Arc::as_ptr(captured)))
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    EntryOwners {
        owners,
        restored: RefCell::default(),
        bindings: RefCell::default(),
    }
}

/// The member name an entry declares in its body, if any.
pub(super) fn entry_member_name(entry: &Entry) -> Option<&String> {
    match entry {
        Entry::Property(prop) => Some(&prop.name),
        Entry::ClassDef(name, ..) | Entry::TypeAlias(name, _) => Some(name),
        _ => None,
    }
}

/// Names declared by the object's own definition body: the entries without a
/// captured scope, including ones a later amendment replaced. `None` when the
/// entries have no captured scopes at all, so every entry is the object's own.
pub(super) fn own_body_names(
    entries: &[Entry],
    entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
    inherited: Option<&ObjectSource>,
) -> Option<HashSet<String>> {
    let entry_scopes = entry_scopes?;
    let own = |entries: &[Entry], scopes: &[Option<Arc<CapturedScope>>]| {
        entries
            .iter()
            .enumerate()
            .filter(|(index, _)| scopes.get(*index).is_none_or(Option::is_none))
            .filter_map(|(_, entry)| entry_member_name(entry).cloned())
            .collect::<Vec<_>>()
    };
    let mut names: HashSet<String> = own(entries, entry_scopes).into_iter().collect();
    if let Some(source) = inherited {
        names.extend(own(&source.entries, &source.entry_scopes));
        // Members an earlier amendment replaced are no longer in `entries`.
        names.extend(source.body_members.iter().cloned());
    }
    Some(names)
}

/// Evaluate an object entry in its lexical module while keeping the locals and
/// sibling properties accumulated for the object instance itself.
///
/// `owned` names the members declared by the body the entry was written in.
/// The object's other members are inherited, and Pkl only resolves those
/// through implicit `this` after the lexically enclosing bodies, so a name
/// declared in an enclosing body (such as a `local` of an outer object) is not
/// shadowed by an inherited member of the same name.
///
/// The entry scope is a child of `lexical` holding the object's members, and
/// is brought up to date with `object` in place: an object only gains members
/// or rebinds them, so members already bound to the same value are left alone.
fn update_object_bindings(
    bindings: &mut ObjectBindings,
    lexical: &Scope,
    object: &Scope,
    owned: &HashSet<String>,
) {
    let ObjectBindings { scope, hidden } = bindings;
    scope.receiver_entries = object.receiver_entries.clone();
    scope.receiver_list_base = object.receiver_list_base;
    // The scope's own bindings come only from the object, so they can be
    // written directly instead of going through `declare`/`set`, which also
    // clear stale poison and module identities for each name.
    for (name, value) in object.vars.iter() {
        // `super` belongs to the body that declared the entry. A later
        // amendment must not replace an inherited entry's parent binding.
        if &**name == "super" || hidden.contains(&**name) {
            continue;
        }
        if scope
            .vars
            .get(&**name)
            .is_some_and(|bound| same_value(bound, value))
        {
            continue;
        }
        if owned.contains(&**name) {
            if !scope.declared.contains(&**name) {
                Rc::make_mut(&mut scope.declared).insert(name.clone());
            }
        } else if !scope.vars.contains_key(&**name) && lexical.is_declared(name) {
            // `lexical` is fixed for this cache entry and `owned` for its key,
            // so a hidden name stays hidden for the life of the cache.
            hidden.insert(name.clone());
            continue;
        }
        Arc::make_mut(&mut scope.vars).insert(name.clone(), value.clone());
    }
    // Rebuilt whenever either side has poisoned names, so names no longer
    // poisoned on the object are dropped from the scope.
    if !object.poisoned.is_empty() || !scope.poisoned.is_empty() {
        let mut poisoned = FxIndexMap::default();
        for (name, message) in object.poisoned.iter() {
            if owned.contains(&**name) {
                if !scope.declared.contains(&**name) {
                    Rc::make_mut(&mut scope.declared).insert(name.clone());
                }
                poisoned.insert(name.clone(), message.clone());
            } else if !lexical.is_declared(name) {
                poisoned.insert(name.clone(), message.clone());
            }
        }
        scope.poisoned = Rc::new(poisoned);
    }
    scope.this_aliases = object.this_aliases.clone();
    // The scope's own maps for these start empty, so it can share the object's.
    scope.type_aliases = object.type_aliases.clone();
    scope.shadows_builtin_type |= object.shadows_builtin_type;
    scope.defining_class |= object.defining_class;
    scope.module_identities = object.module_identities.clone();
}

/// Whether two values are the same binding, compared by identity where a
/// value is shared. A `false` only costs rebinding an equal value.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Object(a, a_src), Value::Object(b, b_src)) => {
            Arc::ptr_eq(a, b)
                && match (a_src, b_src) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                }
        }
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
        }
        (
            Value::Lambda(a_params, a_body, a_captured),
            Value::Lambda(b_params, b_body, b_captured),
        ) => {
            Arc::ptr_eq(a_params, b_params)
                && Arc::ptr_eq(a_body, b_body)
                && Arc::ptr_eq(a_captured, b_captured)
        }
        _ => false,
    }
}

/// Keep inherited entries bound to the imports they captured while letting an
/// amendment resolve names in the scope where the amendment was declared.
/// Both scopes keep which of their names were declared in a body, so an entry's
/// inherited members still resolve after those names.
pub(super) fn mapping_amendment_scopes(
    captured: &ScopeMap,
    captured_declared: &NameSet,
    current: &Scope,
) -> (Scope, Scope) {
    let current_declared = current.flatten_declared();
    let mut inherited = Scope::default();
    for (key, value) in captured {
        if captured_declared.contains(&**key) {
            inherited.declare_name(key.clone(), value.clone());
        } else {
            inherited.set_name(key.clone(), value.clone());
        }
    }
    for (key, value) in current.flatten() {
        if inherited.get(&key).is_none() {
            if current_declared.contains(&*key) {
                inherited.declare_name(key, value);
            } else {
                inherited.set_name(key, value);
            }
        }
    }
    for (key, ty) in current.flatten_type_aliases() {
        inherited.set_type_alias(key, ty);
    }

    let mut amendment = inherited.clone();
    for (key, value) in current.flatten() {
        if current_declared.contains(&*key) {
            amendment.declare_name(key, value);
        } else {
            Rc::make_mut(&mut amendment.declared).remove(&*key);
            amendment.set_name(key, value);
        }
    }
    for (key, ty) in current.flatten_type_aliases() {
        amendment.set_type_alias(key, ty);
    }
    (inherited, amendment)
}
