use super::*;

pub(super) fn refresh_this_aliases(
    scope: &mut Scope,
    aliases: &[Name],
    properties: &Arc<ObjectMap>,
) {
    let snapshot = Value::Object(Arc::clone(properties), None);
    scope.set("this", snapshot.clone());
    for alias in aliases {
        scope.set(alias, snapshot.clone());
        scope.mark_this_alias(alias);
    }
}

/// Drop the scope's references to the current `this` snapshot so the property
/// map is uniquely owned again and can grow in place instead of being copied.
/// Callers refresh the snapshot after the insert.
pub(super) fn release_this_aliases(scope: &mut Scope, aliases: &[Name]) {
    for name in std::iter::once("this").chain(aliases.iter().map(|name| &**name)) {
        if scope.vars.contains_key(name)
            && let Some(slot) = Arc::make_mut(&mut scope.vars).get_mut(name)
        {
            *slot = Value::Null;
        }
    }
}

/// Insert a module property, first releasing the module scope's `this` and
/// `module` snapshots of the property map so it grows in place.
pub(super) fn module_props_insert(
    scope: &mut Scope,
    properties: &mut Arc<ObjectMap>,
    key: impl Into<Arc<str>>,
    value: Value,
) {
    for name in ["this", "module"] {
        if scope.vars.contains_key(name)
            && let Some(slot) = Arc::make_mut(&mut scope.vars).get_mut(name)
        {
            *slot = Value::Null;
        }
    }
    Arc::make_mut(properties).insert(key.into(), value);
}

pub(super) fn props_insert(
    scope: &mut Scope,
    aliases: &[Name],
    properties: &mut Arc<ObjectMap>,
    key: impl Into<Arc<str>>,
    value: Value,
) {
    release_this_aliases(scope, aliases);
    Arc::make_mut(properties).insert(key.into(), value);
}

/// Insert a recovered property at its declaration position after later body
/// members have already populated the receiver snapshot.
pub(super) fn props_shift_insert(
    scope: &mut Scope,
    aliases: &[Name],
    properties: &mut Arc<ObjectMap>,
    index: usize,
    key: impl Into<Arc<str>>,
    value: Value,
) {
    release_this_aliases(scope, aliases);
    Arc::make_mut(properties).shift_insert(index, key.into(), value);
}

/// Insert a receiver member while preserving Mapping key identity for `this`
/// reads that occur later in the same generated body.
pub(super) fn props_insert_mapping(
    scope: &mut Scope,
    aliases: &[Name],
    properties: &mut Arc<ObjectMap>,
    key: Arc<str>,
    value: Value,
    mapping_entries: bool,
) {
    release_this_aliases(scope, aliases);
    let properties = Arc::make_mut(properties);
    if mapping_entries {
        super::insert_mapping_entry(properties, key, value);
    } else {
        properties.insert(key, value);
    }
}

/// Extend a receiver snapshot while retaining a Mapping's semantic key
/// identity for subsequent entries and `this` lookups.
pub(super) fn props_extend_mapping(
    scope: &mut Scope,
    aliases: &[Name],
    properties: &mut Arc<ObjectMap>,
    entries: impl Iterator<Item = (Arc<str>, Value)>,
    mapping_entries: bool,
) {
    release_this_aliases(scope, aliases);
    let properties = Arc::make_mut(properties);
    for (key, value) in entries {
        if mapping_entries {
            super::insert_mapping_entry(properties, key, value);
        } else {
            properties.insert(key, value);
        }
    }
}

/// Apply a mapping's default template to an `["key"] = expr` entry.
///
/// An untyped value (a `Dynamic`, a plain object body, a primitive) is merged
/// onto the template as before. A value that is already an instance of a class
/// is kept as-is: an instance of one of the declared value types has nothing to
/// inherit from the template, and an instance of an unrelated class must not be
/// turned into the template's class. When every declared value type is a known
/// class and none matches, this is the type error Apple Pkl reports.
pub(super) fn apply_mapping_entry_template(
    template: Option<Value>,
    value: Value,
    value_type_names: &[String],
    scope: &Scope,
) -> Result<Value> {
    let merge = |value: Value| match template {
        Some(template) => merge_values(template, value),
        None => value,
    };
    let Value::Object(_, Some(value_src)) = &value else {
        return Ok(merge(value));
    };
    let Some(actual) = value_src.type_name.as_deref() else {
        return Ok(merge(value));
    };
    let allowed = expand_type_alias_names(value_type_names, scope)
        .into_iter()
        .map(|name| mapping_value_type_base(&name).to_string())
        .collect::<Vec<_>>();
    if allowed.is_empty() {
        return Ok(merge(value));
    }
    if allowed
        .iter()
        .any(|name| matches!(name.as_str(), "Any" | "Dynamic" | "Object" | "Typed"))
    {
        return Ok(value);
    }
    // `new Alias {}` tags the value with the alias name; compare the expanded
    // class chain so an alias of a declared class is accepted.
    let chain = std::iter::once(actual.to_string())
        .chain(value_src.parent_type_names.iter().cloned())
        .collect::<Vec<_>>();
    let chain = expand_type_alias_names(&chain, scope);
    for candidate in &chain {
        if allowed.iter().any(|name| type_names_match(name, candidate)) {
            return Ok(value);
        }
    }
    // Only fail when every alternative is understood: a class in scope, or a
    // primitive that an object can never satisfy. Anything unresolved (an
    // imported qualified alias, a generic collection type) stays lenient.
    let all_known = allowed.iter().all(|name| {
        is_object_incompatible_type_name(name)
            || matches!(
                resolve_dotted(scope, name),
                Some(Value::Object(_, Some(src))) if src.type_name.is_some()
            )
    });
    if all_known {
        return Err(Error::Eval(format!(
            "Expected value of type `{}`, but got type `{}`.",
            allowed.join(" | "),
            actual
        )));
    }
    Ok(value)
}

/// Type names an object value can never satisfy.
pub(super) fn is_object_incompatible_type_name(name: &str) -> bool {
    string_literal_type_value(name).is_some()
        || matches!(
            name,
            "Null"
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
                | "String"
                | "Duration"
                | "DataSize"
                | "Regex"
                | "Char"
        )
}

/// Expand mapping value type names, replacing type aliases with the class
/// names they name. `Mapping<String, StepDefinition | Group>` with
/// `typealias StepDefinition = Step | BuiltinFactory` expands to
/// `Step`, `BuiltinFactory`, `Group`.
pub(super) fn expand_type_alias_names(names: &[String], scope: &Scope) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        expand_type_alias_name(name, scope, &mut out, 0);
    }
    out
}

pub(super) fn expand_type_alias_name(
    name: &str,
    scope: &Scope,
    out: &mut Vec<String>,
    depth: usize,
) {
    let is_default = name.starts_with('*');
    let base = name.trim_start_matches('*').trim_end_matches('?');
    if depth < 8
        && let Some(alias) = scope.get_type_alias(base)
    {
        let offset = out.len();
        let alias = alias.clone();
        collect_type_expr_class_names(&alias, scope, out, depth + 1);
        if is_default {
            for name in &mut out[offset..] {
                if !name.starts_with('*') {
                    *name = format!("*{name}");
                }
            }
        }
        return;
    }
    let name = if is_default {
        format!("*{base}")
    } else {
        base.to_string()
    };
    if !out.iter().any(|existing| existing == &name) {
        out.push(name);
    }
}

pub(super) fn collect_type_expr_class_names(
    ty: &crate::parser::TypeExpr,
    scope: &Scope,
    out: &mut Vec<String>,
    depth: usize,
) {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) => expand_type_alias_name(name, scope, out, depth),
        TypeExpr::Constrained(base, _) => expand_type_alias_name(base, scope, out, depth),
        TypeExpr::Generic(name, _) => expand_type_alias_name(name, scope, out, depth),
        TypeExpr::Nullable(inner) => collect_type_expr_class_names(inner, scope, out, depth),
        TypeExpr::Union(variants) => {
            for variant in variants {
                collect_type_expr_class_names(variant, scope, out, depth);
            }
        }
    }
}

/// The runtime name of a Mapping value-type alternative. `*` marks the
/// selected alternative of a union but is not part of the type's name.
pub(super) fn mapping_value_type_base(name: &str) -> &str {
    name.trim_start_matches('*')
}

pub(super) fn select_mapping_type_default<'a>(
    type_defaults: &'a [(String, Value)],
    body: &[Entry],
) -> Option<&'a (String, Value)> {
    if type_defaults.len() <= 1 {
        return type_defaults.first();
    }

    let field_names = body.iter().filter_map(|entry| match entry {
        Entry::Property(prop) if !has_modifier(&prop.modifiers, Modifier::Local) => {
            Some(prop.name.as_ref())
        }
        Entry::DynProperty(Expr::String(name), _) => Some(&**name),
        _ => None,
    });

    let mut best = None;
    let mut best_score = 0;
    for candidate in type_defaults {
        let score = field_names
            .clone()
            .filter(|field| object_declares_field(&candidate.1, field))
            .count();
        if score > best_score {
            best = Some(candidate);
            best_score = score;
        }
    }

    // Empty bodies, unknown fields, and ties fall back to the declared union
    // order. This mirrors Pkl's first assignable type behavior when there is no
    // stronger structural signal in the entry body.
    best.or_else(|| type_defaults.first())
}

pub(super) fn select_mapping_type_default_for_new<'a>(
    type_defaults: &'a [(String, Value)],
    type_name: &str,
) -> Option<&'a (String, Value)> {
    type_defaults
        .iter()
        .find(|(name, _)| name == type_name)
        .or_else(|| {
            type_defaults
                .iter()
                .find(|(name, _)| type_names_match(name, type_name))
        })
        .or_else(|| {
            type_defaults.iter().find(|(_, value)| {
                matches!(
                    value,
                    Value::Object(_, Some(src))
                        if src
                            .type_name
                            .as_deref()
                            .is_some_and(|tn| type_names_match(tn, type_name))
                )
            })
        })
}

pub(super) fn type_names_match(a: &str, b: &str) -> bool {
    a == b
        || (a.len() > b.len() && a.ends_with(b) && a.as_bytes()[a.len() - b.len() - 1] == b'.')
        || (b.len() > a.len() && b.ends_with(a) && b.as_bytes()[b.len() - a.len() - 1] == b'.')
}

pub(super) fn merge_partial_module_values(base: &Value, current: &Value) -> Option<Value> {
    let (Value::Object(base_map, base_source), Value::Object(current_map, current_source)) =
        (base, current)
    else {
        return None;
    };
    let mut merged = (**base_map).clone();
    merged.extend(
        current_map
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    Some(Value::Object(
        Arc::new(merged),
        current_source.clone().or_else(|| base_source.clone()),
    ))
}

pub(super) fn mapping_entry_body(expr: &Expr) -> Option<&[Entry]> {
    match expr {
        Expr::ObjectBody(body) => Some(body),
        Expr::New(Some(_), body, _) => Some(body),
        _ => None,
    }
}

pub(super) fn apply_mapping_type_annotation(
    value: &mut Value,
    type_ann: Option<&crate::parser::TypeExpr>,
) {
    let Some(type_ann) = type_ann else {
        return;
    };
    let Value::Object(_, src_slot) = value else {
        return;
    };
    // An untagged object declared a `Mapping` is a mapping.
    if src_slot.is_none() && is_mapping_type(type_ann) {
        *src_slot = Some(Arc::new(mapping_source()));
    }
    let type_names = mapping_value_type_names(type_ann);
    if type_names.is_empty() {
        return;
    }

    // Usually the source already records these types (a property's value
    // re-tagged on every evaluation); copying it would duplicate its whole
    // captured scope for nothing.
    if let Some(src) = src_slot
        && type_names
            .iter()
            .all(|name| src.mapping_value_types.contains(name))
    {
        return;
    }
    let src = src_slot.get_or_insert_with(|| Arc::new(mapping_source()));
    let src = Arc::make_mut(src);
    for name in type_names {
        if !src.mapping_value_types.contains(&name) {
            src.mapping_value_types.push(name);
        }
    }
}

/// The source of a mapping known only by its type: no entries to rebuild it
/// from, just its kind.
pub(super) fn mapping_source() -> ObjectSource {
    ObjectSource {
        entries: Vec::new().into(),
        captured: SourceScope::default(),
        body_members: HashSet::default(),
        is_open: true,
        is_abstract: false,
        type_name: None,
        type_identity: None,
        parent_type_names: Vec::new(),
        parent_type_identities: Vec::new(),
        entry_scopes: Vec::new(),
        evaluated_properties: Vec::new(),
        elements: Vec::new(),
        mapping_value_types: Vec::new(),
        deprecated: IndexMap::new(),
        poisoned_members: None,
        kind: ObjectKind::Mapping,
        is_parsed_json: false,
        prototype: None,
    }
}

/// Whether `ty` is `Mapping`, `Mapping<K, V>` or a nullable one.
fn is_mapping_type(ty: &crate::parser::TypeExpr) -> bool {
    use crate::parser::TypeExpr;
    match ty {
        TypeExpr::Named(name) | TypeExpr::Generic(name, _) => {
            name.trim_start_matches('*').trim_end_matches('?') == "Mapping"
        }
        TypeExpr::Nullable(inner) => is_mapping_type(inner),
        _ => false,
    }
}

pub(super) fn mapping_value_type_names(type_ann: &crate::parser::TypeExpr) -> Vec<String> {
    use crate::parser::TypeExpr;

    let mut names = Vec::new();
    match type_ann {
        TypeExpr::Generic(name, params) if name == "Mapping" || name == "Map" => {
            if let Some(value_type) = params.get(1) {
                collect_mapping_value_type_names(value_type, &mut names);
            }
        }
        TypeExpr::Nullable(inner) => return mapping_value_type_names(inner),
        TypeExpr::Constrained(base, _) => {
            if let Some(value_type) = mapping_value_type_from_name(base) {
                names.push(value_type);
            }
        }
        _ => {}
    }
    names
}

pub(super) fn mapping_value_type_from_name(name: &str) -> Option<String> {
    let name = name.trim_start_matches('*').trim_end_matches('?');
    let inner = name
        .strip_prefix("Mapping<")
        .or_else(|| name.strip_prefix("Map<"))?
        .strip_suffix('>')?;
    let mut depth = 0usize;
    for (index, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let value_type = inner[index + 1..].trim();
                return Some(
                    value_type
                        .split('<')
                        .next()
                        .unwrap_or(value_type)
                        .trim_end_matches('?')
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    None
}

pub(super) fn collect_mapping_value_type_names(
    type_ann: &crate::parser::TypeExpr,
    names: &mut Vec<String>,
) {
    use crate::parser::TypeExpr;

    match type_ann {
        TypeExpr::Named(name) => {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        TypeExpr::Constrained(base, _) => {
            if !names.contains(base) {
                names.push(base.clone());
            }
        }
        // Keep only the top-level value type. For example,
        // Mapping<String, Mapping<String, Step>> needs a structured Mapping
        // default, not Step as the default for the outer mapping's entries.
        TypeExpr::Generic(name, _) => {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        TypeExpr::Nullable(inner) => collect_mapping_value_type_names(inner, names),
        TypeExpr::Union(variants) => {
            for variant in variants {
                collect_mapping_value_type_names(variant, names);
            }
        }
    }
}

pub(super) fn validate_new_object_body(
    type_name: &str,
    entries: &[Entry],
    base_src: &ObjectSource,
) -> Result<()> {
    if base_src.is_open {
        return Ok(());
    }

    let base_names: HashSet<Name> = base_src
        .entries
        .iter()
        .filter_map(|entry| {
            if let Entry::Property(prop) = entry {
                Some(prop.name.clone())
            } else {
                None
            }
        })
        .collect();

    for entry in entries {
        match entry {
            Entry::Property(prop)
                if !has_modifier(&prop.modifiers, Modifier::Local)
                    && !base_names.contains(&prop.name) =>
            {
                return Err(Error::Eval(format!(
                    "cannot add property '{}' to non-open class {type_name}",
                    prop.name
                )));
            }
            Entry::DynProperty(Expr::String(key), _) if !base_names.contains(&**key) => {
                return Err(Error::Eval(format!(
                    "cannot add property '{key}' to non-open class {type_name}"
                )));
            }
            _ => {}
        }
    }

    Ok(())
}

/// Whether `entries` (including what their generators produce) consist of
/// elements only, with no properties or keyed entries.
pub(super) fn is_element_only_body(entries: &[Entry]) -> bool {
    fn has_members(entries: &[Entry]) -> bool {
        entries.iter().any(|entry| match entry {
            Entry::DynProperty(..) => true,
            Entry::Property(prop) => !has_modifier(&prop.modifiers, Modifier::Local),
            Entry::ForGenerator(generator) => has_members(&generator.body),
            Entry::WhenGenerator(generator) => {
                has_members(&generator.body)
                    || generator
                        .else_body
                        .as_deref()
                        .is_some_and(|body| has_members(body))
            }
            _ => false,
        })
    }
    entries_are_listing_amendment(entries) && !has_members(entries)
}

/// Finds a property (other than `local`s and `default`) that a listing body
/// assigns, including inside `for` and `when` generators.
pub(super) fn find_listing_body_property(entries: &[Entry]) -> Option<&str> {
    entries.iter().find_map(|entry| match entry {
        Entry::Property(prop)
            if &*prop.name != "default" && !has_modifier(&prop.modifiers, Modifier::Local) =>
        {
            Some(prop.name.as_ref())
        }
        Entry::ForGenerator(generator) => find_listing_body_property(&generator.body),
        Entry::WhenGenerator(generator) => {
            find_listing_body_property(&generator.body).or_else(|| {
                generator
                    .else_body
                    .as_deref()
                    .and_then(|body| find_listing_body_property(body))
            })
        }
        _ => None,
    })
}

pub(super) fn find_default_body_entries(entries: &[Entry]) -> Option<crate::parser::Body> {
    entries.iter().find_map(|entry| {
        if let Entry::Property(prop) = entry
            && &*prop.name == "default"
            && !has_modifier(&prop.modifiers, Modifier::Local)
        {
            return prop.body.clone();
        }
        None
    })
}

pub(super) fn object_declares_field(value: &Value, field: &str) -> bool {
    let Value::Object(map, source) = value else {
        return false;
    };
    map.contains_key(field)
        || source.as_ref().is_some_and(|source| {
            source.entries.iter().any(|entry| match entry {
                Entry::Property(prop) => &*prop.name == field,
                Entry::DynProperty(Expr::String(name), _) => **name == *field,
                _ => false,
            })
        })
}

impl Evaluator {
    /// Find and evaluate a `default { ... }` property in an entry list.
    pub(super) fn find_default_template(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        for entry in entries {
            if let Entry::Property(prop) = entry
                && &*prop.name == "default"
                && !has_modifier(&prop.modifiers, Modifier::Local)
            {
                return self.eval_property(prop, scope, depth);
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn eval_mapping_entries_with_type_default(
        &mut self,
        entries: &[crate::parser::Entry],
        scope: &Scope,
        depth: usize,
        map: &mut ObjectMap,
        type_defaults: &[(String, Value)],
        value_type_names: &[String],
        inherited_default: MappingInheritedDefault,
    ) -> Result<()> {
        self.eval_mapping_body(
            entries,
            scope,
            depth,
            map,
            type_defaults,
            value_type_names,
            inherited_default,
            &mut HashSet::default(),
            true,
        )
    }

    /// Evaluate the entries of one mapping body into `map`. `defined_keys`
    /// holds the keys the body has defined so far, including those its
    /// generators produced: a body may amend its parent's entries, but may
    /// define each key only once.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn eval_mapping_body(
        &mut self,
        entries: &[crate::parser::Entry],
        scope: &Scope,
        depth: usize,
        map: &mut ObjectMap,
        type_defaults: &[(String, Value)],
        value_type_names: &[String],
        inherited_default: MappingInheritedDefault,
        defined_keys: &mut HashSet<Arc<str>>,
        bind_mapping_this: bool,
    ) -> Result<()> {
        let mut entry_scope = scope.child();
        let mut deferred_lambdas: Vec<(Name, &crate::parser::Expr)> = Vec::new();
        for entry in entries {
            match entry {
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.value.is_some() =>
                {
                    let expr = prop.value.as_ref().unwrap();
                    // Bind every local in declaration order so later locals and
                    // entries can reference it (e.g. a non-lambda local that
                    // calls a lambda local defined just above it).
                    let val = self.eval_expr(expr, &entry_scope, depth)?;
                    entry_scope.declare(&prop.name, val);
                    if matches!(expr, crate::parser::Expr::Lambda(..)) {
                        // Lambda evaluation only captures the current scope; it
                        // does not run the body. Bind once for declaration-order
                        // visibility, then re-bind after properties for late
                        // binding of overrides.
                        deferred_lambdas.push((prop.name.clone(), expr));
                    }
                }
                Entry::Property(prop)
                    if has_modifier(&prop.modifiers, Modifier::Local) && prop.body.is_some() =>
                {
                    let val =
                        self.eval_entries(prop.body.as_ref().unwrap(), &entry_scope, depth)?;
                    entry_scope.declare(&prop.name, val);
                }
                Entry::ClassDef(name, class_mods, parent, body) => {
                    let defaults = self.eval_class_def(
                        name,
                        class_mods,
                        parent.as_deref(),
                        body,
                        &entry_scope,
                        depth,
                    )?;
                    entry_scope.set(name, defaults);
                }
                Entry::TypeAlias(name, ty) => {
                    self.eval_type_alias(name, ty, &mut entry_scope);
                }
                _ => {}
            }
        }
        for (name, expr) in deferred_lambdas {
            let val = self.eval_expr(expr, &entry_scope, depth)?;
            entry_scope.set(name, val);
        }

        let explicit_default = self
            .find_default_template(entries, &entry_scope, depth)?
            .or(inherited_default.value);
        let explicit_default_entries =
            find_default_body_entries(entries).or(inherited_default.entries);

        for entry in entries {
            match entry {
                Entry::Predicate(..) => return Err(member_predicates_unsupported()),
                Entry::DynProperty(key_expr, val_expr) => {
                    // Direct Mapping entries see a snapshot of the entries
                    // evaluated before them through `this`. Generators retain
                    // their lexical `this`, and the cheap single-name probe
                    // keeps ordinary Mapping evaluation on its fast path.
                    let mapping_scope = (bind_mapping_this
                        && (mapping_entry_mentions_this(key_expr)
                            || mapping_entry_mentions_this(val_expr)))
                    .then(|| {
                        let mut scope = entry_scope.clone();
                        scope.set("this", Value::Object(Arc::new(map.clone()), None));
                        scope
                    });
                    let entry_scope = mapping_scope.as_ref().unwrap_or(&entry_scope);
                    let class_key = self.class_key_of(key_expr, entry_scope);
                    let key = class_key
                        .is_none()
                        .then(|| self.eval_expr(key_expr, entry_scope, depth + 1))
                        .transpose()?;
                    let key_str = match class_key {
                        Some(key) => key,
                        None => value_to_key(
                            key.as_ref().expect("non-class mapping keys are evaluated"),
                        )?,
                    };
                    // Mapping storage preserves Pkl key identity: `1`, `1.0`
                    // and `"1"` are distinct mapping keys.
                    // Object-body entries may amend an earlier value from this
                    // body.
                    let storage_key =
                        equivalent_mapping_key(map, &key_str).unwrap_or_else(|| key_str.clone());
                    if let Some(key) = key.as_ref() {
                        self.remember_mapping_key(storage_key.clone(), key);
                    }
                    if !insert_mapping_key(defined_keys, &key_str) {
                        let key = match key.as_ref() {
                            None => key_str.to_string(),
                            Some(Value::String(s)) => format!("{s:?}"),
                            Some(key) => value_to_display(key),
                        };
                        return Err(Error::Eval(format!(
                            "Duplicate definition of member `{key}`."
                        )));
                    }
                    if let Some(Value::Object(existing_map, Some(existing_src))) =
                        map.get(&storage_key)
                        && let Expr::ObjectBody(body) = val_expr
                    {
                        let val = self.eval_amended_object(
                            existing_map,
                            existing_src,
                            body,
                            entry_scope,
                            depth,
                        )?;
                        map.insert(storage_key, val);
                        continue;
                    }
                    // An entry body of a `Listing` value amends a listing: an
                    // inherited entry's, else the mapping's `default`, else an
                    // empty one. The declared value type decides: `Listing`, or
                    // a union containing `Listing` when the body has only
                    // elements. An untyped element-only body is a Dynamic
                    // rendered as a listing.
                    let expanded_value_type_names =
                        expand_type_alias_names(value_type_names, entry_scope);
                    let selected_value_type = |name: &String| {
                        expanded_value_type_names.len() == 1 || name.starts_with('*')
                    };
                    let listing_value_type = expanded_value_type_names.iter().any(|name| {
                        selected_value_type(name)
                            && mapping_value_type_base(name).split('<').next() == Some("Listing")
                    });
                    // A selected Dynamic alternative can render an
                    // element-only object body as a listing. A bare Mapping
                    // has no value type at all.
                    let dynamic_value_type = expanded_value_type_names.iter().any(|name| {
                        selected_value_type(name) && mapping_value_type_base(name) == "Dynamic"
                    });
                    if let Expr::ObjectBody(body) = val_expr
                        && (matches!(map.get(&storage_key), Some(Value::List(_)))
                            || listing_value_type
                            || ((dynamic_value_type || value_type_names.is_empty())
                                && matches!(explicit_default, None | Some(Value::List(_)))
                                && is_element_only_body(body)))
                    {
                        if find_listing_body_property(body).is_some() {
                            return Err(Error::Eval(
                                "Object of type `Listing` cannot have a property (other than \
                                 `default`)."
                                    .into(),
                            ));
                        }
                        let base = match (map.get(&storage_key), &explicit_default) {
                            (Some(existing @ Value::List(_)), _) => existing.clone(),
                            (_, Some(default @ Value::List(_))) => default.clone(),
                            _ => Value::List(ListValue::new(ListKind::Listing, Vec::new())),
                        };
                        let val = self.eval_value_amendment(base, body, entry_scope, depth)?;
                        map.insert(storage_key, val);
                        continue;
                    }
                    // An element-only entry body is valid only for an
                    // untyped Mapping or a selected Listing/Dynamic value
                    // type. Otherwise Pkl tries to amend the selected value
                    // type and rejects the element, rather than silently
                    // constructing a Dynamic object.
                    if let Expr::ObjectBody(body) = val_expr
                        && is_element_only_body(body)
                        && !value_type_names.is_empty()
                        && !listing_value_type
                        && !dynamic_value_type
                        // A user class keeps the normal amendment path so
                        // it can report the class's qualified name.
                        && !type_defaults.iter().any(|(_, value)| {
                            matches!(value, Value::Object(_, Some(source)) if source.type_identity.is_some())
                        })
                    {
                        let value_type = expanded_value_type_names
                            .first()
                            .map(String::as_str)
                            .unwrap_or("Mapping value");
                        return Err(Error::Eval(format!(
                            "Object of type `{value_type}` cannot have an element."
                        )));
                    }
                    // `new { ... }` for a `Mixin` value is a mixin, not an object.
                    if let Expr::New(None, body, params) = val_expr
                        && expanded_value_type_names.iter().any(|name| {
                            selected_value_type(name) && mapping_value_type_base(name) == "Mixin"
                        })
                    {
                        let mixin = Expr::New(Some("Mixin".into()), body.clone(), params.clone());
                        let val = self.eval_expr(&mixin, entry_scope, depth + 1)?;
                        map.insert(storage_key, val);
                        continue;
                    }
                    // `new { ... }` takes its type from the Mapping's value type
                    // and amends its `default`, like a bare `[key] { ... }` body.
                    // A builtin value type such as `Dynamic` or `Listing` keeps its
                    // own `new` evaluation, and a union (of classes or with a
                    // builtin) has no single type to infer.
                    let inferred_body;
                    let val_expr = match val_expr {
                        Expr::New(None, body, _)
                            if expanded_value_type_names.len() == 1
                                && matches!(
                                    type_defaults,
                                    [(_, Value::Object(_, Some(source)))] if source.type_name.is_some()
                                ) =>
                        {
                            inferred_body = Expr::ObjectBody(body.clone());
                            &inferred_body
                        }
                        _ => val_expr,
                    };
                    let type_default = match val_expr {
                        Expr::ObjectBody(body) => select_mapping_type_default(type_defaults, body)
                            .map(|(name, value)| (Some(name.as_str()), value)),
                        Expr::New(Some(type_name), _, _) => {
                            select_mapping_type_default_for_new(type_defaults, type_name)
                                .map(|(name, value)| (Some(name.as_str()), value))
                        }
                        Expr::New(None, _, _) => None,
                        _ => type_defaults
                            .first()
                            .map(|(name, value)| (Some(name.as_str()), value)),
                    };
                    // `new T { ... }` constructs a fresh T. It never amends the
                    // mapping's `default`, so only T's own defaults apply. In
                    // particular a `default` of another class (the synthetic
                    // `new Step {}` of `new Mapping<String, Step> {}`) must not
                    // turn a `new Group {}` entry into a Step.
                    let is_typed_new = matches!(val_expr, Expr::New(Some(_), _, _));
                    let default_template = match (type_default, explicit_default.as_ref()) {
                        (Some((type_name, type_default)), _) if is_typed_new => {
                            Some((type_name, type_default.clone()))
                        }
                        (None, _) if is_typed_new => None,
                        (Some((type_name, type_default)), Some(explicit_default)) => Some((
                            type_name,
                            merge_values(type_default.clone(), explicit_default.clone()),
                        )),
                        (Some((type_name, type_default)), None) => {
                            Some((type_name, type_default.clone()))
                        }
                        (None, Some(explicit_default)) => Some((None, explicit_default.clone())),
                        (None, None) => None,
                    };
                    // If the default template has ObjectSource, amend its source entries
                    // so late-bound class properties are recomputed after overrides.
                    let val =
                        if let Some((default_type_name, Value::Object(template_map, Some(src)))) =
                            default_template.as_ref()
                            && let Some(body) = mapping_entry_body(val_expr)
                        {
                            if let Expr::New(Some(type_name), _, _) = val_expr {
                                validate_new_object_body(type_name, body, src)?;
                            }
                            let mut result =
                                if let Some(default_entries) = explicit_default_entries.as_ref() {
                                    let mut overlay_entries = default_entries.to_vec();
                                    overlay_entries.extend(body.iter().cloned());
                                    self.eval_amended_object(
                                        template_map,
                                        src,
                                        &overlay_entries,
                                        entry_scope,
                                        depth,
                                    )?
                                } else if !is_typed_new
                                    && let Some(Value::Object(explicit_map, Some(explicit_src))) =
                                        explicit_default.as_ref()
                                    && type_default.is_some()
                                    && default_type_name.is_some_and(|expected| {
                                        let chain = explicit_src
                                            .type_name
                                            .iter()
                                            .chain(explicit_src.parent_type_names.iter())
                                            .cloned()
                                            .collect::<Vec<_>>();
                                        let expected = expand_type_alias_names(
                                            std::slice::from_ref(&expected.to_string()),
                                            entry_scope,
                                        );
                                        expand_type_alias_names(&chain, entry_scope).iter().any(
                                            |actual| {
                                                expected.iter().any(|expected| {
                                                    type_names_match(actual, expected)
                                                })
                                            },
                                        )
                                    })
                                {
                                    // The `default` is itself an instance of the selected
                                    // value type (for example the synthetic `new Step {}`
                                    // of a typed mapping literal). Amend its entries so the
                                    // body's assignments late-bind sibling properties.
                                    self.eval_amended_object(
                                        explicit_map,
                                        explicit_src,
                                        body,
                                        entry_scope,
                                        depth,
                                    )?
                                } else if let Some(Value::Object(explicit_map, _)) =
                                    explicit_default.as_ref()
                                    && type_default.is_some()
                                {
                                    self.eval_object_body_over_template(
                                        template_map,
                                        src,
                                        explicit_map,
                                        body,
                                        entry_scope,
                                        depth,
                                    )?
                                } else {
                                    self.eval_amended_object(
                                        template_map,
                                        src,
                                        body,
                                        entry_scope,
                                        depth,
                                    )?
                                };
                            let type_name = src.type_name.as_deref().or(*default_type_name);
                            if let Some(tn) = type_name
                                && let Value::Object(_, ref mut result_src) = result
                            {
                                let new_src = match result_src.take() {
                                    Some(s) => {
                                        let mut ns = Arc::unwrap_or_clone(s);
                                        ns.type_name = Some(tn.to_string());
                                        ns
                                    }
                                    None => ObjectSource {
                                        entries: vec![].into(),
                                        captured: SourceScope::default(),
                                        body_members: HashSet::default(),
                                        is_open: true,
                                        is_abstract: false,
                                        type_name: Some(tn.to_string()),
                                        type_identity: src.type_identity.clone(),
                                        parent_type_names: src.parent_type_names.clone(),
                                        parent_type_identities: src.parent_type_identities.clone(),
                                        entry_scopes: Vec::new(),
                                        evaluated_properties: Vec::new(),
                                        elements: Vec::new(),
                                        mapping_value_types: Vec::new(),
                                        deprecated: merge_deprecated(&src.deprecated, body),
                                        poisoned_members: None,
                                        kind: ObjectKind::Object,
                                        is_parsed_json: false,
                                        prototype: None,
                                    },
                                };
                                *result_src = Some(std::sync::Arc::new(new_src));
                            }
                            result
                        } else {
                            let val = self.eval_expr(val_expr, entry_scope, depth + 1)?;
                            apply_mapping_entry_template(
                                default_template.map(|(_, template)| template),
                                val,
                                value_type_names,
                                entry_scope,
                            )?
                        };
                    map.insert(storage_key, val);
                }
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {}
                Entry::Property(prop)
                    if &*prop.name == "default"
                        && (explicit_default.is_some() || !type_defaults.is_empty()) => {}
                Entry::Spread(e) => {
                    let val = self.eval_expr(e, &entry_scope, depth + 1)?;
                    check_iterable(&val)?;
                    if let Value::List(_) = val {
                        return Err(Error::Eval(
                            "Cannot spread value of type `List` into object of type `Mapping`."
                                .into(),
                        ));
                    }
                    if let Value::Object(m, _) = val {
                        for (key, value) in m.iter() {
                            insert_mapping_entry(map, key.clone(), value.clone());
                        }
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self.eval_expr(&fgen.collection, &entry_scope, depth + 1)?;
                    for (k, v) in collection_to_items(collection)? {
                        let mut iter_scope = entry_scope.child();
                        iter_scope.set(&fgen.val_var, v);
                        if let Some(kv) = &fgen.key_var {
                            iter_scope.set(kv, k);
                        }
                        self.eval_mapping_body(
                            &fgen.body,
                            &iter_scope,
                            depth + 1,
                            map,
                            type_defaults,
                            value_type_names,
                            MappingInheritedDefault {
                                value: explicit_default.clone(),
                                entries: explicit_default_entries.clone(),
                            },
                            defined_keys,
                            false,
                        )?;
                    }
                }
                Entry::WhenGenerator(generator) => {
                    let condition =
                        self.eval_expr(&generator.condition, &entry_scope, depth + 1)?;
                    let selected = if is_truthy(&condition) {
                        Some(generator.body.as_slice())
                    } else {
                        generator.else_body.as_deref().map(Vec::as_slice)
                    };
                    if let Some(selected) = selected {
                        self.eval_mapping_body(
                            selected,
                            &entry_scope,
                            depth + 1,
                            map,
                            type_defaults,
                            value_type_names,
                            MappingInheritedDefault {
                                value: explicit_default.clone(),
                                entries: explicit_default_entries.clone(),
                            },
                            defined_keys,
                            false,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The backing map retains the first spelling of a native-unit key so `keys`
/// and renderer converters see the source unit. Every mapping operation,
/// however, resolves equivalent normalized amounts to that stored key.
pub(super) fn equivalent_mapping_key(map: &ObjectMap, key: &str) -> Option<Arc<str>> {
    map.get_key_value(key)
        .map(|(key, _)| key.clone())
        .or_else(|| {
            map.keys()
                .find(|stored| crate::value::mapping_storage_keys_equal(stored, key))
                .cloned()
        })
}

pub(super) fn insert_mapping_key(keys: &mut HashSet<Arc<str>>, key: &Arc<str>) -> bool {
    if keys.contains(key)
        || keys
            .iter()
            .any(|stored| crate::value::mapping_storage_keys_equal(stored, key))
    {
        false
    } else {
        keys.insert(key.clone());
        true
    }
}

pub(super) fn insert_mapping_entry(map: &mut ObjectMap, key: Arc<str>, value: Value) {
    let storage_key = equivalent_mapping_key(map, &key).unwrap_or(key);
    map.insert(storage_key, value);
}

pub(super) fn extend_object_entries(
    target: &mut ObjectMap,
    entries: &ObjectMap,
    mapping_entries: bool,
) {
    for (key, value) in entries {
        if mapping_entries {
            insert_mapping_entry(target, key.clone(), value.clone());
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(all(test, feature = "native-io"))]
mod mapping_key_tests {
    use std::sync::Arc;

    use super::Evaluator;
    use crate::Value;

    #[test]
    fn evaluation_boundary_clears_mapping_key_provenance() {
        let mut evaluator = Evaluator::new();
        evaluator
            .mapping_key_values
            .insert(Arc::<str>::from("key"), Value::Int(1));

        evaluator.begin_evaluation();

        assert!(evaluator.mapping_key_values.is_empty());
    }
}
