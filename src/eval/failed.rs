//! Members of a `Listing` or `Mapping` that failed, kept until they are read.
//!
//! pkl evaluates and type checks a `Listing` element or a `Mapping` value only
//! when it is read, so `.length` or an unread element never raises its error.
//! pklr evaluates both when the collection is made; a member whose evaluation
//! or type check raises an error is kept as a [`Value::Failed`] instead of
//! failing the collection, and the error is raised when the member is read or
//! when an operation reads every member.

use super::*;

/// Whether an error is a failure of the member itself, which pkl would raise
/// only when the member is read. The others belong to evaluation as a whole:
/// a name that a retry may bind later, the recursion limit that moves
/// evaluation to a larger stack, and cancellation.
fn is_member_failure(error: &Error) -> bool {
    match error {
        Error::Eval(message) => {
            !message.starts_with("undefined variable")
                && !message.starts_with("maximum recursion depth")
                && !message.contains("cancelled")
                && !message.contains("canceled")
        }
        _ => false,
    }
}

/// The members of `value` that a read of the whole collection raises, as the
/// first failed one.
fn failed_member(value: &Value) -> Option<&Arc<str>> {
    fn failed(member: &Value) -> Option<&Arc<str>> {
        match member {
            Value::Failed(message) => Some(message),
            _ => None,
        }
    }
    match value {
        Value::List(items) => items.iter().find_map(failed),
        Value::Object(map, source)
            if source
                .as_ref()
                .is_none_or(|source| source.kind == ObjectKind::Mapping) =>
        {
            map.values().find_map(failed)
        }
        _ => None,
    }
}

impl Evaluator {
    /// The failed member `error` stands for, when it belongs to a member and
    /// not to the evaluation around it.
    pub(super) fn defer_member_error(&mut self, result: Result<Value>) -> Result<Value> {
        match result {
            Err(Error::Eval(message)) if is_member_failure(&Error::Eval(message.clone())) => {
                self.failed_seen = true;
                Ok(Value::Failed(message.into()))
            }
            other => other,
        }
    }

    /// The error a failed member of `value` raises, for an operation that
    /// reads every member. `length`, `isEmpty` and the keys of a mapping read
    /// none.
    pub(super) fn raise_failed_members(&self, value: &Value, member: &str) -> Result<()> {
        if !self.failed_seen
            || matches!(
                member,
                "length"
                    | "isEmpty"
                    | "keys"
                    | "containsKey"
                    | "getOrNull"
                    | "first"
                    | "firstOrNull"
                    | "last"
                    | "lastOrNull"
                    | "getOrDefault"
            )
        {
            return Ok(());
        }
        match failed_member(value) {
            Some(message) => Err(Error::Eval(message.to_string())),
            None => Ok(()),
        }
    }
}

pub(super) fn type_holds_lazy(ty: &crate::parser::TypeExpr) -> bool {
    ty.holds_lazy_collection()
}

/// The alternatives of a union, through nested unions and `?`.
pub(super) fn union_alternatives<'t>(
    ty: &'t crate::parser::TypeExpr,
    out: &mut Vec<&'t crate::parser::TypeExpr>,
) {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Union(alternatives) => {
            for alternative in alternatives {
                union_alternatives(alternative, out);
            }
        }
        TypeExpr::Nullable(inner) => union_alternatives(inner, out),
        other => out.push(other),
    }
}

/// Whether `ty` names only primitive types and the standard collections, which
/// mean the same wherever they are read.
pub(super) fn names_only_builtins(ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => matches!(
            name.as_str(),
            "String" | "Int" | "Float" | "Number" | "Boolean" | "Any"
        ),
        TypeExpr::Nullable(inner) => names_only_builtins(inner),
        TypeExpr::Union(alternatives) => alternatives.iter().all(names_only_builtins),
        TypeExpr::Generic(name, args) => {
            matches!(
                name.trim_start_matches('*'),
                "Listing" | "Mapping" | "List" | "Map" | "Set"
            ) && args.iter().all(names_only_builtins)
        }
        TypeExpr::Constrained(..) => false,
    }
}

fn is_lazy_generic(ty: &crate::parser::TypeExpr) -> bool {
    matches!(ty, crate::parser::TypeExpr::Generic(name, _)
        if matches!(name.trim_start_matches('*'), "Listing" | "Mapping"))
}

pub(super) fn is_empty_collection(value: &Value) -> bool {
    match value {
        Value::List(items) => items.is_empty(),
        Value::Object(map, _) => map.is_empty(),
        _ => false,
    }
}

/// Whether `value` is plainly a collection of the class `name`.
fn plainly_of_kind(name: &str, value: &Value) -> bool {
    match (name, value) {
        ("Listing", Value::List(items)) => items.kind() == ListKind::Listing,
        ("List", Value::List(items)) => {
            items.kind() == ListKind::List && !items.is_int_seq() && !items.is_bytes()
        }
        ("Mapping", Value::Object(_, Some(source))) => source.kind == ObjectKind::Mapping,
        _ => false,
    }
}

/// Whether `value` is plainly of the primitive type `ty`, without a check.
fn plainly_of_type(value: &Value, ty: &crate::parser::TypeExpr) -> bool {
    let crate::parser::TypeExpr::Named(name) = ty else {
        return false;
    };
    matches!(
        (name.as_str(), value),
        ("String", Value::String(_))
            | ("Int", Value::Int(_))
            | ("Boolean", Value::Bool(_))
            | ("Float", Value::Float(_))
    )
}

/// Whether two values are one collection, sharing their storage.
pub(super) fn same_storage(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::List(a), Value::List(b)) => a.same_items(b),
        (Value::Object(a, _), Value::Object(b, _)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

/// The first failed member anywhere inside `value`. A collection met again
/// through another path is not walked twice.
fn deep_failed<'v>(
    value: &'v Value,
    depth: usize,
    seen: &mut HashSet<usize>,
) -> Option<&'v Arc<str>> {
    if depth > 64 {
        return None;
    }
    match value {
        Value::Failed(message) => Some(message),
        Value::List(items) => {
            if !seen.insert(items.storage_addr()) {
                return None;
            }
            items
                .iter()
                .find_map(|item| deep_failed(item, depth + 1, seen))
        }
        Value::Object(map, source) => {
            if !seen.insert(Arc::as_ptr(map) as *const () as usize) {
                return None;
            }
            map.values()
                .chain(source.iter().flat_map(|source| source.elements.iter()))
                .find_map(|item| deep_failed(item, depth + 1, seen))
        }
        _ => None,
    }
}

impl Evaluator {
    /// The error of the member `obj[key]` names, when it failed.
    pub(super) fn raise_failed_at(&self, obj: &Value, key: &Value) -> Result<()> {
        let member = match (obj, key) {
            (Value::List(items), Value::Int(index)) => usize::try_from(*index)
                .ok()
                .and_then(|index| items.get(index)),
            (Value::Object(map, _), key) => crate::value::mapping_storage_key(key)
                .and_then(|stored| map.get(&*stored))
                .or_else(|| match key {
                    Value::String(name) => map.get(name.as_ref()),
                    _ => None,
                }),
            _ => None,
        };
        match member {
            Some(Value::Failed(message)) => Err(Error::Eval(message.to_string())),
            _ => Ok(()),
        }
    }

    /// Whether a member of `value` failed.
    pub(super) fn holds_failed(&self, value: &Value) -> bool {
        self.failed_seen && deep_failed(value, 0, &mut HashSet::default()).is_some()
    }

    /// The error of a failed member inside `value`, for an operation that
    /// reads every member, such as hashing it into a `Set` or a `Map` key.
    pub(super) fn raise_deep_failed(&self, value: &Value) -> Result<()> {
        if !self.failed_seen {
            return Ok(());
        }
        match deep_failed(value, 0, &mut HashSet::default()) {
            Some(message) => Err(Error::Eval(message.to_string())),
            None => Ok(()),
        }
    }

    /// Check `val` against `ty` the way pkl does: the members of a `Listing`
    /// or `Mapping` are checked when read, everything else when the value is
    /// checked. A member that fails the check is replaced with its error, so
    /// the error is raised only when the member is read.
    pub(super) fn coerce_lazy(
        &mut self,
        val: Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        use crate::parser::TypeExpr;
        if !type_holds_lazy(ty) {
            self.check_declared_value(&val, ty, scope, depth)?;
            return Ok(val);
        }
        match ty {
            TypeExpr::Nullable(inner) => {
                if matches!(val, Value::Null) {
                    return Ok(val);
                }
                self.coerce_lazy(val, inner, scope, depth)
            }
            TypeExpr::Union(_) => {
                let mut alternatives = Vec::new();
                union_alternatives(ty, &mut alternatives);
                let lazy: Vec<_> = alternatives
                    .iter()
                    .copied()
                    .filter(|alt| is_lazy_generic(alt))
                    .collect();
                let holds_lazy_value = match &val {
                    Value::List(items) => items.kind() == ListKind::Listing,
                    Value::Object(_, source) => source
                        .as_ref()
                        .is_some_and(|s| s.kind == ObjectKind::Mapping),
                    _ => false,
                };
                if holds_lazy_value
                    && let [only] = lazy.as_slice()
                    && let TypeExpr::Generic(name, _) = only
                    && (matches!(&val, Value::List(_))
                        == (name.trim_start_matches('*') == "Listing"))
                {
                    return self.coerce_lazy(val, only, scope, depth);
                }
                self.check_declared_value(&val, ty, scope, depth)?;
                Ok(val)
            }
            TypeExpr::Generic(name, args) => {
                let name = name.trim_start_matches('*');
                // An empty collection of the right kind has no member to check.
                if is_empty_collection(&val) && plainly_of_kind(name, &val) {
                    return Ok(val);
                }
                self.coerce_generic(val, ty, name, args, scope, depth)
            }
            _ => {
                self.check_declared_value(&val, ty, scope, depth)?;
                Ok(val)
            }
        }
    }

    fn coerce_generic(
        &mut self,
        val: Value,
        ty: &crate::parser::TypeExpr,
        name: &str,
        args: &[crate::parser::TypeExpr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        use crate::parser::TypeExpr;
        let usable = |ty: Option<&TypeExpr>, scope: &Scope| {
            ty.filter(|ty| !type_is_any(ty) && type_is_runtime_checkable(ty, scope))
                .cloned()
        };
        // The collection itself must be of the right kind before its members
        // are looked at.
        if !plainly_of_kind(name, &val) {
            self.check_declared_value(&val, &TypeExpr::Named(name.to_string()), scope, depth)?;
        }
        match (name, val) {
            ("List" | "Listing" | "Set" | "Collection", Value::List(items)) => {
                let Some(element_ty) = usable(args.first(), scope) else {
                    return Ok(Value::List(items));
                };
                let kind = items.kind();
                // A `Set` hashes its members, so they are checked at once.
                if kind == ListKind::Set {
                    for item in items.iter() {
                        self.raise_deep_failed(item)?;
                        self.check_declared_value(item, &element_ty, scope, depth)?;
                    }
                    return Ok(Value::List(items));
                }
                let lazy_members = name == "Listing";
                // A flat element type leaves a member as it is or fails it,
                // so the list is rebuilt only for a member that fails.
                if !type_holds_lazy(&element_ty) {
                    let mut coerced: Option<Vec<Value>> = None;
                    for (index, item) in items.iter().enumerate() {
                        if matches!(item, Value::Failed(_)) && lazy_members {
                            continue;
                        }
                        if plainly_of_type(item, &element_ty) {
                            continue;
                        }
                        let result = self.check_declared_value(item, &element_ty, scope, depth);
                        let failure = match result {
                            Ok(()) => continue,
                            Err(error) if lazy_members => {
                                match self.defer_member_error(Err(error))? {
                                    failed @ Value::Failed(_) => failed,
                                    _ => continue,
                                }
                            }
                            Err(error) => return Err(error),
                        };
                        coerced.get_or_insert_with(|| items.iter().cloned().collect())[index] =
                            failure;
                    }
                    return Ok(match coerced {
                        None => Value::List(items),
                        Some(coerced) => Value::List(
                            ListValue::new(kind, coerced)
                                .with_elem_class(items.elem_class().cloned())
                                .with_layers(items.layers().cloned()),
                        ),
                    });
                }
                let mut coerced = Vec::with_capacity(items.len());
                for item in items.iter() {
                    if matches!(item, Value::Failed(_)) && lazy_members {
                        coerced.push(item.clone());
                        continue;
                    }
                    let result = self.coerce_lazy(item.clone(), &element_ty, scope, depth);
                    coerced.push(if lazy_members {
                        self.defer_member_error(result)?
                    } else {
                        result?
                    });
                }
                Ok(Value::List(
                    ListValue::new(kind, coerced)
                        .with_elem_class(items.elem_class().cloned())
                        .with_layers(items.layers().cloned()),
                ))
            }
            ("Pair", Value::Object(members, Some(source)))
                if source.type_identity.as_deref() == Some("pkl:base#Pair") =>
            {
                let mut members = Arc::unwrap_or_clone(members);
                for (member, ty) in [("first", args.first()), ("second", args.get(1))] {
                    if let Some(ty) = usable(ty, scope)
                        && let Some(slot) = members.get_mut(member)
                    {
                        *slot = self.coerce_lazy(slot.clone(), &ty, scope, depth)?;
                    }
                }
                Ok(Value::Object(Arc::new(members), Some(source)))
            }
            ("Map" | "Mapping", Value::Object(entries, source))
                if source
                    .as_ref()
                    .is_none_or(|source| source.type_name.is_none()) =>
            {
                let key_ty = usable(args.first(), scope);
                let value_ty = usable(args.get(1), scope);
                if key_ty.is_none() && value_ty.is_none() {
                    return Ok(Value::Object(entries, source));
                }
                let lazy_members = name == "Mapping";
                let mut failures: Vec<(Arc<str>, Value)> = Vec::new();
                let mut rebuilt: Option<ObjectMap> = None;
                for (stored_key, slot) in entries.iter() {
                    if let Some(key_ty) = &key_ty {
                        let key = self.entry_key_value(stored_key);
                        self.check_declared_value(&key, key_ty, scope, depth)?;
                        if name == "Map" {
                            self.raise_deep_failed(&key)?;
                        }
                    }
                    let Some(value_ty) = &value_ty else {
                        continue;
                    };
                    if matches!(slot, Value::Failed(_)) && lazy_members {
                        continue;
                    }
                    if !type_holds_lazy(value_ty) {
                        if plainly_of_type(slot, value_ty) {
                            continue;
                        }
                        match self.check_declared_value(slot, value_ty, scope, depth) {
                            Ok(()) => {}
                            Err(error) if lazy_members => {
                                if let failed @ Value::Failed(_) =
                                    self.defer_member_error(Err(error))?
                                {
                                    failures.push((stored_key.clone(), failed));
                                }
                            }
                            Err(error) => return Err(error),
                        }
                        continue;
                    }
                    let result = self.coerce_lazy(slot.clone(), value_ty, scope, depth);
                    let value = if lazy_members {
                        self.defer_member_error(result)?
                    } else {
                        result?
                    };
                    rebuilt
                        .get_or_insert_with(|| (*entries).clone())
                        .insert(stored_key.clone(), value);
                }
                if rebuilt.is_none() && failures.is_empty() {
                    return Ok(Value::Object(entries, source));
                }
                let mut rebuilt = rebuilt.unwrap_or_else(|| (*entries).clone());
                for (key, failed) in failures {
                    rebuilt.insert(key, failed);
                }
                Ok(Value::Object(Arc::new(rebuilt), source))
            }
            (_, val) => {
                self.check_declared_value(&val, ty, scope, depth)?;
                Ok(val)
            }
        }
    }
}

/// `value` as an error message shows it once every member has been read: a
/// collection lists its members, where `display_value` shows `?` for those
/// that were not evaluated.
pub(super) fn display_forced(value: &Value) -> String {
    match value {
        Value::List(items) => {
            let shown: Vec<String> = items.iter().map(display_forced).collect();
            match items.kind() {
                ListKind::List => format!("List({})", shown.join(", ")),
                ListKind::Set => format!("Set({})", shown.join(", ")),
                ListKind::Listing | ListKind::Dynamic if shown.is_empty() => {
                    if items.kind() == ListKind::Listing {
                        "new Listing {}".into()
                    } else {
                        "new Dynamic {}".into()
                    }
                }
                ListKind::Listing => format!("new Listing {{ {} }}", shown.join("; ")),
                ListKind::Dynamic => format!("new Dynamic {{ {} }}", shown.join("; ")),
            }
        }
        Value::Object(map, Some(source))
            if source.kind == ObjectKind::Mapping && !map.is_empty() =>
        {
            let shown: Vec<String> = map
                .iter()
                .map(|(key, member)| {
                    format!(
                        "[{}] = {}",
                        display_forced(&crate::value::mapping_storage_value(key)),
                        display_forced(member)
                    )
                })
                .collect();
            format!("new Mapping {{ {} }}", shown.join("; "))
        }
        other => crate::eval::render::display_value(other),
    }
}
