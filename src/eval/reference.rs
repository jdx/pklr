//! The `pkl:ref` module: references to a value of a known type.
//!
//! A `Reference` records a domain, the data it refers into and the path from
//! the data to the referent. Reading a property or subscripting a reference
//! gives a reference to the member, whose type is looked up from the
//! declaration of the property (or the element type of a collection).
//!
//! A reference is an object of [`ObjectKind::Reference`] whose members are
//! internal (their names are not identifiers): the domain, the data, the path
//! of `Access` elements and the referent type. The referent type is kept
//! normalized (see [`RType`]) in a table of the evaluator; the object holds
//! its index and its text.

use super::amend::definition_type_scope;
use super::scope::{object_source_type_namespace, restore_scope};
use super::stdlib::typed_object;
use super::typecheck::generic_alias_key;
use super::types::{resolve_dotted, stdlib_alias_source};
use super::*;
use crate::parser::TypeExpr;

pub(crate) const DOMAIN_KEY: &str = "\0domain";
pub(crate) const DATA_KEY: &str = "\0data";
pub(crate) const PATH_KEY: &str = "\0path";
const TYPE_KEY: &str = "\0type";
pub(crate) const TYPE_TEXT_KEY: &str = "\0typetext";

/// A referent type, normalized as pkl does: constraints are erased, `T?` is
/// `T | Null`, aliases are replaced by what they stand for, unions are
/// flattened and ordered by their text, and `unknown` and `Any` absorb every
/// other alternative.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum RType {
    Unknown,
    Nothing,
    /// A string literal type.
    Str(Arc<str>),
    Class {
        id: Arc<str>,
        name: Arc<str>,
        args: Vec<RType>,
    },
    Union(Vec<RType>),
}

impl std::fmt::Display for RType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RType::Unknown => f.write_str("unknown"),
            RType::Nothing => f.write_str("nothing"),
            RType::Str(s) => write!(f, "\"{s}\""),
            RType::Class { name, args, .. } => {
                f.write_str(name)?;
                if !args.is_empty() {
                    f.write_str("<")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    f.write_str(">")?;
                }
                Ok(())
            }
            RType::Union(items) => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }
                    write!(f, "{item}")?;
                }
                Ok(())
            }
        }
    }
}

impl RType {
    fn base(name: &str) -> RType {
        RType::Class {
            id: format!("pkl:base#{name}").into(),
            name: name.into(),
            args: Vec::new(),
        }
    }

    fn is_base(&self, name: &str) -> bool {
        matches!(self, RType::Class { id, .. }
            if id.strip_prefix("pkl:base#") == Some(name))
    }
}

/// The evaluator's table of referent types and the classes they name.
#[derive(Default)]
pub(super) struct RefState {
    /// Whether `pkl:ref` was imported: only then are classes recorded.
    pub(super) active: bool,
    types: Vec<RType>,
    ids: HashMap<RType, usize>,
    classes: HashMap<Arc<str>, Value>,
    /// Set when a type being normalized has a constraint, which a referent
    /// type argument may not have.
    saw_constraint: bool,
    /// The type parameters of the generic alias being expanded, bound to
    /// their arguments: the alias body resolves in its module, its arguments
    /// in the caller's.
    bindings: HashMap<String, RType>,
}

impl RefState {
    /// Frees the classes, which hold value trees that free recursively.
    #[cfg(feature = "native-io")]
    pub(super) fn release(&mut self) {
        self.classes.clear();
    }
}

const BASE_CLASSES: &[&str] = &[
    "Any",
    "Null",
    "Boolean",
    "String",
    "Int",
    "Float",
    "Number",
    "Duration",
    "DataSize",
    "List",
    "Set",
    "Map",
    "Collection",
    "Listing",
    "Mapping",
    "Dynamic",
    "Object",
    "Pair",
    "Regex",
    "IntSeq",
    "Typed",
    "Module",
    "Class",
    "Function",
    "Function0",
    "Function1",
    "Function2",
    "Function3",
    "Function4",
    "Function5",
];

/// The `Int` aliases pkl keeps (rather than replaces by `Int`) when it
/// normalizes a type, with their ranges.
fn int_alias_range(name: &str) -> Option<(i128, i128)> {
    Some(match name {
        "Int8" => (-128, 127),
        "Int16" => (-32768, 32767),
        "Int32" => (-2147483648, 2147483647),
        "UInt8" => (0, 255),
        "UInt16" => (0, 65535),
        "UInt32" => (0, 4294967295),
        "UInt" => (0, i64::MAX as i128),
        _ => return None,
    })
}

fn base_parent(name: &str) -> Option<&'static str> {
    Some(match name {
        "Any" => return None,
        "Int" | "Float" => "Number",
        "Int8" | "Int16" | "Int32" | "UInt8" | "UInt16" | "UInt32" | "UInt" => "Int",
        "Typed" => "Object",
        "Module" => "Typed",
        "List" | "Set" => "Collection",
        "Listing" | "Mapping" | "Dynamic" => "Object",
        _ => "Any",
    })
}

fn is_reference_class(value: &Value) -> bool {
    matches!(value, Value::Object(_, Some(source))
        if source.kind == ObjectKind::Class
            && source.type_name.as_deref() == Some("Reference")
            && source
                .type_identity
                .as_deref()
                .is_some_and(|id| id.starts_with("pkl:ref")))
}

/// Whether `name`, written in `scope`, is the class `pkl:ref`'s `Reference`.
pub(super) fn names_reference_class(scope: &Scope, name: &str) -> bool {
    resolve_dotted(scope, name).is_some_and(|class| is_reference_class(&class))
}

fn reference_source(value: &Value) -> Option<&Arc<ObjectSource>> {
    match value {
        Value::Object(_, Some(source)) if source.kind == ObjectKind::Reference => Some(source),
        _ => None,
    }
}

fn error(message: impl Into<String>) -> Error {
    Error::Eval(message.into())
}

fn access(property: Option<&str>, key: Option<Value>) -> Value {
    let mut members = ObjectMap::default();
    members.insert(
        "property".into(),
        property.map_or(Value::Null, |p| Value::String(p.into())),
    );
    members.insert("key".into(), key.clone().unwrap_or(Value::Null));
    members.insert("isProperty".into(), Value::Bool(property.is_some()));
    members.insert("isSubscript".into(), Value::Bool(property.is_none()));
    let Value::Object(members, Some(source)) = typed_object("Access", members) else {
        unreachable!("typed_object builds an object with a source")
    };
    let mut source = Arc::unwrap_or_clone(source);
    source.type_identity = Some("pkl:ref.Access".into());
    Value::Object(members, Some(Arc::new(source)))
}

/// A union of `items`, normalized.
fn make_union(items: Vec<RType>) -> RType {
    let mut flat = Vec::new();
    for item in items {
        match item {
            RType::Union(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    if flat.contains(&RType::Unknown) {
        return RType::Unknown;
    }
    if flat.iter().any(|t| t.is_base("Any")) {
        return RType::base("Any");
    }
    flat.sort_by_cached_key(ToString::to_string);
    flat.dedup();
    match flat.len() {
        0 => RType::Nothing,
        1 => flat.remove(0),
        _ => RType::Union(flat),
    }
}

impl Evaluator {
    fn class_parents(&self, id: &str) -> Vec<Arc<str>> {
        match self.refs.classes.get(id) {
            Some(Value::Object(_, Some(source))) => source
                .parent_type_identities
                .iter()
                .map(|parent| Arc::from(parent.as_str()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Records the class a module declares, which a referent type can name
    /// before the class is bound where the type is written.
    pub(super) fn register_reference_class(&mut self, class: &Value) {
        if !self.refs.active {
            return;
        }
        if let Value::Object(_, Some(source)) = class
            && source.kind == ObjectKind::Class
            && let Some(id) = source.type_identity.as_deref()
        {
            self.refs.classes.insert(id.into(), class.clone());
        }
    }

    fn class_type(&mut self, class: &Value) -> Option<RType> {
        let Value::Object(_, Some(source)) = class else {
            return None;
        };
        if source.kind != ObjectKind::Class {
            return None;
        }
        let id: Arc<str> = source.type_identity.as_deref()?.into();
        if let Some(name) = id.strip_prefix("pkl:base#") {
            return Some(RType::base(name));
        }
        let name = super::stdlib::render::class_display_name(source);
        // A class that names itself in a default is known by its identity
        // only; the module records the whole class once it is declared.
        if !source.entries.is_empty() || !self.refs.classes.contains_key(&id) {
            self.refs.classes.insert(id.clone(), class.clone());
        }
        Some(RType::Class {
            id,
            name: name.into(),
            args: Vec::new(),
        })
    }

    /// The declaring scope and member name of the generic alias `m.Name`
    /// that `scope` imports as `m`.
    fn imported_generic_scope<'n>(
        &mut self,
        scope: &Scope,
        name: &'n str,
    ) -> Option<(Scope, &'n str)> {
        let (root, member) = name.split_once('.')?;
        scope.get_type_alias(&format!("{root}#{member}"))?;
        let identity = scope.module_identity(root).cloned()?;
        let declaring = self.module_type_scope(&identity)?;
        Some((declaring, member))
    }

    /// The normalized form of the type `ty` written in `scope`.
    pub(super) fn normalize_type(&mut self, ty: &TypeExpr, scope: &Scope) -> RType {
        let mut out = Vec::new();
        self.normalize_into(ty, scope, &mut out, 0);
        make_union(out)
    }

    fn normalize_into(&mut self, ty: &TypeExpr, scope: &Scope, out: &mut Vec<RType>, depth: usize) {
        if depth > 32 {
            out.push(RType::Unknown);
            return;
        }
        match ty {
            TypeExpr::Nullable(inner) => {
                self.normalize_into(inner, scope, out, depth + 1);
                out.push(RType::base("Null"));
            }
            TypeExpr::Union(items) => {
                for item in items {
                    self.normalize_into(item, scope, out, depth + 1);
                }
            }
            TypeExpr::Constrained(base, ..) => {
                self.refs.saw_constraint = true;
                match crate::parser::parse_type_name(base) {
                    Ok(base) if !matches!(base, TypeExpr::Constrained(..)) => {
                        self.normalize_into(&base, scope, out, depth + 1);
                    }
                    _ => out.push(RType::Unknown),
                }
            }
            TypeExpr::Generic(name, args) => {
                let name = name.trim_start_matches('*');
                let (declaring, alias_name) = match self.inherited_alias(scope, name) {
                    Some((_, module_scope)) => (module_scope, name),
                    None => match self.imported_generic_scope(scope, name) {
                        Some((module_scope, member)) => (module_scope, member),
                        None => (scope.clone(), name),
                    },
                };
                if let Some(TypeExpr::Generic(params, written)) =
                    declaring.get_type_alias(&generic_alias_key(alias_name))
                    && let Some(body) = written.first()
                {
                    let params: Vec<&str> = params.split(',').collect();
                    if params.len() == args.len() {
                        let mut frame = HashMap::default();
                        for (param, arg) in params.iter().zip(args) {
                            let ty = self.normalize_type(arg, scope);
                            frame.insert((*param).to_string(), ty);
                        }
                        // The body sees only its own parameters, bound to
                        // the arguments as the caller's scope resolves them.
                        let outer = std::mem::replace(&mut self.refs.bindings, frame);
                        self.normalize_into(body, &declaring, out, depth + 1);
                        self.refs.bindings = outer;
                        return;
                    }
                }
                let member = name.rsplit_once('.').map_or(name, |(_, member)| member);
                if BASE_CLASSES.contains(&member) || names_reference_class(scope, name) {
                    let args = args
                        .iter()
                        .map(|arg| self.normalize_type(arg, scope))
                        .collect();
                    let (id, display): (Arc<str>, &str) = if member == "Reference" {
                        ("pkl:ref.Reference".into(), "Reference")
                    } else {
                        (format!("pkl:base#{member}").into(), member)
                    };
                    out.push(RType::Class {
                        id,
                        name: display.into(),
                        args,
                    });
                } else {
                    out.push(RType::Unknown);
                }
            }
            TypeExpr::Named(name) => {
                let name = name.trim_start_matches('*');
                self.normalize_named(name, scope, out, depth);
            }
        }
    }

    /// Normalizes the body of a type alias that is not the generic alias being
    /// expanded: the parameters of that alias are not names in this body.
    fn normalize_alias_body(
        &mut self,
        body: &TypeExpr,
        scope: &Scope,
        out: &mut Vec<RType>,
        depth: usize,
    ) {
        let outer = std::mem::take(&mut self.refs.bindings);
        self.normalize_into(body, scope, out, depth + 1);
        self.refs.bindings = outer;
    }

    fn normalize_named(&mut self, name: &str, scope: &Scope, out: &mut Vec<RType>, depth: usize) {
        if let Some(bound) = self.refs.bindings.get(name) {
            out.push(bound.clone());
            return;
        }
        if let Some(literal) = name
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
        {
            out.push(RType::Str(literal.into()));
            return;
        }
        match name {
            "unknown" => return out.push(RType::Unknown),
            "nothing" => return out.push(RType::Nothing),
            _ => {}
        }
        if let Some((resolved, alias_scope)) = self.inherited_alias(scope, name) {
            return self.normalize_alias_body(&resolved, &alias_scope, out, depth);
        }
        if let Some(resolved) = scope.get_type_alias(name) {
            let resolved = resolved.clone();
            return self.normalize_alias_body(&resolved, scope, out, depth);
        }
        if let Some((resolved, alias_scope)) = self.imported_alias(scope, name) {
            return self.normalize_alias_body(&resolved, &alias_scope, out, depth);
        }
        if let Some(class) = resolve_dotted(scope, name)
            && let Some(ty) = self.class_type(&class)
        {
            return out.push(ty);
        }
        if int_alias_range(name).is_some() {
            return out.push(RType::base(name));
        }
        if let Some(source) = stdlib_alias_source(name)
            && let Ok(resolved) = crate::parser::parse_type_name(source)
        {
            return self.normalize_alias_body(&resolved, &Scope::default(), out, depth);
        }
        let member = name.rsplit_once('.').map_or(name, |(_, member)| member);
        if BASE_CLASSES.contains(&member) {
            return out.push(RType::base(member));
        }
        out.push(RType::Unknown);
    }

    fn is_subclass(&self, sub: &str, sup: &str) -> bool {
        if sub == sup || sup == "pkl:base#Any" {
            return true;
        }
        if let Some(name) = sub.strip_prefix("pkl:base#") {
            let mut current = name;
            while let Some(parent) = base_parent(current) {
                if sup == format!("pkl:base#{parent}") {
                    return true;
                }
                current = parent;
            }
            return false;
        }
        // A class a module declares is a `Typed` and an `Object`.
        matches!(sup, "pkl:base#Typed" | "pkl:base#Object")
            || self.class_parents(sub).iter().any(|p| &**p == sup)
    }

    /// The type arguments `sub` has as an instance of its ancestor `sup`:
    /// a `List<E>` is a `Collection<E>`, and other ancestors have none.
    /// The type arguments of `sub<args>` seen as its ancestor `sup`, or `None`
    /// when an ancestor on the way takes none of them.
    fn upcast_args(sub: &str, args: &[RType], sup: &str) -> Option<Vec<RType>> {
        if sub == sup {
            return Some(args.to_vec());
        }
        let Some(mut current) = sub.strip_prefix("pkl:base#") else {
            return Some(Vec::new());
        };
        let args = args.to_vec();
        while format!("pkl:base#{current}") != sup {
            let Some(parent) = base_parent(current) else {
                return Some(Vec::new());
            };
            if !matches!((current, parent), ("List" | "Set", "Collection")) && !args.is_empty() {
                return None;
            }
            current = parent;
        }
        Some(args)
    }

    pub(super) fn is_subtype(&self, sub: &RType, sup: &RType) -> bool {
        if sub == sup {
            return true;
        }
        if *sup == RType::Nothing {
            return false;
        }
        if *sub == RType::Unknown || *sup == RType::Unknown || sup.is_base("Any") {
            return true;
        }
        if let RType::Union(items) = sub {
            return items.iter().all(|item| self.is_subtype(item, sup));
        }
        if let RType::Union(items) = sup {
            return items.iter().any(|item| self.is_subtype(sub, item));
        }
        match (sub, sup) {
            (RType::Nothing, _) => true,
            (RType::Str(_), RType::Class { .. }) => sup.is_base("String"),
            (RType::Str(_), _) | (_, RType::Str(_)) | (_, RType::Nothing) => false,
            (
                RType::Class {
                    id: a_id, args: a, ..
                },
                RType::Class {
                    id: b_id, args: b, ..
                },
            ) => {
                if let (Some(a_name), Some(b_name)) = (
                    a_id.strip_prefix("pkl:base#"),
                    b_id.strip_prefix("pkl:base#"),
                ) && let (Some(a_range), Some(b_range)) = (
                    int_alias_range(a_name).or((a_name == "Int").then_some((i128::MIN, i128::MAX))),
                    int_alias_range(b_name).or((b_name == "Int").then_some((i128::MIN, i128::MAX))),
                ) {
                    return b_range.0 <= a_range.0 && a_range.1 <= b_range.1;
                }
                if int_alias_range(a_id.strip_prefix("pkl:base#").unwrap_or("")).is_some()
                    && b_id.as_ref() == "pkl:base#Int"
                {
                    return true;
                }
                if !self.is_subclass(a_id, b_id) {
                    return false;
                }
                // As pkl, type arguments are covariant (keys included) and a
                // raw class never relates to an applied one.
                let Some(a) = Self::upcast_args(a_id, a, b_id) else {
                    return false;
                };
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| self.is_subtype(x, y))
            }
            _ => false,
        }
    }

    fn intern(&mut self, ty: RType) -> usize {
        if let Some(id) = self.refs.ids.get(&ty) {
            return *id;
        }
        let id = self.refs.types.len();
        self.refs.types.push(ty.clone());
        self.refs.ids.insert(ty, id);
        id
    }

    fn reference_type(&self, value: &Value) -> Option<RType> {
        let Value::Object(members, Some(source)) = value else {
            return None;
        };
        if source.kind != ObjectKind::Reference {
            return None;
        }
        match members.get(TYPE_KEY) {
            Some(Value::Int(id)) => self.refs.types.get(usize::try_from(*id).ok()?).cloned(),
            _ => None,
        }
    }

    fn build_reference(
        &mut self,
        source: &Arc<ObjectSource>,
        domain: Value,
        data: Value,
        path: Vec<Value>,
        ty: RType,
    ) -> Value {
        let text = ty.to_string();
        let id = self.intern(ty);
        let mut members = ObjectMap::default();
        members.insert(DOMAIN_KEY.into(), domain);
        members.insert(DATA_KEY.into(), data);
        members.insert(PATH_KEY.into(), Value::List(path.into()));
        members.insert(TYPE_KEY.into(), Value::Int(id as i64));
        members.insert(TYPE_TEXT_KEY.into(), Value::String(text.into()));
        Value::Object(Arc::new(members), Some(source.clone()))
    }

    /// `ref.Reference(domain, class, data)`, when `class` is `pkl:ref`'s
    /// `Reference` class.
    pub(super) fn reference_construct(
        &mut self,
        class: &Value,
        args: &[Value],
    ) -> Option<Result<Value>> {
        if !is_reference_class(class) {
            return None;
        }
        let Value::Object(_, Some(class_source)) = class else {
            return None;
        };
        let [domain, referent, data] = args else {
            return Some(Err(error(format!(
                "Expected 3 function arguments but got {}.",
                args.len()
            ))));
        };
        let is_domain = matches!(domain, Value::Object(_, Some(source))
            if source.parent_type_identities.iter().any(|id| id.starts_with("pkl:ref") && id.ends_with(".Domain")));
        if !is_domain {
            return Some(Err(error(format!(
                "Expected value of type `Domain`, but got type `{}`.",
                value_type_name(domain)
            ))));
        }
        let Some(ty) = self.class_type(referent) else {
            return Some(Err(error(format!(
                "Expected value of type `Class`, but got type `{}`.",
                value_type_name(referent)
            ))));
        };
        let mut source = typed_object("Reference", ObjectMap::default());
        let Value::Object(_, Some(src)) = &mut source else {
            return None;
        };
        let mut new_source = Arc::unwrap_or_clone(src.clone());
        new_source.kind = ObjectKind::Reference;
        new_source.type_identity = class_source.type_identity.clone();
        let new_source = Arc::new(new_source);
        Some(Ok(self.build_reference(
            &new_source,
            domain.clone(),
            data.clone(),
            Vec::new(),
            ty,
        )))
    }

    /// The type of the property `name` of the class `class`.
    fn property_type(&mut self, class: &Value, name: &str, scope: &Scope) -> Option<RType> {
        let Value::Object(_, Some(source)) = class else {
            return None;
        };
        let (index, prop) = source
            .entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, e)| match e {
                Entry::Property(prop) if &*prop.name == name && !prop.is_method => Some((i, prop)),
                _ => None,
            })?;
        let Some(ty) = &prop.type_ann else {
            return Some(RType::Unknown);
        };
        let ty_scope = match source.entry_scopes.get(index).and_then(Clone::clone) {
            Some(captured) => restore_scope(&captured),
            None if scope.type_namespace == object_source_type_namespace(source) => scope.clone(),
            None => definition_type_scope(source),
        };
        Some(self.normalize_type(ty, &ty_scope))
    }

    fn member_type(&mut self, ty: &RType, name: &str, scope: &Scope) -> Result<RType> {
        match ty {
            RType::Unknown => Ok(RType::Unknown),
            RType::Union(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.push(self.member_type(item, name, scope)?);
                }
                Ok(make_union(out))
            }
            RType::Class { id, .. } if id.as_ref() == "pkl:base#Null" => Ok(ty.clone()),
            RType::Class { id, .. } if id.as_ref() == "pkl:base#Dynamic" && name != "default" => {
                Ok(RType::Unknown)
            }
            RType::Class { id, .. } if !id.starts_with("pkl:base#") => {
                let class = self.refs.classes.get(id).cloned();
                match class.and_then(|class| self.property_type(&class, name, scope)) {
                    Some(ty) => Ok(ty),
                    None => Err(error(format!(
                        "Cannot find property `{name}` in object of type `{ty}`."
                    ))),
                }
            }
            _ => Err(error(format!(
                "Cannot find property `{name}` in object of type `{ty}`."
            ))),
        }
    }

    fn key_matches(&self, key: &Value, ty: &RType) -> bool {
        match ty {
            RType::Unknown => true,
            RType::Union(items) => items.iter().any(|item| self.key_matches(key, item)),
            RType::Str(s) => matches!(key, Value::String(k) if k == s),
            RType::Class { id, .. } => {
                let Some(name) = id.strip_prefix("pkl:base#") else {
                    return matches!(key, Value::Object(_, Some(source))
                        if source.type_identity.as_deref() == Some(&**id)
                            || source.parent_type_identities.iter().any(|p| p.as_str() == &**id));
                };
                // As pkl, a key is of exactly the key type's class: `Any`,
                // `Number` and `Collection` accept no `Int` or `List`.
                match key {
                    Value::Object(_, source) => {
                        let own = source.as_ref().and_then(|s| s.type_identity.as_deref());
                        own == Some(&**id)
                            || (own.is_none_or(|own| own.starts_with("pkl:"))
                                && super::render::kind_of(key).pkl_class(key) == name)
                    }
                    _ => value_type_name(key) == name,
                }
            }
            RType::Nothing => false,
        }
    }

    fn subscript_type(&mut self, ty: &RType, key: &Value) -> Result<RType> {
        match ty {
            RType::Unknown => Ok(RType::Unknown),
            RType::Union(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.push(self.subscript_type(item, key)?);
                }
                Ok(make_union(out))
            }
            RType::Class { id, args, .. } => match (id.as_ref(), key) {
                ("pkl:base#Null", _) => Ok(ty.clone()),
                ("pkl:base#Listing" | "pkl:base#List", Value::Int(_)) => {
                    Ok(args.first().cloned().unwrap_or(RType::Unknown))
                }
                ("pkl:base#Mapping" | "pkl:base#Map", _) => {
                    let Some(key_ty) = args.first() else {
                        return Ok(RType::Unknown);
                    };
                    if self.key_matches(key, key_ty) {
                        Ok(args.get(1).cloned().unwrap_or(RType::Unknown))
                    } else {
                        Err(error(format!(
                            "Cannot subscript a reference of type `{ty}` with this key."
                        )))
                    }
                }
                _ => Err(error(format!(
                    "Cannot subscript a reference of type `{ty}`."
                ))),
            },
            _ => Err(error(format!(
                "Cannot subscript a reference of type `{ty}`."
            ))),
        }
    }

    fn extended(&mut self, base: &Value, element: Value, ty: RType) -> Value {
        let Value::Object(members, Some(source)) = base else {
            return base.clone();
        };
        let mut path = match members.get(PATH_KEY) {
            Some(Value::List(items)) => items.iter().cloned().collect(),
            _ => Vec::new(),
        };
        path.push(element);
        let domain = members.get(DOMAIN_KEY).cloned().unwrap_or(Value::Null);
        let data = members.get(DATA_KEY).cloned().unwrap_or(Value::Null);
        self.build_reference(source, domain, data, path, ty)
    }

    /// `reference.name`, a reference to the property of the referent.
    pub(super) fn reference_member(
        &mut self,
        base: &Value,
        name: &str,
        scope: &Scope,
    ) -> Result<Value> {
        let Some(ty) = self.reference_type(base) else {
            return Err(error(format!(
                "Cannot find property `{name}` in reference."
            )));
        };
        let member = self.member_type(&ty, name, scope)?;
        Ok(self.extended(base, access(Some(name), None), member))
    }

    /// `reference[key]`.
    pub(super) fn reference_subscript(&mut self, base: &Value, key: &Value) -> Result<Value> {
        let Some(ty) = self.reference_type(base) else {
            return Err(error("Cannot subscript a malformed reference."));
        };
        let element = self.subscript_type(&ty, key)?;
        Ok(self.extended(base, access(None, Some(key.clone())), element))
    }

    /// The methods of `Reference`: `getDomain()`, `getData()`, `getPath()`.
    pub(super) fn reference_method(
        &self,
        obj: &Value,
        method: &str,
        arg_count: usize,
    ) -> Option<Result<Value>> {
        reference_source(obj)?;
        let Value::Object(members, _) = obj else {
            return None;
        };
        let key = match method {
            "getDomain" => DOMAIN_KEY,
            "getData" => DATA_KEY,
            "getPath" => PATH_KEY,
            _ => return None,
        };
        if arg_count != 0 {
            return Some(Err(error(format!(
                "Expected 0 function arguments but got {arg_count}."
            ))));
        }
        members.get(key).cloned().map(Ok)
    }

    /// `reference.toString()`: the domain renders the reference.
    pub(super) fn reference_to_string(&mut self, obj: &Value, depth: usize) -> Result<String> {
        let Value::Object(members, _) = obj else {
            return Ok(String::new());
        };
        let domain = members.get(DOMAIN_KEY).cloned().unwrap_or(Value::Null);
        match self.eval_object_method_call(
            &domain,
            "renderReference",
            std::slice::from_ref(obj),
            depth,
        )? {
            Some(Value::String(s)) => Ok(s.to_string()),
            Some(other) => Err(error(format!(
                "Expected value of type `String`, but got type `{}`.",
                value_type_name(&other)
            ))),
            None => Err(error("Domain does not define `renderReference`.")),
        }
    }

    /// Whether the reference `val` meets the type arguments `args` of
    /// `Reference<D, T>` written in `scope`.
    pub(super) fn check_reference_args(
        &mut self,
        val: &Value,
        args: &[TypeExpr],
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        let Value::Object(members, Some(source)) = val else {
            return Ok(false);
        };
        if source.kind != ObjectKind::Reference {
            return Ok(false);
        }
        if let Some(domain_ty) = args.first()
            && !super::types::type_is_any(domain_ty)
            && super::types::type_is_runtime_checkable(domain_ty, scope)
        {
            let domain = members.get(DOMAIN_KEY).cloned().unwrap_or(Value::Null);
            if !self.eval_argument_check(&domain, domain_ty, scope, depth, resolving)? {
                return Ok(false);
            }
        }
        if let Some(referent_ty) = args.get(1)
            && let Some(have) = self.reference_type(val)
        {
            self.refs.saw_constraint = false;
            let want = self.normalize_type(referent_ty, scope);
            if std::mem::take(&mut self.refs.saw_constraint) {
                return Err(error(
                    "`Reference` referent type argument may not include type constraints.",
                ));
            }
            return Ok(self.is_subtype(&have, &want));
        }
        Ok(true)
    }
}

/// The text of a reference as `render` prints it:
/// `Reference(<domain>, <type>, <data>)` followed by the path.
pub(crate) fn render_text(members: &ObjectMap, render: impl Fn(&Value) -> String) -> String {
    let mut out = String::from("Reference(");
    out.push_str(&members.get(DOMAIN_KEY).map(&render).unwrap_or_default());
    out.push_str(", ");
    if let Some(Value::String(text)) = members.get(TYPE_TEXT_KEY) {
        out.push_str(text);
    }
    out.push_str(", ");
    out.push_str(
        &members
            .get(DATA_KEY)
            .map(|d| render(&without_hidden(d)))
            .unwrap_or_default(),
    );
    out.push(')');
    if let Some(Value::List(path)) = members.get(PATH_KEY) {
        for element in path.iter() {
            let Value::Object(access, _) = element else {
                continue;
            };
            match (access.get("property"), access.get("key")) {
                (Some(Value::String(name)), _) => {
                    out.push('.');
                    if super::stdlib::render::is_regular_identifier(name) {
                        out.push_str(name);
                    } else {
                        out.push('`');
                        out.push_str(name);
                        out.push('`');
                    }
                }
                (_, Some(key)) => {
                    out.push('[');
                    out.push_str(&render(key));
                    out.push(']');
                }
                _ => {}
            }
        }
    }
    out
}

/// The data of a reference as it is shown: an instance taken from `this`
/// holds its hidden members, which a rendering leaves out.
fn without_hidden(data: &Value) -> Value {
    let Value::Object(map, Some(source)) = data else {
        return data.clone();
    };
    let hidden: Vec<&str> = source
        .entries
        .iter()
        .filter_map(|entry| match entry {
            crate::parser::Entry::Property(p)
                if super::types::has_modifier(&p.modifiers, crate::parser::Modifier::Hidden) =>
            {
                Some(&*p.name)
            }
            _ => None,
        })
        .filter(|name| map.contains_key(*name))
        .collect();
    if hidden.is_empty() {
        return data.clone();
    }
    let mut shown = (**map).clone();
    shown.retain(|key, _| !hidden.contains(&&**key));
    Value::Object(Arc::new(shown), Some(source.clone()))
}
