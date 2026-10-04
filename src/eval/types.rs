use super::*;

/// Collect `@Deprecated` annotations from a list of entries into a map of
/// property name → optional message. Used to populate `ObjectSource.deprecated`
/// so field access can warn lazily, instead of warning eagerly when a module
/// or object body is evaluated.
pub(super) fn collect_deprecated(entries: &[Entry]) -> IndexMap<String, Option<String>> {
    let mut out: IndexMap<String, Option<String>> = IndexMap::new();
    for entry in entries {
        if let Entry::Property(prop) = entry {
            for ann in &prop.annotations {
                if ann.name != "Deprecated" {
                    continue;
                }
                let mut message = None;
                for e in &ann.body {
                    if let Entry::Property(p) = e
                        && p.name == "message"
                        && let Some(Expr::String(s)) = &p.value
                    {
                        message = Some(s.clone());
                    }
                }
                out.insert(prop.name.clone(), message);
            }
        }
    }
    out
}

/// Combine a base deprecation map with any `@Deprecated` annotations on a
/// list of overlay entries, with overlay winning on conflict. Used by the
/// amend/merge code paths so an amended object's `ObjectSource.deprecated`
/// reflects deprecations from both the base and the overlay.
pub(super) fn merge_deprecated(
    base: &IndexMap<String, Option<String>>,
    overlay_entries: &[Entry],
) -> IndexMap<String, Option<String>> {
    let mut out = base.clone();
    for (k, v) in collect_deprecated(overlay_entries) {
        out.insert(k, v);
    }
    out
}

/// Resolve return-type aliases while their definition scope is available.
/// Value::Lambda captures values, so type aliases would otherwise be lost at
/// invocation. Keep inference errors lazy until the selected branch is called.
pub(super) fn capture_method_result_types(expr: &mut Expr, scope: &Scope) {
    match expr {
        Expr::InferredNew(ty, entries) => {
            *expr = match inferred_new_type(ty, scope, 0) {
                Ok((name, params)) => Expr::New(Some(name), std::mem::take(entries), params),
                Err(error) => Expr::Throw(Box::new(Expr::String(error.to_string()))),
            };
        }
        Expr::If(_, then_expr, else_expr) => {
            capture_method_result_types(then_expr, scope);
            capture_method_result_types(else_expr, scope);
        }
        Expr::Let(_, _, body) | Expr::Trace(body) => capture_method_result_types(body, scope),
        _ => {}
    }
}

/// Resolve an implicit method-result constructor without dropping generic
/// arguments or the default alternative of a union.
pub(super) fn inferred_new_type(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    depth: usize,
) -> Result<(String, Vec<String>)> {
    use crate::parser::TypeExpr;
    if depth > 32 {
        return Err(Error::Eval("recursive inferred return type".into()));
    }
    match ty {
        TypeExpr::Nullable(inner) => inferred_new_type(inner, scope, depth + 1),
        TypeExpr::Named(name) => {
            let name = name.trim_start_matches('*');
            if let Some(alias) = scope.get_type_alias(name) {
                return inferred_new_type(alias, scope, depth + 1);
            }
            // Default union alternatives are stored as Named("*Type<...>").
            if name.contains('<') || name.ends_with('?') {
                let ty = crate::parser::parse_type_name(name)?;
                return inferred_new_type(&ty, scope, depth + 1);
            }
            Ok((name.to_string(), Vec::new()))
        }
        TypeExpr::Generic(name, args) => Ok((
            name.trim_start_matches('*').to_string(),
            args.iter()
                .enumerate()
                .flat_map(|(index, arg)| {
                    // The mapping constructor reserves one slot for its key
                    // type, followed by all value-type alternatives.
                    if index == 0 && matches!(name.as_str(), "Mapping" | "Map") {
                        return vec![display_type_expr(arg)];
                    }
                    let mut names = Vec::new();
                    collect_type_expr_class_names(arg, scope, &mut names, 0);
                    names
                })
                .collect(),
        )),
        TypeExpr::Union(types) => {
            let selected = types
                .iter()
                .find(|ty| is_default_type(ty))
                .ok_or_else(|| Error::Eval("Cannot tell which parent to amend".into()))?;
            inferred_new_type(selected, scope, depth + 1)
        }
        TypeExpr::Constrained(name, _) => {
            // Constraints store the underlying type as a runtime name. Parse
            // that name back into a type to retain any generic arguments.
            let ty = crate::parser::parse_type_name(name.trim_start_matches('*'))?;
            inferred_new_type(&ty, scope, depth + 1)
        }
    }
}

pub(super) fn type_default_value(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<Value> {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Constrained(base, _) if base.ends_with('?') => Some(Value::Null),
        TypeExpr::Constrained(base, _) => type_default_for_name(base, scope),
        TypeExpr::Nullable(_) => Some(Value::Null),
        TypeExpr::Named(name) if name == "Null" => Some(Value::Null),
        TypeExpr::Named(name) => {
            if let Some(value) = string_literal_type_value(name) {
                return Some(Value::String(value.to_string()));
            }
            if let Some(alias) = scope.get_type_alias(name).cloned() {
                return type_default_value(&alias, scope);
            }
            type_default_for_name(name, scope)
        }
        TypeExpr::Generic(name, _) => match name.as_str() {
            "Collection" | "List" | "Set" | "Listing" => Some(Value::List(Vec::new())),
            "Map" | "Mapping" => Some(Value::Object(Arc::new(IndexMap::new()), None)),
            _ => resolve_dotted(scope, name),
        },
        TypeExpr::Union(variants) => variants
            .iter()
            .find(|ty| is_default_type(ty))
            .and_then(|ty| type_default_value(ty, scope)),
    }
}

pub(super) fn type_default_for_name(name: &str, scope: &Scope) -> Option<Value> {
    let name = name.trim_start_matches('*').trim_end_matches('?');
    let base_name = name.split('<').next().unwrap_or(name);
    match base_name {
        "Null" => Some(Value::Null),
        "Collection" | "List" | "Set" | "Listing" => Some(Value::List(Vec::new())),
        "Map" | "Mapping" => Some(Value::Object(Arc::new(IndexMap::new()), None)),
        "String" | "Boolean" | "Bool" | "Int" | "Float" | "Number" | "Any" | "Dynamic"
        | "Duration" | "DataSize" | "Pair" | "Regex" => None,
        other => resolve_dotted(scope, other),
    }
}

pub(super) fn nullable_inner_default(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<Value> {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Nullable(inner) => type_default_value(inner, scope),
        TypeExpr::Constrained(name, _) if name.ends_with('?') => {
            type_default_for_name(name.trim_end_matches('?'), scope)
        }
        _ => None,
    }
}

pub(super) fn type_is_listing(ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) | TypeExpr::Generic(name, _) => name == "Listing",
        TypeExpr::Nullable(inner) => type_is_listing(inner),
        TypeExpr::Constrained(name, _) => {
            name.trim_end_matches('?') == "Listing" || name.starts_with("Listing<")
        }
        TypeExpr::Union(variants) => variants.iter().any(type_is_listing),
    }
}

pub(super) fn entries_are_listing_amendment(entries: &[Entry]) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::Elem(_) => true,
        Entry::ForGenerator(generator) => entries_are_listing_amendment(&generator.body),
        Entry::WhenGenerator(generator) => {
            entries_are_listing_amendment(&generator.body)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(|body| entries_are_listing_amendment(body))
        }
        _ => false,
    })
}

pub(super) fn resolve_dotted(scope: &Scope, name: &str) -> Option<Value> {
    let parts: Vec<&str> = name.split('.').collect();
    let mut val = scope.get(parts[0])?.clone();
    for part in &parts[1..] {
        val = match val {
            Value::Object(ref map, _) => map.get(*part)?.clone(),
            _ => return None,
        };
    }
    Some(val)
}

pub(super) fn has_modifier(mods: &[Modifier], target: Modifier) -> bool {
    mods.contains(&target)
}

pub(super) fn should_render_property_value(prop: &Property, value: &Value) -> bool {
    prop.value.is_some() || prop.body.is_some() || !matches!(value, Value::Null | Value::Object(..))
}

pub(super) fn module_is_abstract(module: &Module) -> bool {
    module
        .annotations
        .iter()
        .any(|annotation| annotation.name == "pklr:module:Abstract")
}

pub(super) fn is_unresolved_template_error(message: &str) -> bool {
    message.contains("undefined variable") || message.contains("field not found")
}

pub(super) fn require_str_arg<'a>(args: &'a [Value], idx: usize, method: &str) -> Result<&'a str> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(Error::Eval(format!(
            "{method}() requires a String argument, got {}",
            value_type_name(other)
        ))),
        None => Err(Error::Eval(format!(
            "{method}() requires {} argument(s)",
            idx + 1
        ))),
    }
}

pub(super) fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "Boolean",
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::String(_) => "String",
        Value::Object(..) => "Object",
        Value::List(_) => "List",
        Value::Lambda(..) => "Function",
    }
}

pub(super) fn value_to_key(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Float(f) => Ok(f.to_string()),
        Value::Object(_, _) | Value::List(_) | Value::Lambda(..) | Value::Null => {
            Ok(value_to_display(v))
        }
    }
}

pub(super) fn value_to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => s.clone(),
        _ => format!("{v:?}"),
    }
}

pub(super) fn string_literal_type_value(name: &str) -> Option<&str> {
    name.strip_prefix('*')
        .unwrap_or(name)
        .strip_prefix('"')?
        .strip_suffix('"')
}

pub(super) fn is_default_type(ty: &crate::parser::TypeExpr) -> bool {
    matches!(ty, crate::parser::TypeExpr::Named(name) if name.starts_with('*'))
}

/// Format a TypeExpr for user-facing error messages.
pub(super) fn display_type_expr(ty: &crate::parser::TypeExpr) -> String {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Nullable(inner) => format!("{}?", display_type_expr(inner)),
        TypeExpr::Union(variants) => variants
            .iter()
            .map(display_type_expr)
            .collect::<Vec<_>>()
            .join("|"),
        TypeExpr::Generic(name, args) => {
            let args_str: Vec<_> = args.iter().map(display_type_expr).collect();
            format!("{}<{}>", name, args_str.join(", "))
        }
        TypeExpr::Constrained(base, _) => format!("{base}(... )"),
    }
}

/// Check if a value matches a Pkl type expression (non-constrained types only).
/// For constrained types, use `Evaluator::eval_type_check` instead.
pub(super) fn value_is_type(val: &Value, ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) if string_literal_type_value(name).is_some() => {
            matches!(val, Value::String(actual) if Some(actual.as_str()) == string_literal_type_value(name))
        }
        TypeExpr::Named(name) => match name.strip_prefix('*').unwrap_or(name) {
            "Null" => is_null_value(val),
            "Boolean" | "Bool" => matches!(val, Value::Bool(_)),
            "Int" => matches!(val, Value::Int(_)),
            "Float" => matches!(val, Value::Float(_)),
            "Number" => matches!(val, Value::Int(_) | Value::Float(_)),
            "String" => matches!(val, Value::String(_)),
            "List" | "Listing" | "Set" => matches!(val, Value::List(_)),
            "Map" | "Mapping" | "Object" | "Dynamic" => matches!(val, Value::Object(..)),
            "Function" => matches!(val, Value::Lambda(..)),
            "Any" => true,
            _ => {
                // Unknown type name -- could be a class; treat objects as matching
                matches!(val, Value::Object(..))
            }
        },
        TypeExpr::Nullable(inner) => is_null_value(val) || value_is_type(val, inner),
        TypeExpr::Union(variants) => variants.iter().any(|v| value_is_type(val, v)),
        TypeExpr::Generic(name, _) => {
            // Check the base type, ignore type parameters
            match name.as_str() {
                "List" | "Listing" | "Set" => matches!(val, Value::List(_)),
                "Map" | "Mapping" => matches!(val, Value::Object(..)),
                _ => matches!(val, Value::Object(..)),
            }
        }
        TypeExpr::Constrained(base, _) => {
            // Just check the base type; constraint requires async eval
            value_is_named_type(val, base)
        }
    }
}

pub(super) fn value_is_named_type(val: &Value, name: &str) -> bool {
    if name.ends_with('?') && is_null_value(val) {
        return true;
    }
    value_is_type(
        val,
        &crate::parser::TypeExpr::Named(
            name.trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(name)
                .to_string(),
        ),
    )
}

/// Check a value against a user-defined class that is available in scope.
///
/// Class defaults and instances carry their concrete class and parent class
/// names in `ObjectSource`. Resolve the requested name first so unresolved
/// built-in object types can continue through `value_is_type`.
pub(super) fn value_is_class_type(val: &Value, name: &str, scope: &Scope) -> Option<bool> {
    let expected_name = name.strip_prefix('*').unwrap_or(name);
    let Some(Value::Object(_, Some(expected_source))) = resolve_dotted(scope, expected_name) else {
        return None;
    };
    let expected_type_identity = expected_source.type_identity.as_ref()?;

    let Value::Object(_, Some(source)) = val else {
        return Some(false);
    };
    let Some(actual_type_identity) = source.type_identity.as_ref() else {
        return Some(false);
    };

    Some(
        actual_type_identity == expected_type_identity
            || source
                .parent_type_identities
                .iter()
                .any(|parent| parent == expected_type_identity),
    )
}

/// Whether `name` (optionally `*`-prefixed) is a built-in type checked at
/// runtime, or a string-literal type. An alias of the same name in scope
/// takes precedence over it.
pub(super) fn is_builtin_type_name(name: &str) -> bool {
    string_literal_type_value(name).is_some()
        || matches!(
            name.strip_prefix('*').unwrap_or(name),
            "Null"
                | "Boolean"
                | "Bool"
                | "Int"
                | "Float"
                | "Number"
                | "String"
                | "List"
                | "Listing"
                | "Set"
                | "Map"
                | "Mapping"
                | "Object"
                | "Dynamic"
                | "Function"
                | "Any"
        )
}

pub(super) fn type_is_runtime_checkable(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    type_is_runtime_checkable_inner(ty, scope, &mut Vec::new())
}

/// Type aliases are resolved (following chains, guarding against cycles)
/// before deciding, so an alias is checkable exactly when its target is.
fn type_is_runtime_checkable_inner(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    resolving: &mut Vec<String>,
) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => {
            let runtime_name = name.strip_prefix('*').unwrap_or(name);
            // An alias may shadow a built-in name (`typealias String = ...`),
            // so it is resolved first, as `eval_type_check` does.
            if let Some(alias) = scope.get_type_alias(runtime_name) {
                if resolving.iter().any(|seen| seen == runtime_name) {
                    return false;
                }
                resolving.push(runtime_name.to_string());
                let checkable = type_is_runtime_checkable_inner(alias, scope, resolving);
                resolving.pop();
                return checkable;
            }
            if is_builtin_type_name(name) {
                return true;
            }
            resolve_dotted(scope, runtime_name).is_some()
        }
        TypeExpr::Nullable(inner) => type_is_runtime_checkable_inner(inner, scope, resolving),
        TypeExpr::Union(variants) => variants
            .iter()
            .all(|variant| type_is_runtime_checkable_inner(variant, scope, resolving)),
        // Only collection generics have a runtime representation to check;
        // other generics (`Function1<...>`, `Pair<...>`) are not modeled.
        TypeExpr::Generic(name, _) => {
            matches!(
                name.as_str(),
                "List" | "Listing" | "Set" | "Map" | "Mapping"
            )
        }
        TypeExpr::Constrained(base, _) => {
            let runtime_name = base
                .trim_start_matches('*')
                .trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(base);
            type_is_runtime_checkable_inner(&TypeExpr::Named(runtime_name.into()), scope, resolving)
        }
    }
}

pub(super) fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub(super) fn is_null_value(value: &Value) -> bool {
    matches!(value, Value::Null)
}

pub(super) fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (left, right) if is_null_value(left) && is_null_value(right) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
        (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

pub(super) fn add_values(l: Value, r: Value) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
        (Value::String(a), Value::String(b)) => Ok(Value::String(a + &b)),
        (Value::List(mut a), Value::List(b)) => {
            a.extend(b);
            Ok(Value::List(a))
        }
        (Value::Object(mut a, _), Value::Object(b, _)) => {
            Arc::make_mut(&mut a).extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
            Ok(Value::Object(a, None))
        }
        (l, r) => Err(Error::Eval(format!("cannot add {:?} and {:?}", l, r))),
    }
}

pub(super) fn arithmetic(
    l: Value,
    r: Value,
    fi: impl Fn(i64, i64) -> Result<i64>,
    ff: impl Fn(f64, f64) -> Result<f64>,
) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(fi(a, b)?)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(ff(a, b)?)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(ff(a as f64, b)?)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(ff(a, b as f64)?)),
        (l, r) => Err(Error::Eval(format!(
            "arithmetic type mismatch: {:?} vs {:?}",
            l, r
        ))),
    }
}

pub(super) fn compare(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    Ok(Value::Bool(value_cmp(&l, &r)? == ord))
}

pub(super) fn compare_or_eq(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    let c = value_cmp(&l, &r)?;
    Ok(Value::Bool(c == ord || c == std::cmp::Ordering::Equal))
}

pub(super) fn value_cmp(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => {
            Ok(x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
        }
        (Value::Int(x), Value::Float(y)) => Ok((*x as f64)
            .partial_cmp(y)
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::Float(x), Value::Int(y)) => Ok(x
            .partial_cmp(&(*y as f64))
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::String(x), Value::String(y)) => Ok(x.cmp(y)),
        _ => Err(Error::Eval(format!("cannot compare {:?} and {:?}", a, b))),
    }
}

pub(super) fn merge_values(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut b, base_src), Value::Object(o, overlay_src)) => {
            let b_map = Arc::make_mut(&mut b);
            for (k, v) in o.iter() {
                if let Some(existing) = b_map.shift_remove(k) {
                    b_map.insert(k.clone(), merge_values(existing, v.clone()));
                } else {
                    b_map.insert(k.clone(), v.clone());
                }
            }
            // Keep the base's source (entries/scope for late binding), but when it
            // carries no class identity, inherit the overlay's. This preserves a
            // concrete value's type when it is merged onto a typeless default
            // template (e.g. a union-typed Mapping value).
            let src = match (base_src, overlay_src) {
                (Some(b), Some(o)) if b.type_name.is_none() && o.type_name.is_some() => {
                    let mut nb = (*b).clone();
                    nb.type_name = o.type_name.clone();
                    nb.type_identity = o.type_identity.clone();
                    nb.parent_type_names = o.parent_type_names.clone();
                    nb.parent_type_identities = o.parent_type_identities.clone();
                    Some(Arc::new(nb))
                }
                (Some(b), _) => Some(b),
                (None, o) => o,
            };
            Value::Object(b, src)
        }
        (_, overlay) => overlay,
    }
}

pub(super) fn make_unit_object(value: Value, unit: &str) -> Value {
    let mut map = IndexMap::new();
    map.insert("value".to_string(), value);
    map.insert("unit".to_string(), Value::String(unit.to_string()));
    Value::Object(Arc::new(map), None)
}
