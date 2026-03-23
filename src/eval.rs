use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{self, BinOp, Entry, Expr, Module, Property, UnOp};
use crate::value::Value;

/// Evaluates pkl source files to [`Value`].
pub struct Evaluator {
    base_path: PathBuf,
    /// Maximum import depth to prevent infinite recursion
    max_depth: usize,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
        }
    }
}

impl Evaluator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_base_path(&mut self, path: &Path) {
        self.base_path = path.to_path_buf();
    }

    pub fn eval_source(&mut self, source: &str, path: &Path) -> Result<Value> {
        let tokens = lexer::lex(source)?;
        let module = parser::parse(&tokens)?;
        self.eval_module(&module, path, 0)
    }

    fn eval_module(&mut self, module: &Module, _path: &Path, depth: usize) -> Result<Value> {
        let mut scope = Scope::default();

        // First pass: collect all `local` variable definitions into scope
        for entry in &module.body {
            if let Entry::Property(prop) = entry
                && prop
                    .modifiers
                    .iter()
                    .any(|m| matches!(m, crate::parser::Modifier::Local))
                && let Some(expr) = &prop.value
            {
                let val = self.eval_expr(expr, &scope, depth)?;
                scope.set(prop.name.clone(), val);
            }
        }

        // Second pass: evaluate non-local entries into output object
        let mut out = IndexMap::new();
        for entry in &module.body {
            if let Entry::Property(prop) = entry {
                if prop
                    .modifiers
                    .iter()
                    .any(|m| matches!(m, crate::parser::Modifier::Local))
                {
                    continue; // already collected
                }
                let val = self.eval_property(prop, &scope, depth)?;
                if let Some(v) = val {
                    out.insert(prop.name.clone(), v);
                }
            }
        }

        Ok(Value::Object(out))
    }

    fn eval_property(
        &mut self,
        prop: &Property,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Some(expr) = &prop.value {
            return Ok(Some(self.eval_expr(expr, scope, depth)?));
        }
        if let Some(body) = &prop.body {
            // `foo { ... }` — object body amendment
            let val = self.eval_entries(body, scope, depth)?;
            return Ok(Some(val));
        }
        Ok(None) // bare type-only declaration
    }

    fn eval_entries(&mut self, entries: &[Entry], scope: &Scope, depth: usize) -> Result<Value> {
        let mut child_scope = scope.child();
        // First pass: collect locals
        for entry in entries {
            if let Entry::Property(prop) = entry
                && prop
                    .modifiers
                    .iter()
                    .any(|m| matches!(m, crate::parser::Modifier::Local))
                && let Some(expr) = &prop.value
            {
                let val = self.eval_expr(expr, &child_scope, depth)?;
                child_scope.set(prop.name.clone(), val);
            }
        }

        let mut map: IndexMap<String, Value> = IndexMap::new();
        for entry in entries {
            match entry {
                Entry::Property(prop) => {
                    if prop
                        .modifiers
                        .iter()
                        .any(|m| matches!(m, crate::parser::Modifier::Local))
                    {
                        continue;
                    }
                    if let Some(v) = self.eval_property(prop, &child_scope, depth)? {
                        map.insert(prop.name.clone(), v);
                    }
                }
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, &child_scope, depth)?;
                    let val = self.eval_expr(val_expr, &child_scope, depth)?;
                    let key_str = value_to_key(&key)?;
                    map.insert(key_str, val);
                }
                Entry::Spread(expr) => {
                    let val = self.eval_expr(expr, &child_scope, depth)?;
                    if let Value::Object(m) = val {
                        map.extend(m);
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self.eval_expr(&fgen.collection, &child_scope, depth)?;
                    let items = collection_to_items(collection);
                    for (k, v) in items {
                        let mut iter_scope = child_scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(key_var) = &fgen.key_var {
                            iter_scope.set(key_var.clone(), k);
                        }
                        let body_val = self.eval_entries(&fgen.body, &iter_scope, depth)?;
                        if let Value::Object(m) = body_val {
                            map.extend(m);
                        }
                    }
                }
                Entry::WhenGenerator(wgen) => {
                    let cond = self.eval_expr(&wgen.condition, &child_scope, depth)?;
                    if is_truthy(&cond) {
                        let body_val = self.eval_entries(&wgen.body, &child_scope, depth)?;
                        if let Value::Object(m) = body_val {
                            map.extend(m);
                        }
                    } else if let Some(else_body) = &wgen.else_body {
                        let else_val = self.eval_entries(else_body, &child_scope, depth)?;
                        if let Value::Object(m) = else_val {
                            map.extend(m);
                        }
                    }
                }
            }
        }
        Ok(Value::Object(map))
    }

    fn eval_expr(&mut self, expr: &Expr, scope: &Scope, depth: usize) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        match expr {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::String(s) => Ok(Value::String(s.clone())),
            Expr::Ident(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| Error::Eval(format!("undefined variable: {name}"))),
            Expr::New(type_name, entries) => {
                match type_name.as_deref() {
                    Some("Listing") => {
                        let mut items = Vec::new();
                        for entry in entries {
                            match entry {
                                Entry::Property(p) if p.value.is_some() => {
                                    items.push(self.eval_expr(
                                        p.value.as_ref().unwrap(),
                                        scope,
                                        depth + 1,
                                    )?);
                                }
                                Entry::Spread(e) => {
                                    let v = self.eval_expr(e, scope, depth + 1)?;
                                    match v {
                                        Value::List(l) => items.extend(l),
                                        Value::Object(m) => items.extend(m.into_values()),
                                        other => items.push(other),
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(Value::List(items))
                    }
                    Some("Mapping") | Some("Map") => {
                        let mut map = IndexMap::new();
                        self.eval_mapping_entries(entries, scope, depth, &mut map)?;
                        Ok(Value::Object(map))
                    }
                    _ => {
                        // Generic new: treat as object
                        self.eval_entries(entries, scope, depth + 1)
                    }
                }
            }
            Expr::ObjectBody(entries) => self.eval_entries(entries, scope, depth + 1),
            Expr::Field(obj_expr, field) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
                match &obj {
                    Value::Object(map) => map
                        .get(field)
                        .cloned()
                        .ok_or_else(|| Error::Eval(format!("field not found: {field}"))),
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on non-object"
                    ))),
                }
            }
            Expr::Index(obj_expr, key_expr) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1)?;
                let key = self.eval_expr(key_expr, scope, depth + 1)?;
                let key_str = value_to_key(&key)?;
                match obj {
                    Value::Object(map) => map
                        .get(&key_str)
                        .cloned()
                        .ok_or_else(|| Error::Eval(format!("key not found: {key_str}"))),
                    _ => Err(Error::Eval("cannot index non-object".into())),
                }
            }
            Expr::Call(func_expr, args) => self.eval_call(func_expr, args, scope, depth),
            Expr::If(cond, then_expr, else_expr) => {
                let c = self.eval_expr(cond, scope, depth + 1)?;
                if is_truthy(&c) {
                    self.eval_expr(then_expr, scope, depth + 1)
                } else {
                    self.eval_expr(else_expr, scope, depth + 1)
                }
            }
            Expr::Let(name, val_expr, body_expr) => {
                let val = self.eval_expr(val_expr, scope, depth + 1)?;
                let mut child = scope.child();
                child.set(name.clone(), val);
                self.eval_expr(body_expr, &child, depth + 1)
            }
            Expr::Binop(op, left, right) => self.eval_binop(*op, left, right, scope, depth),
            Expr::Unop(op, operand) => {
                let v = self.eval_expr(operand, scope, depth + 1)?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => Err(Error::Eval("cannot negate non-number".into())),
                    },
                    UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
                }
            }
            Expr::Is(expr, _ty) => {
                // Simplified: just evaluate the expression, ignore type check
                self.eval_expr(expr, scope, depth + 1)
            }
            Expr::As(expr, _ty) => self.eval_expr(expr, scope, depth + 1),
            Expr::Throw(msg_expr) => {
                let msg = self.eval_expr(msg_expr, scope, depth + 1)?;
                Err(Error::Eval(format!("throw: {}", value_to_display(&msg))))
            }
            Expr::Trace(expr) => {
                let v = self.eval_expr(expr, scope, depth + 1)?;
                eprintln!("[pklr trace] {}", value_to_display(&v));
                Ok(v)
            }
            Expr::Read(uri_expr) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1)?;
                Err(Error::Unsupported(format!(
                    "read() not supported: {}",
                    value_to_display(&uri)
                )))
            }
        }
    }

    fn eval_call(
        &mut self,
        func_expr: &Expr,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Handle built-in functions: List(), Listing(), Map()
        if let Expr::Ident(name) = func_expr {
            match name.as_str() {
                "List" | "Listing" => {
                    let items: Result<Vec<_>> = args
                        .iter()
                        .map(|a| self.eval_expr(a, scope, depth + 1))
                        .collect();
                    return Ok(Value::List(items?));
                }
                "Set" => {
                    let items: Result<Vec<_>> = args
                        .iter()
                        .map(|a| self.eval_expr(a, scope, depth + 1))
                        .collect();
                    return Ok(Value::List(items?)); // treat Set as List
                }
                "Map" => {
                    // Map(k1, v1, k2, v2, ...)
                    let mut map = IndexMap::new();
                    let evaled: Result<Vec<_>> = args
                        .iter()
                        .map(|a| self.eval_expr(a, scope, depth + 1))
                        .collect();
                    let evaled = evaled?;
                    for pair in evaled.chunks(2) {
                        if let [k, v] = pair {
                            map.insert(value_to_key(k)?, v.clone());
                        }
                    }
                    return Ok(Value::Object(map));
                }
                _ => {}
            }
        }

        // Object amendment: `(Base) { overrides }` is parsed as Binop(Add, base, ObjectBody)
        // Handle as a call on an object-returning expression
        let func_val = self.eval_expr(func_expr, scope, depth + 1)?;
        // If it's a plain call with no args on an object, return the object
        if args.is_empty() {
            return Ok(func_val);
        }
        Err(Error::Eval("cannot call non-function".into()))
    }

    fn eval_binop(
        &mut self,
        op: BinOp,
        left: &Expr,
        right: &Expr,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Special case: object amendment `base + ObjectBody(entries)`
        if let BinOp::Add = op
            && let Expr::ObjectBody(entries) = right
        {
            let base = self.eval_expr(left, scope, depth + 1)?;
            let overlay = self.eval_entries(entries, scope, depth + 1)?;
            return Ok(merge_values(base, overlay));
        }

        let l = self.eval_expr(left, scope, depth + 1)?;
        let r = self.eval_expr(right, scope, depth + 1)?;
        match op {
            BinOp::Add => add_values(l, r),
            BinOp::Sub => arithmetic(l, r, |a, b| Ok(a - b), |a, b| Ok(a - b)),
            BinOp::Mul => arithmetic(l, r, |a, b| Ok(a * b), |a, b| Ok(a * b)),
            BinOp::Div => arithmetic(
                l,
                r,
                |a, b| {
                    if b == 0 {
                        Err(Error::Eval("division by zero".into()))
                    } else {
                        Ok(a / b)
                    }
                },
                |a, b| Ok(a / b),
            ),
            BinOp::Mod => arithmetic(
                l,
                r,
                |a, b| {
                    if b == 0 {
                        Err(Error::Eval("modulo by zero".into()))
                    } else {
                        Ok(a % b)
                    }
                },
                |a, b| Ok(a % b),
            ),
            BinOp::Eq => Ok(Value::Bool(values_eq(&l, &r))),
            BinOp::Ne => Ok(Value::Bool(!values_eq(&l, &r))),
            BinOp::Lt => compare(l, r, std::cmp::Ordering::Less),
            BinOp::Le => compare_or_eq(l, r, std::cmp::Ordering::Less),
            BinOp::Gt => compare(l, r, std::cmp::Ordering::Greater),
            BinOp::Ge => compare_or_eq(l, r, std::cmp::Ordering::Greater),
            BinOp::And => Ok(Value::Bool(is_truthy(&l) && is_truthy(&r))),
            BinOp::Or => Ok(Value::Bool(is_truthy(&l) || is_truthy(&r))),
            BinOp::NullCoalesce => {
                if matches!(l, Value::Null) {
                    Ok(r)
                } else {
                    Ok(l)
                }
            }
        }
    }

    fn eval_mapping_entries(
        &mut self,
        entries: &[crate::parser::Entry],
        scope: &Scope,
        depth: usize,
        map: &mut IndexMap<String, Value>,
    ) -> Result<()> {
        for entry in entries {
            match entry {
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, scope, depth + 1)?;
                    let val = self.eval_expr(val_expr, scope, depth + 1)?;
                    map.insert(value_to_key(&key)?, val);
                }
                Entry::Property(prop)
                    if prop
                        .modifiers
                        .iter()
                        .any(|m| matches!(m, crate::parser::Modifier::Local)) =>
                {
                    // skip locals in mapping
                }
                Entry::Spread(e) => {
                    let v = self.eval_expr(e, scope, depth + 1)?;
                    if let Value::Object(m) = v {
                        map.extend(m);
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self.eval_expr(&fgen.collection, scope, depth + 1)?;
                    for (k, v) in collection_to_items(collection) {
                        let mut iter_scope = scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(kv) = &fgen.key_var {
                            iter_scope.set(kv.clone(), k);
                        }
                        self.eval_mapping_entries(&fgen.body, &iter_scope, depth + 1, map)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

// --- Scope ---

#[derive(Debug, Default, Clone)]
struct Scope {
    vars: IndexMap<String, Value>,
    parent: Option<Box<Scope>>,
}

impl Scope {
    fn child(&self) -> Self {
        Self {
            vars: IndexMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    fn set(&mut self, name: String, val: Value) {
        self.vars.insert(name, val);
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.vars
            .get(name)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get(name)))
    }
}

// --- Helpers ---

fn value_to_key(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(Error::Eval("mapping key must be a string or int".into())),
    }
}

fn value_to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => s.clone(),
        _ => format!("{v:?}"),
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
        (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

fn add_values(l: Value, r: Value) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
        (Value::String(a), Value::String(b)) => Ok(Value::String(a + &b)),
        (Value::List(mut a), Value::List(b)) => {
            a.extend(b);
            Ok(Value::List(a))
        }
        (Value::Object(mut a), Value::Object(b)) => {
            a.extend(b);
            Ok(Value::Object(a))
        }
        (l, r) => Err(Error::Eval(format!("cannot add {:?} and {:?}", l, r))),
    }
}

fn arithmetic(
    l: Value,
    r: Value,
    fi: impl Fn(i64, i64) -> Result<i64>,
    ff: impl Fn(f64, f64) -> Result<f64>,
) -> Result<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(fi(a, b)?)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(ff(a, b)?)),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(ff(a as f64, b)?)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(ff(a, b as f64)?)),
        (l, r) => Err(Error::Eval(format!(
            "arithmetic type mismatch: {:?} vs {:?}",
            l, r
        ))),
    }
}

fn compare(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    Ok(Value::Bool(value_cmp(&l, &r)? == ord))
}

fn compare_or_eq(l: Value, r: Value, ord: std::cmp::Ordering) -> Result<Value> {
    let c = value_cmp(&l, &r)?;
    Ok(Value::Bool(c == ord || c == std::cmp::Ordering::Equal))
}

fn value_cmp(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => {
            Ok(x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
        }
        (Value::Int(x), Value::Float(y)) => Ok((*x as f64)
            .partial_cmp(y)
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::Float(x), Value::Int(y)) => Ok(x
            .partial_cmp(&(*y as f64))
            .unwrap_or(std::cmp::Ordering::Equal)),
        (Value::String(x), Value::String(y)) => Ok(x.cmp(y)),
        _ => Err(Error::Eval(format!("cannot compare {:?} and {:?}", a, b))),
    }
}

fn merge_values(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut b), Value::Object(o)) => {
            b.extend(o);
            Value::Object(b)
        }
        (_, overlay) => overlay,
    }
}

fn collection_to_items(v: Value) -> Vec<(Value, Value)> {
    match v {
        Value::List(items) => items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Value::Int(i as i64), v))
            .collect(),
        Value::Object(map) => map
            .into_iter()
            .map(|(k, v)| (Value::String(k), v))
            .collect(),
        _ => vec![],
    }
}
