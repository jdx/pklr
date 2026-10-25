//! Output converters: `output.renderer.converters` applied to a rendered value.

use super::*;

impl Evaluator {
    /// Walk the AST of the `output` property to extract converter lambdas.
    /// Looks for the structure: `output { renderer { converters { [Type] = (x) -> ... } } }`.
    /// Converter keys are type identifiers (e.g., `[Regex]`), which we preserve as
    /// strings for matching against `ObjectSource.type_name`.
    pub(super) fn extract_converters_from_ast(
        &mut self,
        output_prop: &Property,
        scope: &Scope,
        depth: usize,
    ) {
        let Some(output_body) = &output_prop.body else {
            return;
        };
        let mut converter_scope = scope.child();
        for entry in output_body.iter() {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
                && let Ok(value) = self.eval_expr(expr, &converter_scope, depth)
            {
                converter_scope.set(&prop.name, value);
            }
        }
        // Find `renderer { converters { ... } }` inside output
        let Some(renderer_body) = output_body.iter().find_map(|entry| {
            if let Entry::Property(p) = entry
                && &*p.name == "renderer"
            {
                p.body.as_ref()
            } else {
                None
            }
        }) else {
            return;
        };
        for entry in renderer_body.iter() {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
                && let Ok(value) = self.eval_expr(expr, &converter_scope, depth)
            {
                converter_scope.set(&prop.name, value);
            }
        }
        let Some(converters_body) = renderer_body.iter().find_map(|entry| {
            if let Entry::Property(p) = entry
                && &*p.name == "converters"
            {
                p.body.as_ref()
            } else {
                None
            }
        }) else {
            return;
        };
        // Each converter is a DynProperty: [ClassName] = (x) -> expr
        for centry in converters_body.iter() {
            if let Entry::DynProperty(key_expr, val_expr) = centry {
                // Extract the class name from the key expression
                let class_name = match key_expr {
                    Expr::Ident(name) => name.clone(),
                    Expr::Field(_, name) => name.clone(),
                    Expr::String(s) => s.to_string(),
                    _ => continue,
                };
                // Evaluate the lambda value
                if let Ok(lambda) = self.eval_expr(val_expr, &converter_scope, depth)
                    && matches!(lambda, Value::Lambda(..))
                {
                    // Child output declarations override converters for the
                    // same class while retaining unrelated inherited ones.
                    self.converters.retain(|(name, _)| name != &class_name);
                    self.converters.push((class_name, lambda));
                }
            }
        }
    }

    /// Apply `output.renderer.converters` to a value tree.
    /// Walks recursively, replacing typed objects with their converter output.
    pub fn apply_converters(&mut self, value: Value) -> Result<Value> {
        if self.module_output.is_some() && self.output_has_scalar_converter()? {
            return self.apply_output_converters(value);
        }
        if self.converters.is_empty() {
            if self.module_output.is_some() {
                return self.apply_output_converters(value);
            }
            return Ok(value);
        }
        let converters = self.converters.clone();
        let mut memo = ConverterMemo::default();
        Ok(self
            .apply_converters_recursive(&value, &converters, Vec::new(), false, &mut memo)?
            .unwrap_or(value))
    }

    /// Apply converters to `value` and everything under it. `None` means no
    /// converter applied anywhere, so the caller can keep `value` (and share
    /// its maps) instead of rebuilding an identical copy.
    pub(super) fn apply_converters_recursive(
        &mut self,
        value: &Value,
        converters: &[(String, Value)],
        blocked_root_converters: Vec<(String, Value)>,
        blocked_at_root: bool,
        memo: &mut ConverterMemo,
    ) -> Result<Option<Value>> {
        // A shared object or list (one mapping referenced from several places,
        // like the same steps under every hook) converts the same way each
        // time, so it is converted once.
        let key = match value {
            Value::Object(map, src) if blocked_root_converters.is_empty() => Some((
                Arc::as_ptr(map) as usize,
                src.as_ref().map_or(0, |src| Arc::as_ptr(src) as usize),
            )),
            Value::List(items) if blocked_root_converters.is_empty() => {
                Some((items.items_ptr() as usize, items.kind() as usize))
            }
            _ => None,
        };
        if let Some(key) = key
            && let Some((_, converted)) = memo.get(&key)
        {
            return Ok(converted.clone());
        }
        let converted = self.apply_converters_uncached(
            value,
            converters,
            blocked_root_converters,
            blocked_at_root,
            memo,
        )?;
        if let Some(key) = key {
            // The original is kept so its address is not reused by another
            // value while the memo is alive.
            memo.insert(key, (value.clone(), converted.clone()));
        }
        Ok(converted)
    }

    pub(super) fn apply_converters_uncached(
        &mut self,
        value: &Value,
        converters: &[(String, Value)],
        blocked_root_converters: Vec<(String, Value)>,
        blocked_at_root: bool,
        memo: &mut ConverterMemo,
    ) -> Result<Option<Value>> {
        match value {
            Value::Object(map, src) => {
                // Check if this object has a type_name that matches a converter
                let type_names = src
                    .iter()
                    .flat_map(|source| {
                        source
                            .type_name
                            .iter()
                            .chain(source.parent_type_names.iter())
                    })
                    .collect::<Vec<_>>();

                if !type_names.is_empty() {
                    for type_name in type_names {
                        for (conv_name, lambda) in converters {
                            if type_names_match(conv_name, type_name)
                                && !(blocked_at_root
                                    && blocked_root_converters
                                        .iter()
                                        .any(|(blocked_name, _)| blocked_name == conv_name))
                                && !converter_is_blocked(&blocked_root_converters, conv_name, value)
                                && let Value::Lambda(params, body, captured) = lambda
                            {
                                if let Some(message) = self.lambda_guard_error(captured) {
                                    return Err(Error::Eval(message.to_string()));
                                }
                                let mut call_scope = Scope::for_call(captured);
                                // Bind the object as the first parameter
                                if let Some(param) = params.first() {
                                    call_scope.declare(param, value.clone());
                                }
                                let result = self.eval_expr(body, &call_scope, 0)?;
                                let mut blocked = blocked_root_converters;
                                blocked.push((conv_name.clone(), value.clone()));
                                let converted = self.apply_converters_recursive(
                                    &result, converters, blocked, true, memo,
                                )?;
                                return Ok(Some(converted.unwrap_or(result)));
                            }
                        }
                    }
                }

                // No converter matched — recurse into children
                let mut new_map: Option<ObjectMap> = None;
                for (index, (k, v)) in map.iter().enumerate() {
                    // Keep blocked roots while walking converter output. The
                    // block applies only to that exact value, so a distinct
                    // nested value of the same class can still convert.
                    let converted = self.apply_converters_recursive(
                        v,
                        converters,
                        blocked_root_converters.clone(),
                        false,
                        memo,
                    )?;
                    match (&mut new_map, converted) {
                        (Some(new_map), converted) => {
                            new_map.insert(k.clone(), converted.unwrap_or_else(|| v.clone()));
                        }
                        (None, Some(converted)) => {
                            let mut changed =
                                ObjectMap::with_capacity_and_hasher(map.len(), Default::default());
                            changed.extend(
                                map.iter().take(index).map(|(k, v)| (k.clone(), v.clone())),
                            );
                            changed.insert(k.clone(), converted);
                            new_map = Some(changed);
                        }
                        (None, None) => {}
                    }
                }
                Ok(new_map.map(|new_map| Value::Object(Arc::new(new_map), src.clone())))
            }
            Value::List(items) => {
                let mut new_items: Option<Vec<Value>> = None;
                for (index, item) in items.iter().enumerate() {
                    let converted = self.apply_converters_recursive(
                        item,
                        converters,
                        blocked_root_converters.clone(),
                        false,
                        memo,
                    )?;
                    match (&mut new_items, converted) {
                        (Some(new_items), converted) => {
                            new_items.push(converted.unwrap_or_else(|| item.clone()));
                        }
                        (None, Some(converted)) => {
                            let mut changed = Vec::with_capacity(items.len());
                            changed.extend_from_slice(&items[..index]);
                            changed.push(converted);
                            new_items = Some(changed);
                        }
                        (None, None) => {}
                    }
                }
                Ok(new_items.map(|new_items| Value::List(ListValue::new(items.kind(), new_items))))
            }
            // Built-in values such as a `Regex` match a converter by class name.
            Value::Regex(_) | Value::Duration(_) | Value::DataSize(_) => {
                let class_name = value.type_name();
                for (conv_name, lambda) in converters {
                    if type_names_match(conv_name, class_name)
                        && !(blocked_at_root
                            && blocked_root_converters
                                .iter()
                                .any(|(blocked_name, _)| blocked_name == conv_name))
                        && !converter_is_blocked(&blocked_root_converters, conv_name, value)
                    {
                        let result = self.invoke_lambda(lambda, std::slice::from_ref(value), 0)?;
                        let mut blocked = blocked_root_converters;
                        blocked.push((conv_name.clone(), value.clone()));
                        let converted = self
                            .apply_converters_recursive(&result, converters, blocked, true, memo)?;
                        return Ok(Some(converted.unwrap_or(result)));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

pub(super) fn converter_is_blocked(blocked: &[(String, Value)], name: &str, value: &Value) -> bool {
    blocked
        .iter()
        .any(|(blocked_name, root)| blocked_name == name && same_value_identity(root, value))
}

pub(super) fn same_value_identity(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a, a_source), Value::Object(b, b_source)) => {
            Arc::ptr_eq(a, b)
                && a_source
                    .as_ref()
                    .zip(b_source.as_ref())
                    .is_none_or(|(a, b)| Arc::ptr_eq(a, b))
        }
        (Value::List(a), Value::List(b)) => a.items_ptr() == b.items_ptr() && a.kind() == b.kind(),
        // Native scalar converter arguments are copied into a lambda, so they
        // have no allocation identity to compare. Their value identity is
        // sufficient here: only the converter root is blocked, while a
        // distinct nested object/list still follows its normal converter path.
        (Value::Duration(_) | Value::DataSize(_), Value::Duration(_) | Value::DataSize(_)) => {
            crate::value::mapping_storage_key(a) == crate::value::mapping_storage_key(b)
        }
        (Value::Regex(a), Value::Regex(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

/// Converted values by the address of the object or list they came from (see
/// `apply_converters_recursive`), each kept with its original.
pub(super) type ConverterMemo = HashMap<(usize, usize), (Value, Option<Value>)>;
