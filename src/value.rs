use rustc_hash::FxHashSet as HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::json;

use crate::parser::{Expr, TypeExpr};

/// The members of an evaluated object, in declaration order. Keys are shared
/// names so copying members between objects and scopes does not allocate.
pub type ObjectMap = IndexMap<Arc<str>, Value, rustc_hash::FxBuildHasher>;

// `ObjectMap` is also the backing store for Pkl Mappings.  Keep non-string
// mapping keys disjoint from property names and from string keys that render
// alike (for example `1` and `"1"`).  The final component preserves the
// user-facing spelling for JSON/object rendering.
pub(crate) const MAPPING_KEY_PREFIX: &str = "\0pklr:mapping-key:";

pub(crate) fn mapping_storage_key(value: &Value) -> Option<Arc<str>> {
    let (kind, identity, display) = match value {
        Value::String(_) => return None,
        Value::Null => ("null", 0, "null".to_string()),
        Value::Bool(value) => ("bool", u64::from(*value), value.to_string()),
        Value::Int(value) => ("int", *value as u64, value.to_string()),
        Value::Float(value) => {
            let identity = if *value == 0.0 { 0.0 } else { *value };
            ("float", identity.to_bits(), value.to_string())
        }
        other => ("display", 0, format!("{other:?}")),
    };
    Some(format!("{MAPPING_KEY_PREFIX}{kind}:{identity:016x}:{display}").into())
}

pub(crate) fn display_storage_key(key: &str) -> &str {
    key.strip_prefix(MAPPING_KEY_PREFIX)
        .and_then(|key| key.rsplit_once(':').map(|(_, display)| display))
        .unwrap_or(key)
}

pub(crate) fn mapping_storage_value(key: &str) -> Value {
    let Some(key) = key.strip_prefix(MAPPING_KEY_PREFIX) else {
        return Value::String(key.into());
    };
    let Some((kind, rest)) = key.split_once(':') else {
        return Value::String(key.into());
    };
    let Some((_, display)) = rest.split_once(':') else {
        return Value::String(key.into());
    };
    match kind {
        "null" => Value::Null,
        "bool" => display
            .parse()
            .map(Value::Bool)
            .unwrap_or_else(|_| Value::String(display.into())),
        "int" => display
            .parse()
            .map(Value::Int)
            .unwrap_or_else(|_| Value::String(display.into())),
        "float" => display
            .parse()
            .map(Value::Float)
            .unwrap_or_else(|_| Value::String(display.into())),
        _ => Value::String(display.into()),
    }
}

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
    /// Whether the object is an `abstract` class, which can be extended but
    /// not instantiated.
    pub(crate) is_abstract: bool,
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
    /// Bare element values of a Dynamic body, in source order. These are
    /// distinct from named properties and are consumed by `xml.Element`.
    pub(crate) elements: Vec<Value>,
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
    /// Members of a module object that failed to evaluate. Reading or
    /// instantiating such a member reports its error instead of treating it
    /// as absent, and rendering the module reports a failed output member.
    pub(crate) poisoned_members: Option<Arc<IndexMap<String, PoisonedMember>>>,
    /// Whether this is a `Mapping`, a class, or another object.
    pub(crate) kind: ObjectKind,
    /// Whether this object was materialized from JSON rather than evaluated
    /// from a Pkl body. Such objects retain their evaluated members when
    /// amended, but still render as Dynamics.
    pub(crate) is_parsed_json: bool,
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

/// A module member that failed to evaluate (see
/// [`ObjectSource::poisoned_members`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PoisonedMember {
    pub(crate) message: String,
    /// Whether the member is a property that rendering the module outputs.
    pub(crate) rendered: bool,
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

    /// Whether this source only carries metadata (failed module members, an
    /// abstract module marker, or a bare Mapping tag), with no entries to
    /// rebuild the object from on amendment.
    pub(crate) fn is_metadata_only(&self) -> bool {
        (self.poisoned_members.is_some()
            || self.is_abstract
            || (matches!(self.kind, ObjectKind::Mapping) && self.mapping_value_types.is_empty()))
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

    /// The address of the shared items, which identifies them while they are
    /// alive.
    pub(crate) fn items_ptr(&self) -> *const Vec<Value> {
        Arc::as_ptr(&self.items)
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
    /// A compiled regular expression (`Regex(pattern)`).
    Regex(Arc<Regex>),
}

/// A pkl `Regex`: the pattern as written and its compiled form.
pub struct Regex {
    pattern: String,
    compiled: fancy_regex::Regex,
}

impl Regex {
    /// Compile `pattern`, returning the regex engine's message on a syntax error.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let compiled = fancy_regex::RegexBuilder::new(&expand_quotes(pattern))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            pattern: pattern.to_string(),
            compiled,
        })
    }

    /// The pattern this regex was compiled from.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub(crate) fn compiled(&self) -> &fancy_regex::Regex {
        &self.compiled
    }
}

/// `pattern` with Java's `\Q...\E` quoted sections replaced by escaped
/// literals, which the Rust engine has no syntax for.
fn expand_quotes(pattern: &str) -> std::borrow::Cow<'_, str> {
    if !pattern.contains("\\Q") {
        return pattern.into();
    }
    let mut out = String::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let escaped = &rest[i + 1..];
        match escaped.chars().next() {
            Some('Q') => {
                let quoted = &escaped[1..];
                let (literal, after) = match quoted.find("\\E") {
                    Some(end) => (&quoted[..end], &quoted[end + 2..]),
                    None => (quoted, ""),
                };
                out.push_str(&fancy_regex::escape(literal));
                rest = after;
            }
            Some(c) => {
                out.push('\\');
                out.push(c);
                rest = &escaped[c.len_utf8()..];
            }
            None => {
                out.push('\\');
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.into()
}

impl std::fmt::Debug for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Regex").field(&self.pattern).finish()
    }
}

/// Regexes are equal when their patterns are, as in pkl.
impl PartialEq for Regex {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}

impl Value {
    /// The pkl class name of this value's type, as pkl reports it in errors.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Bool(_) => "Boolean",
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::String(_) => "String",
            Value::Object(..) => "Dynamic",
            Value::List(_) => "List",
            Value::Lambda(params, ..) => match params.len() {
                0 => "Function0",
                1 => "Function1",
                2 => "Function2",
                3 => "Function3",
                4 => "Function4",
                _ => "Function5",
            },
            Value::Regex(_) => "Regex",
        }
    }

    /// Convert to JSON, failing like `pkl eval -f json` does on values JSON
    /// cannot represent.
    pub fn try_to_json(&self) -> Result<serde_json::Value, crate::Error> {
        match self {
            Value::Regex(_) => Err(crate::Error::Eval(format!(
                "Cannot render value of type `{}` as JSON.\nValue: {}",
                self.type_name(),
                crate::eval::stdlib::render_value(self)
            ))),
            Value::Object(map, _) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map.iter() {
                    obj.insert(display_storage_key(k).to_string(), v.try_to_json()?);
                }
                Ok(serde_json::Value::Object(obj))
            }
            Value::List(items) => Ok(serde_json::Value::Array(
                items
                    .iter()
                    .map(Value::try_to_json)
                    .collect::<Result<_, _>>()?,
            )),
            _ => Ok(self.to_json()),
        }
    }

    /// Convert to JSON. Values JSON cannot represent become a tagged object
    /// (a `Regex` is `{"_type": "regex", "pattern": ...}`); see
    /// [`Value::try_to_json`] to reject them as `pkl eval -f json` does.
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
                    obj.insert(display_storage_key(k).to_string(), v.to_json());
                }
                serde_json::Value::Object(obj)
            }
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
            }
            Value::Lambda(..) => json!("<lambda>"),
            Value::Regex(regex) => json!({ "_type": "regex", "pattern": regex.pattern() }),
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
