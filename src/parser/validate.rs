//! Semantic checks pkl performs after parsing a module.
//!
//! The const check mirrors pkl's lexical rule: a class body, a type alias, an
//! annotation body and every `const` member are "const scopes". Inside one, a
//! name that resolves to a member declared outside the scope (a member of the
//! module, of the enclosing class, or of an enclosing object) must be `const`,
//! and `this`, `outer` and `module` may not be used to reach such members
//! either. Names that resolve inside the scope (parameters, `let` and `for`
//! variables, members of objects built within it) are free. Pkl reports the
//! violations of module and class members when the member runs, so a name that
//! only an unexecuted branch reads is accepted there; this check is static and
//! treats every read as executed, except in method bodies, which it reports
//! only when the module calls a method of that name.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::ast::{Annotation, Entry, Expr, Modifier, Property, StringInterpPart, TypeExpr};

macro_rules! bail {
    ($e:expr) => {
        if let Some(found) = $e {
            return Some(found);
        }
    };
}

/// A use of a non-const member, `this`, `outer` or `module` from a const scope.
pub(super) struct ConstViolation(String);

impl ConstViolation {
    pub(super) fn message(&self) -> String {
        self.0.clone()
    }
}

/// Find the first const-scope violation in a module body. `open` says the
/// module amends or extends another, so it may have members the body does not
/// declare.
pub(super) fn find_const_violation(body: &[Entry], open: bool) -> Option<ConstViolation> {
    let mut walker = Walker::new(body);
    walker.frames.push(Frame::object(members_of(body), open));
    walker.module(body)
}

fn has(modifiers: &[Modifier], modifier: Modifier) -> bool {
    modifiers.contains(&modifier)
}

const STRING_MEMBERS: &[&str] = &[
    "length",
    "isEmpty",
    "isBlank",
    "isRegex",
    "startsWith",
    "endsWith",
    "contains",
    "matches",
    "trim",
    "toInt",
    "toFloat",
    "toString",
    "isLowerCase",
    "isUpperCase",
];
const NUMBER_MEMBERS: &[&str] = &[
    "isNaN",
    "isFinite",
    "isInfinite",
    "isPositive",
    "isNonZero",
    "isBetween",
    "sign",
    "abs",
    "floor",
    "ceil",
    "round",
    "truncate",
    "toInt",
    "toFloat",
    "toString",
    "toDuration",
    "toDataSize",
];
const COLLECTION_MEMBERS: &[&str] = &[
    "length",
    "isEmpty",
    "isUnique",
    "isUniqueBy",
    "first",
    "last",
    "contains",
    "keys",
    "values",
    "size",
    "containsKey",
    "containsValue",
    "any",
    "every",
    "count",
    "toList",
    "toSet",
    "toMap",
    "fold",
    "map",
    "filter",
];
const OBJECT_MEMBERS: &[&str] = &["hasProperty", "getProperty", "toMap"];
const UNIT_MEMBERS: &[&str] = &["unit", "value", "isBetween", "isPositive", "isNonZero"];
const OTHER_MEMBERS: &[&str] = &[
    "isLeapYear",
    "year",
    "month",
    "day",
    "hour",
    "minute",
    "second",
    "path",
    "scheme",
    "host",
];

/// Whether a bare identifier in a constraint on `base` may take `name` from the
/// constrained value (`String(length > 3)`), so it is not a read of an outer
/// member. A base that is not a standard library type may be an alias of any.
fn value_member(base: &str, name: &str) -> bool {
    let groups: &[&[&str]] = match base.trim_end_matches('?') {
        "String" => &[STRING_MEMBERS],
        "Int" | "Float" | "Number" | "UInt" | "Int8" | "Int16" | "Int32" | "UInt8" | "UInt16"
        | "UInt32" => &[NUMBER_MEMBERS],
        "List" | "Listing" | "Set" | "Map" | "Mapping" | "Collection" => &[COLLECTION_MEMBERS],
        "Dynamic" | "Typed" | "Object" => &[OBJECT_MEMBERS],
        "Duration" | "DataSize" => &[UNIT_MEMBERS],
        _ => &[
            STRING_MEMBERS,
            NUMBER_MEMBERS,
            COLLECTION_MEMBERS,
            OBJECT_MEMBERS,
            UNIT_MEMBERS,
            OTHER_MEMBERS,
        ],
    };
    groups.iter().any(|group| group.contains(&name))
}

/// Members by name: `(is_const, is_local)`.
#[derive(Default)]
struct Members<'m> {
    props: HashMap<&'m str, (bool, bool)>,
    methods: HashMap<&'m str, (bool, bool)>,
}

impl Members<'_> {
    fn get(&self, name: &str, method: bool) -> Option<(bool, bool)> {
        if method {
            self.methods.get(name).copied()
        } else {
            self.props.get(name).copied()
        }
    }
}

fn members_of(entries: &[Entry]) -> Members<'_> {
    let mut members = Members::default();
    collect_members(entries, &mut members);
    members
}

fn collect_members<'m>(entries: &'m [Entry], members: &mut Members<'m>) {
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                let flags = (
                    has(&prop.modifiers, Modifier::Const),
                    has(&prop.modifiers, Modifier::Local),
                );
                let map = if prop.is_method {
                    &mut members.methods
                } else {
                    &mut members.props
                };
                map.insert(&*prop.name, flags);
            }
            Entry::ForGenerator(generator) => collect_members(&generator.body, members),
            Entry::WhenGenerator(generator) => {
                collect_members(&generator.body, members);
                if let Some(else_body) = &generator.else_body {
                    collect_members(else_body, members);
                }
            }
            _ => {}
        }
    }
}

/// A class's members and those it inherits from classes of this module.
struct ClassInfo<'m> {
    own: Members<'m>,
    parent: Option<Rc<ClassInfo<'m>>>,
    /// Whether an ancestor is not a class of this module, so the class may have
    /// members this module does not declare.
    open: bool,
}

impl ClassInfo<'_> {
    /// Look a member up as an inheriting class or an instance sees it: a
    /// parent's `local` members are not visible.
    fn find_inherited(&self, name: &str, method: bool) -> Option<bool> {
        match self.own.get(name, method) {
            Some((is_const, false)) => Some(is_const),
            _ => self
                .parent
                .as_ref()
                .and_then(|parent| parent.find_inherited(name, method)),
        }
    }
}

/// What has to use a deferred member for its violation to count.
#[derive(Clone, Copy)]
enum Pending {
    /// A call of the method.
    Method,
    /// A read of the property.
    Property,
    /// A read of the const local lambda that is also applied.
    Lambda,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// The module, a class or an object body: `this` and `outer` count them.
    Object,
    Class,
    /// Variables bound by a lambda, `let` or `for`.
    Vars,
    /// A type constraint, where `this` is the constrained value.
    Constraint,
}

struct Frame<'m> {
    kind: Kind,
    own: Rc<Members<'m>>,
    /// The class an object is an instance of, or a class's parent.
    base: Option<Rc<ClassInfo<'m>>>,
    /// Whether the frame may have members beyond the ones listed.
    open: bool,
}

enum Hit {
    Found(bool),
    Open,
    Miss,
}

impl<'m> Frame<'m> {
    fn object(own: Members<'m>, open: bool) -> Self {
        Frame {
            kind: Kind::Object,
            own: Rc::new(own),
            base: None,
            open,
        }
    }

    fn vars(names: impl IntoIterator<Item = &'m str>) -> Self {
        let mut own = Members::default();
        own.props
            .extend(names.into_iter().map(|name| (name, (true, true))));
        Frame {
            kind: Kind::Vars,
            own: Rc::new(own),
            base: None,
            open: false,
        }
    }

    fn lookup(&self, name: &str, method: bool) -> Hit {
        if let Some((is_const, _)) = self.own.get(name, method) {
            return Hit::Found(is_const);
        }
        if let Some(base) = &self.base
            && let Some(is_const) = base.find_inherited(name, method)
        {
            return Hit::Found(is_const);
        }
        if self.open || self.base.as_ref().is_some_and(|base| base.open) {
            Hit::Open
        } else {
            Hit::Miss
        }
    }
}

struct ClassDecl<'m> {
    parent: Option<&'m str>,
    body: &'m [Entry],
}

struct Walker<'m> {
    frames: Vec<Frame<'m>>,
    /// The innermost frame whose members a const scope may not read unless
    /// they are const; frames at or below it are outside the scope.
    boundary: Option<usize>,
    /// Names of the members being walked, innermost last.
    members: Vec<&'m str>,
    in_annotation: bool,
    /// Inside the body of a module-level `const` method.
    module_method: bool,
    /// The next lambda is the method's own, not one inside its body.
    method_lambda: bool,
    classes: HashMap<&'m str, ClassDecl<'m>>,
    infos: HashMap<&'m str, Option<Rc<ClassInfo<'m>>>>,
    /// Violations in method bodies, which count only if the method is called.
    pending: Vec<(&'m str, Pending, ConstViolation)>,
    /// The base types of the constraints being walked, innermost last.
    constraint_bases: Vec<&'m str>,
}

fn violation(message: impl Into<String>) -> Option<ConstViolation> {
    Some(ConstViolation(message.into()))
}

fn property_message(name: &str, method: bool) -> String {
    if method {
        format!("Cannot call method `{name}` from here because it is not `const`.")
    } else {
        format!("Cannot reference property `{name}` from here because it is not `const`.")
    }
}

impl<'m> Walker<'m> {
    fn new(body: &'m [Entry]) -> Self {
        let mut classes = HashMap::new();
        for entry in body {
            if let Entry::ClassDef(name, _, parent, class_body) = entry {
                classes.insert(
                    name.as_str(),
                    ClassDecl {
                        parent: parent.as_deref(),
                        body: class_body,
                    },
                );
            }
        }
        Walker {
            frames: Vec::new(),
            boundary: None,
            members: Vec::new(),
            in_annotation: false,
            module_method: false,
            method_lambda: false,
            classes,
            infos: HashMap::new(),
            pending: Vec::new(),
            constraint_bases: Vec::new(),
        }
    }

    fn module(&mut self, body: &'m [Entry]) -> Option<ConstViolation> {
        for entry in body {
            match entry {
                Entry::Property(prop) => bail!(self.property(prop, 0)),
                Entry::ClassDef(name, _, parent, class_body) => {
                    bail!(self.class(name, parent.as_deref(), class_body));
                }
                Entry::TypeAlias(name, ty) => {
                    self.members.push(name);
                    let found = self.scoped(0, |walker| walker.type_expr(ty));
                    self.members.pop();
                    bail!(found);
                }
                _ => {}
            }
        }
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return None;
        }
        let mut called: HashSet<(&str, u8)> = HashSet::new();
        collect_calls_in_entries(body, &mut called);
        pending
            .into_iter()
            .find(|(name, kind, _)| match kind {
                Pending::Method => called.contains(&(*name, CALL)),
                Pending::Property => called.contains(&(*name, READ)),
                Pending::Lambda => called.contains(&(*name, APPLY)),
            })
            .map(|(_, _, violation)| violation)
    }

    /// Run `f` inside a const scope whose outside is `frames[owner]` and below.
    fn scoped<T>(&mut self, owner: usize, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.boundary.replace(owner);
        let result = f(self);
        self.boundary = saved;
        result
    }

    fn class_info(&mut self, name: &'m str) -> Option<Rc<ClassInfo<'m>>> {
        if let Some(info) = self.infos.get(name) {
            return info.clone();
        }
        if crate::stack::exhausted() {
            crate::stack::limit_hit();
            return None;
        }
        // A class that extends itself, even through others, has no usable info.
        self.infos.insert(name, None);
        let (parent, body) = {
            let decl = self.classes.get(name)?;
            (decl.parent, decl.body)
        };
        let info = self.build_info(name, parent, body);
        self.infos.insert(name, Some(info.clone()));
        Some(info)
    }

    fn build_info(
        &mut self,
        name: &str,
        parent: Option<&'m str>,
        body: &'m [Entry],
    ) -> Rc<ClassInfo<'m>> {
        let (parent_info, open) = match parent {
            None => (None, false),
            Some(parent) if parent == name => (None, true),
            Some(parent) => match self.class_info(parent) {
                Some(info) => {
                    let open = info.open;
                    (Some(info), open)
                }
                None => (None, !matches!(parent, "Typed" | "Annotation")),
            },
        };
        Rc::new(ClassInfo {
            own: members_of(body),
            parent: parent_info,
            open,
        })
    }

    fn class(
        &mut self,
        name: &'m str,
        parent: Option<&'m str>,
        body: &'m [Entry],
    ) -> Option<ConstViolation> {
        let info = match self.classes.get(name) {
            Some(decl) if std::ptr::eq(decl.body, body) => self.class_info(name),
            _ => None,
        }
        .unwrap_or_else(|| self.build_info(name, parent, body));
        let enclosing = self.frames.len() - 1;
        let owner = self.frames.len();
        self.frames.push(Frame {
            kind: Kind::Class,
            own: Rc::new(members_of(body)),
            base: info.parent.clone(),
            open: info.open,
        });
        let result = self.scoped(enclosing, |walker| {
            body.iter()
                .try_for_each(|member| match walker.entry(member, owner) {
                    Some(found) => Err(found),
                    None => Ok(()),
                })
        });
        self.frames.pop();
        result.err()
    }

    fn annotation(&mut self, annotation: &'m Annotation, owner: usize) -> Option<ConstViolation> {
        let class = self
            .classes
            .contains_key(annotation.name.as_str())
            .then(|| self.class_info(&annotation.name))
            .flatten();
        let (base, open) = match class {
            Some(info) => {
                let open = info.open;
                (Some(info), open)
            }
            None => (None, annotation.name != "Annotation"),
        };
        self.frames.push(Frame {
            kind: Kind::Object,
            own: Rc::new(members_of(&annotation.body)),
            base,
            open,
        });
        let saved = std::mem::replace(&mut self.in_annotation, true);
        let result = self.scoped(owner, |walker| walker.entries(&annotation.body, owner + 1));
        self.in_annotation = saved;
        self.frames.pop();
        result
    }

    fn entries(&mut self, entries: &'m [Entry], owner: usize) -> Option<ConstViolation> {
        entries.iter().find_map(|entry| self.entry(entry, owner))
    }

    fn entry(&mut self, entry: &'m Entry, owner: usize) -> Option<ConstViolation> {
        match entry {
            Entry::Property(prop) => self.property(prop, owner),
            Entry::DynProperty(key, value) | Entry::Predicate(key, value) => {
                self.expr(key).or_else(|| self.expr(value))
            }
            Entry::Spread(value) | Entry::Elem(value) => self.expr(value),
            Entry::ForGenerator(generator) => self.expr(&generator.collection).or_else(|| {
                let names = generator
                    .key_var
                    .iter()
                    .map(String::as_str)
                    .chain([generator.val_var.as_str()]);
                self.frames.push(Frame::vars(names));
                let result = self.entries(&generator.body, owner);
                self.frames.pop();
                result
            }),
            Entry::WhenGenerator(generator) => self
                .expr(&generator.condition)
                .or_else(|| self.entries(&generator.body, owner))
                .or_else(|| {
                    generator
                        .else_body
                        .as_deref()
                        .and_then(|body| self.entries(body, owner))
                }),
            Entry::ClassDef(name, _, parent, body) => self.class(name, parent.as_deref(), body),
            Entry::TypeAlias(name, ty) => {
                let outer = self.frames.len() - 1;
                self.members.push(name);
                let found = self.scoped(outer, |walker| walker.type_expr(ty));
                self.members.pop();
                found
            }
        }
    }

    /// Walk a member of `frames[owner]`.
    fn property(&mut self, prop: &'m Property, owner: usize) -> Option<ConstViolation> {
        // pkl checks a module-level `const` method body only inside lambdas and
        // in `module.name` reads.
        let module_method = prop.is_method && owner == 0 && has(&prop.modifiers, Modifier::Const);
        let saved_method = std::mem::replace(&mut self.module_method, module_method);
        let saved_lambda = std::mem::replace(&mut self.method_lambda, module_method);
        let is_const = has(&prop.modifiers, Modifier::Const)
            && !matches!(self.frames[owner].kind, Kind::Vars | Kind::Constraint)
            && !module_method;
        let saved = self.boundary;
        if is_const {
            self.boundary = Some(owner);
        }
        self.members.push(&prop.name);
        let result = (|| {
            for annotation in &prop.annotations {
                bail!(self.annotation(annotation, owner));
            }
            // A method, or a `const local` property of a class or object, is
            // read only when something uses it, so its violation counts only
            // if the module mentions its name.
            let deferred =
                prop.is_method || (is_const && owner != 0 && has(&prop.modifiers, Modifier::Local));
            if deferred {
                let kind = match (prop.is_method, &prop.value) {
                    (true, _) => Pending::Method,
                    (false, Some(Expr::Lambda(..))) => Pending::Lambda,
                    _ => Pending::Property,
                };
                // The evaluator guards a const local lambda's reads of its own
                // object when it is applied, so only reads of what lies
                // outside that object are checked here.
                let inner = self.boundary;
                if matches!(kind, Pending::Lambda) {
                    self.boundary = Some(owner - 1);
                }
                let found = self.member_value(prop, owner);
                self.boundary = inner;
                if let Some(found) = found {
                    self.pending.push((&prop.name, kind, found));
                }
                return None;
            }
            self.member_value(prop, owner)
        })();
        self.members.pop();
        self.boundary = saved;
        self.module_method = saved_method;
        self.method_lambda = saved_lambda;
        result
    }

    fn member_value(&mut self, prop: &'m Property, owner: usize) -> Option<ConstViolation> {
        if let Some(ty) = &prop.type_ann {
            bail!(self.type_expr(ty));
        }
        if let Some(value) = &prop.value {
            match value {
                Expr::New(None, body, _) => return self.new_object(None, body, prop, owner),
                _ => bail!(self.expr(value)),
            }
        }
        if let Some(body) = &prop.body {
            return self.object(
                Frame::object(members_of(body), !self.infers_dynamic(prop, owner)),
                body,
            );
        }
        None
    }

    /// Whether an untyped object body assigned to `prop` builds a `Dynamic`:
    /// nothing declares the member's type, so no class contributes members.
    fn infers_dynamic(&self, prop: &Property, owner: usize) -> bool {
        let frame = &self.frames[owner];
        prop.type_ann.is_none()
            && !frame.open
            && frame
                .base
                .as_ref()
                .is_none_or(|base| !base.open && base.find_inherited(&prop.name, false).is_none())
    }

    fn new_object(
        &mut self,
        ty: Option<&'m str>,
        body: &'m [Entry],
        prop: &'m Property,
        owner: usize,
    ) -> Option<ConstViolation> {
        let frame = match ty {
            Some(name) => self.typed_frame(name, body),
            None => Frame::object(members_of(body), !self.infers_dynamic(prop, owner)),
        };
        self.object(frame, body)
    }

    fn typed_frame(&mut self, name: &str, body: &'m [Entry]) -> Frame<'m> {
        let own = members_of(body);
        if name == "Dynamic" {
            return Frame::object(own, false);
        }
        let class = self
            .classes
            .get_key_value(name)
            .map(|(name, _)| *name)
            .and_then(|name| self.class_info(name));
        match class {
            Some(info) => Frame {
                kind: Kind::Object,
                own: Rc::new(own),
                open: info.open,
                base: Some(info),
            },
            None => Frame::object(own, true),
        }
    }

    fn object(&mut self, frame: Frame<'m>, body: &'m [Entry]) -> Option<ConstViolation> {
        let owner = self.frames.len();
        self.frames.push(frame);
        let result = self.entries(body, owner);
        self.frames.pop();
        result
    }

    fn type_expr(&mut self, ty: &'m TypeExpr) -> Option<ConstViolation> {
        match ty {
            TypeExpr::Named(_) => None,
            TypeExpr::Nullable(inner) => self.type_expr(inner),
            TypeExpr::Union(variants) => {
                variants.iter().find_map(|variant| self.type_expr(variant))
            }
            TypeExpr::Generic(_, args) => args.iter().find_map(|arg| self.type_expr(arg)),
            TypeExpr::Constrained(base, constraint, _) => {
                self.constraint_bases.push(base);
                self.frames.push(Frame {
                    kind: Kind::Constraint,
                    own: Rc::new(Members::default()),
                    base: None,
                    open: false,
                });
                let result = self.expr(constraint);
                self.frames.pop();
                self.constraint_bases.pop();
                result
            }
        }
    }

    fn expr(&mut self, expr: &'m Expr) -> Option<ConstViolation> {
        if crate::stack::exhausted() {
            crate::stack::limit_hit();
            return None;
        }
        match expr {
            Expr::Ident(name) => self.ident(name),
            Expr::Field(base, name) | Expr::NullSafeField(base, name) => {
                self.access(base, name, false)
            }
            Expr::Call(callee, args) => {
                let callee_found = match callee.as_ref() {
                    Expr::Ident(name) => self.call_name(name),
                    Expr::Field(base, name) | Expr::NullSafeField(base, name) => {
                        self.access(base, name, true)
                    }
                    other => self.expr(other),
                };
                callee_found.or_else(|| args.iter().find_map(|arg| self.expr(arg)))
            }
            Expr::Index(left, right) | Expr::Binop(_, left, right) => {
                self.expr(left).or_else(|| self.expr(right))
            }
            Expr::If(condition, then_expr, else_expr) => self
                .expr(condition)
                .or_else(|| self.expr(then_expr))
                .or_else(|| self.expr(else_expr)),
            Expr::Let(name, value, body) => self.expr(value).or_else(|| {
                self.frames.push(Frame::vars([name.as_str()]));
                let result = self.expr(body);
                self.frames.pop();
                result
            }),
            Expr::Is(value, ty) | Expr::As(value, ty) => {
                self.expr(value).or_else(|| self.type_expr(ty))
            }
            Expr::Checked(value, ty) => self.expr(value).or_else(|| self.type_expr(ty)),
            Expr::Unop(_, value)
            | Expr::Throw(value)
            | Expr::TemplateLocals(_, value)
            | Expr::Trace(value, _)
            | Expr::Constraint(value, _)
            | Expr::Read(value, _)
            | Expr::ReadOrNull(value, _)
            | Expr::ReadGlob(value, _) => self.expr(value),
            Expr::Lambda(params, body) => {
                self.frames
                    .push(Frame::vars(params.iter().map(String::as_str)));
                let saved = self.boundary;
                let own = std::mem::take(&mut self.method_lambda);
                if self.module_method && !own && saved.is_none() {
                    self.boundary = Some(0);
                }
                let result = self.expr(body);
                self.boundary = saved;
                self.frames.pop();
                result
            }
            Expr::StringInterpolation(parts) => parts.iter().find_map(|part| match part {
                StringInterpPart::Expr(value) => self.expr(value),
                StringInterpPart::Literal(_) => None,
            }),
            Expr::New(ty, body, _) => {
                let frame = match ty {
                    Some(name) => self.typed_frame(name, body),
                    None => Frame::object(members_of(body), true),
                };
                self.object(frame, body)
            }
            Expr::InferredNew(ty, body) => {
                let frame = match inferred_class(ty) {
                    Some(name) => self.typed_frame(name, body),
                    None => Frame::object(members_of(body), true),
                };
                self.object(frame, body)
            }
            Expr::ObjectBody(body) => self.object(Frame::object(members_of(body), true), body),
            Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Import(_, _)
            | Expr::ImportGlob(_, _) => None,
        }
    }

    /// The frames `this` and `outer` name: the innermost two object-like frames.
    /// `None` for `this` means the constrained value of a type constraint.
    fn receivers(&self) -> (Option<usize>, Option<usize>) {
        let mut found = self
            .frames
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, frame)| frame.kind != Kind::Vars);
        let this = found.next();
        if this.is_some_and(|(_, frame)| frame.kind == Kind::Constraint) {
            // `this` is the checked value, and `outer` is the object around the
            // one the constraint is written in.
            let mut lexical = found.filter(|(_, frame)| frame.kind != Kind::Constraint);
            lexical.next();
            return (None, lexical.next().map(|(index, _)| index));
        }
        (
            this.map(|(index, _)| index),
            found.next().map(|(index, _)| index),
        )
    }

    /// Whether a variable of this name shadows `this`, `outer` or `module`.
    fn shadowed(&self, name: &str) -> bool {
        self.frames
            .iter()
            .any(|frame| frame.kind == Kind::Vars && frame.own.props.contains_key(name))
    }

    fn outside(&self, frame: usize) -> bool {
        self.boundary.is_some_and(|boundary| frame <= boundary)
    }

    fn ident(&mut self, name: &str) -> Option<ConstViolation> {
        match name {
            "this" | "outer" if self.shadowed(name) => None,
            "this" => {
                let (this, _) = self.receivers();
                if this.is_some_and(|this| self.outside(this)) {
                    return violation(
                        "Cannot reference `this` from here because it is not `const`.",
                    );
                }
                None
            }
            "outer" => {
                let (_, outer) = self.receivers();
                if outer.is_some_and(|outer| self.outside(outer)) {
                    return violation(
                        "Cannot reference `outer` from here because it's not `const`.",
                    );
                }
                None
            }
            "module" => {
                if self.boundary.is_none() || self.shadowed(name) {
                    return None;
                }
                let in_class = self.frames.iter().any(|frame| frame.kind == Kind::Class);
                violation(if in_class {
                    "Cannot reference `module` within a class body.".to_string()
                } else if self.in_annotation {
                    "Cannot reference `module` within an annotation body.".to_string()
                } else {
                    format!(
                        "Cannot reference `module` from const property `{}`.",
                        self.members.last().copied().unwrap_or_default()
                    )
                })
            }
            "super" => None,
            _ => self.resolve(name, false),
        }
    }

    fn call_name(&mut self, name: &str) -> Option<ConstViolation> {
        self.resolve(name, true)
    }

    /// Resolve a name the way the nearest enclosing scope does, and check a hit
    /// on a member outside the const scope.
    fn resolve(&self, name: &str, method: bool) -> Option<ConstViolation> {
        let mut constraints = self.constraint_bases.iter().rev();
        for (index, frame) in self.frames.iter().enumerate().rev() {
            match frame.kind {
                Kind::Vars => {
                    if frame.own.props.contains_key(name) {
                        return None;
                    }
                }
                Kind::Constraint => {
                    let base = constraints.next().copied().unwrap_or_default();
                    if !method
                        && !self.classes.contains_key(user_class(base))
                        && value_member(base, name)
                    {
                        return None;
                    }
                }
                Kind::Object | Kind::Class => match frame.lookup(name, method) {
                    Hit::Found(is_const) => {
                        return (!is_const && self.outside(index))
                            .then(|| ConstViolation(property_message(name, method)));
                    }
                    Hit::Open => return None,
                    Hit::Miss => {}
                },
            }
        }
        None
    }

    /// `base.name` or `base.name(...)`.
    fn access(&mut self, base: &'m Expr, name: &str, method: bool) -> Option<ConstViolation> {
        let Expr::Ident(receiver) = base else {
            return self.expr(base);
        };
        if self.shadowed(receiver) {
            return None;
        }
        let target = match receiver.as_str() {
            "module" => Some(0),
            "this" => self.receivers().0,
            "outer" => self.receivers().1,
            "super" => {
                let (this, _) = self.receivers();
                let frame = this.filter(|&this| self.frames[this].kind == Kind::Class)?;
                let found = self.frames[frame]
                    .base
                    .as_ref()
                    .and_then(|base| base.find_inherited(name, method));
                return (found == Some(false) && self.outside(frame))
                    .then(|| ConstViolation(property_message(name, method)));
            }
            _ => return self.expr(base),
        };
        let index = target?;
        let strict_module = self.module_method && receiver == "module";
        if let Hit::Found(false) = self.frames[index].lookup(name, method)
            && (self.outside(index) || strict_module)
        {
            return violation(property_message(name, method));
        }
        None
    }
}

/// The class a constraint's base type names, without `?`, type arguments or a
/// `module.` prefix.
fn user_class(base: &str) -> &str {
    let base = base.split('<').next().unwrap_or(base);
    base.trim_end_matches('?').trim_start_matches("module.")
}

/// The in-module class an inferred `new` builds, if its type names one.
fn inferred_class(ty: &TypeExpr) -> Option<&str> {
    match ty {
        TypeExpr::Named(name) => Some(name.trim_end_matches('?')),
        TypeExpr::Nullable(inner) => inferred_class(inner),
        _ => None,
    }
}

/// How a name is used: read as a property, called as a method, or applied as
/// a lambda (`name.apply(...)`).
const READ: u8 = 0;
const CALL: u8 = 1;
const APPLY: u8 = 2;

fn collect_calls_in_entries<'m>(entries: &'m [Entry], called: &mut HashSet<(&'m str, u8)>) {
    for entry in entries {
        match entry {
            Entry::Property(prop) => {
                if let Some(ty) = &prop.type_ann {
                    collect_calls_in_type(ty, called);
                }
                if let Some(value) = &prop.value {
                    collect_calls(value, called);
                }
                if let Some(body) = &prop.body {
                    collect_calls_in_entries(body, called);
                }
                for annotation in &prop.annotations {
                    collect_calls_in_entries(&annotation.body, called);
                }
            }
            Entry::DynProperty(key, value) | Entry::Predicate(key, value) => {
                collect_calls(key, called);
                collect_calls(value, called);
            }
            Entry::Spread(value) | Entry::Elem(value) => collect_calls(value, called),
            Entry::ForGenerator(generator) => {
                collect_calls(&generator.collection, called);
                collect_calls_in_entries(&generator.body, called);
            }
            Entry::WhenGenerator(generator) => {
                collect_calls(&generator.condition, called);
                collect_calls_in_entries(&generator.body, called);
                if let Some(body) = &generator.else_body {
                    collect_calls_in_entries(body, called);
                }
            }
            Entry::ClassDef(_, _, _, body) => collect_calls_in_entries(body, called),
            Entry::TypeAlias(_, ty) => collect_calls_in_type(ty, called),
        }
    }
}

fn collect_calls_in_type<'m>(ty: &'m TypeExpr, called: &mut HashSet<(&'m str, u8)>) {
    match ty {
        TypeExpr::Named(_) => {}
        TypeExpr::Nullable(inner) => collect_calls_in_type(inner, called),
        TypeExpr::Union(variants) => variants
            .iter()
            .for_each(|variant| collect_calls_in_type(variant, called)),
        TypeExpr::Generic(_, args) => args
            .iter()
            .for_each(|arg| collect_calls_in_type(arg, called)),
        TypeExpr::Constrained(_, constraint, _) => collect_calls(constraint, called),
    }
}

/// Collect the names an expression calls (`true`) or reads (`false`).
fn collect_calls<'m>(expr: &'m Expr, called: &mut HashSet<(&'m str, u8)>) {
    if crate::stack::exhausted() {
        crate::stack::limit_hit();
        return;
    }
    match expr {
        Expr::Call(callee, args) => {
            match callee.as_ref() {
                Expr::Ident(name) => {
                    called.insert((name, CALL));
                }
                Expr::Field(base, name) | Expr::NullSafeField(base, name) => {
                    called.insert((name, CALL));
                    if let (Expr::Ident(target), "apply") = (base.as_ref(), name.as_str()) {
                        called.insert((target, APPLY));
                    }
                    collect_calls(base, called);
                }
                other => collect_calls(other, called),
            }
            args.iter().for_each(|arg| collect_calls(arg, called));
        }
        Expr::Field(base, name) | Expr::NullSafeField(base, name) => {
            called.insert((name, READ));
            collect_calls(base, called);
        }
        Expr::Index(left, right) | Expr::Binop(_, left, right) => {
            collect_calls(left, called);
            collect_calls(right, called);
        }
        Expr::If(condition, then_expr, else_expr) => {
            collect_calls(condition, called);
            collect_calls(then_expr, called);
            collect_calls(else_expr, called);
        }
        Expr::Let(_, value, body) => {
            collect_calls(value, called);
            collect_calls(body, called);
        }
        Expr::Is(value, ty) | Expr::As(value, ty) | Expr::Checked(value, ty) => {
            collect_calls(value, called);
            collect_calls_in_type(ty, called);
        }
        Expr::Unop(_, value)
        | Expr::Throw(value)
        | Expr::TemplateLocals(_, value)
        | Expr::Trace(value, _)
        | Expr::Constraint(value, _)
        | Expr::Read(value, _)
        | Expr::ReadOrNull(value, _)
        | Expr::ReadGlob(value, _) => collect_calls(value, called),
        Expr::Lambda(_, body) => collect_calls(body, called),
        Expr::StringInterpolation(parts) => {
            for part in parts {
                if let StringInterpPart::Expr(value) = part {
                    collect_calls(value, called);
                }
            }
        }
        Expr::New(_, body, _) | Expr::InferredNew(_, body) | Expr::ObjectBody(body) => {
            collect_calls_in_entries(body, called)
        }
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Import(_, _)
        | Expr::ImportGlob(_, _) => {}
        Expr::Ident(name) => {
            called.insert((name, READ));
        }
    }
}

/// Find a property of a class that redeclares one of an in-module parent class
/// with a different `const` or `fixed` modifier. `module` names the module in
/// messages. Parents defined elsewhere are checked when the class is evaluated.
pub(super) fn find_override_violation(body: &[Entry], module: &str) -> Option<ConstViolation> {
    let classes: HashMap<&str, (Option<&str>, &[Entry])> = body
        .iter()
        .filter_map(|entry| match entry {
            Entry::ClassDef(name, _, parent, class_body) => {
                Some((name.as_str(), (parent.as_deref(), class_body.as_slice())))
            }
            _ => None,
        })
        .collect();
    for entry in body {
        let Entry::ClassDef(_, _, Some(parent), class_body) = entry else {
            continue;
        };
        let parent = parent.as_str();
        for entry in class_body.iter() {
            let Entry::Property(prop) = entry else {
                continue;
            };
            if prop.is_method || has(&prop.modifiers, Modifier::Local) {
                continue;
            }
            // The nearest ancestor that declares the property.
            let mut ancestor = Some(parent);
            let mut steps = 0;
            while let Some(current) = ancestor.filter(|_| steps <= classes.len()) {
                steps += 1;
                let Some((next, ancestor_body)) = classes.get(current) else {
                    break;
                };
                let declared = ancestor_body.iter().find_map(|entry| match entry {
                    Entry::Property(p)
                        if p.name == prop.name
                            && !p.is_method
                            && !has(&p.modifiers, Modifier::Local) =>
                    {
                        Some(p)
                    }
                    _ => None,
                });
                if let Some(declared) = declared {
                    for (modifier, word) in [(Modifier::Const, "const"), (Modifier::Fixed, "fixed")]
                    {
                        let child_has = has(&prop.modifiers, modifier.clone());
                        let parent_has = has(&declared.modifiers, modifier);
                        if child_has == parent_has {
                            continue;
                        }
                        let prop_name = &prop.name;
                        let parent_class = format!("{module}#{parent}");
                        return violation(if parent_has {
                            format!(
                                "Missing modifier `{word}`.\n\nProperty `{prop_name}` must be declared {word}, because it overrides a {word} property on parent class `{parent_class}`."
                            )
                        } else {
                            format!(
                                "Modifier `{word}` cannot be applied to property `{prop_name}`.\n\nProperty `{prop_name}` cannot be declared {word}, because it overrides a non-{word} property on parent class `{parent_class}`."
                            )
                        });
                    }
                    break;
                }
                ancestor = *next;
            }
        }
    }
    None
}
