//! Checking values against declared types and their constraints.

use super::render::display_value;
use super::*;
use crate::parser::{BaseForm, TypeExpr};

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
        scope.vars = Arc::clone(&snapshot.values);
        scope.type_aliases = Arc::clone(&snapshot.type_aliases);
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
        // An imported `m.A` resolves where `m` declares it. The importer may
        // also bind `m.A` rewritten for its own scope, for what needs only the
        // shape of the type; that one drops the constraint and the names `m`
        // itself imports.
        if let Some((root, member)) = name.split_once('.')
            && !member.contains('.')
            && let Some(identity) = scope.module_identity(root).cloned()
            && let Some(module_scope) = self.module_type_scope(&identity)
            && let Some(alias) = module_scope.get_type_alias(member).cloned()
        {
            return Some((alias, Some(module_scope)));
        }
        scope
            .get_type_alias(name)
            .map(|alias| (alias.clone(), None))
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
            TypeExpr::Constrained(base, _, form) => {
                named(self, constrained_name(base, *form).0, seen)
            }
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
            TypeExpr::Constrained(base, _, form) => constrained_name(base, *form).0,
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
            TypeExpr::Constrained(base, _, form) => {
                // A quoted name is exact: a `?` at its end is part of the name,
                // so only an alias it names can make it nullable.
                match form {
                    BaseForm::NullableQuoted => return true,
                    BaseForm::Quoted => {
                        let name = base.trim_start_matches('*');
                        return match self.resolve_unvisited_alias(name, scope, seen) {
                            Some((alias, alias_scope)) => {
                                let alias_scope = alias_scope.as_ref().unwrap_or(scope);
                                self.selected_type_nullable(&alias, alias_scope, seen)
                            }
                            None => false,
                        };
                    }
                    BaseForm::Text => {}
                }
                let (name, nullable) = constrained_name(base, *form);
                if nullable {
                    return true;
                }
                name
            }
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
            TypeExpr::Constrained(base, _, form) => {
                // A quoted name is one name, not a type to parse.
                if let Some(quoted) = quoted_base_type(base, *form) {
                    return self.union_amend_state_in(&quoted, scope, nullable, seen);
                }
                let (name, base_nullable) = constrained_name(base, *form);
                let nullable = nullable || base_nullable;
                match crate::parser::parse_type_name(name) {
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
        Err(Error::Eval(self.type_mismatch_message(
            ty,
            value,
            Some(&prop.name),
            scope,
        )))
    }

    /// The error for the first name in `ty` that `is` and `as` cannot resolve.
    pub(super) fn unresolved_type(
        &mut self,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
    ) -> Option<String> {
        unresolved_type_error(ty, scope, &mut |lookup| match lookup {
            AliasLookup::Imported { identity, name } => self.module_alias(identity, name),
            AliasLookup::ImportedPrivate { identity, name } => {
                let key = self.module_scope_key(identity)?;
                (self.module_scopes.contains_key(&key) && !self.module_exports(&key, name))
                    .then(|| (TypeExpr::Named(name.to_string()), Scope::default()))
            }
            AliasLookup::ImportedClass { identity, name } => self
                .module_class_scope(identity, name)
                .map(|scope| (TypeExpr::Named(name.to_string()), scope)),
            AliasLookup::Inherited { scope, name } => self.inherited_alias(scope, name),
        })
    }

    /// The type alias `name` that the module with the given identity (the
    /// canonical path or URL an import records) declares, with the scope its
    /// definition resolves in. A module's aliases are no members of its value,
    /// so they are found in its final scope.
    fn module_alias(
        &mut self,
        identity: &str,
        name: &str,
    ) -> Option<(crate::parser::TypeExpr, Scope)> {
        let key = self.module_scope_key(identity)?;
        self.module_exports(&key, name).then_some(())?;
        self.alias_in_module(&key, name)
    }

    /// Whether the module with the scope key `key` exports the class or type
    /// alias `name`: a `local` one is no member an importer can name.
    fn module_exports(&self, key: &Path, name: &str) -> bool {
        self.module_scopes
            .get(key)
            .is_some_and(|snapshot| snapshot.exported_types.contains(name))
    }

    /// Check `val` against `m.Name` when a value shadows the import `m`: a type
    /// resolves through the import, never through a value of that name.
    /// `None` when `m` is the import itself or `Name` is not an exported
    /// class or alias of it.
    fn shadowed_import_check(
        &mut self,
        val: &Value,
        name: &str,
        scope: &Scope,
        depth: usize,
        resolving: &mut Vec<String>,
    ) -> Result<Option<bool>> {
        let Some((module, member)) = name.rsplit_once('.') else {
            return Ok(None);
        };
        if module.contains('.') {
            return Ok(None);
        }
        let Some(identity) = scope.module_identity(module).cloned() else {
            return Ok(None);
        };
        if identity.starts_with("pkl:") {
            return Ok(None);
        }
        if let Some((alias, module_scope)) = self.module_alias(&identity, member) {
            return self
                .eval_alias_check(val, name, &alias, &module_scope, depth, resolving)
                .map(Some);
        }
        Ok(self
            .module_class_scope(&identity, member)
            .map(|module_scope| value_is_class_type(val, member, &module_scope).unwrap_or(false)))
    }

    /// The scope of the imported module with the given identity, when it
    /// declares the class `name`.
    fn module_class_scope(&mut self, identity: &str, name: &str) -> Option<Scope> {
        let key = self.module_scope_key(identity)?;
        self.module_exports(&key, name).then_some(())?;
        let module_scope = self.layered_module_scope(&key);
        matches!(
            module_scope.get(name),
            Some(Value::Object(_, Some(source))) if source.kind == ObjectKind::Class
        )
        .then_some(module_scope)
    }

    /// The scope the saved bindings of the module with the key `key` make,
    /// kept for the evaluation: building it copies every binding.
    fn layered_module_scope(&mut self, key: &Path) -> Scope {
        if let Some(scope) = self.layered_scopes.get(key) {
            return scope.clone();
        }
        let mut scope = Scope::default();
        self.layer_evaluated_module_scope(key, &mut scope);
        self.layered_scopes.insert(key.to_path_buf(), scope.clone());
        scope
    }

    /// The type alias `name` of the module with the scope key `module`, with
    /// the scope its definition resolves in. Every path that expands an
    /// alias (the name check, value matching, a nested alias) goes through
    /// here, so an alias the module inherits is always expanded in the module
    /// that declares it, whose imports its names refer to.
    fn alias_in_module(
        &mut self,
        module: &Path,
        name: &str,
    ) -> Option<(crate::parser::TypeExpr, Scope)> {
        let definer = self
            .module_scopes
            .get(module)
            .and_then(|snapshot| snapshot.alias_definers.get(name))
            .map_or_else(|| module.to_path_buf(), Clone::clone);
        let mut module_scope = self.layered_module_scope(&definer);
        module_scope.alias_module = Some(Arc::new(definer));
        let alias = module_scope.get_type_alias(name)?.clone();
        Some((alias, module_scope))
    }

    /// The alias `name` in the declaring module's scope, when `scope` walks a
    /// module that inherits `name` from another module. `None` means the
    /// alias is `scope`'s own.
    fn inherited_alias(
        &mut self,
        scope: &Scope,
        name: &str,
    ) -> Option<(crate::parser::TypeExpr, Scope)> {
        let module = scope.alias_module.as_ref()?;
        // The nearest declaration wins over an inherited one.
        if !scope.alias_declared_at_root(name) {
            return None;
        }
        self.module_scopes
            .get(&**module)?
            .alias_definers
            .contains_key(name)
            .then(|| self.alias_in_module(module, name))
            .flatten()
    }

    /// The key of the final scope of the module with the given identity.
    fn module_scope_key(&mut self, identity: &str) -> Option<PathBuf> {
        if let Some(key) = self.scope_keys.get(identity)
            && self.module_scopes.contains_key(key)
        {
            return Some(key.clone());
        }
        let keys: Vec<PathBuf> = self.module_scopes.keys().cloned().collect();
        let found = keys.into_iter().find(|key| {
            let key_identity = if key.to_string_lossy().contains("://") {
                key.display().to_string()
            } else {
                self.canonicalize_io(key)
                    .unwrap_or_else(|_| key.clone())
                    .display()
                    .to_string()
            };
            key_identity == identity
        });
        if let Some(key) = &found {
            self.scope_keys.insert(identity.to_string(), key.clone());
        }
        found
    }

    /// The type alias `module.member` of an imported module, with the scope
    /// that its definition resolves in.
    fn imported_alias(
        &mut self,
        scope: &Scope,
        name: &str,
    ) -> Option<(crate::parser::TypeExpr, Scope)> {
        let (module, member) = name.rsplit_once('.')?;
        if module.contains('.') || scope.get(module).is_none() {
            return None;
        }
        self.module_alias(scope.module_identity(module)?, member)
    }

    /// Check if a value matches a type expression, including constraint evaluation.
    pub(super) fn eval_type_check(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<bool> {
        self.type_violation = None;
        self.eval_type_check_in(val, ty, scope, depth, &mut Vec::new())
    }

    /// The error for `val` failing `ty`. Like a declared property, a
    /// collection argument is faulted on its first element that is not
    /// of the element type, not on the collection.
    pub(super) fn mismatch_naming_element(
        &mut self,
        ty: &crate::parser::TypeExpr,
        val: &Value,
        scope: &Scope,
        _depth: usize,
    ) -> String {
        self.type_mismatch_message(ty, val, None, scope)
    }

    /// The error for the first element, key, value or pair member of `val`
    /// that is not of the type argument of `ty`, when `val` is the right
    /// kind of collection to begin with.
    fn element_mismatch(
        &mut self,
        ty: &crate::parser::TypeExpr,
        val: &Value,
        scope: &Scope,
    ) -> Option<String> {
        use crate::parser::TypeExpr;
        let (name, args) = match ty {
            TypeExpr::Nullable(inner) if !is_null_value(val) => {
                return self.element_mismatch(inner, val, scope);
            }
            TypeExpr::Generic(name, args) => (name.trim_start_matches('*'), args),
            // The base of a constrained type names the element, as for a
            // declared property.
            TypeExpr::Constrained(base, ..) => {
                let base = crate::parser::parse_type_name(base).ok()?;
                if matches!(base, TypeExpr::Constrained(..)) {
                    return None;
                }
                return self.element_mismatch(&base, val, scope);
            }
            _ => return None,
        };
        if !value_is_type(val, ty) {
            return None;
        }
        // A type parameter or `Any` accepts everything.
        let modeled = |ty: Option<&TypeExpr>| {
            ty.filter(|ty| !type_is_any(ty) && type_is_runtime_checkable(ty, scope))
                .cloned()
        };
        let mut pairs: Vec<(Value, TypeExpr)> = Vec::new();
        match (name, val) {
            ("List" | "Listing" | "Set" | "Collection", Value::List(items)) => {
                if let Some(element) = modeled(args.first()) {
                    pairs.extend(items.iter().map(|item| (item.clone(), element.clone())));
                }
            }
            ("Pair", Value::Object(members, Some(source)))
                if source.type_identity.as_deref() == Some("pkl:base#Pair") =>
            {
                for (member, ty) in [("first", args.first()), ("second", args.get(1))] {
                    if let (Some(ty), Some(item)) = (modeled(ty), members.get(member)) {
                        pairs.push((item.clone(), ty));
                    }
                }
            }
            ("Map" | "Mapping", Value::Object(entries, source))
                if source
                    .as_ref()
                    .is_none_or(|source| source.type_name.is_none()) =>
            {
                let (key_ty, value_ty) = (modeled(args.first()), modeled(args.get(1)));
                for (stored_key, entry) in entries.iter() {
                    if let Some(key_ty) = &key_ty {
                        pairs.push((self.entry_key_value(stored_key), key_ty.clone()));
                    }
                    if let Some(value_ty) = &value_ty {
                        pairs.push((entry.clone(), value_ty.clone()));
                    }
                }
            }
            _ => {}
        }
        for (item, item_ty) in pairs {
            self.type_violation = None;
            if let Ok(false) = self.eval_argument_check(&item, &item_ty, scope, 0, &mut Vec::new())
            {
                return Some(self.type_mismatch_message(&item_ty, &item, None, scope));
            }
        }
        None
    }

    /// The key a `Map` or `Mapping` entry was stored under, as a value.
    pub(super) fn entry_key_value(&self, stored_key: &str) -> Value {
        self.mapping_key_values
            .get(stored_key)
            .cloned()
            .unwrap_or_else(|| match render::class_key_name(stored_key) {
                // `[String] = 1` keys by a class, which no evaluated key
                // value was remembered for.
                Some(class) => stdlib::class_value(class, format!("pkl:base#{class}")),
                None => crate::value::mapping_storage_value(stored_key),
            })
    }

    /// The error for `val` failing the check against `ty`: the constraint the
    /// check found violated, or else the type the value does not have.
    pub(super) fn type_mismatch_message(
        &mut self,
        ty: &crate::parser::TypeExpr,
        val: &Value,
        property: Option<&str>,
        scope: &Scope,
    ) -> String {
        // pkl shows the source line of the property after the message, which
        // `catch` does not report.
        let location = property
            .map(|name| format!("{PROPERTY_CONTEXT}{name}'"))
            .unwrap_or_default();
        if let Some((text, violating)) = self.type_violation.take() {
            return format!(
                "Type constraint `{text}` violated.\nValue: {}{location}",
                violating_value_display(&violating, &text)
            );
        }
        if let Some(message) = self.element_mismatch(ty, val, scope) {
            return format!("{message}{location}");
        }
        let expected = top_level_error_type(ty, scope);
        if is_null_value(val) && !matches!(ty, crate::parser::TypeExpr::Union(_)) {
            return format!("Expected value of type `{expected}`, but got `null`.{location}");
        }
        if is_null_value(val) {
            return format!(
                "Expected value of type `{expected}`, but got `null`.\nValue: null{location}"
            );
        }
        format!(
            "Expected value of type `{expected}`, but got type `{}`.\nValue: {}{location}",
            pkl_class_name(val),
            display_value(val)
        )
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
        // An alias is told apart from one of the same name in another module.
        let key = alias_key(scope, name);
        if resolving.contains(&key) {
            return Err(Error::Eval(format!("type alias '{name}' refers to itself")));
        }
        let open = resolving.len();
        resolving.push(key);
        // Long chains (`A0 = A1`, …) are valid Pkl and must not consume the
        // Rust call stack once per alias.  Flatten the simple-name part here;
        // compound targets still go through the regular checker below.
        let mut target = resolved.clone();
        let mut redirected: Option<Scope> = None;
        while let crate::parser::TypeExpr::Named(next) = &target {
            let current = redirected.as_ref().unwrap_or(scope);
            let (next_target, next_scope) = match self.inherited_alias(current, next) {
                Some((alias, module_scope)) => (alias, Some(module_scope)),
                None => match current.get_type_alias(next) {
                    Some(alias) => (alias.clone(), None),
                    None => break,
                },
            };
            let next_key = alias_key(next_scope.as_ref().unwrap_or(current), next);
            if resolving.contains(&next_key) {
                resolving.truncate(open);
                return Err(Error::Eval(format!("type alias '{next}' refers to itself")));
            }
            resolving.push(next_key);
            target = next_target;
            if next_scope.is_some() {
                redirected = next_scope;
            }
        }
        let scope = redirected.as_ref().unwrap_or(scope);
        let result = self.eval_type_check_in(val, &target, scope, depth + 1, resolving);
        resolving.truncate(open);
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

    /// Check `val` against an alias an imported module declares, as that
    /// module's own scope reads it: its constraint calls the module's
    /// functions and names its constants. The importer's binding of the
    /// alias drops constraints, so it only serves what needs no check.
    fn check_imported_alias(
        &mut self,
        val: &Value,
        name: &str,
        args: &[crate::parser::TypeExpr],
        scope: &Scope,
        depth: usize,
        resolving: &[String],
    ) -> Result<Option<bool>> {
        let Some((root, member)) = name.split_once('.') else {
            return Ok(None);
        };
        // Only an alias the importer bound has a constraint to keep.
        if scope.get_type_alias(&format!("{root}#{member}")).is_none() {
            return Ok(None);
        }
        let Some(identity) = scope.module_identity(root).cloned() else {
            return Ok(None);
        };
        let Some(declaring) = self.module_type_scope(&identity) else {
            return Ok(None);
        };
        if !args.is_empty()
            && let Some(accepted) =
                self.check_generic_alias(val, member, args, (&declaring, scope), depth, resolving)?
        {
            return Ok(Some(accepted));
        }
        // Names that name an alias are tracked per module: another module's
        // `A` is not this one.
        let key = format!("{identity}|{member}");
        if resolving.contains(&key) {
            return Err(Error::Eval(format!("type alias '{name}' refers to itself")));
        }
        let mut inner: Vec<String> = resolving
            .iter()
            .filter(|seen| seen.contains('|'))
            .cloned()
            .collect();
        inner.push(key);
        let ty = TypeExpr::Named(member.to_string());
        self.eval_type_check_in(val, &ty, &declaring, depth + 1, &mut inner)
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
        match ty {
            TypeExpr::Named(name) => {
                if let Some(unmarked) = unmarked_generic(name) {
                    return self.eval_type_check_in(val, &unmarked, scope, depth, resolving);
                }
                if let Some(expected) = string_literal_type_value(name) {
                    return Ok(matches!(val, Value::String(actual) if &**actual == expected));
                }
                // Check if name is a type alias; if so, resolve to the aliased type
                // A `*` marks a union's default alternative and is not part of
                // the type.
                let name = name.trim_start_matches('*');
                if let Some((resolved, module_scope)) = self.inherited_alias(scope, name) {
                    return self.eval_alias_check(
                        val,
                        name,
                        &resolved,
                        &module_scope,
                        depth,
                        resolving,
                    );
                }
                if let Some(accepted) =
                    self.check_imported_alias(val, name, &[], scope, depth, resolving)?
                {
                    return Ok(accepted);
                }
                // The type of the enclosing module, unless an alias of that
                // name shadows it.
                if name == "module"
                    && scope.get_type_alias(name).is_none()
                    && let Some(matches) = value_is_class_type(val, name, scope)
                {
                    return Ok(matches);
                }
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                // An alias of the standard library carries its constraints.
                if let Some(source) = stdlib_alias_source(name)
                    && scope.get(name).is_none()
                    && let Ok(resolved) = crate::parser::parse_type_name(source)
                {
                    // The alias is the standard library's: its base type and
                    // predicates are not the caller's classes or functions.
                    return self.eval_type_check_in(
                        val,
                        &resolved,
                        &Scope::default(),
                        depth + 1,
                        resolving,
                    );
                }
                // A scalar of the built-in type of that name matches without
                // resolving the name as a class, which only a class instance
                // could match, unless a user class of that name shadows it.
                if scalar_is_builtin_type(val, name) && scope.get(name).is_none() {
                    return Ok(true);
                }
                if let Some(accepted) =
                    self.shadowed_import_check(val, name, scope, depth, resolving)?
                {
                    return Ok(accepted);
                }
                if let Some(matches) = value_is_class_type(val, name, scope) {
                    return Ok(matches);
                }
                // An alias of an imported module is no member of its value.
                if name.contains('.')
                    && resolve_dotted(scope, name).is_none()
                    && let Some((resolved, module_scope)) = self.imported_alias(scope, name)
                {
                    return self.eval_alias_check(
                        val,
                        name,
                        &resolved,
                        &module_scope,
                        depth,
                        resolving,
                    );
                }
                // A type of a standard library module (`base.Int`) is the
                // type without its qualifier.
                if let Some(member) = stdlib_qualified_member(scope, name) {
                    return Ok(value_is_type(
                        val,
                        &crate::parser::TypeExpr::Named(member.to_string()),
                    ));
                }
                // Otherwise, plain type check
                Ok(value_is_type(val, ty))
            }
            TypeExpr::Constrained(base, constraint, form) => {
                // A name written in backticks (`C?`) is that one name, and
                // the same text without them is the nullable `C`.
                let (quoted_name, nullable) = match form {
                    BaseForm::Text => (None, base.ends_with('?')),
                    BaseForm::Quoted => (Some(base.trim_start_matches('*')), false),
                    BaseForm::NullableQuoted => (Some(base.trim_start_matches('*')), true),
                };
                // First check the base type
                let class_name = quoted_name.unwrap_or_else(|| {
                    base.trim_start_matches('*')
                        .trim_end_matches('?')
                        .split('<')
                        .next()
                        .unwrap_or(base)
                });
                // `base.Int(c)`: the outer type is the one `pkl:base` declares, not
                // whatever the caller's scope calls `Int`; the type arguments
                // and the constraint are the caller's.
                let stdlib_member = (*form == BaseForm::Text)
                    .then(|| stdlib_qualified_member(scope, class_name))
                    .flatten();
                let base_matches = if nullable && is_null_value(val) {
                    true
                } else if let Some(member) = stdlib_member {
                    let unqualified = base.replacen(class_name, member, 1);
                    match crate::parser::parse_type_name(unqualified.trim_end_matches('?')) {
                        Ok(TypeExpr::Generic(name, args)) => {
                            value_is_type(val, &TypeExpr::Generic(name.clone(), args.clone()))
                                && self.check_type_arguments(
                                    val, &name, &args, scope, depth, resolving,
                                )?
                        }
                        _ => value_is_type(val, &TypeExpr::Named(member.to_string())),
                    }
                } else if let Some((resolved, module_scope)) =
                    self.inherited_alias(scope, class_name)
                {
                    self.eval_alias_check(
                        val,
                        class_name,
                        &resolved,
                        &module_scope,
                        depth,
                        resolving,
                    )?
                } else if let Some(accepted) =
                    self.check_imported_alias(val, class_name, &[], scope, depth, resolving)?
                {
                    accepted
                } else if let Some(accepted) =
                    self.shadowed_import_check(val, class_name, scope, depth, resolving)?
                {
                    accepted
                } else if *form == BaseForm::Text
                    && base.contains('<')
                    && let Ok(generic) = crate::parser::parse_type_name(base)
                {
                    // `Listing<Int>(length == 2)`: the base keeps its type
                    // arguments, which constrain the elements.
                    self.eval_type_check_in(val, &generic, scope, depth, resolving)?
                } else if let Some(resolved) = scope.get_type_alias(class_name) {
                    let resolved = resolved.clone();
                    self.eval_alias_check(val, class_name, &resolved, scope, depth, resolving)?
                } else if let Some(source) = stdlib_alias_source(class_name)
                    && scope.get(class_name).is_none()
                    && let Ok(resolved) = crate::parser::parse_type_name(source)
                {
                    // `UInt8(this != 1)`: the base is the standard library's alias.
                    self.eval_type_check_in(
                        val,
                        &resolved,
                        &Scope::default(),
                        depth + 1,
                        resolving,
                    )?
                } else {
                    value_is_class_type(val, class_name, scope)
                        .unwrap_or_else(|| value_is_named_type(val, base))
                };
                if !base_matches {
                    return Ok(false);
                }
                // Evaluate the constraint with `this` bound to the value
                let constraint_scope = constraint_scope_for(scope, val);
                // A constraint nested in another one leaks its `this`.
                if self.constraint_nesting > 0 {
                    self.leaked_this = Some(val.clone());
                }
                // Each constraint is checked on its own, so that the error
                // names the one that is violated.
                for (constraint, text) in split_constraints(constraint) {
                    let receiver_constraint =
                        with_constraint_receiver(constraint, &constraint_scope);
                    let constraint = receiver_constraint.as_ref().unwrap_or(constraint);
                    self.constraint_nesting += 1;
                    let outer_leak = self.leaked_this.take();
                    let result = self.eval_expr(constraint, &constraint_scope, depth + 1);
                    self.constraint_nesting -= 1;
                    let mut result = result?;
                    let mut leak = self.leaked_this.take();
                    // A failing constraint that leaked is evaluated once more
                    // with the leaked `this`, and the error shows that value.
                    if matches!(result, Value::Bool(false))
                        && let Some(leaked) = leak.take()
                    {
                        let retry_scope = constraint_scope_for(scope, &leaked);
                        self.constraint_nesting += 1;
                        let retry = self.eval_expr(constraint, &retry_scope, depth + 1);
                        self.constraint_nesting -= 1;
                        // A retry that errors (a lambda's nested constraint is
                        // not a leak) leaves the ordinary failure.
                        if matches!(retry, Ok(Value::Bool(false))) {
                            let shown = self.leaked_this.take().unwrap_or(leaked.clone());
                            self.type_violation = text.map(|text| (text.to_string(), shown));
                            self.leaked_this = outer_leak;
                            return Ok(false);
                        }
                        self.leaked_this = None;
                    }
                    self.leaked_this = leak.or(outer_leak);
                    // `Listing(isSorted)` names a predicate function and applies
                    // it to the value being checked.
                    let mut expected = "`Boolean` or `Function`";
                    if matches!(result, Value::Lambda(..)) {
                        result = match self.invoke_lambda(
                            &result,
                            std::slice::from_ref(val),
                            depth + 1,
                        ) {
                            Ok(result) => result,
                            // A predicate reading a member that the value does
                            // not have yet, because pklr has not filled in the
                            // class defaults of the value's members, cannot
                            // disprove the constraint.
                            Err(Error::Eval(message))
                                if message.starts_with("Cannot find property `")
                                    && matches!(val, Value::Object(_, Some(source))
                                            if source.kind == ObjectKind::Mapping || source.type_name.is_some()) =>
                            {
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        expected = "`Boolean`";
                    }
                    if !matches!(result, Value::Bool(_)) {
                        return Err(Error::Eval(format!(
                            "Expected value of type {expected}, but got type `{}`. Value: {}",
                            pkl_class_name(&result),
                            display_value(&result)
                        )));
                    }
                    if !is_truthy(&result) {
                        self.type_violation = text.map(|text| (text.to_string(), val.clone()));
                        return Ok(false);
                    }
                }
                Ok(true)
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
                        // A branch that failed before this one is not the reason.
                        self.type_violation = None;
                        return Ok(true);
                    }
                }
                Ok(has_uncheckable_variant)
            }
            TypeExpr::Generic(name, args) => {
                // A `*` marks a union's default alternative.
                let name = name.trim_start_matches('*');
                if let Some(member) = stdlib_qualified_member(scope, name) {
                    // The outer type is `pkl:base`'s; its arguments are the caller's.
                    let unqualified = TypeExpr::Generic(member.to_string(), args.clone());
                    return if value_is_type(val, &unqualified) {
                        self.check_type_arguments(val, member, args, scope, depth, resolving)
                    } else {
                        Ok(false)
                    };
                }
                if let Some((resolved, module_scope)) = self.inherited_alias(scope, name) {
                    if let Some(accepted) = self.check_generic_alias(
                        val,
                        name,
                        args,
                        (&module_scope, scope),
                        depth,
                        resolving,
                    )? {
                        return Ok(accepted);
                    }
                    return self.eval_alias_check(
                        val,
                        name,
                        &resolved,
                        &module_scope,
                        depth,
                        resolving,
                    );
                }
                if let Some(accepted) =
                    self.check_imported_alias(val, name, args, scope, depth, resolving)?
                {
                    return Ok(accepted);
                }
                if let Some(resolved) = scope.get_type_alias(name) {
                    let resolved = resolved.clone();
                    if let Some(accepted) =
                        self.check_generic_alias(val, name, args, (scope, scope), depth, resolving)?
                    {
                        return Ok(accepted);
                    }
                    return self.eval_alias_check(val, name, &resolved, scope, depth, resolving);
                }
                if !value_is_type(val, ty) {
                    return Ok(false);
                }
                self.check_type_arguments(val, name, args, scope, depth, resolving)
            }
        }
    }

    /// Check `val` against a generic alias used with type arguments
    /// (`MyList<Int>`): the alias as written is checked with each parameter
    /// bound to its argument. `None` when `name` is no generic alias or the
    /// arguments do not match its parameters.
    fn check_generic_alias(
        &mut self,
        val: &Value,
        name: &str,
        args: &[crate::parser::TypeExpr],
        (declaring, use_scope): (&Scope, &Scope),
        depth: usize,
        resolving: &[String],
    ) -> Result<Option<bool>> {
        use crate::parser::TypeExpr;
        let Some(TypeExpr::Generic(params, written)) =
            declaring.get_type_alias(&generic_alias_key(name))
        else {
            return Ok(None);
        };
        let params: Vec<&str> = params.split(',').collect();
        let Some(body) = written.first() else {
            return Ok(None);
        };
        if params.len() != args.len() {
            return Ok(None);
        }
        // Applications of one alias to other arguments are no cycle.
        let key = format!(
            "{}<{}>",
            alias_key(declaring, name),
            args.iter()
                .map(|arg| match arg {
                    // A name bound in the caller's scope is keyed by what
                    // it stands for, so one written argument that means
                    // different types at two depths is no cycle.
                    TypeExpr::Named(n) => use_scope
                        .get_type_alias(n)
                        .map_or_else(|| display_type_expr(arg), display_type_expr),
                    _ => display_type_expr(arg),
                })
                .collect::<Vec<_>>()
                .join(",")
        );
        if resolving.contains(&key) {
            return Err(Error::Eval(format!("type alias '{name}' refers to itself")));
        }
        let mut inner = declaring.child();
        for (param, arg) in params.iter().zip(args) {
            // An argument that is the parameter's own name stands for
            // itself in the scope it was written in.
            if matches!(arg, TypeExpr::Named(arg) if arg == param) {
                continue;
            }
            // An argument naming another parameter's name means what it
            // means where it was written, not the parameter.
            let written = match arg {
                TypeExpr::Named(n) if params.contains(&n.as_str()) => {
                    use_scope.get_type_alias(n).unwrap_or(arg)
                }
                _ => arg,
            };
            inner.set_type_alias(*param, written.clone());
        }
        let body = body.clone();
        // The parameters shadow aliases of the same name that are being
        // resolved outside, so the body starts with its own guard.
        // Other applications still being checked stay in it, so generic
        // aliases that refer to each other are reported.
        let mut inner_resolving: Vec<String> = resolving
            .iter()
            .filter(|seen| seen.contains('<'))
            .cloned()
            .collect();
        inner_resolving.push(key);
        self.eval_type_check_in(val, &body, &inner, depth + 1, &mut inner_resolving)
            .map(Some)
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
            ("Pair", Value::Object(members, Some(source)))
                if source.type_identity.as_deref() == Some("pkl:base#Pair") =>
            {
                for (member, ty) in [("first", args.first()), ("second", args.get(1))] {
                    if let (Some(ty), Some(item)) = (checked(ty, scope), members.get(member))
                        && !self.eval_argument_check(item, ty, scope, depth, resolving)?
                    {
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
                            .unwrap_or_else(|| match render::class_key_name(stored_key) {
                                // `[String] = 1` keys by a class, which no
                                // evaluated key value was remembered for.
                                Some(class) => {
                                    stdlib::class_value(class, format!("pkl:base#{class}"))
                                }
                                None => crate::value::mapping_storage_value(stored_key),
                            });
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
            && !STDLIB_TYPE_NAMES.contains(&name)
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
            TypeExpr::Named(name) => {
                // A chain of aliases is followed in a loop, not by recursion.
                let mut name = name;
                loop {
                    let Some(alias) = scope.get_type_alias(name) else {
                        return false;
                    };
                    if !aliases.insert(nm(name)) {
                        return false;
                    }
                    match alias {
                        TypeExpr::Named(next) => name = next,
                        other => return visit(other, scope, aliases),
                    }
                }
            }
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
            | Expr::Constraint(value, _)
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
            TypeExpr::Constrained(_, constraint, _) => {
                expr_has_throw(constraint, scope, None, &mut HashSet::default())
            }
            TypeExpr::Named(name) | TypeExpr::Generic(name, _) => {
                // A chain of aliases is followed in a loop, not by recursion.
                let mut name = name;
                loop {
                    let Some(alias) = scope.get_type_alias(name) else {
                        return false;
                    };
                    if !aliases.insert(nm(name)) {
                        return false;
                    }
                    match alias {
                        TypeExpr::Named(next) => name = next,
                        other => return visit(other, scope, aliases),
                    }
                }
            }
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
        TypeExpr::Constrained(base, _, _) => class_name(base.split('<').next().unwrap_or(base)),
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

/// The type `Int` when `name` is `base.Int` and `base` is the import of
/// `pkl:base`: a type of the standard library is the type without its
/// qualifier.
fn stdlib_qualified_member<'a>(scope: &Scope, name: &'a str) -> Option<&'a str> {
    let (module, member) = name.rsplit_once('.')?;
    (!module.contains('.')
        && scope.get(module).is_some()
        && scope.module_identity(module).map(String::as_str) == Some("pkl:base")
        && STDLIB_TYPE_NAMES.contains(&member))
    .then_some(member)
}

impl Evaluator {
    /// Check `val` against a declared type, with the error pkl reports: the
    /// first element of a `List<T>` or `Set<T>` that is not a `T` is the one
    /// named, and the rejected value follows on its own line.
    pub(super) fn check_declared_value(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        if !type_is_runtime_checkable(ty, scope) {
            return match self.unresolved_type(ty, scope) {
                Some(message) => Err(Error::Eval(message)),
                None => Ok(()),
            };
        }
        if self.eval_argument_check(val, ty, scope, depth, &mut Vec::new())? {
            return Ok(());
        }
        // A value of the wrong base type is reported against the base, not
        // against the constraint it never reached.
        if let crate::parser::TypeExpr::Constrained(base, ..) = ty
            && let Ok(base) = crate::parser::parse_type_name(base)
            && !matches!(base, crate::parser::TypeExpr::Constrained(..))
        {
            self.check_declared_value(val, &base, scope, depth)?;
        }
        Err(Error::Eval(
            self.type_mismatch_message(ty, val, None, scope),
        ))
    }
}

impl Evaluator {
    /// The check a typed `local` gets when it is evaluated. Listing and
    /// Mapping members are checked as they are read, so only the type of the
    /// value itself is checked here, unless `expr` is not a literal body: the
    /// elements of a Listing or Mapping that already exists are not checked
    /// by anything else.
    pub(super) fn check_local_value(
        &mut self,
        val: &Value,
        ty: &crate::parser::TypeExpr,
        expr: &Expr,
        scope: &Scope,
        depth: usize,
    ) -> Result<()> {
        use crate::parser::TypeExpr;
        // Elements of a listing or mapping are checked as they are read, so
        // only the collection itself is checked here.
        fn shallow(ty: &TypeExpr) -> Option<TypeExpr> {
            match ty {
                TypeExpr::Generic(name, _) if matches!(name.as_str(), "Listing" | "Mapping") => {
                    Some(TypeExpr::Named(name.clone()))
                }
                TypeExpr::Nullable(inner) => {
                    shallow(inner).map(|ty| TypeExpr::Nullable(Box::new(ty)))
                }
                _ => None,
            }
        }
        let literal = matches!(expr, Expr::New(..) | Expr::InferredNew(..));
        match shallow(ty) {
            Some(shallow) => {
                self.check_declared_value(val, &shallow, scope, depth)?;
                if literal {
                    return Ok(());
                }
                self.check_declared_value(val, ty, scope, depth)
            }
            None => self.check_declared_value(val, ty, scope, depth),
        }
    }
}
/// The constraints a constrained type's predicate joins, in source order, each
/// with its source text when the parser recorded it. A `&&` inside one
/// constraint stays inside it.
fn split_constraints(expr: &Expr) -> Vec<(&Expr, Option<&str>)> {
    // The parser and alias expansion build a chain: constraints joined with
    // `&&`, each wrapped in `Expr::Constraint`. Anything else is one constraint.
    let mut parts = Vec::new();
    let mut pending = vec![expr];
    while let Some(node) = pending.pop() {
        match node {
            Expr::Constraint(inner, text) => parts.push((&**inner, Some(text.as_str()))),
            Expr::Binop(BinOp::And, left, right) => {
                pending.push(right);
                pending.push(left);
            }
            _ => return vec![(expr, None)],
        }
    }
    parts
}

/// How an error shows the value that violated the constraint `text`. pkl
/// shows the members of an object the constraint has read and `?` for the
/// others, which it has not evaluated.
fn violating_value_display(value: &Value, text: &str) -> String {
    let shown = display_value(value);
    let Value::Object(map, source) = value else {
        return shown;
    };
    if !shown.starts_with("new Dynamic {") {
        return shown;
    }
    let name = source
        .as_ref()
        .and_then(|source| source.type_name.as_deref())
        .unwrap_or("Dynamic");
    let mut members = Vec::new();
    for (key, member) in map.iter() {
        if key.starts_with('\0') {
            return shown;
        }
        if matches!(member, Value::Lambda(..)) {
            continue;
        }
        let read = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| word == &**key);
        let rendered = if read {
            display_value(member)
        } else {
            "?".into()
        };
        members.push(format!("{key} = {rendered}"));
    }
    if members.is_empty() {
        format!("new {name} {{}}")
    } else {
        format!("new {name} {{ {} }}", members.join("; "))
    }
}

/// The start of the line after a type error that names the property whose
/// value failed the check.
pub(super) const PROPERTY_CONTEXT: &str = "\n\nin property '";

/// The type an error names for a value that is not one: the class of a
/// generic or nullable type, without its arguments or its `?`.
fn top_level_error_type(ty: &crate::parser::TypeExpr, scope: &Scope) -> String {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Nullable(inner) => top_level_error_type(inner, scope),
        TypeExpr::Generic(name, _) => name.trim_start_matches('*').to_string(),
        // pkl names a class as its module declares it.
        TypeExpr::Named(name) => match resolve_dotted(scope, name) {
            Some(Value::Object(_, Some(source)))
                if source.kind == ObjectKind::Class
                    && source
                        .type_identity
                        .as_deref()
                        .is_some_and(|identity| !identity.starts_with("pkl:")) =>
            {
                super::classes::qualified_class_name(source.type_identity.as_deref(), name)
            }
            _ => error_type_expr(ty, scope),
        },
        _ => error_type_expr(ty, scope),
    }
}

/// The name a generic type alias's parameters and written target are kept
/// under, beside the alias itself.
pub(super) fn generic_alias_key(name: &str) -> String {
    format!("\0generic:{name}")
}

/// The scope a constraint on `val` is evaluated in: `this` is the value, and
/// its common properties are bound directly so `length`, `isEmpty` etc. work.
fn constraint_scope_for(scope: &Scope, val: &Value) -> Scope {
    let mut constraint_scope = scope.child();
    constraint_scope.set("this", val.clone());
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
    constraint_scope
}
