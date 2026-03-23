use indexmap::IndexMap;
use serde_json::json;

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
    /// string-keyed Mappings.
    Object(IndexMap<String, Value>),
    /// Listing (ordered list).
    List(Vec<Value>),
}

impl Value {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => json!(b),
            Value::Int(n) => json!(n),
            Value::Float(f) => json!(f),
            Value::String(s) => json!(s),
            Value::Object(map) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map {
                    obj.insert(k.clone(), v.to_json());
                }
                serde_json::Value::Object(obj)
            }
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
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

    pub fn as_object_mut(&mut self) -> Option<&mut IndexMap<String, Value>> {
        if let Value::Object(m) = self {
            Some(m)
        } else {
            None
        }
    }

    /// Merge `other` into `self`. For objects, other's keys win.
    pub fn merge(&mut self, other: Value) {
        match (self, other) {
            (Value::Object(base), Value::Object(overlay)) => {
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
                Value::Object(map)
            }
        }
    }
}
