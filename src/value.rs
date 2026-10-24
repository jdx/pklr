use rustc_hash::FxHashSet as HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

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

// NaN does not equal itself, so each occurrence must be a different map key.
// This ID is internal-only: the stored key still renders from `display`.
static NEXT_NAN_MAPPING_KEY: AtomicU64 = AtomicU64::new(0);

pub(crate) fn mapping_storage_key(value: &Value) -> Option<Arc<str>> {
    let (kind, identity, display) = match value {
        Value::String(_) => return None,
        Value::Null => ("null", "0000000000000000".to_string(), "null".to_string()),
        Value::Bool(value) => (
            "bool",
            format!("{:016x}", u64::from(*value)),
            value.to_string(),
        ),
        Value::Int(value) => ("int", format!("{:016x}", *value as u64), value.to_string()),
        Value::Float(value) => {
            let identity = if *value == 0.0 { 0.0 } else { *value };
            (
                "float",
                format!("{:016x}", identity.to_bits()),
                value.to_string(),
            )
        }
        // Units compare in their smallest units. Keep that normalized amount
        // as identity, but retain the inserted spelling for `keys` and
        // converters.
        Value::Duration(value) => {
            let identity = value.value_in(DurationUnit::Nanos);
            let identity = if identity == 0.0 { 0.0 } else { identity };
            let identity = if identity.is_nan() {
                // Keep NaNs in a namespace which cannot collide with a finite
                // normalized amount.  Do not XOR the unit into the bits: that
                // can turn a NaN payload into a finite amount.
                format!(
                    "nan-{:02x}-{:016x}-{:016x}",
                    value.unit as u8,
                    value.value.to_bits(),
                    value.nan_identity,
                )
            } else {
                format!("finite-{:016x}", identity.to_bits())
            };
            (
                "duration",
                identity,
                format!("{}.{}", value.value, value.unit.symbol()),
            )
        }
        Value::DataSize(value) => {
            let identity = value.value_in(DataSizeUnit::Bytes);
            let identity = if identity == 0.0 { 0.0 } else { identity };
            let identity = if identity.is_nan() {
                format!(
                    "nan-{:02x}-{:016x}-{:016x}",
                    value.unit as u8,
                    value.value.to_bits(),
                    value.nan_identity,
                )
            } else {
                format!("finite-{:016x}", identity.to_bits())
            };
            (
                "dataSize",
                identity,
                format!("{}.{}", value.value, value.unit.symbol()),
            )
        }
        // Do not use derived `Debug` for compound keys. Native units carry an
        // internal NaN identity, and Debug would make two equal lists such as
        // `List(1.s)` acquire unrelated mapping identities.
        Value::List(value) => (
            "list",
            mapping_compound_identity(&Value::List(value.clone())),
            Value::List(value.clone()).to_json().to_string(),
        ),
        Value::Object(map, source) => (
            "object",
            mapping_object_identity(map, source.as_deref()),
            Value::Object(map.clone(), source.clone())
                .to_json()
                .to_string(),
        ),
        other => (
            "display",
            "0000000000000000".to_string(),
            format!("{other:?}"),
        ),
    };
    Some(format!("{MAPPING_KEY_PREFIX}{kind}:{identity}:{display}").into())
}

/// A typed, deterministic identity for a value used inside a compound mapping
/// key. This is intentionally narrower than Pkl's full value equality: the
/// fallback currently represents Lists, whose elements can include native
/// units and regexes that must not inherit implementation-only Debug fields.
fn mapping_compound_identity(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => format!("bool:{value}"),
        Value::Int(value) => format!("int:{value}"),
        Value::Float(value) => format!("float:{:016x}", value.to_bits()),
        Value::String(value) => format!("string:{}:{value}", value.len()),
        Value::Duration(_) | Value::DataSize(_) => mapping_storage_key(value)
            .expect("native units have mapping keys")
            .to_string(),
        Value::Regex(value) => format!("regex:{}:{}", value.pattern().len(), value.pattern()),
        Value::List(items) => {
            let mut identity = format!("list:{:?}:[", items.kind());
            for item in items.iter() {
                let item = mapping_compound_identity(item);
                identity.push_str(&format!("{}:{item}", item.len()));
            }
            identity.push(']');
            identity
        }
        Value::Object(map, source) => mapping_object_identity(map, source.as_deref()),
        // Functions have identity semantics and are not structurally keyable.
        other => format!("debug:{other:?}"),
    }
}

fn mapping_object_identity(map: &ObjectMap, source: Option<&ObjectSource>) -> String {
    let mut members = map
        .iter()
        .map(|(key, value)| {
            let value = mapping_compound_identity(value);
            format!("{}:{key}:{}:{value}", key.len(), value.len())
        })
        .collect::<Vec<_>>();
    // Mapping and Dynamic equality compare members independent of declaration
    // order. Sorting also makes this identity independent of hash/map layout.
    members.sort_unstable();
    let kind = source.map_or_else(
        || "Dynamic".to_string(),
        |source| match source.kind {
            ObjectKind::Mapping => "Mapping".to_string(),
            // A class object and an instance intentionally share the class's
            // definition-site identity, but they are not equal Pkl values and
            // therefore must remain distinct when used as mapping keys.
            ObjectKind::Object => source.type_identity.as_deref().map_or_else(
                || "Dynamic".to_string(),
                |identity| format!("instance:{identity}"),
            ),
            ObjectKind::Class => source.type_identity.as_deref().map_or_else(
                || "Class".to_string(),
                |identity| format!("class:{identity}"),
            ),
        },
    );
    format!("object:{kind}:{}", members.join("|"))
}

/// Whether two storage keys name equal native-unit mapping keys. Their final
/// display component deliberately differs when equivalent units were spelled
/// differently at insertion and lookup time.
pub(crate) fn mapping_storage_keys_equal(a: &str, b: &str) -> bool {
    fn parts(key: &str) -> Option<(&str, &str)> {
        let key = key.strip_prefix(MAPPING_KEY_PREFIX)?;
        let (kind, rest) = key.split_once(':')?;
        let (identity, _) = rest.split_once(':')?;
        matches!(kind, "duration" | "dataSize").then_some((kind, identity))
    }
    parts(a).zip(parts(b)).is_some_and(|(a, b)| a == b)
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
    let Some((identity, display)) = rest.split_once(':') else {
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
        "duration" => parse_unit_value(display, DurationUnit::parse)
            .map(|(value, unit)| {
                Value::Duration(Duration::with_mapping_identity(value, unit, identity))
            })
            .unwrap_or_else(|| Value::String(display.into())),
        "dataSize" => parse_unit_value(display, DataSizeUnit::parse)
            .map(|(value, unit)| {
                Value::DataSize(DataSize::with_mapping_identity(value, unit, identity))
            })
            .unwrap_or_else(|| Value::String(display.into())),
        _ => Value::String(display.into()),
    }
}

fn parse_unit_value<U>(display: &str, unit: impl FnOnce(&str) -> Option<U>) -> Option<(f64, U)> {
    let (value, unit_name) = display.rsplit_once('.')?;
    Some((value.parse().ok()?, unit(unit_name)?))
}

fn mapping_nan_identity(identity: &str) -> Option<(u64, u64)> {
    let mut parts = identity.strip_prefix("nan-")?.split('-');
    let _unit = parts.next()?;
    let bits = u64::from_str_radix(parts.next()?, 16).ok()?;
    let nonce = u64::from_str_radix(parts.next()?, 16).ok()?;
    parts.next().is_none().then_some((bits, nonce))
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
    pub body_members: HashSet<Arc<str>>,
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
    pub(crate) body_members: HashSet<Arc<str>>,
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
    pub(crate) evaluated_properties: Vec<Arc<str>>,
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
    pub(crate) poisoned_members: Option<Arc<IndexMap<Arc<str>, PoisonedMember>>>,
    /// Whether this is a `Mapping`, a class, or another object.
    pub(crate) kind: ObjectKind,
    /// Whether this object was materialized from JSON rather than evaluated
    /// from a Pkl body. Such objects retain their evaluated members when
    /// amended, but still render as Dynamics.
    pub(crate) is_parsed_json: bool,
    /// For an instance of a class, the class's default object. An untyped
    /// `new { ... }` assigned to one of the instance's properties amends the
    /// class default of that property, not the receiver's current value.
    pub(crate) prototype: Option<Value>,
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
    /// Lambda function: params, body and captured scope.
    /// The shared fields are Arc-wrapped so cloning a Lambda is O(1): lambdas are
    /// copied whenever a scope holding them is captured, and deep-copying the
    /// body each time dominated evaluation.
    Lambda(Arc<[String]>, Arc<Expr>, Arc<ScopeMap>),
    /// A compiled regular expression (`Regex(pattern)`).
    Regex(Arc<Regex>),
    /// A quantity of time (`5.min`).
    Duration(Duration),
    /// A quantity of digital information (`5.mb`).
    DataSize(DataSize),
}

/// The unit of a [`Duration`], smallest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurationUnit {
    Nanos,
    Micros,
    Millis,
    Seconds,
    Minutes,
    Hours,
    Days,
}

impl DurationUnit {
    pub const ALL: [DurationUnit; 7] = [
        DurationUnit::Nanos,
        DurationUnit::Micros,
        DurationUnit::Millis,
        DurationUnit::Seconds,
        DurationUnit::Minutes,
        DurationUnit::Hours,
        DurationUnit::Days,
    ];

    /// The unit's pkl name, as in `5.min`.
    pub fn symbol(self) -> &'static str {
        match self {
            DurationUnit::Nanos => "ns",
            DurationUnit::Micros => "us",
            DurationUnit::Millis => "ms",
            DurationUnit::Seconds => "s",
            DurationUnit::Minutes => "min",
            DurationUnit::Hours => "h",
            DurationUnit::Days => "d",
        }
    }

    /// The unit named `symbol`, if any.
    pub fn parse(symbol: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|unit| unit.symbol() == symbol)
    }

    /// Nanoseconds per unit.
    pub fn nanos(self) -> f64 {
        match self {
            DurationUnit::Nanos => 1.0,
            DurationUnit::Micros => 1e3,
            DurationUnit::Millis => 1e6,
            DurationUnit::Seconds => 1e9,
            DurationUnit::Minutes => 60e9,
            DurationUnit::Hours => 3600e9,
            DurationUnit::Days => 86400e9,
        }
    }
}

/// A pkl `Duration`. Like pkl, the value is kept as a float in its unit.
#[derive(Debug, Clone, Copy)]
pub struct Duration {
    pub value: f64,
    pub unit: DurationUnit,
    nan_identity: u64,
}

impl Duration {
    pub(crate) fn new(value: f64, unit: DurationUnit) -> Self {
        Self {
            value,
            unit,
            nan_identity: NEXT_NAN_MAPPING_KEY.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn with_mapping_identity(value: f64, unit: DurationUnit, identity: &str) -> Self {
        let (value, nan_identity) = mapping_nan_identity(identity)
            .map(|(bits, nonce)| (f64::from_bits(bits), nonce))
            .unwrap_or_else(|| (value, NEXT_NAN_MAPPING_KEY.fetch_add(1, Ordering::Relaxed)));
        Self {
            value,
            unit,
            nan_identity,
        }
    }

    /// The value converted to `unit`.
    pub fn value_in(&self, unit: DurationUnit) -> f64 {
        self.value * self.unit.nanos() / unit.nanos()
    }
}

impl PartialEq for Duration {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.unit == other.unit
    }
}

/// The unit of a [`DataSize`], smallest first, decimal before binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataSizeUnit {
    Bytes,
    Kilobytes,
    Kibibytes,
    Megabytes,
    Mebibytes,
    Gigabytes,
    Gibibytes,
    Terabytes,
    Tebibytes,
    Petabytes,
    Pebibytes,
}

impl DataSizeUnit {
    pub const ALL: [DataSizeUnit; 11] = [
        DataSizeUnit::Bytes,
        DataSizeUnit::Kilobytes,
        DataSizeUnit::Kibibytes,
        DataSizeUnit::Megabytes,
        DataSizeUnit::Mebibytes,
        DataSizeUnit::Gigabytes,
        DataSizeUnit::Gibibytes,
        DataSizeUnit::Terabytes,
        DataSizeUnit::Tebibytes,
        DataSizeUnit::Petabytes,
        DataSizeUnit::Pebibytes,
    ];

    /// The unit's pkl name, as in `5.mb`.
    pub fn symbol(self) -> &'static str {
        match self {
            DataSizeUnit::Bytes => "b",
            DataSizeUnit::Kilobytes => "kb",
            DataSizeUnit::Kibibytes => "kib",
            DataSizeUnit::Megabytes => "mb",
            DataSizeUnit::Mebibytes => "mib",
            DataSizeUnit::Gigabytes => "gb",
            DataSizeUnit::Gibibytes => "gib",
            DataSizeUnit::Terabytes => "tb",
            DataSizeUnit::Tebibytes => "tib",
            DataSizeUnit::Petabytes => "pb",
            DataSizeUnit::Pebibytes => "pib",
        }
    }

    /// The unit named `symbol`, if any.
    pub fn parse(symbol: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|unit| unit.symbol() == symbol)
    }

    /// Bytes per unit.
    pub fn bytes(self) -> i64 {
        match self {
            DataSizeUnit::Bytes => 1,
            DataSizeUnit::Kilobytes => 1000,
            DataSizeUnit::Kibibytes => 1024,
            DataSizeUnit::Megabytes => 1000_i64.pow(2),
            DataSizeUnit::Mebibytes => 1024_i64.pow(2),
            DataSizeUnit::Gigabytes => 1000_i64.pow(3),
            DataSizeUnit::Gibibytes => 1024_i64.pow(3),
            DataSizeUnit::Terabytes => 1000_i64.pow(4),
            DataSizeUnit::Tebibytes => 1024_i64.pow(4),
            DataSizeUnit::Petabytes => 1000_i64.pow(5),
            DataSizeUnit::Pebibytes => 1024_i64.pow(5),
        }
    }
}

/// A pkl `DataSize`. Like pkl, the value is kept as a float in its unit.
#[derive(Debug, Clone, Copy)]
pub struct DataSize {
    pub value: f64,
    pub unit: DataSizeUnit,
    nan_identity: u64,
}

impl DataSize {
    pub(crate) fn new(value: f64, unit: DataSizeUnit) -> Self {
        Self {
            value,
            unit,
            nan_identity: NEXT_NAN_MAPPING_KEY.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn with_mapping_identity(value: f64, unit: DataSizeUnit, identity: &str) -> Self {
        let (value, nan_identity) = mapping_nan_identity(identity)
            .map(|(bits, nonce)| (f64::from_bits(bits), nonce))
            .unwrap_or_else(|| (value, NEXT_NAN_MAPPING_KEY.fetch_add(1, Ordering::Relaxed)));
        Self {
            value,
            unit,
            nan_identity,
        }
    }

    /// The value converted to `unit`.
    pub fn value_in(&self, unit: DataSizeUnit) -> f64 {
        self.value * self.unit.bytes() as f64 / unit.bytes() as f64
    }
}

impl PartialEq for DataSize {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.unit == other.unit
    }
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

/// How an object's [`ObjectSource`] changes what it is, as opposed to what it
/// holds: its kind and class. `None` is a plain `Dynamic`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by the late-binding passes in the next commit"
    )
)]
fn object_shape(source: Option<&ObjectSource>) -> (ObjectKind, Option<&str>, Option<&str>) {
    source.map_or((ObjectKind::Object, None, None), |source| {
        (
            source.kind,
            source.type_identity.as_deref(),
            source.type_name.as_deref(),
        )
    })
}

/// Whether two objects' sources hold the same data that is not in the member
/// map: the bare elements of a `Dynamic`, the members that failed to evaluate,
/// and the names of the properties evaluated, which include hidden ones. The
/// definition-site scope is not compared: it is flattened only when something
/// reads it, and flattening is the cost the lazy capture exists to avoid. What
/// else a source holds (the entries, the class flags, the deprecations) comes
/// from the object's syntax and class, which every evaluation of it shares.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by the late-binding passes in the next commit"
    )
)]
fn same_source_data(
    a: Option<&Arc<ObjectSource>>,
    b: Option<&Arc<ObjectSource>>,
    equal: &mut EqualPairs,
    nested: bool,
    depth: usize,
) -> bool {
    let (a, b) = match (a, b) {
        (None, None) => return true,
        (Some(a), Some(b)) => (a, b),
        (Some(only), None) | (None, Some(only)) => {
            return only.elements.is_empty() && only.poisoned_members.is_none();
        }
    };
    if Arc::ptr_eq(a, b) {
        return true;
    }
    // Most objects have neither: nothing to remember, and a short compare.
    let poisoned = match (&a.poisoned_members, &b.poisoned_members) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
        _ => false,
    };
    if !poisoned || a.evaluated_properties != b.evaluated_properties {
        return false;
    }
    if a.elements.is_empty() || b.elements.is_empty() {
        return a.elements.len() == b.elements.len();
    }
    let pair = (Arc::as_ptr(a) as usize, Arc::as_ptr(b) as usize, nested);
    if equal.contains(&pair) {
        return true;
    }
    let same = a.elements.len() == b.elements.len()
        && a.elements
            .iter()
            .zip(&b.elements)
            .all(|(a, b)| same_data(a, b, equal, nested, depth + 1));
    if same {
        equal.insert(pair);
    }
    same
}

/// How deeply `same_data` follows nested values. Each level takes a stack
/// frame, so a value nested deeper is treated as not known to be equal.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by the late-binding passes in the next commit"
    )
)]
const SAME_DATA_MAX_DEPTH: usize = 512;

/// Objects, lists, sources and captured scopes already found equal, by address, and
/// whether their lambdas were compared by body only (`nested`).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by the late-binding passes in the next commit"
    )
)]
type EqualPairs = HashSet<(usize, usize, bool)>;

/// Whether two shared maps hold the same members in the same order.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by the late-binding passes in the next commit"
    )
)]
fn same_members(
    a: &Arc<ObjectMap>,
    b: &Arc<ObjectMap>,
    equal: &mut EqualPairs,
    nested: bool,
    depth: usize,
) -> bool {
    let pair = (Arc::as_ptr(a) as usize, Arc::as_ptr(b) as usize, nested);
    if pair.0 == pair.1 || equal.contains(&pair) {
        return true;
    }
    let same = a.len() == b.len()
        && a.iter().zip(b.iter()).all(|((a_key, a), (b_key, b))| {
            a_key == b_key && same_data(a, b, equal, nested, depth + 1)
        });
    if same {
        equal.insert(pair);
    }
    same
}

/// [`Value::same_data`] for values `depth` levels into the comparison. `nested`
/// is set inside a lambda's capture, where lambdas are not compared by what
/// they captured.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by the late-binding passes in the next commit"
    )
)]
fn same_data(a: &Value, b: &Value, equal: &mut EqualPairs, nested: bool, depth: usize) -> bool {
    if depth > SAME_DATA_MAX_DEPTH {
        return false;
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        // Bits, not `==`: `0.0` and `-0.0` render differently, and a NaN is
        // the same value as another with the same bits.
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Object(a, a_source), Value::Object(b, b_source)) => {
            object_shape(a_source.as_deref()) == object_shape(b_source.as_deref())
                && same_source_data(a_source.as_ref(), b_source.as_ref(), equal, nested, depth)
                && same_members(a, b, equal, nested, depth)
        }
        (Value::List(a), Value::List(b)) => {
            if a.kind() != b.kind() {
                return false;
            }
            let pair = (a.items_ptr() as usize, b.items_ptr() as usize, nested);
            if pair.0 == pair.1 || equal.contains(&pair) {
                return true;
            }
            let same = a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(a, b)| same_data(a, b, equal, nested, depth + 1));
            if same {
                equal.insert(pair);
            }
            same
        }
        // Every evaluation of a lambda expression makes a new lambda with a
        // new capture, so equal lambdas are told apart by what they hold: the
        // parameters, the body and the captured data. A capture holds the
        // lambda's own previous version (through `this`), and that one holds
        // the version before it, so lambdas inside a capture are compared by
        // parameters and body only. What such a lambda captured is a copy of
        // members the caller compares directly; it can lag behind them, which
        // is what late binding allows for.
        (Value::Lambda(a_params, a_body, a_env), Value::Lambda(b_params, b_body, b_env)) => {
            a_params == b_params
                && (Arc::ptr_eq(a_body, b_body) || a_body == b_body)
                && (nested || same_members(a_env, b_env, equal, true, depth))
        }
        (Value::Regex(a), Value::Regex(b)) => a == b,
        (Value::Duration(a), Value::Duration(b)) => {
            a.unit == b.unit && a.value.to_bits() == b.value.to_bits()
        }
        (Value::DataSize(a), Value::DataSize(b)) => {
            a.unit == b.unit && a.value.to_bits() == b.value.to_bits()
        }
        _ => false,
    }
}

impl Value {
    /// Whether `other` holds the same data as `self`: the same members, items
    /// and scalars in the same order, objects of the same kind and class with
    /// the same elements, hidden property names and failed members, and
    /// lambdas with the same parameters, body and captured data.
    ///
    /// This is not `==`. An object's [`ObjectSource`] holds the scopes it was
    /// defined in, which share structure with the objects they contain, so
    /// comparing them is exponential in how deeply objects nest. Here only
    /// what the object holds is compared, and a pair of shared objects or
    /// lists is not compared twice.
    ///
    /// `false` means "not known to be equal", not "different": a value nested
    /// more than `SAME_DATA_MAX_DEPTH` levels deep is not followed, so the
    /// comparison cannot overflow the stack.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used by the late-binding passes in the next commit"
        )
    )]
    pub(crate) fn same_data(&self, other: &Value) -> bool {
        same_data(self, other, &mut EqualPairs::default(), false, 0)
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
            Value::Duration(_) => "Duration",
            Value::DataSize(_) => "DataSize",
        }
    }

    /// Convert to JSON, failing like `pkl eval -f json` does on values JSON
    /// cannot represent.
    pub fn try_to_json(&self) -> Result<serde_json::Value, crate::Error> {
        let mut unrenderable = None;
        let json = self.json_checked(&mut unrenderable);
        match unrenderable {
            None => Ok(json),
            Some(value) => Err(value.json_render_error()),
        }
    }

    /// [`Value::to_json`], noting in `unrenderable` the first value inside
    /// that JSON cannot represent.
    fn json_checked<'a>(&'a self, unrenderable: &mut Option<&'a Value>) -> serde_json::Value {
        match self {
            Value::Float(f) if !f.is_finite() => {
                unrenderable.get_or_insert(self);
                serde_json::Value::Null
            }
            Value::Regex(_) | Value::Duration(_) | Value::DataSize(_) => {
                unrenderable.get_or_insert(self);
                serde_json::Value::Null
            }
            Value::Object(map, _) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map.iter() {
                    // The result is discarded once a value fails.
                    if unrenderable.is_some() {
                        break;
                    }
                    obj.insert(
                        display_storage_key(k).to_string(),
                        v.json_checked(unrenderable),
                    );
                }
                serde_json::Value::Object(obj)
            }
            Value::List(items) => {
                let mut array = Vec::with_capacity(items.len());
                for item in items.iter() {
                    if unrenderable.is_some() {
                        break;
                    }
                    array.push(item.json_checked(unrenderable));
                }
                serde_json::Value::Array(array)
            }
            _ => self.to_json(),
        }
    }

    /// pkl's error for rendering this value as JSON.
    fn json_render_error(&self) -> crate::Error {
        if let Value::Float(f) = self {
            return crate::Error::Eval(format!(
                "Cannot render value `{}` as JSON.",
                if f.is_nan() {
                    "NaN"
                } else if *f > 0.0 {
                    "∞"
                } else {
                    "-∞"
                }
            ));
        }
        crate::Error::Eval(format!(
            "Cannot render value of type `{}` as JSON.\nValue: {}",
            self.type_name(),
            crate::eval::stdlib::render_value(self)
        ))
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
            Value::Object(map, source) => {
                let mut obj = serde_json::Map::new();
                // Methods are stored as function-valued members, but are not
                // members of the object Pkl renders.
                let is_method = |name: &str| {
                    source.as_ref().is_some_and(|source| {
                        source.entries.iter().any(|entry| {
                            matches!(entry, crate::parser::Entry::Property(prop)
                                if prop.is_method && &*prop.name == name)
                        })
                    })
                };
                for (k, v) in map.iter() {
                    if matches!(v, Value::Lambda(..)) && is_method(k) {
                        continue;
                    }
                    obj.insert(display_storage_key(k).to_string(), v.to_json());
                }
                serde_json::Value::Object(obj)
            }
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
            }
            Value::Lambda(..) => json!("<lambda>"),
            Value::Regex(regex) => json!({ "_type": "regex", "pattern": regex.pattern() }),
            Value::Duration(_) | Value::DataSize(_) => {
                json!(crate::eval::stdlib::render_value(self))
            }
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
                    let storage_key = base_map
                        .get_key_value(k)
                        .map(|(key, _)| key.clone())
                        .or_else(|| {
                            base_map
                                .keys()
                                .find(|stored| mapping_storage_keys_equal(stored, k))
                                .cloned()
                        })
                        .unwrap_or_else(|| k.clone());
                    base_map.insert(storage_key, v.clone());
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

#[cfg(test)]
mod same_data_tests {
    use super::*;

    fn object(members: Vec<(&str, Value)>) -> Value {
        let map: ObjectMap = members.into_iter().map(|(k, v)| (k.into(), v)).collect();
        Value::Object(Arc::new(map), None)
    }

    fn lambda(env: Vec<(&str, Value)>) -> Value {
        let env: ScopeMap = env.into_iter().map(|(k, v)| (k.into(), v)).collect();
        Value::Lambda(
            Arc::from(vec!["x".to_string()]),
            Arc::new(Expr::Ident("x".into())),
            Arc::new(env),
        )
    }

    #[test]
    fn floats_are_the_same_only_when_their_bits_are() {
        assert!(Value::Float(1.5).same_data(&Value::Float(1.5)));
        assert!(Value::Float(f64::NAN).same_data(&Value::Float(f64::NAN)));
        assert!(!Value::Float(0.0).same_data(&Value::Float(-0.0)));
        assert!(!Value::Int(1).same_data(&Value::Float(1.0)));
    }

    #[test]
    fn collections_of_different_kinds_are_not_the_same() {
        let items = vec![Value::Int(1), Value::Int(2)];
        let list = |kind| Value::List(ListValue::new(kind, items.clone()));
        assert!(list(ListKind::List).same_data(&list(ListKind::List)));
        assert!(!list(ListKind::List).same_data(&list(ListKind::Set)));
        assert!(!list(ListKind::List).same_data(&list(ListKind::Listing)));
    }

    #[test]
    fn objects_are_the_same_with_the_same_members_in_the_same_order() {
        let a = object(vec![("x", Value::Int(1)), ("y", Value::Int(2))]);
        assert!(a.same_data(&object(vec![("x", Value::Int(1)), ("y", Value::Int(2))])));
        assert!(!a.same_data(&object(vec![("y", Value::Int(2)), ("x", Value::Int(1))])));
        assert!(!a.same_data(&object(vec![("x", Value::Int(1)), ("y", Value::Int(3))])));
        assert!(!a.same_data(&object(vec![("x", Value::Int(1))])));
    }

    /// Each level holds the one below it twice, so comparing the levels as a
    /// tree takes time exponential in how many there are.
    #[test]
    fn deeply_shared_objects_compare_in_linear_time() {
        fn tower(levels: usize) -> Value {
            let mut value = Value::Int(0);
            for _ in 0..levels {
                value = object(vec![("left", value.clone()), ("right", value)]);
            }
            value
        }

        assert!(tower(64).same_data(&tower(64)));
        let mut other = tower(63);
        other = object(vec![("left", other.clone()), ("right", other)]);
        assert!(tower(64).same_data(&other));
        assert!(!tower(64).same_data(&tower(63)));
    }

    #[test]
    fn lambdas_are_the_same_with_the_same_body_and_captured_data() {
        let a = lambda(vec![("n", Value::Int(1))]);
        assert!(a.same_data(&lambda(vec![("n", Value::Int(1))])));
        assert!(!a.same_data(&lambda(vec![("n", Value::Int(2))])));
        assert!(!a.same_data(&lambda(vec![])));
    }

    /// A lambda captures its previous version, which captured the one before.
    /// Comparing those in full never ends in two versions being equal.
    #[test]
    fn lambdas_captured_by_a_lambda_compare_by_body_only() {
        let old = lambda(vec![("n", Value::Int(1))]);
        let new = lambda(vec![("n", Value::Int(2))]);
        assert!(!old.same_data(&new));
        assert!(lambda(vec![("self", old)]).same_data(&lambda(vec![("self", new)])));
    }

    /// A value nested too deeply to follow is not known to be the same, which
    /// is what a caller must assume of any value it cannot compare.
    #[test]
    fn values_nested_beyond_the_depth_limit_are_not_known_to_be_the_same() {
        fn nested(levels: usize) -> Value {
            let mut value = Value::Int(0);
            for _ in 0..levels {
                value = Value::List(ListValue::new(ListKind::List, vec![value]));
            }
            value
        }
        assert!(nested(SAME_DATA_MAX_DEPTH / 2).same_data(&nested(SAME_DATA_MAX_DEPTH / 2)));
        assert!(nested(SAME_DATA_MAX_DEPTH).same_data(&nested(SAME_DATA_MAX_DEPTH)));
        assert!(!nested(SAME_DATA_MAX_DEPTH + 1).same_data(&nested(SAME_DATA_MAX_DEPTH + 1)));

        // Far deeper than a test thread's stack could follow. The values are
        // leaked because dropping them recurses just as deep.
        let (a, b) = (nested(100_000), nested(100_000));
        assert!(!a.same_data(&b));
        std::mem::forget((a, b));
    }

    fn lambda_over(body: &Arc<Expr>, env: Vec<(&str, Value)>) -> Value {
        let env: ScopeMap = env.into_iter().map(|(k, v)| (k.into(), v)).collect();
        Value::Lambda(
            Arc::from(vec!["x".to_string()]),
            Arc::clone(body),
            Arc::new(env),
        )
    }

    /// Two evaluations of one lambda expression share its body, so what tells
    /// them apart is what each captured.
    #[test]
    fn lambdas_sharing_a_body_differ_in_what_they_captured() {
        let body = Arc::new(Expr::Ident("x".into()));
        let a = lambda_over(&body, vec![("n", Value::Int(1))]);
        assert!(a.same_data(&lambda_over(&body, vec![("n", Value::Int(1))])));
        assert!(!a.same_data(&lambda_over(&body, vec![("n", Value::Int(2))])));
    }

    /// `1.min` and `1.s` have equal values and different units: not the same.
    #[test]
    fn durations_and_data_sizes_are_the_same_only_in_the_same_unit() {
        let duration = |value, unit| Value::Duration(Duration::new(value, unit));
        let a = duration(1.0, DurationUnit::Minutes);
        assert!(a.same_data(&duration(1.0, DurationUnit::Minutes)));
        assert!(!a.same_data(&duration(1.0, DurationUnit::Seconds)));
        assert!(!a.same_data(&duration(2.0, DurationUnit::Minutes)));
        let size = |value, unit| Value::DataSize(DataSize::new(value, unit));
        let b = size(1.0, DataSizeUnit::Kilobytes);
        assert!(b.same_data(&size(1.0, DataSizeUnit::Kilobytes)));
        assert!(!b.same_data(&size(1.0, DataSizeUnit::Kibibytes)));
        assert!(!b.same_data(&size(2.0, DataSizeUnit::Kilobytes)));
    }

    /// A lambda that takes other parameters or has another body is not the
    /// same, whatever either captured, also when one is captured by another.
    #[test]
    fn lambdas_with_other_parameters_or_bodies_are_not_the_same() {
        let body = Arc::new(Expr::Ident("x".into()));
        let other_body = Arc::new(Expr::Ident("y".into()));
        let env = || Arc::new(ScopeMap::default());
        let make = |params: &[&str], body: &Arc<Expr>| {
            Value::Lambda(
                Arc::from(params.iter().map(|p| p.to_string()).collect::<Vec<_>>()),
                Arc::clone(body),
                env(),
            )
        };
        let a = make(&["x"], &body);
        assert!(a.same_data(&make(&["x"], &body)));
        assert!(!a.same_data(&make(&["z"], &body)));
        assert!(!a.same_data(&make(&["x", "z"], &body)));
        assert!(!a.same_data(&make(&["x"], &other_body)));
        let capture = |lambda| {
            let mut members = ScopeMap::default();
            members.insert("f".into(), lambda);
            Value::Lambda(
                Arc::from(vec!["x".to_string()]),
                Arc::clone(&body),
                Arc::new(members),
            )
        };
        assert!(!capture(a.clone()).same_data(&capture(make(&["x"], &other_body))));
        assert!(!capture(a.clone()).same_data(&capture(make(&["z"], &body))));
    }

    /// An object with the members `members` and a source holding the other
    /// data an object carries.
    fn object_with_source(
        members: Vec<(&str, Value)>,
        elements: Vec<Value>,
        hidden: &[&str],
        poisoned: Option<&[(&str, &str)]>,
    ) -> Value {
        let map: ObjectMap = members.into_iter().map(|(k, v)| (k.into(), v)).collect();
        let source = ObjectSource {
            entries: Vec::new().into(),
            captured: crate::eval::SourceScope::default(),
            body_members: HashSet::default(),
            is_open: true,
            is_abstract: false,
            type_name: None,
            type_identity: None,
            parent_type_names: Vec::new(),
            parent_type_identities: Vec::new(),
            entry_scopes: Vec::new(),
            evaluated_properties: map
                .keys()
                .cloned()
                .chain(hidden.iter().map(|name| Arc::from(*name)))
                .collect(),
            elements,
            mapping_value_types: Vec::new(),
            deprecated: IndexMap::new(),
            poisoned_members: poisoned.map(|members| {
                Arc::new(
                    members
                        .iter()
                        .map(|(name, message)| {
                            (
                                Arc::from(*name),
                                PoisonedMember {
                                    message: message.to_string(),
                                    rendered: false,
                                },
                            )
                        })
                        .collect(),
                )
            }),
            kind: ObjectKind::Object,
            is_parsed_json: false,
            prototype: None,
        };
        Value::Object(Arc::new(map), Some(Arc::new(source)))
    }

    /// The bare elements of an object are not in its member map, but `xml.Element`
    /// renders them, so objects that differ only there are not the same.
    #[test]
    fn objects_with_other_elements_are_not_the_same() {
        let with = |elements: Vec<Value>| {
            object_with_source(vec![("w", Value::Int(0))], elements, &[], None)
        };
        let a = with(vec![Value::Int(1), Value::Int(2)]);
        assert!(a.same_data(&with(vec![Value::Int(1), Value::Int(2)])));
        assert!(!a.same_data(&with(vec![Value::Int(1), Value::Int(3)])));
        assert!(!a.same_data(&with(vec![Value::Int(1)])));
        assert!(!a.same_data(&with(vec![])));
        assert!(!with(vec![]).same_data(&a));
        // An element is compared as deeply as a member is.
        let nested = |n| with(vec![object(vec![("v", Value::Int(n))])]);
        assert!(nested(1).same_data(&nested(1)));
        assert!(!nested(1).same_data(&nested(2)));
    }

    /// A member that failed to evaluate reports its error when read.
    #[test]
    fn objects_with_other_failed_members_are_not_the_same() {
        let with = |poisoned: Option<&[(&str, &str)]>| {
            object_with_source(vec![("w", Value::Int(0))], vec![], &[], poisoned)
        };
        let a = with(Some(&[("p", "boom")]));
        assert!(a.same_data(&with(Some(&[("p", "boom")]))));
        assert!(!a.same_data(&with(Some(&[("p", "bang")]))));
        assert!(!a.same_data(&with(Some(&[("q", "boom")]))));
        assert!(!a.same_data(&with(Some(&[("p", "boom"), ("q", "boom")]))));
        assert!(!a.same_data(&with(None)));
        assert!(!with(None).same_data(&a));
    }

    /// Hidden properties are not in the member map either, but a read of one
    /// finds it by name.
    #[test]
    fn objects_with_other_hidden_property_names_are_not_the_same() {
        let with =
            |hidden: &[&str]| object_with_source(vec![("w", Value::Int(0))], vec![], hidden, None);
        assert!(with(&["h"]).same_data(&with(&["h"])));
        assert!(!with(&["h"]).same_data(&with(&["g"])));
        assert!(!with(&["h"]).same_data(&with(&[])));
    }

    /// An object without a source has no elements and no failed members.
    #[test]
    fn an_object_without_a_source_is_the_same_as_one_with_an_empty_source() {
        let bare = object(vec![("w", Value::Int(0))]);
        let empty = object_with_source(vec![("w", Value::Int(0))], vec![], &[], None);
        assert!(bare.same_data(&empty));
        assert!(empty.same_data(&bare));
        let with_elements =
            object_with_source(vec![("w", Value::Int(0))], vec![Value::Null], &[], None);
        assert!(!bare.same_data(&with_elements));
        assert!(!with_elements.same_data(&bare));
        let failed = object_with_source(
            vec![("w", Value::Int(0))],
            vec![],
            &[],
            Some(&[("p", "boom")]),
        );
        assert!(!bare.same_data(&failed));
        assert!(!failed.same_data(&bare));
    }

    /// Lambdas inside what a lambda captured are compared by parameters and
    /// body only, also where they are the elements of an object.
    #[test]
    fn lambdas_among_the_elements_of_a_capture_compare_by_body_only() {
        let body = Arc::new(Expr::Ident("x".into()));
        let element = |n: i64| {
            object_with_source(
                vec![],
                vec![lambda_over(&body, vec![("n", Value::Int(n))])],
                &[],
                None,
            )
        };
        assert!(!element(1).same_data(&element(2)));
        let capturing = |n| lambda_over(&body, vec![("o", element(n))]);
        assert!(capturing(1).same_data(&capturing(2)));
    }

    /// Each level holds the one below it as an element twice, so comparing the
    /// elements without remembering which were compared takes exponential time.
    #[test]
    fn deeply_shared_elements_compare_in_linear_time() {
        fn tower(levels: usize) -> Value {
            let mut value = Value::Int(0);
            for _ in 0..levels {
                value = object_with_source(vec![], vec![value.clone(), value], &[], None);
            }
            value
        }
        assert!(tower(64).same_data(&tower(64)));
        assert!(!tower(64).same_data(&tower(63)));
    }

    #[test]
    fn objects_and_lists_with_other_keys_or_lengths_are_not_the_same() {
        let one = |key| object(vec![(key, Value::Int(1))]);
        assert!(one("x").same_data(&one("x")));
        assert!(!one("x").same_data(&one("y")));

        let list = |items: Vec<Value>| Value::List(ListValue::new(ListKind::List, items));
        let two = list(vec![Value::Int(1), Value::Int(2)]);
        assert!(two.same_data(&list(vec![Value::Int(1), Value::Int(2)])));
        assert!(!two.same_data(&list(vec![Value::Int(1), Value::Int(2), Value::Int(3)])));
        assert!(!list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]).same_data(&two));
    }

    /// `a` captures `shared`, so `shared` is first compared inside a capture,
    /// where the lambda in it only has to have the same body. Compared again
    /// as a member of its own, the lambda must also have captured the same.
    #[test]
    fn a_pair_found_equal_inside_a_capture_is_compared_again_outside_it() {
        let body = Arc::new(Expr::Ident("x".into()));
        let object_holding = |n: i64| {
            let shared = object(vec![("f", lambda_over(&body, vec![("n", Value::Int(n))]))]);
            let capturing = lambda_over(&body, vec![("shared", shared.clone())]);
            object(vec![("a", capturing), ("b", shared)])
        };
        assert!(object_holding(1).same_data(&object_holding(1)));
        assert!(!object_holding(1).same_data(&object_holding(2)));
    }

    /// Objects with the same members are not the same when one is a `Mapping`
    /// or an instance of a class and the other a `Dynamic`.
    #[cfg(feature = "native-io")]
    #[test]
    fn objects_of_other_kinds_are_not_the_same_with_the_same_members() {
        let source = "class C { k = 1 }\nd = new Dynamic { k = 1 }\nd2 = new Dynamic { k = 1 }\nm = new Mapping { [\"k\"] = 1 }\nc = new C {}\n";
        let module = crate::eval::Evaluator::new()
            .eval_source(source, std::path::Path::new("kinds.pkl"))
            .unwrap();
        let Value::Object(members, _) = &module else {
            panic!("expected an object");
        };
        assert!(members["d"].same_data(&members["d2"]));
        assert!(!members["d"].same_data(&members["m"]));
        assert!(!members["d"].same_data(&members["c"]));
    }
}
