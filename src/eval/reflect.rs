//! The `pkl:reflect` module: mirrors of classes, modules, type aliases and
//! types, built from the declarations' source. A class or type alias of a
//! user's module carries the location, doc comment and annotations its
//! declaration has; a class of the standard library carries those `base.pkl`
//! gives it (see [`super::reflect_base`]).

use super::amend::definition_type_scope;
use super::reflect_base::{self, AnnotationValue, BaseClass};
use super::scope::object_source_type_namespace;
use super::stdlib::{
    TYPE_ALIAS_IDENTITY_KEY, TYPE_ALIAS_NAME_KEY, module_class_name, module_class_namespace,
    typed_object,
};
use super::types::resolve_dotted;
use super::*;
use crate::parser::{Annotation, BaseForm, Declaration, SourcePos, TypeExpr};

/// The identity of the `pkl:reflect` module object.
pub(super) const MODULE: &str = "pkl:reflect";

/// The `pkl:reflect` module object. Its functions are answered by
/// [`Evaluator::reflect_call`].
pub(super) fn module() -> Value {
    let Value::Object(members, Some(source)) = typed_object("reflect", ObjectMap::default()) else {
        unreachable!("typed_object builds an object with a source")
    };
    let mut source = Arc::unwrap_or_clone(source);
    source.type_identity = Some(MODULE.into());
    Value::Object(members, Some(Arc::new(source)))
}

/// Whether `value` is the `pkl:reflect` module object.
pub(super) fn is_module(value: &Value) -> bool {
    matches!(value, Value::Object(_, Some(source)) if source.type_identity.as_deref() == Some(MODULE))
}

fn strings<'a>(names: impl IntoIterator<Item = &'a str>) -> Value {
    Value::List(ListValue::new(
        ListKind::Set,
        names
            .into_iter()
            .map(|name| Value::String(name.into()))
            .collect::<Vec<_>>(),
    ))
}

fn modifier_name(modifier: &Modifier) -> Option<&'static str> {
    Some(match modifier {
        // `Modifier` has no `local`: a local declaration is not reflected on.
        Modifier::Local => return None,
        Modifier::Const => "const",
        Modifier::Fixed => "fixed",
        Modifier::Hidden => "hidden",
        Modifier::Abstract => "abstract",
        Modifier::Open => "open",
        Modifier::External => "external",
    })
}

fn modifier_names(modifiers: &[Modifier]) -> Vec<&'static str> {
    modifiers.iter().filter_map(modifier_name).collect()
}

fn list_of(kind: ListKind, items: Vec<Value>) -> Value {
    Value::List(ListValue::new(kind, items))
}

fn string_or_null(text: Option<&str>) -> Value {
    text.map_or(Value::Null, |text| Value::String(text.into()))
}

fn map_of(entries: ObjectMap) -> Value {
    Value::Object(Arc::new(entries), None)
}

/// Where a declaration starts, as `pkl:reflect` reports it.
#[derive(Clone)]
struct Location {
    line: u32,
    column: u32,
    display_uri: String,
}

impl Location {
    /// A position in the module whose type namespace is `namespace`.
    fn in_module(namespace: &str, at: SourcePos) -> Location {
        let uri = if namespace.starts_with('/') {
            format!("file://{namespace}")
        } else {
            namespace.to_string()
        };
        Location {
            line: at.line,
            column: at.col,
            display_uri: format!("{uri}, line {}", at.line),
        }
    }

    /// A position in the standard library's `base.pkl`.
    fn in_base(line: u32, column: u32) -> Location {
        Location {
            line,
            column,
            display_uri: reflect_base::display_uri(line),
        }
    }

    fn value(&self) -> Value {
        let mut members = ObjectMap::default();
        members.insert("line".into(), Value::Int(i64::from(self.line)));
        members.insert("column".into(), Value::Int(i64::from(self.column)));
        members.insert(
            "displayUri".into(),
            Value::String(self.display_uri.as_str().into()),
        );
        mirror("SourceLocation", members)
    }
}

/// A type parameter, such as `out Element`.
#[derive(Clone)]
struct TypeParameter {
    variance: &'static str,
    name: String,
}

impl TypeParameter {
    fn value(&self) -> Value {
        let mut members = ObjectMap::default();
        members.insert("name".into(), Value::String(self.name.as_str().into()));
        members.insert(
            "variance".into(),
            string_or_null(Some(self.variance).filter(|variance| !variance.is_empty())),
        );
        mirror("TypeParameter", members)
    }
}

/// A property or method as its declaration gives it.
struct MemberData {
    is_method: bool,
    name: String,
    location: Option<Location>,
    doc: Option<String>,
    annotations: Vec<Value>,
    modifiers: Vec<&'static str>,
    type_parameters: Vec<TypeParameter>,
    parameters: Vec<String>,
}

/// A class as its declaration gives it, and its superclasses.
struct ClassData {
    name: String,
    location: Option<Location>,
    doc: Option<String>,
    annotations: Vec<Value>,
    modifiers: Vec<&'static str>,
    type_parameters: Vec<TypeParameter>,
    members: Vec<MemberData>,
    /// The `extends` clause as written, when it names type arguments.
    extends: Option<&'static str>,
    superclass: Option<Box<ClassData>>,
}

/// A class mirror, with the members it declares and inherits.
struct ClassMirror {
    value: Value,
    all_properties: ObjectMap,
    all_methods: ObjectMap,
}

impl MemberData {
    /// The mirror of this member. `inherited` is the mirror of the member this
    /// one overrides, whose modifiers and annotations a property merges in.
    fn mirror(&self, inherited: Option<&Value>) -> Value {
        let mut members = ObjectMap::default();
        members.insert(
            "location".into(),
            self.location.as_ref().map_or(Value::Null, Location::value),
        );
        members.insert("docComment".into(), string_or_null(self.doc.as_deref()));
        members.insert(
            "annotations".into(),
            list_of(ListKind::Listing, self.annotations.clone()),
        );
        members.insert("modifiers".into(), strings(self.modifiers.iter().copied()));
        members.insert("name".into(), Value::String(self.name.as_str().into()));
        if self.is_method {
            members.insert(
                "typeParameters".into(),
                list_of(
                    ListKind::List,
                    self.type_parameters
                        .iter()
                        .map(TypeParameter::value)
                        .collect(),
                ),
            );
            let parameters = self
                .parameters
                .iter()
                .map(|name| {
                    let mut parameter = ObjectMap::default();
                    parameter.insert("name".into(), Value::String(name.as_str().into()));
                    (name.as_str().into(), mirror("MethodParameter", parameter))
                })
                .collect();
            members.insert("parameters".into(), map_of(parameters));
            return mirror("Method", members);
        }
        let inherited_members = match inherited {
            Some(Value::Object(members, _)) => Some(members),
            _ => None,
        };
        let mut all_modifiers: Vec<Value> = self
            .modifiers
            .iter()
            .map(|modifier| Value::String((*modifier).into()))
            .collect();
        let mut all_annotations = self.annotations.clone();
        if let Some(inherited) = inherited_members {
            if let Some(Value::List(list)) = inherited.get("allModifiers") {
                for modifier in list.iter() {
                    if !all_modifiers.contains(modifier) {
                        all_modifiers.push(modifier.clone());
                    }
                }
            }
            if let Some(Value::List(list)) = inherited.get("allAnnotations") {
                all_annotations.extend(list.iter().cloned());
            }
        }
        members.insert("allModifiers".into(), list_of(ListKind::Set, all_modifiers));
        members.insert(
            "allAnnotations".into(),
            list_of(ListKind::Listing, all_annotations),
        );
        mirror("Property", members)
    }
}

impl ClassData {
    fn mirror(&self) -> ClassMirror {
        let parent = self.superclass.as_deref().map(ClassData::mirror);
        let mut all_properties = parent
            .as_ref()
            .map_or_else(ObjectMap::default, |parent| parent.all_properties.clone());
        let mut all_methods = parent
            .as_ref()
            .map_or_else(ObjectMap::default, |parent| parent.all_methods.clone());
        let mut properties = ObjectMap::default();
        let mut methods = ObjectMap::default();
        for member in &self.members {
            let (own, all) = if member.is_method {
                (&mut methods, &mut all_methods)
            } else {
                (&mut properties, &mut all_properties)
            };
            let key: Arc<str> = member.name.as_str().into();
            let mirror = member.mirror(all.get(&key));
            own.insert(key.clone(), mirror.clone());
            all.insert(key, mirror);
        }
        let mut members = ObjectMap::default();
        members.insert(
            "location".into(),
            self.location.as_ref().map_or(Value::Null, Location::value),
        );
        members.insert("docComment".into(), string_or_null(self.doc.as_deref()));
        members.insert(
            "annotations".into(),
            list_of(ListKind::Listing, self.annotations.clone()),
        );
        members.insert("modifiers".into(), strings(self.modifiers.iter().copied()));
        members.insert("name".into(), Value::String(self.name.as_str().into()));
        members.insert(
            "typeParameters".into(),
            list_of(
                ListKind::List,
                self.type_parameters
                    .iter()
                    .map(TypeParameter::value)
                    .collect(),
            ),
        );
        let (superclass, supertype) = match (&parent, &self.superclass) {
            (Some(parent), Some(superclass)) => {
                let arguments = self.supertype_arguments(superclass);
                (
                    parent.value.clone(),
                    declared_type_value(parent.value.clone(), arguments),
                )
            }
            _ => (Value::Null, Value::Null),
        };
        members.insert("superclass".into(), superclass);
        members.insert("supertype".into(), supertype);
        members.insert("properties".into(), map_of(properties));
        members.insert("allProperties".into(), map_of(all_properties.clone()));
        members.insert("methods".into(), map_of(methods));
        members.insert("allMethods".into(), map_of(all_methods.clone()));
        ClassMirror {
            value: mirror("Class", members),
            all_properties,
            all_methods,
        }
    }

    /// The type arguments of this class's supertype, `Element` in
    /// `class Set<Element> extends Collection<Element>`.
    fn supertype_arguments(&self, superclass: &ClassData) -> Vec<Value> {
        let arguments = self
            .extends
            .and_then(|extends| extends.split_once('<'))
            .map(|(_, rest)| split_arguments(rest.strip_suffix('>').unwrap_or(rest)))
            .unwrap_or_default();
        (0..superclass.type_parameters.len())
            .map(|index| match arguments.get(index) {
                Some(argument) => self.type_argument(argument, 0),
                None => mirror("UnknownType", ObjectMap::default()),
            })
            .collect()
    }

    /// The mirror of a type written in a `base.pkl` `extends` clause.
    fn type_argument(&self, written: &str, nesting: usize) -> Value {
        let written = written.trim();
        if let Some(parameter) = self.type_parameters.iter().find(|p| p.name == written) {
            let mut members = ObjectMap::default();
            members.insert("referent".into(), parameter.value());
            return mirror("TypeVariable", members);
        }
        let unknown = || mirror("UnknownType", ObjectMap::default());
        let (name, arguments) = match written.split_once('<') {
            Some((name, rest)) => (
                name,
                split_arguments(rest.strip_suffix('>').unwrap_or(rest)),
            ),
            None => (written, Vec::new()),
        };
        if nesting > 4 {
            return unknown();
        }
        let Some(class) = base_class_data(name) else {
            return unknown();
        };
        let arguments = (0..class.type_parameters.len())
            .map(|index| match arguments.get(index) {
                Some(argument) => self.type_argument(argument, nesting + 1),
                None => unknown(),
            })
            .collect();
        declared_type_value(class.mirror().value, arguments)
    }
}

/// The top-level, comma-separated parts of a type argument list.
fn split_arguments(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (index, ch) in list.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&list[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[start..]);
    parts
}

/// The declaration of the class or type alias `name`. A declaration at the top
/// of its module wins over a nested one of the same name.
fn find_declaration<'a>(
    declarations: &'a [Declaration],
    name: &str,
    is_class: bool,
) -> Option<&'a Declaration> {
    declarations
        .iter()
        .filter(|d| d.is_class == is_class && *d.name == *name)
        .min_by_key(|d| d.nested)
}

/// The `pkl:base` class named `name`, with its superclasses.
fn base_class_data(name: &str) -> Option<ClassData> {
    let class = reflect_base::find(name)?;
    Some(base_class_data_of(class))
}

fn base_class_data_of(class: BaseClass) -> ClassData {
    let superclass_name = match class.extends {
        Some(extends) => extends.split('<').next().unwrap_or(extends),
        None if class.name == "Any" => "",
        // What a class without an `extends` clause extends: `Typed`, unless it
        // is `external`, which extends `Any`.
        None if class.modifiers.contains(&"external") => "Any",
        None => "Typed",
    };
    let superclass = (!superclass_name.is_empty())
        .then(|| base_class_data(superclass_name))
        .flatten()
        .map(Box::new);
    let type_parameters = |params: &[reflect_base::TypeParam]| {
        params
            .iter()
            .map(|param| TypeParameter {
                variance: param.variance,
                name: param.name.to_string(),
            })
            .collect::<Vec<_>>()
    };
    let members = class
        .members
        .iter()
        .map(|member| MemberData {
            is_method: member.is_method,
            name: member.name.to_string(),
            location: Some(Location::in_base(member.line, 3)),
            doc: member.doc.clone(),
            annotations: member.annotations.iter().map(base_annotation).collect(),
            modifiers: member.modifiers.clone(),
            type_parameters: type_parameters(&member.type_params),
            parameters: member.params.iter().map(|p| p.to_string()).collect(),
        })
        .collect();
    ClassData {
        name: class.name.to_string(),
        location: Some(Location::in_base(class.line, 1)),
        doc: class.doc.clone(),
        annotations: class.annotations.iter().map(base_annotation).collect(),
        modifiers: class.modifiers.clone(),
        type_parameters: type_parameters(&class.type_params),
        members,
        extends: class.extends,
        superclass,
    }
}

/// The value of an annotation of a standard library declaration.
fn base_annotation(annotation: &reflect_base::Annotation) -> Value {
    let mut members = ObjectMap::default();
    for (key, value) in &annotation.properties {
        let value = match value {
            AnnotationValue::String(text) => Value::String((*text).into()),
            AnnotationValue::Strings(items) => list_of(
                ListKind::Listing,
                items
                    .iter()
                    .map(|item| Value::String((*item).into()))
                    .collect(),
            ),
        };
        members.insert((*key).into(), value);
    }
    typed_object(annotation.name, members)
}

/// `DeclaredType` of a class or type alias mirror.
fn declared_type_value(referent: Value, arguments: Vec<Value>) -> Value {
    let mut members = ObjectMap::default();
    members.insert("referent".into(), referent);
    members.insert("typeArguments".into(), list_of(ListKind::List, arguments));
    mirror("DeclaredType", members)
}

/// What `reflect.DeclaredType(referent)` returns: its type arguments default to
/// `unknownType`.
fn declared_type(referent: &Value) -> Result<Value> {
    let parameters = match referent {
        Value::Object(members, Some(source))
            if matches!(
                source.type_identity.as_deref(),
                Some("pkl:reflect#Class" | "pkl:reflect#TypeAlias")
            ) =>
        {
            match members.get("typeParameters") {
                Some(Value::List(list)) => list.iter().count(),
                _ => 0,
            }
        }
        _ => return Err(super::stdlib::type_mismatch("TypeDeclaration", referent)),
    };
    Ok(declared_type_value(
        referent.clone(),
        (0..parameters)
            .map(|_| mirror("UnknownType", ObjectMap::default()))
            .collect(),
    ))
}

fn union_type(members: &Value) -> Result<Value> {
    let Value::List(list) = members else {
        return Err(super::stdlib::type_mismatch("List", members));
    };
    let mut object = ObjectMap::default();
    object.insert(
        "members".into(),
        list_of(ListKind::List, list.iter().cloned().collect()),
    );
    Ok(mirror("UnionType", object))
}

fn string_literal_type(value: &Value) -> Result<Value> {
    let Value::String(_) = value else {
        return Err(super::stdlib::type_mismatch("String", value));
    };
    let mut object = ObjectMap::default();
    object.insert("value".into(), value.clone());
    Ok(mirror("StringLiteralType", object))
}

fn type_variable(parameter: &Value) -> Result<Value> {
    match parameter {
        Value::Object(_, Some(source))
            if source.type_identity.as_deref() == Some("pkl:reflect#TypeParameter") =>
        {
            let mut object = ObjectMap::default();
            object.insert("referent".into(), parameter.clone());
            Ok(mirror("TypeVariable", object))
        }
        _ => Err(super::stdlib::type_mismatch("TypeParameter", parameter)),
    }
}

impl Evaluator {
    /// A call of a `pkl:reflect` function, such as `Class(c)`, `Module(m)`,
    /// `TypeAlias(a)` or `DeclaredType(d)`. `None` when `method` is not one of them.
    #[inline(never)]
    pub(super) fn reflect_call(
        &mut self,
        method: &str,
        arg_exprs: &[Expr],
        args: &[Value],
        scope: &Scope,
        depth: usize,
    ) -> Option<Result<Value>> {
        if !matches!(
            method,
            "Class"
                | "Module"
                | "TypeAlias"
                | "DeclaredType"
                | "UnionType"
                | "StringLiteralType"
                | "TypeVariable"
        ) {
            return None;
        }
        let [arg] = args else {
            return Some(Err(Error::Eval(format!(
                "Expected 1 function arguments but got {}.",
                args.len()
            ))));
        };
        Some(match method {
            "Class" => self.reflect_class(arg, scope, depth),
            "TypeAlias" => self.reflect_type_alias(arg, scope, depth, &mut Vec::new()),
            "DeclaredType" => declared_type(arg),
            "UnionType" => union_type(arg),
            "StringLiteralType" => string_literal_type(arg),
            "TypeVariable" => type_variable(arg),
            _ => {
                let is_module_binding =
                    matches!(arg_exprs, [Expr::Ident(name)] if name == "module");
                self.reflect_module(arg, is_module_binding, scope, depth)
            }
        })
    }

    fn reflect_class(&mut self, class: &Value, scope: &Scope, depth: usize) -> Result<Value> {
        let Value::Object(_, Some(source)) = class else {
            return Err(super::stdlib::type_mismatch("Class", class));
        };
        if source.kind != ObjectKind::Class {
            return Err(super::stdlib::type_mismatch("Class", class));
        }
        // The class of a module (`module.getClass()`).
        if let Some(identity) = source
            .type_identity
            .as_deref()
            .filter(|identity| module_class_name(identity).is_some())
        {
            return Ok(self.module_mirror(identity, scope, depth)?.1);
        }
        let data = self.class_data(source, scope, depth)?;
        Ok(data.mirror().value)
    }

    /// The class `source` describes: a standard library class from `base.pkl`'s
    /// declarations, any other from the module that declares it.
    fn class_data(
        &mut self,
        source: &ObjectSource,
        scope: &Scope,
        depth: usize,
    ) -> Result<ClassData> {
        let name = source.type_name().unwrap_or("Class");
        if let Some(base) = source
            .type_identity
            .as_deref()
            .and_then(|identity| identity.strip_prefix("pkl:base#"))
            && let Some(data) = base_class_data(base)
        {
            return Ok(data);
        }
        let namespace = object_source_type_namespace(source);
        // A class of the module being evaluated sees all of its declarations,
        // including ones that follow the class.
        let class_scope = if scope.type_namespace == namespace {
            scope.clone()
        } else {
            definition_type_scope(source)
        };
        let declarations = namespace
            .as_ref()
            .and_then(|namespace| self.module_declarations.get(namespace))
            .cloned();
        let declaration = declarations
            .as_deref()
            .and_then(|all| find_declaration(all, name, true));
        let mut members = Vec::new();
        for (index, entry) in source.entries.iter().enumerate() {
            // Only what this class declares; the entries it inherits from a
            // parent class carry the parent's scope.
            if source.entry_scopes.get(index).is_some_and(Option::is_some) {
                continue;
            }
            let Entry::Property(prop) = entry else {
                continue;
            };
            if prop.modifiers.contains(&Modifier::Local) {
                continue;
            }
            let at = declaration.and_then(|declaration| {
                declaration
                    .members
                    .iter()
                    .find(|(member, _)| *member == prop.name)
                    .map(|(_, at)| *at)
            });
            let location = at
                .zip(namespace.as_deref())
                .map(|(at, namespace)| Location::in_module(namespace, at));
            members.push(self.member_data(prop, location, &class_scope, depth)?);
        }
        if let Some(declaration) = declaration
            && !declaration.bodyless_methods.is_empty()
        {
            for method in &declaration.bodyless_methods {
                let location = namespace
                    .as_deref()
                    .map(|namespace| Location::in_module(namespace, method.at));
                members.push(MemberData {
                    is_method: true,
                    name: method.name.to_string(),
                    location,
                    doc: method.doc.as_deref().map(str::to_string),
                    annotations: self.eval_annotations(&method.annotations, &class_scope, depth)?,
                    modifiers: modifier_names(&method.modifiers),
                    type_parameters: Vec::new(),
                    parameters: method.parameters.clone(),
                });
            }
            // Members read in the order the class declares them.
            members.sort_by_key(|member| {
                member
                    .location
                    .as_ref()
                    .map_or((u32::MAX, 0), |at| (at.line, at.column))
            });
        }
        let parent_name = declaration.and_then(|declaration| declaration.parent.as_deref());
        let parent = parent_name.and_then(|name| resolve_dotted(&class_scope, name));
        let superclass = match &parent {
            Some(Value::Object(_, Some(parent))) if parent.kind == ObjectKind::Class => {
                self.class_data(parent, &class_scope, depth)?
            }
            _ => base_class_data("Typed").unwrap_or_else(|| empty_class("Typed")),
        };
        let annotations = match declaration {
            Some(declaration) => {
                self.eval_annotations(&declaration.annotations, &class_scope, depth)?
            }
            None => Vec::new(),
        };
        let modifiers = match declaration {
            Some(declaration) => modifier_names(&declaration.modifiers),
            None => {
                let mut modifiers = Vec::new();
                if source.is_abstract {
                    modifiers.push("abstract");
                }
                if source.is_open {
                    modifiers.push("open");
                }
                modifiers
            }
        };
        Ok(ClassData {
            name: name.to_string(),
            location: declaration
                .zip(namespace.as_deref())
                .map(|(declaration, namespace)| Location::in_module(namespace, declaration.at)),
            doc: declaration
                .and_then(|declaration| declaration.doc.as_deref())
                .map(str::to_string),
            annotations,
            modifiers,
            type_parameters: Vec::new(),
            members,
            extends: None,
            superclass: Some(Box::new(superclass)),
        })
    }

    fn reflect_module(
        &mut self,
        module: &Value,
        is_module_binding: bool,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let Value::Object(props, source) = module else {
            return Err(super::stdlib::type_mismatch("Module", module));
        };
        let with_class;
        let source = match source {
            Some(source) => Some(source),
            None if is_module_binding => {
                with_class = self.module_with_class(props, scope);
                match &with_class {
                    Value::Object(_, source) => source.as_ref(),
                    _ => None,
                }
            }
            None => None,
        };
        let identity = source
            .and_then(|source| source.type_identity.as_deref())
            .filter(|identity| module_class_name(identity).is_some());
        let Some(identity) = identity else {
            return Err(unsupported_module());
        };
        let name = module_class_name(identity).unwrap_or_default().to_string();
        let (parsed, module_class) = self.module_mirror(identity, scope, depth)?;
        let mut imports = ObjectMap::default();
        for import in &parsed.imports {
            if import.is_glob {
                continue;
            }
            let key = import.alias.clone().unwrap_or_else(|| {
                let last = import.uri.rsplit(['/', ':']).next().unwrap_or(&import.uri);
                last.strip_suffix(".pkl").unwrap_or(last).to_string()
            });
            imports.insert(key.into(), Value::String(import.uri.as_str().into()));
        }
        let mut members = ObjectMap::default();
        members.insert("name".into(), Value::String(name.as_str().into()));
        members.insert("imports".into(), Value::Object(Arc::new(imports), None));
        members.insert("moduleClass".into(), module_class);
        Ok(mirror("Module", members))
    }

    /// The name of the module with class identity `identity` and the mirror of
    /// its class, and its parsed source.
    fn module_mirror(
        &mut self,
        identity: &str,
        scope: &Scope,
        depth: usize,
    ) -> Result<(Arc<Module>, Value)> {
        let Some(namespace) = module_class_namespace(identity) else {
            return Err(unsupported_module());
        };
        let name = module_class_name(identity).unwrap_or_default().to_string();
        let parsed = self.parse_file(Path::new(namespace))?;
        let mut modifiers = Vec::new();
        for annotation in &parsed.annotations {
            match annotation.name.as_str() {
                "pklr:module:Open" => modifiers.push("open"),
                "pklr:module:Abstract" => modifiers.push("abstract"),
                _ => {}
            }
        }
        let mut members = Vec::new();
        for entry in parsed.body.iter() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if prop.modifiers.contains(&Modifier::Local) {
                continue;
            }
            members.push(self.member_data(prop, None, scope, depth)?);
        }
        let data = ClassData {
            name,
            location: None,
            doc: None,
            annotations: Vec::new(),
            modifiers,
            type_parameters: Vec::new(),
            members,
            extends: None,
            superclass: base_class_data("Module").map(Box::new),
        };
        Ok((parsed, data.mirror().value))
    }

    /// The annotations of the property `name` of an instance of the class
    /// `source` describes, evaluated where the class was declared.
    pub(super) fn property_annotation_values(
        &mut self,
        source: &ObjectSource,
        name: &str,
        depth: usize,
    ) -> Result<Vec<Value>> {
        let mut annotations = Vec::new();
        self.collect_property_annotations(source, name, depth, true, &mut annotations)?;
        Ok(annotations)
    }

    /// Append the annotations of the property `name` of `source` to `out`.
    /// `inherited` says whether to include those a module or class inherits.
    fn collect_property_annotations(
        &mut self,
        source: &ObjectSource,
        name: &str,
        depth: usize,
        inherited: bool,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        // What a module or class inherits comes first, each annotation
        // evaluated where its declaration is.
        let mut chain = Vec::new();
        let mut next = source.module_annotations.as_ref().filter(|_| inherited);
        while let Some(level) = next {
            chain.push(level);
            next = level.parent.as_ref();
        }
        for level in chain.into_iter().rev() {
            let scope = super::scope::restore_scope(&level.scope);
            for (_, list) in level.properties.iter().filter(|(n, _)| &**n == name) {
                out.extend(self.eval_annotations(list, &scope, depth)?);
            }
        }
        let scope = definition_type_scope(source);
        let mut declared_here = false;
        let mut annotated = false;
        for (index, entry) in source.entries.iter().enumerate() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if &*prop.name != name {
                continue;
            }
            declared_here = true;
            // An entry copied from a parent keeps the scope it was declared in.
            let declared = source
                .entry_scopes
                .get(index)
                .and_then(Option::as_ref)
                .map(|captured| super::scope::restore_scope(captured));
            let scope = declared.as_ref().unwrap_or(&scope);
            // `pkl:base`'s own annotations never convert a property.
            let own: Vec<Annotation> = prop
                .annotations
                .iter()
                .filter(|annotation| {
                    !super::module::STDLIB_ANNOTATIONS.contains(&&*annotation.name)
                })
                .cloned()
                .collect();
            annotated |= !prop.annotations.is_empty();
            out.extend(self.eval_annotations(&own, scope, depth)?);
        }
        // An amendment (`new C { a = 1 }`) replaces the property without its
        // annotations, which the object it amends still declares.
        if declared_here
            && !annotated
            && let Some(Value::Object(_, Some(prototype))) = &source.prototype
        {
            let inherited = source.module_annotations.is_none();
            self.collect_property_annotations(prototype, name, depth, inherited, out)?;
        }
        Ok(())
    }

    /// The values of `annotations`, each the instance its declaration writes.
    fn eval_annotations(
        &mut self,
        annotations: &[Annotation],
        scope: &Scope,
        depth: usize,
    ) -> Result<Vec<Value>> {
        let mut values = Vec::with_capacity(annotations.len());
        for annotation in annotations {
            let expr = Expr::New(
                Some(annotation.name.clone()),
                Arc::new(annotation.body.clone()),
                Vec::new(),
            );
            values.push(self.eval_expr(&expr, scope, depth + 1)?);
        }
        Ok(values)
    }

    /// A property or method of a user's class or module.
    fn member_data(
        &mut self,
        prop: &Property,
        location: Option<Location>,
        scope: &Scope,
        depth: usize,
    ) -> Result<MemberData> {
        let parameters = match &prop.value {
            Some(Expr::Lambda(parameters, _)) if prop.is_method => {
                parameters.iter().map(|name| name.to_string()).collect()
            }
            _ => Vec::new(),
        };
        Ok(MemberData {
            is_method: prop.is_method,
            name: prop.name.to_string(),
            location,
            doc: prop.doc.as_deref().map(str::to_string),
            annotations: self.eval_annotations(&prop.annotations, scope, depth)?,
            modifiers: modifier_names(&prop.modifiers),
            type_parameters: Vec::new(),
            parameters,
        })
    }

    fn reflect_type_alias(
        &mut self,
        alias: &Value,
        scope: &Scope,
        depth: usize,
        seen: &mut Vec<String>,
    ) -> Result<Value> {
        let members = match alias {
            Value::Object(members, Some(source))
                if source.type_identity.as_deref() == Some("pkl:base#TypeAlias") =>
            {
                members
            }
            _ => return Err(super::stdlib::type_mismatch("TypeAlias", alias)),
        };
        let Some(Value::String(name)) = members.get(TYPE_ALIAS_NAME_KEY) else {
            return Err(super::stdlib::type_mismatch("TypeAlias", alias));
        };
        let namespace = match members.get(TYPE_ALIAS_IDENTITY_KEY) {
            Some(Value::String(identity)) => identity
                .strip_suffix(&format!(".{name}"))
                .map(str::to_string),
            _ => None,
        };
        self.alias_mirror(namespace, name, scope, depth, seen)
    }

    /// The mirror of the type alias `name` that the module with type namespace
    /// `namespace` declares. `seen` holds the aliases being mirrored, so an
    /// alias that refers to itself reads as unknown where it recurs.
    fn alias_mirror(
        &mut self,
        namespace: Option<String>,
        name: &str,
        scope: &Scope,
        depth: usize,
        seen: &mut Vec<String>,
    ) -> Result<Value> {
        let key = format!("{}.{name}", namespace.as_deref().unwrap_or_default());
        if seen.contains(&key) {
            return Ok(mirror("UnknownType", ObjectMap::default()));
        }
        seen.push(key);
        // The alias's annotations and target resolve where its module declares
        // it, which is not the caller's scope for an imported alias.
        let declaring_scope = if scope.type_namespace == namespace {
            None
        } else {
            namespace
                .as_deref()
                .and_then(|namespace| self.module_type_scope(namespace))
        };
        let scope = declaring_scope.as_ref().unwrap_or(scope);
        let declarations = namespace
            .as_ref()
            .and_then(|namespace| self.module_declarations.get(namespace))
            .cloned();
        let declaration = declarations
            .as_deref()
            .and_then(|all| find_declaration(all, name, false));
        let mut mirror_members = ObjectMap::default();
        let location = declaration
            .zip(namespace.as_deref())
            .map(|(declaration, namespace)| Location::in_module(namespace, declaration.at));
        mirror_members.insert(
            "location".into(),
            location.as_ref().map_or(Value::Null, Location::value),
        );
        mirror_members.insert(
            "docComment".into(),
            string_or_null(declaration.and_then(|declaration| declaration.doc.as_deref())),
        );
        let annotations = match declaration {
            Some(declaration) => self.eval_annotations(&declaration.annotations, scope, depth)?,
            None => Vec::new(),
        };
        mirror_members.insert(
            "annotations".into(),
            list_of(ListKind::Listing, annotations),
        );
        let modifiers = declaration
            .map(|declaration| modifier_names(&declaration.modifiers))
            .unwrap_or_default();
        mirror_members.insert("modifiers".into(), strings(modifiers));
        mirror_members.insert("name".into(), Value::String(name.into()));
        let parameters: Vec<String> = declaration
            .map(|declaration| declaration.type_parameters.clone())
            .unwrap_or_default();
        mirror_members.insert(
            "typeParameters".into(),
            list_of(
                ListKind::List,
                parameters
                    .iter()
                    .map(|name| {
                        TypeParameter {
                            variance: "",
                            name: name.clone(),
                        }
                        .value()
                    })
                    .collect(),
            ),
        );
        if let Some(target) = declaration.and_then(|declaration| declaration.target.as_ref()) {
            let referent = self.type_mirror(target, &parameters, scope, depth, seen)?;
            mirror_members.insert("referent".into(), referent);
        }
        seen.pop();
        Ok(mirror("TypeAlias", mirror_members))
    }

    /// The `pkl:reflect` mirror of the type `ty` written in `scope`.
    /// `parameters` are the type parameters of the declaration it is part of.
    fn type_mirror(
        &mut self,
        ty: &TypeExpr,
        parameters: &[String],
        scope: &Scope,
        depth: usize,
        seen: &mut Vec<String>,
    ) -> Result<Value> {
        let unknown = || mirror("UnknownType", ObjectMap::default());
        Ok(match ty {
            TypeExpr::Nullable(inner) => {
                let mut members = ObjectMap::default();
                members.insert(
                    "member".into(),
                    self.type_mirror(inner, parameters, scope, depth, seen)?,
                );
                mirror("NullableType", members)
            }
            TypeExpr::Union(alternatives) => {
                let mut members = Vec::with_capacity(alternatives.len());
                for alternative in alternatives {
                    members.push(self.type_mirror(alternative, parameters, scope, depth, seen)?);
                }
                union_type(&list_of(ListKind::List, members))?
            }
            // The constraint is not reflected; the type it constrains is.
            TypeExpr::Constrained(base, _, form) => match form {
                BaseForm::Text => match crate::parser::parse_type_name(base) {
                    Ok(inner) => self.type_mirror(&inner, parameters, scope, depth, seen)?,
                    Err(_) => unknown(),
                },
                _ => self.named_type_mirror(base, &[], parameters, scope, depth, seen)?,
            },
            TypeExpr::Named(name) => {
                self.named_type_mirror(name, &[], parameters, scope, depth, seen)?
            }
            TypeExpr::Generic(name, arguments) => {
                self.named_type_mirror(name, arguments, parameters, scope, depth, seen)?
            }
        })
        .map(|value| {
            // A type this does not model reads as unknown rather than wrong.
            if matches!(value, Value::Null) {
                unknown()
            } else {
                value
            }
        })
    }

    fn named_type_mirror(
        &mut self,
        name: &str,
        arguments: &[TypeExpr],
        parameters: &[String],
        scope: &Scope,
        depth: usize,
        seen: &mut Vec<String>,
    ) -> Result<Value> {
        let unknown = || mirror("UnknownType", ObjectMap::default());
        let name = name.trim_start_matches('*');
        if let Some(literal) = super::types::string_literal_type_value(name) {
            return string_literal_type(&Value::String(literal.into()));
        }
        match name {
            "unknown" => return Ok(unknown()),
            "nothing" => return Ok(mirror("NothingType", ObjectMap::default())),
            "module" => return Ok(mirror("ModuleType", ObjectMap::default())),
            _ => {}
        }
        if parameters.iter().any(|parameter| parameter == name) {
            let parameter = TypeParameter {
                variance: "",
                name: name.to_string(),
            };
            return type_variable(&parameter.value());
        }
        // An alias comes first: `typealias A = C` binds `A` to the class `C`,
        // but the type is the alias.
        // `m.A` names the alias `A` of the module imported as `m`.
        let imported = name
            .split_once('.')
            .filter(|(_, member)| !member.contains('.'))
            .and_then(|(root, member)| {
                let identity = scope.module_identity(root)?.clone();
                let module_scope = self.module_type_scope(&identity)?;
                module_scope.get_type_alias(member)?;
                Some((identity, member))
            });
        let referent = match resolve_dotted(scope, name) {
            _ if imported.is_some() => {
                let (identity, member) = imported.unwrap_or_default();
                Some(self.alias_mirror(Some(identity), member, scope, depth, seen)?)
            }
            _ if scope.get_type_alias(name).is_some() => {
                let namespace = scope.type_namespace.clone();
                Some(self.alias_mirror(namespace, name, scope, depth, seen)?)
            }
            Some(class @ Value::Object(_, Some(_))) => {
                let is_alias = matches!(&class, Value::Object(members, _)
                    if members.contains_key(TYPE_ALIAS_NAME_KEY));
                if is_alias {
                    Some(self.reflect_type_alias(&class, scope, depth, seen)?)
                } else {
                    Some(self.reflect_class(&class, scope, depth)?)
                }
            }
            _ => base_class_data(name).map(|data| data.mirror().value),
        };
        let Some(referent) = referent else {
            return Ok(unknown());
        };
        let count = match &referent {
            Value::Object(members, _) => match members.get("typeParameters") {
                Some(Value::List(list)) => list.len(),
                _ => 0,
            },
            _ => 0,
        };
        let mut types = Vec::with_capacity(count);
        for index in 0..count {
            types.push(match arguments.get(index) {
                Some(argument) => self.type_mirror(argument, parameters, scope, depth, seen)?,
                None => unknown(),
            });
        }
        Ok(declared_type_value(referent, types))
    }
}

/// A class with nothing declared, for one that cannot be found.
fn empty_class(name: &str) -> ClassData {
    ClassData {
        name: name.to_string(),
        location: None,
        doc: None,
        annotations: Vec::new(),
        modifiers: Vec::new(),
        type_parameters: Vec::new(),
        members: Vec::new(),
        extends: None,
        superclass: None,
    }
}

fn unsupported_module() -> Error {
    Error::Eval(
        "reflect.Module() is not supported on this module: pklr does not track its source".into(),
    )
}

/// A `pkl:reflect` mirror of the given type, which names its own class rather
/// than a `pkl:base` one.
fn mirror(type_name: &str, members: ObjectMap) -> Value {
    let Value::Object(members, Some(source)) = typed_object(type_name, members) else {
        unreachable!("typed_object builds an object with a source")
    };
    let mut source = Arc::unwrap_or_clone(source);
    source.type_identity = Some(format!("pkl:reflect#{type_name}"));
    Value::Object(members, Some(Arc::new(source)))
}
