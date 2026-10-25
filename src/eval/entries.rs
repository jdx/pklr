//! Evaluating properties and the entries of an object body.

use super::*;

impl Evaluator {
    /// Evaluate a property declared inside a class or object body. A union
    /// without a selected default stays undefined here, so an instance can
    /// assign it; only a module property must have a value to render.
    pub(super) fn eval_property(
        &mut self,
        prop: &Property,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        self.eval_property_with(prop, scope, depth, false)
    }

    pub(super) fn eval_property_with(
        &mut self,
        prop: &Property,
        scope: &Scope,
        depth: usize,
        require_union_default: bool,
    ) -> Result<Option<Value>> {
        if let Some(expr) = &prop.value {
            // `x: T = new { ... }` amends T's default, so an untyped `new`
            // takes its parent from the declared type.
            let mut value = match &prop.type_ann {
                Some(ty) if parser::has_untyped_result_new(expr) && !type_is_any(ty) => {
                    let mut inferred = expr.clone();
                    parser::infer_method_return_new(&mut inferred, ty);
                    self.eval_expr(&inferred, scope, depth)?
                }
                _ => self.eval_expr(expr, scope, depth)?,
            };
            apply_mapping_type_annotation(&mut value, prop.type_ann.as_ref());
            return Ok(Some(value));
        }
        if let Some(body) = &prop.body {
            let mut nullable_default = prop
                .type_ann
                .as_ref()
                .and_then(|ty| nullable_inner_default(ty, scope));
            if let Some(default) = &mut nullable_default {
                apply_mapping_type_annotation(default, prop.type_ann.as_ref());
            }
            // A body amendment amends a member the receiver itself holds. A
            // same-named property of an enclosing object, declared or
            // inherited, is a different member and must not become the base.
            let scoped = scope
                .get(&prop.name)
                .filter(|_| scope.vars.contains_key(prop.name.as_ref()));
            let amendment_base = scoped
                .filter(|value| !is_null_value(value))
                .or(nullable_default.as_ref())
                .or(scoped);
            if let Some(Value::List(items)) = amendment_base
                && matches!(items.kind(), ListKind::List | ListKind::Set)
            {
                let class = if items.kind() == ListKind::List {
                    "List"
                } else {
                    "Set"
                };
                return Err(Error::Eval(format!(
                    "Cannot instantiate, or amend an instance of, external class `{class}`."
                )));
            }
            if let Some(function @ Value::Lambda(params, ..)) = amendment_base {
                return Ok(Some(amended_function(
                    params.len(),
                    function.clone(),
                    body,
                    scope,
                )));
            }
            if matches!(amendment_base, Some(Value::List(_)))
                || (!matches!(amendment_base, Some(Value::Object(..)))
                    && (prop.type_ann.as_ref().is_some_and(type_is_listing)
                        || entries_are_listing_amendment(body)))
            {
                let mut items = match amendment_base {
                    Some(Value::List(existing)) => existing.clone(),
                    _ => ListValue::new(ListKind::Listing, Vec::new()),
                };
                let mut amendment_scope = scope.child();
                amendment_scope.set("super", Value::List(items.clone()));
                amendment_scope.receiver_entries = Some(body.clone());
                amendment_scope.receiver_list_base = Some(items.len());
                self.eval_listing_entries(body, &amendment_scope, depth, items.make_mut())?;
                return Ok(Some(Value::List(items)));
            }
            // `foo { ... }` — object body amendment.
            // If the property already has a value in scope (e.g., from a base class),
            // amend that value so its ObjectSource (type info, default template) is preserved.
            if let Some(Value::Object(existing_map, Some(src))) = amendment_base {
                if !src.mapping_value_types.is_empty() {
                    // Mapping ObjectSource entries are mapping body entries such as
                    // `default` and dynamic keys. Rebuild the entry map with the
                    // type-aware evaluator so single-type and union mappings both keep
                    // mapping defaults plus converter type metadata after amendment.
                    let (inherited_scope, mut amendment_scope) =
                        mapping_amendment_scopes(src.scope(), src.scope_declared(), scope);
                    amendment_scope.set(
                        "super",
                        Value::Object(Arc::clone(existing_map), Some(Arc::clone(src))),
                    );
                    let mut receiver_entries = existing_map
                        .keys()
                        .map(|key| Entry::DynProperty(Expr::String(Arc::clone(key)), Expr::Null))
                        .collect::<Vec<_>>();
                    receiver_entries.extend_from_slice(body);
                    amendment_scope.receiver_entries = Some(Arc::new(receiver_entries));
                    let value_type_defaults = src
                        .mapping_value_types
                        .iter()
                        .filter_map(|name| {
                            let name = mapping_value_type_base(name);
                            resolve_dotted(&inherited_scope, name)
                                .map(|value| (name.to_string(), value))
                        })
                        .collect::<Vec<_>>();
                    let inherited_default =
                        self.find_default_template(&src.entries, &inherited_scope, depth)?;
                    let mut amended = ObjectMap::default();
                    self.eval_mapping_entries_with_type_default(
                        &src.entries,
                        &inherited_scope,
                        depth,
                        &mut amended,
                        &value_type_defaults,
                        &src.mapping_value_types,
                        MappingInheritedDefault::default(),
                    )?;
                    amended.extend(existing_map.iter().map(|(k, v)| (k.clone(), v.clone())));
                    self.eval_mapping_entries_with_type_default(
                        body,
                        &amendment_scope,
                        depth,
                        &mut amended,
                        &value_type_defaults,
                        &src.mapping_value_types,
                        MappingInheritedDefault {
                            value: inherited_default,
                            entries: find_default_body_entries(&src.entries),
                        },
                    )?;
                    return Ok(Some(Value::Object(
                        Arc::new(amended),
                        Some(Arc::clone(src)),
                    )));
                }
                return Ok(Some(self.eval_amended_object(
                    existing_map,
                    src,
                    body,
                    scope,
                    depth,
                )?));
            }
            let mut body_scope = scope.child();
            body_scope.set("super", Value::Object(Arc::default(), None));
            let val = self.eval_entries(body, &body_scope, depth)?;
            return Ok(Some(val));
        }
        if let Some(ty) = &prop.type_ann {
            if let Some(message) = type_default_error(ty, scope) {
                return Err(Error::Eval(message));
            }
            let mut default = type_default_value(ty, scope);
            // A class used as a property's implicit default denotes a fresh
            // instance, not its unchecked class template. Rebuild it so
            // member failures and declared types are checked before the
            // property can render.
            if let Some(Value::Object(map, Some(source))) = default.as_ref()
                && source.type_name.is_some()
            {
                let class_identity = source
                    .type_identity
                    .clone()
                    .or_else(|| source.type_name.clone())
                    .expect("typed class defaults always have an identity or name");
                if !self.active_typed_defaults.insert(nm(&class_identity)) {
                    return Err(Error::Eval("recursive typed default".into()));
                }
                let instance = self.eval_amended_object(map, source, &[], scope, depth);
                self.active_typed_defaults.remove(class_identity.as_str());
                let instance = instance?;
                if let Some(message) = rendered_member_failure(&instance) {
                    return Err(Error::Eval(message.to_string()));
                }
                default = Some(instance);
            }
            if let Some(default) = &mut default {
                apply_mapping_type_annotation(default, Some(ty));
            }
            if require_union_default
                && default.is_none()
                && matches!(ty, crate::parser::TypeExpr::Union(variants) if !variants.iter().any(is_default_type))
            {
                return Err(Error::Eval(format!(
                    "union property '{}' has no selected default",
                    prop.name
                )));
            }
            return Ok(default);
        }
        Ok(None)
    }

    pub(super) fn eval_entries(
        &mut self,
        entries: &Body,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let mut receiver_scope = scope.clone();
        receiver_scope.receiver_entries = Some(entries.clone());
        receiver_scope.receiver_list_base = None;
        self.eval_entries_with_lexical_scopes(
            entries,
            &receiver_scope,
            depth,
            None,
            None,
            None,
            false,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)] // Mapping context is independent of inherited source.
    pub(super) fn eval_entries_with_lexical_scopes(
        &mut self,
        entries: &Body,
        scope: &Scope,
        depth: usize,
        entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
        inherited_source: Option<&ObjectSource>,
        initial_non_const_members: Option<&HashSet<Name>>,
        // Generator bodies do not inherit the receiver's source entries, but
        // they do inherit its mapping-key semantics.
        mapping_context: bool,
        // For a generator body, the members its receiver holds so far. Its
        // members are the receiver's, so a body amendment (`o { ... }` or
        // `["k"] { ... }`) amends the receiver's existing member.
        receiver_members: Option<&ReceiverMembers<'_>>,
    ) -> Result<Value> {
        // A Mapping may retain the first spelling of a native-unit key, while
        // Pkl identity is its normalized amount. Keep that rule in the shared
        // amendment evaluator rather than selecting a second evaluator from
        // the key's surface syntax.
        let mapping_entries = mapping_context
            || inherited_source.is_some_and(|source| source.kind == ObjectKind::Mapping);
        let mut child_scope = scope.child();
        let entry_owners = entry_scope_owners(entries, entry_scopes, inherited_source);
        let own_body = own_body_names(entries, entry_scopes, inherited_source);
        // Entries without a captured scope belong to the object's own
        // definition body. Bindings they see as declared are that body's
        // members; members the object inherits from elsewhere (a parent class
        // or an amendment) are not, so a nested object's inherited member of
        // the same name still resolves through its own `this`.
        let binds_declared = |name: &str| own_body.as_ref().is_none_or(|own| own.contains(name));
        let own_body_scope = own_body.as_ref().map(|own| (scope, own));
        // Inherited members whose entries are evaluated again below. Until its
        // first entry runs, a member holds the parent's final value.
        let mut inherited_seeded: HashSet<&str> = HashSet::default();
        if let Some(source) = inherited_source {
            for entry in source.entries.iter() {
                if let Entry::Property(prop) = entry
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                    && source.evaluated_properties.contains(&prop.name)
                    && let Some(value) = source.scope().get(prop.name.as_ref())
                {
                    child_scope.set(&prop.name, value.clone());
                    inherited_seeded.insert(prop.name.as_ref());
                }
            }
        }
        // Locals of an enclosing object that alias its `this` (`local self =
        // this`) hold the same snapshot. Drop the ones this body never names so
        // they are not captured by the object built here; `outer` keeps them
        // when the body uses `outer`, since `outer.self` would still reach them.
        let unused_this_aliases = {
            let aliases = scope.visible_this_aliases();
            if aliases.is_empty() {
                aliases
            } else {
                let refs = self.body_referenced_roots(entries);
                if refs.contains("outer") {
                    Vec::new()
                } else {
                    aliases
                        .into_iter()
                        .filter(|name| !refs.contains(name))
                        .collect()
                }
            }
        };
        // `outer` is only reachable by name, so a body that never mentions it
        // (the common case) skips flattening the enclosing scope for it.
        if entries_mention(entries, "outer") || scope.type_aliases_mention("outer") {
            // Set `outer` to a snapshot of the parent scope's variables as an object.
            // Also insert Null for any nullable-no-default properties declared in these
            // entries but absent from the parent scope, so that `outer.optionalProp`
            // resolves to Null rather than failing with "field not found".
            let mut outer_map = scope.flatten();
            // `this` inside the body is rebound to the new object, so the parent's
            // `this` snapshot is unreachable through `outer`. Leaving it out keeps
            // nested objects from holding a reference to the parent's property map,
            // which would otherwise force a full copy on every parent insert.
            outer_map.shift_remove("this");
            for name in &unused_this_aliases {
                outer_map.shift_remove(&**name);
            }
            for entry in entries.iter() {
                if let Entry::Property(prop) = entry
                    && prop.value.is_none()
                    && prop.body.is_none()
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                    && !outer_map.contains_key(prop.name.as_ref())
                    && matches!(prop.type_ann, Some(crate::parser::TypeExpr::Nullable(_)))
                {
                    outer_map.insert(prop.name.as_ref().into(), Value::Null);
                }
            }
            let outer_obj = Value::Object(Arc::new(outer_map), None);
            child_scope.set("outer", outer_obj);
        }
        // Class-as-a-function definitions commonly use `local self = this` so
        // output properties can close over the amended instance. Bind `this`
        // before locals are evaluated, then keep direct aliases synchronized as
        // properties populate the instance.
        // A generator body is evaluated as a temporary object, but its `this`
        // is the enclosing receiver. Seed its receiver view without copying
        // those members into its output map.
        let mut all_props: Arc<ObjectMap> = receiver_members.map_or_else(Arc::default, |members| {
            let mut receiver = members
                .outer
                .map_or_else(ObjectMap::default, |outer| (*outer.own).clone());
            receiver.extend(
                members
                    .own
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            Arc::new(receiver)
        });
        let mut this_aliases = Vec::<Name>::new();
        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
        // Locals run before properties, so an untyped local may initially be
        // unable to read a member of this body. Keep only those locals for a
        // later retry as members bind; typed locals retain their existing
        // failure behavior.
        let mut failed_locals: Vec<usize> = Vec::new();
        // A property may depend on one of those locals. This is deliberately
        // narrower than a general object retry: only value properties that
        // failed while a local was pending are revisited.
        let mut failed_properties: Vec<(usize, String)> = Vec::new();
        // Generators and other output entries can also read one of those
        // deferred properties. Keep the whole entry for the same retry phase
        // instead of treating the temporary poison as its final error.
        let mut failed_entries: Vec<(usize, String)> = Vec::new();
        // A const local may only read const members. Seed this with actual
        // inherited members, then add own and generated members as they bind.
        let mut non_const_members: HashSet<Name> = HashSet::default();
        if let Some(source) = inherited_source {
            for name in &source.evaluated_properties {
                if body_member_is_non_const(&source.entries, name) {
                    non_const_members.insert(nm(name));
                }
            }
        }
        if let Some(initial) = initial_non_const_members {
            non_const_members.extend(initial.iter().cloned());
        }
        // Direct properties are known to be non-const from their declaration,
        // even before their value binds. Generator members stay absent here
        // until a branch actually yields them.
        let declared_direct_non_const_members: HashSet<Name> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop)
                    if !has_modifier(&prop.modifiers, Modifier::Local)
                        && !has_modifier(&prop.modifiers, Modifier::Const) =>
                {
                    Some(prop.name.clone())
                }
                _ => None,
            })
            .collect();
        // An own member shadows an enclosing binding even before evaluation.
        // Include generator bodies too: an untaken branch must not make a
        // const local capture an outer member of the same name.
        let pending_members = body_property_names(entries);
        let mut bound_members: HashSet<Name> = HashSet::default();

        macro_rules! retry_failed_locals {
            () => {{
                let mut recovered = true;
                while recovered {
                    recovered = false;
                    let mut retry_index = 0;
                    while retry_index < failed_locals.len() {
                        let entry_index = failed_locals[retry_index];
                        let Entry::Property(prop) = &entries[entry_index] else {
                            unreachable!("only local properties are retried");
                        };
                        let Some(expr) = &prop.value else {
                            unreachable!("only valued local properties are retried");
                        };
                        if let Some(message) = const_local_reads_non_const(prop, &non_const_members)
                        {
                            if binds_declared(&prop.name) {
                                child_scope.declare_poisoned(prop.name.clone(), message);
                            } else {
                                child_scope.poison(prop.name.clone(), message);
                            }
                            failed_locals.swap_remove(retry_index);
                            continue;
                        }
                        let mut active_scope = scope_for_object_entry(
                            entry_index,
                            &child_scope,
                            entry_scopes,
                            &entry_owners,
                            own_body_scope,
                        );
                        for name in pending_members
                            .iter()
                            .filter(|name| !bound_members.contains(*name))
                        {
                            active_scope
                                .poison(name.clone(), format!("undefined variable: {name}"));
                        }
                        let result = self.eval_expr(expr, &active_scope, depth);
                        drop(active_scope);
                        match result {
                            Ok(value) => {
                                if binds_declared(&prop.name) {
                                    child_scope.declare(&prop.name, value);
                                } else {
                                    child_scope.set(&prop.name, value);
                                }
                                if matches!(expr, Expr::Ident(name) if name == "this" || this_aliases.iter().any(|alias| **alias == **name))
                                {
                                    this_aliases.push(prop.name.clone());
                                    child_scope.mark_this_alias(&prop.name);
                                }
                                failed_locals.swap_remove(retry_index);
                                recovered = true;
                            }
                            Err(Error::Eval(message)) => {
                                if binds_declared(&prop.name) {
                                    child_scope.declare_poisoned(prop.name.clone(), message);
                                } else {
                                    child_scope.poison(prop.name.clone(), message);
                                }
                                retry_index += 1;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
            }};
        }

        macro_rules! bind_object_property_value {
            ($prop:expr, $value:expr, $entry_index:expr, $recovered:expr, $generated:ident, $map:ident, $member_indices:ident, $lambdas:expr $(,)?) => {{
                let prop = $prop;
                let value = $value;
                let entry_index = $entry_index;
                $generated.remove(prop.name.as_ref());
                if binds_declared(&prop.name) {
                    child_scope.declare(&prop.name, value.clone());
                } else {
                    child_scope.set(&prop.name, value.clone());
                }
                entry_owners.release_this(&this_aliases);
                if $recovered {
                    let index = all_props
                        .iter()
                        .position(|(key, _)| {
                            $member_indices
                                .get(key)
                                .is_some_and(|existing| *existing > entry_index)
                        })
                        .unwrap_or_else(|| all_props.len());
                    props_shift_insert(
                        &mut child_scope,
                        &this_aliases,
                        &mut all_props,
                        index,
                        prop.name.clone(),
                        value.clone(),
                    );
                } else {
                    props_insert(
                        &mut child_scope,
                        &this_aliases,
                        &mut all_props,
                        prop.name.clone(),
                        value.clone(),
                    );
                }
                if !has_modifier(&prop.modifiers, Modifier::Hidden) {
                    if $recovered {
                        let index = $map
                            .iter()
                            .position(|(key, _)| {
                                $member_indices
                                    .get(key)
                                    .is_some_and(|existing| *existing > entry_index)
                            })
                            .unwrap_or_else(|| $map.len());
                        $map.shift_insert(index, prop.name.as_ref().into(), value);
                    } else {
                        $map.insert(prop.name.as_ref().into(), value);
                    }
                }
                $member_indices.insert(prop.name.as_ref().into(), entry_index);
                if !has_modifier(&prop.modifiers, Modifier::Const) {
                    non_const_members.insert(prop.name.clone());
                }
                bound_members.insert(prop.name.clone());
                self.guard_const_local_lambdas(&mut child_scope, $lambdas, &non_const_members);
                refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                retry_failed_locals!();
            }};
        }

        // First pass: collect locals, class definitions, and type aliases in
        // declaration order so they can reference each other correctly.
        // Non-lambda locals are evaluated eagerly; lambda locals are deferred
        // to a second pass so they capture the fully-populated scope.
        let mut deferred_lambdas: Vec<(Name, &Property, &crate::parser::Expr, usize)> = Vec::new();
        for (entry_index, entry) in entries.iter().enumerate() {
            // Only locals, classes and type aliases are handled in this pass,
            // so build the entry's scope only for those.
            if !matches!(
                entry,
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some()
            ) && !matches!(entry, Entry::ClassDef(..) | Entry::TypeAlias(..))
            {
                continue;
            }
            let mut active_scope = scope_for_object_entry(
                entry_index,
                &child_scope,
                entry_scopes,
                &entry_owners,
                own_body_scope,
            );
            if matches!(entry, Entry::Property(prop)
                if has_modifier(&prop.modifiers, Modifier::Local)
                    && (!matches!(prop.value.as_ref(), Some(Expr::Lambda(..)))
                        || has_modifier(&prop.modifiers, Modifier::Const)))
            {
                for name in pending_members
                    .iter()
                    .filter(|name| !bound_members.contains(*name))
                {
                    active_scope.poison(name.clone(), format!("undefined variable: {name}"));
                }
            }
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    let expr = prop.value.as_ref().unwrap();
                    // Bind every local in declaration order so later locals and
                    // entries can reference it (e.g. a non-lambda local that
                    // calls a lambda local defined just above it).
                    let is_lambda = matches!(expr, Expr::Lambda(..));
                    let const_message = if is_lambda {
                        let mut lambda_non_const_members = non_const_members.clone();
                        lambda_non_const_members
                            .extend(declared_direct_non_const_members.iter().cloned());
                        const_local_reads_non_const(prop, &lambda_non_const_members)
                    } else {
                        const_local_reads_non_const(prop, &non_const_members)
                    };
                    let result = if is_lambda {
                        self.eval_expr(expr, &active_scope, depth)
                            .map(|value| self.const_local_lambda_guard(value, const_message))
                    } else {
                        match const_message {
                            Some(message) => Err(Error::Eval(message)),
                            None => self.eval_expr(expr, &active_scope, depth),
                        }
                    };
                    // Release the entry scope before binding, as for properties.
                    drop(active_scope);
                    match result {
                        Ok(val) => {
                            if binds_declared(&prop.name) {
                                child_scope.declare(&prop.name, val);
                            } else {
                                child_scope.set(&prop.name, val);
                            }
                            if matches!(expr, Expr::Ident(name) if name == "this" || this_aliases.iter().any(|alias| **alias == **name))
                            {
                                this_aliases.push(prop.name.clone());
                                child_scope.mark_this_alias(&prop.name);
                            }
                        }
                        Err(Error::Eval(message)) => {
                            if prop.type_ann.is_none()
                                && const_local_reads_non_const(prop, &non_const_members).is_none()
                            {
                                failed_locals.push(entry_index);
                            }
                            if binds_declared(&prop.name) {
                                child_scope.declare_poisoned(prop.name.clone(), message);
                            } else {
                                child_scope.poison(prop.name.clone(), message);
                            }
                        }
                        Err(error) => return Err(error),
                    }
                    if matches!(expr, crate::parser::Expr::Lambda(..)) {
                        // Lambda evaluation only captures the current scope; it
                        // does not run the body. Bind once for declaration-order
                        // visibility, then re-bind after properties for late
                        // binding of overrides.
                        deferred_lambdas.push((prop.name.clone(), prop, expr, entry_index));
                    }
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self.eval_class_def(
                        name,
                        class_mods,
                        parent.as_deref(),
                        body,
                        &active_scope,
                        depth,
                    )?;
                    if binds_declared(name) {
                        child_scope.declare(name, defaults);
                    } else {
                        child_scope.set(name, defaults);
                    }
                }
                Entry::TypeAlias(name, ty) => {
                    let mut resolved_scope = active_scope;
                    self.eval_type_alias(name, ty, &mut resolved_scope);
                    child_scope.set_type_alias(name.clone(), ty.clone());
                    if let Some(value) = resolved_scope.vars.get(name.as_str()) {
                        child_scope.set(name, value.clone());
                    }
                }
                _ => {}
            }
        }

        self.guard_const_local_lambdas(&mut child_scope, &deferred_lambdas, &non_const_members);

        let mut default_template: Option<Value> = None;
        for (entry_index, entry) in entries.iter().enumerate() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if &*prop.name != "default" || has_modifier(&prop.modifiers, Modifier::Local) {
                continue;
            }
            let mut active_scope = scope_for_object_entry(
                entry_index,
                &child_scope,
                entry_scopes,
                &entry_owners,
                own_body_scope,
            );
            if let Some(template) = &default_template {
                active_scope.set("default", template.clone());
            }
            default_template = self.eval_property(prop, &active_scope, depth)?;
        }

        let mut map: ObjectMap = ObjectMap::default();
        // Source entry positions for members already emitted. A property that
        // recovers after its local becomes available uses these to return to
        // its declaration position instead of remaining appended at the end.
        let mut member_entry_indices: HashMap<Arc<str>, usize> = HashMap::default();
        // A Dynamic body can contain bare elements as well as named members.
        // Keep the former separately so XML elements can render precisely the
        // values Pkl gives them, in source order.
        let mut elements: Vec<Value> = Vec::new();
        // Names of members produced by generators and not since rebound by a
        // property entry.
        let mut generated: HashSet<Arc<str>> = HashSet::default();
        // A single direct dynamic member cannot conflict with anything. Avoid
        // the hash lookup on that common shape; tracking is needed only once a
        // second dynamic member or a generator can produce another name.
        let mut direct_dynamic_members = 0;
        let mut track_dynamic_members = false;
        for entry in entries.iter() {
            match entry {
                Entry::DynProperty(..) => {
                    direct_dynamic_members += 1;
                    track_dynamic_members |= direct_dynamic_members > 1;
                }
                Entry::ForGenerator(..) | Entry::WhenGenerator(..) => {
                    track_dynamic_members = true;
                }
                _ => {}
            }
        }
        // The output map is keyed by its rendered member name, so duplicate
        // bookkeeping must use that same identity. Sharing the set with
        // generators makes direct-before-generator and generator-before-direct
        // checks symmetric.
        let mut defined_by_layer: Option<HashSet<(usize, Arc<str>)>> = None;
        macro_rules! eval_output_entry {
            ($entry_index:expr, $entry:expr) => {{
                (|entry_index: usize, entry: &Entry| -> Result<()> {
            match entry {
                Entry::Property(prop) => {
                    let mods = &prop.modifiers;
                    if has_modifier(mods, Modifier::Local) {
                        return Ok(());
                    }
                    // Skip the `default` property — it's a template, not an output entry
                    if &*prop.name == "default" && default_template.is_some() {
                        return Ok(());
                    }
                    if has_modifier(mods, Modifier::Abstract)
                        && prop.value.is_none()
                        && prop.body.is_none()
                    {
                        return Ok(()); // abstract without value — skip (must be overridden)
                    }
                    // The first entry of a re-evaluated inherited member starts
                    // its amendment chain. Amending the parent's final value
                    // would apply the inherited bodies twice (`l { 1 }`
                    // amended again with `1`), so it starts from no value.
                    if inherited_seeded.remove(prop.name.as_ref())
                        && prop.value.is_none()
                        && prop.body.is_some()
                    {
                        child_scope.set(&prop.name, Value::Null);
                    }
                    refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                    let mut active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    // eval_property amends the value bound for the name in the
                    // receiver's own scope. A generator body can amend both a
                    // member previously generated in this body and one held by
                    // its enclosing receiver.
                    if prop.value.is_none()
                        && prop.body.is_some()
                        && let Some(existing) = all_props.get(prop.name.as_ref()).or_else(|| {
                            receiver_members.and_then(|members| members.get(prop.name.as_ref()))
                        })
                    {
                        active_scope.set(&prop.name, existing.clone());
                    }
                    let value = match self.eval_property(prop, &active_scope, depth) {
                        Ok(value) => value,
                        Err(Error::Eval(message))
                            if prop.value.is_some() && !failed_locals.is_empty() =>
                        {
                            // This body declaration shadows an enclosing or
                            // inherited property even while its local
                            // dependency is unresolved. Keep later members
                            // from binding that stale value before retry.
                            drop(active_scope);
                            if binds_declared(&prop.name) {
                                child_scope.declare_poisoned(prop.name.clone(), message.clone());
                            } else {
                                child_scope.poison(prop.name.clone(), message.clone());
                            }
                            failed_properties.push((entry_index, message));
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    };
                    // Release the entry scope first: it may share the object
                    // scope's bindings, which binding the value would then copy.
                    drop(active_scope);
                    if let Some(v) = value {
                        bind_object_property_value!(
                            prop,
                            v,
                            entry_index,
                            false,
                            generated,
                            map,
                            member_entry_indices,
                            &deferred_lambdas,
                        );
                    }
                }
                Entry::DynProperty(key_expr, val_expr) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let class_key = self.class_key_of(key_expr, &active_scope);
                    let key = class_key
                        .is_none()
                        .then(|| self.eval_expr(key_expr, &active_scope, depth))
                        .transpose()?;
                    let key_str = match class_key {
                        Some(key) => key,
                        None => value_to_key(
                            key.as_ref().expect("non-class mapping keys are evaluated"),
                        )?,
                    };
                    let storage_key = if mapping_entries {
                        equivalent_mapping_key(&map, &key_str).unwrap_or_else(|| key_str.clone())
                    } else {
                        key_str.clone()
                    };
                    if mapping_entries && let Some(key) = key.as_ref() {
                        self.remember_mapping_key(storage_key.clone(), key);
                    }
                    // A body may define each key once. An object body still
                    // amends an inherited value, but it is a definition in
                    // this body and must participate in duplicate detection.
                    if track_dynamic_members {
                        let defined_by_layer = defined_by_layer.get_or_insert_default();
                        let layer = entry_layer(entry_scopes, entry_index);
                        let duplicate = mapping_entries
                            && defined_by_layer.iter().any(|(defined_layer, defined_key)| {
                                *defined_layer == layer
                                    && crate::value::mapping_storage_keys_equal(
                                        defined_key,
                                        &key_str,
                                    )
                            });
                        if duplicate || !defined_by_layer.insert((layer, key_str.clone())) {
                            let key = match key.as_ref() {
                                None => key_str.to_string(),
                                Some(Value::String(s)) => format!("{s:?}"),
                                Some(key) => value_to_display(key),
                            };
                            return Err(Error::Eval(format!(
                                "Duplicate definition of member `{key}`."
                            )));
                        }
                    }
                    // `["key"] { ... }` amends an entry inherited from the parent
                    // (for example when amending an untyped `Mapping`) rather than
                    // replacing it.
                    if let Expr::ObjectBody(body) = val_expr
                        && let Some(existing @ (Value::Object(..) | Value::List(_))) = map
                            .get(&storage_key)
                            .or_else(|| {
                                receiver_members.and_then(|members| {
                                    if mapping_entries {
                                        equivalent_mapping_key(members.own, &key_str)
                                            .and_then(|key| members.own.get(&key))
                                            .or_else(|| {
                                                members.outer.and_then(|outer| {
                                                    equivalent_mapping_key(outer.own, &key_str)
                                                        .and_then(|key| outer.own.get(&key))
                                                })
                                            })
                                    } else {
                                        members.get(&key_str)
                                    }
                                })
                            })
                            .cloned()
                    {
                        // A listing amendment only takes elements, so a property
                        // would otherwise be dropped silently. Reject it as Pkl does.
                        if matches!(existing, Value::List(_))
                            && let Some(name) = find_listing_body_property(body)
                        {
                            return Err(Error::Eval(format!(
                                "cannot amend listing entry '{key_str}' with property '{name}': \
                                 object of type Listing cannot have a property (other than default)"
                            )));
                        }
                        let val =
                            self.eval_value_amendment(existing, body, &active_scope, depth)?;
                        drop(active_scope);
                        entry_owners.release_this(&this_aliases);
                        props_insert_mapping(
                            &mut child_scope,
                            &this_aliases,
                            &mut all_props,
                            storage_key.clone(),
                            val.clone(),
                            mapping_entries,
                        );
                        member_entry_indices.insert(storage_key.clone(), entry_index);
                        map.insert(storage_key, val);
                        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        retry_failed_locals!();
                        return Ok(());
                    }
                    let val = if let Some(Value::Object(template_map, Some(src))) =
                        &default_template
                        && let Expr::ObjectBody(body) = val_expr
                    {
                        // Default template has ObjectSource — use eval_amended_object
                        // so nested property amendments work properly.
                        let mut result = self.eval_amended_object(
                            template_map,
                            src,
                            body,
                            &active_scope,
                            depth,
                        )?;
                        // Propagate the template's type_name so converters can match.
                        if let Some(ref tn) = src.type_name
                            && let Value::Object(_, ref mut result_src) = result
                        {
                            let new_src = match result_src.take() {
                                Some(s) => {
                                    let mut ns = Arc::unwrap_or_clone(s);
                                    ns.type_name = Some(tn.clone());
                                    ns
                                }
                                None => ObjectSource {
                                    entries: vec![].into(),
                                    captured: SourceScope::default(),
                                    body_members: HashSet::default(),
                                    is_open: true,
                                    is_abstract: false,
                                    type_name: Some(tn.clone()),
                                    type_identity: src.type_identity.clone(),
                                    parent_type_names: src.parent_type_names.clone(),
                                    parent_type_identities: src.parent_type_identities.clone(),
                                    entry_scopes: Vec::new(),
                                    evaluated_properties: Vec::new(),
                                    elements: Vec::new(),
                                    mapping_value_types: Vec::new(),
                                    deprecated: merge_deprecated(&src.deprecated, body),
                                    poisoned_members: None,
                                    kind: ObjectKind::Object,
                                    is_parsed_json: false,
                                    prototype: None,
                                },
                            };
                            *result_src = Some(std::sync::Arc::new(new_src));
                        }
                        result
                    } else {
                        let mut val = self.eval_expr(val_expr, &active_scope, depth)?;
                        if let Some(ref tpl) = default_template {
                            val = merge_values(tpl.clone(), val);
                        }
                        val
                    };
                    drop(active_scope);
                    entry_owners.release_this(&this_aliases);
                    props_insert_mapping(
                        &mut child_scope,
                        &this_aliases,
                        &mut all_props,
                        storage_key.clone(),
                        val.clone(),
                        mapping_entries,
                    );
                    member_entry_indices.insert(storage_key.clone(), entry_index);
                    map.insert(storage_key, val);
                    refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                    retry_failed_locals!();
                }
                Entry::Spread(expr) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let val = self.eval_expr(expr, &active_scope, depth)?;
                    check_iterable(&val)?;
                    match val {
                        Value::Object(m, source) => {
                            if let Some(inherited) = inherited_source
                                && let Some(message) =
                                    immutable_copied_member_error(m.keys(), inherited)
                            {
                                return Err(Error::Eval(message));
                            }
                            drop(active_scope);
                            entry_owners.release_this(&this_aliases);
                            props_extend_mapping(
                                &mut child_scope,
                                &this_aliases,
                                &mut all_props,
                                m.iter().map(|(k, v)| (k.clone(), v.clone())),
                                mapping_entries,
                            );
                            extend_object_entries(&mut map, &m, mapping_entries);
                            member_entry_indices
                                .extend(m.keys().cloned().map(|key| (key, entry_index)));
                            if let Some(source) = source {
                                elements.extend(source.elements.iter().cloned());
                            }
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                        }
                        Value::List(items) => elements.extend(items.iter().cloned()),
                        _ => unreachable!("check_iterable accepted an unsupported value"),
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let collection = self.eval_expr(&fgen.collection, &active_scope, depth)?;
                    let items = collection_to_items(collection)?;
                    for (k, v) in items {
                        let mut iter_scope = active_scope.child();
                        iter_scope.set(&fgen.val_var, v);
                        if let Some(key_var) = &fgen.key_var {
                            iter_scope.set(key_var, k);
                        }
                        let body_val = self.eval_entries_with_lexical_scopes(
                            &fgen.body,
                            &iter_scope,
                            depth,
                            None,
                            None,
                            None,
                            mapping_entries,
                            Some(&ReceiverMembers {
                                own: &all_props,
                                outer: receiver_members,
                            }),
                        )?;
                        if let Value::Object(m, source) = body_val {
                            if let Some(inherited) = inherited_source
                                && let Some(message) =
                                    immutable_copied_member_error(m.keys(), inherited)
                            {
                                return Err(Error::Eval(message));
                            }
                            if track_dynamic_members {
                                record_generated_members(
                                    defined_by_layer.get_or_insert_default(),
                                    entry_layer(entry_scopes, entry_index),
                                    &m,
                                    &fgen.body,
                                    mapping_entries,
                                )?;
                            }
                            entry_owners.release_this(&this_aliases);
                            props_extend_mapping(
                                &mut child_scope,
                                &this_aliases,
                                &mut all_props,
                                m.iter().map(|(k, v)| (k.clone(), v.clone())),
                                mapping_entries,
                            );
                            for name in m.keys() {
                                if body_member_is_non_const(&fgen.body, name) {
                                    non_const_members.insert(nm(name));
                                }
                            }
                            bound_members.extend(m.keys().cloned());
                            self.guard_const_local_lambdas(
                                &mut child_scope,
                                &deferred_lambdas,
                                &non_const_members,
                            );
                            generated.extend(m.keys().cloned());
                            extend_object_entries(&mut map, &m, mapping_entries);
                            member_entry_indices
                                .extend(m.keys().cloned().map(|key| (key, entry_index)));
                            if let Some(source) = source {
                                elements.extend(source.elements.iter().cloned());
                            }
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                            retry_failed_locals!();
                        }
                    }
                }
                Entry::WhenGenerator(wgen) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    let cond = self.eval_expr(&wgen.condition, &active_scope, depth)?;
                    if is_truthy(&cond) {
                        let body_val = self.eval_entries_with_lexical_scopes(
                            &wgen.body,
                            &active_scope,
                            depth,
                            None,
                            None,
                            None,
                            mapping_entries,
                            Some(&ReceiverMembers {
                                own: &all_props,
                                outer: receiver_members,
                            }),
                        )?;
                        if let Value::Object(m, source) = body_val {
                            if let Some(inherited) = inherited_source
                                && let Some(message) =
                                    immutable_copied_member_error(m.keys(), inherited)
                            {
                                return Err(Error::Eval(message));
                            }
                            if track_dynamic_members {
                                record_generated_members(
                                    defined_by_layer.get_or_insert_default(),
                                    entry_layer(entry_scopes, entry_index),
                                    &m,
                                    &wgen.body,
                                    mapping_entries,
                                )?;
                            }
                            entry_owners.release_this(&this_aliases);
                            props_extend_mapping(
                                &mut child_scope,
                                &this_aliases,
                                &mut all_props,
                                m.iter().map(|(k, v)| (k.clone(), v.clone())),
                                mapping_entries,
                            );
                            for name in m.keys() {
                                if body_member_is_non_const(&wgen.body, name) {
                                    non_const_members.insert(nm(name));
                                }
                            }
                            bound_members.extend(m.keys().cloned());
                            self.guard_const_local_lambdas(
                                &mut child_scope,
                                &deferred_lambdas,
                                &non_const_members,
                            );
                            generated.extend(m.keys().cloned());
                            extend_object_entries(&mut map, &m, mapping_entries);
                            member_entry_indices
                                .extend(m.keys().cloned().map(|key| (key, entry_index)));
                            if let Some(source) = source {
                                elements.extend(source.elements.iter().cloned());
                            }
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                            retry_failed_locals!();
                        }
                    } else if let Some(else_body) = &wgen.else_body {
                        let else_val = self.eval_entries_with_lexical_scopes(
                            else_body,
                            &active_scope,
                            depth,
                            None,
                            None,
                            None,
                            mapping_entries,
                            Some(&ReceiverMembers {
                                own: &all_props,
                                outer: receiver_members,
                            }),
                        )?;
                        if let Value::Object(m, source) = else_val {
                            if let Some(inherited) = inherited_source
                                && let Some(message) =
                                    immutable_copied_member_error(m.keys(), inherited)
                            {
                                return Err(Error::Eval(message));
                            }
                            if track_dynamic_members {
                                record_generated_members(
                                    defined_by_layer.get_or_insert_default(),
                                    entry_layer(entry_scopes, entry_index),
                                    &m,
                                    else_body,
                                    mapping_entries,
                                )?;
                            }
                            entry_owners.release_this(&this_aliases);
                            props_extend_mapping(
                                &mut child_scope,
                                &this_aliases,
                                &mut all_props,
                                m.iter().map(|(k, v)| (k.clone(), v.clone())),
                                mapping_entries,
                            );
                            for name in m.keys() {
                                if body_member_is_non_const(else_body, name) {
                                    non_const_members.insert(nm(name));
                                }
                            }
                            bound_members.extend(m.keys().cloned());
                            self.guard_const_local_lambdas(
                                &mut child_scope,
                                &deferred_lambdas,
                                &non_const_members,
                            );
                            generated.extend(m.keys().cloned());
                            extend_object_entries(&mut map, &m, mapping_entries);
                            member_entry_indices
                                .extend(m.keys().cloned().map(|key| (key, entry_index)));
                            if let Some(source) = source {
                                elements.extend(source.elements.iter().cloned());
                            }
                            refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
                            retry_failed_locals!();
                        }
                    }
                }
                Entry::Elem(expr) => {
                    let active_scope = scope_for_object_entry(
                        entry_index,
                        &child_scope,
                        entry_scopes,
                        &entry_owners,
                        own_body_scope,
                    );
                    elements.push(self.eval_expr(expr, &active_scope, depth + 1)?);
                }
                Entry::ClassDef(..) | Entry::TypeAlias(..) => {} // handled in scope setup
                Entry::Predicate(..) => return Err(member_predicates_unsupported()),
            }
                    Ok(())
                })($entry_index, $entry)
            }};
        }

        for (entry_index, entry) in entries.iter().enumerate() {
            // Only a failure while locals are still pending is deferred and
            // retried, so the state is snapshotted only then. Entries never add
            // to `failed_locals`, so empty before means empty after and the
            // rollback arm below cannot be taken.
            let rollback = (!failed_locals.is_empty()).then(|| {
                (
                    child_scope.clone(),
                    all_props.clone(),
                    map.clone(),
                    elements.clone(),
                    generated.clone(),
                    member_entry_indices.clone(),
                    non_const_members.clone(),
                    bound_members.clone(),
                    failed_locals.clone(),
                    failed_properties.clone(),
                    this_aliases.clone(),
                    inherited_seeded.clone(),
                    defined_by_layer.clone(),
                )
            });
            match eval_output_entry!(entry_index, entry) {
                Ok(()) => {}
                Err(Error::Eval(message)) if rollback.is_some() && !failed_locals.is_empty() => {
                    (
                        child_scope,
                        all_props,
                        map,
                        elements,
                        generated,
                        member_entry_indices,
                        non_const_members,
                        bound_members,
                        failed_locals,
                        failed_properties,
                        this_aliases,
                        inherited_seeded,
                        defined_by_layer,
                    ) = rollback.expect("checked by the guard");
                    failed_entries.push((entry_index, message));
                }
                Err(error) => return Err(error),
            }
        }
        // Revisit only the ordinary value properties deferred because they
        // read a still-pending local. Once no deferred property recovers, the
        // first current error is the one Pkl surfaces.
        while !failed_properties.is_empty() {
            let mut retrying = std::mem::take(&mut failed_properties);
            let mut recovered_property = false;
            for (entry_index, _) in retrying.drain(..) {
                let Entry::Property(prop) = &entries[entry_index] else {
                    unreachable!("only properties are deferred");
                };
                let active_scope = scope_for_object_entry(
                    entry_index,
                    &child_scope,
                    entry_scopes,
                    &entry_owners,
                    own_body_scope,
                );
                match self.eval_property(prop, &active_scope, depth) {
                    Ok(value) => {
                        drop(active_scope);
                        if let Some(value) = value {
                            bind_object_property_value!(
                                prop,
                                value,
                                entry_index,
                                true,
                                generated,
                                map,
                                member_entry_indices,
                                &deferred_lambdas,
                            );
                        }
                        recovered_property = true;
                    }
                    Err(Error::Eval(message)) => {
                        drop(active_scope);
                        failed_properties.push((entry_index, message));
                    }
                    Err(error) => return Err(error),
                }
            }
            if !recovered_property {
                return Err(Error::Eval(failed_properties[0].1.clone()));
            }
        }
        // Retry non-property output entries only after ordinary properties
        // have had a chance to resolve their deferred local dependencies.
        // This includes nested generator bodies, spreads, elements, and
        // dynamic entries that encountered a temporary poisoned property.
        while !failed_entries.is_empty() {
            let mut retrying = std::mem::take(&mut failed_entries);
            let mut recovered_entry = false;
            for (entry_index, _) in retrying.drain(..) {
                let entry = &entries[entry_index];
                let child_scope_before = child_scope.clone();
                let all_props_before = all_props.clone();
                let map_before = map.clone();
                let elements_before = elements.clone();
                let generated_before = generated.clone();
                let member_entry_indices_before = member_entry_indices.clone();
                let non_const_members_before = non_const_members.clone();
                let bound_members_before = bound_members.clone();
                let failed_locals_before = failed_locals.clone();
                let failed_properties_before = failed_properties.clone();
                let this_aliases_before = this_aliases.clone();
                let inherited_seeded_before = inherited_seeded.clone();
                let defined_by_layer_before = defined_by_layer.clone();
                match eval_output_entry!(entry_index, entry) {
                    Ok(()) => recovered_entry = true,
                    Err(Error::Eval(message)) => {
                        child_scope = child_scope_before;
                        all_props = all_props_before;
                        map = map_before;
                        elements = elements_before;
                        generated = generated_before;
                        member_entry_indices = member_entry_indices_before;
                        non_const_members = non_const_members_before;
                        bound_members = bound_members_before;
                        failed_locals = failed_locals_before;
                        failed_properties = failed_properties_before;
                        this_aliases = this_aliases_before;
                        inherited_seeded = inherited_seeded_before;
                        defined_by_layer = defined_by_layer_before;
                        failed_entries.push((entry_index, message));
                    }
                    Err(error) => return Err(error),
                }
            }
            if !recovered_entry {
                return Err(Error::Eval(failed_entries[0].1.clone()));
            }
        }
        // Evaluate deferred local lambdas (function definitions) AFTER all
        // properties so they capture overridden values (late binding).
        refresh_this_aliases(&mut child_scope, &this_aliases, &all_props);
        for (name, prop, expr, entry_index) in deferred_lambdas {
            let active_scope = scope_for_object_entry(
                entry_index,
                &child_scope,
                entry_scopes,
                &entry_owners,
                own_body_scope,
            );
            let val = self.eval_expr(expr, &active_scope, depth)?;
            drop(active_scope);
            // Lambda construction is lazy, so const-local validation must be
            // deferred until invocation too. At this point the final object
            // members are known; make a forbidden lambda body fail when it is
            // called without rejecting an otherwise unused local function.
            let val = self.const_local_lambda_guard(
                val,
                const_local_reads_non_const(prop, &non_const_members),
            );
            child_scope.set(name, val);
        }
        let hidden_aliases = unused_this_aliases
            .iter()
            // A binding of the same name made by this body shadows the alias.
            .filter(|name| {
                !child_scope.vars.contains_key(&**name)
                    && !child_scope.poisoned.contains_key(&**name)
            })
            .map(|name| name_of(name))
            .collect();
        let source = ObjectSource {
            entries: entries.clone(),
            captured: SourceScope::lazy(&child_scope, hidden_aliases, Vec::new()),
            body_members: own_body
                .clone()
                .unwrap_or_else(|| entries.iter().filter_map(entry_member_name).collect()),
            is_open: true, // default: allow new properties
            is_abstract: false,
            type_name: None,
            type_identity: None,
            parent_type_names: Vec::new(),
            parent_type_identities: Vec::new(),
            entry_scopes: entry_scopes.map(<[_]>::to_vec).unwrap_or_default(),
            evaluated_properties: all_props.keys().cloned().collect(),
            elements,
            mapping_value_types: Vec::new(),
            deprecated: collect_deprecated(entries),
            poisoned_members: None,
            kind: ObjectKind::Object,
            is_parsed_json: false,
            prototype: None,
        };
        Ok(Value::Object(Arc::new(map), Some(Arc::new(source))))
    }
}

/// Whether a body (or a generator in it) has a member matching `is_kind`.
pub(super) fn body_has(entries: &[Entry], is_kind: &dyn Fn(&Entry) -> bool) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::ForGenerator(generator) => body_has(&generator.body, is_kind),
        Entry::WhenGenerator(generator) => {
            body_has(&generator.body, is_kind)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(|body| body_has(body, is_kind))
        }
        entry => is_kind(entry),
    })
}

/// Whether an evaluated member of `entries` is a non-const property. A name
/// is considered only after the entry that produced it actually ran, so a
/// member in an untaken generator branch does not turn a const-local lookup
/// into a const violation.
pub(super) fn body_member_is_non_const(entries: &[Entry], name: &str) -> bool {
    entries.iter().rev().any(|entry| match entry {
        Entry::Property(prop)
            if &*prop.name == name && !has_modifier(&prop.modifiers, Modifier::Local) =>
        {
            !has_modifier(&prop.modifiers, Modifier::Const)
        }
        Entry::ForGenerator(generator) => body_member_is_non_const(&generator.body, name),
        Entry::WhenGenerator(generator) => {
            body_member_is_non_const(&generator.body, name)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(|body| body_member_is_non_const(body, name))
        }
        _ => false,
    })
}

/// Names a body can define as object properties, including names in generator
/// branches that may later bind. They must shadow enclosing bindings while a
/// const local waits to learn whether the branch actually produces them.
pub(super) fn body_property_names(entries: &[Entry]) -> HashSet<Name> {
    let mut names = HashSet::default();
    for entry in entries {
        match entry {
            Entry::Property(prop) if !has_modifier(&prop.modifiers, Modifier::Local) => {
                names.insert(prop.name.clone());
            }
            Entry::ForGenerator(generator) => names.extend(body_property_names(&generator.body)),
            Entry::WhenGenerator(generator) => {
                names.extend(body_property_names(&generator.body));
                if let Some(else_body) = &generator.else_body {
                    names.extend(body_property_names(else_body));
                }
            }
            _ => {}
        }
    }
    names
}

/// Pkl requires a const local to depend only on const members of its body.
/// Bare references are collected with the evaluator's normal lexical walker;
/// `this.member` and `super.member` need their own checks because the walker
/// records the receiver as the root rather than the selected member.
pub(super) fn const_local_reads_non_const(
    prop: &Property,
    non_const_members: &HashSet<Name>,
) -> Option<String> {
    if !has_modifier(&prop.modifiers, Modifier::Const) {
        return None;
    }
    let expr = prop.value.as_ref()?;
    let mut refs = HashSet::default();
    collect_expr_refs(expr, &mut refs, &HashSet::default());
    let name = refs
        .into_iter()
        .find(|name| non_const_members.contains(name))
        .map(|name| name.to_string())
        .or_else(|| this_member_reads_non_const(expr, non_const_members))?;
    Some(format!(
        "Cannot reference property `{name}` from here because it is not `const`."
    ))
}

/// Return the first non-const member read through a direct `this.member` or
/// `super.member` expression. Nested object bodies have their own receiver,
/// so this deliberately does not descend into them.
pub(super) fn this_member_reads_non_const(
    expr: &Expr,
    non_const_members: &HashSet<Name>,
) -> Option<String> {
    match expr {
        Expr::Field(base, name) | Expr::NullSafeField(base, name)
            if matches!(base.as_ref(), Expr::Ident(root) if root == "this" || root == "super")
                && non_const_members.contains(name.as_str()) =>
        {
            Some(name.clone())
        }
        Expr::Field(base, _) | Expr::NullSafeField(base, _) => {
            this_member_reads_non_const(base, non_const_members)
        }
        Expr::Index(base, index) | Expr::Binop(_, base, index) => {
            this_member_reads_non_const(base, non_const_members)
                .or_else(|| this_member_reads_non_const(index, non_const_members))
        }
        Expr::Call(callee, args) => {
            this_member_reads_non_const(callee, non_const_members).or_else(|| {
                args.iter()
                    .find_map(|arg| this_member_reads_non_const(arg, non_const_members))
            })
        }
        Expr::If(condition, then_expr, else_expr) => {
            this_member_reads_non_const(condition, non_const_members)
                .or_else(|| this_member_reads_non_const(then_expr, non_const_members))
                .or_else(|| this_member_reads_non_const(else_expr, non_const_members))
        }
        Expr::Let(_, value, body) => this_member_reads_non_const(value, non_const_members)
            .or_else(|| this_member_reads_non_const(body, non_const_members)),
        Expr::Is(value, _) | Expr::As(value, _) => {
            this_member_reads_non_const(value, non_const_members)
        }
        Expr::Lambda(_, value) => this_member_reads_non_const(value, non_const_members),
        Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::TemplateLocals(_, value)
        | Expr::Trace(value, _)
        | Expr::Read(value, _)
        | Expr::ReadOrNull(value, _)
        | Expr::ReadGlob(value, _) => this_member_reads_non_const(value, non_const_members),
        Expr::StringInterpolation(parts) => parts.iter().find_map(|part| match part {
            StringInterpPart::Expr(expr) => this_member_reads_non_const(expr, non_const_members),
            StringInterpPart::Literal(_) => None,
        }),
        // New and anonymous-object bodies create their own receivers.
        Expr::New(..) | Expr::InferredNew(..) | Expr::ObjectBody(..) | _ => None,
    }
}

/// Reject elements in the body of an object of type `type_display`, and
/// entries too when `typed` (a class instance has only properties).
pub(super) fn check_member_kinds(type_display: &str, entries: &[Entry], typed: bool) -> Result<()> {
    if body_has(entries, &|entry| matches!(entry, Entry::Elem(_))) {
        return Err(Error::Eval(format!(
            "Object of type `{type_display}` cannot have an element."
        )));
    }
    if typed && body_has(entries, &|entry| matches!(entry, Entry::DynProperty(..))) {
        return Err(Error::Eval(format!(
            "Object of type `{type_display}` cannot have an entry."
        )));
    }
    Ok(())
}

/// The members a generator body's receiver holds so far: those of the object
/// it is directly in, over those of any generator body enclosing that. The
/// maps are borrowed rather than merged so nested generators copy nothing.
pub(super) struct ReceiverMembers<'a> {
    own: &'a ObjectMap,
    outer: Option<&'a ReceiverMembers<'a>>,
}

impl ReceiverMembers<'_> {
    fn get(&self, name: &str) -> Option<&Value> {
        self.own
            .get(name)
            .or_else(|| self.outer.and_then(|outer| outer.get(name)))
    }
}
