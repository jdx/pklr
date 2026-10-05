//! Properties and methods of pkl:base's built-in types.
//!
//! [`Evaluator::stdlib_property`] and [`Evaluator::stdlib_method`] answer a
//! member access on a built-in value, or return `None` when the member is not
//! one the type declares so the caller can fall back to its own lookup.

use super::*;

#[allow(dead_code)] // Activated when the compatibility formatter is migrated.
mod dragon;
mod fdlibm;
mod math;
mod numbers;
mod regex;
pub(crate) mod render;
pub(crate) mod string;
mod units;

pub(crate) use numbers::{
    binary_op, logical_left, logical_not, logical_right, negate, units_equal,
};
pub(crate) use regex::compile as compile_regex;
pub(crate) use render::render_value;
use render::{render_value_limited, to_pkl_string};

/// Longest rendering of a value quoted in an error message, as in pkl.
const ERROR_VALUE_LIMIT: usize = 80;

/// An evaluation error carrying pkl's message and the program values it
/// lists under the message (`String: "abc"`).
pub(super) fn error_with_values(message: impl Into<String>, values: &[(&str, &Value)]) -> Error {
    let mut message = message.into();
    let width = values.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    for (name, value) in values {
        message.push('\n');
        message.push_str(name);
        message.extend(std::iter::repeat_n(' ', width - name.len()));
        message.push_str(": ");
        message.push_str(&render_value_limited(value, ERROR_VALUE_LIMIT));
    }
    Error::Eval(message)
}

/// An instance of the built-in class `type_name` with the given members.
pub(super) fn typed_object(type_name: &str, members: ObjectMap) -> Value {
    let source = ObjectSource {
        entries: Vec::new().into(),
        captured: SourceScope::default(),
        body_members: HashSet::default(),
        is_open: false,
        is_abstract: false,
        type_name: Some(type_name.to_string()),
        type_identity: (type_name == "RegexMatch").then(|| "pkl:base#RegexMatch".to_string()),
        parent_type_names: Vec::new(),
        parent_type_identities: Vec::new(),
        entry_scopes: Vec::new(),
        evaluated_properties: members.keys().map(|k| k.to_string()).collect(),
        elements: Vec::new(),
        mapping_value_types: Vec::new(),
        deprecated: IndexMap::new(),
        poisoned_members: None,
        kind: ObjectKind::Object,
        is_parsed_json: false,
        prototype: None,
    };
    Value::Object(Arc::new(members), Some(Arc::new(source)))
}

fn is_builtin_regex_match(value: &Value) -> bool {
    matches!(value, Value::Object(_, Some(source)) if source.type_identity.as_deref() == Some("pkl:base#RegexMatch"))
}

fn is_typed_object(value: &Value, type_name: &str) -> bool {
    matches!(value, Value::Object(_, Some(source)) if source.type_name.as_deref() == Some(type_name))
}

/// pkl's error for a value of the wrong type.
pub(super) fn type_mismatch(expected: &str, actual: &Value) -> Error {
    error_with_values(
        format!(
            "Expected value of type `{expected}`, but got type `{}`.",
            actual.type_name()
        ),
        &[("Value", actual)],
    )
}

/// pkl's error for an operator applied to operands it does not support.
pub(super) fn operator_not_defined(op: &str, left: &Value, right: &Value) -> Error {
    error_with_values(
        format!(
            "Operator `{op}` is not defined for operand types `{}` and `{}`.",
            left.type_name(),
            right.type_name()
        ),
        &[("Left operand", left), ("Right operand", right)],
    )
}

/// pkl's error for a negative count.
pub(super) fn expected_positive(n: i64) -> Error {
    Error::Eval(format!("Expected a positive number, but got `{n}`."))
}

/// The arguments of a built-in method, checked against its declared types.
pub(super) struct Args<'a> {
    method: &'a str,
    args: &'a [Value],
}

impl<'a> Args<'a> {
    fn get(&self, index: usize) -> Result<&'a Value> {
        self.args.get(index).ok_or_else(|| {
            Error::Eval(format!(
                "Too few arguments for method `{}`: expected more than {}.",
                self.method,
                self.args.len()
            ))
        })
    }

    pub(super) fn string(&self, index: usize) -> Result<&'a str> {
        match self.get(index)? {
            Value::String(s) => Ok(s),
            other => Err(type_mismatch("String", other)),
        }
    }

    pub(super) fn int(&self, index: usize) -> Result<i64> {
        match self.get(index)? {
            Value::Int(n) => Ok(*n),
            other => Err(type_mismatch("Int", other)),
        }
    }

    pub(super) fn function(&self, index: usize) -> Result<&'a Value> {
        match self.get(index)? {
            lambda @ Value::Lambda(..) => Ok(lambda),
            other => Err(type_mismatch("Function", other)),
        }
    }

    pub(super) fn value(&self, index: usize) -> Result<&'a Value> {
        self.get(index)
    }
}

/// Check that a built-in method got exactly `expected` arguments.
pub(super) fn check_arity(args: &[Value], expected: usize) -> Result<()> {
    if args.len() == expected {
        return Ok(());
    }
    Err(Error::Eval(format!(
        "Expected {expected} function arguments but got {}.",
        args.len()
    )))
}

/// `value[key]` for a built-in value, or `None` if `value` is not one
/// subscripts apply to here.
pub(super) fn index(value: &Value, key: &Value) -> Option<Result<Value>> {
    match value {
        Value::String(s) => Some(string::subscript(s, key)),
        _ => None,
    }
}

/// The object for the standard library module `pkl:<name>`, for the modules
/// implemented here.
pub(super) fn module(name: &str) -> Option<Value> {
    match name {
        "math" => Some(math::module()),
        _ => None,
    }
}

impl Evaluator {
    /// `value.toString()`, as string interpolation uses it: a class that
    /// defines `toString()` gets to choose its text.
    pub(super) fn value_to_string(&mut self, value: &Value, depth: usize) -> Result<String> {
        if let Value::Object(members, _) = value {
            if is_builtin_regex_match(value)
                && let Some(Value::String(s)) = members.get("value")
            {
                return Ok(s.to_string());
            }
            if let Some(Value::Lambda(params, ..)) = members.get("toString")
                && params.is_empty()
                && let Some(result) = self.eval_object_method_call(value, "toString", &[], depth)?
            {
                return Ok(to_pkl_string(&result));
            }
        }
        Ok(to_pkl_string(value))
    }

    /// The built-in property `name` of `value`, or `None` if `value`'s type
    /// declares no such property.
    #[inline(never)]
    pub(super) fn stdlib_property(&mut self, value: &Value, name: &str) -> Option<Result<Value>> {
        match value {
            Value::String(s) => string::property(s, name),
            Value::Regex(regex) => regex::property(regex, name),
            Value::Int(n) => numbers::int_property(*n, name),
            Value::Float(f) => numbers::float_property(*f, name),
            Value::Duration(d) => units::duration_property(d, name),
            Value::DataSize(d) => units::data_size_property(d, name),
            _ => None,
        }
    }

    /// Call the built-in method `name` of `value`, or return `None` if
    /// `value`'s type declares no such method.
    #[inline(never)]
    pub(super) fn stdlib_method(
        &mut self,
        value: &Value,
        name: &str,
        args: &[Value],
        depth: usize,
    ) -> Option<Result<Value>> {
        if name == "toString" {
            // A class's own `toString()` is called like any other method.
            if let Value::Object(members, _) = value
                && matches!(members.get("toString"), Some(Value::Lambda(..)))
            {
                return None;
            }
            return Some(check_arity(args, 0).and_then(|()| {
                Ok(Value::String(match value {
                    Value::String(s) => Arc::clone(s),
                    _ => self.value_to_string(value, depth)?.into(),
                }))
            }));
        }
        match value {
            Value::String(s) => self.string_method(s, name, args, depth),
            Value::Regex(regex) => self.regex_method(regex, name, args),
            Value::Int(n) => numbers::int_method(*n, name, args),
            Value::Float(f) => numbers::float_method(*f, name, args),
            Value::Bool(b) => numbers::bool_method(*b, name, args),
            Value::Duration(d) => units::duration_method(d, name, args),
            Value::DataSize(d) => units::data_size_method(d, name, args),
            Value::Object(..) if is_typed_object(value, math::MODULE) => math::call(name, args),
            _ => None,
        }
    }

    /// Call a lambda that must return a `Boolean`, as a predicate passed to
    /// a built-in method.
    pub(super) fn call_predicate(
        &mut self,
        lambda: &Value,
        args: &[Value],
        depth: usize,
    ) -> Result<bool> {
        match self.invoke_lambda(lambda, args, depth)? {
            Value::Bool(b) => Ok(b),
            other => Err(type_mismatch("Boolean", &other)),
        }
    }
}
