use super::*;
use std::cell::RefCell;

use rustc_hash::{FxBuildHasher, FxHashSet};

pub(super) type FxIndexMap<K, V> = IndexMap<K, V, FxBuildHasher>;

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
    pub(super) vars: Rc<FxIndexMap<String, Value>>,
    pub(super) type_aliases: Rc<FxIndexMap<String, crate::parser::TypeExpr>>,
    pub(super) module_identities: Rc<FxIndexMap<String, String>>,
    pub(super) poisoned: Rc<FxIndexMap<String, String>>,
    /// Names in `vars` or `poisoned` declared by an entry written in the body
    /// that owns this scope, as opposed to members an object inherits from its
    /// class or parent. Pkl resolves a name in lexically enclosing bodies
    /// before falling back to the object's inherited members, so only these
    /// declared names may win over an inherited member of an inner object.
    pub(super) declared: Rc<FxHashSet<String>>,
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
    pub(super) fn child(&self) -> Self {
        Self {
            vars: Rc::default(),
            type_aliases: Rc::default(),
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
        if self.poisoned.contains_key(&name) {
            Rc::make_mut(&mut self.poisoned).shift_remove(&name);
        }
        if self.module_identities.contains_key(&name) {
            Rc::make_mut(&mut self.module_identities).shift_remove(&name);
        }
        if self.this_aliases.contains(&name) {
            Rc::make_mut(&mut self.this_aliases).remove(&name);
        }
        Rc::make_mut(&mut self.vars).insert(name, val);
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
        let mut names = Vec::new();
        let mut level = Some(self);
        while let Some(scope) = level {
            for name in scope.this_aliases.iter() {
                if !names.contains(name) && self.is_this_alias(name) {
                    names.push(name.clone());
                }
            }
            level = scope.parent.as_deref();
        }
        names
    }

    fn is_this_alias(&self, name: &str) -> bool {
        if self.vars.contains_key(name) || self.poisoned.contains_key(name) {
            self.this_aliases.contains(name)
        } else {
            self.parent
                .as_ref()
                .is_some_and(|parent| parent.is_this_alias(name))
        }
    }

    /// Bind a name declared in the body that owns this scope.
    pub(super) fn declare(&mut self, name: String, val: Value) {
        if !self.declared.contains(&name) {
            Rc::make_mut(&mut self.declared).insert(name.clone());
        }
        self.set(name, val);
    }

    /// Poison a local declared in the body that owns this scope.
    pub(super) fn declare_poisoned(&mut self, name: String, message: String) {
        if !self.declared.contains(&name) {
            Rc::make_mut(&mut self.declared).insert(name.clone());
        }
        self.poison(name, message);
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

    pub(super) fn flatten_declared(&self) -> HashSet<String> {
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
        Rc::make_mut(&mut self.module_identities).insert(name, identity);
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
            identities.shift_remove(name);
        }
        identities.extend(
            self.module_identities
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        identities
    }

    pub(super) fn poison(&mut self, name: String, message: String) {
        Rc::make_mut(&mut self.poisoned).insert(name, message);
    }

    pub(super) fn poison_of(&self, name: &str) -> Option<&String> {
        self.poisoned.get(name).or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.poison_of(name))
        })
    }

    pub(super) fn set_type_alias(&mut self, name: String, ty: crate::parser::TypeExpr) {
        Rc::make_mut(&mut self.type_aliases).insert(name, ty);
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

    pub(super) fn flatten(&self) -> IndexMap<String, Value> {
        let mut result = self
            .parent
            .as_ref()
            .map(|p| p.flatten())
            .unwrap_or_default();
        for name in self.poisoned.keys() {
            result.shift_remove(name);
        }
        result.extend(self.vars.iter().map(|(k, v)| (k.clone(), v.clone())));
        result
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
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        result
    }
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
        scope.set(name.clone(), value.clone());
    }
    scope.declared = Rc::new(captured.declared.iter().cloned().collect());
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
        return scope_with_object_bindings(&lexical, object, owned);
    }
    match own_body {
        Some((definition, owned)) => scope_with_object_bindings(definition, object, owned),
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
}

impl EntryOwners {
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
pub(super) fn scope_with_object_bindings(
    lexical: &Scope,
    object: &Scope,
    owned: &HashSet<String>,
) -> Scope {
    let mut scope = lexical.child();
    scope.receiver_entries = object.receiver_entries.clone();
    scope.receiver_list_base = object.receiver_list_base;
    // `scope` starts empty, so the bindings can be collected directly instead
    // of going through `declare`/`set`, which also clear stale poison and
    // module identities for each name.
    let mut vars = FxIndexMap::with_capacity_and_hasher(object.vars.len(), FxBuildHasher);
    let mut declared = FxHashSet::default();
    for (name, value) in object.vars.iter() {
        // `super` belongs to the body that declared the entry. A later
        // amendment must not replace an inherited entry's parent binding.
        if name == "super" {
            continue;
        }
        if owned.contains(name) {
            declared.insert(name.clone());
            vars.insert(name.clone(), value.clone());
        } else if !lexical.is_declared(name) {
            vars.insert(name.clone(), value.clone());
        }
    }
    scope.vars = Rc::new(vars);
    scope.declared = Rc::new(declared);
    scope.this_aliases = object.this_aliases.clone();
    for (name, message) in object.poisoned.iter() {
        if owned.contains(name) {
            scope.declare_poisoned(name.clone(), message.clone());
        } else if !lexical.is_declared(name) {
            scope.poison(name.clone(), message.clone());
        }
    }
    for (name, ty) in object.type_aliases.iter() {
        scope.set_type_alias(name.clone(), ty.clone());
    }
    for (name, identity) in object.module_identities.iter() {
        scope.set_module_identity(name.clone(), identity.clone());
    }
    scope
}

/// Keep inherited entries bound to the imports they captured while letting an
/// amendment resolve names in the scope where the amendment was declared.
/// Both scopes keep which of their names were declared in a body, so an entry's
/// inherited members still resolve after those names.
pub(super) fn mapping_amendment_scopes(
    captured: &IndexMap<String, Value>,
    captured_declared: &HashSet<String>,
    current: &Scope,
) -> (Scope, Scope) {
    let current_declared = current.flatten_declared();
    let mut inherited = Scope::default();
    for (key, value) in captured {
        if captured_declared.contains(key) {
            inherited.declare(key.clone(), value.clone());
        } else {
            inherited.set(key.clone(), value.clone());
        }
    }
    for (key, value) in current.flatten() {
        if inherited.get(&key).is_none() {
            if current_declared.contains(&key) {
                inherited.declare(key, value);
            } else {
                inherited.set(key, value);
            }
        }
    }
    for (key, ty) in current.flatten_type_aliases() {
        inherited.set_type_alias(key, ty);
    }

    let mut amendment = inherited.clone();
    for (key, value) in current.flatten() {
        if current_declared.contains(&key) {
            amendment.declare(key, value);
        } else {
            Rc::make_mut(&mut amendment.declared).remove(&key);
            amendment.set(key, value);
        }
    }
    for (key, ty) in current.flatten_type_aliases() {
        amendment.set_type_alias(key, ty);
    }
    (inherited, amendment)
}
