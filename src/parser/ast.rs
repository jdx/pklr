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
    /// The name from the `module` declaration, if any.
    pub name: Option<String>,
    pub amends: Option<String>,
    pub extends: Option<String>,
    pub imports: Vec<Import>,
    /// URIs of the module's `import("...")` expressions, in source order.
    /// Glob `import*(...)` expressions are not included.
    pub import_exprs: Vec<String>,
    pub annotations: Vec<Annotation>,
    pub body: Body,
    /// Names of the module's `local` type aliases, which importers cannot see.
    pub local_type_aliases: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub uri: String,
    pub alias: Option<String>,
    /// `import*` glob import — result is a Mapping<String, Module>
    pub is_glob: bool,
}

/// Where a `trace(...)` expression appears: the traced expression's source
/// text, and the module path and line pkl reports with the traced value.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceSite {
    pub source: String,
    pub module: String,
    pub line: usize,
}

/// A top-level or object entry.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Entry {
    /// `key = expr` or `key: Type = expr`
    Property(std::sync::Arc<Property>),
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
    /// Member predicate: `[[predicate]] = expr` or `[[predicate]] { body }`
    /// (the body form is an `Expr::ObjectBody`). Applies to every element or
    /// entry of the amended object for which `predicate` holds, with `this`
    /// bound to the element or entry value.
    Predicate(Expr, Expr),
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
    pub name: std::sync::Arc<str>,
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
    /// The third field says how to read the text of the type before the
    /// constraint (see [`BaseForm`]).
    Constrained(String, Box<Expr>, BaseForm),
}

/// How the text of the base of a constrained type is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseForm {
    /// The text of a type, such as `List<Int>` or `C?` (the nullable `C`).
    Text,
    /// One name written in backticks, such as `C?`, whatever it holds.
    Quoted,
    /// The same quoted name, and null matches it too (the use `L?(c)` of an
    /// alias whose target is a quoted name).
    NullableQuoted,
}

impl TypeExpr {
    /// Whether this is a `Listing<...>` type, possibly nullable.
    pub(crate) fn is_listing_type(&self) -> bool {
        match self {
            TypeExpr::Nullable(inner) => inner.is_listing_type(),
            TypeExpr::Generic(name, _) => name == "Listing",
            // A constrained listing, `Listing<T>(length > 0)`, stores its
            // base type as a name.
            TypeExpr::Constrained(base, _, _) => {
                let base = base.trim_start_matches('*').trim_end_matches('?');
                base == "Listing" || base.starts_with("Listing<")
            }
            _ => false,
        }
    }
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
        TypeExpr::Constrained(name, _, _) => name.clone(),
    }
}

/// Whether `new { ... }` assigned to a property declared `ty` should build
/// `ty`: a Mapping or Listing (with or without type arguments), possibly
/// nullable or constrained, or a class or alias name resolved at evaluation.
pub(super) fn infers_new(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Generic(name, _) => matches!(name.as_str(), "Mapping" | "Listing"),
        TypeExpr::Nullable(inner) => infers_new(inner),
        TypeExpr::Constrained(base, _, _) => {
            let base = base.trim_end_matches('?');
            matches!(base.split('<').next(), Some("Mapping" | "Listing"))
        }
        TypeExpr::Named(name) => {
            name.starts_with(|c: char| c.is_ascii_uppercase())
                && !matches!(
                    name.as_str(),
                    "Dynamic" | "Any" | "Map" | "List" | "Set" | "Object"
                )
        }
        TypeExpr::Union(_) => false,
    }
}

/// Wrap a function body so that its typed parameters and its result type are
/// checked. Untyped parameters and `Any` need no check.
pub(crate) fn check_signature(
    params: &[(String, Option<TypeExpr>)],
    return_type: Option<&TypeExpr>,
    body: Expr,
) -> Expr {
    fn unchecked(ty: &TypeExpr) -> bool {
        match ty {
            TypeExpr::Named(name) => matches!(name.as_str(), "Any" | "unknown"),
            TypeExpr::Nullable(inner) => unchecked(inner),
            _ => false,
        }
    }
    let mut body = match return_type {
        Some(ty) if !unchecked(ty) => Expr::Checked(Box::new(body), ty.clone()),
        _ => body,
    };
    for (name, ty) in params.iter().rev() {
        if let Some(ty) = ty
            && !unchecked(ty)
        {
            body = Expr::Let(
                "_".to_string(),
                Box::new(Expr::Checked(
                    Box::new(Expr::Ident(name.clone())),
                    ty.clone(),
                )),
                Box::new(body),
            );
        }
    }
    body
}

/// Whether `expr` has an untyped `new { ... }` in a result position.
pub(crate) fn has_untyped_result_new(expr: &Expr) -> bool {
    match expr {
        Expr::New(None, _, _) => true,
        Expr::If(_, then_expr, else_expr) => {
            has_untyped_result_new(then_expr) || has_untyped_result_new(else_expr)
        }
        Expr::Let(_, _, body) | Expr::Trace(body, _) | Expr::TemplateLocals(_, body) => {
            has_untyped_result_new(body)
        }
        _ => false,
    }
}

/// Propagate the method's expected result type through result expressions only.
/// Nested members and call arguments have their own inference contexts.
pub(crate) fn infer_method_return_new(expr: &mut Expr, return_type: &TypeExpr) {
    rewrite_untyped_result_new(expr, &mut |entries| {
        Expr::InferredNew(return_type.clone(), entries)
    });
}

/// Replace each untyped `new { ... }` in a result position of `expr`.
pub(crate) fn rewrite_untyped_result_new(expr: &mut Expr, rewrite: &mut impl FnMut(Body) -> Expr) {
    match expr {
        Expr::New(None, entries, _) => *expr = rewrite(std::mem::take(entries)),
        Expr::If(_, then_expr, else_expr) => {
            rewrite_untyped_result_new(then_expr, rewrite);
            rewrite_untyped_result_new(else_expr, rewrite);
        }
        Expr::Let(_, _, body) | Expr::Trace(body, _) | Expr::TemplateLocals(_, body) => {
            rewrite_untyped_result_new(body, rewrite)
        }
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
    String(std::sync::Arc<str>),
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
    /// A value that must match a declared type: a function's argument or
    /// result. Unlike `as`, a type pklr cannot model accepts the value, as
    /// it does for a declared property.
    Checked(Box<Expr>, TypeExpr),
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
    Lambda(std::sync::Arc<[String]>, std::sync::Arc<Expr>),
    /// `throw("msg")`
    Throw(Box<Expr>),
    /// `trace(expr)`, with where it appears for the trace message.
    Trace(Box<Expr>, std::sync::Arc<TraceSite>),
    /// `read("uri")`. Fields: the URI, and the path of the module the
    /// expression was written in, which relative URIs resolve against.
    Read(Box<Expr>, String),
    /// `read?("uri")` — returns null on failure
    ReadOrNull(Box<Expr>, String),
    /// `read*("glob")`
    ReadGlob(Box<Expr>, String),
    /// `import("uri")` — evaluates the imported module as a value.
    /// Fields: the URI, and the path of the module the expression was written in,
    /// which relative URIs resolve against.
    Import(String, String),
    /// `import*("glob")` — Mapping of matched module paths to their values.
    /// Fields: the glob pattern, and the path of the module the expression was
    /// written in, which relative patterns resolve against.
    ImportGlob(String, String),
    /// An expression written in a module that amended objects inherit from:
    /// the module's `local` bindings shadow whatever an amending module
    /// declared under the same names while the expression is evaluated.
    TemplateLocals(LocalBindings, Box<Expr>),
}

/// The values of a module's `local` members, captured when the module's
/// scope was complete.
#[derive(Debug, Clone)]
pub struct LocalBindings(pub std::sync::Arc<Vec<(std::sync::Arc<str>, crate::value::Value)>>);

impl PartialEq for LocalBindings {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
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

/// Replace the type parameters of a generic type alias in its target by
/// `unknown`, which accepts every value. Evaluation does not know an alias's
/// parameters, and a parameter named like a real alias (`typealias Pair<Foo> =
/// List<Foo>`) must not be resolved as that alias.
pub(crate) fn erase_type_parameters(ty: TypeExpr, parameters: &[String]) -> TypeExpr {
    if parameters.is_empty() {
        return ty;
    }
    fn erase_word(text: &str, parameters: &[String]) -> String {
        let mut out = String::with_capacity(text.len());
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            if parameters.iter().any(|p| p == word) {
                out.push_str("unknown");
            } else {
                out.push_str(word);
            }
            word.clear();
        };
        // Words inside a string literal type (`Listing<"T">`) are text, not
        // type names.
        let mut in_string = false;
        let mut escaped = false;
        for c in text.chars() {
            if in_string {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
            } else if c.is_alphanumeric() || c == '_' || c == '.' {
                word.push(c);
            } else if c == '"' {
                flush(&mut word, &mut out);
                out.push(c);
                in_string = true;
            } else {
                flush(&mut word, &mut out);
                out.push(c);
            }
        }
        flush(&mut word, &mut out);
        out
    }
    match ty {
        TypeExpr::Named(name) => {
            let (star, bare) = match name.strip_prefix('*') {
                Some(bare) => ("*", bare),
                None => ("", name.as_str()),
            };
            if parameters.iter().any(|p| p == bare) {
                TypeExpr::Named(format!("{star}unknown"))
            } else {
                TypeExpr::Named(name)
            }
        }
        TypeExpr::Nullable(inner) => {
            TypeExpr::Nullable(Box::new(erase_type_parameters(*inner, parameters)))
        }
        TypeExpr::Union(variants) => TypeExpr::Union(
            variants
                .into_iter()
                .map(|variant| erase_type_parameters(variant, parameters))
                .collect(),
        ),
        TypeExpr::Generic(name, args) => TypeExpr::Generic(
            name,
            args.into_iter()
                .map(|arg| erase_type_parameters(arg, parameters))
                .collect(),
        ),
        TypeExpr::Constrained(base, constraint, quoted) => {
            TypeExpr::Constrained(erase_word(&base, parameters), constraint, quoted)
        }
    }
}
