use super::*;

pub(super) fn refresh_this_aliases(
    scope: &mut Scope,
    aliases: &[String],
    properties: &Arc<IndexMap<String, Value>>,
) {
    let snapshot = Value::Object(Arc::clone(properties), None);
    scope.set("this".into(), snapshot.clone());
    for alias in aliases {
        scope.set(alias.clone(), snapshot.clone());
        scope.mark_this_alias(alias);
    }
}

/// Drop the scope's references to the current `this` snapshot so the property
/// map is uniquely owned again and can grow in place instead of being copied.
/// Callers refresh the snapshot after the insert.
fn release_this_aliases(scope: &mut Scope, aliases: &[String]) {
    for name in std::iter::once("this").chain(aliases.iter().map(String::as_str)) {
        if scope.vars.contains_key(name)
            && let Some(slot) = Rc::make_mut(&mut scope.vars).get_mut(name)
        {
            *slot = Value::Null;
        }
    }
}

pub(super) fn props_insert(
    scope: &mut Scope,
    aliases: &[String],
    properties: &mut Arc<IndexMap<String, Value>>,
    key: String,
    value: Value,
) {
    release_this_aliases(scope, aliases);
    Arc::make_mut(properties).insert(key, value);
}

pub(super) fn props_extend(
    scope: &mut Scope,
    aliases: &[String],
    properties: &mut Arc<IndexMap<String, Value>>,
    entries: impl Iterator<Item = (String, Value)>,
) {
    release_this_aliases(scope, aliases);
    Arc::make_mut(properties).extend(entries);
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
    let allowed = expand_type_alias_names(value_type_names, scope);
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
    let base = name.trim_start_matches('*').trim_end_matches('?');
    if depth < 8
        && let Some(alias) = scope.get_type_alias(base)
    {
        let alias = alias.clone();
        collect_type_expr_class_names(&alias, scope, out, depth + 1);
        return;
    }
    if !out.iter().any(|existing| existing == base) {
        out.push(base.to_string());
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
        TypeExpr::Generic(name, _) => {
            if !out.iter().any(|existing| existing == name) {
                out.push(name.clone());
            }
        }
        TypeExpr::Nullable(inner) => collect_type_expr_class_names(inner, scope, out, depth),
        TypeExpr::Union(variants) => {
            for variant in variants {
                collect_type_expr_class_names(variant, scope, out, depth);
            }
        }
    }
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
            Some(prop.name.as_str())
        }
        Entry::DynProperty(Expr::String(name), _) => Some(name.as_str()),
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
    let type_names = mapping_value_type_names(type_ann);
    if type_names.is_empty() {
        return;
    }
    let Value::Object(_, src_slot) = value else {
        return;
    };

    let mut src = src_slot
        .as_ref()
        .map(|src| (**src).clone())
        .unwrap_or_else(|| ObjectSource {
            entries: Vec::new(),
            scope: ScopeMap::default(),
            scope_declared: NameSet::default(),
            body_members: HashSet::new(),
            is_open: true,
            type_name: None,
            type_identity: None,
            parent_type_names: Vec::new(),
            parent_type_identities: Vec::new(),
            scope_module_identities: IndexMap::new(),
            scope_type_aliases: IndexMap::new(),
            entry_scopes: Vec::new(),
            evaluated_properties: Vec::new(),
            mapping_value_types: Vec::new(),
            deprecated: IndexMap::new(),
        });
    for name in type_names {
        if !src.mapping_value_types.contains(&name) {
            src.mapping_value_types.push(name);
        }
    }
    *src_slot = Some(Arc::new(src));
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
                        .trim_start_matches('*')
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

    let base_names: HashSet<String> = base_src
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
            Entry::DynProperty(Expr::String(key), _) if !base_names.contains(key) => {
                return Err(Error::Eval(format!(
                    "cannot add property '{key}' to non-open class {type_name}"
                )));
            }
            _ => {}
        }
    }

    Ok(())
}

/// Finds a property (other than `local`s and `default`) that a listing body
/// assigns, including inside `for` and `when` generators.
pub(super) fn find_listing_body_property(entries: &[Entry]) -> Option<&str> {
    entries.iter().find_map(|entry| match entry {
        Entry::Property(prop)
            if prop.name != "default" && !has_modifier(&prop.modifiers, Modifier::Local) =>
        {
            Some(prop.name.as_str())
        }
        Entry::ForGenerator(generator) => find_listing_body_property(&generator.body),
        Entry::WhenGenerator(generator) => {
            find_listing_body_property(&generator.body).or_else(|| {
                generator
                    .else_body
                    .as_deref()
                    .and_then(find_listing_body_property)
            })
        }
        _ => None,
    })
}

pub(super) fn find_default_body_entries(entries: &[Entry]) -> Option<Vec<Entry>> {
    entries.iter().find_map(|entry| {
        if let Entry::Property(prop) = entry
            && prop.name == "default"
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
                Entry::Property(prop) => prop.name == field,
                Entry::DynProperty(Expr::String(name), _) => name == field,
                _ => false,
            })
        })
}
