//! Semantic checks pkl performs after parsing a module.

use std::collections::HashSet;

use super::ast::{Entry, Expr, Modifier, StringInterpPart, TypeExpr};

/// A non-const module member read from a const scope.
pub(super) struct ConstViolation {
    name: String,
    is_method: bool,
}

impl ConstViolation {
    pub(super) fn message(&self) -> String {
        if self.is_method {
            format!(
                "Cannot call method `{}` from here because it is not `const`.",
                self.name
            )
        } else {
            format!(
                "Cannot reference property `{}` from here because it is not `const`.",
                self.name
            )
        }
    }
}

/// Find a module member that a const property or const class member may not
/// read. Parameters, local bindings, generator variables and nested members
/// shadow module members as they do at evaluation time.
pub(super) fn find_const_violation(body: &[Entry]) -> Option<ConstViolation> {
    let non_const: Vec<(&str, bool)> = body
        .iter()
        .filter_map(|entry| match entry {
            Entry::Property(prop) if !has(&prop.modifiers, Modifier::Const) => {
                Some((prop.name.as_str(), prop.is_method))
            }
            _ => None,
        })
        .collect();
    if non_const.is_empty() {
        return None;
    }

    let checker = ConstChecker { non_const };
    for entry in body {
        match entry {
            Entry::Property(prop) if has(&prop.modifiers, Modifier::Const) => {
                if let Some(name) = checker.property(prop, &mut Vec::new()) {
                    return Some(checker.violation(name));
                }
            }
            Entry::Property(prop) => {
                if let Some(name) = checker.nested_const_property(prop, &mut Vec::new()) {
                    return Some(checker.violation(name));
                }
            }
            Entry::ClassDef(name, _, parent, class_body) => {
                // An external parent may define any member, so unqualified
                // reads must be treated as shadowed. Explicit `module.x`
                // reads still bypass this shadow and remain checkable.
                let mut shadow =
                    class_member_names(body, name, parent.as_deref()).unwrap_or_else(|| {
                        checker
                            .non_const
                            .iter()
                            .map(|(member, _)| (*member).to_string())
                            .collect()
                    });
                for member in class_body.iter() {
                    if let Entry::Property(prop) = member {
                        let found = if has(&prop.modifiers, Modifier::Const) {
                            checker.property(prop, &mut shadow)
                        } else {
                            checker.nested_const_property(prop, &mut shadow)
                        };
                        if let Some(found) = found {
                            return Some(checker.violation(found));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn has(modifiers: &[Modifier], modifier: Modifier) -> bool {
    modifiers.contains(&modifier)
}

/// The member names of a class and the in-module classes it extends. An
/// external parent is intentionally unknown and therefore skipped.
fn class_member_names(module: &[Entry], name: &str, parent: Option<&str>) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some((name, parent));
    while let Some((class_name, parent)) = current {
        if !seen.insert(class_name.to_string()) {
            break;
        }
        let body = module.iter().find_map(|entry| match entry {
            Entry::ClassDef(found, _, _, body) if found == class_name => Some(body),
            _ => None,
        })?;
        names.extend(body.iter().filter_map(|entry| match entry {
            Entry::Property(prop) => Some(prop.name.clone()),
            _ => None,
        }));
        current = match parent {
            None => None,
            Some(parent) => {
                let parent_parent = module.iter().find_map(|entry| match entry {
                    Entry::ClassDef(found, _, grandparent, _) if found == parent => {
                        Some(grandparent.as_deref())
                    }
                    _ => None,
                })?;
                Some((parent, parent_parent))
            }
        };
    }
    Some(names)
}

struct ConstChecker<'a> {
    non_const: Vec<(&'a str, bool)>,
}

impl ConstChecker<'_> {
    fn violation(&self, name: String) -> ConstViolation {
        let is_method = self
            .non_const
            .iter()
            .any(|(member, is_method)| *member == name && *is_method);
        ConstViolation { name, is_method }
    }

    fn resolves_to_non_const(&self, name: &str, shadow: &[String]) -> bool {
        !shadow.iter().any(|bound| bound == name)
            && self.non_const.iter().any(|(member, _)| *member == name)
    }

    fn property(&self, prop: &super::ast::Property, shadow: &mut Vec<String>) -> Option<String> {
        prop.type_ann
            .as_ref()
            .and_then(|ty| self.type_expr(ty, shadow))
            .or_else(|| {
                prop.value
                    .as_ref()
                    .and_then(|value| self.expr(value, shadow))
            })
            .or_else(|| {
                prop.body
                    .as_deref()
                    .and_then(|body| self.entries(body, shadow))
            })
    }

    fn type_expr(&self, ty: &TypeExpr, shadow: &mut Vec<String>) -> Option<String> {
        match ty {
            TypeExpr::Named(_) => None,
            TypeExpr::Nullable(inner) => self.type_expr(inner, shadow),
            TypeExpr::Union(variants) => variants
                .iter()
                .find_map(|variant| self.type_expr(variant, shadow)),
            TypeExpr::Generic(_, args) => args.iter().find_map(|arg| self.type_expr(arg, shadow)),
            TypeExpr::Constrained(_, constraint) => self.expr(constraint, shadow),
        }
    }

    /// Inspect a non-const member only for nested members that themselves
    /// introduce a const scope. Ordinary nested values may read module
    /// members, but `const` descendants may not.
    fn nested_const_property(
        &self,
        prop: &super::ast::Property,
        shadow: &mut Vec<String>,
    ) -> Option<String> {
        prop.value
            .as_ref()
            .and_then(|value| self.nested_const_expr(value, shadow))
            .or_else(|| {
                prop.body
                    .as_deref()
                    .and_then(|body| self.nested_const_entries(body, shadow))
            })
    }

    fn nested_const_expr(&self, expr: &Expr, shadow: &mut Vec<String>) -> Option<String> {
        match expr {
            Expr::Field(base, _)
            | Expr::NullSafeField(base, _)
            | Expr::Unop(_, base)
            | Expr::Throw(base)
            | Expr::Trace(base, _)
            | Expr::Read(base, _)
            | Expr::ReadOrNull(base, _)
            | Expr::ReadGlob(base, _) => self.nested_const_expr(base, shadow),
            Expr::Index(left, right) | Expr::Binop(_, left, right) => self
                .nested_const_expr(left, shadow)
                .or_else(|| self.nested_const_expr(right, shadow)),
            Expr::Call(callee, args) => self.nested_const_expr(callee, shadow).or_else(|| {
                args.iter()
                    .find_map(|arg| self.nested_const_expr(arg, shadow))
            }),
            Expr::If(condition, then_expr, else_expr) => self
                .nested_const_expr(condition, shadow)
                .or_else(|| self.nested_const_expr(then_expr, shadow))
                .or_else(|| self.nested_const_expr(else_expr, shadow)),
            Expr::Let(name, value, body) => self.nested_const_expr(value, shadow).or_else(|| {
                self.bound(shadow, std::slice::from_ref(name), |shadow| {
                    self.nested_const_expr(body, shadow)
                })
            }),
            Expr::Is(value, _) | Expr::As(value, _) => self.nested_const_expr(value, shadow),
            Expr::Lambda(params, body) => self.bound(shadow, params, |shadow| {
                self.nested_const_expr(body, shadow)
            }),
            Expr::StringInterpolation(parts) => parts.iter().find_map(|part| match part {
                StringInterpPart::Expr(value) => self.nested_const_expr(value, shadow),
                StringInterpPart::Literal(_) => None,
            }),
            Expr::New(_, body, _) | Expr::InferredNew(_, body) | Expr::ObjectBody(body) => {
                self.nested_const_entries(body, shadow)
            }
            Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Ident(_)
            | Expr::Import(_, _)
            | Expr::ImportGlob(_, _) => None,
        }
    }

    fn nested_const_entries(&self, entries: &[Entry], shadow: &mut Vec<String>) -> Option<String> {
        let names: Vec<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some(prop.name.clone()),
                _ => None,
            })
            .collect();
        self.bound(shadow, &names, |shadow| {
            entries.iter().find_map(|entry| match entry {
                Entry::Property(prop) if has(&prop.modifiers, Modifier::Const) => {
                    self.property(prop, shadow)
                }
                Entry::Property(prop) => self.nested_const_property(prop, shadow),
                Entry::DynProperty(key, value) | Entry::Predicate(key, value) => self
                    .nested_const_expr(key, shadow)
                    .or_else(|| self.nested_const_expr(value, shadow)),
                Entry::Spread(value) | Entry::Elem(value) => self.nested_const_expr(value, shadow),
                Entry::ForGenerator(generator) => self
                    .nested_const_expr(&generator.collection, shadow)
                    .or_else(|| {
                        let mut names = Vec::new();
                        if let Some(key) = &generator.key_var {
                            names.push(key.clone());
                        }
                        names.push(generator.val_var.clone());
                        self.bound(shadow, &names, |shadow| {
                            self.nested_const_entries(&generator.body, shadow)
                        })
                    }),
                Entry::WhenGenerator(generator) => self
                    .nested_const_expr(&generator.condition, shadow)
                    .or_else(|| self.nested_const_entries(&generator.body, shadow))
                    .or_else(|| {
                        generator
                            .else_body
                            .as_deref()
                            .and_then(|body| self.nested_const_entries(body, shadow))
                    }),
                Entry::ClassDef(_, _, _, body) => self.nested_const_entries(body, shadow),
                Entry::TypeAlias(_, _) => None,
            })
        })
    }

    fn expr(&self, expr: &Expr, shadow: &mut Vec<String>) -> Option<String> {
        match expr {
            Expr::Ident(name) if self.resolves_to_non_const(name, shadow) => Some(name.clone()),
            Expr::Ident(_) => None,
            Expr::Field(base, name) | Expr::NullSafeField(base, name)
                if matches!(base.as_ref(), Expr::Ident(receiver) if receiver == "module")
                    && self.non_const.iter().any(|(member, _)| *member == name) =>
            {
                Some(name.clone())
            }
            Expr::Field(base, _) | Expr::NullSafeField(base, _) => self.expr(base, shadow),
            Expr::Index(left, right) | Expr::Binop(_, left, right) => {
                self.expr(left, shadow).or_else(|| self.expr(right, shadow))
            }
            Expr::Call(callee, args) => self
                .expr(callee, shadow)
                .or_else(|| args.iter().find_map(|arg| self.expr(arg, shadow))),
            Expr::If(condition, then_expr, else_expr) => self
                .expr(condition, shadow)
                .or_else(|| self.expr(then_expr, shadow))
                .or_else(|| self.expr(else_expr, shadow)),
            Expr::Let(name, value, body) => self.expr(value, shadow).or_else(|| {
                self.bound(shadow, std::slice::from_ref(name), |shadow| {
                    self.expr(body, shadow)
                })
            }),
            Expr::Is(value, ty) | Expr::As(value, ty) => self
                .expr(value, shadow)
                .or_else(|| self.type_expr(ty, shadow)),
            Expr::Unop(_, value)
            | Expr::Throw(value)
            | Expr::Trace(value, _)
            | Expr::Read(value, _)
            | Expr::ReadOrNull(value, _)
            | Expr::ReadGlob(value, _) => self.expr(value, shadow),
            Expr::Lambda(params, body) => {
                self.bound(shadow, params, |shadow| self.expr(body, shadow))
            }
            Expr::StringInterpolation(parts) => parts.iter().find_map(|part| match part {
                StringInterpPart::Expr(value) => self.expr(value, shadow),
                StringInterpPart::Literal(_) => None,
            }),
            Expr::New(_, body, _) | Expr::InferredNew(_, body) | Expr::ObjectBody(body) => {
                self.entries(body, shadow)
            }
            Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Import(_, _)
            | Expr::ImportGlob(_, _) => None,
        }
    }

    fn entries(&self, entries: &[Entry], shadow: &mut Vec<String>) -> Option<String> {
        let names: Vec<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Property(prop) => Some(prop.name.clone()),
                _ => None,
            })
            .collect();
        self.bound(shadow, &names, |shadow| {
            entries.iter().find_map(|entry| match entry {
                Entry::Property(prop) => self.property(prop, shadow),
                Entry::DynProperty(key, value) | Entry::Predicate(key, value) => {
                    self.expr(key, shadow).or_else(|| self.expr(value, shadow))
                }
                Entry::Spread(value) | Entry::Elem(value) => self.expr(value, shadow),
                Entry::ForGenerator(generator) => {
                    self.expr(&generator.collection, shadow).or_else(|| {
                        let mut names = Vec::new();
                        if let Some(key) = &generator.key_var {
                            names.push(key.clone());
                        }
                        names.push(generator.val_var.clone());
                        self.bound(shadow, &names, |shadow| {
                            self.entries(&generator.body, shadow)
                        })
                    })
                }
                Entry::WhenGenerator(generator) => self
                    .expr(&generator.condition, shadow)
                    .or_else(|| self.entries(&generator.body, shadow))
                    .or_else(|| {
                        generator
                            .else_body
                            .as_deref()
                            .and_then(|body| self.entries(body, shadow))
                    }),
                Entry::ClassDef(_, _, _, body) => self.entries(body, shadow),
                Entry::TypeAlias(_, _) => None,
            })
        })
    }

    fn bound<T>(
        &self,
        shadow: &mut Vec<String>,
        names: &[String],
        f: impl FnOnce(&mut Vec<String>) -> T,
    ) -> T {
        let len = shadow.len();
        shadow.extend(names.iter().cloned());
        let result = f(shadow);
        shadow.truncate(len);
        result
    }
}
