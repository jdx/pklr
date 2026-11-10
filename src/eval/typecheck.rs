//! Checking values against declared types and their constraints.

use super::*;

impl Evaluator {
    /// Check a concrete property value after its defining scope is complete.
    /// Defaults and declarations without values are checked when they are
    /// materialized, not merely declared.
    pub(super) fn check_declared_property_type(
        &mut self,
        prop: &Property,
        value: &Value,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        if prop.value.is_none() && prop.body.is_none() {
            return Ok(());
        }
        let Some(ty) = &prop.type_ann else {
            return Ok(());
        };
        // An explicit `new Dynamic { ... }` is a `Dynamic` even when the value
        // model represents its elements as a `Listing` or its members as an
        // untyped object, so the declared type must accept a `Dynamic`.
        if is_explicit_dynamic(prop) && !type_accepts_dynamic(ty, scope) {
            return Err(Error::Eval(format!(
                "property '{}' expected {}, got Dynamic",
                prop.name,
                display_type_expr(ty),
            )));
        }
        // Its elements are stored as a `Listing`, which a `Dynamic` type
        // would otherwise not match, so check the type's constraints on it
        // separately.
        let accepted = if is_explicit_dynamic(prop) && matches!(value, Value::List(_)) {
            self.check_element_dynamic(value, ty, scope, depth + 1, &mut Vec::new())?
        } else {
            !type_is_runtime_checkable(ty, scope)
                || self.eval_type_check(value, ty, scope, depth + 1)?
        };
        if accepted {
            return Ok(());
        }
        Err(Error::Eval(format!(
            "property '{}' expected {}, got {}",
            prop.name,
            display_type_expr(ty),
            if is_explicit_dynamic(prop) {
                "Dynamic"
            } else {
                value_type_name(value)
            }
        )))
    }

    /// Check an element-only `Dynamic`, which the value model stores as a
    /// list, against a declared type: the type must accept a `Dynamic` and
    /// every constraint along the way must hold for the value.
    pub(super) fn check_element_dynamic(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        use crate::parser::TypeExpr;
        // Walk simple alias chains (`A0 = A1`, ...) in a loop, as
        // `eval_alias_check` does, so a long chain does not use the Rust
        // stack once per alias.
        let mark = resolving.len();
        let mut current = ty.clone();
        let result = loop {
            match &current {
                TypeExpr::Nullable(inner) => {
                    let inner = (**inner).clone();
                    current = inner;
                }
                TypeExpr::Union(variants) => {
                    let mut accepted = false;
                    for variant in variants {
                        match self.check_element_dynamic(val, variant, scope, depth + 1, resolving)
                        {
                            Ok(true) => {
                                accepted = true;
                                break;
                            }
                            Ok(false) => {}
                            Err(err) => {
                                resolving.truncate(mark);
                                return Err(err);
                            }
                        }
                    }
                    break Ok(accepted);
                }
                TypeExpr::Named(name) | TypeExpr::Generic(name, _) => {
                    let base = name.trim_start_matches('*').trim_end_matches('?');
                    let base = base.split('<').next().unwrap_or(base);
                    let Some(target) = scope.get_type_alias(base) else {
                        break Ok(type_accepts_dynamic(&current, scope));
                    };
                    if resolving.iter().any(|seen| seen == base) {
                        break Err(Error::Eval(format!("type alias '{base}' refers to itself")));
                    }
                    resolving.push(base.to_string());
                    let target = target.clone();
                    current = target;
                }
                TypeExpr::Constrained(base, constraint) => {
                    let base_ty = TypeExpr::Named(base.clone());
                    match self.check_element_dynamic(val, &base_ty, scope, depth + 1, resolving) {
                        Ok(true) => {}
                        other => break other,
                    }
                    // The base type accepted it; what is left is the
                    // predicate, run without resolving a base type again.
                    break self.eval_element_dynamic_constraint(val, constraint, scope, depth);
                }
            }
        };
        resolving.truncate(mark);
        result
    }

    /// Run a constraint on an element-only `Dynamic`. Its `this` is the
    /// stored list, which is marked so a type test on it sees a `Dynamic`
    /// (see `eval_type_check_in`), and it has none of the members the list
    /// would add to the scope.
    fn eval_element_dynamic_constraint(
        &mut self,
        val: &Value,
        constraint: &Expr,
        scope: &Scope,
        depth: usize,
    ) -> Result<bool> {
        let Value::List(items) = val else {
            return Ok(false);
        };
        self.element_dynamics.push(items.items_ptr() as usize);
        let result = self.eval_constraint(val, constraint, scope, depth, false);
        self.element_dynamics.pop();
        result
    }

    /// Whether `val` is the stored list of an element-only `Dynamic` whose
    /// constraint is running.
    fn is_element_dynamic(&self, val: &Value) -> bool {
        matches!(val, Value::List(items)
            if items.kind() == ListKind::Listing
                && self.element_dynamics.contains(&(items.items_ptr() as usize)))
    }

    /// Evaluate a constraint with `this` bound to the value, for a value that
    /// has passed its base type.
    fn eval_constraint(
        &mut self,
        val: &Value,
        constraint: &Expr,
        scope: &Scope,
        depth: usize,
        bind_members: bool,
    ) -> Result<bool> {
        let mut constraint_scope = scope.child();
        constraint_scope.set("this", val.clone());
        // Also bind common properties directly so `length`, `isEmpty` etc. work
        if bind_members {
            match val {
                Value::String(s) => {
                    constraint_scope.set("length", Value::Int(s.chars().count() as i64));
                    constraint_scope.set("isEmpty", Value::Bool(s.is_empty()));
                }
                Value::List(items) => {
                    constraint_scope.set("length", Value::Int(items.len() as i64));
                    constraint_scope.set("isEmpty", Value::Bool(items.is_empty()));
                }
                Value::Object(items, _) => {
                    constraint_scope.set("length", Value::Int(items.len() as i64));
                    constraint_scope.set("isEmpty", Value::Bool(items.is_empty()));
                }
                _ => {}
            }
        }
        let receiver_constraint = with_constraint_receiver(constraint, &constraint_scope);
        let constraint = receiver_constraint.as_ref().unwrap_or(constraint);
        let mut result = self.eval_expr(constraint, &constraint_scope, depth + 1)?;
        // `Listing(isSorted)` names a predicate function and applies
        // it to the value being checked.
        if matches!(result, Value::Lambda(..)) {
            result = self.invoke_lambda(&result, std::slice::from_ref(val), depth + 1)?;
        }
        Ok(is_truthy(&result))
    }

    /// Check if a value matches a type expression, including constraint evaluation.
    pub(super) fn eval_type_check(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<bool> {
        self.eval_type_check_in(val, ty, scope, depth, &mut Vec::new())
    }

    /// Resolve aliases with a guard: malformed or recursively-expanded
    /// aliases must report an evaluation error instead of recursing until the
    /// process stack overflows.
    pub(super) fn eval_alias_check(
        &mut self,
        val: &Value,
        name: &str,
        resolved: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        if resolving.iter().any(|seen| seen == name) {
            return Err(Error::Eval(format!("type alias '{name}' refers to itself")));
        }
        resolving.push(name.to_string());
        // Long chains (`A0 = A1`, …) are valid Pkl and must not consume the
        // Rust call stack once per alias.  Flatten the simple-name part here;
        // compound targets still go through the regular checker below.
        let mut target = resolved.clone();
        while let crate::parser::TypeExpr::Named(next) = &target {
            let Some(next_target) = scope.get_type_alias(next) else {
                break;
            };
            if resolving.iter().any(|seen| seen == next) {
                while resolving.pop().as_deref() != Some(name) {}
                return Err(Error::Eval(format!("type alias '{next}' refers to itself")));
            }
            resolving.push(next.clone());
            target = next_target.clone();
        }
        let result = self.eval_type_check_in(val, &target, scope, depth + 1, resolving);
        while resolving.pop().as_deref() != Some(name) {}
        result
    }

    /// Check `val` against an alias an imported module declares, as that
    /// module's own scope reads it: its constraint calls the module's
    /// functions and names its constants.
    fn check_imported_alias(
        &mut self,
        val: &Value,
        name: &str,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<Option<bool>> {
        if !name.contains('.') || scope.get_type_alias(name).is_none() {
            return Ok(None);
        }
        let Some((tail, declaring)) = self.imported_alias_scope(scope, name) else {
            return Ok(None);
        };
        let ty = crate::parser::TypeExpr::Named(tail);
        self.eval_type_check_in(val, &ty, &declaring, depth + 1, resolving)
            .map(Some)
    }

    pub(super) fn eval_type_check_in(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        use crate::parser::TypeExpr;
        // A type test on the `this` of an element-only `Dynamic` constraint
        // sees a `Dynamic`, not the list that stores it.
        if self.is_element_dynamic(val) {
            return self.check_element_dynamic(val, ty, scope, depth + 1, resolving);
        }
        match ty {
            TypeExpr::Named(name) => {
                if let Some(expected) = string_literal_type_value(name) {
                    return Ok(matches!(val, Value::String(actual) if &**actual == expected));
                }
                if let Some(accepted) =
                    self.check_imported_alias(val, name, scope, depth, resolving)?
                {
                    return Ok(accepted);
                }
                // Check if name is a type alias; if so, resolve to the aliased type
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                if let Some(matches) = value_is_class_type(val, name, scope) {
                    return Ok(matches);
                }
                // Otherwise, plain type check
                Ok(value_is_type(val, ty))
            }
            TypeExpr::Constrained(base, constraint) => {
                // First check the base type
                let class_name = base
                    .trim_start_matches('*')
                    .trim_end_matches('?')
                    .split('<')
                    .next()
                    .unwrap_or(base);
                let base_matches = if base.ends_with('?') && is_null_value(val) {
                    true
                } else if let Some(accepted) =
                    self.check_imported_alias(val, class_name, scope, depth, resolving)?
                {
                    accepted
                } else if let Some(resolved) = scope.get_type_alias(class_name) {
                    let resolved = resolved.clone();
                    self.eval_alias_check(val, class_name, &resolved, scope, depth, resolving)?
                } else {
                    value_is_class_type(val, class_name, scope)
                        .unwrap_or_else(|| value_is_named_type(val, base))
                };
                if !base_matches {
                    return Ok(false);
                }
                self.eval_constraint(val, constraint, scope, depth, true)
            }
            TypeExpr::Nullable(inner) => {
                if is_null_value(val) {
                    return Ok(true);
                }
                self.eval_type_check_in(val, inner, scope, depth, resolving)
            }
            TypeExpr::Union(variants) => {
                // A type pklr does not model cannot be disproved at runtime.
                // Check every modeled variant first, but preserve that
                // conservative acceptance only if none of them matches.
                let mut has_uncheckable_variant = false;
                for v in variants {
                    if !type_is_runtime_checkable(v, scope) {
                        has_uncheckable_variant = true;
                    } else if self.eval_type_check_in(val, v, scope, depth, resolving)? {
                        return Ok(true);
                    }
                }
                Ok(has_uncheckable_variant)
            }
            TypeExpr::Generic(name, _) => {
                if let Some(accepted) =
                    self.check_imported_alias(val, name, scope, depth, resolving)?
                {
                    return Ok(accepted);
                }
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                Ok(value_is_type(val, ty))
            }
        }
    }
}

/// Calls the evaluator handles as constructors rather than methods of a
/// constrained value.
pub(super) const CONSTRAINT_BUILTIN_CALLS: &[&str] = &[
    "List", "Listing", "Set", "Map", "Regex", "IntSeq", "Pair", "Mapping",
];

/// Rewrite the unbound value names in a constraint to read from its implicit
/// receiver. In Pkl, `Address(street.endsWith("St."))` reads `street` from
/// the address being checked, and `List(every(...))` invokes the checked
/// list's `every` method. Names bound by the surrounding scope, `let`, or a
/// lambda keep their normal meaning.
pub(super) fn with_constraint_receiver(constraint: &Expr, scope: &Scope) -> Option<Expr> {
    use std::cell::Cell;

    fn unbound(name: &str, scope: &Scope, bound: &[String]) -> bool {
        !matches!(name, "this" | "module" | "outer" | "super")
            && !bound.iter().any(|bound| bound == name)
            && scope.get(name).is_none()
            // Pkl resolves a type alias before an identically named member in
            // this expression, so it must not become `this.name`.
            && scope.get_type_alias(name).is_none()
            && scope.poison_of(name).is_none()
    }

    fn receiver(name: &str) -> Expr {
        Expr::Field(Box::new(Expr::Ident("this".into())), name.into())
    }

    fn rewrite(expr: &Expr, scope: &Scope, bound: &mut Vec<String>, changed: &Cell<bool>) -> Expr {
        let go =
            |expr: &Expr, bound: &mut Vec<String>| Box::new(rewrite(expr, scope, bound, changed));
        match expr {
            Expr::Ident(name) if unbound(name, scope, bound) => {
                changed.set(true);
                receiver(name)
            }
            Expr::Call(callee, args) => {
                let callee = match callee.as_ref() {
                    Expr::Ident(name) if CONSTRAINT_BUILTIN_CALLS.contains(&name.as_str()) => {
                        callee.clone()
                    }
                    Expr::Ident(name) if unbound(name, scope, bound) => {
                        changed.set(true);
                        Box::new(receiver(name))
                    }
                    other => go(other, bound),
                };
                let args = args.iter().map(|arg| *go(arg, bound)).collect();
                Expr::Call(callee, args)
            }
            Expr::Field(base, field) => Expr::Field(go(base, bound), field.clone()),
            Expr::NullSafeField(base, field) => Expr::NullSafeField(go(base, bound), field.clone()),
            Expr::Index(base, key) => Expr::Index(go(base, bound), go(key, bound)),
            Expr::If(condition, then_expr, else_expr) => Expr::If(
                go(condition, bound),
                go(then_expr, bound),
                go(else_expr, bound),
            ),
            Expr::Let(name, value, body) => {
                let value = go(value, bound);
                bound.push(name.clone());
                let body = go(body, bound);
                bound.pop();
                Expr::Let(name.clone(), value, body)
            }
            Expr::Is(value, ty) => Expr::Is(go(value, bound), ty.clone()),
            Expr::As(value, ty) => Expr::As(go(value, bound), ty.clone()),
            Expr::Binop(op, left, right) => Expr::Binop(*op, go(left, bound), go(right, bound)),
            Expr::Unop(op, value) => Expr::Unop(*op, go(value, bound)),
            Expr::Lambda(params, body) => {
                let before = bound.len();
                bound.extend(params.iter().cloned());
                let body = Arc::new(rewrite(body, scope, bound, changed));
                bound.truncate(before);
                Expr::Lambda(params.clone(), body)
            }
            Expr::StringInterpolation(parts) => Expr::StringInterpolation(
                parts
                    .iter()
                    .map(|part| match part {
                        StringInterpPart::Expr(expr) => StringInterpPart::Expr(*go(expr, bound)),
                        literal => literal.clone(),
                    })
                    .collect(),
            ),
            Expr::Throw(value) => Expr::Throw(go(value, bound)),
            Expr::Trace(value, site) => Expr::Trace(go(value, bound), Arc::clone(site)),
            Expr::Read(value, module_path) => Expr::Read(go(value, bound), module_path.clone()),
            Expr::ReadOrNull(value, module_path) => {
                Expr::ReadOrNull(go(value, bound), module_path.clone())
            }
            Expr::ReadGlob(value, module_path) => {
                Expr::ReadGlob(go(value, bound), module_path.clone())
            }
            // Object bodies and `new` establish their own implicit receivers.
            other => other.clone(),
        }
    }

    let changed = Cell::new(false);
    let rewritten = rewrite(constraint, scope, &mut Vec::new(), &changed);
    changed.get().then_some(rewritten)
}

/// Whether a type, after resolving visible aliases, contains a constraint.
pub(super) fn type_has_constraint(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    fn visit(ty: &crate::parser::TypeExpr, scope: &Scope, aliases: &mut HashSet<Name>) -> bool {
        use crate::parser::TypeExpr;

        match ty {
            TypeExpr::Constrained(..) => true,
            TypeExpr::Named(name) => scope
                .get_type_alias(name)
                .is_some_and(|alias| aliases.insert(nm(name)) && visit(alias, scope, aliases)),
            TypeExpr::Nullable(inner) => visit(inner, scope, aliases),
            TypeExpr::Union(variants) => variants
                .iter()
                .any(|variant| visit(variant, scope, aliases)),
            TypeExpr::Generic(name, args) => {
                scope
                    .get_type_alias(name)
                    .is_some_and(|alias| aliases.insert(nm(name)) && visit(alias, scope, aliases))
                    || args.iter().any(|arg| visit(arg, scope, aliases))
            }
        }
    }

    visit(ty, scope, &mut HashSet::default())
}

/// A constraint whose evaluated branch explicitly throws has a meaningful Pkl
/// failure to preserve. This follows named predicate lambdas too; other
/// evaluator errors remain conservatively deferred because some package
/// predicates use language features pklr cannot yet model.
pub(super) fn type_constraint_has_explicit_throw(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
) -> bool {
    fn lambda_has_throw(value: &Value, scope: &Scope, seen: &mut HashSet<usize>) -> bool {
        let Value::Lambda(_, body, captured) = value else {
            return false;
        };
        if !seen.insert(Arc::as_ptr(body) as usize) {
            return false;
        }
        expr_has_throw(body, scope, Some(captured), seen)
    }

    fn static_value<'a>(
        expr: &Expr,
        scope: &'a Scope,
        captured: Option<&'a ScopeMap>,
    ) -> Option<&'a Value> {
        match expr {
            Expr::Ident(name) => captured
                .and_then(|bindings| bindings.get(name.as_str()))
                .or_else(|| scope.get(name.as_str())),
            Expr::Field(receiver, field) | Expr::NullSafeField(receiver, field) => {
                let Value::Object(members, _) = static_value(receiver, scope, captured)? else {
                    return None;
                };
                members.get(field.as_str())
            }
            _ => None,
        }
    }

    fn expr_has_throw(
        expr: &Expr,
        scope: &Scope,
        captured: Option<&ScopeMap>,
        seen: &mut HashSet<usize>,
    ) -> bool {
        match expr {
            Expr::Throw(_) => true,
            Expr::TemplateLocals(_, inner) => expr_has_throw(inner, scope, captured, seen),
            Expr::Ident(_) => static_value(expr, scope, captured)
                .is_some_and(|value| lambda_has_throw(value, scope, seen)),
            Expr::Field(value, _) | Expr::NullSafeField(value, _) => {
                expr_has_throw(value, scope, captured, seen)
                    || static_value(expr, scope, captured)
                        .is_some_and(|value| lambda_has_throw(value, scope, seen))
            }
            Expr::Unop(_, value)
            | Expr::Trace(value, _)
            | Expr::Read(value, _)
            | Expr::ReadOrNull(value, _)
            | Expr::ReadGlob(value, _) => expr_has_throw(value, scope, captured, seen),
            Expr::Index(left, right) | Expr::Binop(_, left, right) => {
                expr_has_throw(left, scope, captured, seen)
                    || expr_has_throw(right, scope, captured, seen)
            }
            Expr::Call(callee, args) => {
                expr_has_throw(callee, scope, captured, seen)
                    || args
                        .iter()
                        .any(|arg| expr_has_throw(arg, scope, captured, seen))
            }
            Expr::If(condition, then_expr, else_expr) => {
                expr_has_throw(condition, scope, captured, seen)
                    || expr_has_throw(then_expr, scope, captured, seen)
                    || expr_has_throw(else_expr, scope, captured, seen)
            }
            Expr::Let(_, value, body) => {
                expr_has_throw(value, scope, captured, seen)
                    || expr_has_throw(body, scope, captured, seen)
            }
            Expr::Lambda(_, body) => expr_has_throw(body, scope, captured, seen),
            Expr::StringInterpolation(parts) => parts.iter().any(|part| {
                matches!(part, StringInterpPart::Expr(expr) if expr_has_throw(expr, scope, captured, seen))
            }),
            Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::New(..)
            | Expr::InferredNew(..)
            | Expr::Is(..)
            | Expr::As(..)
            | Expr::ObjectBody(_)
            | Expr::Import(..)
            | Expr::ImportGlob(..) => false,
        }
    }

    fn visit(ty: &crate::parser::TypeExpr, scope: &Scope, aliases: &mut HashSet<Name>) -> bool {
        use crate::parser::TypeExpr;

        match ty {
            TypeExpr::Constrained(_, constraint) => {
                expr_has_throw(constraint, scope, None, &mut HashSet::default())
            }
            TypeExpr::Named(name) | TypeExpr::Generic(name, _) => scope
                .get_type_alias(name)
                .is_some_and(|alias| aliases.insert(nm(name)) && visit(alias, scope, aliases)),
            TypeExpr::Nullable(inner) => visit(inner, scope, aliases),
            TypeExpr::Union(variants) => variants
                .iter()
                .any(|variant| visit(variant, scope, aliases)),
        }
    }

    visit(ty, scope, &mut HashSet::default())
}

/// Some pantry constraints use Pkl's predicate-composition semantics, which
/// pklr does not implement yet. Keep that evaluator limitation deferred; it
/// is distinct from a user `throw` reached by a supported predicate.
pub(super) fn is_deferred_constraint_evaluator_error(message: &str) -> bool {
    message.starts_with("Operator `")
        && message.contains(" is not defined for operand types `")
        && message.contains("Function")
}
