use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::json;

use crate::parser::{Entry, Expr};

/// Captures the original AST entries and scope for an object, enabling
/// late binding: when this object is amended, its entries can be merged
/// with the overlay's entries and re-evaluated so that dependent
/// properties pick up overridden values.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSource {
    pub entries: Vec<Entry>,
    pub scope: IndexMap<String, Value>,
    /// Whether the class was declared `open` (allows adding new properties)
    pub is_open: bool,
    /// The class name this object was instantiated from (e.g., "Step", "Group").
    /// Used to inject `_type` in JSON output, matching pkl's output converter behavior.
    pub class_name: Option<String>,
}

/// A pkl runtime value.
///
/// Pkl's `Mapping` type (arbitrary key→value) is represented as `Object` when
/// keys are strings — which is the only case supported by JSON output. All
/// `new Mapping { ["key"] = ... }` expressions therefore produce `Object`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// Object (ordered string-keyed map). Represents both pkl objects and
    /// string-keyed Mappings.  The optional [`ObjectSource`] stores the
    /// original entry definitions so late binding works on amendment.
    Object(IndexMap<String, Value>, Option<Arc<ObjectSource>>),
    /// Listing (ordered list).
    List(Vec<Value>),
    /// Lambda function: param names + body expression + captured scope values
    Lambda(Vec<String>, Expr, IndexMap<String, Value>),
}

impl Value {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => json!(b),
            Value::Int(n) => json!(n),
            Value::Float(f) => json!(f),
            Value::String(s) => json!(s),
            Value::Object(map, src) => {
                let mut obj = serde_json::Map::new();
                // Inject _type for typed objects (instances of user-defined classes),
                // matching pkl's output converter convention.
                if let Some(s) = src
                    && let Some(ref name) = s.class_name
                    && !map.contains_key("_type")
                {
                    obj.insert(
                        "_type".to_string(),
                        json!(name.to_ascii_lowercase()),
                    );
                }
                for (k, v) in map {
                    obj.insert(k.clone(), v.to_json());
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

    pub fn as_object_mut(&mut self) -> Option<&mut IndexMap<String, Value>> {
        if let Value::Object(m, _) = self {
            Some(m)
        } else {
            None
        }
    }

    /// Merge `other` into `self`. For objects, other's keys win.
    pub fn merge(&mut self, other: Value) {
        match (self, other) {
            (Value::Object(base, _), Value::Object(overlay, _)) => {
                for (k, v) in overlay {
                    base.insert(k, v);
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
            serde_json::Value::String(s) => Value::String(s),
            serde_json::Value::Array(a) => Value::List(a.into_iter().map(Value::from).collect()),
            serde_json::Value::Object(o) => {
                let mut map = IndexMap::new();
                for (k, v) in o {
                    map.insert(k, Value::from(v));
                }
                Value::Object(map, None)
            }
        }
    }
}
