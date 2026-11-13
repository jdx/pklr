//! Class and type alias definitions, and the checks on declaring and
//! instantiating them.

use super::*;

impl Evaluator {
    /// Evaluate a class definition, optionally inheriting from a parent class.
    ///
    /// If `parent_name` is provided, the parent class is looked up in scope,
    /// its defaults are used as a base, and `super` is bound to the parent
    /// value so the child class body can reference it.
    pub(super) fn eval_class_def(
        &mut self,
        class_name: &str,
        class_mods: &[Modifier],
        parent_name: Option<&str>,
        body: &Body,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let parent_val = parent_name.and_then(|name| resolve_dotted(scope, name));
        if let Some(parent_name) = parent_name {
            check_supertype(scope, class_name, parent_name, parent_val.as_ref())?;
        }
        if let Some(Value::Object(_, Some(parent))) = &parent_val {
            check_override_modifiers(body, parent)?;
        }
        // A parent class that failed to evaluate fails its subclasses too,
        // rather than leaving them without the inherited members.
        if parent_val.is_none()
            && let Some(message) =
                parent_name.and_then(|name| poisoned_member(scope, name.trim_end_matches('?')))
        {
            return Err(Error::Eval(message));
        }
        let (parent_type_names, parent_type_identities) = match &parent_val {
            Some(Value::Object(_, Some(source))) => {
                let names = source
                    .type_name
                    .iter()
                    .cloned()
                    .chain(source.parent_type_names.iter().cloned())
                    .collect();
                let identities = source
                    .type_identity
                    .iter()
                    .cloned()
                    .chain(source.parent_type_identities.iter().cloned())
                    .collect();
                (names, identities)
            }
            // `extends module` inherits the module's declarations, which the
            // snapshot bound to `module` does not carry; the marker lets
            // `new` accept them.
            _ if parent_name == Some("module") => (vec!["module".to_string()], Vec::new()),
            _ => (Vec::new(), Vec::new()),
        };

        // Class defaults are built before their parent source is merged into
        // the child body. Keep its actual non-const members available for
        // local validation without eagerly rejecting a lazy local lambda.
        let mut inherited_non_const: HashSet<Name> = HashSet::default();
        if let Some(Value::Object(_, Some(parent_source))) = &parent_val {
            for name in &parent_source.evaluated_properties {
                if body_member_is_non_const(&parent_source.entries, name) {
                    inherited_non_const.insert(nm(name));
                }
            }
        }
        let mut child_scope = scope.child();
        if let Some(ref pv) = parent_val {
            child_scope.set("super", pv.clone());
        }

        // A property the child redefines without restating its type keeps the
        // parent's declared type. Its default is resolved in that parent's
        // definition scope, which matters for imported parent classes.
        let typed_body = match &parent_val {
            Some(Value::Object(_, Some(parent))) => {
                self.inherit_property_types(body, parent, &mut child_scope, depth)?
            }
            _ => None,
        };
        let body = typed_body.as_ref().unwrap_or(body);
        // A `local` member never overrides: the parent's member of that name
        // stays, and the parent's expressions keep reading it.
        let body_reads: HashSet<Name> = body
            .iter()
            .flat_map(|entry| referenced_roots(std::slice::from_ref(entry)))
            .collect();
        let without_local_overrides: Option<Body> = match &parent_val {
            Some(Value::Object(_, Some(parent))) => {
                let overrides_nothing_local = |entry: &Entry| match entry {
                    Entry::Property(prop)
                        if has_modifier(&prop.modifiers, Modifier::Local)
                            && !body_reads.contains(&prop.name) =>
                    {
                        parent.entries.iter().any(|inherited| {
                            matches!(inherited, Entry::Property(p)
                                if p.name == prop.name
                                    && !has_modifier(&p.modifiers, Modifier::Local))
                        })
                    }
                    _ => false,
                };
                body.iter().any(overrides_nothing_local).then(|| {
                    body.iter()
                        .filter(|e| !overrides_nothing_local(e))
                        .cloned()
                        .collect::<Vec<_>>()
                        .into()
                })
            }
            _ => None,
        };
        let body = without_local_overrides.as_ref().unwrap_or(body);

        let mut child_receiver_scope = child_scope.clone();
        child_receiver_scope.receiver_entries = Some(body.clone());
        child_receiver_scope.receiver_list_base = None;
        let parent_source = match &parent_val {
            Some(Value::Object(_, Some(parent))) => Some(Arc::clone(parent)),
            _ => None,
        };
        let child_defaults = self.eval_entries_with_lexical_scopes(
            body,
            &child_receiver_scope,
            depth + 1,
            None,
            parent_source.as_deref(),
            Some(&inherited_non_const),
            false,
            None,
        )?;
        if let Some(Value::Object(parent_map, parent_src)) = parent_val {
            // Merge: parent defaults first, child overrides on top
            let mut merged: ObjectMap = (*parent_map).clone();
            if let Value::Object(child_map, child_src) = child_defaults {
                for (k, v) in child_map.iter() {
                    merged.insert(k.clone(), v.clone());
                }
                // Preserve the child's ObjectSource for late binding,
                // but prepend parent entries so inherited props are available
                let source = if let Some(child_arc) = child_src {
                    let mut src = (*child_arc).clone();
                    // Collect child property names (including dynamic string-key entries)
                    let child_names: std::collections::HashSet<Name> = body
                        .iter()
                        .filter_map(|e| match e {
                            Entry::Property(p) => Some(p.name.clone()),
                            Entry::DynProperty(Expr::String(s), _) => Some(nm(s)),
                            _ => None,
                        })
                        .collect();
                    if let Some(psrc) = parent_src {
                        let mut combined_entries = Vec::new();
                        let mut combined_entry_scopes = Vec::new();
                        let mut combined_evaluated_properties = Vec::new();
                        let mut inherited_property_values = IndexMap::new();
                        // One snapshot for every entry of the parent's body, so
                        // they resolve each other as members of the same body.
                        let parent_scope = Arc::new({
                            let mut captured = capture_object_source_scope(&psrc);
                            captured.body_members.extend(
                                psrc.entries
                                    .iter()
                                    .enumerate()
                                    .filter(|(index, _)| {
                                        psrc.entry_scopes.get(*index).is_none_or(Option::is_none)
                                    })
                                    .filter_map(|(_, entry)| entry_member_name(entry)),
                            );
                            captured
                        });
                        // A parent's `local` member is not overridden: the
                        // parent's own expressions keep reading it.
                        let shadowed: Vec<(Name, Value)> = psrc
                            .entries
                            .iter()
                            .filter_map(|entry| match entry {
                                Entry::Property(p)
                                    if has_modifier(&p.modifiers, Modifier::Local)
                                        && child_names.contains(&p.name) =>
                                {
                                    Some((p.name.clone(), psrc.scope().get(&p.name)?.clone()))
                                }
                                _ => None,
                            })
                            .collect();
                        let child_entries = std::mem::take(&mut src.entries);
                        let child_entry_scopes = std::mem::take(&mut src.entry_scopes);
                        // An annotation on a property the parent declares also
                        // applies to the child's override, ahead of its own.
                        let inherited_annotations: Vec<(Name, Vec<crate::parser::Annotation>)> =
                            psrc.entries
                                .iter()
                                .filter_map(|entry| match entry {
                                    Entry::Property(p)
                                        if !p.annotations.is_empty()
                                            && child_names.contains(&p.name) =>
                                    {
                                        Some((p.name.clone(), p.annotations.clone()))
                                    }
                                    _ => None,
                                })
                                .collect();
                        let mut prepared_child: Vec<(Entry, Option<Arc<CapturedScope>>)> =
                            Vec::new();
                        for (entry_index, entry) in
                            Arc::unwrap_or_clone(child_entries).into_iter().enumerate()
                        {
                            let entry = match entry {
                                Entry::Property(prop)
                                    if inherited_annotations
                                        .iter()
                                        .any(|(name, _)| *name == prop.name) =>
                                {
                                    let mut prop = Arc::unwrap_or_clone(prop);
                                    let mut annotations: Vec<_> = inherited_annotations
                                        .iter()
                                        .filter(|(name, _)| *name == prop.name)
                                        .flat_map(|(_, annotations)| annotations.iter())
                                        .filter(|annotation| annotation.name != "Deprecated")
                                        .cloned()
                                        .collect();
                                    annotations.append(&mut prop.annotations);
                                    prop.annotations = annotations;
                                    Entry::Property(Arc::new(prop))
                                }
                                entry => entry,
                            };
                            prepared_child.push((
                                entry,
                                child_entry_scopes.get(entry_index).cloned().unwrap_or(None),
                            ));
                        }
                        let mut child_used = vec![false; prepared_child.len()];
                        for (entry_index, pe) in psrc.entries.iter().enumerate() {
                            // A child's override keeps the parent member's place.
                            if let Entry::Property(p) = pe
                                && child_names.contains(&p.name)
                                && !has_modifier(&p.modifiers, Modifier::Local)
                                && let Some(ci) =
                                    prepared_child.iter().enumerate().position(|(i, (e, _))| {
                                        !child_used[i]
                                            && matches!(e, Entry::Property(c) if c.name == p.name)
                                    })
                            {
                                child_used[ci] = true;
                                combined_entries.push(prepared_child[ci].0.clone());
                                combined_entry_scopes.push(prepared_child[ci].1.clone());
                                continue;
                            }
                            if let Entry::Property(p) = pe
                                && (!child_names.contains(&p.name)
                                    || has_modifier(&p.modifiers, Modifier::Local))
                            {
                                combined_entries.push(shadow_local_reads(pe, &shadowed));
                                combined_entry_scopes.push(
                                    psrc.entry_scopes
                                        .get(entry_index)
                                        .and_then(Clone::clone)
                                        .or_else(|| Some(Arc::clone(&parent_scope))),
                                );
                                if psrc.evaluated_properties.contains(&p.name) {
                                    combined_evaluated_properties.push(p.name.clone());
                                    if let Some(value) = psrc.scope().get(p.name.as_ref()) {
                                        inherited_property_values
                                            .insert(p.name.clone(), value.clone());
                                    }
                                }
                            }
                        }
                        for (i, (entry, scope)) in prepared_child.into_iter().enumerate() {
                            if !child_used[i] {
                                combined_entries.push(entry);
                                combined_entry_scopes.push(scope);
                            }
                        }
                        combined_evaluated_properties.extend(src.evaluated_properties);
                        // Keep the ObjectSource invariant used by amendment
                        // seeding: every evaluated property name resolves to
                        // that property's value in `scope`, even when a child
                        // module has a same-named lexical binding.
                        for name in inherited_property_values.keys() {
                            src.captured.parts_mut().declared.remove(name.as_ref());
                        }
                        src.captured
                            .parts_mut()
                            .values
                            .extend(inherited_property_values);
                        src.entries = combined_entries.into();
                        src.entry_scopes = combined_entry_scopes;
                        src.evaluated_properties = combined_evaluated_properties;
                    }
                    Some(Arc::new(src))
                } else {
                    None
                };
                Ok(Value::Object(Arc::new(merged), source))
            } else {
                Ok(Value::Object(Arc::new(merged), None))
            }
        } else {
            Ok(child_defaults)
        }
        .map(|val| {
            // Set is_open flag and class_name on the result's ObjectSource
            let is_open = has_modifier(class_mods, Modifier::Open);
            if let Value::Object(map, Some(src)) = val {
                let data_property_names = body
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::Property(prop)
                            if !has_modifier(&prop.modifiers, Modifier::Local) =>
                        {
                            Some(prop.name.as_ref())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let schema_member_names = body
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::ClassDef(name, ..)
                            if !data_property_names.contains(name.as_str()) =>
                        {
                            Some(name.as_str())
                        }
                        Entry::Property(prop)
                            if matches!(prop.value, Some(Expr::Lambda(..)))
                                && has_modifier(&prop.modifiers, Modifier::Local)
                                && !data_property_names.contains(prop.name.as_ref()) =>
                        {
                            Some(prop.name.as_ref())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let mut map = map;
                Arc::make_mut(&mut map).retain(|key, _| !schema_member_names.contains(&**key));
                let mut new_src = Arc::unwrap_or_clone(src);
                new_src.is_open = is_open;
                new_src.is_abstract = has_modifier(class_mods, Modifier::Abstract);
                new_src.type_name = Some(class_name.to_string());
                new_src.type_identity = Some(scope.runtime_type_identity(class_name));
                new_src.parent_type_names = parent_type_names;
                new_src.parent_type_identities = parent_type_identities;
                new_src.kind = ObjectKind::Class;
                Value::Object(map, Some(Arc::new(new_src)))
            } else {
                val
            }
        })
    }

    /// Evaluate a type alias declaration.
    ///
    /// If the aliased type is a named type that exists in scope (e.g. a class),
    /// the alias name is bound to the same value so `new AliasName { ... }` works.
    pub(super) fn eval_type_alias(
        &self,
        name: &str,
        ty: &crate::parser::TypeExpr,
        generic: Option<&crate::parser::GenericAlias>,
        scope: &mut Scope,
    ) {
        // Store the TypeExpr so `is`/`as` can resolve alias names to their definitions
        scope.set_type_alias(name.to_string(), ty.clone());
        if let Some(generic) = generic {
            scope.set_type_alias(
                generic_alias_key(name),
                crate::parser::TypeExpr::Generic(generic.0.join(","), vec![generic.1.clone()]),
            );
        }
        // Also mark the name as a type among the bindings, which a function
        // captures (its alias map is not), so `new Alias {}` inside one is
        // still known to name a type.
        scope.declare(type_alias_marker(name), Value::Bool(true));
        match ty {
            crate::parser::TypeExpr::Named(target) => {
                // Alias to a class or another alias already in scope, possibly
                // qualified by an imported module (`Config.Script`)
                if let Some(val) = resolve_dotted(scope, target) {
                    scope.declare(name, val);
                }
            }
            crate::parser::TypeExpr::Nullable(inner) => {
                // typealias Foo = Bar? -- alias to the inner type
                if let crate::parser::TypeExpr::Named(target) = inner.as_ref()
                    && let Some(val) = resolve_dotted(scope, target)
                {
                    scope.declare(name, val);
                }
            }
            crate::parser::TypeExpr::Constrained(base, _, _) => {
                self.bind_type_alias_value(name, base, scope);
            }
            // Union types, generics, etc. -- no runtime representation needed
            _ => {}
        }
    }

    pub(super) fn bind_type_alias_value(&self, name: &str, target: &str, scope: &mut Scope) {
        if let Some(val) = resolve_dotted(scope, target.trim_end_matches('?')) {
            scope.declare(name, val);
        }
    }

    /// Validate values added through an inherited typed Mapping property.
    ///
    /// Generic Mapping checks normally validate the Mapping container only.
    /// For a synthesized declaration that would let an amending module insert
    /// a value of the wrong type while preserving a lazy module-member error,
    /// validate its completed entries here. This runs only while that member
    /// is evaluated, so a narrowed import of an unrelated member stays lazy.
    pub(super) fn check_inherited_mapping_value_types(
        &mut self,
        value: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        fn mapping_type_pairs(
            ty: &crate::parser::TypeExpr,
            scope: &Scope,
            aliases: &mut HashSet<Name>,
            out: &mut Vec<(crate::parser::TypeExpr, crate::parser::TypeExpr)>,
        ) {
            use crate::parser::TypeExpr;

            match ty {
                TypeExpr::Named(name) => {
                    if let Some(alias) = scope.get_type_alias(name)
                        && aliases.insert(nm(name))
                    {
                        mapping_type_pairs(alias, scope, aliases, out);
                    }
                }
                TypeExpr::Nullable(inner) => mapping_type_pairs(inner, scope, aliases, out),
                TypeExpr::Union(variants) => {
                    for variant in variants {
                        mapping_type_pairs(variant, scope, aliases, out);
                    }
                }
                TypeExpr::Generic(name, params) if name == "Mapping" || name == "Map" => {
                    if let [key, value, ..] = params.as_slice() {
                        out.push((key.clone(), value.clone()));
                    }
                }
                TypeExpr::Constrained(base, _, _) => {
                    if let Ok(base) = parser::parse_type_name(base) {
                        mapping_type_pairs(&base, scope, aliases, out);
                    }
                }
                TypeExpr::Generic(name, _) => {
                    if let Some(alias) = scope.get_type_alias(name)
                        && aliases.insert(nm(name))
                    {
                        mapping_type_pairs(alias, scope, aliases, out);
                    }
                }
            }
        }

        let Value::Object(entries, _) = value else {
            return Ok(());
        };
        let mut pairs = Vec::new();
        mapping_type_pairs(ty, scope, &mut HashSet::default(), &mut pairs);
        if pairs.is_empty() {
            return Ok(());
        }
        // An outer Mapping union selects one alternative for the whole
        // mapping, not independently for each entry. A nested union remains
        // part of its key or value type and is handled by eval_type_check.
        for (key_ty, value_ty) in &pairs {
            let key_checkable = type_is_runtime_checkable(key_ty, scope);
            let value_checkable = type_is_runtime_checkable(value_ty, scope);
            let mut alternative_matches = true;
            for (stored_key, entry) in entries.iter() {
                let key = self
                    .mapping_key_values
                    .get(stored_key)
                    .cloned()
                    .unwrap_or_else(|| crate::value::mapping_storage_value(stored_key));
                if (key_checkable && !self.eval_type_check(&key, key_ty, scope, depth + 1)?)
                    || (value_checkable
                        && !self.eval_type_check(entry, value_ty, scope, depth + 1)?)
                {
                    alternative_matches = false;
                    break;
                }
            }
            if alternative_matches {
                return Ok(());
            }
        }
        let (stored_key, entry) = entries.iter().next().expect("non-empty Mapping mismatch");
        let key = self
            .mapping_key_values
            .get(stored_key)
            .cloned()
            .unwrap_or_else(|| crate::value::mapping_storage_value(stored_key));
        Err(Error::Eval(format!(
            "Expected Mapping entry of type `{} -> {}`, but got `{}` -> `{}`.",
            pairs
                .iter()
                .map(|(key, _)| display_type_expr(key))
                .collect::<Vec<_>>()
                .join(" | "),
            pairs
                .iter()
                .map(|(_, value)| display_type_expr(value))
                .collect::<Vec<_>>()
                .join(" | "),
            value_type_name(&key),
            value_type_name(entry)
        )))
    }

    pub(super) fn remember_mapping_key(&mut self, storage_key: Arc<str>, value: &Value) {
        // An amendment can replace an equal key with a differently-spelled
        // native unit (`[1.s]` with `[1000.ms]`). Pkl evaluates constraints
        // against the replacement expression, so the most recent key value
        // is authoritative even though map storage keeps the normalized key.
        self.mapping_key_values.insert(storage_key, value.clone());
    }

    /// Evaluate an amended object with late binding.
    ///
    /// Merges the base object's original entries with the overlay entries,
    /// then re-evaluates everything so that dependent properties pick up
    /// overridden values.
    /// `referenced_roots` of an object body, computed once per body: a class
    /// or object amended many times would otherwise be analyzed every time.
    pub(super) fn body_referenced_roots(
        &mut self,
        body: &crate::parser::Body,
    ) -> Arc<HashSet<Name>> {
        let key = Arc::as_ptr(body) as usize;
        if let Some((_, roots)) = self.body_roots_cache.get(&key) {
            return Arc::clone(roots);
        }
        let roots = Arc::new(referenced_roots(body));
        self.body_roots_cache
            .insert(key, (Arc::clone(body), Arc::clone(&roots)));
        roots
    }

    /// Give each property an amending module redefines without a type the
    /// inherited declaration's type. An amending module may not repeat a
    /// non-local annotation, but it still inherits its type and its default
    /// when assigning an untyped `new`.
    pub(super) fn inherit_module_property_types(
        &self,
        body: &Body,
        inherited: &[Arc<Property>],
        child_scope: &mut Scope,
    ) -> Option<Body> {
        let parent_type = |name: &str| {
            inherited.iter().rev().find_map(|prop| {
                (&*prop.name == name && !has_modifier(&prop.modifiers, Modifier::Local))
                    .then_some(prop.type_ann.as_ref())
                    .flatten()
            })
        };
        let needs_type = |entry: &Entry| match entry {
            Entry::Property(prop)
                if prop.type_ann.is_none()
                    && !has_modifier(&prop.modifiers, Modifier::Local)
                    && (prop.body.is_some()
                        || prop
                            .value
                            .as_ref()
                            .is_some_and(parser::has_untyped_result_new)) =>
            {
                parent_type(&prop.name)
            }
            _ => None,
        };
        if !body.iter().any(|entry| needs_type(entry).is_some()) {
            return None;
        }

        let mut typed = Vec::with_capacity(body.len());
        for entry in body.iter() {
            let (Entry::Property(prop), Some(ty)) = (entry, needs_type(entry)) else {
                typed.push(entry.clone());
                continue;
            };
            let mut prop = (**prop).clone();
            prop.type_ann = Some(ty.clone());
            if let Some(expr) = &mut prop.value
                && parser::has_untyped_result_new(expr)
                && !type_is_any(ty)
                && let Some(default) = child_scope
                    .get(&prop.name)
                    .filter(|value| matches!(value, Value::Object(..) | Value::List(_)))
                    .cloned()
            {
                let binding = format!("#parent:{}", prop.name);
                child_scope.set(&binding, default);
                parser::rewrite_untyped_result_new(expr, &mut |entries| {
                    let entries =
                        infer_amendment_members(ty, &entries, child_scope).unwrap_or(entries);
                    Expr::Binop(
                        BinOp::Add,
                        Box::new(Expr::Ident(binding.clone())),
                        Box::new(Expr::ObjectBody(entries)),
                    )
                });
            }
            typed.push(Entry::Property(Arc::new(prop)));
        }
        Some(typed.into())
    }

    /// Give each property a child class redefines without a type the parent's
    /// declared type. Untyped `new` values amend a default constructed where
    /// that inherited type was declared.
    pub(super) fn inherit_property_types(
        &mut self,
        body: &Body,
        parent: &ObjectSource,
        child_scope: &mut Scope,
        depth: usize,
    ) -> Result<Option<Body>> {
        let parent_type = |name: &str| {
            parent
                .entries
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, entry)| match entry {
                    Entry::Property(prop)
                        if &*prop.name == name
                            && !has_modifier(&prop.modifiers, Modifier::Local) =>
                    {
                        prop.type_ann.as_ref().map(|ty| (index, ty))
                    }
                    _ => None,
                })
        };
        let needs_type = |entry: &Entry| match entry {
            Entry::Property(prop)
                if prop.type_ann.is_none() && !has_modifier(&prop.modifiers, Modifier::Local) =>
            {
                parent_type(&prop.name)
            }
            _ => None,
        };
        if !body.iter().any(|entry| needs_type(entry).is_some()) {
            return Ok(None);
        }

        let mut parent_scope = None;
        let mut typed = Vec::with_capacity(body.len());
        for entry in body.iter() {
            let (Entry::Property(prop), Some((index, ty))) = (entry, needs_type(entry)) else {
                typed.push(entry.clone());
                continue;
            };
            // A union with no `*` alternative cannot be amended without a value
            // to start from. Its aliases resolve where the parent declared the
            // property, which the amending scope may not see.
            if prop.value.is_none()
                && let Some(body) = &prop.body
            {
                let declaring = match parent.entry_scopes.get(index).and_then(Clone::clone) {
                    Some(captured) => restore_scope(&captured),
                    None => parent_scope
                        .get_or_insert_with(|| restore_scope(&capture_object_source_scope(parent)))
                        .clone(),
                };
                let state = self.union_amend_state(ty, &declaring);
                if matches!(
                    state,
                    UnionAmendState::NoDefault | UnionAmendState::NullDefault(_)
                ) {
                    // A parent's default value is not in scope here, so leave
                    // such an amendment untyped rather than have it rejected
                    // as amending an undefined property.
                    if matches!(parent.entries.get(index), Some(Entry::Property(parent_prop))
                        if parent_prop.value.is_some() || parent_prop.body.is_some())
                    {
                        typed.push(entry.clone());
                        continue;
                    }
                }
                child_scope.set(
                    union_amend_marker(&prop.name),
                    Value::String(encode_union_amend(ty, body, &state).into()),
                );
            }
            let mut prop = (**prop).clone();
            prop.type_ann = Some(ty.clone());
            if let Some(expr) = &mut prop.value
                && parser::has_untyped_result_new(expr)
                && !type_is_any(ty)
            {
                let binding = format!("#parent:{}", prop.name);
                let default = match parent.scope().get(binding.as_str()) {
                    Some(default) => default.clone(),
                    None => {
                        let scope = match parent.entry_scopes.get(index).and_then(Clone::clone) {
                            Some(captured) => restore_scope(&captured),
                            None => parent_scope
                                .get_or_insert_with(|| {
                                    restore_scope(&capture_object_source_scope(parent))
                                })
                                .clone(),
                        };
                        self.eval_expr(
                            &Expr::InferredNew(ty.clone(), Vec::new().into()),
                            &scope,
                            depth + 1,
                        )?
                    }
                };
                child_scope.set(&binding, default);
                parser::rewrite_untyped_result_new(expr, &mut |entries| {
                    let entries =
                        infer_amendment_members(ty, &entries, child_scope).unwrap_or(entries);
                    Expr::Binop(
                        BinOp::Add,
                        Box::new(Expr::Ident(binding.clone())),
                        Box::new(Expr::ObjectBody(entries)),
                    )
                });
            }
            typed.push(Entry::Property(Arc::new(prop)));
        }
        Ok(Some(typed.into()))
    }
}

/// Standard library classes declared `external`. User code can neither
/// instantiate nor extend them.
pub(super) const EXTERNAL_CLASSES: &[&str] = &[
    "Any",
    "Null",
    "Class",
    "TypeAlias",
    "Module",
    "Number",
    "Int",
    "Float",
    "Boolean",
    "String",
    "Regex",
    "Duration",
    "DataSize",
    "Object",
    "Function",
    "Function0",
    "Function1",
    "Function2",
    "Function3",
    "Function4",
    "Function5",
    "Pair",
    "Collection",
    "IntSeq",
    "VarArgs",
    "List",
    "Set",
    "Map",
    "Bytes",
];

/// Abstract standard library classes that aren't external.
pub(super) const ABSTRACT_STDLIB_CLASSES: &[&str] = &[
    "Annotation",
    "Typed",
    "BaseValueRenderer",
    "ValueRenderer",
    "BytesRenderer",
];

/// Standard library classes that are neither `open` nor `abstract`.
pub(super) const CLOSED_STDLIB_CLASSES: &[&str] = &["Dynamic", "Listing", "Mapping"];

/// A property a subclass redeclares must keep its parent's `const` and `fixed`
/// modifiers, and may not add them.
fn check_override_modifiers(body: &[Entry], parent: &ObjectSource) -> Result<()> {
    for entry in body {
        let Entry::Property(prop) = entry else {
            continue;
        };
        if prop.is_method || has_modifier(&prop.modifiers, Modifier::Local) {
            continue;
        }
        let Some(inherited) = parent
            .entries
            .iter()
            .rev()
            .find_map(|inherited| match inherited {
                Entry::Property(p)
                    if p.name == prop.name
                        && !p.is_method
                        && !has_modifier(&p.modifiers, Modifier::Local) =>
                {
                    Some(p)
                }
                _ => None,
            })
        else {
            continue;
        };
        let parent_class = parent
            .type_name
            .as_deref()
            .map(|name| qualified_class_name(parent.type_identity.as_deref(), name))
            .unwrap_or_default();
        for (modifier, word) in [(Modifier::Const, "const"), (Modifier::Fixed, "fixed")] {
            let child_has = has_modifier(&prop.modifiers, modifier.clone());
            let parent_has = has_modifier(&inherited.modifiers, modifier);
            if child_has == parent_has {
                continue;
            }
            let name = &prop.name;
            return Err(Error::Eval(if parent_has {
                format!(
                    "Missing modifier `{word}`.\n\nProperty `{name}` must be declared {word}, because it overrides a {word} property on parent class `{parent_class}`."
                )
            } else {
                format!(
                    "Modifier `{word}` cannot be applied to property `{name}`.\n\nProperty `{name}` cannot be declared {word}, because it overrides a non-{word} property on parent class `{parent_class}`."
                )
            }));
        }
    }
    Ok(())
}

/// A class name as pkl reports it, `module#Class`, from the class's
/// definition-site identity (`<module path>.<Class>`).
pub(super) fn qualified_class_name(identity: Option<&str>, name: &str) -> String {
    let simple = name.rsplit('.').next().unwrap_or(name);
    let module = identity
        .and_then(|identity| identity.strip_suffix(simple))
        .and_then(|path| path.strip_suffix('.'))
        .and_then(|path| Path::new(path).file_stem())
        .map(|stem| stem.to_string_lossy().into_owned());
    match module {
        Some(module) => format!("{module}#{simple}"),
        None => simple.to_string(),
    }
}

/// Check that `class class_name extends parent_name` names a class pkl lets
/// user code extend.
pub(super) fn check_supertype(
    scope: &Scope,
    class_name: &str,
    parent_name: &str,
    parent: Option<&Value>,
) -> Result<()> {
    if parent_name == class_name {
        return Err(Error::Eval(format!(
            "Class `{}` cannot extend itself.",
            qualified_class_name(Some(&scope.runtime_type_identity(class_name)), class_name)
        )));
    }
    if scope.get_type_alias(parent_name).is_some() {
        return Err(Error::Eval(format!(
            "`{parent_name}` is not a valid supertype."
        )));
    }
    match parent {
        Some(Value::Object(_, Some(source)))
            if source.type_name.is_some() && !source.is_open && !source.is_abstract =>
        {
            Err(Error::Eval(format!(
                "Cannot extend non-open class `{}`.",
                qualified_class_name(source.type_identity.as_deref(), parent_name)
            )))
        }
        // Built-ins are bound to a marker string of their own name.
        None | Some(Value::String(_)) => {
            if EXTERNAL_CLASSES.contains(&parent_name) {
                Err(Error::Eval(format!(
                    "Cannot extend external class `{parent_name}`."
                )))
            } else if CLOSED_STDLIB_CLASSES.contains(&parent_name) {
                Err(Error::Eval(format!(
                    "Cannot extend non-open class `{parent_name}`."
                )))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// Check that `new type_name { ... }` names a class user code can
/// instantiate.
pub(super) fn check_instantiable(
    scope: &Scope,
    type_name: Option<&str>,
    class: Option<&Value>,
) -> Result<()> {
    let Some(type_name) = type_name else {
        return Ok(());
    };
    // The class a type alias names (`typealias R = Regex`).
    let mut resolved = type_name;
    let mut seen = HashSet::default();
    while seen.insert(resolved) {
        match scope.get_type_alias(resolved) {
            Some(crate::parser::TypeExpr::Named(target)) if !target.starts_with('*') => {
                resolved = target;
            }
            _ => break,
        }
    }
    match class {
        Some(Value::Object(_, Some(source))) if source.is_abstract => Err(Error::Eval(format!(
            "Cannot instantiate abstract class `{}`.",
            qualified_class_name(
                source.type_identity.as_deref(),
                // A `Listing<T>` element is built through a synthetic name.
                if type_name == LISTING_ELEM_CLASS || type_name.starts_with('\0') {
                    source.type_name.as_deref().unwrap_or(type_name)
                } else {
                    type_name
                }
            )
        ))),
        // A class is bound under its own name, or under a type alias of it.
        // Any other binding of an object (`local Foo2 = Foo`) is a value.
        Some(Value::Object(_, Some(source)))
            if !type_name.contains('.')
                && source.type_name.as_deref() != Some(type_name)
                && scope.get_type_alias(type_name).is_none()
                && scope.get(&type_alias_marker(type_name)).is_none()
                // Module metadata is carried on a source without entries,
                // whereas an ordinary object has its declaration body here.
                && !(source.entries.is_empty()
                    && (source.poisoned_members.is_some() || !source.deprecated.is_empty())) =>
        {
            Err(Error::Eval(format!(
                "Expected `{type_name}` to be a type, but it is not."
            )))
        }
        // Built-ins are bound to a marker string of their own name.
        None if EXTERNAL_CLASSES.contains(&resolved) => Err(Error::Eval(format!(
            "Cannot instantiate, or amend an instance of, external class `{resolved}`."
        ))),
        Some(Value::String(name)) if EXTERNAL_CLASSES.contains(&name.as_ref()) => Err(Error::Eval(
            format!("Cannot instantiate, or amend an instance of, external class `{name}`."),
        )),
        None | Some(Value::String(_)) if ABSTRACT_STDLIB_CLASSES.contains(&resolved) => Err(
            Error::Eval(format!("Cannot instantiate abstract class `{resolved}`.")),
        ),
        _ => Ok(()),
    }
}

/// The binding that marks `name` as a type alias (see `eval_type_alias`).
/// The NUL prefix keeps it apart from any pkl identifier.
pub(super) fn type_alias_marker(name: &str) -> String {
    format!("\0typealias:{name}")
}

/// The binding that carries a property's union-amend state, decided where its
/// type was declared, to the place its body is evaluated.
pub(super) fn union_amend_marker(name: &str) -> String {
    format!("#union-amend:{name}")
}

/// `state` for the declared type `ty`, tagged with the type so that a
/// same-named property of another type does not read it.
pub(super) fn encode_union_amend(
    ty: &crate::parser::TypeExpr,
    body: &Body,
    state: &UnionAmendState,
) -> String {
    let (kind, union) = match state {
        UnionAmendState::NullDefault(union) => ("null", union.as_str()),
        UnionAmendState::NoDefault => ("none", ""),
        _ => ("ok", ""),
    };
    format!(
        "{kind}\u{1}{}\u{1}{}\u{1}{union}",
        body_identity(body),
        display_type_expr(ty)
    )
}

/// Identifies the object body of the amendment that recorded a state. Bodies
/// are shared (`Arc`) across the clones the evaluator makes of a property, so
/// this tells apart amendments that merely print the same.
fn body_identity(body: &Body) -> usize {
    Arc::as_ptr(body) as usize
}

/// The union-amend state recorded in `scope` for the property `name` declared
/// `ty`, whose amending `body` is being evaluated. The binding is visible to
/// every object nested in that body, so a state counts only when it was
/// recorded for this very body and type: a same-named property of a nested
/// object, or one whose alias merely prints the same, decides for itself.
pub(super) fn recorded_union_amend(
    scope: &Scope,
    name: &str,
    ty: &crate::parser::TypeExpr,
    body: &Body,
) -> Option<UnionAmendState> {
    let Some(Value::String(recorded)) = scope.get(&union_amend_marker(name)) else {
        return None;
    };
    let mut parts = recorded.splitn(4, '\u{1}');
    let (kind, recorded_body, recorded_ty, union) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if recorded_body != body_identity(body).to_string() || recorded_ty != display_type_expr(ty) {
        return None;
    }
    Some(match kind {
        "null" => UnionAmendState::NullDefault(union.to_string()),
        "none" => UnionAmendState::NoDefault,
        _ => UnionAmendState::NotUnion,
    })
}
