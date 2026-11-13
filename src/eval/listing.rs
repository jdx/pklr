//! Evaluating the members and entries of a `Listing` body.

use super::*;
use crate::value::{ListLayer, ListLayers};

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
        if depth > self.max_depth || crate::stack::exhausted() {
            crate::stack::limit_hit();
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
                                let (expr, elem_scope) =
                                    typed_listing_elem(expr, &scope, elem_class);
                                self.eval_expr(
                                    &with_listing_locals(&expr, &locals),
                                    &elem_scope,
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
                    let selected = if expect_bool(&condition)? {
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
        // `outer` is the object the listing body sits in, bound once from the
        // scope outside the body: the generators and locals within it do not
        // shadow the enclosing members it reads.
        if super::entries_mention(entries, "outer") {
            let mut with_outer = scope.child();
            super::entries::bind_outer(&mut with_outer, scope, entries, &[]);
            return self.eval_listing_inner(entries, &with_outer, depth, items, elem_class);
        }
        self.eval_listing_inner(entries, scope, depth, items, elem_class)
    }

    fn eval_listing_inner(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        items: &mut Vec<Value>,
        elem_class: Option<&Arc<Value>>,
    ) -> Result<()> {
        let mut listing_scope = scope.child();
        // The listing's `default`, which an untyped `new { ... }` element
        // amends and the body may read.
        let listing_default = match scope.get(LISTING_DEFAULT) {
            Some(default @ Value::Lambda(..)) => Some(default.clone()),
            _ => None,
        };
        if let Some(default) = &listing_default {
            listing_scope.set("default", default.clone());
        }
        // A replay keeps the final `default` its caller bound.
        let keep_default = scope.get(LISTING_REPLAY).is_some();
        self.bind_listing_members(entries, &mut listing_scope, keep_default, depth)?;
        // `this` in an element reads the elements before it.
        let final_this = scope.get(LISTING_FINAL_THIS).cloned();
        let reads_this = final_this.is_none()
            && entries.iter().any(|entry| {
                matches!(entry, Entry::Elem(expr) | Entry::DynProperty(_, expr)
                    if mapping_entry_mentions_this(expr))
            });
        if let Some(this) = final_this {
            listing_scope.set("this", this);
        }
        // `this` has the kind of the body being built (a Dynamic's `super`
        // is a Dynamic list).
        let this_kind = match scope.get("super") {
            Some(Value::List(parent)) => parent.kind(),
            _ => ListKind::Listing,
        };
        let total_len = items.len()
            + entries
                .iter()
                .filter(|entry| matches!(entry, Entry::Elem(_)))
                .count();
        for entry in entries {
            if reads_this && matches!(entry, Entry::Elem(_) | Entry::DynProperty(..)) {
                let mut snapshot = items.clone();
                // Elements still to come, so `this` has about its final length.
                snapshot.resize(total_len.max(items.len()), Value::Null);
                listing_scope.set("this", Value::List(ListValue::new(this_kind, snapshot)));
            }
            match entry {
                Entry::Predicate(..) => return Err(member_predicates_unsupported()),
                Entry::Property(_) => {}
                Entry::Elem(expr)
                    if listing_default.is_some() && parser::has_untyped_result_new(expr) =>
                {
                    // `new { ... }` amends what `default` makes for this
                    // index, also as the result of an `if`, `let` or `trace`.
                    let index = items.len() as i64;
                    let mut element = expr.clone();
                    parser::rewrite_untyped_result_new(&mut element, &mut |body| {
                        let made = Expr::Call(
                            Box::new(Expr::Field(
                                Box::new(Expr::Ident(LISTING_DEFAULT.into())),
                                "apply".into(),
                            )),
                            vec![Expr::Int(index)],
                        );
                        Expr::Binop(BinOp::Add, Box::new(made), Box::new(Expr::ObjectBody(body)))
                    });
                    let element = self.eval_expr(&element, &listing_scope, depth + 1);
                    items.push(self.defer_member_error(element)?)
                }
                Entry::Elem(expr) => {
                    let (expr, elem_scope) = typed_listing_elem(expr, &listing_scope, elem_class);
                    let element = self.eval_expr(&expr, &elem_scope, depth + 1);
                    items.push(self.defer_member_error(element)?)
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
                                "Element index `{}` is out of range `0`..`{}`.",
                                group_digits(index),
                                parent_len as i64 - 1
                            ))
                        })?;
                    let element = if let Expr::ObjectBody(entries) = value {
                        self.eval_value_amendment(
                            items[index].clone(),
                            entries,
                            &listing_scope,
                            depth + 1,
                        )
                    } else {
                        // A replacement `new { ... }` is an element of the
                        // declared class too.
                        let (value, elem_scope) =
                            typed_listing_elem(value, &listing_scope, elem_class);
                        self.eval_expr(&value, &elem_scope, depth + 1)
                    };
                    items[index] = self.defer_member_error(element)?;
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
                        reject_generated_property(&listing_scope, &generator.body)?;
                        self.eval_listing_inner(
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
                    let selected = if expect_bool(&condition)? {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(selected) = selected {
                        reject_generated_property(&listing_scope, selected)?;
                        self.eval_listing_inner(
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

/// The scope binding of a listing's `default`: a `Lambda`, or `Null`.
pub(super) const LISTING_DEFAULT: &str = "\0listing_default";

/// The scope binding of the final elements of a listing being rebuilt, which
/// `this` reads in a replayed body.
const LISTING_FINAL_THIS: &str = "\0listing_final_this";

/// Set while a listing is replayed: the `default` in scope is the final one,
/// which a body's own `default = ...` must not replace.
const LISTING_REPLAY: &str = "\0listing_replay";

/// The parameter of a listing's `default` function.
const DEFAULT_INDEX: &str = "\0listing_default_index";

/// The `default` of a listing that declares none: it makes an instance of the
/// element class, or a `Dynamic`.
pub(super) fn implicit_listing_default(elem_class: Option<&Arc<Value>>) -> Value {
    let (body, scope) = typed_listing_elem(
        &Expr::New(None, Arc::default(), Vec::new()),
        &Scope::default(),
        elem_class,
    );
    Value::Lambda(
        Arc::from(vec![DEFAULT_INDEX.to_string()]),
        Arc::new(body),
        Arc::new(scope.flatten()),
    )
}

/// What a listing body makes late bound.
struct ListingBodyFlags {
    /// An element reads `this`.
    reads_this: bool,
    /// An untyped `new { ... }` element, which amends the `default`.
    default_elements: bool,
    defines_default: bool,
}

fn default_property(entries: &[Entry]) -> Option<&Arc<Property>> {
    entries.iter().find_map(|entry| match entry {
        Entry::Property(prop)
            if &*prop.name == "default" && !has_modifier(&prop.modifiers, Modifier::Local) =>
        {
            Some(prop)
        }
        _ => None,
    })
}

fn listing_body_flags(entries: &[Entry], flags: &mut ListingBodyFlags) {
    for entry in entries {
        match entry {
            Entry::Property(prop)
                if &*prop.name == "default" && !has_modifier(&prop.modifiers, Modifier::Local) =>
            {
                flags.defines_default = true;
            }
            Entry::Elem(expr) | Entry::DynProperty(_, expr) => {
                flags.reads_this |= mapping_entry_mentions_this(expr);
                flags.default_elements |= parser::has_untyped_result_new(expr);
            }
            Entry::ForGenerator(generator) => listing_body_flags(&generator.body, flags),
            Entry::WhenGenerator(generator) => {
                listing_body_flags(&generator.body, flags);
                if let Some(body) = &generator.else_body {
                    listing_body_flags(body, flags);
                }
            }
            _ => {}
        }
    }
}

impl Evaluator {
    /// The `default` function of the listing a body builds: the body's own, or
    /// `None` when it has none. A `default { ... }` body amends what
    /// `super.default` makes.
    fn listing_default_of(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        let Some(prop) = default_property(entries) else {
            return Ok(None);
        };
        let function = match (&prop.value, &prop.body) {
            (Some(expr), _) => expr.clone(),
            (None, Some(body)) => {
                let parent = Expr::Call(
                    Box::new(Expr::Field(
                        Box::new(Expr::Field(
                            Box::new(Expr::Ident("super".into())),
                            "default".into(),
                        )),
                        "apply".into(),
                    )),
                    vec![Expr::Ident(DEFAULT_INDEX.into())],
                );
                Expr::Lambda(
                    Arc::from(vec![DEFAULT_INDEX.to_string()]),
                    Arc::new(Expr::Binop(
                        BinOp::Add,
                        Box::new(parent),
                        Box::new(Expr::ObjectBody(body.clone())),
                    )),
                )
            }
            _ => return Ok(None),
        };
        // The function sees the body's own locals.
        let mut scope = scope.child();
        if let Some(inherited @ Value::Lambda(..)) = scope.get(LISTING_DEFAULT).cloned() {
            scope.set("default", inherited);
        }
        // A local may call a body-form `default`, which does not see the
        // body's locals yet: bind a first version for it to read.
        if prop.value.is_none() {
            let first = self.eval_expr(&function, &scope, depth + 1)?;
            scope.set("default", first);
            // Then one with the locals bound against that, for the locals
            // to read in turn.
            let mut round = scope.child();
            self.bind_listing_members(entries, &mut round, false, depth)?;
            let second = self.eval_expr(&function, &round, depth + 1)?;
            scope.set("default", second);
        }
        self.bind_listing_members(entries, &mut scope, false, depth)?;
        self.eval_expr(&function, &scope, depth + 1).map(Some)
    }

    /// Build the listing `entries` make over `base` (`None` for a new one).
    /// A body with late-bound elements is kept, so an amendment replays it.
    pub(super) fn eval_listing_body(
        &mut self,
        base: Option<ListValue>,
        entries: &Body,
        scope: &Scope,
        depth: usize,
        elem_class: Option<Arc<Value>>,
    ) -> Result<ListValue> {
        let mut flags = ListingBodyFlags {
            reads_this: false,
            default_elements: false,
            defines_default: false,
        };
        listing_body_flags(entries, &mut flags);
        let base_layers = base.as_ref().and_then(|base| base.layers().cloned());
        let base = base.unwrap_or_else(|| ListValue::new(ListKind::Listing, Vec::new()));
        let kind = base.kind();
        let base = base.with_elem_class(elem_class.clone());
        let layer = ListLayer {
            entries: entries.clone(),
            scope: scope.clone(),
            default: None,
        };
        let record = base_layers.is_some()
            || flags.defines_default
            || flags.reads_this
            || flags.default_elements;
        // The default so far, for `super.default` and for the new elements.
        let inherited = base_layers
            .as_ref()
            .and_then(|layers| layers.default.clone());
        let mut body_scope = scope.child();
        let super_list = base.clone().with_layers(base_layers.clone());
        body_scope.set("super", Value::List(super_list));
        body_scope.receiver_entries = Some(entries.clone());
        body_scope.receiver_list_base = Some(base.len());
        body_scope.set(LISTING_DEFAULT, inherited.clone().unwrap_or(Value::Null));
        let own_default = self.listing_default_of(entries, &body_scope, depth)?;
        let default = own_default.or(inherited);
        body_scope.set(LISTING_DEFAULT, default.clone().unwrap_or(Value::Null));
        // An amendment that changes the default, or replaces an element a
        // late-bound one reads, rebuilds what the earlier bodies made.
        if let Some(base_layers) = base_layers
            .as_ref()
            .filter(|layers| flags.defines_default || layers.reads_this)
        {
            let mut layers = (**base_layers).clone();
            layers.layers.push(ListLayer {
                default: default.clone(),
                ..layer
            });
            layers.default = default;
            layers.reads_this |= flags.reads_this;
            let items = self.replay_listing(&layers, kind, elem_class.as_ref(), depth)?;
            return Ok(ListValue::new(kind, items)
                .with_elem_class(elem_class)
                .with_layers(Some(Arc::new(layers))));
        }
        let mut list = base;
        let init = Arc::clone(list.items_arc());
        self.eval_listing_entries(
            entries,
            &body_scope,
            depth + 1,
            list.make_mut(),
            elem_class.as_ref(),
        )?;
        let layers = record.then(|| {
            let mut layers = base_layers.map_or_else(
                || ListLayers {
                    init,
                    ..ListLayers::default()
                },
                |layers| (*layers).clone(),
            );
            layers.layers.push(ListLayer {
                default: default.clone(),
                ..layer
            });
            layers.default = default;
            layers.reads_this |= flags.reads_this;
            Arc::new(layers)
        });
        Ok(list.with_elem_class(elem_class).with_layers(layers))
    }

    /// Evaluate every body of `layers` in order, with the final default and,
    /// where a body reads `this`, the final elements.
    fn replay_listing(
        &mut self,
        layers: &ListLayers,
        kind: ListKind,
        elem_class: Option<&Arc<Value>>,
        depth: usize,
    ) -> Result<Vec<Value>> {
        let mut final_items: Option<Vec<Value>> = None;
        // Elements that read `this` read the previous pass's result, so
        // repeat until they settle (each pass fixes at least one more).
        // A chain of `this[i]` reads is at most as long as the final list, so
        // the pass limit follows the result, not the layers.
        let mut pass = 0usize;
        let mut limit: Option<usize> = None;
        loop {
            pass += 1;
            let mut items = layers.init.to_vec();
            let mut parent_default = None;
            for layer in &layers.layers {
                let mut scope = layer.scope.child();
                let parent_layers = parent_default.take().map(|default| {
                    Arc::new(ListLayers {
                        init: Arc::new(Vec::new()),
                        layers: Vec::new(),
                        default: Some(default),
                        reads_this: false,
                    })
                });
                parent_default = layer.default.clone();
                let super_list = ListValue::new(kind, items.clone())
                    .with_elem_class(elem_class.cloned())
                    .with_layers(parent_layers);
                scope.set("super", Value::List(super_list));
                scope.set(LISTING_REPLAY, Value::Null);
                scope.receiver_entries = Some(layer.entries.clone());
                scope.receiver_list_base = Some(items.len());
                scope.set(
                    LISTING_DEFAULT,
                    layers.default.clone().unwrap_or(Value::Null),
                );
                if let Some(final_items) = &final_items {
                    scope.set(
                        LISTING_FINAL_THIS,
                        Value::List(ListValue::new(kind, final_items.clone())),
                    );
                }
                self.eval_listing_entries(
                    &layer.entries,
                    &scope,
                    depth + 1,
                    &mut items,
                    elem_class,
                )?;
            }
            // A function holds the elements it read, which `same_data` does
            // not follow; a chain of them is at most as long as there are
            // functions, so that many extra passes follow once the data is
            // equal.
            let functions = items
                .iter()
                .filter(|item| matches!(item, Value::Lambda(..)))
                .count();
            let settled = !layers.reads_this
                || (pass > functions
                    && final_items.as_ref().is_some_and(|prev| {
                        prev.len() == items.len()
                            && prev.iter().zip(&items).all(|(a, b)| a.same_data(b))
                    }));
            // Fixed by the first pass, so a list that keeps growing ends.
            let limit = *limit.get_or_insert(items.len() + layers.layers.len() + 4);
            let grew = final_items
                .as_ref()
                .is_some_and(|prev| prev.len() != items.len());
            final_items = Some(items);
            if settled || pass >= limit {
                if grew {
                    return Err(Error::Eval(
                        "Listing elements that read `this` do not settle.".into(),
                    ));
                }
                break;
            }
        }
        Ok(final_items.unwrap_or_default())
    }
}

/// A `for` or `when` body of a Listing that runs may not assign a property,
/// which would otherwise be dropped silently.
fn reject_generated_property(scope: &Scope, body: &[Entry]) -> Result<()> {
    let is_listing = matches!(scope.get("super"),
        Some(Value::List(parent)) if parent.kind() == ListKind::Listing);
    let assigns = body.iter().any(|entry| {
        matches!(entry, Entry::Property(prop)
            if &*prop.name != "default" && !has_modifier(&prop.modifiers, Modifier::Local))
    });
    if is_listing && assigns {
        return Err(Error::Eval(
            "Object of type `Listing` cannot have a property (other than `default`).".into(),
        ));
    }
    Ok(())
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
    resolves_to_listing_in(ty, scope, &mut seen, 0)
}

/// [`resolves_to_listing`], following a constrained type (`CL = L(pred)`) to
/// its base, which names another alias or a listing.
fn resolves_to_listing_in(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    seen: &mut HashSet<String>,
    nesting: usize,
) -> bool {
    use crate::parser::TypeExpr;
    match resolve_alias_chain(ty, scope, seen) {
        Some(TypeExpr::Constrained(base, _, _)) => {
            nesting < MAX_LISTING_NESTING
                && crate::parser::parse_type_name(base.trim_start_matches('*'))
                    .is_ok_and(|base| resolves_to_listing_in(&base, scope, seen, nesting + 1))
        }
        // A bare `Listing` (`Listing(length <= 2)`) has no type arguments.
        Some(TypeExpr::Named(name)) => name == "Listing",
        Some(other) => other.is_listing_type(),
        None => false,
    }
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
            // An element type with no class to build (`Listing<String>`) still
            // makes this element a `Listing`, as pkl does.
            let inner = type_elem_class_in(&args[0], scope, seen, nesting + 1);
            Some(Arc::new(Value::List(
                ListValue::new(ListKind::Listing, Vec::new()).with_elem_class(inner),
            )))
        }
        // A bare `Listing`: its own elements have no class to build.
        TypeExpr::Named(name) if name == "Listing" => Some(Arc::new(Value::List(ListValue::new(
            ListKind::Listing,
            Vec::new(),
        )))),
        TypeExpr::Named(name) => listing_elem_class(std::slice::from_ref(name), scope),
        // `T(x > 0)`: the element is a `T`, whatever the predicate says.
        TypeExpr::Constrained(base, _, _) => {
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
        if is_never_listing_type(
            ty,
            &|name| scope.get(name).is_some_and(is_class_value),
            &|name| scope.get_type_alias(name).is_some(),
        ) {
            return None;
        }
        let class = declared_listing_elem_class(ty, scope);
        // A nested listing (`Listing<Listing<Node>>`) is a listing even when
        // its innermost class is not visible in the saved scope (a class
        // cannot see itself, or classes declared after it), so also look again
        // in the module's final scope when that class is unresolved.
        let unresolved_leaf = class.is_some()
            && listing_leaf_name(ty, scope)
                .is_some_and(|leaf| resolve_dotted(scope, &leaf).is_none())
            && class.as_deref().is_some_and(|class| !has_leaf_class(class));
        if class.is_some() && !unresolved_leaf {
            return class;
        }
        let namespace = scope.type_namespace.clone()?;
        let mut declaring = scope.clone();
        self.layer_evaluated_module_scope(Path::new(&namespace), &mut declaring);
        declared_listing_elem_class(ty, &declaring).or(class)
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
        let is_alias = |name: &str| match entry_scope {
            Some(captured) => captured.type_aliases.contains_key(name),
            None => base_source.scope_type_aliases().contains_key(name),
        };
        if is_never_listing_type(ty, &is_class, &is_alias) {
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
        // A `Map` is no object to spread: it has no source.
        value @ Value::Object(_, None) => Err(Error::Eval(format!(
            "Cannot spread value of type `Map` into object of type `Listing`.\nValue: {}",
            crate::eval::render::display_value(&value)
        ))),
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
        Some(type_name) => {
            let qualified = qualified_class_name(source.type_identity.as_deref(), type_name);
            // A body with generators is checked as it is evaluated, when
            // Pkl names the object by its expression rather than its class.
            let entry_display = if entries
                .iter()
                .any(|e| matches!(e, Entry::ForGenerator(_) | Entry::WhenGenerator(_)))
            {
                format!("new {type_name} {{}}")
            } else {
                qualified.clone()
            };
            check_member_kinds(&qualified, &entry_display, entries, true)
        }
        None if source.kind == ObjectKind::Mapping => {
            check_member_kinds("Mapping", "Mapping", entries, false)
        }
        None => Ok(()),
    }
}

/// Whether `ty` cannot declare a listing, judged without resolving aliases or
/// module scopes: a non-`Listing` generic, a primitive, or a name that
/// `is_class` says is a class. Lets hot paths skip building a type scope.
pub(super) fn is_never_listing_type(
    ty: &crate::parser::TypeExpr,
    is_class: &impl Fn(&str) -> bool,
    is_alias: &impl Fn(&str) -> bool,
) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Nullable(inner) => is_never_listing_type(inner, is_class, is_alias),
        TypeExpr::Generic(name, _) => name != "Listing",
        TypeExpr::Named(name) => {
            let name = name.trim_start_matches('*');
            // An alias of the same name shadows the built-in type.
            (matches!(
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
            ) && !is_alias(name))
                || is_class(name)
        }
        _ => false,
    }
}

/// Whether a listing prototype, however deeply nested, ends in an element
/// class.
pub(super) fn has_leaf_class(value: &Value) -> bool {
    match value {
        Value::List(list) => list.elem_class().is_some_and(|inner| has_leaf_class(inner)),
        _ => true,
    }
}

/// The name a nested `Listing<...<Name>>` type ends in, through aliases and
/// nullability, when it is not a built-in type.
pub(super) fn listing_leaf_name(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<String> {
    use crate::parser::TypeExpr;
    let mut ty = ty.clone();
    for _ in 0..64 {
        ty = match resolve_alias_chain(&ty, scope, &mut HashSet::default())? {
            TypeExpr::Generic(name, args) if name == "Listing" && args.len() == 1 => {
                args[0].clone()
            }
            // `Listing<Node>(!isEmpty)`: the type it constrains.
            TypeExpr::Constrained(base, ..) => {
                crate::parser::parse_type_name(base.trim_start_matches('*')).ok()?
            }
            TypeExpr::Named(name) => {
                let name = name
                    .trim_start_matches('*')
                    .trim_end_matches('?')
                    .to_string();
                return (!matches!(
                    name.as_str(),
                    "String"
                        | "Int"
                        | "Float"
                        | "Boolean"
                        | "Number"
                        | "Any"
                        | "Dynamic"
                        | "Duration"
                        | "DataSize"
                        | "Listing"
                ))
                .then_some(name);
            }
            _ => return None,
        };
    }
    None
}

impl Evaluator {
    /// A body's locals and properties (`default`) are visible to all of its
    /// entries, including ones written before them, and to each other
    /// regardless of order. Bind each once the members of this body it reads
    /// are bound, so a function captures them too. `keep_default` leaves a
    /// `default` already in `scope` alone.
    fn bind_listing_members(
        &mut self,
        entries: &[Entry],
        scope: &mut Scope,
        keep_default: bool,
        depth: usize,
    ) -> Result<()> {
        let mut pending: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop)
                    if !(keep_default
                        && &*prop.name == "default"
                        && !has_modifier(&prop.modifiers, Modifier::Local)) =>
                {
                    Some((prop, prop.value.as_ref()?))
                }
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
            let value = self.eval_expr(expr, scope, depth + 1)?;
            bind_listing_member(scope, prop, value);
            unbound.remove(&prop.name);
        }
        Ok(())
    }
}
