//! Static checks pkl performs while building a module: modifier
//! applicability, duplicate member definitions and a few member rules.
//! They run while parsing, so the errors point at the offending member.

use std::collections::HashSet;

use super::Parser;
use super::ast::{Expr, Import, Modifier, Property};
use crate::error::Result;
use crate::lexer::TokenKind;

/// The kind of body whose members are being parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BodyKind {
    /// A module that does not amend another module. Its members are class
    /// members.
    Module,
    /// A module that amends another module. Its members are object members.
    AmendingModule,
    Class,
    Object,
    /// The body of a `when` generator.
    When,
    /// The body of a `for` generator, or of a `when` generator nested in one.
    For,
}

/// A constant object entry key, as far as duplicate detection is concerned.
#[derive(Debug, PartialEq, Eq, Hash)]
enum ConstKey {
    String(String),
    Int(i64),
    Bool(bool),
}

/// Members defined so far in one body.
pub(super) struct BodyScope {
    pub(super) kind: BodyKind,
    /// Offset of the innermost enclosing `for` keyword, if the body is
    /// (nested in) a for-generator body.
    for_offset: Option<usize>,
    /// Names of properties, classes, type aliases and imports. In modules
    /// and classes, local and non-local members share one namespace.
    properties: HashSet<String>,
    /// Local property names (object bodies keep them apart from non-local
    /// ones).
    local_properties: HashSet<String>,
    methods: HashSet<String>,
    entries: HashSet<ConstKey>,
    /// Whether the body has a generator or spread member. pkl merges the
    /// members of such a body at runtime, so a duplicate in it is not a
    /// static error.
    has_generator: bool,
    /// The first duplicate found in an object body, reported once the body
    /// is known to have no generator.
    duplicate: Option<(usize, String)>,
}

impl BodyScope {
    pub(super) fn new(kind: BodyKind, for_offset: Option<usize>) -> Self {
        Self {
            kind,
            for_offset,
            properties: HashSet::new(),
            local_properties: HashSet::new(),
            methods: HashSet::new(),
            entries: HashSet::new(),
            has_generator: false,
            duplicate: None,
        }
    }

    /// Record the names a module's imports bind, which share the namespace
    /// of its properties. Returns a name bound twice.
    pub(super) fn declare_imports(&mut self, imports: &[Import]) -> Option<String> {
        for import in imports {
            let name = match &import.alias {
                Some(alias) => alias.as_str(),
                None if import.is_glob => continue,
                None => inferred_import_name(&import.uri),
            };
            if !self.properties.insert(name.to_string()) {
                return Some(name.to_string());
            }
        }
        None
    }

    pub(super) fn set_has_generator(&mut self) {
        self.has_generator = true;
    }
}

fn modifier_keyword(modifier: &Modifier) -> &'static str {
    match modifier {
        Modifier::Local => "local",
        Modifier::Const => "const",
        Modifier::Fixed => "fixed",
        Modifier::Hidden => "hidden",
        Modifier::Abstract => "abstract",
        Modifier::Open => "open",
        Modifier::External => "external",
    }
}

const MODULE_MODIFIERS: &[Modifier] = &[Modifier::Abstract, Modifier::Open];
const CLASS_MODIFIERS: &[Modifier] = &[
    Modifier::Abstract,
    Modifier::Open,
    Modifier::Local,
    Modifier::External,
];
const TYPE_ALIAS_MODIFIERS: &[Modifier] = &[Modifier::Local, Modifier::External];
const METHOD_MODIFIERS: &[Modifier] = &[
    Modifier::Abstract,
    Modifier::Local,
    Modifier::External,
    Modifier::Const,
];
const PROPERTY_MODIFIERS: &[Modifier] = &[
    Modifier::Abstract,
    Modifier::Local,
    Modifier::Hidden,
    Modifier::External,
    Modifier::Fixed,
    Modifier::Const,
];
const OBJECT_MEMBER_MODIFIERS: &[Modifier] = &[Modifier::Local, Modifier::Const];

/// The name a module import is bound to when it has no `as` alias: the last
/// path segment of the URI without its extension.
fn inferred_import_name(uri: &str) -> &str {
    let last = uri.rsplit(['/', ':', '#']).next().unwrap_or(uri);
    match last.rfind('.') {
        Some(dot) if dot > 0 => &last[..dot],
        _ => last,
    }
}

impl Parser<'_> {
    fn semantic_error(&self, offset: usize, message: impl Into<String>) -> crate::error::Error {
        crate::error::Error::parse(self.name, self.source, offset, message.into())
    }

    /// Check `modifiers` against the set valid for a kind of member, then
    /// reject the combinations pkl never allows.
    fn check_modifiers(
        &self,
        offset: usize,
        modifiers: &[Modifier],
        valid: &[Modifier],
        what: &str,
    ) -> Result<()> {
        if let Some(invalid) = modifiers.iter().find(|m| !valid.contains(m)) {
            return Err(self.semantic_error(
                offset,
                format!(
                    "Modifier `{}` is not applicable to {what}.",
                    modifier_keyword(invalid)
                ),
            ));
        }
        let has = |m: Modifier| modifiers.contains(&m);
        let message = if has(Modifier::External) {
            "External members can only be defined by standard library modules."
        } else if has(Modifier::Local) && has(Modifier::Hidden) {
            "Modifier `hidden` is redundant here; just use `local`."
        } else if has(Modifier::Local) && has(Modifier::Fixed) {
            "Modifier `fixed` is redundant here; just use `local`."
        } else if has(Modifier::Abstract) && has(Modifier::Open) {
            "Modifier `open` is redundant here; just use `abstract`."
        } else {
            return Ok(());
        };
        Err(self.semantic_error(offset, message))
    }

    fn check_object_member_modifiers(&self, offset: usize, modifiers: &[Modifier]) -> Result<()> {
        self.check_modifiers(offset, modifiers, OBJECT_MEMBER_MODIFIERS, "object members")?;
        if modifiers.contains(&Modifier::Const) && !modifiers.contains(&Modifier::Local) {
            return Err(self.semantic_error(
                offset,
                "Modifier `const` can only be applied to object members that are also `local`.",
            ));
        }
        Ok(())
    }

    /// Check the modifiers of the module declaration (`open module foo`).
    pub(super) fn check_module_modifiers(
        &self,
        offset: usize,
        modifiers: &[Modifier],
        amends: bool,
    ) -> Result<()> {
        self.check_modifiers(offset, modifiers, MODULE_MODIFIERS, "modules")?;
        if amends {
            self.check_modifiers(offset, modifiers, &[], "modules that amend another module")?;
        }
        Ok(())
    }

    pub(super) fn duplicate_error(&self, offset: usize, name: &str) -> crate::error::Error {
        self.semantic_error(offset, format!("Duplicate definition of member `{name}`."))
    }

    /// Record a member name, reporting a duplicate right away in modules and
    /// classes, and once the body is complete in object bodies.
    fn declare(&mut self, offset: usize, name: &str, method: bool, local: bool) -> Result<()> {
        let in_object = matches!(
            self.body.kind,
            BodyKind::Object | BodyKind::When | BodyKind::For
        );
        let set = if method {
            &mut self.body.methods
        } else if in_object && local {
            &mut self.body.local_properties
        } else {
            &mut self.body.properties
        };
        if set.insert(name.to_string()) {
            return Ok(());
        }
        if !in_object {
            return Err(self.duplicate_error(offset, name));
        }
        if self.body.duplicate.is_none() {
            self.body.duplicate = Some((offset, name.to_string()));
        }
        Ok(())
    }

    /// Report a duplicate member of a finished body, unless pkl only finds it
    /// at runtime.
    pub(super) fn finish_body(&self, scope: &BodyScope) -> Result<()> {
        if scope.has_generator || matches!(scope.kind, BodyKind::When | BodyKind::For) {
            return Ok(());
        }
        match &scope.duplicate {
            Some((offset, name)) => Err(self.duplicate_error(*offset, name)),
            None => Ok(()),
        }
    }

    pub(super) fn check_class(
        &mut self,
        offset: usize,
        modifiers: &[Modifier],
        name: &str,
    ) -> Result<()> {
        self.check_modifiers(offset, modifiers, CLASS_MODIFIERS, "classes")?;
        if self.body.kind == BodyKind::AmendingModule && !modifiers.contains(&Modifier::Local) {
            return Err(self.semantic_error(offset, "Class needs a `local` modifier."));
        }
        self.declare(offset, name, false, modifiers.contains(&Modifier::Local))
    }

    pub(super) fn check_type_alias(
        &mut self,
        offset: usize,
        modifiers: &[Modifier],
        name: &str,
    ) -> Result<()> {
        self.check_modifiers(offset, modifiers, TYPE_ALIAS_MODIFIERS, "type aliases")?;
        if self.body.kind == BodyKind::AmendingModule && !modifiers.contains(&Modifier::Local) {
            return Err(self.semantic_error(offset, "Type alias needs a `local` modifier."));
        }
        self.declare(offset, name, false, modifiers.contains(&Modifier::Local))
    }

    /// Reject a duplicate type parameter in the `<...>` list starting at
    /// token `start`, ignoring the `in`/`out` variance markers.
    pub(super) fn check_type_parameters(&self, start: usize) -> Result<()> {
        let mut seen: Vec<&str> = Vec::new();
        let mut depth = 0;
        for (i, tok) in self.tokens.iter().enumerate().skip(start) {
            match &tok.kind {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                TokenKind::Ident(name) if depth == 1 => {
                    let is_variance = name == "out"
                        && matches!(
                            self.tokens.get(i + 1).map(|t| &t.kind),
                            Some(TokenKind::Ident(_))
                        );
                    if is_variance {
                        continue;
                    }
                    if seen.contains(&name.as_str()) {
                        return Err(self.semantic_error(
                            tok.offset,
                            format!("Duplicate type parameter `{name}`."),
                        ));
                    }
                    seen.push(name);
                }
                TokenKind::Eof => break,
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn type_parameters_error(&self) -> crate::error::Error {
        self.semantic_error(
            self.peek_tok().offset,
            "Only standard library members can have type parameters.",
        )
    }

    pub(super) fn check_method(
        &mut self,
        offset: usize,
        modifiers: &[Modifier],
        name: &str,
    ) -> Result<()> {
        let local = modifiers.contains(&Modifier::Local);
        match self.body.kind {
            BodyKind::Module | BodyKind::Class => {
                self.check_modifiers(offset, modifiers, METHOD_MODIFIERS, "methods")?;
            }
            BodyKind::For => {
                return Err(self.semantic_error(
                    self.body.for_offset.unwrap_or(offset),
                    "A for-generator cannot generate object methods (only entries and elements).",
                ));
            }
            kind => {
                self.check_modifiers(offset, modifiers, OBJECT_MEMBER_MODIFIERS, "object members")?;
                if !local {
                    let message = if kind == BodyKind::AmendingModule {
                        "Method needs a `local` modifier."
                    } else {
                        "Method needs a `local` modifier because it is defined in an object, not a class."
                    };
                    return Err(self.semantic_error(offset, message));
                }
            }
        }
        self.declare(offset, name, true, local)
    }

    pub(super) fn check_property(&mut self, offset: usize, prop: &Property) -> Result<()> {
        let local = prop.modifiers.contains(&Modifier::Local);
        match self.body.kind {
            BodyKind::Module | BodyKind::Class => {
                self.check_modifiers(offset, &prop.modifiers, PROPERTY_MODIFIERS, "properties")?;
            }
            BodyKind::For => {
                return Err(self.semantic_error(
                    self.body.for_offset.unwrap_or(offset),
                    "A for-generator cannot generate object properties (only entries and elements).",
                ));
            }
            _ => {
                self.check_object_member_modifiers(offset, &prop.modifiers)?;
                if !local && prop.type_ann.is_some() {
                    return Err(self.semantic_error(
                        offset,
                        "A non-local object property cannot have a type annotation.",
                    ));
                }
                if local && prop.body.is_some() {
                    return Err(self
                        .semantic_error(offset, "A local property definition cannot be amended."));
                }
            }
        }
        if local && prop.value.is_none() && prop.body.is_none() {
            return Err(self.semantic_error(offset, "Missing property value."));
        }
        self.declare(offset, &prop.name, false, local)
    }

    /// Record an object entry whose key is a constant.
    pub(super) fn check_entry_key(&mut self, offset: usize, key: &Expr) {
        let key = match key {
            Expr::String(s) => ConstKey::String(s.to_string()),
            Expr::Int(i) => ConstKey::Int(*i),
            Expr::Bool(b) => ConstKey::Bool(*b),
            _ => return,
        };
        let shown = match &key {
            ConstKey::String(s) => format!("\"{s}\""),
            ConstKey::Int(i) => i.to_string(),
            ConstKey::Bool(b) => b.to_string(),
        };
        if !self.body.entries.insert(key) && self.body.duplicate.is_none() {
            self.body.duplicate = Some((offset, shown));
        }
    }

    pub(super) fn for_scope(&self, for_offset: usize) -> BodyScope {
        BodyScope::new(BodyKind::For, Some(for_offset))
    }

    pub(super) fn when_scope(&self) -> BodyScope {
        match self.body.for_offset {
            Some(offset) => BodyScope::new(BodyKind::For, Some(offset)),
            None => BodyScope::new(BodyKind::When, None),
        }
    }
}
