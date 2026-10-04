use super::*;

pub(super) fn analysis_entries_for_requested_fields(
    entries: &[Entry],
    requested_fields: Option<&HashSet<String>>,
) -> Vec<Entry> {
    let Some(requested_fields) = requested_fields else {
        return entries.to_vec();
    };
    entries
        .iter()
        .filter(|entry| match entry {
            Entry::Property(prop) => {
                has_modifier(&prop.modifiers, Modifier::Local)
                    || requested_fields.contains(&prop.name)
            }
            // These entries are still evaluated outside the property output
            // filter, so their imports must remain visible to the analysis.
            Entry::DynProperty(..)
            | Entry::Spread(_)
            | Entry::ForGenerator(_)
            | Entry::WhenGenerator(_)
            | Entry::ClassDef(..)
            | Entry::TypeAlias(..) => true,
            Entry::Elem(_) => false,
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ImportUse {
    Fields(HashSet<String>),
    Whole,
}

pub(super) fn requested_fields_for_import(
    uses: &HashMap<String, ImportUse>,
    alias: &str,
) -> Option<HashSet<String>> {
    match uses.get(alias) {
        Some(ImportUse::Fields(fields)) => Some(fields.clone()),
        Some(ImportUse::Whole) | None => None,
    }
}

/// The keys a glob import's mapping is read with, when it is read only as
/// `Alias["key"]`. A field read such as `Alias.length` or `Alias.keys` reads
/// the whole mapping, even when a matched module happens to have that name.
pub(super) fn glob_index_keys(
    uses: &HashMap<String, ImportUse>,
    alias: &str,
) -> Option<HashSet<String>> {
    if uses.contains_key(&other_uses_key(alias)) {
        return None;
    }
    requested_fields_for_import(uses, &index_reads_key(alias))
}

/// Marks that `Alias` is used other than as `Alias["key"]`.
fn other_uses_key(alias: &str) -> String {
    format!("{alias}\0*")
}

/// Where `Alias["key"]` reads are recorded alongside `Alias`'s own uses.
fn index_reads_key(alias: &str) -> String {
    format!("{alias}\0[]")
}

pub(super) fn import_field_uses(entries: &[Entry]) -> HashMap<String, ImportUse> {
    let mut uses = HashMap::default();
    let shadows = HashSet::default();
    collect_entry_import_field_uses(entries, &mut uses, &shadows);
    uses
}

pub(super) fn collect_entry_import_field_uses(
    entries: &[Entry],
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
) {
    let mut entry_shadows = shadows.clone();
    entry_shadows.extend(declared_entry_roots(entries));
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(ty) = &prop.type_ann {
                    collect_type_import_field_uses(ty, uses, &entry_shadows);
                }
                if let Some(expr) = &prop.value {
                    collect_expr_import_field_uses(expr, uses, &entry_shadows);
                }
                if let Some(body) = &prop.body {
                    collect_entry_import_field_uses(body, uses, &entry_shadows);
                }
            }
            Entry::DynProperty(key, value) => {
                collect_expr_import_field_uses(key, uses, &entry_shadows);
                collect_expr_import_field_uses(value, uses, &entry_shadows);
            }
            Entry::ForGenerator(fgen) => {
                collect_expr_import_field_uses(&fgen.collection, uses, &entry_shadows);
                let mut body_shadows = entry_shadows.clone();
                body_shadows.insert(fgen.val_var.clone());
                if let Some(key_var) = &fgen.key_var {
                    body_shadows.insert(key_var.clone());
                }
                collect_entry_import_field_uses(&fgen.body, uses, &body_shadows);
            }
            Entry::WhenGenerator(wgen) => {
                collect_expr_import_field_uses(&wgen.condition, uses, &entry_shadows);
                collect_entry_import_field_uses(&wgen.body, uses, &entry_shadows);
                if let Some(else_body) = &wgen.else_body {
                    collect_entry_import_field_uses(else_body, uses, &entry_shadows);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => {
                collect_expr_import_field_uses(expr, uses, &entry_shadows);
            }
            Entry::ClassDef(_, _, _, body) => {
                collect_entry_import_field_uses(body, uses, &entry_shadows);
            }
            // A type alias's constraint runs when a value is checked against it.
            Entry::TypeAlias(_, ty) => collect_type_import_field_uses(ty, uses, &entry_shadows),
        }
    }
}

pub(super) fn collect_expr_import_field_uses(
    expr: &Expr,
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
) {
    match expr {
        Expr::Ident(name) => record_whole_import_use(uses, shadows, name),
        Expr::Field(base, field) | Expr::NullSafeField(base, field) => {
            if let Expr::Ident(name) = base.as_ref() {
                record_field_import_use(uses, shadows, name, field);
            } else {
                collect_expr_import_field_uses(base, uses, shadows);
            }
        }
        // `Alias["key"]` names one matched module of a glob import, which
        // `glob_index_keys` narrows to. An ordinary import read this way is
        // still evaluated whole.
        Expr::Index(base, index)
            if matches!(base.as_ref(), Expr::Ident(_))
                && matches!(index.as_ref(), Expr::String(_)) =>
        {
            if let (Expr::Ident(name), Expr::String(key)) = (base.as_ref(), index.as_ref())
                && !shadows.contains(name)
            {
                uses.insert(name.clone(), ImportUse::Whole);
                record_field_import_use(uses, shadows, &index_reads_key(name), key);
            }
        }
        Expr::Index(base, index) | Expr::Binop(_, base, index) => {
            collect_expr_import_field_uses(base, uses, shadows);
            collect_expr_import_field_uses(index, uses, shadows);
        }
        Expr::New(type_name, entries, generic_params) => {
            if let Some(type_name) = type_name {
                if let Some((root, field)) = type_name.split_once('.') {
                    record_field_import_use(uses, shadows, root, field);
                } else {
                    record_whole_import_use(uses, shadows, type_name);
                }
            }
            for param in generic_params {
                record_type_name_import_use(uses, shadows, param);
            }
            collect_entry_import_field_uses(entries, uses, shadows);
        }
        Expr::Call(callee, args) => {
            if let Some(name) = object_method_import_receiver(callee.as_ref()) {
                record_whole_import_use(uses, shadows, name);
            } else {
                collect_expr_import_field_uses(callee, uses, shadows);
            }
            for arg in args {
                collect_expr_import_field_uses(arg, uses, shadows);
            }
        }
        Expr::If(cond, then_expr, else_expr) => {
            collect_expr_import_field_uses(cond, uses, shadows);
            collect_expr_import_field_uses(then_expr, uses, shadows);
            collect_expr_import_field_uses(else_expr, uses, shadows);
        }
        Expr::Let(name, value, body) => {
            collect_expr_import_field_uses(value, uses, shadows);
            let mut body_shadows = shadows.clone();
            body_shadows.insert(name.clone());
            collect_expr_import_field_uses(body, uses, &body_shadows);
        }
        Expr::Is(value, ty) | Expr::As(value, ty) => {
            collect_expr_import_field_uses(value, uses, shadows);
            collect_type_import_field_uses(ty, uses, shadows);
        }
        Expr::Lambda(params, value) => {
            let mut body_shadows = shadows.clone();
            body_shadows.extend(params.iter().cloned());
            collect_expr_import_field_uses(value, uses, &body_shadows);
        }
        Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::Trace(value)
        | Expr::Read(value)
        | Expr::ReadOrNull(value) => collect_expr_import_field_uses(value, uses, shadows),
        Expr::InferredNew(ty, entries) => {
            collect_type_import_field_uses(ty, uses, shadows);
            collect_entry_import_field_uses(entries, uses, shadows);
        }
        Expr::ObjectBody(entries) => collect_entry_import_field_uses(entries, uses, shadows),
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(expr) = part {
                    collect_expr_import_field_uses(expr, uses, shadows);
                }
            }
        }
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Import(..)
        | Expr::ImportGlob(..) => {}
    }
}

pub(super) fn object_method_import_receiver(expr: &Expr) -> Option<&str> {
    let (base, method) = match expr {
        Expr::Field(base, method) | Expr::NullSafeField(base, method) => (base.as_ref(), method),
        _ => return None,
    };
    if !is_intrinsic_object_method(method) {
        return None;
    }
    match base {
        Expr::Ident(name) => Some(name),
        _ => None,
    }
}

pub(super) fn is_intrinsic_object_method(method: &str) -> bool {
    // Import aliases evaluate to module objects. Only methods dispatched for
    // `Value::Object` are whole-object uses here; list/string method names can
    // still be user-defined exported methods such as `Dep.map()`.
    matches!(
        method,
        "containsKey" | "toMap" | "toMapping" | "mapValues" | "filter" | "toList" | "toDynamic"
    )
}

pub(super) fn collect_type_import_field_uses(
    ty: &crate::parser::TypeExpr,
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
) {
    match ty {
        crate::parser::TypeExpr::Named(name) => {
            if string_literal_type_value(name).is_none() {
                record_type_name_import_use(uses, shadows, name);
            }
        }
        crate::parser::TypeExpr::Nullable(inner) => {
            collect_type_import_field_uses(inner, uses, shadows);
        }
        crate::parser::TypeExpr::Union(types) => {
            for ty in types {
                collect_type_import_field_uses(ty, uses, shadows);
            }
        }
        crate::parser::TypeExpr::Generic(name, params) => {
            record_type_name_import_use(uses, shadows, name);
            for param in params {
                collect_type_import_field_uses(param, uses, shadows);
            }
        }
        crate::parser::TypeExpr::Constrained(base, constraint) => {
            for component in constrained_type_components(base) {
                record_type_name_import_use(uses, shadows, component);
            }
            collect_expr_import_field_uses(constraint, uses, shadows);
        }
    }
}

pub(super) fn record_type_name_import_use(
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
    name: &str,
) {
    if let Some((root, field)) = name.split_once('.') {
        record_field_import_use(uses, shadows, root, field);
    } else {
        record_whole_import_use(uses, shadows, name);
    }
}

pub(super) fn record_field_import_use(
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
    name: &str,
    field: &str,
) {
    if shadows.contains(name) {
        return;
    }
    mark_other_use(uses, name);
    match uses.entry(name.to_string()) {
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if let ImportUse::Fields(fields) = entry.get_mut() {
                fields.insert(field.to_string());
            }
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(ImportUse::Fields(HashSet::from_iter([field.to_string()])));
        }
    }
}

pub(super) fn record_whole_import_use(
    uses: &mut HashMap<String, ImportUse>,
    shadows: &HashSet<String>,
    name: &str,
) {
    if !shadows.contains(name) {
        mark_other_use(uses, name);
        uses.insert(name.to_string(), ImportUse::Whole);
    }
}

/// Record that `name` is used other than through a literal index, unless
/// `name` is itself one of the internal per-alias keys.
fn mark_other_use(uses: &mut HashMap<String, ImportUse>, name: &str) {
    if !name.contains('\0') {
        uses.insert(other_uses_key(name), ImportUse::Whole);
    }
}

/// `inherited_builtins` are the built-in type names (of
/// `BINDING_BUILTIN_TYPES`) that a module this one amends or extends may
/// redefine: checks here also resolve its type aliases, which this analysis
/// doesn't see.
pub(super) fn expand_requested_fields(
    entries: &[Entry],
    requested: &HashSet<String>,
    inherited_builtins: &[&str],
) -> HashSet<String> {
    let property_names: HashSet<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Property(prop) if !has_modifier(&prop.modifiers, Modifier::Local) => {
                Some(prop.name.clone())
            }
            _ => None,
        })
        .collect();
    // Type aliases and classes evaluate their constraints and defaults when a
    // value is checked or built, so a property using one also depends on what
    // the definition reads.
    let definitions = Definitions {
        types: entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::TypeAlias(name, _) | Entry::ClassDef(name, ..) => {
                    Some((name.as_str(), entry))
                }
                _ => None,
            })
            .collect(),
        values: entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some(prop.name.as_str()),
                _ => None,
            })
            .collect(),
    };
    let mut module_aliases: TypeAliases = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::TypeAlias(name, ty) => Some((name.as_str(), Some(ty))),
            _ => None,
        })
        .collect();
    // Leave the built-ins an inherited alias may redefine (and this module
    // doesn't) unresolved.
    for builtin in inherited_builtins {
        module_aliases.entry(builtin).or_insert(None);
    }
    let aliases = Some(&module_aliases);
    // An importer reading `dep.ClassName` needs the module properties the
    // class reads through `module` (it is evaluated against them). Other
    // definitions don't see module properties, so they need nothing more.
    let module_reading_definitions = if requested
        .iter()
        .any(|name| definitions.types.contains_key(name.as_str()))
    {
        module_dependent_members(entries)
    } else {
        indexmap::IndexSet::new()
    };
    let mut expanded = requested.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for entry in entries {
            let mut refs = HashSet::default();
            let shadows = HashSet::default();
            match entry {
                Entry::Property(prop) if expanded.contains(&prop.name) => {
                    if let Some(ty) = &prop.type_ann {
                        collect_type_refs(ty, &mut refs, &shadows, aliases, Some(&definitions));
                    }
                    if let Some(expr) = &prop.value {
                        collect_expr_refs_in(
                            expr,
                            &mut refs,
                            &shadows,
                            aliases,
                            Some(&definitions),
                        );
                        collect_sibling_field_refs_expr(expr, &mut refs, true);
                    }
                    if let Some(body) = &prop.body {
                        collect_entry_refs_in(
                            body,
                            &mut refs,
                            &shadows,
                            aliases,
                            Some(&definitions),
                        );
                        collect_sibling_field_refs_entries(body, &mut refs);
                    }
                }
                // A requested class or type alias that reads `module` (an
                // importer reading `dep.ClassName`) depends on what its
                // definition reads.
                Entry::ClassDef(name, ..) | Entry::TypeAlias(name, _)
                    if expanded.contains(name) && module_reading_definitions.contains(name) =>
                {
                    refs.insert(name.clone());
                }
                _ => continue,
            }
            // Bodies that change the aliases followed what they reach
            // themselves (see `collect_entry_refs_in`).
            follow_definitions(&definitions, &mut refs, aliases);
            for dep in refs {
                if dep == DYNAMIC_SIBLING_REF {
                    for name in &property_names {
                        if expanded.insert(name.clone()) {
                            changed = true;
                        }
                    }
                    continue;
                }
                if property_names.contains(&dep) && expanded.insert(dep) {
                    changed = true;
                }
            }
        }
    }
    expanded
}

/// Adds to `refs` what the type aliases and classes in `definitions` that
/// `refs` names read (transitively), resolving constraint bases through
/// `aliases`.
fn follow_definitions(
    definitions: &Definitions,
    refs: &mut HashSet<String>,
    aliases: Option<&TypeAliases>,
) {
    let shadows = HashSet::default();
    let mut pending: Vec<String> = refs.iter().cloned().collect();
    let mut visited = HashSet::default();
    while let Some(name) = pending.pop() {
        let Some(definition) = definitions.types.get(name.as_str()) else {
            continue;
        };
        if !visited.insert(name) {
            continue;
        }
        let mut definition_refs = HashSet::default();
        collect_entry_refs_unnarrowed(
            std::slice::from_ref(*definition),
            &mut definition_refs,
            &shadows,
            aliases,
            None,
        );
        collect_sibling_field_refs_entries(std::slice::from_ref(*definition), &mut definition_refs);
        definition_refs.remove("this");
        for dep in definition_refs {
            if refs.insert(dep.clone()) {
                pending.push(dep);
            }
        }
    }
}

pub(super) fn collect_sibling_field_refs_entries(entries: &[Entry], refs: &mut HashSet<String>) {
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(ty) = &prop.type_ann {
                    collect_sibling_field_refs_type(ty, refs);
                }
                if let Some(expr) = &prop.value {
                    collect_sibling_field_refs_expr(expr, refs, false);
                }
                if let Some(body) = &prop.body {
                    collect_sibling_field_refs_entries(body, refs);
                }
            }
            Entry::DynProperty(key, value) => {
                collect_sibling_field_refs_expr(key, refs, false);
                collect_sibling_field_refs_expr(value, refs, false);
            }
            Entry::ForGenerator(fgen) => {
                collect_sibling_field_refs_expr(&fgen.collection, refs, false);
                collect_sibling_field_refs_entries(&fgen.body, refs);
            }
            Entry::WhenGenerator(wgen) => {
                collect_sibling_field_refs_expr(&wgen.condition, refs, false);
                collect_sibling_field_refs_entries(&wgen.body, refs);
                if let Some(else_body) = &wgen.else_body {
                    collect_sibling_field_refs_entries(else_body, refs);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => {
                collect_sibling_field_refs_expr(expr, refs, false);
            }
            Entry::ClassDef(_, _, _, body) => collect_sibling_field_refs_entries(body, refs),
            Entry::TypeAlias(_, ty) => collect_sibling_field_refs_type(ty, refs),
        }
    }
}

/// The names a type check binds for the checked value inside a constraint on
/// `base` (see `eval_type_check`): `this` always, and `length` and `isEmpty`
/// only when the base is a string or collection type, which bind them. For
/// any other base, including aliases and classes that can't be resolved
/// here, those two are kept as references: keeping an unneeded reference only
/// evaluates more, while dropping a needed one breaks the check.
fn constraint_bound_names(base: &str) -> &'static [&'static str] {
    // A nullable base also runs its constraint on `null`, which binds nothing.
    if base.ends_with('?') {
        return &["this"];
    }
    let base = base.trim_start_matches('*');
    // A generic base such as `Listing<String>` checks as its class.
    let base = base.split('<').next().unwrap_or(base).trim();
    if BINDING_BUILTIN_TYPES.contains(&base) {
        &["this", "length", "isEmpty"]
    } else {
        &["this"]
    }
}

/// The built-in types whose constraints bind `length` and `isEmpty`; see
/// `constraint_bound_names`.
pub(super) const BINDING_BUILTIN_TYPES: &[&str] = &[
    "String",
    "List",
    "Listing",
    "Map",
    "Mapping",
    "Set",
    "Collection",
];

/// A module's own type aliases by name, for resolving a constraint's base.
/// `None` marks a name that can't be resolved here: a built-in type that a
/// nested body redeclares, which may no longer bind what the built-in does.
type TypeAliases<'a> = HashMap<&'a str, Option<&'a crate::parser::TypeExpr>>;

/// A module's own type aliases and classes by name, and the names of its
/// properties, which live in a separate namespace and may share a type's name.
struct Definitions<'a> {
    types: HashMap<&'a str, &'a Entry>,
    values: HashSet<&'a str>,
}

/// Adds to `out` the name of every type alias or class declared directly in
/// `entries`, except a type alias that redeclares one of `aliases`
/// identically, which reads the same either way.
fn collect_entries_type_decls<'e>(
    entries: &'e [Entry],
    aliases: &TypeAliases,
    out: &mut HashSet<&'e str>,
) {
    for entry in entries {
        match entry {
            Entry::TypeAlias(name, ty) => {
                if aliases
                    .get(name.as_str())
                    .is_none_or(|outer| *outer != Some(ty))
                {
                    out.insert(name);
                }
            }
            Entry::ClassDef(name, ..) => {
                out.insert(name);
            }
            _ => {}
        }
    }
}

/// Type names mentioned by `ty` (identifiers in its named, generic and
/// constraint-base types; not in constraint expressions).
fn mentioned_type_names<'t>(ty: &'t crate::parser::TypeExpr, out: &mut Vec<&'t str>) {
    fn names_in<'s>(text: &'s str, out: &mut Vec<&'s str>) {
        out.extend(
            text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter(|name| !name.is_empty()),
        );
    }
    match ty {
        crate::parser::TypeExpr::Named(name) | crate::parser::TypeExpr::Constrained(name, _) => {
            names_in(name, out)
        }
        crate::parser::TypeExpr::Nullable(inner) => mentioned_type_names(inner, out),
        crate::parser::TypeExpr::Union(types) => {
            for ty in types {
                mentioned_type_names(ty, out);
            }
        }
        crate::parser::TypeExpr::Generic(name, params) => {
            names_in(name, out);
            for param in params {
                mentioned_type_names(param, out);
            }
        }
    }
}

/// `aliases` without each alias whose meaning may differ where the `declared`
/// type names are in scope: one that is itself declared, or whose definition,
/// followed through the other `aliases`, mentions a declared name. Such an
/// alias named after a built-in that `constraint_bound_names` reads is marked
/// unresolvable rather than dropped, since dropped it would read as the
/// built-in. `None` when every alias is kept.
fn narrow_aliases<'a>(
    aliases: &TypeAliases<'a>,
    declared: &HashSet<&str>,
) -> Option<TypeAliases<'a>> {
    if declared.is_empty() {
        return None;
    }
    let affected = |start: &'a str| {
        let mut seen = HashSet::default();
        let mut pending = vec![start];
        while let Some(name) = pending.pop() {
            if declared.contains(name) {
                return true;
            }
            if !seen.insert(name) {
                continue;
            }
            if let Some(Some(ty)) = aliases.get(name) {
                mentioned_type_names(ty, &mut pending);
            }
        }
        false
    };
    let mut changed = false;
    let kept: TypeAliases<'a> = aliases
        .iter()
        .filter_map(|(name, ty)| {
            if ty.is_none() || !affected(name) {
                return Some((*name, *ty));
            }
            changed = true;
            BINDING_BUILTIN_TYPES
                .contains(name)
                .then_some((*name, None))
        })
        .collect();
    changed.then_some(kept)
}

/// `aliases` with each of the `declared` names that `constraint_bound_names`
/// would read as a built-in marked unresolvable, since where the declaration
/// is in scope the built-in's bindings may no longer apply. A built-in that
/// `entries` only redeclares as one type alias which binds the same names
/// (such as `typealias String = List`) stays resolved, to that definition, when
/// nothing in `aliases` already gave it another meaning: it binds them before
/// and after the declaration alike, and a deeper body that redeclares what
/// the definition refers to still sees it. `None` when `aliases` is
/// unchanged.
fn with_builtins_unresolved<'a>(
    aliases: &TypeAliases<'a>,
    entries: &'a [Entry],
    declared: &HashSet<&str>,
) -> Option<TypeAliases<'a>> {
    let redeclared: Vec<&str> = BINDING_BUILTIN_TYPES
        .iter()
        .copied()
        .filter(|builtin| declared.contains(builtin) && aliases.get(builtin) != Some(&None))
        .collect();
    if redeclared.is_empty() {
        return None;
    }
    let mut marked = aliases.clone();
    for builtin in &redeclared {
        marked.insert(builtin, None);
    }
    // Resolve the new definitions with every redeclared built-in unresolved,
    // so one that reaches another still counts as unknown.
    let binding_definition = |builtin: &str| {
        if aliases.contains_key(builtin) {
            return None;
        }
        let mut definitions = entries.iter().filter_map(|entry| match entry {
            Entry::TypeAlias(name, ty) if name == builtin => Some(Some(ty)),
            Entry::ClassDef(name, ..) if name == builtin => Some(None),
            _ => None,
        });
        match (definitions.next(), definitions.next()) {
            (Some(Some(ty)), None) if alias_binds_collection_names(ty, &marked) => Some(ty),
            _ => None,
        }
    };
    let kept: Vec<_> = redeclared
        .iter()
        .filter_map(|builtin| Some((*builtin, binding_definition(builtin)?)))
        .collect();
    for (builtin, ty) in kept {
        marked.insert(builtin, Some(ty));
    }
    Some(marked)
}

/// Whether a check against the type alias definition `ty` binds `length` and
/// `isEmpty`, resolving through `aliases`.
fn alias_binds_collection_names(ty: &crate::parser::TypeExpr, aliases: &TypeAliases) -> bool {
    match ty {
        crate::parser::TypeExpr::Named(base)
        | crate::parser::TypeExpr::Generic(base, _)
        | crate::parser::TypeExpr::Constrained(base, _) => {
            constraint_bound_names_resolving(base, Some(aliases)).contains(&"length")
        }
        crate::parser::TypeExpr::Nullable(_) | crate::parser::TypeExpr::Union(_) => false,
    }
}

/// `constraint_bound_names`, after following `base` through `aliases` (with
/// cycle protection) to the type it names. A base that resolves to a nullable
/// or union type, to an unresolvable name, or that loops, binds only `this`;
/// a name that isn't one of
/// `aliases` (a class, an imported alias, an unknown name) is left to
/// `constraint_bound_names` as is.
fn constraint_bound_names_resolving(
    base: &str,
    aliases: Option<&TypeAliases>,
) -> &'static [&'static str] {
    let Some(aliases) = aliases else {
        return constraint_bound_names(base);
    };
    let mut base = base;
    let mut seen = HashSet::default();
    loop {
        if base.ends_with('?') {
            return &["this"];
        }
        let name = base.trim_start_matches('*');
        let name = name.split('<').next().unwrap_or(name).trim();
        let Some(ty) = aliases.get(name) else {
            return constraint_bound_names(base);
        };
        let Some(ty) = ty else {
            return &["this"];
        };
        if !seen.insert(name) {
            return &["this"];
        }
        base = match ty {
            crate::parser::TypeExpr::Named(name) | crate::parser::TypeExpr::Generic(name, _) => {
                name
            }
            crate::parser::TypeExpr::Constrained(base, _) => base,
            crate::parser::TypeExpr::Nullable(_) | crate::parser::TypeExpr::Union(_) => {
                return &["this"];
            }
        };
    }
}

/// `module.field` reads in a type's constraints, which run when a value is
/// checked against it.
fn collect_sibling_field_refs_type(ty: &crate::parser::TypeExpr, refs: &mut HashSet<String>) {
    match ty {
        crate::parser::TypeExpr::Constrained(_, constraint) => {
            collect_sibling_field_refs_expr(constraint, refs, false);
        }
        crate::parser::TypeExpr::Nullable(inner) => collect_sibling_field_refs_type(inner, refs),
        crate::parser::TypeExpr::Union(types) | crate::parser::TypeExpr::Generic(_, types) => {
            for ty in types {
                collect_sibling_field_refs_type(ty, refs);
            }
        }
        crate::parser::TypeExpr::Named(_) => {}
    }
}

pub(super) fn collect_sibling_field_refs_expr(
    expr: &Expr,
    refs: &mut HashSet<String>,
    include_this: bool,
) {
    match expr {
        Expr::Field(base, field) | Expr::NullSafeField(base, field) => {
            if is_module_sibling_ref(base, include_this) {
                refs.insert(field.clone());
            }
            collect_sibling_field_refs_expr(base, refs, include_this);
        }
        Expr::Index(base, index) => {
            if is_module_sibling_ref(base, include_this) {
                if let Expr::String(key) = index.as_ref() {
                    refs.insert(key.clone());
                } else {
                    refs.insert(DYNAMIC_SIBLING_REF.to_string());
                }
            }
            collect_sibling_field_refs_expr(base, refs, include_this);
            collect_sibling_field_refs_expr(index, refs, include_this);
        }
        Expr::Binop(_, left, right) => {
            collect_sibling_field_refs_expr(left, refs, include_this);
            collect_sibling_field_refs_expr(right, refs, include_this);
        }
        Expr::New(type_name, entries, _) => {
            // `new module.C {}` reads the module member `C`.
            if let Some((root, rest)) = type_name.as_deref().and_then(|name| name.split_once('.'))
                && (root == "module" || (include_this && root == "this"))
            {
                let member = rest.split('.').next().unwrap_or(rest);
                refs.insert(member.to_string());
            }
            collect_sibling_field_refs_entries(entries, refs);
        }
        Expr::InferredNew(_, entries) | Expr::ObjectBody(entries) => {
            collect_sibling_field_refs_entries(entries, refs);
        }
        Expr::Call(callee, args) => {
            collect_sibling_field_refs_expr(callee, refs, include_this);
            for arg in args {
                collect_sibling_field_refs_expr(arg, refs, include_this);
            }
        }
        Expr::If(cond, then_expr, else_expr) => {
            collect_sibling_field_refs_expr(cond, refs, include_this);
            collect_sibling_field_refs_expr(then_expr, refs, include_this);
            collect_sibling_field_refs_expr(else_expr, refs, include_this);
        }
        Expr::Let(_, value, body) => {
            collect_sibling_field_refs_expr(value, refs, include_this);
            collect_sibling_field_refs_expr(body, refs, include_this);
        }
        Expr::Is(value, ty) | Expr::As(value, ty) => {
            collect_sibling_field_refs_expr(value, refs, include_this);
            // `module.field` reads in the checked type's constraints.
            collect_sibling_field_refs_type(ty, refs);
        }
        Expr::Lambda(_, body)
        | Expr::Unop(_, body)
        | Expr::Throw(body)
        | Expr::Trace(body)
        | Expr::Read(body)
        | Expr::ReadOrNull(body) => {
            collect_sibling_field_refs_expr(body, refs, include_this);
        }
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(expr) = part {
                    collect_sibling_field_refs_expr(expr, refs, include_this);
                }
            }
        }
        Expr::Ident(_)
        | Expr::Import(..)
        | Expr::ImportGlob(..)
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_) => {}
    }
}

pub(super) fn property_reference_names(prop: &Property) -> HashSet<String> {
    let mut refs = HashSet::default();
    let shadows = HashSet::default();
    if let Some(expr) = &prop.value {
        collect_expr_refs(expr, &mut refs, &shadows);
        collect_sibling_field_refs_expr(expr, &mut refs, true);
        if has_modifier(&prop.modifiers, Modifier::Local) && is_module_sibling_ref(expr, true) {
            refs.insert(DYNAMIC_SIBLING_REF.to_string());
        }
    }
    if let Some(body) = &prop.body {
        collect_entry_refs(body, &mut refs, &shadows);
        collect_sibling_field_refs_entries(body, &mut refs);
    }
    refs
}

pub(super) fn is_module_sibling_ref(expr: &Expr, include_this: bool) -> bool {
    matches!(expr, Expr::Ident(name) if name == "module" || (include_this && name == "this"))
}

/// Names, in declaration order, of the module members in `entries` that must
/// be evaluated again once the module's properties are available: classes
/// whose bodies read `module`, and the members that reference such a member
/// (a subclass, a type alias naming it, a local, or a module function
/// building an instance).
pub(super) fn module_dependent_members(entries: &[Entry]) -> indexmap::IndexSet<String> {
    // A member qualified through the module object (`module.C`, or `this.C`
    // where `this` is the module) is a reference to `C` like a bare `C`.
    fn qualified_member(name: &str, include_this: bool) -> Option<&str> {
        let (root, rest) = name.split_once('.')?;
        (root == "module" || (include_this && root == "this"))
            .then(|| rest.split('.').next().unwrap_or(rest))
    }
    // Only a class that mentions `module` starts a dependency chain. Most
    // modules have none, so check that without collecting any names.
    if !entries
        .iter()
        .any(|entry| matches!(entry, Entry::ClassDef(..)) && entry_mentions(entry, "module"))
    {
        return indexmap::IndexSet::new();
    }
    let members: Vec<(&String, bool, HashSet<String>)> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::ClassDef(name, _, parent, body) => {
                let mut refs = referenced_roots(body);
                // Inside a class body `this` is the instance, so only
                // `module.C` names a module member.
                collect_sibling_field_refs_entries(body, &mut refs);
                if let Some(parent) = parent {
                    collect_name_root(parent, &mut refs, &HashSet::default());
                    refs.extend(qualified_member(parent, false).map(str::to_string));
                }
                Some((name, true, refs))
            }
            Entry::TypeAlias(name, ty) => {
                let mut refs = HashSet::default();
                collect_type_refs(ty, &mut refs, &HashSet::default(), None, None);
                // `typealias Ok = Int(module.C.v == "b")` reads `C` when a
                // value is checked against it.
                collect_sibling_field_refs_type(ty, &mut refs);
                if let Some(target) = type_alias_target(ty) {
                    refs.extend(qualified_member(target, true).map(str::to_string));
                }
                Some((name, false, refs))
            }
            // Locals, and module functions (a non-local property whose value
            // is a lambda). Other module properties are evaluated in order by
            // the property pass and already see the refreshed classes.
            Entry::Property(prop)
                if has_modifier(&prop.modifiers, Modifier::Local)
                    || matches!(prop.value, Some(Expr::Lambda(..))) =>
            {
                let value = prop.value.as_ref()?;
                let mut refs = HashSet::default();
                collect_expr_refs(value, &mut refs, &HashSet::default());
                // At module level `this` is the module object too.
                collect_sibling_field_refs_expr(value, &mut refs, true);
                Some((&prop.name, false, refs))
            }
            _ => None,
        })
        .collect();
    let mut dependent = HashSet::default();
    loop {
        let before = dependent.len();
        for (name, is_class, refs) in &members {
            // A dynamic `module[key]` read can reach any tracked member.
            if !dependent.contains(name.as_str())
                && ((refs.contains(DYNAMIC_SIBLING_REF) && !dependent.is_empty())
                    || refs.iter().any(|root| {
                        (*is_class && root == "module") || dependent.contains(root.as_str())
                    }))
            {
                dependent.insert(name.to_string());
            }
        }
        if dependent.len() == before {
            break;
        }
    }
    // Refresh order: each member after the members it reads, so a function
    // or class declared before a class it uses sees that class's new value.
    // Ties and cycles keep declaration order; a dynamic `module[key]` read
    // may reach any member, so it comes after the members without one.
    let tracked: Vec<&(&String, bool, HashSet<String>)> = members
        .iter()
        .filter(|(name, ..)| dependent.contains(name.as_str()))
        .collect();
    let index: HashMap<&str, usize> = tracked
        .iter()
        .enumerate()
        .map(|(i, (name, ..))| (name.as_str(), i))
        .collect();
    let dynamic: Vec<bool> = tracked
        .iter()
        .map(|(_, _, refs)| refs.contains(DYNAMIC_SIBLING_REF))
        .collect();
    let static_deps: Vec<Vec<usize>> = tracked
        .iter()
        .enumerate()
        .map(|(i, (_, _, refs))| {
            let mut deps: Vec<usize> = refs
                .iter()
                .filter_map(|name| index.get(name.as_str()).copied())
                .filter(|&j| j != i)
                .collect();
            deps.sort_unstable();
            deps
        })
        .collect();
    // Whether `from` reads `to`, directly or through other members.
    let reaches = |from: usize, to: usize| {
        let mut seen = vec![false; tracked.len()];
        let mut stack = vec![from];
        while let Some(i) = stack.pop() {
            if i == to {
                return true;
            }
            if !std::mem::replace(&mut seen[i], true) {
                stack.extend(&static_deps[i]);
            }
        }
        false
    };
    // A dynamic reader goes after the other members, except those that read
    // it: ordering it after them would put it after its own dependents.
    let deps: Vec<Vec<usize>> = (0..tracked.len())
        .map(|i| {
            let mut deps = static_deps[i].clone();
            if dynamic[i] {
                deps.extend(
                    (0..tracked.len()).filter(|&j| j != i && !dynamic[j] && !reaches(j, i)),
                );
                deps.sort_unstable();
                deps.dedup();
            }
            deps
        })
        .collect();
    // 0 = unvisited, 1 = in progress, 2 = done.
    fn visit(i: usize, deps: &[Vec<usize>], state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &j in &deps[i] {
            visit(j, deps, state, order);
        }
        state[i] = 2;
        order.push(i);
    }
    let mut state = vec![0u8; tracked.len()];
    let mut order = Vec::with_capacity(tracked.len());
    for i in 0..tracked.len() {
        visit(i, &deps, &mut state, &mut order);
    }
    order.into_iter().map(|i| tracked[i].0.clone()).collect()
}

/// The module members `entry` (a module-level class, alias, local or
/// function) reads through the module object, as `module.C` (or `this.C`
/// outside a class body, where `this` is the module).
pub(super) fn qualified_module_member_refs(entry: &Entry) -> HashSet<String> {
    let mut refs = HashSet::default();
    match entry {
        Entry::ClassDef(_, _, parent, body) => {
            collect_sibling_field_refs_entries(body, &mut refs);
            if let Some((root, rest)) = parent.as_deref().and_then(|name| name.split_once('.'))
                && root == "module"
            {
                refs.insert(rest.split('.').next().unwrap_or(rest).to_string());
            }
        }
        Entry::Property(prop) => {
            if let Some(value) = &prop.value {
                collect_sibling_field_refs_expr(value, &mut refs, true);
            }
            if let Some(ty) = &prop.type_ann {
                collect_sibling_field_refs_type(ty, &mut refs);
            }
        }
        _ => {}
    }
    refs
}

/// Whether evaluating module property `prop` can read one of `members` (see
/// `module_dependent_members`). Members are only refreshed before such a
/// property, so other properties cost nothing extra. Conservative: any
/// dynamic `module[...]` read counts.
pub(super) fn reads_module_members(prop: &Property, members: &indexmap::IndexSet<String>) -> bool {
    let mut refs = property_reference_names(prop);
    if let Some(ty) = &prop.type_ann {
        collect_type_refs(ty, &mut refs, &HashSet::default(), None, None);
        collect_sibling_field_refs_type(ty, &mut refs);
    }
    refs.contains(DYNAMIC_SIBLING_REF)
        || refs.iter().any(|name| members.contains(name))
        || members.iter().any(|name| property_mentions(prop, name))
}

/// Whether module member `entry`, evaluated during a refresh, can read one of
/// `pending` (refreshed members not yet written to the module object): as
/// `module.C`/`this.C`, through any dynamic `module[...]` read, or by name in
/// a type (`x: module.C`, `is module.C`).
pub(super) fn member_reads_pending(entry: &Entry, pending: &[(String, Option<Value>)]) -> bool {
    let refs = qualified_module_member_refs(entry);
    refs.contains(DYNAMIC_SIBLING_REF)
        || pending
            .iter()
            .any(|(name, _)| refs.contains(name) || entry_mentions(entry, name))
}

/// The class or alias a type alias binds to at runtime (`typealias A = C`,
/// `C?` or `C(constraint)`), as `eval_type_alias` resolves it.
pub(super) fn type_alias_target(ty: &crate::parser::TypeExpr) -> Option<&str> {
    match ty {
        crate::parser::TypeExpr::Named(target)
        | crate::parser::TypeExpr::Constrained(target, _) => Some(target),
        crate::parser::TypeExpr::Nullable(inner) => match inner.as_ref() {
            crate::parser::TypeExpr::Named(target) => Some(target),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn referenced_roots(entries: &[Entry]) -> HashSet<String> {
    let mut refs = HashSet::default();
    let shadows = HashSet::default();
    collect_entry_refs(entries, &mut refs, &shadows);
    refs
}

pub(super) fn collect_entry_refs(
    entries: &[Entry],
    refs: &mut HashSet<String>,
    shadows: &HashSet<String>,
) {
    collect_entry_refs_in(entries, refs, shadows, None, None);
}

/// Like `collect_entry_refs`, but resolves a constraint's base through the
/// module's own type aliases (see `constraint_bound_names_resolving`).
fn collect_entry_refs_in(
    entries: &[Entry],
    refs: &mut HashSet<String>,
    shadows: &HashSet<String>,
    aliases: Option<&TypeAliases>,
    definitions: Option<&Definitions>,
) {
    // A type alias or class declared in this body takes effect only from its
    // declaration on (a local evaluated earlier still sees the module's
    // aliases), and it can change what a module alias's definition refers to
    // (`typealias String = Int` under an identical `typealias S = String`). So
    // stop resolving the module aliases whose definitions reach a name this
    // body declares (other than by an identical redeclaration), keeping the
    // conservative reading of their constraints, and leave a redeclared
    // built-in unresolved (even without module aliases).
    let no_aliases = TypeAliases::default();
    let narrowed = {
        let aliases = aliases.unwrap_or(&no_aliases);
        let mut declared = HashSet::default();
        collect_entries_type_decls(entries, aliases, &mut declared);
        let narrowed = narrow_aliases(aliases, &declared);
        with_builtins_unresolved(narrowed.as_ref().unwrap_or(aliases), entries, &declared)
            .or(narrowed)
    };
    // A definition's constraints resolve aliases where the check happens, so
    // when following `definitions` and this body changes the aliases, follow
    // the ones reached from here with this body's aliases, and leave only
    // what they read to the caller (keeping a name that is also a module
    // property, which this body may read as a value).
    if let (Some(narrowed), Some(definitions)) = (&narrowed, definitions) {
        let mut body_refs = HashSet::default();
        collect_entry_refs_unnarrowed(
            entries,
            &mut body_refs,
            shadows,
            Some(narrowed),
            Some(definitions),
        );
        follow_definitions(definitions, &mut body_refs, Some(narrowed));
        body_refs.retain(|name| {
            !definitions.types.contains_key(name.as_str())
                || definitions.values.contains(name.as_str())
        });
        refs.extend(body_refs);
        return;
    }
    collect_entry_refs_unnarrowed(
        entries,
        refs,
        shadows,
        narrowed.as_ref().or(aliases),
        definitions,
    );
}

/// `collect_entry_refs_in` without narrowing `aliases` for the types
/// `entries` declare, for a module-level definition followed on its own.
fn collect_entry_refs_unnarrowed(
    entries: &[Entry],
    refs: &mut HashSet<String>,
    shadows: &HashSet<String>,
    aliases: Option<&TypeAliases>,
    definitions: Option<&Definitions>,
) {
    let mut entry_shadows = shadows.clone();
    entry_shadows.extend(declared_entry_roots(entries));
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(ty) = &prop.type_ann {
                    collect_type_refs(ty, refs, &entry_shadows, aliases, definitions);
                }
                if let Some(expr) = &prop.value {
                    collect_expr_refs_in(expr, refs, &entry_shadows, aliases, definitions);
                }
                if let Some(body) = &prop.body {
                    collect_entry_refs_in(body, refs, &entry_shadows, aliases, definitions);
                }
            }
            Entry::DynProperty(key, value) => {
                collect_expr_refs_in(key, refs, &entry_shadows, aliases, definitions);
                collect_expr_refs_in(value, refs, &entry_shadows, aliases, definitions);
            }
            Entry::ForGenerator(fgen) => {
                collect_expr_refs_in(&fgen.collection, refs, &entry_shadows, aliases, definitions);
                let mut body_shadows = entry_shadows.clone();
                body_shadows.insert(fgen.val_var.clone());
                if let Some(key_var) = &fgen.key_var {
                    body_shadows.insert(key_var.clone());
                }
                collect_entry_refs_in(&fgen.body, refs, &body_shadows, aliases, definitions);
            }
            Entry::WhenGenerator(wgen) => {
                collect_expr_refs_in(&wgen.condition, refs, &entry_shadows, aliases, definitions);
                collect_entry_refs_in(&wgen.body, refs, &entry_shadows, aliases, definitions);
                if let Some(else_body) = &wgen.else_body {
                    collect_entry_refs_in(else_body, refs, &entry_shadows, aliases, definitions);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => {
                collect_expr_refs_in(expr, refs, &entry_shadows, aliases, definitions)
            }
            Entry::ClassDef(_, _, parent, body) => {
                if let Some(parent) = parent {
                    collect_name_root(parent, refs, &entry_shadows);
                }
                collect_entry_refs_in(body, refs, &entry_shadows, aliases, definitions);
            }
            Entry::TypeAlias(_, ty) => {
                collect_type_refs(ty, refs, &entry_shadows, aliases, definitions)
            }
        }
    }
}

// Wrap only the locals needed by the selected element. Forcing unrelated
// locals here can recurse when a local itself reads super.first or super.last.
pub(super) fn with_listing_locals(expr: &Expr, locals: &[(String, Expr)]) -> Expr {
    let mut expr = expr.clone();
    let mut refs = HashSet::default();
    collect_expr_refs(&expr, &mut refs, &HashSet::default());
    for (name, value) in locals.iter().rev() {
        if refs.remove(name) {
            collect_expr_refs(value, &mut refs, &HashSet::default());
            expr = Expr::Let(name.clone(), Box::new(value.clone()), Box::new(expr));
        }
    }
    expr
}

pub(super) fn collect_expr_refs(
    expr: &Expr,
    refs: &mut HashSet<String>,
    shadows: &HashSet<String>,
) {
    collect_expr_refs_in(expr, refs, shadows, None, None);
}

fn collect_expr_refs_in(
    expr: &Expr,
    refs: &mut HashSet<String>,
    shadows: &HashSet<String>,
    aliases: Option<&TypeAliases>,
    definitions: Option<&Definitions>,
) {
    match expr {
        Expr::Ident(name) => {
            if !shadows.contains(name) {
                refs.insert(name.clone());
            }
        }
        Expr::New(type_name, entries, generic_params) => {
            if let Some(type_name) = type_name {
                collect_name_root(type_name, refs, shadows);
            }
            for param in generic_params {
                collect_name_root(param, refs, shadows);
            }
            collect_entry_refs_in(entries, refs, shadows, aliases, definitions);
        }
        Expr::Field(base, _) | Expr::NullSafeField(base, _) => {
            collect_expr_refs_in(base, refs, shadows, aliases, definitions);
        }
        Expr::Index(base, index) | Expr::Binop(_, base, index) => {
            collect_expr_refs_in(base, refs, shadows, aliases, definitions);
            collect_expr_refs_in(index, refs, shadows, aliases, definitions);
        }
        Expr::Call(callee, args) => {
            collect_expr_refs_in(callee, refs, shadows, aliases, definitions);
            for arg in args {
                collect_expr_refs_in(arg, refs, shadows, aliases, definitions);
            }
        }
        Expr::If(cond, then_expr, else_expr) => {
            collect_expr_refs_in(cond, refs, shadows, aliases, definitions);
            collect_expr_refs_in(then_expr, refs, shadows, aliases, definitions);
            collect_expr_refs_in(else_expr, refs, shadows, aliases, definitions);
        }
        Expr::Let(name, value, body) => {
            collect_expr_refs_in(value, refs, shadows, aliases, definitions);
            let mut body_shadows = shadows.clone();
            body_shadows.insert(name.clone());
            collect_expr_refs_in(body, refs, &body_shadows, aliases, definitions);
        }
        Expr::Is(value, ty) | Expr::As(value, ty) => {
            collect_expr_refs_in(value, refs, shadows, aliases, definitions);
            collect_type_refs(ty, refs, shadows, aliases, definitions);
        }
        Expr::Lambda(params, value) => {
            let mut body_shadows = shadows.clone();
            body_shadows.extend(params.iter().cloned());
            collect_expr_refs_in(value, refs, &body_shadows, aliases, definitions);
        }
        Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::Trace(value)
        | Expr::Read(value)
        | Expr::ReadOrNull(value) => {
            collect_expr_refs_in(value, refs, shadows, aliases, definitions)
        }
        Expr::InferredNew(ty, entries) => {
            collect_type_refs(ty, refs, shadows, aliases, definitions);
            collect_entry_refs_in(entries, refs, shadows, aliases, definitions);
        }
        Expr::ObjectBody(entries) => {
            collect_entry_refs_in(entries, refs, shadows, aliases, definitions)
        }
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(expr) = part {
                    collect_expr_refs_in(expr, refs, shadows, aliases, definitions);
                }
            }
        }
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Import(..)
        | Expr::ImportGlob(..) => {}
    }
}

fn collect_type_refs(
    ty: &crate::parser::TypeExpr,
    refs: &mut HashSet<String>,
    shadows: &HashSet<String>,
    aliases: Option<&TypeAliases>,
    definitions: Option<&Definitions>,
) {
    match ty {
        crate::parser::TypeExpr::Named(name) => collect_name_root(name, refs, shadows),
        crate::parser::TypeExpr::Nullable(inner) => {
            collect_type_refs(inner, refs, shadows, aliases, definitions)
        }
        crate::parser::TypeExpr::Union(types) => {
            for ty in types {
                collect_type_refs(ty, refs, shadows, aliases, definitions);
            }
        }
        crate::parser::TypeExpr::Generic(name, params) => {
            collect_name_root(name, refs, shadows);
            for param in params {
                collect_type_refs(param, refs, shadows, aliases, definitions);
            }
        }
        crate::parser::TypeExpr::Constrained(base, constraint) => {
            for component in constrained_type_components(base) {
                collect_name_root(component, refs, shadows);
            }
            // A type check binds the checked value's own names inside the
            // constraint, so they are not references to the enclosing scope.
            let mut constraint_shadows = shadows.clone();
            constraint_shadows.extend(
                constraint_bound_names_resolving(base, aliases)
                    .iter()
                    .map(|name| name.to_string()),
            );
            collect_expr_refs_in(constraint, refs, &constraint_shadows, aliases, definitions);
        }
    }
}

pub(super) fn collect_name_root(name: &str, refs: &mut HashSet<String>, shadows: &HashSet<String>) {
    if let Some(root) = name.split('.').next()
        && !root.is_empty()
        && !shadows.contains(root)
    {
        refs.insert(root.to_string());
    }
}

pub(super) fn constrained_type_components(name: &str) -> impl Iterator<Item = &str> {
    name.trim_start_matches('*')
        .trim_end_matches('?')
        .split(['<', '>', ','])
        .map(str::trim)
        .filter(|component| !component.is_empty())
}

pub(super) fn declared_entry_roots(entries: &[Entry]) -> HashSet<String> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::ClassDef(name, ..) | Entry::TypeAlias(name, _) => Some(name.clone()),
            Entry::Property(prop)
                if has_modifier(&prop.modifiers, Modifier::Local)
                    || matches!(prop.value, Some(Expr::Lambda(..))) =>
            {
                Some(prop.name.clone())
            }
            _ => None,
        })
        .collect()
}

/// Every name an expression could resolve in its enclosing scope, ignoring
/// shadowing. Over-approximating is safe for deciding what a lambda captures:
/// a local of an object built in the body may be read before it is declared,
/// so an enclosing binding of the same name must stay reachable.
pub(super) fn collect_unshadowed_names(expr: &Expr, names: &mut HashSet<String>) {
    match expr {
        Expr::Ident(name) => {
            names.insert(name.clone());
        }
        Expr::New(type_name, entries, generic_params) => {
            if type_name.is_some() || !generic_params.is_empty() {
                mark_type_use(names);
            }
            collect_unshadowed_entry_names(entries, names);
        }
        Expr::Field(base, _) | Expr::NullSafeField(base, _) => {
            collect_unshadowed_names(base, names);
        }
        Expr::Index(base, index) | Expr::Binop(_, base, index) => {
            collect_unshadowed_names(base, names);
            collect_unshadowed_names(index, names);
        }
        Expr::Call(callee, args) => {
            collect_unshadowed_names(callee, names);
            for arg in args {
                collect_unshadowed_names(arg, names);
            }
        }
        Expr::If(cond, then_expr, else_expr) => {
            collect_unshadowed_names(cond, names);
            collect_unshadowed_names(then_expr, names);
            collect_unshadowed_names(else_expr, names);
        }
        Expr::Let(_, value, body) => {
            collect_unshadowed_names(value, names);
            collect_unshadowed_names(body, names);
        }
        Expr::Is(value, ty) | Expr::As(value, ty) => {
            collect_unshadowed_names(value, names);
            collect_unshadowed_type_names(ty, names);
        }
        Expr::Lambda(_, value)
        | Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::Trace(value)
        | Expr::Read(value)
        | Expr::ReadOrNull(value) => collect_unshadowed_names(value, names),
        Expr::InferredNew(ty, entries) => {
            collect_unshadowed_type_names(ty, names);
            collect_unshadowed_entry_names(entries, names);
        }
        Expr::ObjectBody(entries) => collect_unshadowed_entry_names(entries, names),
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(expr) = part {
                    collect_unshadowed_names(expr, names);
                }
            }
        }
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Import(..)
        | Expr::ImportGlob(..) => {}
    }
}

fn collect_unshadowed_entry_names(entries: &[Entry], names: &mut HashSet<String>) {
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(ty) = &prop.type_ann {
                    collect_unshadowed_type_names(ty, names);
                }
                if let Some(expr) = &prop.value {
                    collect_unshadowed_names(expr, names);
                }
                if let Some(body) = &prop.body {
                    collect_unshadowed_entry_names(body, names);
                }
            }
            Entry::DynProperty(key, value) => {
                collect_unshadowed_names(key, names);
                collect_unshadowed_names(value, names);
            }
            Entry::ForGenerator(fgen) => {
                collect_unshadowed_names(&fgen.collection, names);
                collect_unshadowed_entry_names(&fgen.body, names);
            }
            Entry::WhenGenerator(wgen) => {
                collect_unshadowed_names(&wgen.condition, names);
                collect_unshadowed_entry_names(&wgen.body, names);
                if let Some(else_body) = &wgen.else_body {
                    collect_unshadowed_entry_names(else_body, names);
                }
            }
            Entry::Spread(expr) | Entry::Elem(expr) => collect_unshadowed_names(expr, names),
            Entry::ClassDef(_, _, parent, body) => {
                if parent.is_some() {
                    mark_type_use(names);
                }
                collect_unshadowed_entry_names(body, names);
            }
            Entry::TypeAlias(_, ty) => collect_unshadowed_type_names(ty, names),
        }
    }
}

fn collect_unshadowed_type_names(ty: &crate::parser::TypeExpr, names: &mut HashSet<String>) {
    mark_type_use(names);
    // A constraint is an ordinary expression evaluated against the value.
    if let crate::parser::TypeExpr::Constrained(_, constraint) = ty {
        collect_unshadowed_names(constraint, names);
    }
    match ty {
        crate::parser::TypeExpr::Nullable(inner) => collect_unshadowed_type_names(inner, names),
        crate::parser::TypeExpr::Union(types) | crate::parser::TypeExpr::Generic(_, types) => {
            for ty in types {
                collect_unshadowed_type_names(ty, names);
            }
        }
        _ => {}
    }
}

/// Recorded by [`collect_unshadowed_names`] when the expression names a type
/// or class anywhere (`new T`, `is`/`as`, type annotations, constraints, class
/// parents). Type names come in many spellings (defaults, generics, quoted
/// names containing separators), and resolving them looks up bindings in ways
/// a name list cannot capture reliably, so callers that see this capture the
/// whole scope.
pub(super) const NAMES_A_TYPE: &str = "\0type";

fn mark_type_use(names: &mut HashSet<String>) {
    names.insert(NAMES_A_TYPE.to_string());
}

/// Whether `name` appears as an identifier anywhere in `entries`, including
/// nested bodies, lambdas and type constraints, ignoring shadowing. Cheaper
/// than collecting every name when only one matters.
pub(super) fn entries_mention(entries: &[Entry], name: &str) -> bool {
    entries.iter().any(|entry| entry_mentions(entry, name))
}

fn property_mentions(prop: &Property, name: &str) -> bool {
    prop.type_ann
        .as_ref()
        .is_some_and(|ty| type_mentions(ty, name))
        || prop
            .value
            .as_ref()
            .is_some_and(|expr| expr_mentions(expr, name))
        || prop
            .body
            .as_ref()
            .is_some_and(|body| entries_mention(body, name))
}

fn entry_mentions(entry: &Entry, name: &str) -> bool {
    match entry {
        Entry::Property(prop) => property_mentions(prop, name),
        Entry::DynProperty(key, value) => expr_mentions(key, name) || expr_mentions(value, name),
        Entry::ForGenerator(fgen) => {
            expr_mentions(&fgen.collection, name) || entries_mention(&fgen.body, name)
        }
        Entry::WhenGenerator(wgen) => {
            expr_mentions(&wgen.condition, name)
                || entries_mention(&wgen.body, name)
                || wgen
                    .else_body
                    .as_ref()
                    .is_some_and(|body| entries_mention(body, name))
        }
        Entry::Spread(expr) | Entry::Elem(expr) => expr_mentions(expr, name),
        Entry::ClassDef(_, _, parent, body) => {
            parent
                .as_deref()
                .is_some_and(|parent| type_name_mentions(parent, name))
                || entries_mention(body, name)
        }
        Entry::TypeAlias(_, ty) => type_mentions(ty, name),
    }
}

/// Whether a type name as written (`outer.Step`, `*Foo<Bar>?`, a quoted name)
/// contains `name` as an identifier, so a type resolved through that binding
/// counts as a mention.
fn type_name_mentions(type_name: &str, name: &str) -> bool {
    type_name
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .any(|token| token == name)
}

fn expr_mentions(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Ident(ident) => ident == name,
        Expr::New(type_name, entries, generic_params) => {
            type_name
                .iter()
                .chain(generic_params)
                .any(|type_name| type_name_mentions(type_name, name))
                || entries_mention(entries, name)
        }
        Expr::ObjectBody(entries) => entries_mention(entries, name),
        Expr::InferredNew(ty, entries) => type_mentions(ty, name) || entries_mention(entries, name),
        Expr::Field(base, _) | Expr::NullSafeField(base, _) => expr_mentions(base, name),
        Expr::Index(base, index) | Expr::Binop(_, base, index) => {
            expr_mentions(base, name) || expr_mentions(index, name)
        }
        Expr::Call(callee, args) => {
            expr_mentions(callee, name) || args.iter().any(|arg| expr_mentions(arg, name))
        }
        Expr::If(cond, then_expr, else_expr) => {
            expr_mentions(cond, name)
                || expr_mentions(then_expr, name)
                || expr_mentions(else_expr, name)
        }
        Expr::Let(_, value, body) => expr_mentions(value, name) || expr_mentions(body, name),
        Expr::Is(value, ty) | Expr::As(value, ty) => {
            expr_mentions(value, name) || type_mentions(ty, name)
        }
        Expr::Lambda(_, value)
        | Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::Trace(value)
        | Expr::Read(value)
        | Expr::ReadOrNull(value) => expr_mentions(value, name),
        Expr::StringInterpolation(parts) => parts.iter().any(|part| match part {
            StringInterpPart::Expr(expr) => expr_mentions(expr, name),
            StringInterpPart::Literal(_) => false,
        }),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Import(..)
        | Expr::ImportGlob(..) => false,
    }
}

pub(super) fn type_mentions(ty: &crate::parser::TypeExpr, name: &str) -> bool {
    match ty {
        crate::parser::TypeExpr::Constrained(base, constraint) => {
            type_name_mentions(base, name) || expr_mentions(constraint, name)
        }
        crate::parser::TypeExpr::Nullable(inner) => type_mentions(inner, name),
        crate::parser::TypeExpr::Union(types) => types.iter().any(|ty| type_mentions(ty, name)),
        crate::parser::TypeExpr::Generic(base, types) => {
            type_name_mentions(base, name) || types.iter().any(|ty| type_mentions(ty, name))
        }
        crate::parser::TypeExpr::Named(type_name) => type_name_mentions(type_name, name),
    }
}
