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
                        *value = Some(self.eval_expr(
                            &with_listing_locals(expr, &locals),
                            &scope,
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
                Entry::Elem(expr) => items.push(self.eval_expr(expr, &listing_scope, depth + 1)?),
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
                        self.eval_listing_entries(&generator.body, &iter_scope, depth + 1, items)?;
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
                        self.eval_listing_entries(selected, &listing_scope, depth + 1, items)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
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
