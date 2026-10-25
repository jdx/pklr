//! Calling functions, methods and lambdas, and the builtin functions.

use super::*;

impl Evaluator {
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
            return Err(Error::Eval(format!(
                "unknown method '{method}' on {}",
                value_type_name(&obj)
            )));
        }

        // Handle built-in functions: List(), Listing(), Map()
        if let Expr::Ident(name) = func_expr {
            match name.as_str() {
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
                            let key = value_to_key(k)?;
                            let storage_key =
                                equivalent_mapping_key(&map, &key).unwrap_or_else(|| key.clone());
                            self.remember_mapping_key(storage_key.clone(), k);
                            map.insert(storage_key, v.clone());
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
            if let Some(Value::Object(this_map, _)) = scope.get("this") {
                for (k, v) in this_map.iter() {
                    call_scope.set(k, v.clone());
                }
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
        if let Some(result) = self.stdlib_method(obj, method, args, depth) {
            return result.map(Some);
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
            (Value::List(items), "toList") => {
                Ok(Some(Value::List(items.clone().with_kind(ListKind::List))))
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

            // Object/Mapping methods
            (Value::Object(map, _), "containsKey") => {
                let key = args.first().and_then(|v| v.as_str()).unwrap_or("");
                Ok(Some(Value::Bool(map.contains_key(key))))
            }
            (Value::Object(map, _), "toMap" | "toMapping") => {
                Ok(Some(Value::Object(map.clone(), None)))
            }
            (Value::Object(map, _), "mapValues") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("mapValues requires a function".into()))?;
                let mut result = ObjectMap::default();
                for (k, v) in map.iter() {
                    let new_v =
                        self.invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)?;
                    result.insert(k.clone(), new_v);
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
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
                        result.insert(k.clone(), v.clone());
                    }
                }
                Ok(Some(Value::Object(Arc::new(result), None)))
            }
            (Value::Object(..), "toList") => Ok(Some(obj.clone())),
            (Value::Object(map, _), "toDynamic") => Ok(Some(Value::Object(map.clone(), None))),

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
    match error {
        Error::Eval(message) => message,
        error => error.to_string(),
    }
}

pub(super) fn seed_builtins(scope: &mut Scope) {
    for name in [
        "Regex",
        "Dynamic",
        "Annotation",
        "Duration",
        "DataSize",
        "IntSeq",
        "Pair",
    ] {
        scope.set(name, Value::String(name.into()));
    }
}
