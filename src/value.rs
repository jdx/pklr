use rustc_hash::FxHashSet as HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::json;

use crate::parser::{Expr, TypeExpr};

/// The members of an evaluated object, in declaration order. Keys are shared
/// names so copying members between objects and scopes does not allocate.
pub type ObjectMap = IndexMap<Arc<str>, Value, rustc_hash::FxBuildHasher>;

/// Captured lexical bindings. The same type as [`ObjectMap`], so a scope can
/// become an object (and back) without rebuilding it.
pub type ScopeMap = ObjectMap;

/// Type aliases captured with a scope. The types are shared so capturing a
/// scope does not copy every alias's type expression.
pub(crate) type TypeAliasMap = IndexMap<Arc<str>, Arc<TypeExpr>, rustc_hash::FxBuildHasher>;

/// A set of binding names, shared with the scopes they came from.
pub(crate) type NameSet = rustc_hash::FxHashSet<Arc<str>>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CapturedScope {
    pub values: ScopeMap,
    /// Names in `values` declared in a lexically enclosing body.
    pub declared: NameSet,
    /// Members declared by the body whose entries are evaluated in this scope,
    /// kept so an amendment that replaces one does not drop it from the body.
    pub body_members: HashSet<String>,
    pub module_identities: IndexMap<String, String>,
    pub type_aliases: TypeAliasMap,
    pub type_namespace: Option<String>,
}

/// Captures the original AST entries and scope for an object, enabling
/// late binding: when this object is amended, its entries can be merged
/// with the overlay's entries and re-evaluated so that dependent
/// properties pick up overridden values.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSource {
    pub(crate) entries: crate::parser::Body,
    /// The bindings visible where the object was defined (see
    /// [`ObjectSource::scope`]).
    pub(crate) captured: crate::eval::SourceScope,
    /// Members declared by the object's own definition body, including ones an
    /// amendment has since replaced.
    pub(crate) body_members: HashSet<String>,
    /// Whether the class was declared `open` (allows adding new properties)
    pub(crate) is_open: bool,
    /// The pkl class name this object was instantiated from (e.g., "Step", "Group").
    /// Used by `output.renderer.converters` to apply type-specific transforms.
    pub(crate) type_name: Option<String>,
    /// Stable definition-site identity used to distinguish same-named classes.
    pub(crate) type_identity: Option<String>,
    /// Parent class names, nearest first. Converters declared for a base class
    /// also apply to instances of its subclasses.
    pub(crate) parent_type_names: Vec<String>,
    /// Stable definition-site identities for parent classes, nearest first.
    pub(crate) parent_type_identities: Vec<String>,
    /// Lexical scopes for entries introduced by earlier amendments. `None`
    /// entries use this object's definition-site `scope`.
    pub(crate) entry_scopes: Vec<Option<Arc<CapturedScope>>>,
    /// Property names that produced values when this object was evaluated.
    /// Kept separate from `scope`, which can contain unrelated same-named
    /// lexical bindings.
    pub(crate) evaluated_properties: Vec<String>,
    /// Possible value type names for mapping entries, e.g. `Step | Group` from
    /// `Mapping<String, Step | Group>`. Used when amending mappings so bare
    /// entries inherit the right class template.
    pub(crate) mapping_value_types: Vec<String>,
    /// Map of property name → optional deprecation message for properties
    /// annotated with `@Deprecated`. Consulted on field access so the
    /// warning fires when a deprecated property is *used*, not when the
    /// containing module is loaded. Crate-private: only the evaluator
    /// reads/writes this; not part of the public API.
    pub(crate) deprecated: IndexMap<String, Option<String>>,
    /// Members of a module object that failed to evaluate, mapped to their
    /// error: a class whose defaults read a `module` property that never
    /// resolved. Reading or instantiating such a member reports the error
    /// instead of treating it as absent.
    pub(crate) poisoned_members: Option<Arc<IndexMap<String, String>>>,
    /// Whether this is a `Mapping`, a class, or another object.
    pub(crate) kind: ObjectKind,
}

/// What an object with an [`ObjectSource`] is, beyond its members. Objects
/// of different kinds never compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ObjectKind {
    /// A `Dynamic` object, or an instance of the class in `type_name`.
    #[default]
    Object,
    /// A `Mapping`.
    Mapping,
    /// The class named by `type_name` itself, holding its defaults.
    Class,
}

impl ObjectSource {
    /// The object's original body entries.
    pub fn entries(&self) -> &[crate::parser::Entry] {
        &self.entries
    }

    /// Whether the object's class was declared `open`.
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// The pkl class this object was instantiated from, if any.
    pub fn type_name(&self) -> Option<&str> {
        self.type_name.as_deref()
    }

    /// Parent class names, nearest first.
    pub fn parent_type_names(&self) -> &[String] {
        &self.parent_type_names
    }

    /// The bindings visible where the object was defined.
    pub fn scope(&self) -> &ScopeMap {
        &self.captured.parts().values
    }

    /// Names in `scope` declared in a lexically enclosing body, which an
    /// inherited member of an inner object must not shadow.
    pub(crate) fn scope_declared(&self) -> &NameSet {
        &self.captured.parts().declared
    }

    /// Canonical module identities for imports captured in `scope`.
    pub(crate) fn scope_module_identities(&self) -> &IndexMap<String, String> {
        &self.captured.parts().module_identities
    }

    /// Type aliases captured alongside `scope` at the object's definition site.
    pub(crate) fn scope_type_aliases(&self) -> &TypeAliasMap {
        &self.captured.parts().type_aliases
    }

    /// Whether this source only carries a module object's failed members,
    /// with no entries to rebuild the object from on amendment.
    pub(crate) fn is_metadata_only(&self) -> bool {
        self.poisoned_members.is_some()
            && self.entries.is_empty()
            && self.evaluated_properties.is_empty()
            && self.type_name.is_none()
    }
}

/// Which Pkl collection a [`Value::List`] is. All three render the same, but
/// they never equal each other, and a `Set` compares without regard to order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ListKind {
    #[default]
    List,
    Listing,
    Set,
}

/// The items of a [`Value::List`] and which collection they form. Derefs to
/// the items.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListValue {
    items: Arc<Vec<Value>>,
    kind: ListKind,
}

impl ListValue {
    pub fn new(kind: ListKind, items: impl Into<Arc<Vec<Value>>>) -> Self {
        ListValue {
            items: items.into(),
            kind,
        }
    }

    pub fn kind(&self) -> ListKind {
        self.kind
    }

    /// The same items as a collection of another kind.
    pub fn with_kind(mut self, kind: ListKind) -> Self {
        self.kind = kind;
        self
    }

    /// Mutable access to the items, copying them first if they are shared.
    pub fn make_mut(&mut self) -> &mut Vec<Value> {
        Arc::make_mut(&mut self.items)
    }
}

impl std::ops::Deref for ListValue {
    type Target = Vec<Value>;

    fn deref(&self) -> &Vec<Value> {
        &self.items
    }
}

/// A `List` of the given items.
impl From<Vec<Value>> for ListValue {
    fn from(items: Vec<Value>) -> Self {
        ListValue::new(ListKind::List, items)
    }
}

/// A `List` of the given items.
impl From<Arc<Vec<Value>>> for ListValue {
    fn from(items: Arc<Vec<Value>>) -> Self {
        ListValue::new(ListKind::List, items)
    }
}

/// A pkl runtime value.
///
/// Pkl's `Mapping` type (arbitrary key→value) is represented as `Object` when
/// keys are strings — which is the only case supported by JSON output. All
/// `new Mapping { ["key"] = ... }` expressions therefore produce `Object`.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Strings are shared so copying a string value does not allocate.
    String(Arc<str>),
    /// Object (ordered string-keyed map). Represents both pkl objects and
    /// string-keyed Mappings.  The optional [`ObjectSource`] stores the
    /// original entry definitions so late binding works on amendment.
    /// The map is Arc-wrapped so cloning a Value::Object is O(1).
    Object(Arc<ObjectMap>, Option<Arc<ObjectSource>>),
    /// A `List`, `Listing` or `Set` (see [`ListKind`]). The items are
    /// Arc-wrapped so cloning is O(1).
    List(ListValue),
    /// Lambda function: param names + body expression + captured scope values.
    /// All three are Arc-wrapped so cloning a Lambda is O(1): lambdas are
    /// copied whenever a scope holding them is captured, and deep-copying the
    /// body each time dominated evaluation.
    Lambda(Arc<[String]>, Arc<Expr>, Arc<ScopeMap>),
}

impl Value {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => json!(b),
            Value::Int(n) => json!(n),
            Value::Float(f) => json!(f),
            Value::String(s) => serde_json::Value::String(s.to_string()),
            Value::Object(map, _) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map.iter() {
                    obj.insert(k.to_string(), v.to_json());
                }
                serde_json::Value::Object(obj)
            }
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
            }
            Value::Lambda(..) => json!("<lambda>"),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self {
            Some(s)
        } else {
            None
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut ObjectMap> {
        if let Value::Object(m, _) = self {
            Some(Arc::make_mut(m))
        } else {
            None
        }
    }

    /// Merge `other` into `self`. For objects, other's keys win.
    pub fn merge(&mut self, other: Value) {
        match (self, other) {
            (Value::Object(base, _), Value::Object(overlay, _)) => {
                let base_map = Arc::make_mut(base);
                for (k, v) in overlay.iter() {
                    base_map.insert(k.clone(), v.clone());
                }
            }
            (s, other) => *s = other,
        }
    }
}

impl From<serde_json::Value> for Value {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    Value::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            serde_json::Value::String(s) => Value::String(s.into()),
            serde_json::Value::Array(a) => {
                let items: Vec<Value> = a.into_iter().map(Value::from).collect();
                Value::List(ListValue::new(ListKind::Listing, items))
            }
            serde_json::Value::Object(o) => {
                let mut map = ObjectMap::default();
                for (k, v) in o {
                    map.insert(k.into(), Value::from(v));
                }
                Value::Object(Arc::new(map), None)
            }
        }
    }
}
