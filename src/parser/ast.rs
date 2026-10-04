#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StringInterpPart {
    Literal(String),
    Expr(Expr),
}

/// The entries of an object, class or generator body. Shared so the
/// evaluator can hand a body to every object built from it without copying.
pub type Body = std::sync::Arc<Vec<Entry>>;

/// An annotation: `@Name { body }` or `@Name`
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub name: String,
    pub body: Vec<Entry>,
}

/// A pkl module (top-level file).
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub amends: Option<String>,
    pub extends: Option<String>,
    pub imports: Vec<Import>,
    pub annotations: Vec<Annotation>,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub uri: String,
    pub alias: Option<String>,
    /// `import*` glob import — result is a Mapping<String, Module>
    pub is_glob: bool,
}

/// A top-level or object entry.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Entry {
    /// `key = expr` or `key: Type = expr`
    Property(Property),
    /// `["key"] = expr` (dynamic key)
    DynProperty(Expr, Expr),
    /// `for (k, v in collection) { ... }`
    ForGenerator(ForGenerator),
    /// `when (cond) { ... }`
    WhenGenerator(WhenGenerator),
    /// `...spread`
    Spread(Expr),
    /// Bare element expression (used in Listing bodies)
    Elem(Expr),
    /// Class definition: `[modifiers] class Name [extends Parent] { properties... }`
    /// Fields: (name, modifiers, optional_parent, body)
    ClassDef(String, Vec<Modifier>, Option<String>, Body),
    /// Type alias: `typealias Name = Type`
    TypeAlias(String, TypeExpr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub annotations: Vec<Annotation>,
    pub modifiers: Vec<Modifier>,
    pub name: String,
    pub type_ann: Option<TypeExpr>,
    pub value: Option<Expr>,
    /// Object body amendment: `foo { ... }` (no `=`)
    pub body: Option<Body>,
    /// Declared with `function`: a method, whose `value` is the lambda it
    /// evaluates to. Methods are not members, so they take no part in an
    /// object's equality.
    pub is_method: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Modifier {
    Local,
    Const,
    Fixed,
    Hidden,
    Abstract,
    Open,
    External,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TypeExpr {
    Named(String),
    Nullable(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    Generic(String, Vec<TypeExpr>),
    /// Constrained type: `Type(predicate)` e.g. `Int(this >= 0)`.
    /// Multiple source constraints are represented by a conjunctive expression.
    Constrained(String, Box<Expr>),
}

pub(super) fn type_expr_runtime_name(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Nullable(inner) => format!("{}?", type_expr_runtime_name(inner)),
        TypeExpr::Union(_) => "Any".to_string(),
        TypeExpr::Generic(name, args) => format!(
            "{}<{}>",
            name,
            args.iter()
                .map(type_expr_runtime_name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExpr::Constrained(name, _) => name.clone(),
    }
}

/// Propagate the method's expected result type through result expressions only.
/// Nested members and call arguments have their own inference contexts.
pub(super) fn infer_method_return_new(expr: &mut Expr, return_type: &TypeExpr) {
    match expr {
        Expr::New(None, entries, _) => {
            *expr = Expr::InferredNew(return_type.clone(), std::mem::take(entries));
        }
        Expr::If(_, then_expr, else_expr) => {
            infer_method_return_new(then_expr, return_type);
            infer_method_return_new(else_expr, return_type);
        }
        Expr::Let(_, _, body) | Expr::Trace(body) => infer_method_return_new(body, return_type),
        _ => {}
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Expr {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Ident(String),
    /// `new TypeName? { entries... }`
    /// The third field holds optional generic type parameter names (e.g., `<String, Step>`).
    New(Option<String>, Body, Vec<String>),
    /// An implicit `new` whose parent is inferred from a method return type.
    InferredNew(TypeExpr, Body),
    /// `expr.field`
    Field(Box<Expr>, String),
    /// `expr[key]`
    Index(Box<Expr>, Box<Expr>),
    /// `expr(args...)`
    Call(Box<Expr>, Vec<Expr>),
    /// `if (cond) then else`
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `let (name = val) body`
    Let(String, Box<Expr>, Box<Expr>),
    /// `expr is Type`
    Is(Box<Expr>, TypeExpr),
    /// `expr as Type`
    As(Box<Expr>, TypeExpr),
    /// Binary operation
    Binop(BinOp, Box<Expr>, Box<Expr>),
    /// Unary operation
    Unop(UnOp, Box<Expr>),
    /// Object/listing literal — anonymous `{ ... }`
    ObjectBody(Body),
    /// String interpolation: alternating literal strings and expressions
    StringInterpolation(Vec<StringInterpPart>),
    /// Null-safe field access: `expr?.field`
    NullSafeField(Box<Expr>, String),
    /// Lambda: `(params) -> body`
    Lambda(Vec<String>, Box<Expr>),
    /// `throw("msg")`
    Throw(Box<Expr>),
    /// `trace(expr)`
    Trace(Box<Expr>),
    /// `read("uri")`
    Read(Box<Expr>),
    /// `read?("uri")` — returns null on failure
    ReadOrNull(Box<Expr>),
    /// `import("uri")` — evaluates the imported module as a value.
    /// Fields: the URI, and the path of the module the expression was written in,
    /// which relative URIs resolve against.
    Import(String, String),
    /// `import*("glob")` — Mapping of matched module paths to their values.
    /// Fields: the glob pattern, and the path of the module the expression was
    /// written in, which relative patterns resolve against.
    ImportGlob(String, String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    IntDiv,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    NullCoalesce,
    Pipe,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum UnOp {
    Neg,
    Not,
    NonNull,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForGenerator {
    pub key_var: Option<String>,
    pub val_var: String,
    pub collection: Expr,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhenGenerator {
    pub condition: Expr,
    pub body: Body,
    pub else_body: Option<Body>,
}
