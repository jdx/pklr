//! Calling functions, methods and lambdas, and the builtin functions.

use super::*;

impl Evaluator {
    /// `getClass()` called without a receiver: a method of the enclosing
    /// object, `this`.
    #[inline(never)]
    fn eval_bare_get_class(&mut self, scope: &Scope, depth: usize) -> Result<Value> {
        if let Some(class) = scope.get(super::entries::THIS_CLASS) {
            return Ok(class.clone());
        }
        // A module body binds `this` to a source-less object that does not
        // name its class; the module does.
        let receiver = if matches!(scope.get("this"), Some(Value::Object(_, None)) | None) {
            "module"
        } else {
            "this"
        };
        let call = Expr::Field(Box::new(Expr::Ident(receiver.into())), "getClass".into());
        self.eval_call(&call, &[], scope, depth)
    }

    /// A call on a `Class` or on the `pkl:reflect` module, which neither
    /// resolves like a call on an ordinary object.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn special_receiver_call(
        &mut self,
        receiver: &Expr,
        obj: &Value,
        method: &str,
        arg_exprs: &[Expr],
        args: &[Value],
        scope: &Scope,
        depth: usize,
    ) -> Option<Box<Result<Value>>> {
        let Value::Object(_, Some(source)) = obj else {
            return None;
        };
        if source.kind != ObjectKind::Class && source.type_identity.is_none() {
            return None;
        }
        if super::reflect::is_module(obj) {
            return self
                .reflect_call(method, arg_exprs, args, scope, depth)
                .map(Box::new);
        }
        if is_class_method_miss(receiver, source, method) {
            return Some(Box::new(Err(class_method_error(method))));
        }
        None
    }

    pub(super) fn eval_call(
        &mut self,
        func_expr: &Expr,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // `IntSeq(start, end).step(n)`: IntSeq evaluates to a plain list, so
        // the step is applied while the range bounds are still known.
        if let Expr::Field(obj_expr, method) = func_expr
            && method == "step"
            && let Expr::Call(seq_func, seq_args) = obj_expr.as_ref()
            && matches!(seq_func.as_ref(), Expr::Ident(name) if name == "IntSeq")
            && int_seq_is_builtin(scope)
        {
            let (start, end) = self.eval_int_seq_bounds(seq_args, scope, depth)?;
            let [step_expr] = args else {
                return Err(Error::Eval(
                    "IntSeq.step() expects exactly one argument".into(),
                ));
            };
            let step = match self.eval_expr(step_expr, scope, depth + 1)? {
                Value::Int(n) => n,
                other => {
                    return Err(Error::Eval(format!(
                        "IntSeq.step() expects an Int, got {}",
                        value_type_name(&other)
                    )));
                }
            };
            return int_seq(start, end, step);
        }
        if let Expr::Field(obj_expr, method) = func_expr
            && method == "relativePathTo"
            && let [other] = args
            && let Some(result) = self.relative_path_to(obj_expr, other, scope)
        {
            // Both sides are modules that must exist, which evaluating them
            // checks.
            self.eval_expr(obj_expr, scope, depth + 1)?;
            self.eval_expr(other, scope, depth + 1)?;
            // Set after evaluating the receiver: that can load another
            // module, whose own members reset the flag.
            self.module_arg_used |= matches!(other, Expr::Ident(name) if name == "module");
            return result;
        }
        // Handle method calls: obj.method(args)
        if let Expr::Field(obj_expr, method) = func_expr {
            let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
            }
            if let Value::Lambda(..) = obj {
                match (method.as_str(), evaled_args.as_slice()) {
                    ("apply", _) => {
                        return self.apply_function(&obj, &evaled_args, scope, depth);
                    }
                    ("applyToList", _) => {
                        return self.apply_to_list(&obj, &evaled_args, scope, depth);
                    }
                    _ => {}
                }
            }
            if let Some(result) =
                self.special_receiver_call(obj_expr, &obj, method, args, &evaled_args, scope, depth)
            {
                return *result;
            }
            // The same for an element-only `default`, which is a list and has no
            // `apply` of its own.
            if method == "apply"
                && evaled_args.len() == 1
                && matches!(obj, Value::List(_))
                && is_default_member_ref(obj_expr)
            {
                return Ok(obj);
            }
            if let Some(result) = self.eval_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_renderer_method(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_field_call(&obj, method, &evaled_args)? {
                return Ok(result);
            }
            // A `default` that is an object stands for the function that
            // returns it for every key or index, unless the object has its own
            // `apply`.
            if method == "apply"
                && evaled_args.len() == 1
                && matches!(obj, Value::Object(..) | Value::List(_))
                && is_default_member_ref(obj_expr)
            {
                return Ok(obj);
            }
            if method == "getClass" && evaled_args.is_empty() {
                // The module's value names its class only here, where it is asked.
                if let (Expr::Ident(name), Value::Object(props, None)) = (obj_expr.as_ref(), &obj)
                    && name == "module"
                {
                    return stdlib::get_class(&self.module_with_class(props, scope));
                }
                return stdlib::get_class(&obj);
            }
            return Err(Error::Eval(format!(
                "unknown method '{method}' on {}",
                value_type_name(&obj)
            )));
        }
        // Handle null-safe method calls: obj?.method(args)
        if let Expr::NullSafeField(obj_expr, method) = func_expr {
            let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
            if is_null_value(&obj) {
                return Ok(Value::Null);
            }
            if matches!(&obj, Value::Object(_, Some(source))
                if is_class_method_miss(obj_expr, source, method))
            {
                return Err(class_method_error(method));
            }
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
            }
            if let Value::Lambda(..) = obj {
                match (method.as_str(), evaled_args.as_slice()) {
                    ("apply", _) => {
                        return self.apply_function(&obj, &evaled_args, scope, depth);
                    }
                    ("applyToList", _) => {
                        return self.apply_to_list(&obj, &evaled_args, scope, depth);
                    }
                    _ => {}
                }
            }
            if let Some(result) =
                self.special_receiver_call(obj_expr, &obj, method, args, &evaled_args, scope, depth)
            {
                return *result;
            }
            if let Some(result) = self.eval_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_renderer_method(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_method_call(&obj, method, &evaled_args, depth)? {
                return Ok(result);
            }
            if let Some(result) = self.eval_object_field_call(&obj, method, &evaled_args)? {
                return Ok(result);
            }
            if method == "getClass" && evaled_args.is_empty() {
                if let (Expr::Ident(name), Value::Object(props, None)) = (obj_expr.as_ref(), &obj)
                    && name == "module"
                {
                    return stdlib::get_class(&self.module_with_class(props, scope));
                }
                return stdlib::get_class(&obj);
            }
            return Err(Error::Eval(format!(
                "unknown method '{method}' on {}",
                value_type_name(&obj)
            )));
        }

        // Handle built-in functions: List(), Listing(), Map()
        if let Expr::Ident(name) = func_expr {
            match name.as_str() {
                "getClass" if args.is_empty() && scope.get("getClass").is_none() => {
                    return self.eval_bare_get_class(scope, depth);
                }
                "List" | "Listing" => {
                    let mut items = Vec::new();
                    for a in args {
                        items.push(self.eval_expr(a, scope, depth + 1)?);
                    }
                    return Ok(Value::List(items.into()));
                }
                "Set" => {
                    let mut items = Vec::new();
                    for a in args {
                        let val = self.eval_expr(a, scope, depth + 1)?;
                        // Hashing a member reads all of it.
                        self.raise_deep_failed(&val)?;
                        if !set_contains(&items, &val) {
                            items.push(val);
                        }
                    }
                    return Ok(Value::List(ListValue::new(ListKind::Set, items)));
                }
                "IntSeq" if int_seq_is_builtin(scope) => {
                    let (start, end) = self.eval_int_seq_bounds(args, scope, depth)?;
                    return int_seq(start, end, 1);
                }
                "Bytes" if scope.get("Bytes").is_none() => {
                    return self.eval_bytes_constructor(args, scope, depth);
                }
                "Pair" if pair_is_builtin(scope) => {
                    let [first, second] = args else {
                        return Err(Error::Eval("Pair() expects 2 arguments".into()));
                    };
                    let mut members = ObjectMap::default();
                    members.insert("first".into(), self.eval_expr(first, scope, depth + 1)?);
                    members.insert("second".into(), self.eval_expr(second, scope, depth + 1)?);
                    return Ok(stdlib::typed_object("Pair", members));
                }
                // `Null(default)` is a null value that carries a default object or
                // function for amending; pklr keeps only the null.
                "Null" if args.len() == 1 && scope.get("Null").is_none() => {
                    return self.null_default(&args[0], scope, depth);
                }
                "Regex" => {
                    if let Some(arg) = args.first() {
                        let val = self.eval_expr(arg, scope, depth + 1)?;
                        return regex_value(val);
                    }
                    return Err(Error::Eval("Regex() requires a pattern argument".into()));
                }
                TEST_CATCH | TEST_CATCH_OR_NULL => {
                    let [fun] = args else {
                        return Err(Error::Eval("catch() expects one argument".into()));
                    };
                    let fun = self.eval_expr(fun, scope, depth + 1)?;
                    return match self.apply_function(&fun, &[], scope, depth) {
                        Err(error) if is_evaluator_control_error(&error) => Err(error),
                        Err(error) => Ok(Value::String(caught_error_message(error).into())),
                        Ok(_) if name == TEST_CATCH => Err(Error::Eval(
                            "Expected an exception, but none was thrown.".into(),
                        )),
                        Ok(_) => Ok(Value::Null),
                    };
                }
                "Map" => {
                    // Map(k1, v1, k2, v2, ...)
                    let mut map = ObjectMap::default();
                    let mut evaled = Vec::new();
                    for a in args {
                        evaled.push(self.eval_expr(a, scope, depth + 1)?);
                    }
                    for pair in evaled.chunks(2) {
                        if let [k, v] = pair {
                            self.raise_deep_failed(k)?;
                            let key = value_to_key(k)?;
                            let storage_key =
                                equivalent_mapping_key(&map, &key).unwrap_or_else(|| key.clone());
                            self.remember_mapping_key(storage_key.clone(), k);
                            map.insert(storage_key, v.clone().into_entry_value());
                        }
                    }
                    return Ok(Value::Object(Arc::new(map), None));
                }
                _ => {}
            }
        }

        // Evaluate the function expression
        let func_val = self.eval_expr(func_expr, scope, depth + 1)?;

        // Lambda call
        if let Value::Lambda(params, body, captured) = func_val {
            let mut call_scope = Scope::for_call(&captured);
            // If we're inside a method call context (scope has `this` as an Object),
            // layer the instance's properties so local functions see overridden values
            if let Some(this @ Value::Object(this_map, _)) = scope.get("this") {
                for (k, v) in this_map.iter() {
                    call_scope.set(k, v.clone());
                }
                rebind_module_this(&mut call_scope, this, &captured);
            }
            // Bind arguments to parameters
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
            }
            if params.len() != evaled_args.len() {
                return Err(Error::Eval(format!(
                    "Expected {} function arguments but got {}.",
                    params.len(),
                    evaled_args.len()
                )));
            }
            if let Some(message) = self.lambda_guard_error(&captured) {
                return Err(Error::Eval(message.to_string()));
            }
            for (param, arg) in params.iter().zip(evaled_args) {
                call_scope.declare(param, arg);
            }
            return self.eval_expr(&body, &call_scope, depth + 1);
        }

        // Built-in type constructors resolved from scope (e.g. base.Regex)
        if let Value::String(ref name) = func_val
            && &**name == "Regex"
            && let Some(arg) = args.first()
        {
            let val = self.eval_expr(arg, scope, depth + 1)?;
            return regex_value(val);
        }

        // A property holding a number, boolean or null is no function.
        if let Expr::Ident(name) = func_expr
            && matches!(
                func_val,
                Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Null
            )
        {
            return Err(Error::Eval(format!("Cannot find method `{name}`.")));
        }
        // Plain call with no args on an object — return the object
        if args.is_empty() {
            return Ok(func_val);
        }
        Err(Error::Eval("cannot call non-function".into()))
    }

    pub(super) fn eval_object_field_call(
        &mut self,
        obj: &Value,
        method: &str,
        evaled_args: &[Value],
    ) -> Result<Option<Value>> {
        if let Value::Object(map, source) = obj
            && let Some(func_val) = map.get(method).cloned()
        {
            self.warn_if_deprecated_access(source, method);
            if let Value::String(ref name) = func_val
                && &**name == "Regex"
                && let Some(arg) = evaled_args.first()
            {
                return Ok(Some(regex_value(arg.clone())?));
            }
            if evaled_args.is_empty() {
                return Ok(Some(func_val));
            }
            return Err(Error::Eval("cannot call non-function".into()));
        }
        Ok(None)
    }

    pub(super) fn eval_object_method_call(
        &mut self,
        obj: &Value,
        method: &str,
        evaled_args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Value::Object(map, _) = obj
            && let Some(Value::Lambda(params, body, captured)) = map.get(method)
        {
            if params.len() != evaled_args.len() {
                return Err(Error::Eval(format!(
                    "Expected {} function arguments but got {}.",
                    params.len(),
                    evaled_args.len()
                )));
            }
            if let Some(message) = self.lambda_guard_error(captured) {
                return Err(Error::Eval(message.to_string()));
            }
            let mut call_scope = Scope::for_call(captured);
            // Layer in all instance properties, including lambdas, so local
            // functions called by this method see overrides.
            for (k, v) in map.iter() {
                call_scope.set(k, v.clone());
            }
            call_scope.set("this", obj.clone());
            for (i, param) in params.iter().enumerate() {
                if let Some(arg) = evaled_args.get(i) {
                    call_scope.declare(param, arg.clone());
                }
            }
            return Ok(Some(self.eval_expr(body, &call_scope, depth + 1)?));
        }
        Ok(None)
    }

    pub(super) fn eval_method_call(
        &mut self,
        obj: &Value,
        method: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        self.raise_failed_members(obj, method)?;
        if let Some(result) = self.stdlib_method(obj, method, args, depth) {
            return result.map(Some);
        }
        // An element-only `Dynamic` is an object, not a collection: it has only
        // the methods of `Dynamic`, not the `List` ones its storage could answer.
        if let Value::List(items) = obj
            && items.kind() == ListKind::Dynamic
        {
            return dynamic_elements_method(obj, items, method, args);
        }
        match (obj, method) {
            // String methods
            (Value::String(s), "contains") => {
                let arg = require_str_arg(args, 0, "contains")?;
                Ok(Some(Value::Bool(s.contains(arg))))
            }
            (Value::String(s), "startsWith") => {
                let arg = require_str_arg(args, 0, "startsWith")?;
                Ok(Some(Value::Bool(s.starts_with(arg))))
            }
            (Value::String(s), "endsWith") => {
                let arg = require_str_arg(args, 0, "endsWith")?;
                Ok(Some(Value::Bool(s.ends_with(arg))))
            }
            (Value::String(s), "replaceLast") => {
                let from = require_str_arg(args, 0, "replaceLast")?;
                let to = require_str_arg(args, 1, "replaceLast")?;
                let mut result = s.to_string();
                if let Some(start) = s.rfind(from) {
                    result.replace_range(start..start + from.len(), to);
                }
                Ok(Some(Value::String(result.into())))
            }
            (Value::String(s), "replaceAll") => {
                let from = require_str_arg(args, 0, "replaceAll")?;
                let to = require_str_arg(args, 1, "replaceAll")?;
                Ok(Some(Value::String(s.replace(from, to).into())))
            }
            (Value::String(s), "split") => {
                let sep = require_str_arg(args, 0, "split")?;
                Ok(Some(Value::List(
                    s.split(sep)
                        .map(|p| Value::String(p.into()))
                        .collect::<Vec<_>>()
                        .into(),
                )))
            }
            (Value::String(s), "trim") => Ok(Some(Value::String(s.trim().into()))),
            (Value::String(s), "trimStart") => Ok(Some(Value::String(s.trim_start().into()))),
            (Value::String(s), "trimEnd") => Ok(Some(Value::String(s.trim_end().into()))),
            (Value::String(s), "toUpperCase") => Ok(Some(Value::String(s.to_uppercase().into()))),
            (Value::String(s), "toLowerCase") => Ok(Some(Value::String(s.to_lowercase().into()))),
            (Value::String(s), "toInt") => s
                .parse::<i64>()
                .map(|n| Some(Value::Int(n)))
                .map_err(|_| Error::Eval(format!("cannot convert '{s}' to Int"))),
            (Value::String(s), "toBoolean") => match s.to_ascii_lowercase().as_str() {
                "true" => Ok(Some(Value::Bool(true))),
                "false" => Ok(Some(Value::Bool(false))),
                _ => Err(Error::Eval(format!("cannot convert '{s}' to Boolean"))),
            },

            // List methods
            (Value::List(items), "contains") => {
                let arg = args.first().cloned().unwrap_or(Value::Null);
                Ok(Some(Value::Bool(set_contains(items, &arg))))
            }
            (Value::List(items), "add")
                if matches!(items.kind(), ListKind::List | ListKind::Set) && args.len() == 1 =>
            {
                let mut elements: Vec<Value> = items.iter().cloned().collect();
                if items.kind() != ListKind::Set || !set_contains(&elements, &args[0]) {
                    elements.push(args[0].clone());
                }
                Ok(Some(Value::List(ListValue::new(items.kind(), elements))))
            }
            (Value::List(items), "toList") => {
                Ok(Some(Value::List(items.clone().with_kind(ListKind::List))))
            }
            (Value::List(items), "toListing")
                if matches!(items.kind(), ListKind::List | ListKind::Set) && args.is_empty() =>
            {
                Ok(Some(Value::List(
                    items.clone().with_kind(ListKind::Listing),
                )))
            }
            (Value::List(items), "toSet") => {
                if items.kind() == ListKind::Set {
                    return Ok(Some(obj.clone()));
                }
                let mut seen = Vec::new();
                for item in items.iter() {
                    if !set_contains(&seen, item) {
                        seen.push(item.clone());
                    }
                }
                Ok(Some(Value::List(ListValue::new(ListKind::Set, seen))))
            }
            (Value::List(items), "toMap")
                if matches!(items.kind(), ListKind::List | ListKind::Set)
                    && !items.is_bytes()
                    && !items.is_int_seq()
                    && args.len() == 2 =>
            {
                self.list_to_map(items, args, depth).map(Some)
            }
            (Value::List(items), "map") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("map requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items.iter() {
                    result.push(self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?);
                }
                Ok(Some(collection_result(items.kind(), result)))
            }
            (Value::List(items), "flatMap") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("flatMap requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items.iter() {
                    let val = self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?;
                    if let Value::List(inner) = val {
                        result.extend(inner.iter().cloned());
                    } else {
                        result.push(val);
                    }
                }
                Ok(Some(collection_result(items.kind(), result)))
            }
            (Value::List(items), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items.iter() {
                    let cond = self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?;
                    if is_truthy(&cond) {
                        result.push(item.clone());
                    }
                }
                Ok(Some(collection_result(items.kind(), result)))
            }
            (Value::List(items), "filterNonNull") => Ok(Some(collection_result(
                items.kind(),
                items
                    .iter()
                    .filter(|item| !is_null_value(item))
                    .cloned()
                    .collect(),
            ))),
            (Value::List(items), "fold") => {
                let init = args
                    .first()
                    .ok_or_else(|| Error::Eval("fold requires initial value".into()))?
                    .clone();
                let lambda = args
                    .get(1)
                    .ok_or_else(|| Error::Eval("fold requires a function argument".into()))?;
                let mut acc = init;
                for item in items.iter() {
                    acc = self.invoke_lambda(lambda, &[acc, item.clone()], depth)?;
                }
                Ok(Some(acc))
            }
            (Value::List(items), "any") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("any requires a function argument".into()))?;
                for item in items.iter() {
                    if is_truthy(&self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?) {
                        return Ok(Some(Value::Bool(true)));
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            (Value::List(items), "every") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("every requires a function argument".into()))?;
                for item in items.iter() {
                    if !is_truthy(&self.invoke_lambda(lambda, std::slice::from_ref(item), depth)?) {
                        return Ok(Some(Value::Bool(false)));
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            (Value::List(items), "join") => {
                let sep = args.first().and_then(|v| v.as_str()).unwrap_or(",");
                let s: Vec<String> = items.iter().map(value_to_display).collect();
                Ok(Some(Value::String(s.join(sep).into())))
            }
            (Value::List(items), "reverse") => {
                let mut rev = items.clone();
                rev.make_mut().reverse();
                Ok(Some(Value::List(rev)))
            }

            // `Listing.getOrDefault`: the element, or what `default` makes.
            (Value::List(items), "getOrDefault")
                if items.kind() == ListKind::Listing && args.len() == 1 =>
            {
                let Value::Int(index) = &args[0] else {
                    return Err(Error::Eval(format!(
                        "Expected value of type `Int`, but got type `{}`.",
                        value_type_name(&args[0])
                    )));
                };
                if let Some(item) = usize::try_from(*index).ok().and_then(|i| items.get(i)) {
                    return Ok(Some(item.clone()));
                }
                let default = items
                    .layers()
                    .and_then(|layers| layers.default.clone())
                    .unwrap_or_else(|| implicit_listing_default(items.elem_class()));
                Ok(Some(self.invoke_lambda(&default, &args[..1], depth)?))
            }

            // Object/Mapping methods
            (Value::Object(map, Some(source)), "getOrDefault")
                if source.kind == ObjectKind::Mapping && args.len() == 1 =>
            {
                let key = value_to_key(&args[0])?;
                if let Some(stored) = equivalent_mapping_key(map, &key)
                    && let Some(value) = map.get(&stored)
                {
                    return Ok(Some(value.clone()));
                }
                let mut scope = Scope::for_call(&Arc::new(source.scope().clone()));
                // A body default `default { key -> ... }` amends the default it
                // replaces through `super.default`; none is left to amend here.
                scope.set("super", Value::Object(Arc::default(), None));
                // The default may read the mapping's own locals.
                for entry in source.entries.iter() {
                    if let Entry::Property(prop) = entry
                        && has_modifier(&prop.modifiers, Modifier::Local)
                        && let Some(expr) = &prop.value
                    {
                        let value = self.eval_expr(expr, &scope, depth + 1)?;
                        scope.declare(&prop.name, value);
                    }
                }
                let Some(default) = self.find_default_template(&source.entries, &scope, depth)?
                else {
                    return Err(Error::Eval(format!(
                        "Tried to read missing entry with key `{}` of `Mapping`, which has no default.",
                        value_to_display(&args[0])
                    )));
                };
                if let Value::Lambda(..) = default {
                    return Ok(Some(self.apply_function(&default, args, &scope, depth)?));
                }
                Ok(Some(default))
            }
            (Value::Object(map, _), "containsKey") => {
                let key = args.first().and_then(|v| v.as_str()).unwrap_or("");
                Ok(Some(Value::Bool(map.contains_key(key))))
            }
            (Value::Object(map, source), "toMap") => Ok(Some(Value::Object(
                without_class_members(map, source.as_deref()),
                None,
            ))),
            // `Map.toMapping()` is a `Mapping`, which can be amended.
            (Value::Object(map, source), "toMapping") => Ok(Some(Value::Object(
                without_class_members(map, source.as_deref()),
                Some(Arc::new(super::mapping::mapping_source())),
            ))),
            (Value::Object(map, _), "mapValues") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("mapValues requires a function".into()))?;
                let mut result = ObjectMap::default();
                for (k, v) in map.iter() {
                    let new_v =
                        self.invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)?;
                    result.insert(k.clone(), new_v.into_entry_value());
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
            }
            (Value::Object(map, None), name @ ("every" | "any")) => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval(format!("{name} requires a function")))?;
                let wanted = name == "any";
                for (k, v) in map.iter() {
                    let key = self.entry_key_value(k);
                    let result = self.invoke_lambda(lambda, &[key, v.clone()], depth)?;
                    if is_truthy(&result) == wanted {
                        return Ok(Some(Value::Bool(wanted)));
                    }
                }
                Ok(Some(Value::Bool(!wanted)))
            }
            (Value::Object(map, _), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function".into()))?;
                let mut result = ObjectMap::default();
                for (k, v) in map.iter() {
                    let keep =
                        self.invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)?;
                    if is_truthy(&keep) {
                        result.insert(k.clone(), v.clone().into_entry_value());
                    }
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
            }
            // A `Dynamic`'s elements, which it has besides its members.
            (Value::Object(_, Some(source)), "toList")
                if source.kind.dynamic_as_object() == ObjectKind::Object
                    && source.type_name.is_none() =>
            {
                Ok(Some(Value::List(source.elements.clone().into())))
            }
            (Value::Object(..), "toList") => Ok(Some(obj.clone())),
            // `Dynamic` and `Mapping` have no `toDynamic()`; it is a method of
            // `Typed` (class instances, modules) and `Map`.
            (Value::Object(_, Some(source)), "toDynamic")
                if source.kind == ObjectKind::Mapping
                    || (source.kind != ObjectKind::Class
                        && !source.is_module
                        && source.type_name.is_none()
                        && source.type_identity.is_none()) =>
            {
                Ok(None)
            }
            (Value::Object(map, source), "toDynamic") => {
                let members = without_class_members(map, source.as_deref());
                Ok(Some(stdlib::dynamic_object(members, |key| {
                    self.mapping_key_values
                        .get(key)
                        .cloned()
                        .unwrap_or_else(|| crate::value::mapping_storage_value(key))
                })))
            }

            // Int/Float methods
            (Value::Int(n), "toString") => Ok(Some(Value::String(n.to_string().into()))),
            (Value::Float(f), "toString") => Ok(Some(Value::String(f.to_string().into()))),
            (Value::Bool(b), "toString") => Ok(Some(Value::String(b.to_string().into()))),

            // Lambda.apply()
            (Value::Lambda(params, body, captured), "apply") => {
                if params.len() != args.len() {
                    return Err(Error::Eval(format!(
                        "Expected {} function arguments but got {}.",
                        params.len(),
                        args.len()
                    )));
                }
                if let Some(message) = self.lambda_guard_error(captured) {
                    return Err(Error::Eval(message.to_string()));
                }
                let mut call_scope = Scope::for_call(captured);
                for (param, arg) in params.iter().zip(args.iter()) {
                    call_scope.declare(param, arg.clone());
                }
                Ok(Some(self.eval_expr(body, &call_scope, depth + 1)?))
            }
            (Value::Lambda(..), "toString") => Ok(Some(Value::String(pkl_value_text(obj).into()))),

            _ => Ok(None), // not a known method
        }
    }

    /// Call `function` with `args` on behalf of code evaluated in `caller`.
    /// Like a direct call, the function sees the members of the caller's
    /// `this` it did not capture: a module-level function created before the
    /// module's properties were evaluated reads them from there.
    pub(super) fn apply_function(
        &mut self,
        function: &Value,
        args: &[Value],
        caller: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let Value::Lambda(params, body, captured) = function else {
            return Err(Error::Eval("expected a function".into()));
        };
        if params.len() != args.len() {
            return Err(Error::Eval(format!(
                "Expected {} function arguments but got {}.",
                params.len(),
                args.len()
            )));
        }
        if let Some(message) = self.lambda_guard_error(captured) {
            return Err(Error::Eval(message.to_string()));
        }
        let mut call_scope = Scope::for_call(captured);
        if let Some(Value::Object(this_map, _)) = caller.get("this") {
            for (k, v) in this_map.iter() {
                if call_scope.get(k).is_none() {
                    call_scope.set(k, v.clone());
                }
            }
        }
        for (param, arg) in params.iter().zip(args) {
            call_scope.declare(param, arg.clone());
        }
        self.eval_expr(body, &call_scope, depth + 1)
    }

    /// `super.method(args)`: the inherited method runs on the receiver, so
    /// what it reads is late bound rather than fixed to the parent's defaults.
    /// `None` when `super` has no such method to run that way.
    #[inline(never)]
    pub(super) fn eval_super_or_call(
        &mut self,
        func_expr: &Expr,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        if let Expr::Field(obj_expr, method) = func_expr
            && matches!(obj_expr.as_ref(), Expr::Ident(name) if name == "super")
            && let Some(result) = self.eval_super_call(method, args, scope, depth)?
        {
            return Ok(*result);
        }
        self.eval_call(func_expr, args, scope, depth)
    }

    #[inline(never)]
    fn eval_super_call(
        &mut self,
        method: &str,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Box<Value>>> {
        let Some(Value::Object(map, source)) = scope.get("super") else {
            return Ok(None);
        };
        let function = match source {
            Some(source) => {
                let declared = source.entries.iter().any(|entry| {
                    matches!(entry, Entry::Property(prop)
                        if &*prop.name == method
                            && !has_modifier(&prop.modifiers, Modifier::Local)
                            && matches!(prop.value, Some(Expr::Lambda(..))))
                });
                if !declared {
                    return Ok(None);
                }
                let function = self.eval_super_member(method, scope, depth + 1, false)?;
                if !matches!(function, Value::Lambda(..)) {
                    return Ok(None);
                }
                function
            }
            // A module's `super` is a plain view of its base's members.
            None => {
                if !matches!(scope.get("this"), Some(Value::Object(_, None)))
                    && (map.is_empty() || scope.get(SUPER_MODULE_NAME).is_none())
                {
                    return Ok(None);
                }
                match map.get(method) {
                    Some(function @ Value::Lambda(..)) => function.clone(),
                    Some(_) => return Ok(None),
                    None => {
                        let Some(Value::String(module)) = scope.get(SUPER_MODULE_NAME) else {
                            return Ok(None);
                        };
                        return Err(Error::Eval(format!(
                            "Cannot find method `{method}` in module `{module}`."
                        )));
                    }
                }
            }
        };
        let mut evaled_args = Vec::new();
        for a in args {
            evaled_args.push(self.eval_expr(a, scope, depth + 1)?);
        }
        if source.is_some() {
            self.apply_function(&function, &evaled_args, scope, depth)
        } else {
            self.apply_late_bound(&function, &evaled_args, scope, depth)
        }
        .map(|v| Some(Box::new(v)))
    }

    /// Call a module's function on the module of `caller` (its `this`), whose
    /// members replace the ones the function captured.
    fn apply_late_bound(
        &mut self,
        function: &Value,
        args: &[Value],
        caller: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let Value::Lambda(params, body, captured) = function else {
            return Err(Error::Eval("expected a function".into()));
        };
        if params.len() != args.len() {
            return Err(Error::Eval(format!(
                "Expected {} function arguments but got {}.",
                params.len(),
                args.len()
            )));
        }
        if let Some(message) = self.lambda_guard_error(captured) {
            return Err(Error::Eval(message.to_string()));
        }
        let mut call_scope = Scope::for_call(captured);
        if let Some(this @ Value::Object(this_map, _)) = caller.get("this") {
            // Only the base's members late bind: a `local` of the declaring
            // module is lexical, and a same-named child member leaves it be.
            let base = match caller.get("super") {
                Some(Value::Object(base, _)) => Some(base),
                _ => None,
            };
            for (k, v) in this_map.iter() {
                if base.is_none_or(|base| base.contains_key(k)) {
                    call_scope.set(k, v.clone());
                }
            }
            call_scope.set("this", this.clone());
        }
        for (param, arg) in params.iter().zip(args) {
            call_scope.declare(param, arg.clone());
        }
        self.eval_expr(body, &call_scope, depth + 1)
    }

    /// `Function.applyToList` accepts a `List`, but not the distinct `Listing`
    /// and `Set` collection kinds.
    pub(super) fn apply_to_list(
        &mut self,
        function: &Value,
        args: &[Value],
        caller: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let [arg] = args else {
            return Err(Error::Eval(format!(
                "Expected 1 function arguments but got {}.",
                args.len()
            )));
        };
        let Value::List(items) = arg else {
            return Err(Error::Eval(format!(
                "Expected value of type `List`, but got type `{}`.",
                pkl_class_name(arg)
            )));
        };
        if items.kind() != ListKind::List {
            return Err(Error::Eval(format!(
                "Expected value of type `List`, but got type `{}`.",
                pkl_class_name(arg)
            )));
        }
        self.apply_function(function, items, caller, depth)
    }

    pub(super) fn invoke_lambda(
        &mut self,
        lambda: &Value,
        args: &[Value],
        depth: usize,
    ) -> Result<Value> {
        if let Value::Lambda(params, body, captured) = lambda {
            if params.len() != args.len() {
                return Err(Error::Eval(format!(
                    "Expected {} function arguments but got {}.",
                    params.len(),
                    args.len()
                )));
            }
            if let Some(message) = self.lambda_guard_error(captured) {
                return Err(Error::Eval(message.to_string()));
            }
            let mut scope = Scope::for_call(captured);
            for (param, arg) in params.iter().zip(args.iter()) {
                scope.declare(param, arg.clone());
            }
            self.eval_expr(body, &scope, depth + 1)
        } else {
            Err(Error::Eval("expected a function".into()))
        }
    }
}

/// `pkl:test`'s `catch(fun)`: the error `fun()` throws, as a string.
pub(super) const TEST_CATCH: &str = "\0pklr:test.catch";

/// `pkl:test`'s `catchOrNull(fun)`: like `catch`, but null when `fun()` succeeds.
pub(super) const TEST_CATCH_OR_NULL: &str = "\0pklr:test.catchOrNull";

/// A function value that passes its arguments to the evaluator's `builtin`
/// (see `eval_call`). Builtin names can't be written in Pkl, so the call
/// can't be shadowed.
pub(super) fn builtin_function(builtin: &str, params: &[&str]) -> Value {
    let args = params
        .iter()
        .map(|param| Expr::Ident(param.to_string()))
        .collect();
    Value::Lambda(
        params.iter().map(|param| param.to_string()).collect(),
        Arc::new(Expr::Call(Box::new(Expr::Ident(builtin.into())), args)),
        Arc::default(),
    )
}

/// The message `catch` returns for an error: Pkl's message, without
/// pklr's "Eval error: " prefix.
pub(super) fn is_evaluator_control_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Eval(message)
            if message == "evaluation cancelled" || message == "maximum recursion depth exceeded"
    )
}

pub(super) fn caught_error_message(error: Error) -> String {
    let message = match error {
        // A parser's detail is the cause of the error, which `catch` does
        // not report: only the first paragraph is its message.
        Error::Eval(message)
            if message.starts_with("Error parsing JSON document.")
                || message.starts_with("Error parsing YAML document.") =>
        {
            message
                .split_once("\n\n")
                .map_or(message.clone(), |(first, _)| first.to_string())
        }
        Error::Eval(message) => message,
        error => error.to_string(),
    };
    // pkl renders a caught error without its source frame, so the value
    // pklr puts on its own line follows the message, and a renderer's hint to
    // add a converter is dropped. Only a renderer's own errors are rewritten:
    // text a program throws is returned as it is.
    if message.starts_with("Type constraint `") || message.starts_with("Expected value of type `") {
        // The context is a suffix naming one property.
        let message = match message.rsplit_once(super::typecheck::PROPERTY_CONTEXT) {
            Some((first, name)) if name.ends_with('\'') && !name.contains('\n') => first,
            _ => message.as_str(),
        };
        return message.replacen("\nValue: ", " Value: ", 1);
    }
    if !(message.starts_with("Cannot render ")
        || message.starts_with("The top-level value of ")
        || message.starts_with("Cannot spread value of type `"))
    {
        return message;
    }
    message
        .replace(
            "\n\nConsider adding a converter to `output.converters`.",
            "",
        )
        .replace("\nValue: ", " Value: ")
        .replace("\nObject: ", " Object: ")
}

pub(super) fn seed_builtins(scope: &mut Scope) {
    for name in super::stdlib::BUILTIN_MARKER_NAMES {
        scope.set(name, super::stdlib::builtin_marker(name));
    }
}

/// A method of an element-only `Dynamic`, whose elements `items` holds. A
/// `Dynamic` has no members besides its elements, so `toMap()` is empty and
/// `toList()` gives the elements.
fn dynamic_elements_method(
    this: &Value,
    items: &ListValue,
    method: &str,
    args: &[Value],
) -> Result<Option<Value>> {
    match method {
        "toList" => Ok(Some(Value::List(items.clone().with_kind(ListKind::List)))),
        "toMap" => Ok(Some(Value::Object(Arc::default(), None))),
        "getClass" if args.is_empty() => stdlib::get_class(this).map(Some),
        "hasProperty" => {
            require_str_arg(args, 0, method)?;
            Ok(Some(Value::Bool(false)))
        }
        "getPropertyOrNull" => {
            require_str_arg(args, 0, method)?;
            Ok(Some(Value::Null))
        }
        "getProperty" => {
            let name = require_str_arg(args, 0, method)?;
            Err(Error::Eval(format!(
                "Cannot find property `{name}` in object of type `Dynamic`."
            )))
        }
        _ => Err(Error::Eval(format!(
            "Cannot find method `{method}` in class `Dynamic`."
        ))),
    }
}

/// `map` as the entries of a converted object: without the classes a module
/// holds as members (they are not properties), and with any other class held
/// as plain data, so a further conversion does not take it for a declaration.
fn without_class_members(map: &Arc<ObjectMap>, source: Option<&ObjectSource>) -> Arc<ObjectMap> {
    let marked =
        |value: &Value| matches!(value, Value::Object(_, Some(s)) if s.declared_as.is_some());
    if !map.values().any(marked) {
        return map.clone();
    }
    Arc::new(
        map.iter()
            .filter(|(key, value)| !value.is_class_member(key, source))
            .map(|(key, value)| (key.clone(), value.clone().into_entry_value()))
            .collect(),
    )
}

/// Whether `expr` is `super.default`.
fn is_default_member_ref(expr: &Expr) -> bool {
    match expr {
        Expr::Field(base, name) => {
            name == "default" && matches!(base.as_ref(), Expr::Ident(base) if base == "super")
        }
        _ => false,
    }
}

impl Evaluator {
    /// `Null(default)`: the null a valid default stands for. Kept out of line
    /// so it adds nothing to the evaluator's stack frame.
    #[inline(never)]
    fn null_default(&mut self, arg: &Expr, scope: &Scope, depth: usize) -> Result<Value> {
        // Only the selected branch counts.
        if let Expr::If(cond, then_expr, else_expr) = arg {
            let c = self.eval_expr(cond, scope, depth + 1)?;
            let branch = if is_truthy(&c) { then_expr } else { else_expr };
            return self.null_default(branch, scope, depth + 1);
        }
        // A `new` default is not built: it may name the class being
        // instantiated, whose property this is.
        if matches!(arg, Expr::New(..)) {
            return Ok(Value::Null);
        }
        match self.eval_expr(arg, scope, depth + 1)? {
            // Dynamic and Listing objects are held as lists.
            // A class is not an `Object`.
            value @ Value::Object(..) if !is_class_value(&value) => Ok(Value::Null),
            Value::Lambda(..) => Ok(Value::Null),
            Value::List(items) if matches!(items.kind(), ListKind::Dynamic | ListKind::Listing) => {
                Ok(Value::Null)
            }
            other => Err(Error::Eval(format!(
                "Expected value of type `Object | Function<Object>`, but got type `{}`.\nValue: {}",
                pkl_class_name(&other),
                render::display_value(&other)
            ))),
        }
    }
}

/// The error for a method a `Class` does not have.
#[cold]
#[inline(never)]
fn class_method_error(method: &str) -> Error {
    Error::Eval(format!("Cannot find method `{method}` in class `Class`."))
}

/// Whether `method` is one a `Class` value does not have: a `Class` has the
/// methods of `Class`, not those of the class it stands for.
#[inline(never)]
fn is_class_method_miss(receiver: &Expr, source: &ObjectSource, method: &str) -> bool {
    source.kind == ObjectKind::Class
        && !stdlib::is_module_class(source)
        && (source.entries.is_empty()
            || !source
                .type_identity
                .as_deref()
                .is_some_and(|identity| identity.starts_with("pkl:")))
        && !matches!(receiver, Expr::Ident(name) if matches!(name.as_str(), "super" | "this" | "module"))
        && !matches!(method, "getClass" | "toString" | "hashCode")
}

impl Evaluator {
    /// `Bytes(...)`: every argument is an `Int` in `0..=255`.
    #[inline(never)]
    fn eval_bytes_constructor(
        &mut self,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let mut bytes = Vec::with_capacity(args.len());
        for arg in args {
            match self.eval_expr(arg, scope, depth + 1)? {
                value @ Value::Int(0..=255) => bytes.push(value),
                Value::Int(n) => {
                    return Err(Error::Eval(format!(
                        "Type constraint `isBetween(0, 255)` violated.\nValue: {n}"
                    )));
                }
                other => return Err(stdlib::type_mismatch("UInt8", &other)),
            }
        }
        Ok(Value::List(ListValue::from(bytes).into_bytes()))
    }

    /// `List.toMap(keyFn, valueFn)` (also on `Set`).
    #[inline(never)]
    fn list_to_map(&mut self, items: &ListValue, args: &[Value], depth: usize) -> Result<Value> {
        let mut map = ObjectMap::default();
        for item in items.iter() {
            let key = self.invoke_lambda(&args[0], std::slice::from_ref(item), depth)?;
            let value = self.invoke_lambda(&args[1], std::slice::from_ref(item), depth)?;
            let storage_key = value_to_key(&key)?;
            let storage_key = equivalent_mapping_key(&map, &storage_key).unwrap_or(storage_key);
            self.remember_mapping_key(storage_key.clone(), &key);
            map.insert(storage_key, value.into_entry_value());
        }
        Ok(Value::Object(Arc::new(map), None))
    }
}

/// A module's function runs on the module that calls it, which may extend the
/// module that declared it.
#[inline(never)]
fn rebind_module_this(call_scope: &mut Scope, this: &Value, captured: &ScopeMap) {
    if matches!(this, Value::Object(_, None))
        && matches!(captured.get("this"), Some(Value::Object(_, None)))
    {
        call_scope.set("this", this.clone());
    }
}
