//! Checking values against declared types and their constraints.

use super::*;
use crate::parser::TypeExpr;

impl Evaluator {
    /// The scope a loaded module's own names resolve in (its type aliases,
    /// classes and imports), by its identity (canonical path or URL).
    /// Snapshots are keyed by the path the module was loaded with, which need
    /// not be canonical.
    fn module_type_scope(&mut self, identity: &str) -> Option<Scope> {
        let key = if self.module_scopes.contains_key(Path::new(identity)) {
            PathBuf::from(identity)
        } else {
            let keys: Vec<PathBuf> = self.module_scopes.keys().cloned().collect();
            keys.into_iter()
                .find(|key| self.module_type_namespace(key) == identity)?
        };
        let snapshot = self.module_scopes.get(&key)?;
        let mut scope = Scope {
            type_namespace: Some(identity.to_string()),
            ..Scope::default()
        };
        if !snapshot.values.is_empty() {
            scope.vars = Arc::new(snapshot.values.clone());
        }
        for (name, ty) in &snapshot.type_aliases {
            scope.set_type_alias(name.clone(), ty.clone());
        }
        for (name, identity) in &snapshot.module_identities {
            scope.set_module_identity(name.clone(), identity.clone());
        }
        Some(scope)
    }

    /// The type a name stands for when it is a type alias, with the scope its
    /// right-hand side resolves in: `None` for the scope it was looked up in,
    /// or the defining module's own scope for an imported `m.A`, whose
    /// right-hand side may name that module's other aliases and imports.
    fn resolve_type_alias(
        &mut self,
        name: &str,
        scope: &Scope,
    ) -> Option<(TypeExpr, Option<Scope>)> {
        if let Some(alias) = scope.get_type_alias(name) {
            return Some((alias.clone(), None));
        }
        let (root, member) = name.split_once('.')?;
        if member.contains('.') {
            return None;
        }
        let identity = scope.module_identity(root)?.clone();
        let module_scope = self.module_type_scope(&identity)?;
        let alias = module_scope.get_type_alias(member)?.clone();
        Some((alias, Some(module_scope)))
    }

    /// `resolve_type_alias`, except that an alias already followed on this
    /// walk resolves to `None`. A cycle of aliases ends there, however long
    /// the chain leading to it was.
    ///
    /// A chain of aliases that each name the next (`A0 = A1`, ...) is followed
    /// here in a loop, so a long one does not use the call stack once per
    /// alias: the result is the end of the chain, or the first name that does
    /// not resolve or would close a cycle, left for the caller to handle.
    fn resolve_unvisited_alias(
        &mut self,
        name: &str,
        scope: &Scope,
        seen: &mut Vec<String>,
    ) -> Option<(TypeExpr, Option<Scope>)> {
        let (mut alias, mut alias_scope) = self.resolve_alias_step(name, scope, seen)?;
        while let TypeExpr::Named(next) = &alias {
            if next.starts_with('*') {
                break;
            }
            let next = next.clone();
            let current = alias_scope.as_ref().unwrap_or(scope);
            match self.resolve_alias_step(&next, current, seen) {
                Some((target, target_scope)) => {
                    alias = target;
                    if target_scope.is_some() {
                        alias_scope = target_scope;
                    }
                }
                None => break,
            }
        }
        Some((alias, alias_scope))
    }

    /// One alias lookup of `resolve_unvisited_alias`. The cycle key names the
    /// module the alias is defined in as well as its name and body: distinct
    /// modules can each declare `typealias A = m.A` without forming a cycle.
    fn resolve_alias_step(
        &mut self,
        name: &str,
        scope: &Scope,
        seen: &mut Vec<String>,
    ) -> Option<(TypeExpr, Option<Scope>)> {
        let (alias, alias_scope) = self.resolve_type_alias(name, scope)?;
        let defined_in = alias_scope
            .as_ref()
            .unwrap_or(scope)
            .type_namespace
            .clone()
            .unwrap_or_default();
        let key = format!("{defined_in}|{name}={}", display_type_expr(&alias));
        if seen.contains(&key) {
            return None;
        }
        seen.push(key);
        Some((alias, alias_scope))
    }

    /// Whether `ty` accepts the Dynamic a body amending null builds.
    pub(super) fn type_takes_dynamic(
        &mut self,
        ty: &TypeExpr,
        scope: &Scope,
        seen: &mut Vec<String>,
    ) -> bool {
        let named = |this: &mut Self, name: &str, seen: &mut Vec<String>| {
            let name = name.trim_start_matches('*');
            // An alias or class declaration shadows the standard library type
            // of its name, so it is resolved before the built-in names.
            if let Some((alias, alias_scope)) = this.resolve_unvisited_alias(name, scope, seen) {
                let alias_scope = alias_scope.as_ref().unwrap_or(scope);
                return this.type_takes_dynamic(&alias, alias_scope, seen);
            }
            if matches!(resolve_dotted(scope, name),
                Some(Value::Object(_, Some(source))) if source.kind == ObjectKind::Class)
            {
                return false;
            }
            matches!(name, "Dynamic" | "Any" | "Object")
        };
        match ty {
            TypeExpr::Named(name) => named(self, name, seen),
            TypeExpr::Constrained(base, _) => named(self, base.trim_end_matches('?'), seen),
            TypeExpr::Nullable(inner) => self.type_takes_dynamic(inner, scope, seen),
            TypeExpr::Union(variants) => variants
                .iter()
                .any(|variant| self.type_takes_dynamic(variant, scope, seen)),
            TypeExpr::Generic(..) => false,
        }
    }

    /// Whether the type a union marks with `*` is known to supply no default
    /// value: a standard library type other than `Listing`, `Mapping` and
    /// `Dynamic` (`Int`, `Duration`, `Map<K, V>`, ...), directly or behind
    /// aliases. A class, or a type this cannot resolve, counts as supplying
    /// one.
    fn selected_type_lacks_default(
        &mut self,
        ty: &TypeExpr,
        scope: &Scope,
        seen: &mut Vec<String>,
    ) -> bool {
        let name = match ty {
            TypeExpr::Named(name) => name.trim_start_matches('*'),
            TypeExpr::Constrained(base, _) => base.trim_end_matches('?'),
            TypeExpr::Generic(name, _) => name.as_str(),
            // An alias to a union supplies the default of the alternative it
            // marks with `*`, and none when it marks nothing. A nullable one
            // starts from null.
            TypeExpr::Union(variants) => {
                return match variants.iter().find(|variant| is_default_type(variant)) {
                    Some(marked) => self.selected_type_lacks_default(marked, scope, seen),
                    None => true,
                };
            }
            // A nullable type has the default of the type it wraps, or null.
            TypeExpr::Nullable(inner) => {
                return self.selected_type_lacks_default(inner, scope, seen);
            }
        };
        let name = name.trim_end_matches('?');
        let name = name.split('<').next().unwrap_or(name);
        if let Some((alias, alias_scope)) = self.resolve_unvisited_alias(name, scope, seen) {
            let alias_scope = alias_scope.as_ref().unwrap_or(scope);
            return self.selected_type_lacks_default(&alias, alias_scope, seen);
        }
        // A class declaration shadows the standard library type of its name.
        if matches!(resolve_dotted(scope, name),
            Some(Value::Object(_, Some(source))) if source.kind == ObjectKind::Class)
        {
            return false;
        }
        matches!(
            name,
            "String"
                | "Boolean"
                | "Bool"
                | "Int"
                | "Int8"
                | "Int16"
                | "Int32"
                | "UInt"
                | "UInt8"
                | "UInt16"
                | "UInt32"
                | "Float"
                | "Number"
                | "Duration"
                | "DataSize"
                | "Pair"
                | "Regex"
                | "Map"
                | "List"
                | "Set"
                | "Collection"
                | "IntSeq"
                | "Bytes"
                | "Any"
                | "Object"
                | "Typed"
                | "Class"
        ) || name.starts_with("Function")
    }

    /// Whether a type the union marks with `*` is nullable, directly or behind
    /// aliases. When it also supplies no default of its own, the property
    /// starts from null.
    fn selected_type_nullable(
        &mut self,
        ty: &TypeExpr,
        scope: &Scope,
        seen: &mut Vec<String>,
    ) -> bool {
        let name = match ty {
            TypeExpr::Nullable(_) => return true,
            TypeExpr::Named(name) => name.trim_start_matches('*'),
            TypeExpr::Constrained(base, _) => base.as_str(),
            _ => return false,
        };
        if name.ends_with('?') {
            return true;
        }
        match self.resolve_unvisited_alias(name, scope, seen) {
            Some((alias, alias_scope)) => {
                let alias_scope = alias_scope.as_ref().unwrap_or(scope);
                self.selected_type_nullable(&alias, alias_scope, seen)
            }
            None => false,
        }
    }

    /// What amending a property declared `ty` would start from, when `ty` is a
    /// union (possibly nullable, possibly behind aliases). A union has no
    /// default unless one alternative carries the `*` marker; only a marker
    /// written on the property's own union counts, since an alias used as an
    /// alternative contributes none. `scope` must be where the declaration
    /// lives: its aliases are not necessarily visible where the property is
    /// amended. Imported aliases (`m.A`) resolve through the module they come
    /// from, and that module's own imports through theirs; one that cannot be
    /// resolved is not a union here, so it is never rejected.
    pub(super) fn union_amend_state(&mut self, ty: &TypeExpr, scope: &Scope) -> UnionAmendState {
        self.union_amend_state_in(ty, scope, false, &mut Vec::new())
    }

    fn union_amend_state_in(
        &mut self,
        ty: &TypeExpr,
        scope: &Scope,
        nullable: bool,
        seen: &mut Vec<String>,
    ) -> UnionAmendState {
        match ty {
            TypeExpr::Nullable(inner) => self.union_amend_state_in(inner, scope, true, seen),
            TypeExpr::Union(variants) => {
                // A marked alternative's own default is what the property
                // starts from, unless that type has none (`Int`, `Duration`).
                let marked = variants.iter().find(|variant| is_default_type(variant));
                if let Some(marked) = marked
                    && !self.selected_type_lacks_default(marked, scope, &mut Vec::new())
                {
                    return UnionAmendState::HasDefault;
                }
                let nullable = nullable
                    || marked.is_some_and(|marked| {
                        self.selected_type_nullable(marked, scope, &mut Vec::new())
                    });
                if !nullable {
                    return UnionAmendState::NoDefault;
                }
                // Nothing supplies a default, so a nullable union's is null
                // and the amendment builds a Dynamic that must itself satisfy
                // the union. One that takes a Dynamic is checked once the body
                // has run.
                if variants
                    .iter()
                    .any(|variant| self.type_takes_dynamic(variant, scope, &mut Vec::new()))
                {
                    UnionAmendState::NotUnion
                } else {
                    UnionAmendState::NullDefault(display_type_expr(ty))
                }
            }
            TypeExpr::Named(name) if !name.starts_with('*') => {
                match self.resolve_unvisited_alias(name, scope, seen) {
                    Some((alias, alias_scope)) => {
                        let alias_scope = alias_scope.as_ref().unwrap_or(scope);
                        self.union_amend_state_in(&alias, alias_scope, nullable, seen)
                    }
                    None => UnionAmendState::NotUnion,
                }
            }
            // A constraint is checked on the value afterwards and gives it no
            // default: follow its base, which names an alias or spells a union.
            TypeExpr::Constrained(base, _) => {
                let nullable = nullable || base.ends_with('?');
                match crate::parser::parse_type_name(base.trim_end_matches('?')) {
                    Ok(base) => self.union_amend_state_in(&base, scope, nullable, seen),
                    Err(_) => UnionAmendState::NotUnion,
                }
            }
            _ => UnionAmendState::NotUnion,
        }
    }

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
        if !type_is_runtime_checkable(ty, scope)
            || self.eval_type_check(value, ty, scope, depth + 1)?
        {
            return Ok(());
        }
        Err(Error::Eval(format!(
            "property '{}' expected {}, got {}",
            prop.name,
            display_type_expr(ty),
            value_type_display(value)
        )))
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

    /// Check an element, key, value, argument or result against its declared
    /// type. An untyped object body is accepted where the type names a class,
    /// as pkl would have made it an instance of it.
    pub(super) fn eval_argument_check(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        if is_uninferred_object(val) && type_names_user_class(ty, scope, 0) {
            return Ok(true);
        }
        self.eval_type_check_in(val, ty, scope, depth, resolving)
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
        match ty {
            TypeExpr::Named(name) => {
                if let Some(expected) = string_literal_type_value(name) {
                    return Ok(matches!(val, Value::String(actual) if &**actual == expected));
                }
                // Check if name is a type alias; if so, resolve to the aliased type
                // A `*` marks a union's default alternative and is not part of
                // the type.
                let name = name.trim_start_matches('*');
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                // A scalar of the built-in type of that name matches without
                // resolving the name as a class, which only a class instance
                // could match, unless a user class of that name shadows it.
                if scalar_is_builtin_type(val, name) && scope.get(name).is_none() {
                    return Ok(true);
                }
                if let Some(matches) = value_is_class_type(val, name, scope) {
                    return Ok(matches);
                }
                // Otherwise, plain type check
                Ok(value_is_type(val, ty))
            }
            TypeExpr::Constrained(base, constraint) => {
                // First check the base type
                let class_name = base.trim_end_matches('?').split('<').next().unwrap_or(base);
                let base_matches = if base.ends_with('?') && is_null_value(val) {
                    true
                } else if base.contains('<')
                    && let Ok(generic) = crate::parser::parse_type_name(base)
                {
                    // `Listing<Int>(length == 2)`: the base keeps its type
                    // arguments, which constrain the elements.
                    self.eval_type_check_in(val, &generic, scope, depth, resolving)?
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
                // Evaluate the constraint with `this` bound to the value
                let mut constraint_scope = scope.child();
                constraint_scope.set("this", val.clone());
                // Also bind common properties directly so `length`, `isEmpty` etc. work
                match val {
                    Value::String(s) => {
                        constraint_scope.set("length", Value::Int(s.chars().count() as i64));
                        constraint_scope.set("isEmpty", Value::Bool(s.is_empty()));
                    }
                    Value::Int(_) | Value::Float(_) => {}
                    // An element-only `Dynamic` has no `length`.
                    Value::List(items) if items.kind() != ListKind::Dynamic => {
                        constraint_scope.set("length", Value::Int(items.len() as i64));
                        constraint_scope.set("isEmpty", Value::Bool(items.is_empty()));
                    }
                    Value::Object(items, _) => {
                        constraint_scope.set("length", Value::Int(items.len() as i64));
                        constraint_scope.set("isEmpty", Value::Bool(items.is_empty()));
                    }
                    _ => {}
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
            TypeExpr::Generic(name, args) => {
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                if !value_is_type(val, ty) {
                    return Ok(false);
                }
                self.check_type_arguments(val, name, args, scope, depth, resolving)
            }
        }
    }

    /// Check the elements of a `List<T>`, `Set<T>`, `Listing<T>` or
    /// `Collection<T>`, and the keys and values of a `Map<K, V>` or
    /// `Mapping<K, V>`, against their type arguments. The value is already
    /// known to be an instance of the generic class. An argument pklr cannot
    /// model (a type parameter, an unresolved class) accepts everything, as
    /// does `Any`.
    fn check_type_arguments(
        &mut self,
        val: &Value,
        name: &str,
        args: &[crate::parser::TypeExpr],
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<bool> {
        fn checked<'t>(
            ty: Option<&'t crate::parser::TypeExpr>,
            scope: &Scope,
        ) -> Option<&'t crate::parser::TypeExpr> {
            ty.filter(|ty| !type_is_any(ty) && type_is_runtime_checkable(ty, scope))
        }
        match (name, val) {
            ("List" | "Listing" | "Set" | "Collection", Value::List(items)) => {
                let Some(element_ty) = checked(args.first(), scope) else {
                    return Ok(true);
                };
                for item in items.iter() {
                    if !self.eval_argument_check(item, element_ty, scope, depth, resolving)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            ("Map" | "Mapping", Value::Object(entries, source))
                if source
                    .as_ref()
                    .is_none_or(|source| source.type_name.is_none()) =>
            {
                let key_ty = checked(args.first(), scope);
                let value_ty = checked(args.get(1), scope);
                if key_ty.is_none() && value_ty.is_none() {
                    return Ok(true);
                }
                for (stored_key, entry) in entries.iter() {
                    if let Some(key_ty) = key_ty {
                        let key = self
                            .mapping_key_values
                            .get(stored_key)
                            .cloned()
                            .unwrap_or_else(|| crate::value::mapping_storage_value(stored_key));
                        if !self.eval_argument_check(&key, key_ty, scope, depth, resolving)? {
                            return Ok(false);
                        }
                    }
                    if let Some(value_ty) = value_ty
                        && !self.eval_argument_check(entry, value_ty, scope, depth, resolving)?
                    {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(true),
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
            Expr::Checked(value, ty) => Expr::Checked(go(value, bound), ty.clone()),
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
            | Expr::Checked(..)
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

/// Whether `val` is an object body whose class pklr did not infer: not a
/// `Dynamic` made by `new Dynamic`, not a `Mapping`, and no class instance.
/// pkl gives such a `new { ... }` the class its declared type names; pklr does
/// not always, so it cannot disprove that the type accepts it.
fn is_uninferred_object(val: &Value) -> bool {
    matches!(
        val,
        Value::Object(_, Some(source))
            if source.kind == ObjectKind::Object
                && source.type_name.is_none()
                && source.type_identity.is_none()
    )
}

/// Whether `ty` names a class of the program, directly or through an alias,
/// a nullable, a constraint or a union.
fn type_names_user_class(ty: &crate::parser::TypeExpr, scope: &Scope, depth: usize) -> bool {
    use crate::parser::TypeExpr;
    if depth > 8 {
        return false;
    }
    let class_name = |name: &str| {
        let name = name.trim_start_matches('*').trim_end_matches('?');
        if let Some(alias) = scope.get_type_alias(name) {
            return type_names_user_class(alias, scope, depth + 1);
        }
        matches!(
            resolve_dotted(scope, name),
            Some(Value::Object(_, Some(source))) if source.type_identity.is_some()
        )
    };
    match ty {
        TypeExpr::Named(name) | TypeExpr::Generic(name, _) => class_name(name),
        TypeExpr::Constrained(base, _) => class_name(base.split('<').next().unwrap_or(base)),
        TypeExpr::Nullable(inner) => type_names_user_class(inner, scope, depth + 1),
        TypeExpr::Union(variants) => variants
            .iter()
            .any(|variant| type_names_user_class(variant, scope, depth + 1)),
    }
}

/// Whether `val` is a scalar (a number, string, boolean, regex, duration or
/// data size) and `name` is the built-in type it is an instance of.
fn scalar_is_builtin_type(val: &Value, name: &str) -> bool {
    match val {
        Value::Int(_) => matches!(name, "Int" | "Number"),
        Value::Float(_) => matches!(name, "Float" | "Number"),
        Value::String(_) => name == "String",
        Value::Bool(_) => name == "Boolean",
        Value::Regex(_) => name == "Regex",
        Value::Duration(_) => name == "Duration",
        Value::DataSize(_) => name == "DataSize",
        _ => false,
    }
}

/// Whether amending a union-typed property has a value to start from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UnionAmendState {
    /// Not a union, or one this check cannot decide.
    NotUnion,
    /// A union with a `*` alternative.
    HasDefault,
    /// A union with no `*` alternative: the property is undefined.
    NoDefault,
    /// A nullable union with no `*` alternative: the property is null. Holds
    /// the union as written, for the error message.
    NullDefault(String),
}
