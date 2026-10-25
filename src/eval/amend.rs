//! Amending objects and values: layering an amendment body over its parent.

use super::*;

impl Evaluator {
    /// Rewrite assignments of untyped `new` on an instance to amend the
    /// corresponding property of its class default object.
    pub(super) fn amend_prototype_members(
        &self,
        prototype: &Value,
        overlay_entries: &[Entry],
        amendment_scope: &mut CapturedScope,
    ) -> Option<Vec<Entry>> {
        let Value::Object(prototype_map, prototype_source) = prototype else {
            return None;
        };
        let class_default = |name: &str| {
            prototype_map
                .get(name)
                .or_else(|| {
                    let source = prototype_source.as_ref()?;
                    source
                        .evaluated_properties
                        .iter()
                        .any(|evaluated| &**evaluated == name)
                        .then(|| source.scope().get(name))
                        .flatten()
                })
                .filter(|value| matches!(value, Value::Object(..) | Value::List(_)))
        };
        let amended_default = |entry: &Entry| match entry {
            Entry::Property(prop)
                if !has_modifier(&prop.modifiers, Modifier::Local)
                    && prop
                        .value
                        .as_ref()
                        .is_some_and(parser::has_untyped_result_new) =>
            {
                class_default(&prop.name).cloned()
            }
            _ => None,
        };
        if !overlay_entries
            .iter()
            .any(|entry| amended_default(entry).is_some())
        {
            return None;
        }
        Some(
            overlay_entries
                .iter()
                .map(|entry| {
                    let (Entry::Property(prop), Some(value)) = (entry, amended_default(entry))
                    else {
                        return entry.clone();
                    };
                    let binding = format!("#parent:{}", prop.name);
                    amendment_scope
                        .values
                        .insert(binding.as_str().into(), value);
                    let mut prop = (**prop).clone();
                    if let Some(expr) = &mut prop.value {
                        parser::rewrite_untyped_result_new(expr, &mut |entries| {
                            Expr::Binop(
                                BinOp::Add,
                                Box::new(Expr::Ident(binding.clone())),
                                Box::new(Expr::ObjectBody(entries)),
                            )
                        });
                    }
                    Entry::Property(Arc::new(prop))
                })
                .collect(),
        )
    }

    pub(super) fn eval_amended_object(
        &mut self,
        base_map: &Arc<ObjectMap>,
        base_source: &Arc<ObjectSource>,
        overlay_entries: &[Entry],
        current_scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Class defaults can recursively materialize more class defaults
        // without passing through `eval_expr`. Keep that path under the same
        // depth ceiling as ordinary expression evaluation.
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        // A module object's source can carry only error metadata. It has no
        // entries to rebuild the object from, so amend the evaluated members.
        if base_source.is_metadata_only() {
            return self.eval_value_amendment(
                Value::Object(Arc::clone(base_map), Some(Arc::clone(base_source))),
                &Arc::new(overlay_entries.to_vec()),
                current_scope,
                depth,
            );
        }
        if let Some(value) = self.try_eval_simple_class_amendment(
            base_map,
            base_source,
            overlay_entries,
            current_scope,
            depth,
        )? {
            return Ok(value);
        }
        // Every amendment of a class instance goes through here: `new C {}`,
        // property bodies, and entries of typed Mappings and Listings.
        check_no_elements(base_source, overlay_entries)?;
        let base_entries = &base_source.entries;
        let base_scope = base_source.scope();
        // Build merged entry list preserving base order.
        // Overridden properties are replaced in-place so that later
        // properties that reference them see the new value.
        let mut merged: Vec<Entry> = Vec::new();
        let mut merged_entry_scopes = Vec::new();
        let mut amendment_scope = capture_scope(current_scope);
        let mut parent_members = (**base_map).clone();
        // Hidden properties are absent from the rendered map but still
        // accessible through super. Mapping keys, in contrast, need the map:
        // they need not have a binding in the lexical scope.
        for name in &base_source.evaluated_properties {
            if !parent_members.contains_key(&**name)
                && let Some(value) = base_source.scope().get(&**name)
            {
                parent_members.insert(name.clone(), value.clone());
            }
        }
        amendment_scope.values.insert(
            "super".into(),
            Value::Object(Arc::new(parent_members), Some(Arc::clone(base_source))),
        );
        let prototype = base_source.prototype.clone().or_else(|| {
            (base_source.kind == ObjectKind::Class)
                .then(|| Value::Object(Arc::clone(base_map), Some(Arc::clone(base_source))))
        });
        let rewritten_overlay = prototype.as_ref().and_then(|prototype| {
            self.amend_prototype_members(prototype, overlay_entries, &mut amendment_scope)
        });
        let overlay_entries = rewritten_overlay.as_deref().unwrap_or(overlay_entries);
        let overlay_assigned_properties = overlay_entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) if prop.value.is_some() || prop.body.is_some() => {
                    Some(prop.name.as_ref())
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        let amendment_captured = Arc::new(CapturedScope {
            body_members: overlay_entries
                .iter()
                .filter_map(entry_member_name)
                .collect(),
            ..amendment_scope
        });
        let amendment_entry_scope = Some(Arc::clone(&amendment_captured));
        let mut overlay_by_name: FxIndexMap<&str, &Entry> = FxIndexMap::default();
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry {
                if let Some(message) = base_entries
                    .iter()
                    .filter_map(|base| match base {
                        Entry::Property(inherited) if inherited.name == prop.name => {
                            Some(inherited)
                        }
                        _ => None,
                    })
                    .find_map(|inherited| immutable_member_error(prop, inherited))
                {
                    return Err(Error::Eval(message));
                }
                overlay_by_name.insert(prop.name.as_ref(), entry);
            }
        }
        let last_base_property_index = base_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                Entry::Property(prop) => Some((prop.name.as_ref(), index)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        // Walk base entries: substitute overridden properties in-place.
        // If the overlay has a body amendment (no `=`), keep the base entry first
        // so its value is in scope, then add the overlay body entry after.
        let mut used_overlay: HashSet<&str> = HashSet::default();
        for (entry_index, entry) in base_entries.iter().enumerate() {
            let inherited_entry_scope = base_source
                .entry_scopes
                .get(entry_index)
                .cloned()
                .unwrap_or(None);
            if let Entry::Property(prop) = entry
                && let Some(replacement) = overlay_by_name.get(prop.name.as_ref())
            {
                if let Entry::Property(overlay_prop) = replacement
                    && overlay_prop.body.is_some()
                    && overlay_prop.value.is_none()
                    && (prop.value.is_some() || prop.body.is_some() || prop.type_ann.is_some())
                {
                    // Body amendment: keep base entry (for its value) AND overlay
                    // entry (for body amendment). eval_property will see the base
                    // value in scope and amend it.
                    merged.push(entry.clone());
                    merged_entry_scopes.push(inherited_entry_scope.clone());
                    // A prior amendment leaves the original property and its
                    // body entry in ObjectSource. Preserve that chain, but add
                    // this amendment only once after its final entry.
                    if last_base_property_index.get(prop.name.as_ref()) != Some(&entry_index) {
                        continue;
                    }
                }
                let mut replacement = (*replacement).clone();
                if let Entry::Property(overlay_prop) = &mut replacement {
                    let overlay_prop = Arc::make_mut(overlay_prop);
                    if overlay_prop.type_ann.is_none() {
                        overlay_prop.type_ann = prop.type_ann.clone();
                    }
                    // `hidden` is declared on the class property. An overlay
                    // that assigns it (`new Step { staged = true }` or
                    // `(step) { staged = true }`) keeps it out of the output.
                    if has_modifier(&prop.modifiers, Modifier::Hidden)
                        && !has_modifier(&overlay_prop.modifiers, Modifier::Hidden)
                    {
                        overlay_prop.modifiers.push(Modifier::Hidden);
                    }
                }
                merged.push(replacement);
                merged_entry_scopes.push(amendment_entry_scope.clone());
                used_overlay.insert(prop.name.as_ref());
                continue;
            }
            merged.push(entry.clone());
            merged_entry_scopes.push(inherited_entry_scope);
        }

        // Append overlay entries that are genuinely new (not replacing a base entry)
        for entry in overlay_entries {
            if let Entry::Property(prop) = entry
                && used_overlay.contains(prop.name.as_ref())
            {
                continue; // already placed in-order above
            }
            merged.push(entry.clone());
            merged_entry_scopes.push(amendment_entry_scope.clone());
        }

        // Build scope: start with the base's captured scope, then layer current scope
        // The new scope starts empty, so the base's maps are copied whole
        // rather than rebinding each name.
        let mut eval_scope = Scope {
            type_namespace: object_source_type_namespace(base_source),
            ..Scope::default()
        };
        if !base_scope.is_empty() {
            eval_scope.vars = Arc::new(base_scope.clone());
            let base_declared = base_source.scope_declared();
            if !base_declared.is_empty() {
                eval_scope.declared = Arc::new(
                    base_declared
                        .iter()
                        .filter(|name| base_scope.contains_key(&***name))
                        .cloned()
                        .collect(),
                );
            }
        }
        for (name, identity) in base_source.scope_module_identities() {
            eval_scope.set_module_identity(name.clone(), identity.clone());
        }
        let base_type_aliases = base_source.scope_type_aliases();
        if !base_type_aliases.is_empty() {
            eval_scope.type_aliases = Arc::new(base_type_aliases.clone());
        }
        // Layer in current scope values (imports, module-level locals, etc.).
        // The same imported module can be field-pruned differently at its
        // definition and use sites. Preserve both partial views so methods
        // retain the classes captured by their definition-site import.
        // A different binding with the same name must not replace a free name
        // used by an inherited entry: those names are lexically bound where
        // the base object was defined, not where it is amended.
        let mut inherited_references = self.body_referenced_roots(base_entries);
        // An inherited annotation also reads the free names in the aliases it
        // reaches. Keep those definition-site bindings when the caller has an
        // unrelated value (or module) of the same name. The reference set also
        // guards recursive aliases while following them transitively.
        let mut pending: Vec<Name> = inherited_references
            .iter()
            .filter(|name| base_type_aliases.contains_key(name.as_ref()))
            .cloned()
            .collect();
        while let Some(name) = pending.pop() {
            if let Some(ty) = base_type_aliases.get(&*name) {
                for root in type_reference_names(ty) {
                    if Arc::make_mut(&mut inherited_references).insert(root.clone()) {
                        pending.push(root);
                    }
                }
            }
        }
        let mut preserved_inherited_bindings = HashSet::default();
        // Resolved once rather than walking each scope's chain per binding.
        let base_identities = eval_scope.flatten_module_identities();
        let current_identities = current_scope.flatten_module_identities();
        // The amendment's captured scope is `current_scope` flattened, plus
        // the `super` binding skipped here.
        for (k, v) in &amendment_captured.values {
            // Inherited entries retain their original parent. The overlay's
            // parent is captured separately in amendment_entry_scope.
            if &**k == "super" {
                continue;
            }
            let (k, v) = (k.clone(), v.clone());
            let same_module = base_identities
                .get(&*k)
                .zip(current_identities.get(&*k))
                .is_some_and(|(base, current)| base == current);
            let value = if same_module {
                // Still the base's own import, so it keeps the base's mark.
                eval_scope
                    .get(&k)
                    .and_then(|base| merge_partial_module_values(base, &v))
                    .unwrap_or(v)
            } else if inherited_references.contains(&*k) && eval_scope.get(&k).is_some() {
                preserved_inherited_bindings.insert(k.clone());
                continue;
            } else {
                // A use-site binding is not declared in the base's body.
                if eval_scope.declared.contains(&*k) {
                    Arc::make_mut(&mut eval_scope.declared).remove(&*k);
                }
                v
            };
            eval_scope.set_name(k, value);
        }
        for (name, identity) in &amendment_captured.module_identities {
            if !preserved_inherited_bindings.contains(name.as_str()) {
                eval_scope.set_module_identity(name.clone(), identity.clone());
            }
        }
        // Preserve definition-site aliases used by inherited entries. Overlay
        // entries are evaluated against `current_scope` separately below.
        for (k, ty) in &amendment_captured.type_aliases {
            if !inherited_references.contains(&**k) || eval_scope.get_type_alias(k).is_none() {
                eval_scope.set_type_alias(k.clone(), Arc::clone(ty));
            }
        }
        // Seed Null for nullable-no-default base properties absent from eval_scope.
        // This ensures `outer.optProp` resolves to Null rather than "field not found"
        // when the property was never assigned a value in the base class or any overlay.
        for entry in base_entries.iter() {
            if let Entry::Property(prop) = entry
                && prop.value.is_none()
                && prop.body.is_none()
                && !has_modifier(&prop.modifiers, Modifier::Local)
                && eval_scope.get(&prop.name).is_none()
                && matches!(prop.type_ann, Some(crate::parser::TypeExpr::Nullable(_)))
            {
                eval_scope.set(&prop.name, Value::Null);
            }
        }

        // Evaluate the merged entries (eval_entries handles locals, classes,
        // and evaluates properties in order with each added to scope)
        let merged: Body = Arc::new(merged);
        eval_scope.receiver_entries = Some(merged.clone());
        let mut result = self.eval_entries_with_lexical_scopes(
            &merged,
            &eval_scope,
            depth + 1,
            Some(&merged_entry_scopes),
            Some(base_source),
            None,
            false,
            None,
        )?;
        if let Value::Object(map, Some(source)) = result {
            let mut source = Arc::unwrap_or_clone(source);
            source.entry_scopes = merged_entry_scopes;
            result = Value::Object(map, Some(Arc::new(source)));
        }
        let assigned_property_names = merged
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) if prop.value.is_some() || prop.body.is_some() => {
                    Some(prop.name.as_ref())
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut type_failures = IndexMap::new();
        if let Value::Object(map, source) = &result {
            let mut member_scope: Option<Scope> = None;
            for entry in base_entries.iter() {
                let Entry::Property(prop) = entry else {
                    continue;
                };
                let Some(type_ann) = &prop.type_ann else {
                    continue;
                };
                let value = map.get(prop.name.as_ref()).or_else(|| {
                    assigned_property_names
                        .contains(prop.name.as_ref())
                        .then(|| {
                            source
                                .as_ref()
                                .and_then(|source| source.scope().get(prop.name.as_ref()))
                        })
                        .flatten()
                });
                let Some(value) = value else {
                    continue;
                };
                let check_scope: &Scope = if type_has_constraint(type_ann, &eval_scope) {
                    member_scope.get_or_insert_with(|| {
                        let mut members = eval_scope.child();
                        // `map` omits hidden members, while an object's saved
                        // scope keeps their final values. Both are visible to
                        // a property constraint on this instance.
                        if let Some(source) = source {
                            let object_scope = source.scope();
                            for name in &source.evaluated_properties {
                                if let Some(value) = object_scope.get(&**name) {
                                    members.set(name, value.clone());
                                }
                            }
                        }
                        for (name, value) in map.iter() {
                            members.set_name(Arc::clone(name), value.clone());
                        }
                        members
                    })
                } else {
                    &eval_scope
                };
                if type_is_runtime_checkable(type_ann, check_scope)
                    && !self.eval_type_check(value, type_ann, check_scope, depth + 1)?
                {
                    let message = format!(
                        "property '{}' expected {}, got {}",
                        prop.name,
                        display_type_expr(type_ann),
                        value_type_name(value)
                    );
                    if overlay_assigned_properties.contains(prop.name.as_ref()) {
                        return Err(Error::Eval(message));
                    }
                    type_failures.insert(
                        prop.name.clone(),
                        PoisonedMember {
                            message,
                            rendered: !has_modifier(&prop.modifiers, Modifier::Hidden),
                        },
                    );
                }
            }
        }
        if !type_failures.is_empty()
            && let Value::Object(map, Some(source)) = result
        {
            self.rendered_member_failed |= type_failures.values().any(|member| member.rendered);
            let mut source = Arc::unwrap_or_clone(source);
            let mut poisoned = source
                .poisoned_members
                .as_deref()
                .cloned()
                .unwrap_or_default();
            poisoned.extend(type_failures);
            source.poisoned_members = Some(Arc::new(poisoned));
            result = Value::Object(map, Some(Arc::new(source)));
        }
        // Amending an object preserves its class identity (so `is Foo` and
        // output converters still match). eval_entries does not know the base
        // type, so re-tag the result here.
        match (object_type_metadata(base_source), result) {
            (Some(base_type), Value::Object(map, Some(src))) => {
                let mut new_src = Arc::unwrap_or_clone(src);
                new_src.type_name = Some(base_type.name);
                new_src.type_identity = base_type.identity;
                new_src.parent_type_names = base_type.parent_names;
                new_src.parent_type_identities = base_type.parent_identities;
                new_src.prototype = prototype;
                Ok(Value::Object(map, Some(Arc::new(new_src))))
            }
            // An amended mapping is still a mapping.
            (None, Value::Object(map, Some(src)))
                if base_source.kind == ObjectKind::Mapping && src.kind != ObjectKind::Mapping =>
            {
                let mut new_src = Arc::unwrap_or_clone(src);
                new_src.kind = ObjectKind::Mapping;
                Ok(Value::Object(map, Some(Arc::new(new_src))))
            }
            (_, other) => Ok(other),
        }
    }

    /// Amend a class default without re-evaluating every unchanged member.
    ///
    /// Typed Mapping entries commonly start from the same class template. If
    /// an overlay consists exclusively of direct replacements and neither the
    /// overlay nor an unchanged base property reads a replaced member, the
    /// already-evaluated defaults are still valid. Reusing them avoids a full
    /// class reconstruction per mapping entry while preserving the source
    /// metadata needed for later amendments. Anything that could require late
    /// binding stays on the complete `eval_amended_object` path below.
    pub(super) fn try_eval_simple_class_amendment(
        &mut self,
        base_map: &Arc<ObjectMap>,
        base_source: &Arc<ObjectSource>,
        overlay_entries: &[Entry],
        current_scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        let static_default = |expr: &Expr| {
            matches!(
                expr,
                Expr::Null | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_)
            ) || matches!(
                expr,
                Expr::New(Some(name), body, _) if (name == "Mapping" || name == "Map") && body.is_empty()
            ) || matches!(
                expr,
                Expr::Call(callee, args)
                    if args.is_empty()
                        && matches!(callee.as_ref(), Expr::Ident(name)
                            if matches!(name.as_str(), "List" | "Listing" | "Set" | "Map"))
            )
        };
        let reusable_default = |prop: &Property| {
            prop.value.as_ref().is_some_and(static_default)
                || (prop.value.is_none()
                    && matches!(prop.type_ann, Some(crate::parser::TypeExpr::Nullable(_))))
        };
        if base_source.kind != ObjectKind::Class
            || base_source.is_abstract
            || overlay_entries.is_empty()
            || base_source.entries.iter().any(|entry| {
                !matches!(entry, Entry::Property(prop)
                        if prop.modifiers.is_empty()
                            && prop.body.is_none()
                            && reusable_default(prop))
            })
        {
            return Ok(None);
        }

        // Property annotations belong to the definition site of the class.
        // An imported class's aliases and sibling classes may not be visible
        // in the scope that amends one of its instances.
        let mut type_scope = Scope {
            type_namespace: object_source_type_namespace(base_source),
            ..Scope::default()
        };
        let base_scope = base_source.scope();
        if !base_scope.is_empty() {
            type_scope.vars = Arc::new(base_scope.clone());
            let declared = base_source.scope_declared();
            if !declared.is_empty() {
                type_scope.declared = Arc::new(
                    declared
                        .iter()
                        .filter(|name| base_scope.contains_key(&***name))
                        .cloned()
                        .collect(),
                );
            }
        }
        for (name, identity) in base_source.scope_module_identities() {
            type_scope.set_module_identity(name.clone(), identity.clone());
        }
        let aliases = base_source.scope_type_aliases();
        if !aliases.is_empty() {
            type_scope.type_aliases = Arc::new(aliases.clone());
        }

        let base_properties = base_source
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                Entry::Property(prop) if !has_modifier(&prop.modifiers, Modifier::Local) => {
                    Some((prop.name.as_ref(), (index, Arc::clone(prop))))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut overlays = IndexMap::new();
        for entry in overlay_entries {
            let Entry::Property(prop) = entry else {
                return Ok(None);
            };
            if prop.is_method
                || !prop.modifiers.is_empty()
                || prop.type_ann.is_some()
                || prop.value.is_none()
                || prop.body.is_some()
                || overlays.contains_key(prop.name.as_ref())
            {
                return Ok(None);
            }
            let Some((_, base_prop)) = base_properties.get(prop.name.as_ref()) else {
                return Ok(None);
            };
            if let Some(message) = immutable_member_error(prop, base_prop) {
                return Err(Error::Eval(message));
            }
            overlays.insert(prop.name.as_ref(), Arc::clone(prop));
        }
        if overlays.is_empty()
            || base_source.entries.iter().any(|entry| match entry {
                Entry::Property(prop) => {
                    prop.type_ann
                        .as_ref()
                        .is_some_and(|ty| type_has_constraint(ty, &type_scope))
                        || (!overlays.contains_key(prop.name.as_ref())
                            && property_reference_names(prop).iter().any(|name| {
                                &**name == DYNAMIC_SIBLING_REF
                                    || overlays.contains_key(name.as_ref())
                            }))
                }
                _ => true,
            })
            || overlay_entries.iter().any(|entry| {
                property_reference_names(match entry {
                    Entry::Property(prop) => prop,
                    _ => unreachable!("overlay entries were checked above"),
                })
                .iter()
                .any(|name| {
                    &**name == "this"
                        || &**name == "super"
                        || &**name == "outer"
                        || &**name == DYNAMIC_SIBLING_REF
                        || base_properties.contains_key(name.as_ref())
                })
            })
        {
            return Ok(None);
        }

        let overlay_scope = Arc::new(capture_scope(current_scope));
        let mut map = (**base_map).clone();
        let mut source = (**base_source).clone();
        let entries = Arc::make_mut(&mut source.entries);
        let entry_scopes = &mut source.entry_scopes;
        entry_scopes.resize(entries.len(), None);
        for (name, overlay) in overlays {
            let (index, base_prop) = base_properties
                .get(name)
                .expect("eligible overlay has a base property");
            let mut prop = (*overlay).clone();
            prop.type_ann = base_prop.type_ann.clone();
            if has_modifier(&base_prop.modifiers, Modifier::Hidden) {
                prop.modifiers.push(Modifier::Hidden);
            }
            let value = self
                .eval_property(&prop, current_scope, depth)?
                .expect("valued overlay properties evaluate to a value");
            self.check_declared_property_type(&prop, &value, &type_scope, depth)?;
            map.insert(prop.name.clone(), value);
            entries[*index] = Entry::Property(Arc::new(prop));
            entry_scopes[*index] = Some(Arc::clone(&overlay_scope));
        }
        source.kind = ObjectKind::Object;
        source.prototype = base_source.prototype.clone().or_else(|| {
            Some(Value::Object(
                Arc::clone(base_map),
                Some(Arc::clone(base_source)),
            ))
        });
        Ok(Some(Value::Object(Arc::new(map), Some(Arc::new(source)))))
    }

    pub(super) fn eval_object_body_over_template(
        &mut self,
        template_map: &Arc<ObjectMap>,
        template_src: &Arc<ObjectSource>,
        explicit_map: &ObjectMap,
        body: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // This template combines already-evaluated type and explicit defaults.
        // Its original source describes only the type default, so retain the
        // explicit values as source bindings before applying the entry body.
        // Keep untouched class expressions for late binding to entry overrides.
        let mut source = (**template_src).clone();
        for key in explicit_map.keys() {
            let Some(value) = template_map.get(key) else {
                continue;
            };
            let binding = format!("\0mapping_template:{key}");
            source
                .captured
                .parts_mut()
                .values
                .insert(binding.as_str().into(), value.clone());
            let mut replaced = false;
            for (index, entry) in Arc::make_mut(&mut source.entries).iter_mut().enumerate() {
                if let Entry::Property(prop) = entry
                    && *prop.name == **key
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                {
                    let prop = Arc::make_mut(prop);
                    prop.value = Some(Expr::Ident(binding.clone()));
                    prop.body = None;
                    if let Some(entry_scope) = source.entry_scopes.get_mut(index) {
                        *entry_scope = None;
                    }
                    replaced = true;
                }
            }
            if !replaced {
                Arc::make_mut(&mut source.entries).push(Entry::Property(Arc::new(Property {
                    annotations: Vec::new(),
                    modifiers: Vec::new(),
                    name: key.clone(),
                    type_ann: None,
                    value: Some(Expr::Ident(binding)),
                    body: None,
                    is_method: false,
                })));
                source.entry_scopes.resize(source.entries.len(), None);
            }
            source.body_members.insert(nm(key));
        }
        self.eval_amended_object(template_map, &Arc::new(source), body, scope, depth)
    }

    pub(super) fn eval_value_amendment(
        &mut self,
        base: Value,
        overlay_entries: &Body,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Amending an abstract module's object instantiates it.
        if let Value::Object(_, Some(source)) = &base
            && source.is_abstract
            && source.type_name.is_none()
        {
            return Err(Error::Eval(format!(
                "Cannot instantiate abstract class `{}`.",
                source.type_identity.as_deref().unwrap_or_default()
            )));
        }
        if let Value::Lambda(params, ..) = &base {
            return Ok(amended_function(
                params.len(),
                base.clone(),
                overlay_entries,
                scope,
            ));
        }
        if let Value::Null | Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::String(_) =
            base
        {
            return Err(Error::Eval(format!(
                "Cannot instantiate, or amend an instance of, external class `{}`.",
                value_type_name(&base)
            )));
        }
        if let Value::Object(base_map, Some(base_src)) = &base
            && base_src.kind == ObjectKind::Mapping
        {
            check_no_elements(base_src, overlay_entries)?;
            let needs_mapping_evaluation = body_has(overlay_entries, &|entry| {
                matches!(entry, Entry::Spread(_))
                    || matches!(entry, Entry::Property(prop) if &*prop.name == "default")
            });
            if !base_src.is_metadata_only() || !needs_mapping_evaluation {
                // Unannotated mapping entries with bodies still use the general
                // amendment path, which preserves Listing-shaped entry bodies.
            } else {
                let (inherited_scope, mut amendment_scope) =
                    mapping_amendment_scopes(base_src.scope(), base_src.scope_declared(), scope);
                amendment_scope.set("super", base.clone());
                let mut receiver_entries = base_map
                    .keys()
                    .map(|key| Entry::DynProperty(Expr::String(Arc::clone(key)), Expr::Null))
                    .collect::<Vec<_>>();
                receiver_entries.extend_from_slice(overlay_entries);
                amendment_scope.receiver_entries = Some(Arc::new(receiver_entries));
                let value_type_defaults = base_src
                    .mapping_value_types
                    .iter()
                    .filter_map(|name| {
                        let name = mapping_value_type_base(name);
                        resolve_dotted(&inherited_scope, name)
                            .map(|value| (name.to_string(), value))
                    })
                    .collect::<Vec<_>>();
                let inherited_default =
                    self.find_default_template(&base_src.entries, &inherited_scope, depth)?;
                let mut amended = ObjectMap::default();
                self.eval_mapping_entries_with_type_default(
                    &base_src.entries,
                    &inherited_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    &base_src.mapping_value_types,
                    MappingInheritedDefault::default(),
                )?;
                amended.extend(
                    base_map
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                self.eval_mapping_entries_with_type_default(
                    overlay_entries,
                    &amendment_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    &base_src.mapping_value_types,
                    MappingInheritedDefault {
                        value: inherited_default,
                        entries: find_default_body_entries(&base_src.entries),
                    },
                )?;
                // A metadata-only Mapping has no entries of its own, so this
                // specialized first amendment must retain the overlay as its
                // future source. Otherwise a later ordinary amendment loses
                // an introduced `default` (and any captured bindings it uses).
                let mut source = Arc::unwrap_or_clone(Arc::clone(base_src));
                source.entries = base_src
                    .entries
                    .iter()
                    .cloned()
                    .chain(overlay_entries.iter().cloned())
                    .collect::<Vec<_>>()
                    .into();
                source.captured = SourceScope::lazy(scope, Vec::new(), Vec::new());
                return Ok(Value::Object(Arc::new(amended), Some(Arc::new(source))));
            }
        }
        // Values of external classes have no members to amend.
        let external_class = match &base {
            Value::List(items) if items.kind() == ListKind::List => Some("List"),
            Value::List(items) if items.kind() == ListKind::Set => Some("Set"),
            _ => None,
        };
        if let Some(class) = external_class {
            return Err(Error::Eval(format!(
                "Cannot instantiate, or amend an instance of, external class `{class}`."
            )));
        }
        if let Value::List(existing) = base {
            if overlay_entries.iter().any(|entry| {
                matches!(entry, Entry::Property(prop)
                    if &*prop.name != "default" && !has_modifier(&prop.modifiers, Modifier::Local))
            }) {
                return Err(Error::Eval(
                    "Object of type `Listing` cannot have a property.".into(),
                ));
            }
            let mut amended = existing;
            let mut amendment_scope = scope.child();
            amendment_scope.set("super", Value::List(amended.clone()));
            amendment_scope.receiver_entries = Some(overlay_entries.clone());
            amendment_scope.receiver_list_base = Some(amended.len());
            self.eval_listing_entries(
                overlay_entries,
                &amendment_scope,
                depth + 1,
                amended.make_mut(),
            )?;
            return Ok(Value::List(amended));
        }
        if let Value::Object(base_map, Some(base_src)) = &base
            && !base_src.is_metadata_only()
            && !base_src.is_parsed_json
        {
            check_no_elements(base_src, overlay_entries)?;
            let has_mapping_default = base_src
                .entries
                .iter()
                .any(|entry| matches!(entry, Entry::Property(prop) if &*prop.name == "default"));
            if !base_src.mapping_value_types.is_empty()
                || (base_src.kind == ObjectKind::Mapping && has_mapping_default)
            {
                let (inherited_scope, mut amendment_scope) =
                    mapping_amendment_scopes(base_src.scope(), base_src.scope_declared(), scope);
                amendment_scope.set("super", base.clone());
                let mut receiver_entries = base_map
                    .keys()
                    .map(|key| Entry::DynProperty(Expr::String(Arc::clone(key)), Expr::Null))
                    .collect::<Vec<_>>();
                receiver_entries.extend_from_slice(overlay_entries);
                amendment_scope.receiver_entries = Some(Arc::new(receiver_entries));
                let value_type_defaults = base_src
                    .mapping_value_types
                    .iter()
                    .filter_map(|name| {
                        let name = mapping_value_type_base(name);
                        resolve_dotted(&inherited_scope, name)
                            .map(|value| (name.to_string(), value))
                    })
                    .collect::<Vec<_>>();
                let inherited_default =
                    self.find_default_template(&base_src.entries, &inherited_scope, depth)?;
                let mut amended = ObjectMap::default();
                self.eval_mapping_entries_with_type_default(
                    &base_src.entries,
                    &inherited_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    &base_src.mapping_value_types,
                    MappingInheritedDefault::default(),
                )?;
                if let Value::Object(existing_map, _) = &base {
                    amended.extend(existing_map.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                let applied_default = self.rebase_entries_on_amended_default(
                    base_src,
                    overlay_entries,
                    &inherited_scope,
                    &amendment_scope,
                    &value_type_defaults,
                    inherited_default.clone(),
                    depth,
                    &mut amended,
                )?;
                self.eval_mapping_entries_with_type_default(
                    overlay_entries,
                    &amendment_scope,
                    depth,
                    &mut amended,
                    &value_type_defaults,
                    &base_src.mapping_value_types,
                    MappingInheritedDefault {
                        value: inherited_default,
                        entries: find_default_body_entries(&base_src.entries),
                    },
                )?;
                // The mapping's default is now the one just applied; a later
                // amendment compares its entries against it.
                let source = match applied_default {
                    Some(default) => {
                        let replaced: HashSet<&str> = overlay_entries
                            .iter()
                            .filter_map(|entry| match entry {
                                Entry::DynProperty(Expr::String(key), value)
                                    if !matches!(value, Expr::ObjectBody(_)) =>
                                {
                                    Some(&**key)
                                }
                                _ => None,
                            })
                            .collect();
                        // A replaced entry is saved as its evaluated value, so
                        // later replays neither rebuild the discarded body nor
                        // let other entries read a placeholder. Other values are
                        // blanked only if no entry reads its siblings.
                        let reads_siblings = {
                            let refs = property_reference_names(&Property {
                                annotations: Vec::new(),
                                modifiers: Vec::new(),
                                name: "".into(),
                                type_ann: None,
                                value: None,
                                body: Some(base_src.entries.clone()),
                                is_method: false,
                            });
                            ["this", "module", DYNAMIC_SIBLING_REF]
                                .iter()
                                .any(|name| refs.contains(*name))
                        };
                        let mut replayable = true;
                        let entries: Vec<Entry> = base_src
                            .entries
                            .iter()
                            .map(|entry| match entry {
                                Entry::Property(prop) if &*prop.name == "default" => {
                                    Entry::Property(default.clone())
                                }
                                Entry::DynProperty(key @ Expr::String(name), _)
                                    if replaced.contains(&**name) =>
                                {
                                    let literal = match amended.get(&**name) {
                                        Some(Value::Null) => Some(Expr::Null),
                                        Some(Value::Bool(b)) => Some(Expr::Bool(*b)),
                                        Some(Value::Int(n)) => Some(Expr::Int(*n)),
                                        Some(Value::Float(f)) => Some(Expr::Float(*f)),
                                        Some(Value::String(text)) => {
                                            Some(Expr::String(Arc::clone(text)))
                                        }
                                        _ => None,
                                    };
                                    replayable &= literal.is_some() || !reads_siblings;
                                    Entry::DynProperty(key.clone(), literal.unwrap_or(Expr::Null))
                                }
                                entry => entry.clone(),
                            })
                            .collect();
                        if replayable {
                            let mut source = Arc::unwrap_or_clone(Arc::clone(base_src));
                            source.entries = entries.into();
                            Arc::new(source)
                        } else {
                            Arc::clone(base_src)
                        }
                    }
                    None => Arc::clone(base_src),
                };
                return Ok(Value::Object(Arc::new(amended), Some(source)));
            }
            return self.eval_amended_object(base_map, base_src, overlay_entries, scope, depth);
        }
        let mut amendment_scope = scope.child();
        if let Value::Object(existing, _) = &base {
            for (name, value) in existing.iter() {
                amendment_scope.set(name, value.clone());
            }
            amendment_scope.set("super", base.clone());
        }
        let overlay = self.eval_entries(overlay_entries, &amendment_scope, depth + 1)?;
        Ok(merge_values(base, overlay))
    }

    /// An amendment that assigns a new `default` also applies it to the
    /// entries the base declared without a value of their own: pkl builds
    /// those entries from the final default. This runs before the amendment's
    /// own entries, so one that amends a base entry sees the rebuilt value.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn rebase_entries_on_amended_default(
        &mut self,
        base_src: &ObjectSource,
        overlay_entries: &Body,
        inherited_scope: &Scope,
        amendment_scope: &Scope,
        value_type_defaults: &[(String, Value)],
        old_default: Option<Value>,
        depth: usize,
        amended: &mut ObjectMap,
    ) -> Result<Option<Arc<Property>>> {
        let assigned_default = overlay_entries.iter().find_map(|entry| match entry {
            Entry::Property(prop)
                if &*prop.name == "default"
                    && prop.value.is_some()
                    && !has_modifier(&prop.modifiers, Modifier::Local) =>
            {
                Some(prop.clone())
            }
            _ => None,
        });
        // Only a base made of a `default` and literal-key entries is rebuilt:
        // locals, generators and computed keys are left as they were.
        let plain_base = base_src.entries.iter().all(|entry| match entry {
            Entry::Property(prop) => {
                &*prop.name == "default" && !has_modifier(&prop.modifiers, Modifier::Local)
            }
            Entry::DynProperty(key, _) => matches!(key, Expr::String(_)),
            _ => false,
        });
        let Some(assigned_default) = assigned_default else {
            return Ok(None);
        };
        if !plain_base {
            return Ok(None);
        }
        let Some(default) = self.find_default_template(overlay_entries, amendment_scope, depth)?
        else {
            return Ok(None);
        };
        let recorded = default.clone();
        // Entries the amendment replaces outright are never built from the base.
        let replaced: HashSet<&str> = overlay_entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::DynProperty(Expr::String(key), value)
                    if !matches!(value, Expr::ObjectBody(_)) =>
                {
                    Some(&**key)
                }
                _ => None,
            })
            .collect();
        let rebased: Vec<Entry> = base_src
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    Entry::DynProperty(Expr::String(key), Expr::ObjectBody(_) | Expr::New(None, ..))
                        if !replaced.contains(&**key)
                )
            })
            .cloned()
            .collect();
        if !rebased.is_empty() {
            // Build them apart from `amended`, whose entries already hold the old
            // default: a body entry would amend that value instead of the default.
            let mut fresh = ObjectMap::default();
            self.eval_mapping_entries_with_type_default(
                &rebased,
                inherited_scope,
                depth,
                &mut fresh,
                value_type_defaults,
                &base_src.mapping_value_types,
                MappingInheritedDefault {
                    value: Some(default),
                    entries: None,
                },
            )?;
            // An entry an earlier amendment changed is no longer what the base
            // built from its own default; keep it.
            let mut original = ObjectMap::default();
            self.eval_mapping_entries_with_type_default(
                &rebased,
                inherited_scope,
                depth,
                &mut original,
                value_type_defaults,
                &base_src.mapping_value_types,
                MappingInheritedDefault {
                    value: old_default,
                    entries: None,
                },
            )?;
            for (key, value) in fresh {
                let unchanged = match (original.get(&key), amended.get(&key)) {
                    (Some(original), Some(current)) => original.to_json() == current.to_json(),
                    _ => false,
                };
                if unchanged {
                    amended.insert(key, value);
                }
            }
        }
        // The default can only be recorded on the mapping if it also means
        // the same thing where the mapping was declared.
        let declared = self
            .find_default_template(overlay_entries, inherited_scope, depth)
            .ok()
            .flatten();
        let same = declared.is_some_and(|declared| declared.to_json() == recorded.to_json());
        Ok(same.then_some(assigned_default))
    }
}

/// The function `(function) { body }` evaluates to: one that calls `function`
/// with its `arity` arguments and amends the result with `body`, evaluated
/// where the amendment is written.
/// Add a property declared in a base module's source to the inherited late
/// properties. A body-only amendment is skipped when the base's snapshot
/// already holds the property: the snapshot has that body composed onto its
/// base expression, and applying the raw body again would pin the object to
/// the value it had when the base was evaluated.
pub(super) fn push_base_property(inherited: &mut Vec<Arc<Property>>, prop: &Arc<Property>) {
    if prop.value.is_none()
        && prop.body.is_some()
        && inherited.iter().any(|known| known.name == prop.name)
    {
        return;
    }
    inherited.push(prop.clone());
}

/// Whether `expr` builds a fresh object, so that amending it with a body is
/// the same as evaluating the body over that object again.
pub(super) fn builds_amendable_object(expr: &Expr) -> bool {
    match expr {
        Expr::New(..) | Expr::InferredNew(..) => true,
        Expr::TemplateLocals(_, inner) => builds_amendable_object(inner),
        Expr::Binop(BinOp::Add, left, right) => {
            overlay_body(right).is_some() && builds_amendable_object(left)
        }
        _ => false,
    }
}

/// The body of an amendment's right operand, and the template locals it was
/// written under, if any.
pub(super) fn overlay_body(expr: &Expr) -> Option<(&Body, Option<&LocalBindings>)> {
    match expr {
        Expr::ObjectBody(body) => Some((body, None)),
        Expr::TemplateLocals(locals, inner) => match inner.as_ref() {
            Expr::ObjectBody(body) => Some((body, Some(locals))),
            _ => None,
        },
        _ => None,
    }
}

/// The `local` members of a module, as the bindings its own expressions need
/// to keep reading when an amending module declares a local of the same name.
pub(super) fn module_local_bindings(body: &[Entry], scope: &Scope) -> Option<LocalBindings> {
    let locals: Vec<(Name, Value)> = body
        .iter()
        .filter_map(|entry| match entry {
            Entry::Property(prop)
                if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
            {
                Some((prop.name.clone(), scope.get(&prop.name)?.clone()))
            }
            _ => None,
        })
        .collect();
    (!locals.is_empty()).then(|| LocalBindings(Arc::new(locals)))
}

/// Wrap `expr` so it sees `locals` when `prop` reads one of them.
pub(super) fn with_template_locals(
    locals: Option<&LocalBindings>,
    prop: &Property,
    expr: Expr,
) -> Expr {
    match locals {
        Some(locals)
            if {
                let refs = property_reference_names(prop);
                locals.0.iter().any(|(name, _)| refs.contains(name))
            } =>
        {
            Expr::TemplateLocals(locals.clone(), Box::new(expr))
        }
        _ => expr,
    }
}

/// Replace the base members that `body` overrides with `null`, so building the
/// base never evaluates a value the amendment is about to replace. The member
/// keeps its position. Nothing is replaced when another base member could read
/// the overridden one.
pub(super) fn without_overridden_members(base: &Expr, body: &Body) -> Expr {
    let mut properties: HashSet<&str> = HashSet::default();
    let mut keys: HashSet<&str> = HashSet::default();
    let reads_super = |value: &Expr| {
        let mut refs = HashSet::default();
        collect_expr_refs(value, &mut refs, &HashSet::default());
        refs.contains("super")
    };
    for entry in body.iter() {
        match entry {
            Entry::Property(prop)
                if !has_modifier(&prop.modifiers, Modifier::Local)
                    && prop.type_ann.is_none()
                    && prop.body.is_none()
                    && prop.value.as_ref().is_some_and(|value| !reads_super(value)) =>
            {
                properties.insert(&prop.name);
            }
            Entry::DynProperty(Expr::String(key), value)
                if !matches!(value, Expr::ObjectBody(_)) && !reads_super(value) =>
            {
                keys.insert(key);
            }
            _ => {}
        }
    }
    if properties.is_empty() && keys.is_empty() {
        return base.clone();
    }
    let mut layers = Vec::new();
    if !collect_prunable_layers(base, &mut layers) {
        return base.clone();
    }
    // A Mapping's `default` still shapes the entries the base declares.
    if builds_mapping(base) {
        properties.remove("default");
    }
    let overridden = |entry: &Entry| match entry {
        Entry::Property(prop) => {
            !has_modifier(&prop.modifiers, Modifier::Local)
                && prop.type_ann.is_none()
                && prop.body.is_none()
                && prop.value.is_some()
                && properties.contains(&*prop.name)
        }
        Entry::DynProperty(Expr::String(key), value) => {
            !matches!(value, Expr::ObjectBody(_)) && keys.contains(&**key)
        }
        _ => false,
    };
    let mut refs = HashSet::default();
    for layer in &layers {
        let kept: Vec<Entry> = layer
            .iter()
            .filter(|entry| !overridden(entry))
            .cloned()
            .collect();
        refs.extend(property_reference_names(&Property {
            annotations: Vec::new(),
            modifiers: Vec::new(),
            name: "".into(),
            type_ann: None,
            value: None,
            body: Some(Arc::new(kept)),
            is_method: false,
        }));
    }
    if ["this", "super", "module", DYNAMIC_SIBLING_REF]
        .iter()
        .any(|name| refs.contains(*name))
        || properties.iter().any(|name| refs.contains(*name))
    {
        return base.clone();
    }
    let blank = |body: &Body| -> Body {
        Arc::new(
            body.iter()
                .map(|entry| match entry {
                    Entry::Property(prop) if overridden(entry) => {
                        Entry::Property(Arc::new(Property {
                            value: Some(Expr::Null),
                            ..(**prop).clone()
                        }))
                    }
                    Entry::DynProperty(key, _) if overridden(entry) => {
                        Entry::DynProperty(key.clone(), Expr::Null)
                    }
                    entry => entry.clone(),
                })
                .collect(),
        )
    };
    rebuild_layers(base, &blank)
}

pub(super) fn builds_mapping(expr: &Expr) -> bool {
    match expr {
        Expr::New(type_name, ..) => type_name.as_deref() == Some("Mapping"),
        Expr::TemplateLocals(_, inner) => builds_mapping(inner),
        Expr::Binop(BinOp::Add, left, _) => builds_mapping(left),
        _ => false,
    }
}

/// The bodies of the object `expr` builds and of the amendments layered on it,
/// when the object is plain enough to blank out a member of: untyped or a
/// Dynamic, Mapping or Listing, with no generic arguments to check.
pub(super) fn collect_prunable_layers<'a>(expr: &'a Expr, layers: &mut Vec<&'a Body>) -> bool {
    match expr {
        Expr::New(type_name, body, generics) => {
            layers.push(body);
            generics.is_empty()
                && matches!(
                    type_name.as_deref(),
                    None | Some("Dynamic" | "Mapping" | "Listing")
                )
        }
        Expr::TemplateLocals(_, inner) => collect_prunable_layers(inner, layers),
        Expr::Binop(BinOp::Add, left, right) => match overlay_body(right) {
            Some((body, _)) => {
                layers.push(body);
                collect_prunable_layers(left, layers)
            }
            None => false,
        },
        _ => false,
    }
}

pub(super) fn rebuild_layers(expr: &Expr, blank: &dyn Fn(&Body) -> Body) -> Expr {
    match expr {
        Expr::New(type_name, body, generics) => {
            Expr::New(type_name.clone(), blank(body), generics.clone())
        }
        Expr::TemplateLocals(locals, inner) => {
            Expr::TemplateLocals(locals.clone(), Box::new(rebuild_layers(inner, blank)))
        }
        Expr::Binop(BinOp::Add, left, right) => {
            let right = match right.as_ref() {
                Expr::ObjectBody(body) => Expr::ObjectBody(blank(body)),
                Expr::TemplateLocals(locals, inner) => match inner.as_ref() {
                    Expr::ObjectBody(body) => Expr::TemplateLocals(
                        locals.clone(),
                        Box::new(Expr::ObjectBody(blank(body))),
                    ),
                    _ => right.as_ref().clone(),
                },
                _ => right.as_ref().clone(),
            };
            Expr::Binop(
                BinOp::Add,
                Box::new(rebuild_layers(left, blank)),
                Box::new(right),
            )
        }
        expr => expr.clone(),
    }
}

/// The late-bound form of `prop` once it amends the inherited `inherited`.
///
/// `foo { ... }` amends the value its base computed. A module further down
/// the chain that overrides something `foo` reads must see that override, so
/// the amendment keeps the base expression and layers its body on top of it:
/// evaluating the result later rebuilds the object in the final scope.
///
/// `locals` are the `local` members of the module declaring `prop`, which its
/// body keeps reading even where an amending module declares the same names.
pub(super) fn compose_amended_object(
    inherited: &Arc<Property>,
    prop: &Arc<Property>,
    locals: Option<&LocalBindings>,
) -> Arc<Property> {
    let (Some(base), Some(body), None) = (&inherited.value, &prop.body, &prop.value) else {
        return prop.clone();
    };
    if !builds_amendable_object(base) {
        return prop.clone();
    }
    let type_ann = prop.type_ann.clone().or_else(|| inherited.type_ann.clone());
    let base = if type_ann.is_none() {
        without_overridden_members(base, body)
    } else {
        base.clone()
    };
    let overlay = with_template_locals(locals, prop, Expr::ObjectBody(body.clone()));
    Arc::new(Property {
        type_ann,
        value: Some(Expr::Binop(BinOp::Add, Box::new(base), Box::new(overlay))),
        body: None,
        ..(**prop).clone()
    })
}

pub(super) fn amended_function(arity: usize, function: Value, body: &Body, scope: &Scope) -> Value {
    const FUNCTION: &str = "\0pklr:amended-function";
    let params: Vec<String> = (0..arity).map(|i| format!("\0pklr:arg{i}")).collect();
    let call = Expr::Call(
        Box::new(Expr::Ident(FUNCTION.into())),
        params.iter().cloned().map(Expr::Ident).collect(),
    );
    let mut captured = scope.flatten();
    captured.insert(FUNCTION.into(), function);
    Value::Lambda(
        params.into(),
        Arc::new(Expr::Binop(
            BinOp::Add,
            Box::new(call),
            Box::new(Expr::ObjectBody(body.clone())),
        )),
        Arc::new(captured),
    )
}
