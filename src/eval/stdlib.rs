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
        type_identity: matches!(type_name, "RegexMatch" | "Pair")
            .then(|| format!("pkl:base#{type_name}")),
        parent_type_names: Vec::new(),
        parent_type_identities: Vec::new(),
        entry_scopes: Vec::new(),
        evaluated_properties: members.keys().cloned().collect(),
        elements: Vec::new(),
        mapping_value_types: Vec::new(),
        deprecated: IndexMap::new(),
        poisoned_members: None,
        declared_as: None,
        kind: ObjectKind::Object,
        is_parsed_json: false,
        is_module: false,
        prototype: None,
    };
    Value::Object(Arc::new(members), Some(Arc::new(source)))
}

/// A `Dynamic` with the given members, such as `toDynamic()` builds. Its body
/// spreads the members from a bound variable, so an amendment of it, which
/// rebuilds the object from its body, keeps them and can replace any of them.
pub(super) fn dynamic_object(
    members: Arc<ObjectMap>,
    entry_key: impl Fn(&Arc<str>) -> Value,
) -> Value {
    const MEMBERS: &str = "\0pklr:members";
    const KEYS: &str = "\0pklr:member-keys";
    let Value::Object(_, Some(source)) = typed_object("Dynamic", ObjectMap::default()) else {
        unreachable!("typed_object builds an object with a source")
    };
    let mut source = Arc::unwrap_or_clone(source);
    source.type_name = None;
    source.kind = ObjectKind::Dynamic;
    let mut scope = Scope::default();
    scope.set(MEMBERS, Value::Object(members.clone(), None));
    // A string key is a property, read from the bound variable: an amendment
    // replaces it in place, and its body reads it by name. Any other key is an
    // entry (`Map(1, "a").toDynamic()` is `[1] = "a"`), keyed by its value.
    // Only an entry's key is resolved (to its original value, which the
    // storage key cannot always give back), and only when there is one, so the
    // usual object of properties builds no key list.
    let is_entry = |key: &str| key.starts_with(crate::value::MAPPING_KEY_PREFIX);
    let keys: Vec<Option<Value>> = if members.keys().any(|key| is_entry(key)) {
        members
            .keys()
            .map(|key| is_entry(key).then(|| entry_key(key)))
            .collect()
    } else {
        Vec::new()
    };
    let is_property = |i: usize| keys.get(i).is_none_or(Option::is_none);
    if !keys.is_empty() {
        let list = keys
            .iter()
            .map(|key| key.clone().unwrap_or(Value::Null))
            .collect::<Vec<_>>();
        scope.set(KEYS, Value::List(list.into()));
    }
    let member = |name: &Arc<str>| {
        Expr::Index(
            Box::new(Expr::Ident(MEMBERS.into())),
            Box::new(Expr::String(name.clone())),
        )
    };
    source.entries = members
        .keys()
        .enumerate()
        .map(|(i, name)| {
            if is_property(i) {
                Entry::Property(Arc::new(Property {
                    annotations: Vec::new(),
                    modifiers: Vec::new(),
                    name: name.clone(),
                    type_ann: None,
                    value: Some(member(name)),
                    body: None,
                    is_method: false,
                }))
            } else {
                Entry::DynProperty(
                    Expr::Index(
                        Box::new(Expr::Ident(KEYS.into())),
                        Box::new(Expr::Int(i as i64)),
                    ),
                    member(name),
                )
            }
        })
        .collect::<Vec<_>>()
        .into();
    source.captured = SourceScope::lazy(&scope, Vec::new(), Vec::new());
    source.evaluated_properties = members
        .keys()
        .enumerate()
        .filter(|(i, _)| is_property(*i))
        .map(|(_, name)| name.clone())
        .collect();
    Value::Object(members, Some(Arc::new(source)))
}

/// A `Class` value for the class `type_name` with the given identity. Classes
/// compare by identity, so it carries no members.
pub(super) fn class_value(type_name: &str, type_identity: String) -> Value {
    let Value::Object(_, Some(source)) = typed_object(type_name, ObjectMap::default()) else {
        unreachable!("typed_object builds an object with a source")
    };
    let mut source = Arc::unwrap_or_clone(source);
    source.type_identity = Some(type_identity);
    source.kind = ObjectKind::Class;
    Value::Object(Arc::default(), Some(Arc::new(source)))
}

/// A `TypeAlias` value with the given identity.
/// The member of a `TypeAlias` value that holds the alias's identity.
pub(crate) const TYPE_ALIAS_IDENTITY_KEY: &str = "\0alias";

/// The member of a `TypeAlias` value that holds the name it was declared with.
pub(crate) const TYPE_ALIAS_NAME_KEY: &str = "\0name";

pub(super) fn type_alias_value(identity: String, name: &str) -> Value {
    let Value::Object(_, Some(source)) = typed_object("TypeAlias", ObjectMap::default()) else {
        unreachable!("typed_object builds an object with a source")
    };
    let mut source = Arc::unwrap_or_clone(source);
    source.type_name = Some("TypeAlias".into());
    // The built-in class's identity tells this value from an instance of a
    // user class named `TypeAlias`, and `getClass()` is shared.
    source.type_identity = Some("pkl:base#TypeAlias".into());
    // The alias's own identity is a member, so it survives being stored in a
    // `Map` and tells one alias from another when values are compared.
    let mut members = ObjectMap::default();
    members.insert(
        TYPE_ALIAS_IDENTITY_KEY.into(),
        Value::String(identity.into()),
    );
    members.insert(TYPE_ALIAS_NAME_KEY.into(), Value::String(name.into()));
    Value::Object(Arc::new(members), Some(Arc::new(source)))
}

/// The `Class` value a standard library class name stands for, when it is not
/// bound in scope: the classes `getClass()` can answer.
pub(super) fn stdlib_class(name: &str) -> Option<Value> {
    const NAMES: &[&str] = &[
        "Null", "Boolean", "Int", "Float", "String", "Duration", "DataSize", "Regex", "List",
        "Listing", "Set", "Mapping", "Dynamic", "Class",
    ];
    NAMES
        .contains(&name)
        .then(|| class_value(name, format!("pkl:base#{name}")))
}

/// The marker `seed_builtins` binds to `name`, shared by every scope so a
/// binding can be told from a user's string of the same contents by identity.
pub(super) fn builtin_marker(name: &str) -> Value {
    static MARKERS: std::sync::LazyLock<Vec<(&'static str, std::sync::Arc<str>)>> =
        std::sync::LazyLock::new(|| {
            BUILTIN_MARKER_NAMES
                .iter()
                .map(|name| (*name, std::sync::Arc::from(*name)))
                .collect()
        });
    match MARKERS.iter().find(|(known, _)| *known == name) {
        Some((_, marker)) => Value::String(marker.clone()),
        None => Value::String(name.into()),
    }
}

/// The names `seed_builtins` binds to a marker string of their own name.
pub(super) const BUILTIN_MARKER_NAMES: &[&str] = &[
    "Regex",
    "Dynamic",
    "Annotation",
    "Duration",
    "DataSize",
    "IntSeq",
    "Pair",
];

/// The `Class` value for a name `seed_builtins` bound to its marker
/// (`Duration`, `DataSize`, `Regex`, `Dynamic`), when `value` is that very
/// marker. A user's string with the same contents is a different allocation
/// and is left alone.
pub(super) fn builtin_marker_class(name: &str, value: &Value) -> Option<Value> {
    if !matches!(name, "Duration" | "DataSize" | "Regex" | "Dynamic") {
        return None;
    }
    match (value, builtin_marker(name)) {
        (Value::String(got), Value::String(marker)) if std::sync::Arc::ptr_eq(got, &marker) => {
            stdlib_class(name)
        }
        _ => None,
    }
}

/// The identity of a module's class, which the `module` binding carries and
/// `getClass()` hands back: the module's name, then where it lives, so two
/// modules of the same name are different classes.
pub(super) fn module_class_identity(name: &str, namespace: &str) -> String {
    format!("{MODULE_IDENTITY_PREFIX}{name}\0{namespace}")
}

const MODULE_IDENTITY_PREFIX: &str = "\0module:";

/// The name a module class identity (see `module_class_identity`) carries.
pub(crate) fn module_class_name(identity: &str) -> Option<&str> {
    let rest = identity.strip_prefix(MODULE_IDENTITY_PREFIX)?;
    Some(rest.split_once('\0').map_or(rest, |(name, _)| name))
}

/// The error for a `getClass()` pklr cannot answer faithfully.
pub(super) fn get_class_unsupported(what: &str) -> Error {
    Error::Eval(format!(
        "getClass() is not supported on {what}: pklr does not track its class"
    ))
}

/// `Any.getClass()`: the `Class` of a value. A class from the standard library
/// is identified as `pkl:base#Name`. A `Map`, and a `Dynamic` that a method
/// built, have no source and are not told apart (see `object_kinds_match`), so
/// they are an error rather than a guess.
pub(super) fn get_class(value: &Value) -> Result<Value> {
    let (name, identity) = match value {
        // A module's class is a `ModuleClass`, which `toString()` shows by the
        // module's name.
        Value::Object(_, Some(source))
            if source.is_module
                && source
                    .type_identity
                    .as_deref()
                    .is_some_and(|identity| module_class_name(identity).is_some()) =>
        {
            ("ModuleClass", source.type_identity.clone())
        }
        Value::Object(_, Some(source)) if source.kind == ObjectKind::Class => ("Class", None),
        Value::Object(_, Some(source)) if source.kind == ObjectKind::Mapping => ("Mapping", None),
        Value::Object(_, Some(source)) => match source.type_name.as_deref() {
            Some(name) => (name, source.type_identity.clone()),
            None => ("Dynamic", None),
        },
        Value::Object(_, None) => return Err(get_class_unsupported("a Map or a computed Dynamic")),
        Value::List(items) => match items.kind() {
            ListKind::List if items.is_int_seq() => ("IntSeq", None),
            ListKind::List => ("List", None),
            ListKind::Listing => ("Listing", None),
            ListKind::Set => ("Set", None),
            ListKind::Dynamic => ("Dynamic", None),
        },
        other => (other.type_name(), None),
    };
    Ok(class_value(
        name,
        identity.unwrap_or_else(|| format!("pkl:base#{name}")),
    ))
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
