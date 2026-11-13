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

pub use ast::BaseForm;
pub use ast::{
    Annotation, BinOp, Body, Entry, Expr, ForGenerator, GenericAlias, Import, LocalBindings,
    Modifier, Module, Property, StringInterpPart, TraceSite, TypeExpr, UnOp, WhenGenerator,
};
pub(crate) use ast::{BodylessMethod, Declaration, SourcePos};
use ast::{
    check_signature, erase_type_parameters, infers_new, type_expr_constrained_name,
    type_expr_runtime_name,
};
pub(crate) use ast::{has_untyped_result_new, infer_method_return_new, rewrite_untyped_result_new};
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

/// Parse a module together with the declarations `pkl:reflect` describes,
/// which the public `Module` does not carry.
pub(crate) fn parse_named_declarations(
    tokens: &[Token],
    source: &str,
    name: &str,
) -> Result<(Module, std::sync::Arc<Vec<Declaration>>)> {
    let mut p = Parser::new(tokens, source, name);
    let module = p.parse_module()?;
    Ok((module, std::mem::take(&mut p.declarations).into()))
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

/// The nesting limit of the parser and lexer on a stack of unknown size, such
/// as a direct call to [`parse`]. Evaluation runs on a large stack of its own,
/// where the parser allows far deeper nesting.
pub const MAX_NESTING_DEPTH: usize = crate::stack::SMALL_STACK_PARSER_DEPTH;

/// The nesting limit that applies on the current thread's stack.
fn nesting_limit() -> usize {
    if crate::stack::on_large_stack() {
        crate::stack::PARSER_DEPTH
    } else {
        MAX_NESTING_DEPTH
    }
}

/// Whether any module parsed so far calls `getClass()` on its enclosing object
/// without a receiver. Only then does the evaluator track the class of each
/// object body for it.
pub(crate) static BARE_GET_CLASS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The doc comment, annotations and modifiers in front of a member.
#[derive(Default)]
struct MemberHeader {
    /// Offset of the first doc comment line
    doc_comment: Option<usize>,
    /// The text of the doc comment, without the `///` markers
    doc: Option<std::sync::Arc<str>>,
    annotations: Vec<Annotation>,
    /// Offset of the first annotation
    annotations_start: Option<usize>,
    /// Where the declaration proper starts: its first modifier or keyword.
    at: SourcePos,
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
    /// Qualifiers of `new q.Type` expressions and their offsets, which must be imports.
    new_qualifiers: Vec<(String, usize)>,
    /// Type parameter names for aliases currently being parsed.
    type_alias_parameters: std::collections::HashMap<String, Vec<String>>,
    /// Local aliases are resolved only if a value requires them, so their
    /// definitions are intentionally excluded from the module-level cycle
    /// check.
    local_type_aliases: std::collections::HashSet<String>,
    /// The module-level subset of `local_type_aliases`, which importers cannot see.
    module_local_type_aliases: std::collections::HashSet<String>,
    /// Simple type names written in annotations, `is`/`as`, type arguments,
    /// constraints and `new` outside a type alias's own definition.
    type_uses: std::collections::HashSet<String>,
    /// Whether the type being parsed is a type alias's definition.
    in_alias_definition: Option<String>,
    /// The type names each alias's definition uses, constraint predicates
    /// (`is`, `as`) included.
    alias_definition_uses: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Expressions and type constraints currently nested around the cursor.
    depth: usize,
    /// The classes and type aliases declared so far, for `pkl:reflect`.
    declarations: Vec<Declaration>,
    /// How many class or object bodies enclose what is being parsed.
    declaration_nesting: u32,
    /// The methods without a body in the class being parsed.
    bodyless_methods: Vec<BodylessMethod>,
}
