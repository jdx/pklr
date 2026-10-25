use crate::error::{Error, Result};
use crate::lexer::{StringPart, Token, TokenKind};

mod ast;
mod checks;
mod cursor;
mod decls;
mod expr;
mod imports;
mod objects;
mod types;
mod validate;

pub use ast::{
    Annotation, BinOp, Body, Entry, Expr, ForGenerator, Import, LocalBindings, Modifier, Module,
    Property, StringInterpPart, TraceSite, TypeExpr, UnOp, WhenGenerator,
};
pub(crate) use ast::{has_untyped_result_new, infer_method_return_new, rewrite_untyped_result_new};
use ast::{infers_new, type_expr_runtime_name};
use checks::*;
use cursor::*;
use decls::*;
pub use imports::collect_imports;
pub(crate) use imports::collect_imports_with_kind;
use objects::*;

pub fn parse(tokens: &[Token]) -> Result<Module> {
    parse_named(tokens, "", "<input>")
}

pub fn parse_named(tokens: &[Token], source: &str, name: &str) -> Result<Module> {
    let mut p = Parser::new(tokens, source, name);
    p.parse_module()
}

pub fn parse_expr_tokens(tokens: &[Token], source: &str, name: &str) -> Result<Expr> {
    let mut p = Parser::new(tokens, source, name);
    let expr = p.parse_expr()?;
    if !p.at_eof() {
        return Err(p.unexpected_token("end of file"));
    }
    Ok(expr)
}

#[cfg(feature = "eval-core")]
pub(crate) fn parse_type_name(name: &str) -> Result<TypeExpr> {
    let tokens = crate::lexer::lex(name)?;
    Parser::new(&tokens, name, "<type>").parse_type()
}

/// The shared parser and lexer nesting limit.
pub const MAX_NESTING_DEPTH: usize = 128;

/// The doc comment, annotations and modifiers in front of a member.
#[derive(Default)]
struct MemberHeader {
    /// Offset of the first doc comment line
    doc_comment: Option<usize>,
    annotations: Vec<Annotation>,
    /// Offset of the first annotation
    annotations_start: Option<usize>,
    /// Modifiers and their offsets
    modifiers: Vec<(Modifier, usize)>,
}

impl MemberHeader {
    fn is_empty(&self) -> bool {
        self.doc_comment.is_none() && self.annotations.is_empty() && self.modifiers.is_empty()
    }

    fn start(&self) -> Option<usize> {
        self.doc_comment
            .or(self.annotations_start)
            .or(self.modifiers.first().map(|(_, offset)| *offset))
    }

    fn modifiers(&self) -> Vec<Modifier> {
        self.modifiers.iter().map(|(m, _)| m.clone()).collect()
    }

    fn has(&self, modifier: Modifier) -> bool {
        self.modifiers.iter().any(|(m, _)| *m == modifier)
    }
}

/// The kind of declaration a set of modifiers is attached to.
#[derive(Clone, Copy)]
enum MemberKind {
    Module,
    AmendingModule,
    Class,
    TypeAlias,
    Method,
    Property,
    ObjectMember,
}

impl MemberKind {
    fn allows(self, modifier: &Modifier) -> bool {
        use Modifier::*;
        match self {
            MemberKind::Module => matches!(modifier, Abstract | Open),
            MemberKind::AmendingModule => false,
            MemberKind::Class => matches!(modifier, Abstract | Open | Local | External),
            MemberKind::TypeAlias => matches!(modifier, Local | External),
            MemberKind::Method => matches!(modifier, Abstract | Local | External | Const),
            MemberKind::Property => {
                matches!(
                    modifier,
                    Abstract | Local | Hidden | External | Fixed | Const
                )
            }
            MemberKind::ObjectMember => matches!(modifier, Local | Const),
        }
    }

    fn description(self) -> &'static str {
        match self {
            MemberKind::Module => "modules",
            MemberKind::AmendingModule => "modules that amend another module",
            MemberKind::Class => "classes",
            MemberKind::TypeAlias => "type aliases",
            MemberKind::Method => "methods",
            MemberKind::Property => "properties",
            MemberKind::ObjectMember => "object members",
        }
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    source: &'a str,
    name: &'a str,
    pos: usize,
    /// URIs of `import(...)` expressions parsed in this module.
    import_exprs: Vec<String>,
    /// End offset of the last consumed token.
    prev_end: usize,
    /// Line of the last consumed token, used when no source is available.
    prev_line: usize,
    /// Whether the module amends another module.
    amending: bool,
    /// Simple class names of `new` expressions and their offsets, checked
    /// against the module's declarations once it is parsed.
    new_types: Vec<(String, usize)>,
    /// Type parameter names for aliases currently being parsed.
    type_alias_parameters: std::collections::HashMap<String, Vec<String>>,
    /// Local aliases are resolved only if a value requires them, so their
    /// definitions are intentionally excluded from the module-level cycle
    /// check.
    local_type_aliases: std::collections::HashSet<String>,
    /// The module-level subset of `local_type_aliases`, which importers cannot see.
    module_local_type_aliases: std::collections::HashSet<String>,
    /// Expressions and type constraints currently nested around the cursor.
    depth: usize,
}
