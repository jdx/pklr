//! Evaluating expressions.

use super::*;

impl Evaluator {
    /// Evaluate `expr`. Literals and plain name lookups, which are most of the
    /// expressions evaluated, are answered here without allocating the boxed
    /// future that a recursive evaluation needs.
    pub(super) fn eval_expr(&mut self, expr: &Expr, scope: &Scope, depth: usize) -> Result<Value> {
        if depth <= self.max_depth
            && let Some(result) = eval_simple_expr(expr, scope, depth, self.max_depth)
        {
            return result;
        }
        self.eval_expr_boxed(expr, scope, depth)
    }

    pub(super) fn eval_expr_boxed(
        &mut self,
        expr: &Expr,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        self.check_cancelled()?;
        match expr {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::String(s) => Ok(Value::String(Arc::clone(s))),
            Expr::StringInterpolation(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        StringInterpPart::Literal(s) => result.push_str(s),
                        StringInterpPart::Expr(e) => {
                            let val = self.eval_expr(e, scope, depth + 1)?;
                            match &val {
                                Value::String(s) => result.push_str(s),
                                _ => result.push_str(&self.value_to_string(&val, depth + 1)?),
                            }
                        }
                    }
                }
                Ok(Value::String(result.into()))
            }
            Expr::Ident(name) => scope.get(name).cloned().ok_or_else(|| {
                Error::Eval(
                    scope
                        .poison_of(name)
                        .cloned()
                        .unwrap_or_else(|| format!("undefined variable: {name}")),
                )
            }),
            Expr::Lambda(params, body) => {
                // The body is shared with the AST unless one of the rewrites
                // below applies to it.
                let mut body = Arc::clone(body);
                if needs_method_result_types(&body) {
                    capture_method_result_types(Arc::make_mut(&mut body), scope);
                }
                let mut names = HashSet::default();
                collect_unshadowed_names(&body, &mut names);
                // A body that names a type captures the whole scope (below),
                // so resolving its aliases leaves `names` as it is.
                if names.contains(NAMES_A_TYPE) && scope.has_type_aliases() {
                    capture_type_aliases(Arc::make_mut(&mut body), scope);
                }
                let mut refs = HashSet::default();
                let shadows = params.iter().map(nm).collect::<HashSet<_>>();
                collect_expr_refs(&body, &mut refs, &shadows);
                // Capture only the bindings the body can reach: every name it
                // mentions, ignoring shadowing, plus the implicit receivers.
                // Flattening the whole scope for every lambda value, and
                // restoring all of it on every call, dominated evaluation. An
                // object built in the body sees its enclosing bindings through
                // `outer`, so a body that mentions `outer` keeps everything, as
                // does a body that names a type (see `NAMES_A_TYPE`).
                let captured_body = refs
                    .iter()
                    .filter(|name| scope.get(name).is_none())
                    .find_map(|name| scope.poison_of(name))
                    .map(|message| {
                        Arc::new(Expr::Throw(Box::new(Expr::String(message.clone().into()))))
                    })
                    .unwrap_or(body);
                // `ModuleOutput.text` is the only consumer of this hidden
                // binding, reached through `super.text`. In particular, a
                // renderer converter that merely constructs a typed value
                // must not retain the output's implicit module value through
                // its type-resolution capture.
                let captures_output_text_context = names.contains("super");
                let captured = if names.contains("outer") || names.contains(NAMES_A_TYPE) {
                    let mut captured = scope.flatten();
                    if !captures_output_text_context {
                        captured.shift_remove(MODULE_OUTPUT_TEXT_CONTEXT);
                    }
                    captured
                } else {
                    scope.flatten_names(
                        names
                            .iter()
                            .map(|name| &**name)
                            .chain(["this", "module", "super"])
                            .chain(
                                captures_output_text_context.then_some(MODULE_OUTPUT_TEXT_CONTEXT),
                            ),
                    )
                };
                let captured = Arc::new(captured);
                self.register_lambda_guard(&captured);
                Ok(Value::Lambda(Arc::clone(params), captured_body, captured))
            }
            Expr::InferredNew(ty, entries) => {
                let (name, params) = inferred_new_type(ty, scope, 0)?;
                self.eval_expr(
                    &Expr::New(Some(name), entries.clone(), params),
                    scope,
                    depth + 1,
                )
            }
            Expr::New(type_name, entries, generic_params) => {
                let mut constructor_scope = scope.child();
                constructor_scope.set("super", Value::Object(Arc::default(), None));
                constructor_scope.receiver_entries = Some(entries.clone());
                constructor_scope.receiver_list_base = None;
                let scope = &constructor_scope;
                // A Dynamic holding only elements renders as a JSON array. If
                // a generator may create either elements or members, decide
                // based on the branch actually taken.
                let has_elements = matches!(type_name.as_deref(), None | Some("Dynamic"))
                    && entries_are_listing_amendment(entries);
                let defines_members = has_elements && entries_define_members(entries);
                let eval_elements = |this: &mut Self| -> Result<Vec<Value>> {
                    let mut listing_scope = scope.child();
                    listing_scope.set(
                        "super",
                        Value::List(ListValue::new(ListKind::Listing, Vec::new())),
                    );
                    listing_scope.receiver_list_base = Some(0);
                    let mut items = Vec::new();
                    this.eval_listing_entries(entries, &listing_scope, depth + 1, &mut items)?;
                    Ok(items)
                };
                match type_name.as_deref() {
                    Some("module") => Err(Error::Eval("`new module` is not supported".into())),
                    _ if has_elements && !defines_members => Ok(Value::List(ListValue::new(
                        ListKind::Listing,
                        eval_elements(self)?,
                    ))),
                    _ if has_elements => {
                        let object = self.eval_entries(entries, scope, depth + 1)?;
                        if matches!(&object, Value::Object(map, _) if map.is_empty()) {
                            let items = eval_elements(self)?;
                            if !items.is_empty() {
                                return Ok(Value::List(ListValue::new(ListKind::Listing, items)));
                            }
                        }
                        Ok(object)
                    }
                    Some("Listing") => {
                        let mut listing_scope = scope.child();
                        listing_scope.set(
                            "super",
                            Value::List(ListValue::new(ListKind::Listing, Vec::new())),
                        );
                        listing_scope.receiver_list_base = Some(0);
                        let mut items = Vec::new();
                        self.eval_listing_entries(entries, &listing_scope, depth + 1, &mut items)?;
                        Ok(Value::List(ListValue::new(ListKind::Listing, items)))
                    }
                    Some("Mapping") => {
                        check_member_kinds("Mapping", entries, false)?;
                        // If the Mapping has a value type param (e.g., Mapping<String, Step>),
                        // resolve it as a default template so entries inherit the class type.
                        let value_type_defaults = generic_params
                            .iter()
                            .skip(1)
                            .filter_map(|name| {
                                let name = mapping_value_type_base(name);
                                resolve_dotted(scope, name).map(|value| (name.to_string(), value))
                            })
                            .collect::<Vec<_>>();
                        let mut map = ObjectMap::default();
                        self.eval_mapping_entries_with_type_default(
                            entries,
                            scope,
                            depth,
                            &mut map,
                            &value_type_defaults,
                            generic_params.get(1..).unwrap_or(&[]),
                            MappingInheritedDefault::default(),
                        )?;
                        // Build ObjectSource with a synthetic `default` entry so that
                        // body amendments (`steps { ["x"] { ... } }`) merge new entries
                        // with the value type class, preserving type_name for converters.
                        let mut src_entries = entries.to_vec();
                        if value_type_defaults.len() == 1
                            && !entries
                                .iter()
                                .any(|e| matches!(e, Entry::Property(p) if &*p.name == "default"))
                        {
                            // Inject a synthetic default property referencing the value type
                            let vt_name = mapping_value_type_base(&generic_params[1]).to_string();
                            src_entries.push(Entry::Property(Arc::new(Property {
                                annotations: vec![],
                                modifiers: vec![],
                                name: "default".into(),
                                type_ann: None,
                                value: Some(Expr::New(Some(vt_name), vec![].into(), vec![])),
                                body: None,
                                is_method: false,
                            })));
                        }
                        let source_body_members =
                            src_entries.iter().filter_map(entry_member_name).collect();
                        let deprecated = collect_deprecated(&src_entries);
                        let source = ObjectSource {
                            entries: src_entries.into(),
                            captured: SourceScope::lazy(
                                scope,
                                vec![name_of("outer"), name_of("this")],
                                vec!["outer", "this"],
                            ),
                            body_members: source_body_members,
                            is_open: true,
                            is_abstract: false,
                            type_name: None,
                            type_identity: None,
                            parent_type_names: Vec::new(),
                            parent_type_identities: Vec::new(),
                            entry_scopes: Vec::new(),
                            evaluated_properties: map.keys().cloned().collect(),
                            elements: Vec::new(),
                            mapping_value_types: generic_params.iter().skip(1).cloned().collect(),
                            deprecated,
                            poisoned_members: None,
                            kind: ObjectKind::Mapping,
                            is_parsed_json: false,
                            prototype: None,
                        };
                        Ok(Value::Object(Arc::new(map), Some(Arc::new(source))))
                    }
                    Some("Dynamic") => self.eval_entries(entries, scope, depth + 1),
                    // `new Mixin { body }` is a function that amends its
                    // argument with `body`.
                    Some("Mixin") => {
                        let arg = "\0pklr:arg0";
                        Ok(Value::Lambda(
                            Arc::new([arg.to_string()]),
                            Arc::new(Expr::Binop(
                                BinOp::Add,
                                Box::new(Expr::Ident(arg.into())),
                                Box::new(Expr::ObjectBody(entries.clone())),
                            )),
                            Arc::new(scope.flatten()),
                        ))
                    }
                    _ => {
                        // Check if type name matches a class in scope (supports dotted names)
                        let base = type_name.as_ref().and_then(|name| {
                            let parts: Vec<&str> = name.split('.').collect();
                            let mut val = scope.get(parts[0])?.clone();
                            for part in &parts[1..] {
                                val = match val {
                                    Value::Object(ref map, _) => map.get(*part)?.clone(),
                                    _ => return None,
                                };
                            }
                            Some(val)
                        });
                        let base = match base {
                            Some(base) => Some(base),
                            None => match type_name.as_deref() {
                                Some(name) => self.builtin_class(name)?,
                                None => None,
                            },
                        };
                        // A class whose definition failed (for example one
                        // reading a `module` property not evaluated yet) is
                        // poisoned; report why instead of building a bare object.
                        if base.is_none()
                            && let Some(message) = type_name
                                .as_deref()
                                .and_then(|name| poisoned_member(scope, name))
                        {
                            return Err(Error::Eval(message));
                        }
                        check_instantiable(scope, type_name.as_deref(), base.as_ref())?;
                        if let Some(Value::Object(ref base_map, Some(ref base_src))) = base {
                            check_no_elements(base_src, entries)?;
                            // Enforce open modifier: non-open classes reject new properties
                            if !base_src.is_open {
                                // Collect all declared property names from the base class
                                // (includes those with no default value)
                                let base_names: std::collections::HashSet<Name> = base_src
                                    .entries
                                    .iter()
                                    .filter_map(|e| {
                                        if let Entry::Property(p) = e {
                                            Some(p.name.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .chain(base_map.keys().cloned())
                                    .collect();
                                for entry in entries.iter() {
                                    match entry {
                                        Entry::Property(p)
                                            if !has_modifier(&p.modifiers, Modifier::Local)
                                                && !base_names.contains(&p.name) =>
                                        {
                                            return Err(Error::Eval(format!(
                                                "cannot add property '{}' to non-open class",
                                                p.name
                                            )));
                                        }
                                        Entry::DynProperty(Expr::String(key), _)
                                            if !base_names.contains(&**key) =>
                                        {
                                            return Err(Error::Eval(format!(
                                                "cannot add property '{key}' to non-open class"
                                            )));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            let is_open = base_src.is_open;
                            // Late binding: re-evaluate merged base + overlay entries
                            let mut result = self
                                .eval_amended_object(base_map, base_src, entries, scope, depth)?;
                            // Preserve the base class's is_open flag and tag the
                            // type_name so output.renderer.converters can match it.
                            if let Value::Object(_, ref mut src_slot) = result {
                                // Keep the identity resolved from the class value, including
                                // when the constructor expression uses an imported class.
                                let tn = base_src.type_name.clone().or_else(|| type_name.clone());
                                let new_src = if let Some(src) = src_slot.take() {
                                    let mut s = Arc::unwrap_or_clone(src);
                                    if s.is_open != is_open {
                                        s.is_open = is_open;
                                    }
                                    s.type_name = tn;
                                    s.parent_type_names = base_src.parent_type_names.clone();
                                    s
                                } else {
                                    ObjectSource {
                                        entries: Vec::new().into(),
                                        captured: SourceScope::default(),
                                        body_members: HashSet::default(),
                                        is_open,
                                        is_abstract: false,
                                        type_name: tn,
                                        type_identity: base_src.type_identity.clone(),
                                        parent_type_names: base_src.parent_type_names.clone(),
                                        parent_type_identities: base_src
                                            .parent_type_identities
                                            .clone(),
                                        entry_scopes: Vec::new(),
                                        evaluated_properties: Vec::new(),
                                        elements: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&base_src.deprecated, entries),
                                        poisoned_members: None,
                                        kind: ObjectKind::Object,
                                        is_parsed_json: false,
                                        prototype: None,
                                    }
                                };
                                *src_slot = Some(Arc::new(new_src));
                            }
                            Ok(result)
                        } else if let Some(Value::Object(base_map, base_src)) = base {
                            // Fallback: eager merge
                            let overlay = self.eval_entries(entries, scope, depth + 1)?;
                            let mut merged: ObjectMap = (*base_map).clone();
                            let mut deprecated = base_src
                                .as_ref()
                                .map(|s| s.deprecated.clone())
                                .unwrap_or_default();
                            if let Value::Object(overlay_map, overlay_src) = &overlay {
                                merged.extend(
                                    overlay_map.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                                if let Some(os) = overlay_src.as_ref() {
                                    for (k, v) in &os.deprecated {
                                        deprecated.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                            let src = ObjectSource {
                                entries: Vec::new().into(),
                                captured: SourceScope::default(),
                                body_members: HashSet::default(),
                                is_open: true,
                                is_abstract: false,
                                type_name: type_name.clone(),
                                type_identity: None,
                                parent_type_names: Vec::new(),
                                parent_type_identities: Vec::new(),
                                entry_scopes: Vec::new(),
                                evaluated_properties: Vec::new(),
                                elements: Vec::new(),
                                mapping_value_types: Vec::new(),
                                deprecated,
                                poisoned_members: None,
                                kind: ObjectKind::Object,
                                is_parsed_json: false,
                                prototype: None,
                            };
                            Ok(Value::Object(Arc::new(merged), Some(Arc::new(src))))
                        } else {
                            self.eval_entries(entries, scope, depth + 1)
                        }
                    }
                }
            }
            Expr::ObjectBody(entries) => {
                let mut body_scope = scope.child();
                body_scope.set("super", Value::Object(Arc::default(), None));
                self.eval_entries(entries, &body_scope, depth + 1)
            }
            Expr::Field(obj_expr, field) => {
                if matches!(obj_expr.as_ref(), Expr::Ident(name) if name == "super") {
                    let value = self.eval_super_member(field, scope, depth + 1, true)?;
                    if let Some(Value::Object(_, source)) = scope.get("super") {
                        self.warn_if_deprecated_access(source, field);
                    }
                    return Ok(value);
                }
                let obj = self.eval_field_base(obj_expr, field, scope, depth)?;
                // Objects resolve their own members below; built-in values
                // answer from the standard library.
                if !matches!(obj, Value::Object(..))
                    && let Some(result) = self.stdlib_property(&obj, field)
                {
                    return result;
                }
                // Built-in properties
                match (&obj, field.as_str()) {
                    (Value::List(items), "length") => return Ok(Value::Int(items.len() as i64)),
                    (Value::List(items), "isEmpty") => return Ok(Value::Bool(items.is_empty())),
                    (Value::List(items), "first") => {
                        return items
                            .first()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::List(items), "last") => {
                        return items
                            .last()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::String(s), "length") => {
                        return Ok(Value::Int(s.chars().count() as i64));
                    }
                    (Value::String(s), "isEmpty") => return Ok(Value::Bool(s.is_empty())),
                    (Value::Object(map, _), "length") => return Ok(Value::Int(map.len() as i64)),
                    (Value::Object(map, _), "isEmpty") => return Ok(Value::Bool(map.is_empty())),
                    (Value::Object(map, _), "keys") => {
                        return Ok(Value::List(ListValue::new(
                            ListKind::Set,
                            map.keys()
                                .map(|key| crate::value::mapping_storage_value(key))
                                .collect::<Vec<_>>(),
                        )));
                    }
                    (Value::Object(map, _), "values") => {
                        return Ok(Value::List(
                            map.values().cloned().collect::<Vec<_>>().into(),
                        ));
                    }
                    _ => {}
                }
                match &obj {
                    Value::Object(map, source) => {
                        if let Some(message) = poisoned_member_error(source.as_deref(), field) {
                            return Err(Error::Eval(message));
                        }
                        let hidden = || {
                            let source = source.as_ref()?;
                            source
                                .evaluated_properties
                                .iter()
                                .any(|name| name.as_ref() == field.as_str())
                                .then(|| source.scope().get(field.as_str()).cloned())?
                        };
                        let val = map
                            .get(field.as_str())
                            .cloned()
                            .or_else(hidden)
                            .ok_or_else(|| {
                                Error::Eval(
                                    missing_member_error(source, obj_expr, field, scope)
                                        .unwrap_or_else(|| {
                                            missing_property_message(
                                                source,
                                                obj_expr,
                                                field,
                                                scope,
                                                &self.module_names,
                                            )
                                        }),
                                )
                            })?;
                        self.warn_if_deprecated_access(source, field);
                        Ok(val)
                    }
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::NullSafeField(obj_expr, field) => {
                let obj = self.eval_field_base(obj_expr, field, scope, depth)?;
                match &obj {
                    Value::Null => Ok(Value::Null),
                    _ if self.stdlib_property(&obj, field).is_some() => {
                        self.stdlib_property(&obj, field).unwrap()
                    }
                    Value::Object(map, source) => {
                        if let Some(message) = poisoned_member_error(source.as_deref(), field) {
                            return Err(Error::Eval(message));
                        }
                        if !map.contains_key(field.as_str())
                            && let Some(message) =
                                missing_member_error(source, obj_expr, field, scope)
                        {
                            return Err(Error::Eval(message));
                        }
                        let val = map.get(field.as_str()).cloned().unwrap_or(Value::Null);
                        if !is_null_value(&val) {
                            self.warn_if_deprecated_access(source, field);
                        }
                        Ok(val)
                    }
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::Index(obj_expr, key_expr) => {
                if matches!(obj_expr.as_ref(), Expr::Ident(name) if name == "super") {
                    let key = self.eval_expr(key_expr, scope, depth + 1)?;
                    if let Some(Value::List(items)) = scope.get("super") {
                        return match key {
                            Value::Int(index) => usize::try_from(index)
                                .ok()
                                .and_then(|index| items.get(index))
                                .cloned()
                                .ok_or_else(|| {
                                    Error::Eval(format!("index out of bounds: {index}"))
                                }),
                            _ => Err(Error::Eval("listing index must be an Int".into())),
                        };
                    }
                    return self.eval_super_member(&value_to_key(&key)?, scope, depth + 1, false);
                }
                let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
                let key = self.eval_expr(key_expr, scope, depth + 1)?;
                if let Some(result) = stdlib::index(&obj, &key) {
                    return result;
                }
                let key_str = value_to_key(&key)?;
                match obj {
                    Value::Object(map, source) => {
                        if let Some(message) = poisoned_member_error(source.as_deref(), &key_str) {
                            return Err(Error::Eval(message));
                        }
                        map.get(&key_str)
                            .cloned()
                            .or_else(|| {
                                map.iter()
                                    .find(|(stored, _)| {
                                        crate::value::mapping_storage_keys_equal(stored, &key_str)
                                    })
                                    .map(|(_, value)| value.clone())
                            })
                            .ok_or_else(|| {
                                Error::Eval(
                                    missing_member_error(&source, obj_expr, &key_str, scope)
                                        .unwrap_or_else(|| format!("key not found: {key_str}")),
                                )
                            })
                    }
                    Value::List(items) if items.kind() != ListKind::Set => match key {
                        Value::Int(index) => usize::try_from(index)
                            .ok()
                            .and_then(|index| items.get(index))
                            .cloned()
                            .ok_or_else(|| Error::Eval(format!("index out of bounds: {index}"))),
                        _ => Err(Error::Eval("listing index must be an Int".into())),
                    },
                    Value::List(_) => Err(Error::Eval("cannot index Set".into())),
                    _ => Err(Error::Eval("cannot index non-object".into())),
                }
            }
            Expr::Call(func_expr, args) => self.eval_call(func_expr, args, scope, depth),
            Expr::If(cond, then_expr, else_expr) => {
                let c = self.eval_expr(cond, scope, depth + 1)?;
                if is_truthy(&c) {
                    self.eval_expr(then_expr, scope, depth + 1)
                } else {
                    self.eval_expr(else_expr, scope, depth + 1)
                }
            }
            Expr::Let(name, val_expr, body_expr) => {
                let val = self.eval_expr(val_expr, scope, depth + 1)?;
                let mut child = scope.child();
                child.set(name, val);
                self.eval_expr(body_expr, &child, depth + 1)
            }
            Expr::Binop(op, left, right) => self.eval_binop(*op, left, right, scope, depth),
            Expr::Unop(op, operand) => {
                let v = self.eval_expr(operand, scope, depth + 1)?;
                match op {
                    UnOp::Neg => stdlib::negate(&v).unwrap_or_else(|| {
                        Err(stdlib::error_with_values(
                            format!(
                                "Operator `-` is not defined for operand type `{}`.",
                                v.type_name()
                            ),
                            &[("Operand", &v)],
                        ))
                    }),
                    UnOp::Not => stdlib::logical_not(&v),
                    UnOp::NonNull => {
                        if is_null_value(&v) {
                            Err(Error::Eval(
                                "non-null assertion failed: value is null".into(),
                            ))
                        } else {
                            Ok(v)
                        }
                    }
                }
            }
            Expr::Is(expr, ty) => {
                let val = self.eval_expr(expr, scope, depth + 1)?;
                let matches = self.eval_type_check(&val, ty, scope, depth)?;
                Ok(Value::Bool(matches))
            }
            Expr::As(expr, ty) => {
                let val = self.eval_expr(expr, scope, depth + 1)?;
                let matches = self.eval_type_check(&val, ty, scope, depth)?;
                if matches {
                    Ok(val)
                } else {
                    Err(Error::Eval(format!(
                        "cannot cast {} to {}",
                        value_type_name(&val),
                        display_type_expr(ty)
                    )))
                }
            }
            Expr::TemplateLocals(locals, inner) => {
                let mut template_scope = scope.child();
                for (name, value) in locals.0.iter() {
                    template_scope.set(name, value.clone());
                }
                self.eval_expr(inner, &template_scope, depth)
            }
            Expr::Throw(msg_expr) => {
                let msg = self.eval_expr(msg_expr, scope, depth + 1)?;
                Err(Error::Eval(value_to_display(&msg)))
            }
            Expr::Trace(expr, site) => {
                let v = self.eval_expr(expr, scope, depth + 1)?;
                self.trace(site, &v);
                Ok(v)
            }
            Expr::Read(uri_expr, module_path) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1)?;
                self.eval_read(&value_to_display(&uri), Path::new(module_path), false)
            }
            Expr::Import(uri, module_path) => {
                self.eval_import_expr(uri, Path::new(module_path), depth, None)
            }
            Expr::ImportGlob(pattern, module_path) => {
                let module_path = Path::new(module_path);
                let resolved = resolve_remote_relative(module_path, pattern);
                let pattern: &str = resolved.as_deref().unwrap_or(pattern);
                self.eval_glob_import(pattern, module_path, depth, None)
            }
            Expr::ReadOrNull(uri_expr, module_path) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1)?;
                self.eval_read(&value_to_display(&uri), Path::new(module_path), true)
            }
            Expr::ReadGlob(pattern_expr, module_path) => {
                let pattern = self.eval_expr(pattern_expr, scope, depth + 1)?;
                self.eval_read_glob(&value_to_display(&pattern), Path::new(module_path))
            }
        }
    }

    /// Evaluate the `(start, end)` arguments of an `IntSeq(start, end)` call.
    pub(super) fn eval_int_seq_bounds(
        &mut self,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<(i64, i64)> {
        let [start_expr, end_expr] = args else {
            return Err(Error::Eval(format!(
                "IntSeq() expects 2 arguments (start, end), got {}",
                args.len()
            )));
        };
        let mut bounds = [0i64; 2];
        for (slot, expr) in bounds.iter_mut().zip([start_expr, end_expr]) {
            match self.eval_expr(expr, scope, depth + 1)? {
                Value::Int(n) => *slot = n,
                other => {
                    return Err(Error::Eval(format!(
                        "IntSeq() expects Int arguments, got {}",
                        value_type_name(&other)
                    )));
                }
            }
        }
        Ok((bounds[0], bounds[1]))
    }

    pub(super) fn eval_binop(
        &mut self,
        op: BinOp,
        left: &Expr,
        right: &Expr,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Special case: object amendment `base + ObjectBody(entries)`
        if let BinOp::Add = op
            && let Some((overlay_entries, locals)) = overlay_body(right)
        {
            let base = self.eval_expr(left, scope, depth + 1)?;
            let overlay_scope = locals.map(|locals| {
                let mut overlay_scope = scope.child();
                for (name, value) in locals.0.iter() {
                    overlay_scope.set(name, value.clone());
                }
                overlay_scope
            });
            let scope = overlay_scope.as_ref().unwrap_or(scope);
            if let Value::Lambda(params, ..) = &base {
                return Ok(amended_function(params.len(), base, overlay_entries, scope));
            }
            return self.eval_value_amendment(base, overlay_entries, scope, depth);
        }

        // Logical `&&` and `||` short-circuit: the right operand must not be
        // evaluated when the left already determines the result (e.g.
        // `x is Foo && x.fooField` must not touch `fooField` when `x` is not a
        // `Foo`).
        if matches!(op, BinOp::And | BinOp::Or) {
            let l = self.eval_expr(left, scope, depth + 1)?;
            let left_value = stdlib::logical_left(op, &l)?;
            let short_circuit = match op {
                BinOp::And => !left_value,
                _ => left_value,
            };
            if short_circuit {
                return Ok(Value::Bool(left_value));
            }
            let r = self.eval_expr(right, scope, depth + 1)?;
            return stdlib::logical_right(op, &l, &r).map(Value::Bool);
        }

        let l = self.eval_expr(left, scope, depth + 1)?;
        let r = self.eval_expr(right, scope, depth + 1)?;
        match op {
            BinOp::Pipe => {
                // x |> f  is equivalent to  f(x)
                match r {
                    Value::Lambda(ref params, ..) => {
                        if params.len() != 1 {
                            return Err(Error::Eval(format!(
                                "pipe operator requires a single-parameter function, got {}",
                                params.len()
                            )));
                        }
                        self.apply_function(&r, &[l], scope, depth)
                    }
                    r => Err(Error::Eval(format!(
                        "Operator `|>` is not defined for operand types `{}` and `{}`. \
                         Left operand : {} Right operand: {}",
                        pkl_class_name(&l),
                        pkl_class_name(&r),
                        pkl_value_text(&l),
                        pkl_value_text(&r)
                    ))),
                }
            }
            _ => apply_binop(op, l, r),
        }
    }
}

/// The value of a literal or a plain name, or `None` for any expression that
/// needs the full evaluator. Must agree with `Evaluator::eval_expr_boxed`.
pub(super) fn eval_simple_expr(
    expr: &Expr,
    scope: &Scope,
    depth: usize,
    max_depth: usize,
) -> Option<Result<Value>> {
    if depth > max_depth {
        return None;
    }
    Some(match expr {
        Expr::Null => Ok(Value::Null),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Int(n) => Ok(Value::Int(*n)),
        Expr::Float(f) => Ok(Value::Float(*f)),
        Expr::String(s) => Ok(Value::String(Arc::clone(s))),
        Expr::Ident(name) => scope.get(name).cloned().ok_or_else(|| {
            Error::Eval(
                scope
                    .poison_of(name)
                    .cloned()
                    .unwrap_or_else(|| format!("undefined variable: {name}")),
            )
        }),
        // An operator over simple operands, other than `|>` (a call) and an
        // object-body amendment, which need the evaluator. `&&`, `||` keep
        // short-circuiting; the depth mirrors `eval_binop`'s `depth + 1`.
        Expr::Binop(op, left, right)
            if !(matches!(op, BinOp::Pipe)
                || matches!(op, BinOp::Add) && matches!(right.as_ref(), Expr::ObjectBody(_))) =>
        {
            if !is_simple_expr(left) || !is_simple_expr(right) {
                return None;
            }
            let l = match eval_simple_expr(left, scope, depth + 1, max_depth)? {
                Ok(l) => l,
                Err(error) => return Some(Err(error)),
            };
            if matches!(op, BinOp::And | BinOp::Or) {
                let left = match stdlib::logical_left(*op, &l) {
                    Ok(left) => left,
                    Err(error) => return Some(Err(error)),
                };
                let short_circuit = match op {
                    BinOp::And => !left,
                    _ => left,
                };
                if short_circuit {
                    return Some(Ok(Value::Bool(left)));
                }
                return Some(
                    eval_simple_expr(right, scope, depth + 1, max_depth)?
                        .and_then(|r| stdlib::logical_right(*op, &l, &r).map(Value::Bool)),
                );
            }
            let r = match eval_simple_expr(right, scope, depth + 1, max_depth)? {
                Ok(r) => r,
                Err(error) => return Some(Err(error)),
            };
            apply_binop(*op, l, r)
        }
        _ => return None,
    })
}

/// Whether `eval_simple_expr` can evaluate `expr` without the evaluator.
pub(super) fn is_simple_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Ident(_) => true,
        Expr::Binop(op, left, right) => {
            !(matches!(op, BinOp::Pipe)
                || matches!(op, BinOp::Add) && matches!(right.as_ref(), Expr::ObjectBody(_)))
                && is_simple_expr(left)
                && is_simple_expr(right)
        }
        _ => false,
    }
}

pub(super) fn member_predicates_unsupported() -> Error {
    Error::Eval("member predicates (`[[...]]`) are not supported".into())
}

/// Apply a binary operator other than `|>` to evaluated operands.
pub(super) fn apply_binop(op: BinOp, l: Value, r: Value) -> Result<Value> {
    if let Some(result) = stdlib::binary_op(op, &l, &r) {
        return result;
    }
    match op {
        BinOp::Add => add_values(l, r),
        BinOp::Sub => arithmetic(l, r, |a, b| Ok(a - b), |a, b| Ok(a - b)),
        BinOp::Mul => arithmetic(l, r, |a, b| Ok(a * b), |a, b| Ok(a * b)),
        BinOp::Div => arithmetic(
            l,
            r,
            |a, b| {
                if b == 0 {
                    Err(Error::Eval("division by zero".into()))
                } else {
                    Ok(a / b)
                }
            },
            |a, b| Ok(a / b),
        ),
        BinOp::Mod => arithmetic(
            l,
            r,
            |a, b| {
                if b == 0 {
                    Err(Error::Eval("modulo by zero".into()))
                } else {
                    Ok(a % b)
                }
            },
            |a, b| Ok(a % b),
        ),
        BinOp::Eq => Ok(Value::Bool(values_eq(&l, &r))),
        BinOp::Ne => Ok(Value::Bool(!values_eq(&l, &r))),
        BinOp::Lt => compare(l, r, std::cmp::Ordering::Less),
        BinOp::Le => compare_or_eq(l, r, std::cmp::Ordering::Less),
        BinOp::Gt => compare(l, r, std::cmp::Ordering::Greater),
        BinOp::Ge => compare_or_eq(l, r, std::cmp::Ordering::Greater),
        BinOp::And | BinOp::Or => {
            let left = stdlib::logical_left(op, &l)?;
            if left == matches!(op, BinOp::Or) {
                return Ok(Value::Bool(left));
            }
            stdlib::logical_right(op, &l, &r).map(Value::Bool)
        }
        BinOp::IntDiv => arithmetic(
            l,
            r,
            |a, b| {
                if b == 0 {
                    Err(Error::Eval("division by zero".into()))
                } else {
                    Ok(a / b)
                }
            },
            |a, b| Ok((a / b).floor()),
        ),
        BinOp::Pow => arithmetic(
            l,
            r,
            |a, b| {
                if b < 0 {
                    Err(Error::Eval(
                        "integer exponentiation with negative exponent is not supported".into(),
                    ))
                } else {
                    Ok(a.pow(b as u32))
                }
            },
            |a, b| Ok(a.powf(b)),
        ),
        BinOp::NullCoalesce => {
            if is_null_value(&l) {
                Ok(r)
            } else {
                Ok(l)
            }
        }
        BinOp::Pipe => unreachable!("`|>` calls a function; see eval_binop"),
    }
}

pub(super) fn regex_value(pattern: Value) -> Result<Value> {
    match pattern {
        Value::String(pattern) => Ok(Value::Regex(Arc::new(stdlib::compile_regex(&pattern)?))),
        other => Err(stdlib::type_mismatch("String", &other)),
    }
}

/// Whether `IntSeq` in `scope` is still the built-in, which `seed_builtins`
/// binds to a marker string, rather than a user binding of that name.
pub(super) fn int_seq_is_builtin(scope: &Scope) -> bool {
    matches!(scope.get("IntSeq"), Some(Value::String(name)) if &**name == "IntSeq")
}

/// Largest number of elements an `IntSeq` may produce. IntSeq is
/// materialized as a list, so an unbounded range would exhaust memory.
pub(super) const MAX_INT_SEQ_LEN: i128 = 1_000_000;

/// Materialize `IntSeq(start, end).step(step)` as a list of ints. The range is
/// inclusive of `end` when a step lands on it, and empty when `step` points
/// away from `end` (e.g. `IntSeq(5, 1)` with the default step of 1).
pub(super) fn int_seq(start: i64, end: i64, step: i64) -> Result<Value> {
    if step == 0 {
        return Err(Error::Eval("IntSeq step must not be 0".into()));
    }
    let (start_w, end_w, step_w) = (start as i128, end as i128, step as i128);
    let len = if (step > 0 && start <= end) || (step < 0 && start >= end) {
        (end_w - start_w) / step_w + 1
    } else {
        0
    };
    if len > MAX_INT_SEQ_LEN {
        return Err(Error::Eval(format!(
            "IntSeq({start}, {end}) with step {step} has {len} elements, more than the supported maximum of {MAX_INT_SEQ_LEN}"
        )));
    }
    Ok(Value::List(
        (0..len)
            .map(|i| Value::Int((start_w + i * step_w) as i64))
            .collect::<Vec<_>>()
            .into(),
    ))
}

/// The key/value pairs a for-generator iterates over.
pub(super) fn collection_to_items(v: Value) -> Result<Vec<(Value, Value)>> {
    check_iterable(&v)?;
    Ok(match v {
        Value::List(items) => items
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, v)| (Value::Int(i as i64), v))
            .collect(),
        Value::Object(map, _) => map
            .iter()
            .map(|(k, v)| (Value::String(k.clone()), v.clone()))
            .collect(),
        _ => vec![],
    })
}

/// A value's type as pkl names it in error messages: `module#Class` for a
/// class instance.
pub(super) fn value_type_display(value: &Value) -> String {
    match value {
        Value::Object(_, Some(source)) if source.type_name.is_some() => qualified_class_name(
            source.type_identity.as_deref(),
            source.type_name.as_deref().unwrap_or_default(),
        ),
        Value::Object(..) => "Dynamic".to_string(),
        value => value_type_name(value).to_string(),
    }
}

/// Reject a value a generator or spread can't iterate over: anything but a
/// collection, Listing, Mapping or Dynamic object. Instances of classes are
/// typed objects and can't be iterated either.
pub(super) fn check_iterable(value: &Value) -> Result<()> {
    match value {
        Value::List(_) => Ok(()),
        Value::Object(_, source) if source.as_ref().is_none_or(|s| s.type_name.is_none()) => Ok(()),
        value => Err(Error::Eval(format!(
            "Cannot iterate over value of type `{}`.",
            value_type_display(value)
        ))),
    }
}
