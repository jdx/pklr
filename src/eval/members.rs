//! Reading members of objects: `super`, keys, lengths and field bases.

use super::*;

impl Evaluator {
    pub(super) fn eval_super_member(
        &mut self,
        field: &str,
        scope: &Scope,
        depth: usize,
        property_access: bool,
    ) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        if let Some(Value::List(items)) = scope.get("super") {
            let mut length = scope.receiver_list_base.unwrap_or(items.len());
            if let Some(entries) = &scope.receiver_entries {
                self.eval_listing_length(entries, scope, depth + 1, &mut length)?;
            }
            return match field {
                "length" => Ok(Value::Int(length as i64)),
                "isEmpty" => Ok(Value::Bool(length == 0)),
                "isNotEmpty" => Ok(Value::Bool(length != 0)),
                "first" | "last" => {
                    if length == 0 {
                        return Err(Error::Eval(format!("{field} called on an empty Listing")));
                    }
                    let target = if field == "first" { 0 } else { length - 1 };
                    let mut value = items.get(target).cloned();
                    if let Some(entries) = &scope.receiver_entries {
                        let mut position = scope.receiver_list_base.unwrap_or(items.len());
                        self.eval_listing_member(
                            entries,
                            scope,
                            depth + 1,
                            target,
                            &mut position,
                            &mut value,
                            &[],
                        )?;
                    }
                    value.ok_or_else(|| {
                        Error::Eval(format!("listing index {target} is out of bounds"))
                    })
                }
                _ => Err(Error::Eval(format!("field not found: {field}"))),
            };
        }
        let Some(Value::Object(map, source)) = scope.get("super") else {
            return Err(Error::Eval("undefined variable: super".into()));
        };
        if let Some(source) = source {
            if let Some(message) = poisoned_member_error(Some(source.as_ref()), field) {
                return Err(Error::Eval(message));
            }
            let entry = source
                .entries
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, entry)| match entry {
                    Entry::Property(prop)
                        if &*prop.name == field
                            && !has_modifier(&prop.modifiers, Modifier::Local) =>
                    {
                        Some((index, prop))
                    }
                    _ => None,
                });
            if let Some((index, prop)) = entry {
                let definition = restore_scope(&capture_object_source_scope(source));
                let owners =
                    entry_scope_owners(&source.entries, Some(&source.entry_scopes), Some(source));
                let mut receiver = Scope {
                    receiver_entries: scope.receiver_entries.clone(),
                    ..Scope::default()
                };
                if let Some(Value::Object(members, _)) = scope.get("this") {
                    for (name, value) in members.iter() {
                        receiver.set(name, value.clone());
                    }
                }
                if let Some(this) = scope.get("this") {
                    receiver.set("this", this.clone());
                }
                let mut active = scope_for_object_entry(
                    index,
                    &receiver,
                    Some(&source.entry_scopes),
                    &owners,
                    Some((&definition, &source.body_members)),
                );
                // Reconstruct a body amendment from its own parent, rather
                // than reapplying it to the already-amended cached member.
                if prop.body.is_some() {
                    let has_parent_member = matches!(active.get("super"),
                        Some(Value::Object(parent, _)) if parent.contains_key(field));
                    let inherited = if has_parent_member {
                        self.eval_super_member(field, &active, depth + 1, true)?
                    } else {
                        Value::Null
                    };
                    active.set(field, inherited);
                }
                if let Some(value) = self.eval_property(prop, &active, depth + 1)? {
                    return Ok(value);
                }
            }
        }
        match field {
            "length" | "keys" | "isEmpty" | "isNotEmpty"
                if property_access
                    && source
                        .as_ref()
                        .is_none_or(|source| source.type_name.is_none()) =>
            {
                let mut keys = IndexMap::new();
                if source.as_ref().is_some_and(|source| source.is_parsed_json) {
                    keys.extend(map.keys().map(|key| (key.clone(), ())));
                } else if let Some(entries) = &scope.receiver_entries {
                    self.eval_receiver_keys(entries, scope, depth + 1, &mut keys)?;
                } else {
                    keys.extend(map.keys().map(|key| (key.clone(), ())));
                }
                return Ok(match field {
                    "length" => Value::Int(keys.len() as i64),
                    "keys" => Value::List(ListValue::new(
                        ListKind::Set,
                        keys.into_keys()
                            .map(|key| crate::value::mapping_storage_value(&key))
                            .collect::<Vec<_>>(),
                    )),
                    "isEmpty" => Value::Bool(keys.is_empty()),
                    _ => Value::Bool(!keys.is_empty()),
                });
            }
            _ => {}
        }
        if let Some(value) = map.get(field) {
            return Ok(value.clone());
        }
        if field == "text"
            && source
                .as_ref()
                .is_some_and(|source| source.type_name.as_deref() == Some("ModuleOutput"))
        {
            let context = scope
                .get(MODULE_OUTPUT_TEXT_CONTEXT)
                .ok_or_else(|| Error::Eval("missing ModuleOutput text context".into()))?;
            return self.module_output_text(context, depth + 1);
        }
        // An untyped object's prototype supplies an empty Dynamic default
        // for a newly declared property, never a member of an outer object.
        if property_access
            && source
                .as_ref()
                .is_none_or(|source| source.type_name.is_none())
        {
            return Ok(Value::Object(Arc::default(), None));
        }
        Err(Error::Eval(format!("field not found: {field}")))
    }

    /// Enumerate receiver members without evaluating their values. Mapping
    /// metadata such as super.length includes entries after the current one.
    pub(super) fn eval_receiver_keys(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        keys: &mut IndexMap<Arc<str>, ()>,
    ) -> Result<()> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        let scope = scope.child();
        for entry in entries {
            match entry {
                // Locals are values, not members. Evaluating a local that
                // reads super.length while counting would re-enter this walk.
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Property(prop)
                    if &*prop.name != "default"
                        && !has_modifier(&prop.modifiers, Modifier::Hidden) =>
                {
                    keys.insert(prop.name.as_ref().into(), ());
                }
                Entry::DynProperty(key, _) => {
                    let key = self.eval_expr(key, &scope, depth + 1)?;
                    keys.insert(value_to_key(&key)?, ());
                }
                Entry::Spread(expr) => {
                    let value = self.eval_expr(expr, &scope, depth + 1)?;
                    check_iterable(&value)?;
                    if let Value::Object(map, _) = value {
                        keys.extend(map.keys().map(|key| (key.clone(), ())));
                    }
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection)? {
                        let mut iter = scope.child();
                        iter.set(&generator.val_var, value);
                        if let Some(name) = &generator.key_var {
                            iter.set(name, key);
                        }
                        self.eval_receiver_keys(&generator.body, &iter, depth + 1, keys)?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition = self.eval_expr(&generator.condition, &scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(body) = selected {
                        self.eval_receiver_keys(body, &scope, depth + 1, keys)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Count listing members without evaluating element values, so metadata
    /// can be read by an element itself without recursing into that element.
    pub(super) fn eval_listing_length(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
        length: &mut usize,
    ) -> Result<()> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        let scope = scope.child();
        for entry in entries {
            match entry {
                Entry::Elem(_) => *length += 1,
                Entry::DynProperty(index, _) => {
                    let index = self.eval_expr(index, &scope, depth + 1)?;
                    if let Value::Int(index) = index
                        && usize::try_from(index).ok() == Some(*length)
                    {
                        *length += 1;
                    }
                }
                // Locals are values, not members. Evaluating a local that
                // reads super.length while counting would re-enter this walk.
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Spread(expr) => {
                    let value = self.eval_expr(expr, &scope, depth + 1)?;
                    *length += listing_spread_values(value)?.len();
                }
                Entry::ForGenerator(generator) => {
                    let collection = self.eval_expr(&generator.collection, &scope, depth + 1)?;
                    for (key, value) in collection_to_items(collection)? {
                        let mut iter = scope.child();
                        iter.set(&generator.val_var, value);
                        if let Some(name) = &generator.key_var {
                            iter.set(name, key);
                        }
                        self.eval_listing_length(&generator.body, &iter, depth + 1, length)?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition = self.eval_expr(&generator.condition, &scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(body) = selected {
                        self.eval_listing_length(body, &scope, depth + 1, length)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Evaluate the object an `expr.field` / `expr?.field` access reads from.
    ///
    /// An `import("uri")` base is narrowed to the single field being read, so
    /// `import("mod.pkl").field` leaves the module's other properties
    /// unevaluated just as `import "mod.pkl" as Mod` + `Mod.field` does.
    pub(super) fn eval_field_base(
        &mut self,
        obj_expr: &Expr,
        field: &str,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        if let Expr::Import(uri, module_path) = obj_expr {
            let requested = HashSet::from_iter([nm(field)]);
            return self.eval_import_expr(uri, Path::new(module_path), depth, Some(requested));
        }
        self.eval_expr(obj_expr, scope, depth + 1)
    }
}

/// The error of the first failed member rendering `value` would output: a
/// failed property of a module object it contains.
pub(super) fn rendered_member_failure(value: &Value) -> Option<&str> {
    match value {
        Value::Object(map, source) => source
            .as_ref()
            .and_then(|source| source.poisoned_members.as_ref())
            .and_then(|members| members.values().find(|member| member.rendered))
            .map(|member| member.message.as_str())
            .or_else(|| map.values().find_map(rendered_member_failure)),
        Value::List(items) => items.iter().find_map(rendered_member_failure),
        _ => None,
    }
}

/// Return a deferred member error when that exact member is read.
pub(super) fn poisoned_member_error(source: Option<&ObjectSource>, field: &str) -> Option<String> {
    source
        .and_then(|source| source.poisoned_members.as_ref())
        .and_then(|members| members.get(field))
        .map(|member| member.message.clone())
}

/// Whether `prop` reads the module's `super`, rather than an object's own
/// `super` in a nested body. Inherited properties with a module-super read
/// retain the value computed in their defining module when a child overrides
/// other members.
pub(super) fn property_reads_module_super(prop: &Property) -> bool {
    prop.value.as_ref().is_some_and(expr_reads_module_super)
}

/// Check object members copied at runtime (by a spread or generator) against
/// immutable members of the receiver they amend. A temporary property models
/// the assignment performed by copying the value into the receiver.
pub(super) fn immutable_copied_member_error<'a>(
    names: impl IntoIterator<Item = &'a Arc<str>>,
    inherited: &ObjectSource,
) -> Option<String> {
    names.into_iter().find_map(|name| {
        inherited.entries.iter().find_map(|entry| match entry {
            Entry::Property(prop) if *prop.name == **name => immutable_member_error(
                &Property {
                    value: Some(Expr::Null),
                    modifiers: Vec::new(),
                    ..prop.as_ref().clone()
                },
                prop,
            ),
            _ => None,
        })
    })
}

pub(super) fn expr_reads_module_super(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(name) => name == "super",
        Expr::Field(value, _)
        | Expr::NullSafeField(value, _)
        | Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::TemplateLocals(_, value)
        | Expr::Trace(value, _)
        | Expr::Read(value, _)
        | Expr::ReadOrNull(value, _)
        | Expr::ReadGlob(value, _) => expr_reads_module_super(value),
        Expr::Index(left, right) | Expr::Binop(_, left, right) => {
            expr_reads_module_super(left) || expr_reads_module_super(right)
        }
        Expr::Call(callee, args) => {
            expr_reads_module_super(callee) || args.iter().any(expr_reads_module_super)
        }
        Expr::If(condition, then_expr, else_expr) => {
            expr_reads_module_super(condition)
                || expr_reads_module_super(then_expr)
                || expr_reads_module_super(else_expr)
        }
        Expr::Let(_, value, body) => {
            expr_reads_module_super(value) || expr_reads_module_super(body)
        }
        Expr::Is(value, _) | Expr::As(value, _) => expr_reads_module_super(value),
        Expr::Lambda(_, body) => expr_reads_module_super(body),
        Expr::StringInterpolation(parts) => parts.iter().any(
            |part| matches!(part, StringInterpPart::Expr(expr) if expr_reads_module_super(expr)),
        ),
        // Each of these owns a receiver, so its `super` is not the module's.
        Expr::New(..) | Expr::InferredNew(..) | Expr::ObjectBody(..) => false,
        Expr::Import(..)
        | Expr::ImportGlob(..)
        | Expr::Null
        | Expr::Bool(..)
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::String(..) => false,
    }
}

#[cfg(all(test, feature = "native-io"))]
mod super_deprecation_tests {
    #[test]
    fn super_property_access_warns_once() {
        let mut evaluator = super::Evaluator::default();
        let source = r#"
class Base {
  @Deprecated { message = "use replacement" }
  old: Int = 1
}
local base = new Base {}
result = (base) { old = super.old + super.old }
"#;
        let value = evaluator
            .eval_source(source, std::path::Path::new("super.pkl"))
            .unwrap();
        assert_eq!(value.to_json()["result"]["old"], 2);
        assert_eq!(evaluator.warned_deprecated.len(), 1);
        assert!(
            evaluator
                .warned_deprecated
                .contains(&("old".into(), Some("use replacement".into())))
        );
    }
}
