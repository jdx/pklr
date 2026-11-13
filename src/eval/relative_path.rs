//! `Module.relativePathTo(other)`: the descendent directory path from one
//! module to another.

use super::*;

const MESSAGE: &str = "Expected a module as argument, but got an object that amends a module.";

impl Evaluator {
    /// The URI of the module an expression names: `module`, an import's name
    /// or an `import(...)` expression. `None` for any other expression, whose
    /// module pklr does not track.
    fn module_uri_of(&mut self, expr: &Expr, scope: &Scope) -> Option<String> {
        let identity = match expr {
            Expr::Ident(name) if name == "module" => scope.type_namespace.clone()?,
            Expr::Ident(name) => scope.module_identity(name)?.clone(),
            Expr::Import(uri, module_path) => {
                let module_path = Path::new(module_path);
                let resolved = self.resolve_relative(module_path, uri);
                let uri: &str = resolved.as_deref().unwrap_or(uri);
                if uri.starts_with("pkl:") {
                    return Some(uri.to_string());
                }
                if uri.contains("://") && !uri.starts_with("file://") {
                    uri.to_string()
                } else {
                    let path = self.local_file_path(module_path, uri).ok()?;
                    self.module_type_namespace(&path)
                }
            }
            _ => return None,
        };
        Some(
            if identity.contains("://") || identity.starts_with("pkl:") {
                identity
            } else {
                format!("file://{identity}")
            },
        )
    }

    /// `receiver.relativePathTo(other)`, or `None` when either side is not a
    /// module pklr can name.
    pub(super) fn relative_path_to(
        &mut self,
        receiver: &Expr,
        other: &Expr,
        scope: &Scope,
    ) -> Option<Result<Value>> {
        self.module_arg_used |= matches!(other, Expr::Ident(name) if name == "module");
        let from = self.module_uri_of(receiver, scope)?;
        let to = self.module_uri_of(other, scope)?;
        let directory = |uri: &str| {
            uri.rsplit_once('/')
                .map_or(uri.to_string(), |(d, _)| d.to_string())
        };
        let (from_dir, to_dir) = (directory(&from), directory(&to));
        let rest = if to_dir == from_dir {
            Some("")
        } else {
            to_dir
                .strip_prefix(&from_dir)
                .and_then(|rest| rest.strip_prefix('/'))
        };
        Some(match rest {
            Some(rest) => Ok(Value::List(ListValue::from(
                rest.split('/')
                    .filter(|segment| !segment.is_empty())
                    .map(|segment| Value::String(segment.into()))
                    .collect::<Vec<_>>(),
            ))),
            None => Err(Error::Eval(format!(
                "No descendent path exists between modules `{from}` and `{to}`."
            ))),
        })
    }

    /// An amended copy of the module: `module` now names the amending object,
    /// which `relativePathTo` rejects, so reading a member that passes it
    /// fails (an overlay that assigns the member replaces it).
    pub(super) fn poison_module_arguments(
        &self,
        amended: Value,
        overlay: &[Entry],
        scope: &Scope,
    ) -> Value {
        let Some(members) = scope
            .type_namespace
            .as_ref()
            .and_then(|namespace| self.module_arg_members.get(namespace))
        else {
            return amended;
        };
        let Value::Object(map, source) = amended else {
            return amended;
        };
        let mut source = source.map_or_else(
            || ObjectSource {
                kind: ObjectKind::Object,
                ..mapping::mapping_source()
            },
            Arc::unwrap_or_clone,
        );
        let mut poisoned = source
            .poisoned_members
            .as_deref()
            .cloned()
            .unwrap_or_default();
        for (name, rendered) in members {
            let assigned = overlay
                .iter()
                .any(|entry| matches!(entry, Entry::Property(prop) if prop.name == *name));
            if !assigned {
                poisoned.insert(
                    name.clone(),
                    PoisonedMember {
                        message: MESSAGE.into(),
                        rendered: *rendered,
                    },
                );
            }
        }
        source.poisoned_members = Some(Arc::new(poisoned));
        Value::Object(map, Some(Arc::new(source)))
    }
}
