//! The `pkl:reflect` module: mirrors of classes, modules and type aliases,
//! built from the declarations' source.

use super::amend::definition_type_scope;
use super::scope::object_source_type_namespace;
use super::stdlib::{TYPE_ALIAS_NAME_KEY, module_class_name, module_class_namespace, typed_object};
use super::*;

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

fn strings(names: impl IntoIterator<Item = &'static str>) -> Value {
    Value::List(ListValue::new(
        ListKind::Set,
        names
            .into_iter()
            .map(|name| Value::String(name.into()))
            .collect::<Vec<_>>(),
    ))
}

fn modifier_names(modifiers: &[Modifier]) -> Value {
    strings(modifiers.iter().map(|modifier| match modifier {
        Modifier::Local => "local",
        Modifier::Const => "const",
        Modifier::Fixed => "fixed",
        Modifier::Hidden => "hidden",
        Modifier::Abstract => "abstract",
        Modifier::Open => "open",
        Modifier::External => "external",
    }))
}

fn empty_list(kind: ListKind) -> Value {
    Value::List(ListValue::new(kind, Vec::new()))
}

/// What a class mirror is built from.
struct ClassDecl<'a> {
    name: &'a str,
    modifiers: Value,
    entries: &'a [Entry],
}

impl Evaluator {
    /// A call of a `pkl:reflect` function: `Class(c)`, `Module(m)` or
    /// `TypeAlias(a)`. `None` when `method` is not one of them.
    #[inline(never)]
    pub(super) fn reflect_call(
        &mut self,
        method: &str,
        arg_exprs: &[Expr],
        args: &[Value],
        scope: &Scope,
        depth: usize,
    ) -> Option<Result<Value>> {
        if !matches!(method, "Class" | "Module" | "TypeAlias") {
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
            "TypeAlias" => reflect_type_alias(arg),
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
        let name = source.type_name().unwrap_or("Class");
        let mut modifiers = Vec::new();
        if source.is_abstract {
            modifiers.push(Modifier::Abstract);
        }
        if source.is_open {
            modifiers.push(Modifier::Open);
        }
        // A class of the module being evaluated sees all of its declarations,
        // including ones that follow the class.
        let class_scope = if scope.type_namespace == object_source_type_namespace(source) {
            scope.clone()
        } else {
            definition_type_scope(source)
        };
        let decl = ClassDecl {
            name,
            modifiers: modifier_names(&modifiers),
            entries: &source.entries,
        };
        self.class_mirror(&decl, &class_scope, depth)
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
        Ok(typed_object("Module", members))
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
                "pklr:module:Open" => modifiers.push(Modifier::Open),
                "pklr:module:Abstract" => modifiers.push(Modifier::Abstract),
                _ => {}
            }
        }
        let decl = ClassDecl {
            name: &name,
            modifiers: modifier_names(&modifiers),
            entries: &parsed.body,
        };
        let class = self.class_mirror(&decl, scope, depth)?;
        Ok((parsed, class))
    }

    fn class_mirror(&mut self, decl: &ClassDecl, scope: &Scope, depth: usize) -> Result<Value> {
        let mut properties = ObjectMap::default();
        let mut methods = ObjectMap::default();
        for entry in decl.entries {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if prop.modifiers.contains(&Modifier::Local) {
                continue;
            }
            let mirror = self.member_mirror(prop, scope, depth)?;
            if prop.is_method {
                methods.insert(prop.name.clone(), mirror);
            } else {
                properties.insert(prop.name.clone(), mirror);
            }
        }
        let mut members = ObjectMap::default();
        members.insert("name".into(), Value::String(decl.name.into()));
        members.insert("modifiers".into(), decl.modifiers.clone());
        members.insert("annotations".into(), empty_list(ListKind::Listing));
        members.insert("docComment".into(), Value::Null);
        members.insert(
            "properties".into(),
            Value::Object(Arc::new(properties), None),
        );
        members.insert("methods".into(), Value::Object(Arc::new(methods), None));
        Ok(typed_object("Class", members))
    }

    /// The mirror of a property or method.
    fn member_mirror(&mut self, prop: &Property, scope: &Scope, depth: usize) -> Result<Value> {
        let mut annotations = Vec::with_capacity(prop.annotations.len());
        for annotation in &prop.annotations {
            let expr = Expr::New(
                Some(annotation.name.clone()),
                Arc::new(annotation.body.clone()),
                Vec::new(),
            );
            annotations.push(self.eval_expr(&expr, scope, depth + 1)?);
        }
        let mut members = ObjectMap::default();
        members.insert("name".into(), Value::String(prop.name.clone()));
        members.insert("modifiers".into(), modifier_names(&prop.modifiers));
        members.insert(
            "annotations".into(),
            Value::List(ListValue::new(ListKind::Listing, annotations)),
        );
        members.insert(
            "docComment".into(),
            prop.doc
                .as_ref()
                .map_or(Value::Null, |doc| Value::String(doc.clone())),
        );
        Ok(typed_object(
            if prop.is_method { "Method" } else { "Property" },
            members,
        ))
    }
}

fn reflect_type_alias(alias: &Value) -> Result<Value> {
    let name = match alias {
        Value::Object(members, Some(source))
            if source.type_identity.as_deref() == Some("pkl:base#TypeAlias") =>
        {
            members.get(TYPE_ALIAS_NAME_KEY)
        }
        _ => None,
    };
    let Some(Value::String(name)) = name else {
        return Err(super::stdlib::type_mismatch("TypeAlias", alias));
    };
    let mut members = ObjectMap::default();
    members.insert("name".into(), Value::String(name.clone()));
    members.insert("modifiers".into(), strings([]));
    members.insert("annotations".into(), empty_list(ListKind::Listing));
    members.insert("typeParameters".into(), empty_list(ListKind::List));
    members.insert("docComment".into(), Value::Null);
    Ok(typed_object("TypeAlias", members))
}

fn unsupported_module() -> Error {
    Error::Eval(
        "reflect.Module() is not supported on this module: pklr does not track its source".into(),
    )
}
