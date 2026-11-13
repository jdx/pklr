use super::*;
use crate::parser::BaseForm;

/// Collect `@Deprecated` annotations from a list of entries into a map of
/// property name → optional message. Used to populate `ObjectSource.deprecated`
/// so field access can warn lazily, instead of warning eagerly when a module
/// or object body is evaluated.
pub(super) fn collect_deprecated(entries: &[Entry]) -> IndexMap<String, Option<String>> {
    let mut out: IndexMap<String, Option<String>> = IndexMap::new();
    for entry in entries {
        if let Entry::Property(prop) = entry {
            for ann in &prop.annotations {
                if ann.name != "Deprecated" {
                    continue;
                }
                let mut message = None;
                for e in &ann.body {
                    if let Entry::Property(p) = e
                        && &*p.name == "message"
                        && let Some(Expr::String(s)) = &p.value
                    {
                        message = Some(s.to_string());
                    }
                }
                out.insert(prop.name.to_string(), message);
            }
        }
    }
    out
}

/// Combine a base deprecation map with any `@Deprecated` annotations on a
/// list of overlay entries, with overlay winning on conflict. Used by the
/// amend/merge code paths so an amended object's `ObjectSource.deprecated`
/// reflects deprecations from both the base and the overlay.
pub(super) fn merge_deprecated(
    base: &IndexMap<String, Option<String>>,
    overlay_entries: &[Entry],
) -> IndexMap<String, Option<String>> {
    let mut out = base.clone();
    for (k, v) in collect_deprecated(overlay_entries) {
        out.insert(k, v);
    }
    out
}

/// Resolve return-type aliases while their definition scope is available.
/// Value::Lambda captures values, so type aliases would otherwise be lost at
/// invocation. Keep inference errors lazy until the selected branch is called.
/// Whether [`capture_method_result_types`] would change `expr`.
pub(super) fn needs_method_result_types(expr: &Expr) -> bool {
    match expr {
        Expr::InferredNew(..) => true,
        Expr::If(_, then_expr, else_expr) => {
            needs_method_result_types(then_expr) || needs_method_result_types(else_expr)
        }
        Expr::Let(_, _, body) | Expr::Trace(body, _) | Expr::Constraint(body, _) => {
            needs_method_result_types(body)
        }
        _ => false,
    }
}

pub(super) fn capture_method_result_types(expr: &mut Expr, scope: &Scope) {
    match expr {
        Expr::InferredNew(ty, entries) => {
            *expr = match inferred_new_type(ty, scope, 0) {
                Ok((name, params)) => Expr::New(Some(name), std::mem::take(entries), params),
                Err(error) => Expr::Throw(Box::new(Expr::String(error.to_string().into()))),
            };
        }
        Expr::If(_, then_expr, else_expr) => {
            capture_method_result_types(then_expr, scope);
            capture_method_result_types(else_expr, scope);
        }
        Expr::Let(_, _, body) | Expr::Trace(body, _) | Expr::Constraint(body, _) => {
            capture_method_result_types(body, scope)
        }
        _ => {}
    }
}

/// Replace the type aliases a lambda body names with the types they stand for
/// in the scope that defines the lambda. Value::Lambda captures values only,
/// so an alias declared in an enclosing body or module would otherwise be
/// unknown when the lambda is called, and `x is Alias` would check against a
/// class or built-in of the same name instead. Names declared again by a
/// `typealias` or `class` in a body inside the lambda keep their inner meaning.
pub(super) fn capture_type_aliases(
    expr: &mut Expr,
    scope: &Scope,
    deferred: &HashMap<usize, (Arc<Expr>, Arc<str>)>,
) {
    AliasResolver {
        scope,
        deferred,
        bindings: Vec::new(),
        expanding: Vec::new(),
    }
    .expr(expr, &HashSet::default());
}

struct AliasResolver<'a> {
    scope: &'a Scope,
    /// Lambda bodies whose const violation waits for the call, by address: a
    /// copy of the body made here would otherwise lose it.
    deferred: &'a HashMap<usize, (Arc<Expr>, Arc<str>)>,
    /// Type parameters of the generic alias being expanded, with the
    /// arguments they stand for.
    bindings: Vec<(String, crate::parser::TypeExpr)>,
    /// Aliases whose definitions are being expanded. An alias that names
    /// itself, directly or in a constraint, is left as written there.
    expanding: Vec<String>,
}

impl AliasResolver<'_> {
    fn expr(&mut self, expr: &mut Expr, shadowed: &HashSet<Name>) {
        match expr {
            Expr::Is(value, ty) | Expr::As(value, ty) | Expr::Checked(value, ty) => {
                self.expr(value, shadowed);
                self.ty(ty, shadowed);
            }
            Expr::InferredNew(ty, entries) => {
                self.ty(ty, shadowed);
                self.body(entries, shadowed);
            }
            Expr::New(_, entries, _) | Expr::ObjectBody(entries) => self.body(entries, shadowed),
            Expr::Field(value, _)
            | Expr::NullSafeField(value, _)
            | Expr::Unop(_, value)
            | Expr::Throw(value)
            | Expr::TemplateLocals(_, value)
            | Expr::Trace(value, _)
            | Expr::Constraint(value, _)
            | Expr::Read(value, _)
            | Expr::ReadOrNull(value, _)
            | Expr::ReadGlob(value, _) => self.expr(value, shadowed),
            Expr::Lambda(_, value) => {
                if let Some((_, message)) = self.deferred.get(&(Arc::as_ptr(value) as usize)) {
                    *value = Arc::new(Expr::Throw(Box::new(Expr::String(Arc::clone(message)))));
                } else {
                    self.expr(Arc::make_mut(value), shadowed)
                }
            }
            Expr::Index(left, right) | Expr::Binop(_, left, right) | Expr::Let(_, left, right) => {
                self.expr(left, shadowed);
                self.expr(right, shadowed);
            }
            Expr::Call(callee, args) => {
                self.expr(callee, shadowed);
                for arg in args {
                    self.expr(arg, shadowed);
                }
            }
            Expr::If(cond, then_expr, else_expr) => {
                self.expr(cond, shadowed);
                self.expr(then_expr, shadowed);
                self.expr(else_expr, shadowed);
            }
            Expr::StringInterpolation(parts) => {
                for part in parts {
                    if let StringInterpPart::Expr(expr) = part {
                        self.expr(expr, shadowed);
                    }
                }
            }
            Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Ident(_)
            | Expr::Import(..)
            | Expr::ImportGlob(..) => {}
        }
    }

    fn body(&mut self, entries: &mut crate::parser::Body, shadowed: &HashSet<Name>) {
        let declared = entries.iter().filter_map(|entry| match entry {
            Entry::TypeAlias(name, ..) | Entry::ClassDef(name, ..) => Some(name),
            _ => None,
        });
        let mut inner_shadowed = None::<HashSet<Name>>;
        for name in declared {
            if !shadowed.contains(name.as_str()) {
                inner_shadowed
                    .get_or_insert_with(|| shadowed.clone())
                    .insert(Name::from(name.as_str()));
            }
        }
        let shadowed = inner_shadowed.as_ref().unwrap_or(shadowed);
        for entry in Arc::make_mut(entries) {
            self.entry(entry, shadowed);
        }
    }

    fn entry(&mut self, entry: &mut Entry, shadowed: &HashSet<Name>) {
        match entry {
            Entry::Property(prop) => {
                let prop = Arc::make_mut(prop);
                if let Some(ty) = &mut prop.type_ann {
                    self.ty(ty, shadowed);
                }
                if let Some(value) = &mut prop.value {
                    self.expr(value, shadowed);
                }
                if let Some(body) = &mut prop.body {
                    self.body(body, shadowed);
                }
            }
            Entry::DynProperty(key, value) | Entry::Predicate(key, value) => {
                self.expr(key, shadowed);
                self.expr(value, shadowed);
            }
            Entry::ForGenerator(generator) => {
                self.expr(&mut generator.collection, shadowed);
                self.body(&mut generator.body, shadowed);
            }
            Entry::WhenGenerator(generator) => {
                self.expr(&mut generator.condition, shadowed);
                self.body(&mut generator.body, shadowed);
                if let Some(else_body) = &mut generator.else_body {
                    self.body(else_body, shadowed);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => self.expr(expr, shadowed),
            Entry::ClassDef(_, _, _, body) => self.body(body, shadowed),
            Entry::TypeAlias(_, ty, ..) => self.ty(ty, shadowed),
        }
    }

    /// The alias `name` stands for, fully resolved, unless it is shadowed
    /// where it is used or already being expanded.
    fn resolved_alias(
        &mut self,
        name: &str,
        shadowed: &HashSet<Name>,
    ) -> Option<crate::parser::TypeExpr> {
        if shadowed.contains(name) || self.expanding.iter().any(|n| n == name) {
            return None;
        }
        let mut ty = self.scope.get_type_alias(name)?.clone();
        self.expanding.push(name.to_string());
        // The definition belongs to the defining scope, where names the use
        // site redeclares still mean the enclosing aliases.
        self.ty(&mut ty, &HashSet::default());
        self.expanding.pop();
        Some(ty)
    }

    /// A generic alias applied to arguments, expanded from its written
    /// definition with the parameters replaced by the arguments.
    fn generic_alias(
        &mut self,
        name: &str,
        args: &[crate::parser::TypeExpr],
        shadowed: &HashSet<Name>,
    ) -> Option<crate::parser::TypeExpr> {
        use crate::parser::TypeExpr;
        if shadowed.contains(name) || self.expanding.iter().any(|n| n == name) {
            return None;
        }
        let Some(TypeExpr::Generic(params, written)) = self
            .scope
            .get_type_alias(&super::typecheck::generic_alias_key(name))
        else {
            return None;
        };
        let params: Vec<&str> = params.split(',').collect();
        if params.len() != args.len() {
            return None;
        }
        let mut body = written.first()?.clone();
        let mut args = args.to_vec();
        for arg in &mut args {
            self.ty(arg, shadowed);
        }
        let outer = std::mem::replace(
            &mut self.bindings,
            params.iter().map(|p| p.to_string()).zip(args).collect(),
        );
        self.expanding.push(name.to_string());
        self.ty(&mut body, &HashSet::default());
        self.expanding.pop();
        self.bindings = outer;
        Some(body)
    }

    fn ty(&mut self, ty: &mut crate::parser::TypeExpr, shadowed: &HashSet<Name>) {
        use crate::parser::TypeExpr;
        match ty {
            TypeExpr::Named(name) => {
                if let Some((_, arg)) = self.bindings.iter().find(|(param, _)| param == name) {
                    *ty = arg.clone();
                    return;
                }
                let (default_marker, plain) = match name.strip_prefix('*') {
                    Some(plain) => ("*", plain),
                    None => ("", name.as_str()),
                };
                let Some(resolved) = self.resolved_alias(plain, shadowed) else {
                    return;
                };
                *ty = match resolved {
                    TypeExpr::Named(target) => TypeExpr::Named(format!("{default_marker}{target}")),
                    // Only a plain name can carry the union default marker.
                    resolved if default_marker.is_empty() => resolved,
                    _ => return,
                };
            }
            TypeExpr::Nullable(inner) => self.ty(inner, shadowed),
            TypeExpr::Generic(name, args) => {
                // A generic alias (`L<Int>` with `typealias L<T> = List<T>`)
                // stands for its erased definition.
                let (default_marker, plain) = match name.strip_prefix('*') {
                    Some(plain) => ("*", plain),
                    None => ("", name.as_str()),
                };
                if default_marker.is_empty()
                    && let Some(resolved) = self.generic_alias(plain, args, shadowed)
                {
                    *ty = resolved;
                    return;
                }
                if default_marker.is_empty()
                    && let Some(resolved) = self.resolved_alias(plain, shadowed)
                {
                    *ty = resolved;
                    return;
                }
                for ty in args {
                    self.ty(ty, shadowed);
                }
            }
            TypeExpr::Union(types) => {
                for ty in types {
                    self.ty(ty, shadowed);
                }
            }
            TypeExpr::Constrained(base, constraint, form) => {
                self.expr(constraint, shadowed);
                // A quoted base is one name: a trailing `?` belongs to it.
                let (nullable, unsuffixed) = match form {
                    BaseForm::Text => (base.ends_with('?'), base.trim_end_matches('?')),
                    BaseForm::Quoted => (false, base.as_str()),
                    BaseForm::NullableQuoted => (true, base.as_str()),
                };
                let (default_marker, plain) = match unsuffixed.strip_prefix('*') {
                    Some(plain) => ("*", plain),
                    None => ("", unsuffixed),
                };
                // `List<I>(length == 1)`: the aliases are in the type
                // arguments of the base, not in its name.
                if *form == BaseForm::Text
                    && plain.contains('<')
                    && let Ok(mut generic) = crate::parser::parse_type_name(plain)
                {
                    self.ty(&mut generic, shadowed);
                    if let Some(text) = runtime_base_name(&generic)
                        && text != plain
                    {
                        let suffix = if nullable { "?" } else { "" };
                        *base = format!("{default_marker}{text}{suffix}");
                    }
                    return;
                }
                let bound = self
                    .bindings
                    .iter()
                    .find(|(param, _)| param == plain)
                    .map(|(_, arg)| arg.clone());
                let Some(resolved) = bound.or_else(|| self.resolved_alias(plain, shadowed)) else {
                    return;
                };
                // The expanded base is a quoted name when the alias's target is
                // one (`typealias L = `C?``), and not when it is `C?` or
                // `List<Int>`.
                let target_form = match &resolved {
                    TypeExpr::Named(name) if !crate::lexer::is_plain_qualified_name(name) => {
                        BaseForm::Quoted
                    }
                    TypeExpr::Constrained(_, _, form) => *form,
                    _ => BaseForm::Text,
                };
                let (target, constraint_expr) = match resolved {
                    // `A(c)` with `A = B(c2)` checks `B`, then `c2`, then `c`,
                    // all against the same value. A nullable `A?` lets null
                    // skip `c2`, which a single conjunction cannot express.
                    TypeExpr::Constrained(target, inner, inner_form)
                        if !nullable
                            || inner_form == BaseForm::NullableQuoted
                            || (inner_form == BaseForm::Text && target.ends_with('?')) =>
                    {
                        let outer = std::mem::replace(constraint.as_mut(), Expr::Null);
                        (target, Expr::Binop(BinOp::And, inner, Box::new(outer)))
                    }
                    TypeExpr::Constrained(..) => return,
                    resolved => match runtime_base_name(&resolved) {
                        Some(target) => {
                            (target, std::mem::replace(constraint.as_mut(), Expr::Null))
                        }
                        None => return,
                    },
                };
                // The use site's `?` is not part of a quoted target's name, so
                // the form carries it: null matches the target, then the
                // constraint runs on it, as in pkl.
                let (suffix, expanded_form) = match target_form {
                    BaseForm::Text => (
                        if nullable && !target.ends_with('?') {
                            "?"
                        } else {
                            ""
                        },
                        BaseForm::Text,
                    ),
                    BaseForm::Quoted if nullable => ("", BaseForm::NullableQuoted),
                    form => ("", form),
                };
                *base = format!("{default_marker}{target}{suffix}");
                *form = expanded_form;
                **constraint = constraint_expr;
            }
        }
    }
}

/// The body of the alias `module.Alias`, written for the module that imports
/// `module` as `prefix`: each name the module declares (`declared`) becomes
/// `prefix.Name`, so it resolves from the importer to the same type it named
/// where the alias was declared. Other names (built-ins) are left alone.
/// Constraints are dropped, as imported aliases are not checked against them.
pub(super) fn qualify_alias_body(
    ty: &crate::parser::TypeExpr,
    prefix: &str,
    declared: &dyn Fn(&str) -> bool,
) -> crate::parser::TypeExpr {
    use crate::parser::TypeExpr;
    let qualify_name = |name: &str| -> String {
        let (marker, rest) = match name.strip_prefix('*') {
            Some(rest) => ("*", rest),
            None => ("", name),
        };
        let (core, nullable) = match rest.strip_suffix('?') {
            Some(core) => (core, "?"),
            None => (rest, ""),
        };
        let core = if core.contains('<') {
            // A generic name carries its arguments as text.
            match crate::parser::parse_type_name(core) {
                Ok(parsed) => runtime_base_name(&qualify_alias_body(&parsed, prefix, declared))
                    .unwrap_or_else(|| core.to_string()),
                Err(_) => core.to_string(),
            }
        } else if declared(core) {
            format!("{prefix}.{core}")
        } else {
            core.to_string()
        };
        format!("{marker}{core}{nullable}")
    };
    match ty {
        TypeExpr::Named(name) => TypeExpr::Named(qualify_name(name)),
        TypeExpr::Nullable(inner) => {
            TypeExpr::Nullable(Box::new(qualify_alias_body(inner, prefix, declared)))
        }
        TypeExpr::Union(types) => TypeExpr::Union(
            types
                .iter()
                .map(|ty| qualify_alias_body(ty, prefix, declared))
                .collect(),
        ),
        TypeExpr::Generic(name, args) => TypeExpr::Generic(
            if declared(name) {
                format!("{prefix}.{name}")
            } else {
                name.clone()
            },
            args.iter()
                .map(|ty| qualify_alias_body(ty, prefix, declared))
                .collect(),
        ),
        // A constraint is an expression of the declaring module, which names
        // its functions and constants. It cannot be evaluated here, so only
        // the type it constrains is kept.
        TypeExpr::Constrained(base, _, _) => {
            let plain = base.strip_suffix('?').unwrap_or(base);
            match crate::parser::parse_type_name(plain) {
                // A generic base without a `*` marker is the generic type itself.
                Ok(parsed @ TypeExpr::Generic(..)) if !plain.starts_with('*') => {
                    let parsed = qualify_alias_body(&parsed, prefix, declared);
                    if plain.len() < base.len() {
                        TypeExpr::Nullable(Box::new(parsed))
                    } else {
                        parsed
                    }
                }
                _ => TypeExpr::Named(qualify_name(base)),
            }
        }
    }
}

/// The name a constrained type stores for its base, for a type that has one.
fn runtime_base_name(ty: &crate::parser::TypeExpr) -> Option<String> {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => Some(name.clone()),
        TypeExpr::Nullable(inner) => Some(format!("{}?", runtime_base_name(inner)?)),
        TypeExpr::Generic(name, args) => Some(format!(
            "{name}<{}>",
            args.iter()
                .map(runtime_base_name)
                .collect::<Option<Vec<_>>>()?
                .join(", ")
        )),
        TypeExpr::Union(_) | TypeExpr::Constrained(..) => None,
    }
}

/// Resolve an implicit method-result constructor without dropping generic
/// arguments or the default alternative of a union.
pub(super) fn inferred_new_type(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    depth: usize,
) -> Result<(String, Vec<String>)> {
    use crate::parser::TypeExpr;
    if depth > 32 {
        return Err(Error::Eval("recursive inferred return type".into()));
    }
    // Nullability and chains of aliases are followed in a loop, so a long
    // chain cannot overflow the stack and a cyclic one is an error.
    let mut ty = ty;
    let mut seen = HashSet::default();
    loop {
        match ty {
            TypeExpr::Nullable(inner) => ty = inner,
            TypeExpr::Named(name) => {
                let name = name.trim_start_matches('*');
                let Some(alias) = scope.get_type_alias(name) else {
                    break;
                };
                if !seen.insert(name) {
                    return Err(Error::Eval(
                        "Type alias definitions must not be cyclic.".into(),
                    ));
                }
                ty = alias;
            }
            _ => break,
        }
    }
    match ty {
        TypeExpr::Nullable(inner) => inferred_new_type(inner, scope, depth + 1),
        TypeExpr::Named(name) => {
            let name = name.trim_start_matches('*');
            // Default union alternatives are stored as Named("*Type<...>").
            if name.contains('<') || name.ends_with('?') {
                let ty = crate::parser::parse_type_name(name)?;
                return inferred_new_type(&ty, scope, depth + 1);
            }
            Ok((name.to_string(), Vec::new()))
        }
        TypeExpr::Generic(name, args) => Ok((
            name.trim_start_matches('*').to_string(),
            args.iter()
                .enumerate()
                .flat_map(|(index, arg)| {
                    // The mapping constructor reserves one slot for its key
                    // type, followed by all value-type alternatives.
                    if index == 0 && matches!(name.as_str(), "Mapping" | "Map") {
                        return vec![display_type_expr(arg)];
                    }
                    // `Listing<Listing<T>>` keeps the inner listing type.
                    if name == "Listing" && resolves_to_listing(arg, scope) {
                        return vec![display_type_expr(arg)];
                    }
                    let mut names = Vec::new();
                    collect_type_expr_class_names(arg, scope, &mut names, 0);
                    names
                })
                .collect(),
        )),
        TypeExpr::Union(types) => {
            let selected = types
                .iter()
                .find(|ty| is_default_type(ty))
                .ok_or_else(|| Error::Eval("Cannot tell which parent to amend".into()))?;
            inferred_new_type(selected, scope, depth + 1)
        }
        TypeExpr::Constrained(name, _, _) => {
            // Constraints store the underlying type as a runtime name. Parse
            // that name back into a type to retain any generic arguments.
            let ty = crate::parser::parse_type_name(name.trim_start_matches('*'))?;
            inferred_new_type(&ty, scope, depth + 1)
        }
    }
}

/// The type alias `name` stands for in `scope`, including one an importer
/// bound for the module it imports as `m` (`m.A`, kept under `m#A` so only the
/// default-value helpers, which need no more than its shape, see it).
fn alias_of<'a>(scope: &'a Scope, name: &str) -> Option<&'a crate::parser::TypeExpr> {
    scope.get_type_alias(name).or_else(|| {
        let (root, member) = name.split_once('.')?;
        scope.get_type_alias(&format!("{root}#{member}"))
    })
}

pub(super) fn type_default_value(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<Value> {
    type_default_value_seen(ty, scope, &mut Vec::new())
}

/// [`type_default_value`] with the aliases already being expanded, so a cyclic
/// alias such as `typealias A = *A|Int` yields no default instead of recursing
/// until the stack overflows. [`type_default_error`] reports the cycle.
fn type_default_value_seen(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    seen: &mut Vec<String>,
) -> Option<Value> {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Constrained(_, _, BaseForm::NullableQuoted) => Some(Value::Null),
        // A quoted name is one name, looked up as it is written.
        TypeExpr::Constrained(base, _, BaseForm::Quoted) => {
            quoted_default(base.trim_start_matches('*'), scope, seen)
        }
        TypeExpr::Constrained(base, _, BaseForm::Text) if base.ends_with('?') => Some(Value::Null),
        // A constraint supplies no default: the type it constrains does.
        TypeExpr::Constrained(base, _, _) => {
            type_default_value_seen(&TypeExpr::Named(base.clone()), scope, seen)
        }
        TypeExpr::Nullable(_) => Some(Value::Null),
        TypeExpr::Named(name) => {
            // Follow a chain of plain aliases in a loop, not by recursion; a
            // cyclic one has no default.
            let mark = seen.len();
            let mut name = name;
            let value = loop {
                // A class named `Null` is that class, not the null type.
                if name == "Null" && declared_class(scope, name).is_none() {
                    break Some(Value::Null);
                }
                if let Some(value) = string_literal_type_value(name) {
                    break Some(Value::String(value.into()));
                }
                // A marked alternative (`*A`) takes its default from what `A`
                // aliases.
                let base = name.trim_start_matches('*');
                match alias_of(scope, base) {
                    Some(alias) => {
                        if seen.iter().any(|seen| seen == base) {
                            break None;
                        }
                        seen.push(base.to_string());
                        match alias {
                            TypeExpr::Named(next) => name = next,
                            other => break type_default_value_seen(other, scope, seen),
                        }
                    }
                    // A name the parser read (no `*`) that ends in `?` was quoted,
                    // and is looked up as it is written.
                    None if !name.starts_with('*') && name.ends_with('?') => {
                        break exact_default(name, scope);
                    }
                    None => break type_default_for_name(name, scope),
                }
            };
            seen.truncate(mark);
            value
        }
        TypeExpr::Generic(name, _) => match name.as_str() {
            "Collection" | "List" | "Set" | "Listing" => Some(empty_collection(name)),
            "Mapping" => Some(Value::Object(
                Arc::default(),
                Some(Arc::new(mapping_source())),
            )),
            "Map" => Some(Value::Object(Arc::default(), None)),
            _ => resolve_dotted(scope, name).map(as_instance),
        },
        TypeExpr::Union(variants) => variants
            .iter()
            .find(|ty| is_default_type(ty))
            // A nullable default alternative leaves the property unset, as
            // `*Int?` does.
            .filter(|ty| !is_nullable_default(ty, scope))
            // The built-in `Map` and `IntSeq` are values with no default of
            // their own. A class of that name declared in scope shadows them
            // and keeps its own default.
            .filter(|ty| match ty {
                TypeExpr::Named(name) => {
                    !(matches!(
                        name.trim_start_matches('*').split('<').next(),
                        Some("Map" | "IntSeq")
                    ) && !names_user_class(name, scope))
                }
                // pkl gives `*Map<K, V>(c)` its empty map, but `*IntSeq(c)` has
                // no default, as bare `IntSeq` has none.
                TypeExpr::Constrained(name, _, _) => {
                    !(name.trim_start_matches('*') == "IntSeq" && !names_user_class(name, scope))
                }
                _ => true,
            })
            .and_then(|ty| type_default_value_seen(ty, scope, seen)),
    }
}

/// Whether a `*` default alternative is nullable, directly or through an alias.
fn is_nullable_default(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    is_nullable_default_seen(ty, scope, &mut Vec::new())
}

/// The alias `name` stands for, followed along a chain of aliases that each
/// name the next (`A0 = A1`, ...) in a loop, so a long chain does not use the
/// stack once per alias. The aliases followed are added to `seen`, and `None`
/// means there is none, or that following it would close a cycle.
fn alias_chain_end(
    scope: &Scope,
    name: &str,
    seen: &mut Vec<String>,
) -> Option<crate::parser::TypeExpr> {
    use crate::parser::TypeExpr;
    let name = name.trim_start_matches('*').trim_end_matches('?');
    let mut target = alias_of(scope, name)?;
    // A `?` on a name along the chain makes what the chain ends at nullable.
    let mut nullable = false;
    if seen.iter().any(|seen| seen == name) {
        return None;
    }
    seen.push(name.to_string());
    while let TypeExpr::Named(next) = target {
        let next_nullable = next.ends_with('?');
        let next = next.trim_start_matches('*').trim_end_matches('?');
        let Some(following) = alias_of(scope, next) else {
            break;
        };
        if seen.iter().any(|seen| seen == next) {
            return None;
        }
        seen.push(next.to_string());
        nullable |= next_nullable;
        target = following;
    }
    Some(if nullable && !matches!(target, TypeExpr::Nullable(_)) {
        TypeExpr::Nullable(Box::new(target.clone()))
    } else {
        target.clone()
    })
}

fn is_nullable_default_seen(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    seen: &mut Vec<String>,
) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Nullable(_) => true,
        // A bare `*Item?` keeps its `?` on the name.
        TypeExpr::Constrained(_, _, BaseForm::NullableQuoted) => true,
        TypeExpr::Constrained(name, _, BaseForm::Text) if name.ends_with('?') => true,
        TypeExpr::Named(name) if name.ends_with('?') => true,
        TypeExpr::Named(name) => {
            let mark = seen.len();
            let nullable = alias_chain_end(scope, name, seen).is_some_and(|target| match &target {
                // The alias's own `*` alternative decides, as for a direct union.
                TypeExpr::Union(variants) => variants
                    .iter()
                    .find(|alt| is_default_type(alt))
                    .is_some_and(|alt| is_nullable_default_seen(alt, scope, seen)),
                target => is_nullable_default_seen(target, scope, seen),
            });
            seen.truncate(mark);
            nullable
        }
        _ => false,
    }
}

/// The deferred error of a class requested as an implicit property default.
/// This deliberately follows only the branch [`type_default_value`] selects:
/// nullable and built-in collection defaults remain non-fallible, while a
/// poisoned user class (including through an alias) reports its original
/// error when the property is actually evaluated.
pub(super) fn type_default_error(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<String> {
    // `raw` is a name as the parser read it, which can hold a `?` or `<` of its
    // own when it was quoted. Other text is a type, whose `?` and type
    // arguments are not part of the name.
    fn named_error(name: &str, raw: bool, scope: &Scope, seen: &mut Vec<String>) -> Option<String> {
        let mark = seen.len();
        let mut name = name;
        let mut raw = raw;
        // A chain of aliases is followed in a loop, not by recursion.
        let error = loop {
            let starless = name.trim_start_matches('*');
            let base = if raw {
                starless
            } else {
                let plain = starless.trim_end_matches('?');
                plain.split('<').next().unwrap_or(plain)
            };
            let Some(alias) = alias_of(scope, base) else {
                // A qualified class can be absent from an imported module's
                // rendered map because evaluating its definition failed. Walk
                // the dotted path so its ObjectSource poison is still reported
                // when this default is actually materialized.
                break poisoned_member(scope, base);
            };
            if seen.iter().any(|seen| seen == base) {
                // An alias an importer bound names the next module's alias
                // of the same name as it was written (`A = m.A`): that is
                // another alias, not a cycle.
                break scope
                    .get_type_alias(base)
                    .map(|_| "Type alias definitions must not be cyclic.".to_string());
            }
            seen.push(base.to_string());
            match alias {
                crate::parser::TypeExpr::Named(next) if next != "Null" => {
                    name = next;
                    raw = !next.starts_with('*');
                }
                other => break visit(other, scope, seen),
            }
        };
        seen.truncate(mark);
        error
    }

    fn visit(
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        seen: &mut Vec<String>,
    ) -> Option<String> {
        use crate::parser::TypeExpr;

        match ty {
            TypeExpr::Constrained(base, _, form)
                if *form == BaseForm::Quoted
                    || (*form == BaseForm::Text && !base.ends_with('?')) =>
            {
                named_error(base, *form == BaseForm::Quoted, scope, seen)
            }
            TypeExpr::Constrained(..) | TypeExpr::Nullable(_) => None,
            TypeExpr::Named(name) if name != "Null" => {
                named_error(name, !name.starts_with('*'), scope, seen)
            }
            TypeExpr::Named(_) => None,
            TypeExpr::Generic(name, _) => match name.as_str() {
                "Collection" | "List" | "Set" | "Listing" | "Mapping" | "Map" => None,
                _ => named_error(name, true, scope, seen),
            },
            TypeExpr::Union(variants) => variants
                .iter()
                .find(|variant| is_default_type(variant))
                .and_then(|variant| visit(variant, scope, seen)),
        }
    }

    visit(ty, scope, &mut Vec::new())
}

/// Whether `name` resolves to a class declared in scope, which shadows the
/// built-in type of that name.
fn names_user_class(name: &str, scope: &Scope) -> bool {
    let name = name.trim_start_matches('*').trim_end_matches('?');
    let base = name.split('<').next().unwrap_or(name);
    matches!(
        resolve_dotted(scope, base),
        Some(Value::Object(_, Some(source))) if source.kind == ObjectKind::Class
    )
}

pub(super) fn type_default_for_name(name: &str, scope: &Scope) -> Option<Value> {
    let name = name.trim_start_matches('*').trim_end_matches('?');
    exact_default(name.split('<').next().unwrap_or(name), scope)
}

/// The default of the alias, class or built-in type called `name`, which is
/// looked up as it is written: it is not trimmed of a `?`, so a quoted
/// `C?` is not `C`. `None` when nothing is called that.
fn quoted_default(name: &str, scope: &Scope, seen: &mut Vec<String>) -> Option<Value> {
    if let Some(alias) = alias_of(scope, name).cloned() {
        if seen.iter().any(|seen| seen == name) {
            return None;
        }
        seen.push(name.to_string());
        let value = type_default_value_seen(&alias, scope, seen);
        seen.pop();
        return value;
    }
    exact_default(name, scope)
}

/// The default of the class or built-in type called `base_name`.
fn exact_default(base_name: &str, scope: &Scope) -> Option<Value> {
    // A class declaration shadows the built-in type with the same name.
    // Check it before the built-in default cases, whose scope bindings are
    // marker strings rather than class objects.
    if let Some(value) = resolve_dotted(scope, base_name)
        && matches!(&value, Value::Object(_, Some(source)) if source.kind == ObjectKind::Class)
    {
        return Some(as_instance(value));
    }
    match base_name {
        "Null" => Some(Value::Null),
        "Collection" | "List" | "Set" | "Listing" => Some(empty_collection(base_name)),
        "Mapping" => Some(Value::Object(
            Arc::default(),
            Some(Arc::new(mapping_source())),
        )),
        "Map" => Some(Value::Object(Arc::default(), None)),
        "Dynamic" => empty_mapping_or_listing("Dynamic"),
        "String" | "Boolean" | "Bool" | "Int" | "Float" | "Number" | "Any" | "Duration"
        | "DataSize" | "Pair" | "Regex" => None,
        other => resolve_dotted(scope, other).map(as_instance),
    }
}

/// An empty `Mapping`, `Listing` or `Dynamic`, for amending the class itself.
pub(super) fn empty_mapping_or_listing(class: &str) -> Option<Value> {
    match class {
        "Listing" => Some(empty_collection("Listing")),
        "Mapping" => Some(Value::Object(
            Arc::default(),
            Some(Arc::new(mapping_source())),
        )),
        "Dynamic" => {
            let mut source = mapping_source();
            source.kind = ObjectKind::Dynamic;
            Some(Value::Object(Arc::default(), Some(Arc::new(source))))
        }
        _ => None,
    }
}

/// An empty value of the collection type `name`.
fn empty_collection(name: &str) -> Value {
    let kind = match name {
        "Listing" => ListKind::Listing,
        "Set" => ListKind::Set,
        _ => ListKind::List,
    };
    Value::List(ListValue::new(kind, Vec::new()))
}

pub(super) fn nullable_inner_default(ty: &crate::parser::TypeExpr, scope: &Scope) -> Option<Value> {
    nullable_inner_default_seen(ty, scope, &mut Vec::new())
}

fn nullable_inner_default_seen(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    seen: &mut Vec<String>,
) -> Option<Value> {
    use crate::parser::TypeExpr;
    // Aliases already followed on this walk end a cycle.
    let alias = |name: &str, seen: &mut Vec<String>| alias_chain_end(scope, name, seen);
    let mark = seen.len();
    let result = match ty {
        // Look through a nullable alias first: its own default is null, but an
        // amendment starts from the type it wraps.
        TypeExpr::Nullable(inner) => nullable_inner_default_seen(inner, scope, seen)
            .or_else(|| type_default_value(inner, scope)),
        // A quoted name is one name, looked up as it is written; only a
        // nullable one amends from what it wraps.
        TypeExpr::Constrained(base, _, form) if *form != BaseForm::Text => {
            let name = base.trim_start_matches('*');
            let nullable = *form == BaseForm::NullableQuoted;
            match alias(name, seen) {
                Some(target) => nullable_inner_default_seen(&target, scope, seen).or_else(|| {
                    nullable
                        .then(|| type_default_value(&target, scope))
                        .flatten()
                }),
                None => nullable.then(|| exact_default(name, scope)).flatten(),
            }
        }
        TypeExpr::Constrained(name, _, form) => {
            let (name, nullable) = constrained_name(name, *form);
            match alias(name, seen) {
                // Only a nullable type amends from what it wraps.
                Some(target) => nullable_inner_default_seen(&target, scope, seen).or_else(|| {
                    nullable
                        .then(|| type_default_value(&target, scope))
                        .flatten()
                }),
                None if nullable => type_default_for_name(name, scope),
                None => None,
            }
        }
        // `Listing<String>`, `Mapping<K, V>` and the like start from their
        // empty collection (or a generic class's instance).
        TypeExpr::Generic(..) => type_default_value(ty, scope),
        // An alias amends like the type it names, so look through it.
        TypeExpr::Named(name) => match alias(name, seen) {
            Some(target) => nullable_inner_default_seen(&target, scope, seen),
            // A quoted name ending in `?` is that name, not a nullable type.
            None if !name.starts_with('*') && name.ends_with('?') => exact_default(name, scope),
            None if name.ends_with('?') => type_default_for_name(name.trim_end_matches('?'), scope),
            None => None,
        },
        // A nullable `*` alternative leaves the property unset, but amending it
        // still starts from that alternative's own default.
        TypeExpr::Union(variants) => variants
            .iter()
            .find(|alt| is_default_type(alt))
            .filter(|alt| is_nullable_default(alt, scope))
            .and_then(|alt| nullable_inner_default_seen(alt, scope, seen)),
    };
    seen.truncate(mark);
    result
}

pub(super) fn type_is_listing(ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) | TypeExpr::Generic(name, _) => name == "Listing",
        TypeExpr::Nullable(inner) => type_is_listing(inner),
        // A default alternative keeps its `*` marker on the base name.
        TypeExpr::Constrained(name, _, form) => {
            let (name, _) = constrained_name(name, *form);
            name == "Listing" || (*form == BaseForm::Text && name.starts_with("Listing<"))
        }
        TypeExpr::Union(variants) => variants.iter().any(type_is_listing),
    }
}

/// `Any` (optionally nullable) gives an untyped `new` no parent to infer.
pub(super) fn type_is_any(ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => name == "Any",
        TypeExpr::Nullable(inner) => type_is_any(inner),
        _ => false,
    }
}

/// Whether `entries`, including generator bodies, define a named member.
pub(super) fn entries_define_members(entries: &[Entry]) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::Property(prop) => !has_modifier(&prop.modifiers, crate::parser::Modifier::Local),
        Entry::DynProperty(..) => true,
        Entry::ForGenerator(generator) => entries_define_members(&generator.body),
        Entry::WhenGenerator(generator) => {
            entries_define_members(&generator.body)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(|body| entries_define_members(body))
        }
        _ => false,
    })
}

pub(super) fn entries_are_listing_amendment(entries: &[Entry]) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::Elem(_) => true,
        Entry::ForGenerator(generator) => entries_are_listing_amendment(&generator.body),
        Entry::WhenGenerator(generator) => {
            entries_are_listing_amendment(&generator.body)
                || generator
                    .else_body
                    .as_deref()
                    .is_some_and(|body| entries_are_listing_amendment(body))
        }
        _ => false,
    })
}

pub(super) fn resolve_dotted(scope: &Scope, name: &str) -> Option<Value> {
    // An imported alias binds the classes it names under their qualified name.
    if name.contains('.')
        && let Some(value) = scope.get(name)
    {
        return Some(value.clone());
    }
    let parts: Vec<&str> = name.split('.').collect();
    let mut val = scope.get(parts[0])?.clone();
    for part in &parts[1..] {
        val = match val {
            Value::Object(ref map, _) => map.get(*part)?.clone(),
            _ => return None,
        };
    }
    Some(val)
}

pub(super) fn has_modifier(mods: &[Modifier], target: Modifier) -> bool {
    mods.contains(&target)
}

/// The error for assigning or amending an inherited `const` or `fixed`
/// property without redeclaring that modifier.
pub(super) fn immutable_member_error(prop: &Property, inherited: &Property) -> Option<String> {
    if prop.value.is_none() && prop.body.is_none() {
        return None;
    }
    let verb = if prop.value.is_some() {
        "assign to"
    } else {
        "amend"
    };
    [(Modifier::Const, "const"), (Modifier::Fixed, "fixed")]
        .into_iter()
        .find(|(modifier, _)| {
            has_modifier(&inherited.modifiers, modifier.clone())
                && !has_modifier(&prop.modifiers, modifier.clone())
        })
        .map(|(_, kind)| format!("Cannot {verb} {kind} property `{}`.", prop.name))
}

pub(super) fn should_render_property_value(prop: &Property, value: &Value) -> bool {
    prop.value.is_some() || prop.body.is_some() || !matches!(value, Value::Null)
}

pub(super) fn module_is_abstract(module: &Module) -> bool {
    module
        .annotations
        .iter()
        .any(|annotation| annotation.name == "pklr:module:Abstract")
}

pub(super) fn is_unresolved_template_error(message: &str) -> bool {
    message.contains("undefined variable")
        || message.contains("field not found")
        || message.starts_with("Cannot find property")
}

/// Pkl's error for reading `name` from an object that has no such member:
/// the object `obj_expr` evaluated to, described by `source`.
/// `module_names` maps module identities to the names the modules declare.
pub(super) fn missing_property_message(
    source: &Option<Arc<ObjectSource>>,
    obj_expr: &Expr,
    name: &str,
    scope: &Scope,
    module_names: &HashMap<String, String>,
) -> String {
    if let Expr::Ident(root) = obj_expr
        && let Some(identity) = scope.module_identity(root)
    {
        return format!(
            "Cannot find property `{name}` in module `{}`.",
            module_name_of(identity, module_names)
        );
    }
    format!(
        "Cannot find property `{name}` in object of type `{}`.",
        object_class_name(source.as_deref(), Some(module_names))
    )
}

/// The name Pkl gives the class of an object `source` describes: `Dynamic`,
/// or `module#Class` for an instance of a class.
fn object_class_name(
    source: Option<&ObjectSource>,
    module_names: Option<&HashMap<String, String>>,
) -> String {
    match source {
        Some(source) if source.kind == ObjectKind::Class => "Class".to_string(),
        Some(ObjectSource {
            type_name: Some(class),
            type_identity,
            ..
        }) => match type_identity
            .as_deref()
            .and_then(|identity| identity.strip_suffix(class.as_str())?.strip_suffix('.'))
        {
            Some(module) => {
                let module = module_names
                    .and_then(|names| names.get(module).map(String::as_str))
                    .unwrap_or_else(|| {
                        let file = module.rsplit(['/', '\\']).next().unwrap_or(module);
                        file.strip_suffix(".pkl").unwrap_or(file)
                    });
                format!("{module}#{class}")
            }
            None => class.clone(),
        },
        _ => "Dynamic".to_string(),
    }
}

/// The name of `value`'s class as Pkl writes it in error messages.
pub(super) fn pkl_class_name(value: &Value) -> String {
    match value {
        Value::Object(_, source) => object_class_name(source.as_deref(), None),
        Value::List(items) => match items.kind() {
            ListKind::List if items.is_int_seq() => "IntSeq".to_string(),
            ListKind::List if items.is_bytes() => "Bytes".to_string(),
            ListKind::List => "List".to_string(),
            ListKind::Listing => "Listing".to_string(),
            ListKind::Set => "Set".to_string(),
            ListKind::Dynamic => "Dynamic".to_string(),
        },
        Value::Lambda(params, ..) => format!("Function{}", params.len()),
        Value::Bool(_) => "Boolean".to_string(),
        value => value_type_name(value).to_string(),
    }
}

/// `value` written on one line as Pkl writes values in error messages.
pub(super) fn pkl_value_text(value: &Value) -> String {
    match value {
        Value::String(s) => format!("{s:?}"),
        Value::List(items) => format!(
            "List({})",
            items
                .iter()
                .map(pkl_value_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(map, source) => {
            let class = object_class_name(source.as_deref(), None);
            let class = class.rsplit('#').next().unwrap_or(&class);
            if map.is_empty() {
                return format!("new {class} {{}}");
            }
            let members = map
                .iter()
                .map(|(name, value)| {
                    format!(
                        "{} = {}",
                        crate::value::display_storage_key(name),
                        pkl_value_text(value)
                    )
                })
                .collect::<Vec<_>>();
            format!("new {class} {{ {} }}", members.join("; "))
        }
        Value::Lambda(params, ..) => format!("new Function{} {{}}", params.len()),
        value => value_to_display(value),
    }
}

/// The name of the module with identity `uri`: the one it declares, or else
/// its file name without the extension.
fn module_name_of<'a>(uri: &'a str, module_names: &'a HashMap<String, String>) -> &'a str {
    if let Some(name) = module_names.get(uri) {
        return name;
    }
    let file = uri.rsplit(['/', '\\']).next().unwrap_or(uri);
    file.strip_suffix(".pkl").unwrap_or(file)
}

/// Which body the entry at `index` of a (possibly merged) object body was
/// written in: entries of one amendment share their captured scope.
pub(super) fn entry_layer(
    entry_scopes: Option<&[Option<Arc<CapturedScope>>]>,
    index: usize,
) -> usize {
    entry_scopes
        .and_then(|scopes| scopes.get(index)?.as_ref())
        .map_or(0, |scope| Arc::as_ptr(scope) as usize)
}

/// Record the members a generator produced in body `layer`. A body may
/// define each member only once, so a member its generators already
/// produced is an error. `body` is the generator's body, which tells
/// properties from entries for the message.
pub(super) fn record_generated_members(
    seen: &mut HashSet<(usize, Arc<str>)>,
    layer: usize,
    members: &ObjectMap,
    body: &[Entry],
    mapping_entries: bool,
) -> Result<()> {
    for name in members.keys() {
        let duplicate = mapping_entries
            && seen.iter().any(|(seen_layer, seen_name)| {
                *seen_layer == layer && crate::value::mapping_storage_keys_equal(seen_name, name)
            });
        if duplicate || !seen.insert((layer, name.clone())) {
            let is_property = body
                .iter()
                .any(|entry| matches!(entry, Entry::Property(prop) if *prop.name == **name));
            let name = if is_property {
                name.to_string()
            } else {
                format!("{:?}", crate::value::display_storage_key(name))
            };
            return Err(Error::Eval(format!(
                "Duplicate definition of member `{name}`."
            )));
        }
    }
    Ok(())
}

pub(super) fn require_str_arg<'a>(args: &'a [Value], idx: usize, method: &str) -> Result<&'a str> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(&**s),
        Some(other) => Err(Error::Eval(format!(
            "{method}() requires a String argument, got {}",
            value_type_name(other)
        ))),
        None => Err(Error::Eval(format!(
            "{method}() requires {} argument(s)",
            idx + 1
        ))),
    }
}

/// An integer as Pkl's error messages show it: digits in groups of three.
pub(super) fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(super) fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "Boolean",
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::String(_) => "String",
        Value::Regex(_) => "Regex",
        Value::Object(..) => "Object",
        Value::List(items) => match items.kind() {
            ListKind::List => "List",
            ListKind::Listing => "Listing",
            ListKind::Set => "Set",
            ListKind::Dynamic => "Dynamic",
        },
        Value::Lambda(..) => "Function",
        Value::Duration(_) => "Duration",
        Value::DataSize(_) => "DataSize",
    }
}

pub(super) fn value_to_key(v: &Value) -> Result<Arc<str>> {
    // Render directives need their text-preserving representation for
    // renderer dispatch. Other compound keys keep main's typed storage so
    // List kind and class identity remain distinct.
    if super::render::kind_of(v) == super::render::Kind::RenderDirective {
        return Ok(super::render::object_key(v));
    }
    if let Some(key) = crate::value::mapping_storage_key(v) {
        return Ok(key);
    }
    match v {
        Value::String(s) => Ok(Arc::clone(s)),
        Value::Int(_) | Value::Bool(_) | Value::Float(_) => unreachable!(),
        Value::Object(_, _)
        | Value::List(_)
        | Value::Lambda(..)
        | Value::Regex(_)
        | Value::Duration(_)
        | Value::DataSize(_)
        | Value::Null => Ok(value_to_display(v).into()),
    }
}

pub(super) fn value_to_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string(),
        Value::Object(map, Some(source))
            if source.type_identity.as_deref() == Some("pkl:base#RegexMatch") =>
        {
            map.get("value").map(value_to_display).unwrap_or_default()
        }
        _ => super::stdlib::render_value(v),
    }
}

pub(super) fn string_literal_type_value(name: &str) -> Option<&str> {
    name.strip_prefix('*')
        .unwrap_or(name)
        .strip_prefix('"')?
        .strip_suffix('"')
}

pub(super) fn is_default_type(ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) | TypeExpr::Constrained(name, _, _) => name.starts_with('*'),
        TypeExpr::Nullable(inner) => is_default_type(inner),
        _ => false,
    }
}

/// The definition of a type alias `pkl:base` declares, written as the type it
/// aliases. Values of these are checked against the definition, which carries
/// the constraints.
pub(super) fn stdlib_alias_source(name: &str) -> Option<&'static str> {
    Some(match name {
        "Int8" => "Int(isBetween(-128, 127))",
        "Int16" => "Int(isBetween(-32768, 32767))",
        "Int32" => "Int(isBetween(-2147483648, 2147483647))",
        "UInt8" => "Int(isBetween(0, 255))",
        "UInt16" => "Int(isBetween(0, 65535))",
        "UInt32" => "Int(isBetween(0, 4294967295))",
        "UInt" => "Int(isPositive)",
        "NonNull" => "Any(!(this is Null))",
        "Uri" => "String",
        "Char" => "String(length == 1)",
        _ => return None,
    })
}

/// Format a TypeExpr for user-facing error messages.
/// The type as an error names it: without the constraints of a constrained
/// type, which are reported on their own when one is violated.
pub(super) fn error_type_expr(ty: &crate::parser::TypeExpr, scope: &Scope) -> String {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => {
            let name = name.trim_start_matches('*');
            // The standard library's aliases are reported as what they alias.
            match stdlib_alias_source(name) {
                Some(source) if scope.get(name).is_none() => {
                    source.split('(').next().unwrap_or(source).to_string()
                }
                _ => name.to_string(),
            }
        }
        TypeExpr::Nullable(inner) => format!("{}?", error_type_expr(inner, scope)),
        TypeExpr::Union(variants) => variants
            .iter()
            .map(|variant| error_type_expr(variant, scope))
            .collect::<Vec<_>>()
            .join("|"),
        TypeExpr::Generic(name, args) => {
            let args: Vec<_> = args.iter().map(|arg| error_type_expr(arg, scope)).collect();
            format!("{}<{}>", name.trim_start_matches('*'), args.join(", "))
        }
        // pkl names the class of a constrained generic, without its arguments.
        TypeExpr::Constrained(base, _, form) => {
            let base = base.trim_start_matches('*');
            // A literal type (`"<"`) or a quoted name is not a generic.
            let generic = if base.starts_with('"') || matches!(form, BaseForm::Quoted) {
                None
            } else {
                base.split_once('<')
            };
            match generic {
                Some((class, _)) => {
                    format!("{class}{}", if base.ends_with('?') { "?" } else { "" })
                }
                None => base.to_string(),
            }
        }
    }
}

pub(super) fn display_type_expr(ty: &crate::parser::TypeExpr) -> String {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Nullable(inner) => format!("{}?", display_type_expr(inner)),
        TypeExpr::Union(variants) => variants
            .iter()
            .map(display_type_expr)
            .collect::<Vec<_>>()
            .join("|"),
        TypeExpr::Generic(name, args) => {
            let args_str: Vec<_> = args.iter().map(display_type_expr).collect();
            format!("{}<{}>", name, args_str.join(", "))
        }
        TypeExpr::Constrained(base, _, _) => format!("{base}(... )"),
    }
}

/// Check if a value matches a Pkl type expression (non-constrained types only).
/// For constrained types, use `Evaluator::eval_type_check` instead.
pub(super) fn value_is_type(val: &Value, ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    // A `List`, `Listing`, `Set` and element-only `Dynamic` are distinct
    // classes that share one representation.
    if let Value::List(items) = val
        && let TypeExpr::Named(name) | TypeExpr::Generic(name, _) = ty
        && string_literal_type_value(name).is_none()
    {
        return list_is_type(items, name.strip_prefix('*').unwrap_or(name));
    }
    match ty {
        TypeExpr::Named(name) if string_literal_type_value(name).is_some() => {
            matches!(val, Value::String(actual) if Some(&**actual) == string_literal_type_value(name))
        }
        TypeExpr::Named(name) => match name.strip_prefix('*').unwrap_or(name) {
            "Null" => is_null_value(val),
            "Boolean" | "Bool" => matches!(val, Value::Bool(_)),
            "Int" => matches!(val, Value::Int(_)),
            "Float" => matches!(val, Value::Float(_)),
            "Number" => matches!(val, Value::Int(_) | Value::Float(_)),
            "String" => matches!(val, Value::String(_)),
            "Regex" => matches!(val, Value::Regex(_)),
            "List" | "Listing" | "Set" => matches!(val, Value::List(_)),
            "Class" => is_class_value(val),
            "Map" | "Mapping" => {
                matches!(val, Value::Object(..))
                    && !is_class_value(val)
                    && !is_untyped_dynamic_object(val)
            }
            "Dynamic" => {
                matches!(val, Value::Object(..)) && !is_class_value(val) && !is_mapping_object(val)
            }
            "Object" => matches!(val, Value::Object(..)) && !is_class_value(val),
            "Function" => matches!(val, Value::Lambda(..)),
            "Duration" => matches!(val, Value::Duration(_)),
            "DataSize" => matches!(val, Value::DataSize(_)),
            "Any" | "unknown" => true,
            "nothing" => false,
            _ => {
                // Unknown type name -- could be a class; treat objects as matching
                matches!(val, Value::Object(..))
            }
        },
        TypeExpr::Nullable(inner) => is_null_value(val) || value_is_type(val, inner),
        TypeExpr::Union(variants) => variants.iter().any(|v| value_is_type(val, v)),
        TypeExpr::Generic(name, _) => {
            // Check the base type, ignore type parameters
            match name.trim_start_matches('*') {
                "List" | "Listing" | "Set" => matches!(val, Value::List(_)),
                "Map" | "Mapping" => {
                    matches!(val, Value::Object(..))
                        && !is_class_value(val)
                        && !is_untyped_dynamic_object(val)
                }
                "Class" => is_class_value(val),
                "Regex" => matches!(val, Value::Regex(_)),
                "Function" | "Mixin" => matches!(val, Value::Lambda(..)),
                // A function type fixes the number of parameters.
                name @ ("Function0" | "Function1" | "Function2" | "Function3" | "Function4"
                | "Function5") => {
                    matches!(val, Value::Lambda(params, ..) if params.len() < 10 && name.as_bytes()[8] == b'0' + params.len() as u8)
                }
                _ => matches!(val, Value::Object(..)),
            }
        }
        TypeExpr::Constrained(base, _, _) => {
            // Just check the base type; constraint requires async eval
            value_is_named_type(val, base)
        }
    }
}

/// Whether `val` is a `Class` value (`getClass()`'s result or a class name),
/// which is of type `Class` and not a `Dynamic`, `Object` or `Mapping`.
pub(super) fn is_class_value(val: &Value) -> bool {
    matches!(val, Value::Object(_, Some(source)) if source.kind == ObjectKind::Class)
}

/// Whether a value of the given list kind is an instance of the built-in
/// class `name`. `Object` is the superclass of `Dynamic` and `Listing`.
pub(super) fn list_is_type(list: &crate::value::ListValue, name: &str) -> bool {
    let kind = list.kind();
    match name {
        "Any" | "unknown" => true,
        // An `IntSeq` is its own class, not a `List`.
        "List" => kind == ListKind::List && !list.is_int_seq() && !list.is_bytes(),
        "IntSeq" => list.is_int_seq(),
        "Bytes" => list.is_bytes(),
        "Listing" => kind == ListKind::Listing,
        "Set" => kind == ListKind::Set,
        "Collection" => {
            matches!(kind, ListKind::List | ListKind::Set) && !list.is_int_seq() && !list.is_bytes()
        }
        "Dynamic" => kind == ListKind::Dynamic,
        "Object" => matches!(kind, ListKind::Listing | ListKind::Dynamic),
        _ => false,
    }
}

pub(super) fn value_is_named_type(val: &Value, name: &str) -> bool {
    if name.ends_with('?') && is_null_value(val) {
        return true;
    }
    value_is_type(
        val,
        &crate::parser::TypeExpr::Named(
            name.trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(name)
                .to_string(),
        ),
    )
}

/// Check a value against a user-defined class that is available in scope.
///
/// Class defaults and instances carry their concrete class and parent class
/// names in `ObjectSource`. Resolve the requested name first so unresolved
/// built-in object types can continue through `value_is_type`.
pub(super) fn value_is_class_type(val: &Value, name: &str, scope: &Scope) -> Option<bool> {
    let expected_name = name.strip_prefix('*').unwrap_or(name);
    // `module` is the type of the enclosing module, never a `Dynamic`.
    // pklr tracks no class for a module's value, so it is told from a class
    // instance by having none.
    if expected_name == "module" {
        return Some(match val {
            // A module's own value, as seen from inside it, has no source.
            Value::Object(_, None) => true,
            Value::Object(_, Some(source)) => {
                source.is_module
                    || (source.kind == ObjectKind::Object
                        && source.type_name.is_none()
                        && source.type_identity.is_none())
            }
            _ => false,
        });
    }
    let Some(Value::Object(_, Some(expected_source))) = resolve_dotted(scope, expected_name) else {
        return None;
    };
    let Some(expected_type_identity) = expected_source.type_identity.as_ref() else {
        // A name bound to an object that is no class (`module`, an imported
        // module) is not a type a `Dynamic` is an instance of.
        return is_untyped_dynamic(val).then_some(false);
    };

    let Value::Object(_, Some(source)) = val else {
        return Some(false);
    };
    // A class value is a `Class`, not an instance of the class it stands for.
    if source.kind == ObjectKind::Class {
        return Some(false);
    }
    let Some(actual_type_identity) = source.type_identity.as_ref() else {
        return Some(false);
    };

    Some(
        actual_type_identity == expected_type_identity
            || source
                .parent_type_identities
                .iter()
                .any(|parent| parent == expected_type_identity),
    )
}

/// Whether `val` is known to be a `Dynamic` object, one made by `new Dynamic`.
/// An object an amendment rebuilt keeps no class name, and can be a `Mapping`.
fn is_untyped_dynamic_object(val: &Value) -> bool {
    matches!(val, Value::Object(_, Some(source)) if source.kind == ObjectKind::Dynamic)
}

fn is_mapping_object(val: &Value) -> bool {
    matches!(val, Value::Object(_, Some(source)) if source.kind == ObjectKind::Mapping)
}

/// Whether `val` is a `Dynamic`: an object that is no class instance, or
/// an element-only `Dynamic`.
fn is_untyped_dynamic(val: &Value) -> bool {
    match val {
        // An object without a source is a `Map`.
        Value::Object(_, source) => source
            .as_deref()
            .is_some_and(|source| source.type_name.is_none() && source.type_identity.is_none()),
        Value::List(items) => items.kind() == ListKind::Dynamic,
        _ => false,
    }
}

/// The type a union's default alternative stands for when it keeps type
/// arguments (`*Listing<Int>`, stored as the name `*Listing<Int>`): the `*`
/// marks the default and is not part of the type.
pub(super) fn unmarked_generic(name: &str) -> Option<crate::parser::TypeExpr> {
    let plain = name.strip_prefix('*')?;
    plain
        .contains('<')
        .then(|| crate::parser::parse_type_name(plain).ok())
        .flatten()
}

pub(super) fn type_is_runtime_checkable(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => {
            if let Some(unmarked) = unmarked_generic(name) {
                return type_is_runtime_checkable(&unmarked, scope);
            }
            let runtime_name = name.strip_prefix('*').unwrap_or(name);
            string_literal_type_value(name).is_some()
                || matches!(
                    runtime_name,
                    "Null"
                        | "Boolean"
                        | "Bool"
                        | "Int"
                        | "Float"
                        | "Number"
                        | "String"
                        | "List"
                        | "Listing"
                        | "Set"
                        | "Collection"
                        | "Map"
                        | "Mapping"
                        | "Object"
                        | "Dynamic"
                        | "Class"
                        | "Function"
                        | "Regex"
                        | "Duration"
                        | "DataSize"
                        | "Any"
                        | "nothing"
                )
                || stdlib_alias_source(runtime_name).is_some()
                || alias_of(scope, runtime_name).is_some()
                || resolve_dotted(scope, runtime_name).is_some()
        }
        TypeExpr::Nullable(inner) => type_is_runtime_checkable(inner, scope),
        TypeExpr::Union(variants) => variants
            .iter()
            .all(|variant| type_is_runtime_checkable(variant, scope)),
        TypeExpr::Generic(_, _) => true,
        TypeExpr::Constrained(base, _, _) => {
            let runtime_name = base
                .trim_start_matches('*')
                .trim_end_matches('?')
                .split('<')
                .next()
                .unwrap_or(base);
            type_is_runtime_checkable(&TypeExpr::Named(runtime_name.into()), scope)
        }
    }
}

pub(super) fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub(super) fn is_null_value(value: &Value) -> bool {
    matches!(value, Value::Null)
}

/// Pkl's `==`. Collections and objects compare by kind and members (see
/// [`ListKind`] and [`ObjectKind`]), sets and mappings without regard to
/// order, classes by identity, and functions only equal themselves.
pub(super) fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (left, right) if is_null_value(left) && is_null_value(right) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
        (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
        (Value::String(a), Value::String(b)) => a == b,
        // Regexes are equal when their patterns are.
        (Value::Regex(a), Value::Regex(b)) => a == b,
        (Value::List(a), Value::List(b)) => {
            a.kind() == b.kind()
                && a.len() == b.len()
                && if a.kind() == ListKind::Set {
                    a.iter().all(|item| set_contains(b, item))
                } else {
                    a.iter().zip(b.iter()).all(|(a, b)| values_eq(a, b))
                }
        }
        (Value::Object(a_map, a_src), Value::Object(b_map, b_src)) => {
            if Arc::ptr_eq(a_map, b_map) && a_src == b_src {
                return true;
            }
            match object_kinds_match(a_src.as_deref(), b_src.as_deref()) {
                Some(true) => true,
                Some(false) => false,
                None => {
                    // Methods are not members, so they take no part.
                    object_elements(a_src).len() == object_elements(b_src).len()
                        && object_elements(a_src)
                            .iter()
                            .zip(object_elements(b_src))
                            .all(|(a, b)| values_eq(a, b))
                        && members(a_map, a_src).count() == members(b_map, b_src).count()
                        && members(a_map, a_src).all(|(key, a)| {
                            b_map
                                .get(key)
                                .or_else(|| {
                                    b_map.iter().find_map(|(stored, value)| {
                                        crate::value::mapping_storage_keys_equal(stored, key)
                                            .then_some(value)
                                    })
                                })
                                .is_some_and(|b| values_eq(a, b))
                        })
                }
            }
        }
        // An element-only `Dynamic` without elements is `new Dynamic {}`.
        (Value::List(list), Value::Object(map, source))
        | (Value::Object(map, source), Value::List(list))
            if list.kind() == ListKind::Dynamic && list.is_empty() =>
        {
            map.is_empty()
                && source.as_deref().is_some_and(|source| {
                    source.kind.dynamic_as_object() == ObjectKind::Object
                        && source.type_name.is_none()
                        && source.elements.is_empty()
                })
        }
        (Value::Lambda(_, a_body, a_captured), Value::Lambda(_, b_body, b_captured)) => {
            Arc::ptr_eq(a_body, b_body) && Arc::ptr_eq(a_captured, b_captured)
        }
        (a, b) => super::stdlib::units_equal(a, b).unwrap_or(false),
    }
}

/// The elements of an object that has both elements and members.
fn object_elements(source: &Option<Arc<ObjectSource>>) -> &[Value] {
    source.as_deref().map_or(&[], |source| &source.elements)
}

/// The members of an object: its map without its methods.
fn members<'a>(
    map: &'a ObjectMap,
    src: &'a Option<Arc<ObjectSource>>,
) -> impl Iterator<Item = (&'a Arc<str>, &'a Value)> {
    map.iter().filter(move |(key, value)| {
        !(matches!(value, Value::Lambda(..))
            && src.as_deref().is_some_and(|src| is_method(src, key)))
    })
}

/// Whether `name` is a method of the object `src` describes.
fn is_method(src: &ObjectSource, name: &str) -> bool {
    src.entries.iter().any(
        |entry| matches!(entry, Entry::Property(prop) if prop.is_method && &*prop.name == name),
    )
}

/// The result of a `map`, `flatMap` or `filter` on a collection of `kind`:
/// a `Set` stays a set (keeping one of equal elements), anything else gives
/// a `List`, as `Listing` has no such methods.
pub(super) fn collection_result(kind: ListKind, items: Vec<Value>) -> Value {
    if kind != ListKind::Set {
        return Value::List(items.into());
    }
    let mut set = Vec::with_capacity(items.len());
    for item in items {
        if !set_contains(&set, &item) {
            set.push(item);
        }
    }
    Value::List(ListValue::new(ListKind::Set, set))
}

/// Whether `set` has an element equal to `item`.
pub(super) fn set_contains(set: &[Value], item: &Value) -> bool {
    set.iter().any(|element| values_eq(element, item))
}

/// Compares what two objects are. `Some(false)` when they are of different
/// kinds or classes, `Some(true)` when both are the same class (which compare
/// by identity), and `None` when their members decide. An object without a
/// source (a module, a `Map`, or a `Dynamic` built by a method) is not told
/// apart from a `Dynamic` or a `Mapping`.
fn object_kinds_match(a: Option<&ObjectSource>, b: Option<&ObjectSource>) -> Option<bool> {
    fn class(src: Option<&ObjectSource>) -> Option<(&String, bool)> {
        let src = src?;
        let name = src.type_name.as_ref()?;
        Some((
            src.type_identity.as_ref().unwrap_or(name),
            src.kind == ObjectKind::Class,
        ))
    }
    match (class(a), class(b)) {
        (Some((a, a_is_class)), Some((b, b_is_class))) => {
            if a != b || a_is_class != b_is_class {
                Some(false)
            } else if a_is_class {
                Some(true)
            } else {
                None
            }
        }
        (Some(_), None) | (None, Some(_)) => Some(false),
        (None, None) => match (
            a.map(|src| src.kind.dynamic_as_object()),
            b.map(|src| src.kind.dynamic_as_object()),
        ) {
            (Some(a), Some(b)) if a != b => Some(false),
            _ => None,
        },
    }
}

pub(super) fn add_values(l: Value, r: Value) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
        (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{a}{b}").into())),
        // `+` takes a `List` or `Set` on each side; the result has the left
        // side's kind. Listings and Dynamics can't be added.
        (Value::List(a), Value::List(b))
            if matches!(a.kind(), ListKind::Listing | ListKind::Dynamic)
                || matches!(b.kind(), ListKind::Listing | ListKind::Dynamic) =>
        {
            let name = |kind| match kind {
                ListKind::Listing => "Listing",
                ListKind::Dynamic => "Dynamic",
                ListKind::Set => "Set",
                _ => "List",
            };
            Err(Error::Eval(format!(
                "Operator `+` is not defined for operand types `{}` and `{}`.",
                name(a.kind()),
                name(b.kind())
            )))
        }
        (Value::List(mut a), Value::List(b)) => {
            let set = a.kind() == ListKind::Set;
            let items = a.make_mut();
            for item in b.iter() {
                // A set's union keeps one of each element.
                if !set || !items.iter().any(|existing| values_eq(existing, item)) {
                    items.push(item.clone());
                }
            }
            Ok(Value::List(a))
        }
        (Value::Object(mut a, _), Value::Object(b, _)) => {
            Arc::make_mut(&mut a).extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
            Ok(Value::Object(a, None))
        }
        (l, r) => Err(Error::Eval(format!("cannot add {l:?} and {r:?}"))),
    }
}

pub(super) fn arithmetic(
    l: Value,
    r: Value,
    fi: impl Fn(i64, i64) -> Result<i64>,
    ff: impl Fn(f64, f64) -> Result<f64>,
) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(fi(a, b)?)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(ff(a, b)?)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(ff(a as f64, b)?)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(ff(a, b as f64)?)),
        (l, r) => Err(Error::Eval(format!(
            "arithmetic type mismatch: {l:?} vs {r:?}"
        ))),
    }
}

pub(super) fn compare(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    Ok(Value::Bool(value_cmp(&l, &r)? == ord))
}

pub(super) fn compare_or_eq(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    let c = value_cmp(&l, &r)?;
    Ok(Value::Bool(c == ord || c == std::cmp::Ordering::Equal))
}

pub(super) fn value_cmp(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => {
            Ok(x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
        }
        (Value::Int(x), Value::Float(y)) => Ok((*x as f64)
            .partial_cmp(y)
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::Float(x), Value::Int(y)) => Ok(x
            .partial_cmp(&(*y as f64))
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::String(x), Value::String(y)) => Ok(x.cmp(y)),
        _ => Err(Error::Eval(format!("cannot compare {a:?} and {b:?}"))),
    }
}

/// `value`, or an instance of it if it is a class. A class used as a default
/// or template stands for an instance with the class's defaults.
pub(super) fn as_instance(value: Value) -> Value {
    match value {
        Value::Object(map, Some(src)) if src.kind == ObjectKind::Class => {
            let mut src = Arc::unwrap_or_clone(src);
            src.kind = ObjectKind::Object;
            Value::Object(map, Some(Arc::new(src)))
        }
        value => value,
    }
}

pub(super) fn merge_values(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut b, base_src), Value::Object(o, overlay_src)) => {
            let b_map = Arc::make_mut(&mut b);
            for (k, v) in o.iter() {
                let storage_key = b_map
                    .get_key_value(k)
                    .map(|(key, _)| key.clone())
                    .or_else(|| {
                        b_map
                            .keys()
                            .find(|stored| crate::value::mapping_storage_keys_equal(stored, k))
                            .cloned()
                    })
                    .unwrap_or_else(|| k.clone());
                if let Some(existing) = b_map.shift_remove(&storage_key) {
                    b_map.insert(storage_key, merge_values(existing, v.clone()));
                } else {
                    b_map.insert(storage_key, v.clone());
                }
            }
            // Keep the base's source (entries/scope for late binding), but when it
            // carries no class identity, inherit the overlay's. This preserves a
            // concrete value's type when it is merged onto a typeless default
            // template (e.g. a union-typed Mapping value).
            let src = match (base_src, overlay_src) {
                (Some(b), Some(o)) if b.type_name.is_none() && o.type_name.is_some() => {
                    let mut nb = (*b).clone();
                    nb.type_name = o.type_name.clone();
                    nb.type_identity = o.type_identity.clone();
                    nb.parent_type_names = o.parent_type_names.clone();
                    nb.parent_type_identities = o.parent_type_identities.clone();
                    Some(Arc::new(nb))
                }
                (Some(b), _) => Some(b),
                (None, o) => o,
            };
            as_instance(Value::Object(b, src))
        }
        (_, overlay) => overlay,
    }
}

/// Classes and type aliases of `pkl:base`, and the keyword types, that a type
/// expression names without a declaration in the module.
pub(super) const STDLIB_TYPE_NAMES: &[&str] = &[
    "Any",
    "Null",
    "Class",
    "TypeAlias",
    "Module",
    "Annotation",
    "Since",
    "Deprecated",
    "AlsoKnownAs",
    "Unlisted",
    "DocExample",
    "SourceCode",
    "ModuleInfo",
    "FileOutput",
    "ModuleOutput",
    "BaseValueRenderer",
    "ConvertProperty",
    "ValueRenderer",
    "BytesRenderer",
    "PcfRenderer",
    "RenderDirective",
    "PcfRenderDirective",
    "JsonRenderer",
    "YamlRenderer",
    "PListRenderer",
    "PropertiesRenderer",
    "Resource",
    "Number",
    "Int",
    "Float",
    "Boolean",
    "String",
    "Regex",
    "RegexMatch",
    "Duration",
    "DataSize",
    "Object",
    "Typed",
    "Dynamic",
    "Listing",
    "Mapping",
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
    "NonNull",
    "Int8",
    "Int16",
    "Int32",
    "UInt8",
    "UInt16",
    "UInt32",
    "UInt",
    "Comparable",
    "Char",
    "Charset",
    "Uri",
    "DurationUnit",
    "DataSizeUnit",
    "Mixin",
    // Keyword types, and the lenient spelling `Bool` pklr accepts.
    "unknown",
    "nothing",
    "Bool",
];

/// The error pkl reports for the first type name in `ty` that names no class
/// or type alias, or `None` when every name resolves. A name that pklr cannot
/// resolve (one inherited from an amended module, say) is reported only when
/// it is a plain or dotted identifier that nothing in `scope` provides.
///
/// The names inside a type alias are checked too, in the scope that declares
/// it. `module_alias` returns the type alias of the imported module with the
/// given identity, and the module's scope (aliases are not members of the
/// module's value).
pub(super) fn unresolved_type_error(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    module_alias: &mut ModuleAliasLookup<'_>,
) -> Option<String> {
    let mut expanding = std::collections::HashSet::new();
    TypeResolver {
        scope,
        module_alias,
        expanding: &mut expanding,
    }
    .check(ty)
}

/// How the type check finds the scope that an alias resolves in.
pub(super) enum AliasLookup<'a> {
    /// The alias `name` of the imported module with this identity.
    Imported { identity: &'a str, name: &'a str },
    /// Whether the imported module with this identity has a saved scope that
    /// does not export `name` (it is `local`): answers `Some` when so.
    ImportedPrivate { identity: &'a str, name: &'a str },
    /// The class `name` of the imported module with this identity. The
    /// lookup answers with the module's scope when it declares the class.
    ImportedClass { identity: &'a str, name: &'a str },
    /// The alias `name` of the module `scope` walks, when that module inherits
    /// it from the module that declares it.
    Inherited { scope: &'a Scope, name: &'a str },
}

pub(super) type ModuleAliasLookup<'a> =
    dyn FnMut(AliasLookup<'_>) -> Option<(crate::parser::TypeExpr, Scope)> + 'a;

struct TypeResolver<'a> {
    scope: &'a Scope,
    module_alias: &'a mut ModuleAliasLookup<'a>,
    /// The aliases being expanded, so a recursive alias ends the walk. Each is
    /// keyed by its module identity and name.
    expanding: &'a mut std::collections::HashSet<String>,
}

impl TypeResolver<'_> {
    fn check(&mut self, ty: &crate::parser::TypeExpr) -> Option<String> {
        use crate::parser::TypeExpr;
        match ty {
            // A plain name is one name, whatever characters its quotes held.
            TypeExpr::Named(name) if !name.starts_with('*') => {
                if name.is_empty() || string_literal_type_value(name).is_some() {
                    None
                } else {
                    self.check_name(name)
                }
            }
            // A constrained type or a default-marked variant holds the text of
            // the type it was made from (`List<Foo>`, `*Int?`), which does not
            // tell a quoted name (`Step?`) from the syntax around a name.
            // Where it cannot be read with certainty, accept it.
            TypeExpr::Named(text) | TypeExpr::Constrained(text, _, _) => {
                let whole = text.trim_start_matches('*');
                if whole.is_empty() || string_literal_type_value(whole).is_some() {
                    return None;
                }
                // A name written in backticks is one name, whatever it holds.
                if matches!(
                    ty,
                    TypeExpr::Constrained(_, _, BaseForm::Quoted | BaseForm::NullableQuoted)
                ) {
                    return self.check_name(whole);
                }
                // A default-marked name is the name as written, quoted or not.
                if matches!(ty, TypeExpr::Named(_)) && self.declares_literally(whole) {
                    return self.check_bare_name(whole);
                }
                if crate::lexer::is_plain_qualified_name(whole) {
                    return self.check_name(whole);
                }
                // A declared name that is no plain identifier could be part
                // of the text, which then cannot be split into names.
                if self.declares_quoted_name_in(whole) {
                    return None;
                }
                match crate::parser::parse_type_name(whole) {
                    Ok(parsed) => self.check(&parsed),
                    Err(_) => None,
                }
            }
            TypeExpr::Nullable(inner) => self.check(inner),
            TypeExpr::Union(variants) => variants.iter().find_map(|variant| self.check(variant)),
            TypeExpr::Generic(name, args) => self
                .check_name(name)
                .or_else(|| args.iter().find_map(|arg| self.check(arg))),
        }
    }

    fn check_name(&mut self, name: &str) -> Option<String> {
        let scope = self.scope;
        if STDLIB_TYPE_NAMES.contains(&name) || name == "module" {
            return None;
        }
        let Some((module, member)) = name.rsplit_once('.') else {
            return self.check_bare_name(name);
        };
        let first = name.split('.').next().unwrap_or(name);
        // `module`, `this` and `outer` stand for the enclosing scopes, whose
        // members must still name a class or alias.
        let enclosing = matches!(first, "module" | "this" | "outer") && !module.contains('.');
        // A type resolves through the import, whatever value shadows its name.
        if !enclosing
            && resolve_dotted(scope, module).is_none()
            && scope.module_identity(module).is_none()
        {
            return Some(format!("Cannot find module import `{first}`."));
        }
        // A `local` class of an imported module is no member an importer can name.
        let is_private = !enclosing
            && scope.module_identity(module).is_some_and(|identity| {
                (self.module_alias)(AliasLookup::ImportedPrivate {
                    identity,
                    name: member,
                })
                .is_some()
            });
        let names_class = (!is_private
            && matches!(
                resolve_dotted(scope, name),
                Some(Value::Object(_, Some(source))) if source.kind == ObjectKind::Class
            ))
            || (!enclosing
                && scope.module_identity(module).is_some_and(|identity| {
                    (self.module_alias)(AliasLookup::ImportedClass {
                        identity,
                        name: member,
                    })
                    .is_some()
                }));
        // A class reached through a property is not named by an import.
        let qualified_by_value = !enclosing
            && scope.module_identity(module).is_none()
            && !module.contains('.')
            && resolve_dotted(scope, module).is_some();
        if names_class && !qualified_by_value {
            return None;
        }
        if enclosing {
            return if scope.get_type_alias(member).is_some() {
                self.check_bare_name(member)
            } else {
                Some(format!("Cannot find type `{member}` in module `{module}`."))
            };
        }
        let imported = scope.module_identity(module).and_then(|identity| {
            (self.module_alias)(AliasLookup::Imported {
                identity,
                name: member,
            })
        });
        let Some((alias, module_scope)) = imported else {
            // A type of a standard library module (`import "pkl:base" as base`)
            // is a marker in the module's value, which has no module identity.
            if scope.module_identity(module).map(String::as_str) == Some("pkl:base")
                && STDLIB_TYPE_NAMES.contains(&member)
            {
                return None;
            }
            // A glob import's alias is a mapping of modules, not a module.
            if scope.module_identity(module).is_none()
                && !module.contains('.')
                && matches!(resolve_dotted(scope, module), Some(Value::Object(_, None)))
            {
                return Some(format!("Expected `{module}` to be a type, but it is not."));
            }
            // A property, class or local is no module import to qualify a type by.
            if scope.module_identity(module).is_none()
                && !module.contains('.')
                && resolve_dotted(scope, module).is_some()
            {
                return Some(format!("Cannot find module import `{module}`."));
            }
            return Some(format!("Cannot find type `{member}` in module `{module}`."));
        };
        // Walk the body in the scope of the module that declares it.
        self.walk_alias(&module_scope, member, &alias)
    }

    /// The error in the body of the alias `name`, which resolves in `scope`.
    fn walk_alias(
        &mut self,
        scope: &Scope,
        name: &str,
        alias: &crate::parser::TypeExpr,
    ) -> Option<String> {
        let key = Self::alias_key(scope, name);
        if !self.expanding.insert(key.clone()) {
            return None;
        }
        let error = TypeResolver {
            scope,
            module_alias: &mut *self.module_alias,
            expanding: &mut *self.expanding,
        }
        .check(alias);
        self.expanding.remove(&key);
        error
    }

    fn alias_key(scope: &Scope, name: &str) -> String {
        alias_key(scope, name)
    }

    /// Whether a class or type alias in scope has a name that is no plain
    /// identifier and occurs in `text`.
    fn declares_quoted_name_in(&self, text: &str) -> bool {
        let mut level = Some(self.scope);
        while let Some(scope) = level {
            let aliases = scope.type_aliases.keys().map(|name| &**name);
            let classes = scope.vars.iter().filter_map(|(name, value)| {
                matches!(value, Value::Object(_, Some(source)) if source.kind == ObjectKind::Class)
                    .then_some(&**name)
            });
            for name in aliases.chain(classes) {
                if !crate::lexer::is_plain_qualified_name(name) && text.contains(name) {
                    return true;
                }
            }
            level = scope.parent.as_deref();
        }
        false
    }

    /// Whether `name`, taken literally, is a class or type alias in scope.
    fn declares_literally(&self, name: &str) -> bool {
        !name.contains('.')
            && (self.scope.get_type_alias(name).is_some()
                || matches!(
                    self.scope.get(name),
                    Some(Value::Object(_, Some(source))) if source.kind == ObjectKind::Class
                ))
    }

    fn check_bare_name(&mut self, name: &str) -> Option<String> {
        let scope = self.scope;
        if scope.get_type_alias(name).is_some() {
            // Walk a chain of aliases that each name another alias in a loop,
            // so a long chain does not use the stack.
            let mut opened = Vec::new();
            let mut current = name;
            let error = loop {
                // An alias the module inherits is walked where it is declared.
                if let Some((alias, module_scope)) = (self.module_alias)(AliasLookup::Inherited {
                    scope,
                    name: current,
                }) {
                    break self.walk_alias(&module_scope, current, &alias);
                }
                let key = Self::alias_key(scope, current);
                if !self.expanding.insert(key.clone()) {
                    break None;
                }
                opened.push(key);
                let alias = scope.get_type_alias(current).expect("alias");
                match alias {
                    crate::parser::TypeExpr::Named(next)
                        if next != current && scope.get_type_alias(next).is_some() =>
                    {
                        current = next;
                    }
                    _ => break self.check(alias),
                }
            };
            for key in opened {
                self.expanding.remove(&key);
            }
            return error;
        }
        // A function captures the marker of an alias, not its definition.
        if scope.get(&type_alias_marker(name)).is_some() {
            return None;
        }
        match scope.get(name) {
            Some(Value::Object(_, Some(source))) if source.kind == ObjectKind::Class => None,
            // The name an import binds is the type of that module.
            Some(Value::Object(..)) if scope.module_identity(name).is_some() => None,
            Some(_) => Some(format!("Expected `{name}` to be a type, but it is not.")),
            None => Some(format!("Cannot find type `{name}`.")),
        }
    }
}

/// The key that tells the alias `name` of the module `scope` walks apart from
/// an alias of the same name in another module. Cycle guards use it.
pub(super) fn alias_key(scope: &Scope, name: &str) -> String {
    let module = scope
        .alias_module
        .as_ref()
        .map(|module| module.display().to_string())
        .unwrap_or_default();
    format!("{module}\0{name}")
}

/// The generic `Listing` or `Mapping` type `ty` stands for, through aliases,
/// nullability and constraints.
pub(super) fn resolve_collection_type(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    depth: usize,
) -> Option<crate::parser::TypeExpr> {
    use crate::parser::TypeExpr;
    if depth > 8 {
        return None;
    }
    match ty {
        TypeExpr::Generic(name, _) if matches!(name.as_str(), "Listing" | "Mapping") => {
            Some(ty.clone())
        }
        TypeExpr::Nullable(inner) => resolve_collection_type(inner, scope, depth + 1),
        TypeExpr::Named(name) => {
            let alias = scope.get_type_alias(name.trim_start_matches('*'))?;
            resolve_collection_type(alias, scope, depth + 1)
        }
        TypeExpr::Constrained(base, _, BaseForm::Text) if base.contains('<') => {
            let base = crate::parser::parse_type_name(base.trim_start_matches('*')).ok()?;
            resolve_collection_type(&base, scope, depth + 1)
        }
        _ => None,
    }
}

/// An untyped `new { ... }` element of a `Listing<E>` takes its parent from
/// `E` when that is a `Listing` or `Mapping` (`Listing<Listing<Int>> = new {
/// new { 1 } }`), as does the value of a `Mapping<K, V>` entry
/// (`Mapping<String, Listing<Int>> = new { ["k"] = new { 1 } }`). Elsewhere it
/// is a `Dynamic`, as pkl does.
pub(super) fn infer_listing_elements(
    ty: &crate::parser::TypeExpr,
    entries: &crate::parser::Body,
    scope: &Scope,
) -> crate::parser::Body {
    infer_collection_body(ty, entries, scope, false, false).unwrap_or_else(|| entries.clone())
}

/// [`infer_listing_elements`] for the body of a property amendment (`xs { ...
/// }`) of declared type `ty`, `None` when nothing needs inferring.
pub(super) fn infer_amendment_members(
    ty: &crate::parser::TypeExpr,
    entries: &crate::parser::Body,
    scope: &Scope,
) -> Option<crate::parser::Body> {
    infer_collection_body(ty, entries, scope, false, false)
}

/// The element type of a `Listing` or the value type of a `Mapping` that is a
/// collection itself: the members of such a body need their parent inferred.
fn nested_collection_type(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
) -> Option<crate::parser::TypeExpr> {
    use crate::parser::TypeExpr;
    let Some(TypeExpr::Generic(name, args)) = resolve_collection_type(ty, scope, 0) else {
        return None;
    };
    let member_ty = match (name.as_str(), args.as_slice()) {
        ("Listing", [element_ty]) => element_ty,
        ("Mapping", [_, value_ty]) => value_ty,
        _ => return None,
    };
    resolve_collection_type(member_ty, scope, 0).map(|_| member_ty.clone())
}

/// `entries` with the members of a body of type `ty` rewritten, or `None` when
/// the type has no collection members to infer. `amended` is set for the body
/// of an entry amendment (`["k"] { ... }`), whose members have no value types
/// of their own to fall back on.
fn infer_collection_body(
    ty: &crate::parser::TypeExpr,
    entries: &crate::parser::Body,
    scope: &Scope,
    amended: bool,
    declared: bool,
) -> Option<crate::parser::Body> {
    let member_ty = nested_collection_type(ty, scope)?;
    let is_listing = matches!(
        resolve_collection_type(ty, scope, 0),
        Some(crate::parser::TypeExpr::Generic(name, _)) if name == "Listing"
    );
    Some(infer_members_in(
        entries, &member_ty, is_listing, amended, scope, declared,
    ))
}

/// Rewrite the untyped `new` members of `entries` (elements of a listing,
/// entry values of a mapping), including those inside `for` and `when`
/// generator bodies and the bodies of entry amendments (`["k"] { ... }`).
fn infer_members_in(
    entries: &crate::parser::Body,
    member_ty: &crate::parser::TypeExpr,
    is_listing: bool,
    amended: bool,
    scope: &Scope,
    declared: bool,
) -> crate::parser::Body {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::Elem(Expr::New(None, body, _)) if is_listing => {
                Entry::Elem(typed_member(member_ty, body.clone(), scope, declared))
            }
            // A listing index assignment (`[0] = new { ... }`) takes the
            // element type like a plain element does.
            Entry::DynProperty(key, Expr::New(None, body, _)) if is_listing => Entry::DynProperty(
                key.clone(),
                typed_member(member_ty, body.clone(), scope, declared),
            ),
            // A mapping value whose own members need inferring stays an entry
            // body, which amends the mapping's `default`, with its members
            // inferred. Other values keep their untyped `new`, which honors
            // the `default` too.
            Entry::DynProperty(key, Expr::New(None, body, _))
                if !is_listing
                    && !amended
                    && nested_collection_type(member_ty, scope).is_some() =>
            {
                let body = infer_collection_body(member_ty, body, scope, false, declared)
                    .unwrap_or_else(|| body.clone());
                let is_listing_value = matches!(
                    resolve_collection_type(member_ty, scope, 0),
                    Some(crate::parser::TypeExpr::Generic(name, _)) if name == "Listing"
                );
                if is_listing_value {
                    Entry::DynProperty(key.clone(), Expr::ObjectBody(body))
                } else {
                    Entry::DynProperty(
                        key.clone(),
                        typed_member(member_ty, body.clone(), scope, declared),
                    )
                }
            }
            // Any value of an amended body: it has no value types of its own
            // to fall back on.
            Entry::DynProperty(key, Expr::New(None, body, _)) if !is_listing && amended => {
                Entry::DynProperty(
                    key.clone(),
                    typed_member(member_ty, body.clone(), scope, declared),
                )
            }
            // The body of an amended entry has no parent to amend, so the
            // entry is a new value of the member type.
            Entry::DynProperty(key, Expr::ObjectBody(body)) if !is_listing && amended => {
                Entry::DynProperty(
                    key.clone(),
                    typed_member(member_ty, body.clone(), scope, declared),
                )
            }
            // An index amendment (`[0] { ... }`) of a listing amends an
            // element, whose own members are inferred from the element type.
            // The element exists, so its entry bodies amend rather than build.
            Entry::DynProperty(key, Expr::ObjectBody(body)) => {
                match infer_collection_body(member_ty, body, scope, !is_listing, declared) {
                    Some(body) => Entry::DynProperty(key.clone(), Expr::ObjectBody(body)),
                    None => entry.clone(),
                }
            }
            // An untyped `new` that an `if` or `let` picks as the member takes
            // the member type as well, when that is a collection.
            Entry::Elem(expr) if is_listing && selects_untyped_new(expr) => {
                Entry::Elem(infer_selected_new(expr, member_ty, false, scope, declared))
            }
            Entry::DynProperty(key, expr)
                if (is_listing || !amended) && selects_untyped_new(expr) =>
            {
                Entry::DynProperty(
                    key.clone(),
                    infer_selected_new(expr, member_ty, !is_listing, scope, declared),
                )
            }
            Entry::ForGenerator(generator) => {
                let mut generator = generator.clone();
                generator.body = infer_members_in(
                    &generator.body,
                    member_ty,
                    is_listing,
                    amended,
                    scope,
                    declared,
                );
                Entry::ForGenerator(generator)
            }
            Entry::WhenGenerator(generator) => {
                let mut generator = generator.clone();
                generator.body = infer_members_in(
                    &generator.body,
                    member_ty,
                    is_listing,
                    amended,
                    scope,
                    declared,
                );
                generator.else_body = generator.else_body.as_ref().map(|body| {
                    infer_members_in(body, member_ty, is_listing, amended, scope, declared)
                });
                Entry::WhenGenerator(generator)
            }
            other => other.clone(),
        })
        .collect::<Vec<_>>()
        .into()
}

/// Whether `expr` is an `if` or `let` (not a plain `new`) whose result may be
/// an untyped `new`.
fn selects_untyped_new(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::If(..) | Expr::Let(..) | Expr::Trace(..) | Expr::TemplateLocals(..)
    )
}

/// `expr` with the untyped `new` results of its `if` and `let` branches
/// inferred from `member_ty`, which must be a `Listing` or `Mapping`. For a
/// `Listing` mapping value the results stay untyped, so the mapping amends its
/// `default` with them (see `eval_mapping_body`), with their members inferred.
fn infer_selected_new(
    expr: &Expr,
    member_ty: &crate::parser::TypeExpr,
    mapping_value: bool,
    scope: &Scope,
    declared: bool,
) -> Expr {
    let mut expr = expr.clone();
    let Some(crate::parser::TypeExpr::Generic(kind, _)) =
        resolve_collection_type(member_ty, scope, 0)
    else {
        return expr;
    };
    crate::parser::rewrite_untyped_result_new(&mut expr, &mut |entries| {
        if mapping_value && kind == "Listing" {
            let entries = infer_collection_body(member_ty, &entries, scope, false, declared)
                .unwrap_or(entries);
            Expr::New(None, entries, Vec::new())
        } else {
            typed_member(member_ty, entries, scope, declared)
        }
    });
    expr
}

/// The type a quoted base of a constrained type stands for: its name, as the
/// parser reads a plain name, and nullable when the form says null matches.
/// `None` for the text of a type, which readers take apart themselves.
pub(super) fn quoted_base_type(base: &str, form: BaseForm) -> Option<crate::parser::TypeExpr> {
    use crate::parser::TypeExpr;
    let name = TypeExpr::Named(base.trim_start_matches('*').to_string());
    match form {
        BaseForm::Text => None,
        BaseForm::Quoted => Some(name),
        BaseForm::NullableQuoted => Some(TypeExpr::Nullable(Box::new(name))),
    }
}

/// The name in the text of the base of a constrained type (without a default
/// marker or a nullable suffix) and whether the type is nullable. A quoted
/// base is one name, so a trailing `?` belongs to it.
pub(super) fn constrained_name(base: &str, form: BaseForm) -> (&str, bool) {
    let base = base.trim_start_matches('*');
    match form {
        BaseForm::Text => (base.trim_end_matches('?'), base.ends_with('?')),
        BaseForm::Quoted => (base, false),
        BaseForm::NullableQuoted => (base, true),
    }
}

/// The class `name` names in `scope`, exactly as written.
fn declared_class(scope: &Scope, name: &str) -> Option<Value> {
    resolve_dotted(scope, name).filter(
        |value| matches!(value, Value::Object(_, Some(source)) if source.kind == ObjectKind::Class),
    )
}

/// Whether `ty` is built only of `Listing`s down to a type that is not a
/// collection (`Listing<Listing<T>>`), so that an element class saved for it
/// covers every level. A `Mapping` on the way needs its values inferred.
pub(super) fn is_listing_chain(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    let mut ty = ty.clone();
    for _ in 0..64 {
        match resolve_collection_type(&ty, scope, 0) {
            None => return true,
            Some(crate::parser::TypeExpr::Generic(name, args)) if name == "Listing" => {
                match args.as_slice() {
                    [arg] => ty = arg.clone(),
                    _ => return true,
                }
            }
            Some(_) => return false,
        }
    }
    true
}

/// The expression for the untyped `new` member of declared type `member_ty`.
/// With `declared` set, `scope` is where the type was declared: the class it
/// names is resolved there and bound by value (under a key made from the class
/// itself, so same-named classes of different modules stay apart), so that the
/// amending scope, which may not see it or may mean something else by its name,
/// is not consulted.
fn typed_member(
    member_ty: &crate::parser::TypeExpr,
    body: crate::parser::Body,
    scope: &Scope,
    declared: bool,
) -> Expr {
    if !declared {
        return Expr::InferredNew(member_ty.clone(), body);
    }
    let Ok((name, params)) = inferred_new_type(member_ty, scope, 0) else {
        return Expr::InferredNew(member_ty.clone(), body);
    };
    // A member that is a collection itself has members of its own to build
    // from the declared type.
    let body = if nested_collection_type(member_ty, scope).is_some() {
        infer_collection_body(member_ty, &body, scope, false, true).unwrap_or(body)
    } else {
        body
    };
    let mut bindings: Vec<(Arc<str>, Value)> = Vec::new();
    let mut bind = |written: &str| -> String {
        let plain = written.trim_start_matches('*').trim_end_matches('?');
        match resolve_dotted(scope, plain) {
            Some(class @ Value::Object(_, Some(_))) if matches!(&class, Value::Object(_, Some(source)) if source.type_name.is_some()) =>
            {
                let Value::Object(_, Some(source)) = &class else {
                    unreachable!()
                };
                let key = format!("\0declared:{:p}", Arc::as_ptr(source));
                if !bindings.iter().any(|(bound, _)| **bound == *key) {
                    bindings.push((key.as_str().into(), class.clone()));
                    bindings.push((type_alias_marker(&key).as_str().into(), Value::Null));
                }
                // The `*` default marker selects the alternative, so it stays
                // with the name.
                if written.starts_with('*') {
                    format!("*{key}")
                } else {
                    key
                }
            }
            _ => written.to_string(),
        }
    };
    let name = bind(&name);
    let params = params.iter().map(|param| bind(param)).collect();
    Expr::TemplateLocals(
        crate::parser::LocalBindings(Arc::new(bindings)),
        Box::new(Expr::New(Some(name), body, params)),
    )
}

/// [`infer_amendment_members`] for a type read in the scope that declared it:
/// the members are built from the classes resolved there.
pub(super) fn infer_declared_members(
    ty: &crate::parser::TypeExpr,
    entries: &crate::parser::Body,
    declaring: &Scope,
) -> Option<crate::parser::Body> {
    infer_collection_body(ty, entries, declaring, false, true)
}

/// The error of reading a declared property that never received a value.
pub(super) fn undefined_property_message(name: &str) -> String {
    format!("Tried to read property `{name}` but its value is undefined.")
}

/// Whether a property declared with this type and no value stays undefined:
/// its type supplies no default. Only plain primitives (and unions of them or
/// of string literals) are recognised, so a type this cannot resolve is never
/// reported as lacking one.
pub(super) fn type_lacks_default(ty: &crate::parser::TypeExpr, scope: &Scope) -> bool {
    use crate::parser::TypeExpr;
    fn primitive(name: &str, scope: &Scope) -> bool {
        matches!(name, "String" | "Int" | "Float" | "Number" | "Boolean")
            && alias_of(scope, name).is_none()
            && !names_user_class(name, scope)
    }
    match ty {
        TypeExpr::Named(name) => primitive(name, scope),
        TypeExpr::Constrained(name, _, BaseForm::Text) => primitive(name, scope),
        TypeExpr::Union(variants) => variants.iter().all(|variant| match variant {
            TypeExpr::Named(name) => {
                primitive(name, scope) || string_literal_type_value(name).is_some()
            }
            _ => false,
        }),
        _ => false,
    }
}
