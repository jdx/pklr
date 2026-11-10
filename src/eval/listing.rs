//! Evaluating the members and entries of a `Listing` body.

use super::*;

impl Evaluator {
    /// Read one member of the amended receiver without forcing unrelated
    /// elements (which may themselves reference super.first or super.last).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn eval_listing_member(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        target: usize,
        position: &mut usize,
        value: &mut Option<Value>,
        inherited_locals: &[(String, Expr)],
        elem_class: Option<&Arc<Value>>,
    ) -> Result<()> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        let scope = scope.child();
        let mut locals = inherited_locals.to_vec();
        for entry in entries {
            match entry {
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {
                    if let Some(expr) = &prop.value {
                        locals.push((prop.name.to_string(), expr.clone()));
                    }
                }
                Entry::Elem(expr) => {
                    if *position == target {
                        let (expr, elem_scope) = typed_listing_elem(expr, &scope, elem_class);
                        *value = Some(self.eval_expr(
                            &with_listing_locals(&expr, &locals),
                            &elem_scope,
                            depth + 1,
                        )?);
                    }
                    *position += 1;
                }
                Entry::DynProperty(index, expr) => {
                    let index = self.eval_expr(index, &scope, depth + 1)?;
                    let Value::Int(index) = index else {
                        return Err(Error::Eval(
                            "listing index amendment requires an Int index".into(),
                        ));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| Error::Eval("listing index cannot be negative".into()))?;
                    if index == target {
                        *value = Some(
                            if let (Some(base), Expr::ObjectBody(body)) = (value.as_ref(), expr) {
                                let mut amendment_scope = scope.child();
                                let base_name = "\0listing_endpoint_base".to_string();
                                amendment_scope.set(&base_name, base.clone());
                                let amendment = Expr::Binop(
                                    BinOp::Add,
                                    Box::new(Expr::Ident(base_name)),
                                    Box::new(Expr::ObjectBody(body.clone())),
                                );
                                self.eval_expr(
                                    &with_listing_locals(&amendment, &locals),
                                    &amendment_scope,
                                    depth + 1,
                                )?
                            } else {
                                self.eval_expr(
                                    &with_listing_locals(expr, &locals),
                                    &scope,
                                    depth + 1,
                                )?
                            },
                        );
                    }
                    *position = (*position).max(index + 1);
                }
                Entry::Spread(expr) => {
                    let values = listing_spread_values(self.eval_expr(expr, &scope, depth + 1)?)?;
                    if target >= *position && target - *position < values.len() {
                        *value = Some(values[target - *position].clone());
                    }
                    *position += values.len();
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, item) in collection_to_items(collection)? {
                        let mut iter_scope = scope.child();
                        let mut iter_locals = locals.clone();
                        // Bind generator variables after enclosing locals so
                        // shadowing does not change those locals' definitions.
                        let value_binding = format!("\0listing_generator_value:{depth}");
                        iter_scope.set(&value_binding, item.clone());
                        iter_scope.set(&generator.val_var, item);
                        iter_locals.push((generator.val_var.clone(), Expr::Ident(value_binding)));
                        if let Some(key_var) = &generator.key_var {
                            let key_binding = format!("\0listing_generator_key:{depth}");
                            iter_scope.set(&key_binding, key.clone());
                            iter_scope.set(key_var, key);
                            iter_locals.push((key_var.clone(), Expr::Ident(key_binding)));
                        }
                        self.eval_listing_member(
                            &generator.body,
                            &iter_scope,
                            depth + 1,
                            target,
                            position,
                            value,
                            &iter_locals,
                            elem_class,
                        )?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition = self.eval_expr(&generator.condition, &scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(selected) = selected {
                        self.eval_listing_member(
                            selected,
                            &scope,
                            depth + 1,
                            target,
                            position,
                            value,
                            &locals,
                            elem_class,
                        )?;
                    }
                }
                Entry::Predicate(..) => return Err(member_predicates_unsupported()),
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn eval_listing_entries(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        items: &mut Vec<Value>,
        elem_class: Option<&Arc<Value>>,
    ) -> Result<()> {
        let mut listing_scope = scope.child();
        // A body's locals and properties (`default`) are visible to all of
        // its entries, including ones written before them, and to each other
        // regardless of order. Bind each once the members of this body it
        // reads are bound, so a function captures them too.
        let mut pending: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some((prop, prop.value.as_ref()?)),
                _ => None,
            })
            .collect();
        let mut unbound: HashSet<Name> =
            pending.iter().map(|(prop, _)| prop.name.clone()).collect();
        while !pending.is_empty() {
            let ready = pending
                .iter()
                .position(|(prop, expr)| {
                    let mut refs = HashSet::default();
                    collect_expr_refs(expr, &mut refs, &HashSet::default());
                    !refs
                        .iter()
                        .any(|name| *name != prop.name && unbound.contains(name))
                })
                // A cycle: bind in declaration order.
                .unwrap_or(0);
            let (prop, expr) = pending.remove(ready);
            let value = self.eval_expr(expr, &listing_scope, depth + 1)?;
            bind_listing_member(&mut listing_scope, prop, value);
            unbound.remove(&prop.name);
        }
        for entry in entries {
            match entry {
                Entry::Predicate(..) => return Err(member_predicates_unsupported()),
                Entry::Property(_) => {}
                Entry::Elem(expr) => {
                    let (expr, elem_scope) = typed_listing_elem(expr, &listing_scope, elem_class);
                    items.push(self.eval_expr(&expr, &elem_scope, depth + 1)?)
                }
                Entry::DynProperty(index, value) => {
                    let index = self.eval_expr(index, &listing_scope, depth + 1)?;
                    let Value::Int(index) = index else {
                        return Err(Error::Eval(format!(
                            "Expected key of type `Int`, but got type `{}`.",
                            value_type_name(&index)
                        )));
                    };
                    // Only the parent's elements can be amended by index, not
                    // ones this body adds.
                    let parent_len = scope
                        .receiver_list_base
                        .map_or(items.len(), |len| len.min(items.len()));
                    let index = usize::try_from(index)
                        .ok()
                        .filter(|index| *index < parent_len)
                        .ok_or_else(|| {
                            Error::Eval(format!(
                                "Element index `{index}` is out of range `0`..`{}`.",
                                parent_len as i64 - 1
                            ))
                        })?;
                    items[index] = if let Expr::ObjectBody(entries) = value {
                        self.eval_value_amendment(
                            items[index].clone(),
                            entries,
                            &listing_scope,
                            depth + 1,
                        )?
                    } else {
                        self.eval_expr(value, &listing_scope, depth + 1)?
                    };
                }
                Entry::Spread(expr) => {
                    let value = self.eval_expr(expr, &listing_scope, depth + 1)?;
                    items.extend(listing_spread_values(value)?.iter().cloned());
                }
                Entry::ForGenerator(generator) => {
                    let collection =
                        self.eval_expr(&generator.collection, &listing_scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection)? {
                        let mut iter_scope = listing_scope.child();
                        iter_scope.set(&generator.val_var, value);
                        if let Some(key_var) = &generator.key_var {
                            iter_scope.set(key_var, key);
                        }
                        self.eval_listing_entries(
                            &generator.body,
                            &iter_scope,
                            depth + 1,
                            items,
                            elem_class,
                        )?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition =
                        self.eval_expr(&generator.condition, &listing_scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(selected) = selected {
                        self.eval_listing_entries(
                            selected,
                            &listing_scope,
                            depth + 1,
                            items,
                            elem_class,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The scope name an untyped `new { ... }` element of a `Listing<T>` is
/// constructed through.
pub(super) const LISTING_ELEM_CLASS: &str = "\0listing_elem_class";

/// An element expression, and the scope to evaluate it in. An untyped
/// `new { ... }` of a `Listing<T>` becomes a `T`; any other element is kept.
/// The class is bound by value: `T` may be visible only where the listing's
/// type was declared, not in this scope.
fn typed_listing_elem(
    expr: &Expr,
    scope: &Scope,
    elem_class: Option<&Arc<Value>>,
) -> (Expr, Scope) {
    let Some(class) = elem_class else {
        return (expr.clone(), scope.clone());
    };
    if !parser::has_untyped_result_new(expr) {
        return (expr.clone(), scope.clone());
    }
    let mut elem_scope = scope.child();
    elem_scope.set(LISTING_ELEM_CLASS, (**class).clone());
    let listings = matches!(**class, Value::List(_));
    if !listings {
        // A class bound under another name is a value, not a type.
        elem_scope.set(type_alias_marker(LISTING_ELEM_CLASS), Value::Null);
    }
    // Reach `new { ... }` through `if`, `let` and `trace` result positions.
    let mut typed = expr.clone();
    parser::rewrite_untyped_result_new(&mut typed, &mut |body| {
        if listings {
            // The elements are listings: `new { new { ... } }` of a
            // `Listing<Listing<T>>`.
            Expr::New(
                Some("Listing".into()),
                body,
                vec![LISTING_ELEM_CLASS.to_string()],
            )
        } else {
            Expr::New(Some(LISTING_ELEM_CLASS.into()), body, Vec::new())
        }
    });
    (typed, elem_scope)
}

/// Whether `ty` is a `Listing<...>`, through nullability and type aliases.
pub(super) fn resolves_to_listing(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    // A chain of aliases is walked in a loop, so a long one cannot overflow
    // the stack and a cyclic one ends at its first repeat.
    let mut seen = HashSet::default();
    resolve_alias_chain(ty, scope, &mut seen).is_some_and(|ty| ty.is_listing_type())
}

/// Follow `ty` through nullability and type aliases to the first type that is
/// neither. `seen` holds the aliases already followed; reaching one again is a
/// cyclic alias (a `local` one escapes the parser's cycle check), which yields
/// `None` rather than looping.
fn resolve_alias_chain<'a>(
    mut ty: &'a crate::parser::TypeExpr,
    scope: &'a Scope,
    seen: &mut HashSet<String>,
) -> Option<&'a crate::parser::TypeExpr> {
    use crate::parser::TypeExpr;
    loop {
        match ty {
            TypeExpr::Nullable(inner) => ty = inner,
            TypeExpr::Named(name) => {
                let name = name.trim_start_matches('*').trim_end_matches('?');
                let Some(alias) = scope.get_type_alias(name) else {
                    return Some(ty);
                };
                if !seen.insert(name.to_string()) {
                    return None;
                }
                ty = alias;
            }
            other => return Some(other),
        }
    }
}

/// The class an untyped `new { ... }` element of a `Listing<T>` constructs:
/// `T` when it is a single class (an alias or nullable `T?` included). A
/// union, a built-in or an unresolved name leaves such an element untyped.
pub(super) fn listing_elem_class(type_args: &[String], scope: &Scope) -> Option<Arc<Value>> {
    // `Listing<Listing<T>>`, or `Listing<L>` with `L` an alias of a listing
    // type: the argument is itself a listing type, read structurally.
    if let [arg] = type_args
        && let Ok(ty) = crate::parser::parse_type_name(arg.trim_start_matches('*'))
        && resolves_to_listing(&ty, scope)
    {
        return type_elem_class(&ty, scope);
    }
    let names = expand_type_alias_names(type_args, scope);
    let [name] = names.as_slice() else {
        return None;
    };
    let name = mapping_value_type_base(name).trim_end_matches('?');
    match resolve_dotted(scope, name) {
        Some(class @ Value::Object(_, Some(_))) if matches!(&class, Value::Object(_, Some(source)) if source.type_name.is_some()) => {
            Some(Arc::new(class))
        }
        // An element class bound by `typed_listing_elem` for an inner
        // listing: its own elements are of the class the listing holds.
        Some(Value::List(inner)) if name == LISTING_ELEM_CLASS => inner.elem_class().cloned(),
        _ => None,
    }
}

/// What an untyped `new { ... }` element of a listing of `ty` constructs: a
/// class, or for a `Listing<..>` element type a listing prototype holding
/// the class of its own elements.
fn type_elem_class(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<Arc<Value>> {
    type_elem_class_in(ty, scope, &mut HashSet::default(), 0)
}

/// `Listing<Listing<...>>` nests this deep at most; a cyclic alias or an
/// absurd nesting leaves the element untyped instead of overflowing the stack.
const MAX_LISTING_NESTING: usize = 64;

/// `type_elem_class`, with the aliases `seen` on the way down to `ty`.
fn type_elem_class_in(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    seen: &mut HashSet<String>,
    nesting: usize,
) -> Option<Arc<Value>> {
    use crate::parser::TypeExpr;
    match resolve_alias_chain(ty, scope, seen)? {
        TypeExpr::Generic(name, args) if name == "Listing" && args.len() == 1 => {
            if nesting >= MAX_LISTING_NESTING {
                return None;
            }
            let inner = type_elem_class_in(&args[0], scope, seen, nesting + 1)?;
            Some(Arc::new(Value::List(
                ListValue::new(ListKind::Listing, Vec::new()).with_elem_class(Some(inner)),
            )))
        }
        TypeExpr::Named(name) => listing_elem_class(std::slice::from_ref(name), scope),
        // `T(x > 0)`: the element is a `T`, whatever the predicate says.
        TypeExpr::Constrained(base, _) => {
            if nesting >= MAX_LISTING_NESTING {
                return None;
            }
            let base = crate::parser::parse_type_name(base.trim_start_matches('*')).ok()?;
            type_elem_class_in(&base, scope, seen, nesting + 1)
        }
        other => listing_elem_class(&[display_type_expr(other)], scope),
    }
}

/// The element class of a property declared `ty`, when that is a `Listing<T>`.
pub(super) fn declared_listing_elem_class(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
) -> Option<Arc<Value>> {
    use crate::parser::TypeExpr;
    let mut seen = HashSet::default();
    match resolve_alias_chain(ty, scope, &mut seen)? {
        TypeExpr::Generic(name, args) if name == "Listing" && args.len() == 1 => {
            type_elem_class_in(&args[0], scope, &mut seen, 1)
        }
        TypeExpr::Named(_) => None,
        _ => {
            let (name, args) = inferred_new_type(ty, scope, 0).ok()?;
            (name == "Listing")
                .then(|| listing_elem_class(&args, scope))
                .flatten()
        }
    }
}

impl Evaluator {
    /// The element class of a property declared `ty`, resolved where the
    /// property's class is declared. A class cannot see itself, or classes
    /// declared after it, in the scope it was captured with, so fall back to
    /// the final scope of its module.
    pub(super) fn declared_elem_class(
        &self,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
    ) -> Option<Arc<Value>> {
        if is_never_listing_type(ty, &|name| scope.get(name).is_some_and(is_class_value)) {
            return None;
        }
        declared_listing_elem_class(ty, scope).or_else(|| {
            let namespace = scope.type_namespace.clone()?;
            let mut declaring = scope.clone();
            self.layer_evaluated_module_scope(Path::new(&namespace), &mut declaring);
            declared_listing_elem_class(ty, &declaring)
        })
    }

    /// The element class of an inherited property of `base_source` whose
    /// annotation is `ty`, resolved in the scope that declared `ty`, never in
    /// the scope that assigned a value to the property. `entry_scope` is the
    /// scope recorded for the inherited entry, or `None` for the object's own
    /// definition scope.
    pub(super) fn inherited_elem_class(
        &self,
        base_source: &ObjectSource,
        ty: &crate::parser::TypeExpr,
        entry_scope: Option<&Arc<CapturedScope>>,
    ) -> Option<(Option<Value>, Scope)> {
        // A class cannot always see itself, or classes declared after it, in
        // the scope it was captured with: the instance's scope has them. A
        // name the declaring scope binds otherwise (an alias, a value) is not
        // the instance's class of that name.
        let is_class = |name: &str| match entry_scope {
            Some(captured) => match captured.values.get(name) {
                Some(value) => is_class_value(value),
                None => {
                    !captured.type_aliases.contains_key(name)
                        && base_source.scope().get(name).is_some_and(is_class_value)
                }
            },
            None => base_source.scope().get(name).is_some_and(is_class_value),
        };
        if is_never_listing_type(ty, &is_class) {
            return None;
        }
        let mut declaring = match entry_scope {
            Some(captured) => restore_scope(captured),
            None => definition_type_scope(base_source),
        };
        let class = self.declared_elem_class(ty, &declaring);
        // The returned scope is the one `ty` is checked in, so it also holds
        // the classes the declaring module defines after this one.
        if let Some(namespace) = declaring.type_namespace.clone() {
            self.layer_evaluated_module_scope(Path::new(&namespace), &mut declaring);
        }
        // A listing without an element class (`Listing<String>(length == 1)`)
        // still has its annotation resolved where it was declared.
        if class.is_none() && !resolves_to_listing(ty, &declaring) {
            return None;
        }
        Some((class.map(|class| (*class).clone()), declaring))
    }
}

/// Bind a local or property of a listing body for the body's entries.
pub(super) fn bind_listing_member(scope: &mut Scope, prop: &Property, value: Value) {
    if has_modifier(&prop.modifiers, Modifier::Local) {
        scope.declare(&prop.name, value);
    } else {
        scope.set(&prop.name, value);
    }
}

/// The elements `...value` adds to a Listing. Only collections and Listings
/// can be spread into one: objects have properties or entries instead.
pub(super) fn listing_spread_values(value: Value) -> Result<Arc<Vec<Value>>> {
    check_iterable(&value)?;
    match value {
        Value::List(values) => Ok(Arc::new(values.to_vec())),
        Value::Object(members, _) if members.is_empty() => Ok(Arc::default()),
        Value::Object(_, source) => {
            let members = if source.is_some_and(|s| s.kind == ObjectKind::Mapping) {
                "entries"
            } else {
                "properties"
            };
            Err(Error::Eval(format!(
                "Cannot spread object containing {members} into object of type `Listing`."
            )))
        }
        _ => Ok(Arc::default()),
    }
}

/// Reject elements in a body that amends or instantiates a class: only
/// Listings and Dynamic objects can have elements.
pub(super) fn check_no_elements(source: &ObjectSource, entries: &[Entry]) -> Result<()> {
    match &source.type_name {
        Some(type_name) => check_member_kinds(
            &qualified_class_name(source.type_identity.as_deref(), type_name),
            entries,
            true,
        ),
        None if source.kind == ObjectKind::Mapping => check_member_kinds("Mapping", entries, false),
        None => Ok(()),
    }
}

/// Whether `ty` cannot declare a listing, judged without resolving aliases or
/// module scopes: a non-`Listing` generic, a primitive, or a name that
/// `is_class` says is a class. Lets hot paths skip building a type scope.
pub(super) fn is_never_listing_type(
    ty: &crate::parser::TypeExpr,
    is_class: &impl Fn(&str) -> bool,
) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Nullable(inner) => is_never_listing_type(inner, is_class),
        TypeExpr::Generic(name, _) => name != "Listing",
        TypeExpr::Named(name) => {
            let name = name.trim_start_matches('*');
            matches!(
                name,
                "String"
                    | "Int"
                    | "Float"
                    | "Boolean"
                    | "Number"
                    | "Any"
                    | "Dynamic"
                    | "Duration"
                    | "DataSize"
            ) || is_class(name)
        }
        _ => false,
    }
}
