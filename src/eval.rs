use std::collections::HashMap;

use async_recursion::async_recursion;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{self, BinOp, Entry, Expr, Modifier, Module, Property, StringInterpPart, UnOp};
use crate::value::Value;

/// Evaluates pkl source files to [`Value`].
pub struct Evaluator {
    base_path: PathBuf,
    /// Maximum import depth to prevent infinite recursion
    max_depth: usize,
    /// Cache for fetched HTTP sources (URL → source text)
    http_cache: HashMap<String, String>,
    /// Reusable HTTP client for connection pooling
    http_client: reqwest::Client,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self {
            base_path: PathBuf::from("."),
            max_depth: 32,
            http_cache: HashMap::new(),
            http_client: reqwest::Client::new(),
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

    async fn fetch_source(&mut self, url: &str) -> Result<String> {
        if let Some(cached) = self.http_cache.get(url) {
            return Ok(cached.clone());
        }
        let body = self
            .http_client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Eval(format!("HTTP fetch failed for {url}: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Eval(format!("HTTP error for {url}: {e}")))?
            .text()
            .await
            .map_err(|e| Error::Eval(format!("HTTP read failed for {url}: {e}")))?;
        self.http_cache.insert(url.to_string(), body.clone());
        Ok(body)
    }

    pub async fn eval_source(&mut self, source: &str, path: &Path) -> Result<Value> {
        let name = path.display().to_string();
        let tokens = lexer::lex_named(source, &name)?;
        let module = parser::parse_named(&tokens, source, &name)?;
        self.eval_module(&module, path, 0).await
    }

    #[async_recursion(?Send)]
    async fn eval_module(&mut self, module: &Module, path: &Path, depth: usize) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval(format!(
                "max import depth {} exceeded",
                self.max_depth
            )));
        }
        let mut scope = Scope::default();

        // Process imports
        for import in &module.imports {
            let uri = &import.uri;

            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP import
                let source = self.fetch_source(uri).await?;
                let imported_val = {
                    let tokens = lexer::lex_named(&source, uri)?;
                    let imp_module = parser::parse_named(&tokens, &source, uri)?;
                    self.eval_module(&imp_module, Path::new(uri), depth + 1)
                        .await?
                };
                let alias = import.alias.clone().unwrap_or_else(|| {
                    uri.rsplit('/')
                        .next()
                        .unwrap_or(uri)
                        .strip_suffix(".pkl")
                        .unwrap_or(uri)
                        .to_string()
                });
                scope.set(alias, imported_val);
                continue;
            }

            if uri.starts_with("package://") {
                // Convert package URI to GitHub release URL
                // Format: package://pkg.pkl-lang.org/github.com/owner/repo@version#/path.pkl
                // → https://github.com/owner/repo/releases/download/version/path.pkl
                if let Some(rest) = uri.strip_prefix("package://pkg.pkl-lang.org/github.com/")
                    && let Some((repo_ver, fragment)) = rest.split_once('#')
                    && let Some((repo, version)) = repo_ver.split_once('@')
                {
                    let file_path = fragment.strip_prefix('/').unwrap_or(fragment);
                    let url = format!(
                        "https://github.com/{repo}/releases/download/{version}/{file_path}"
                    );
                    let source = self.fetch_source(&url).await?;
                    let imported_val = {
                        let tokens = lexer::lex_named(&source, &url)?;
                        let imp_module = parser::parse_named(&tokens, &source, &url)?;
                        self.eval_module(&imp_module, Path::new(&url), depth + 1)
                            .await?
                    };
                    let alias = import.alias.clone().unwrap_or_else(|| {
                        file_path
                            .rsplit('/')
                            .next()
                            .unwrap_or(file_path)
                            .strip_suffix(".pkl")
                            .unwrap_or(file_path)
                            .to_string()
                    });
                    scope.set(alias, imported_val);
                } else {
                    return Err(Error::Eval(format!(
                        "unsupported package URI (only pkg.pkl-lang.org/github.com is supported): {uri}"
                    )));
                }
                continue;
            }

            // Skip other non-local imports
            if uri.contains("://") && !uri.starts_with("file://") {
                continue;
            }

            let import_path = if let Some(rel) = uri.strip_prefix("file://") {
                PathBuf::from(rel)
            } else {
                let base = path.parent().unwrap_or(Path::new("."));
                base.join(uri)
            };
            if !import_path.exists() {
                return Err(Error::ImportNotFound(import_path.display().to_string()));
            }
            {
                let source = std::fs::read_to_string(&import_path)
                    .map_err(|e| Error::Io(import_path.clone(), e))?;
                let imported_val = {
                    let name = import_path.display().to_string();
                    let tokens = lexer::lex_named(&source, &name)?;
                    let imp_module = parser::parse_named(&tokens, &source, &name)?;
                    self.eval_module(&imp_module, &import_path, depth + 1)
                        .await?
                };
                // Determine the binding name: alias or filename stem
                let alias = import.alias.clone().unwrap_or_else(|| {
                    import_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                });
                scope.set(alias, imported_val);
            }
        }

        // Process amends: load base module as starting values
        let mut base_obj = IndexMap::new();
        if let Some(uri) = &module.amends {
            if uri.starts_with("https://") || uri.starts_with("http://") {
                // HTTP amends
                let source = self.fetch_source(uri).await?;
                let tokens = lexer::lex_named(&source, uri)?;
                let base_module = parser::parse_named(&tokens, &source, uri)?;
                let base_val = self
                    .eval_module(&base_module, Path::new(uri), depth + 1)
                    .await?;
                if let Value::Object(m) = base_val {
                    base_obj = m;
                }
            } else if uri.starts_with("package://") {
                // Convert package URI to GitHub release URL
                if let Some(rest) = uri.strip_prefix("package://pkg.pkl-lang.org/github.com/")
                    && let Some((repo_ver, fragment)) = rest.split_once('#')
                    && let Some((repo, version)) = repo_ver.split_once('@')
                {
                    let file_path = fragment.strip_prefix('/').unwrap_or(fragment);
                    let url = format!(
                        "https://github.com/{repo}/releases/download/{version}/{file_path}"
                    );
                    let source = self.fetch_source(&url).await?;
                    let tokens = lexer::lex_named(&source, &url)?;
                    let base_module = parser::parse_named(&tokens, &source, &url)?;
                    let base_val = self
                        .eval_module(&base_module, Path::new(&url), depth + 1)
                        .await?;
                    if let Value::Object(m) = base_val {
                        base_obj = m;
                    }
                } else {
                    return Err(Error::Eval(format!(
                        "unsupported package URI (only pkg.pkl-lang.org/github.com is supported): {uri}"
                    )));
                }
            } else if !uri.contains("://") || uri.starts_with("file://") {
                let amends_path = if let Some(rel) = uri.strip_prefix("file://") {
                    PathBuf::from(rel)
                } else {
                    let base = path.parent().unwrap_or(Path::new("."));
                    base.join(uri)
                };
                if amends_path.exists() {
                    let source = std::fs::read_to_string(&amends_path)
                        .map_err(|e| Error::Io(amends_path.clone(), e))?;
                    let name = amends_path.display().to_string();
                    let tokens = lexer::lex_named(&source, &name)?;
                    let base_module = parser::parse_named(&tokens, &source, &name)?;
                    let base_val = self
                        .eval_module(&base_module, &amends_path, depth + 1)
                        .await?;
                    if let Value::Object(m) = base_val {
                        base_obj = m;
                    }
                }
            }
        }

        // First pass: collect all `local` variable definitions into scope
        for entry in &module.body {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
            {
                let val = self.eval_expr(expr, &scope, depth).await?;
                scope.set(prop.name.clone(), val);
            }
        }
        // Collect class definitions into module scope (after locals so defaults can reference them)
        for entry in &module.body {
            if let Entry::ClassDef(name, body) = entry {
                let defaults = self.eval_entries(body, &scope, depth + 1).await?;
                scope.set(name.clone(), defaults);
            }
        }

        // Second pass: evaluate non-local entries into output object
        let mut out = base_obj;
        for entry in &module.body {
            if let Entry::Property(prop) = entry {
                let mods = &prop.modifiers;
                if has_modifier(mods, Modifier::Local) {
                    continue; // already collected
                }
                // abstract/external properties must have a value (or be overridden)
                if (has_modifier(mods, Modifier::Abstract)
                    || has_modifier(mods, Modifier::External))
                    && prop.value.is_none()
                    && prop.body.is_none()
                {
                    if !out.contains_key(&prop.name) {
                        let kind = if has_modifier(mods, Modifier::Abstract) {
                            "abstract"
                        } else {
                            "external"
                        };
                        return Err(Error::Eval(format!(
                            "{kind} property '{}' must be assigned a value",
                            prop.name
                        )));
                    }
                    continue;
                }
                let val = self.eval_property(prop, &scope, depth).await?;
                if let Some(v) = val {
                    // const/fixed: error if overriding an immutable property from base
                    if (has_modifier(mods, Modifier::Const) || has_modifier(mods, Modifier::Fixed))
                        && out.contains_key(&prop.name)
                    {
                        let kind = if has_modifier(mods, Modifier::Const) {
                            "const"
                        } else {
                            "fixed"
                        };
                        return Err(Error::Eval(format!(
                            "cannot override {kind} property '{}'",
                            prop.name
                        )));
                    }
                    // Always add to scope so other properties can reference it
                    scope.set(prop.name.clone(), v.clone());
                    if !has_modifier(mods, Modifier::Hidden) {
                        out.insert(prop.name.clone(), v);
                    }
                }
            }
        }

        Ok(Value::Object(out))
    }

    #[async_recursion(?Send)]
    async fn eval_property(
        &mut self,
        prop: &Property,
        scope: &Scope,
        depth: usize,
    ) -> Result<Option<Value>> {
        if let Some(expr) = &prop.value {
            return Ok(Some(self.eval_expr(expr, scope, depth).await?));
        }
        if let Some(body) = &prop.body {
            // `foo { ... }` — object body amendment
            let val = self.eval_entries(body, scope, depth).await?;
            return Ok(Some(val));
        }
        Ok(None) // bare type-only declaration
    }

    #[async_recursion(?Send)]
    async fn eval_entries(
        &mut self,
        entries: &[Entry],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        let mut child_scope = scope.child();
        // Set `outer` to a snapshot of the parent scope's variables as an object
        let outer_obj = Value::Object(scope.flatten());
        child_scope.set("outer".into(), outer_obj);
        // First pass: collect locals
        for entry in entries {
            if let Entry::Property(prop) = entry
                && has_modifier(&prop.modifiers, Modifier::Local)
                && let Some(expr) = &prop.value
            {
                let val = self.eval_expr(expr, &child_scope, depth).await?;
                child_scope.set(prop.name.clone(), val);
            }
        }
        // Collect class definitions into scope (after locals so defaults can reference them)
        for entry in entries {
            if let Entry::ClassDef(name, body) = entry {
                let defaults = self.eval_entries(body, &child_scope, depth + 1).await?;
                child_scope.set(name.clone(), defaults);
            }
        }

        let mut map: IndexMap<String, Value> = IndexMap::new();
        for entry in entries {
            match entry {
                Entry::Property(prop) => {
                    let mods = &prop.modifiers;
                    if has_modifier(mods, Modifier::Local) {
                        continue;
                    }
                    if has_modifier(mods, Modifier::Abstract)
                        && prop.value.is_none()
                        && prop.body.is_none()
                    {
                        continue; // abstract without value — skip (must be overridden)
                    }
                    if let Some(v) = self.eval_property(prop, &child_scope, depth).await? {
                        child_scope.set(prop.name.clone(), v.clone());
                        if !has_modifier(mods, Modifier::Hidden) {
                            map.insert(prop.name.clone(), v);
                        }
                    }
                }
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, &child_scope, depth).await?;
                    let val = self.eval_expr(val_expr, &child_scope, depth).await?;
                    let key_str = value_to_key(&key)?;
                    map.insert(key_str, val);
                }
                Entry::Spread(expr) => {
                    let val = self.eval_expr(expr, &child_scope, depth).await?;
                    if let Value::Object(m) = val {
                        map.extend(m);
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self
                        .eval_expr(&fgen.collection, &child_scope, depth)
                        .await?;
                    let items = collection_to_items(collection);
                    for (k, v) in items {
                        let mut iter_scope = child_scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(key_var) = &fgen.key_var {
                            iter_scope.set(key_var.clone(), k);
                        }
                        let body_val = self.eval_entries(&fgen.body, &iter_scope, depth).await?;
                        if let Value::Object(m) = body_val {
                            map.extend(m);
                        }
                    }
                }
                Entry::WhenGenerator(wgen) => {
                    let cond = self.eval_expr(&wgen.condition, &child_scope, depth).await?;
                    if is_truthy(&cond) {
                        let body_val = self.eval_entries(&wgen.body, &child_scope, depth).await?;
                        if let Value::Object(m) = body_val {
                            map.extend(m);
                        }
                    } else if let Some(else_body) = &wgen.else_body {
                        let else_val = self.eval_entries(else_body, &child_scope, depth).await?;
                        if let Value::Object(m) = else_val {
                            map.extend(m);
                        }
                    }
                }
                Entry::Elem(_) => {} // bare elements only valid in Listing bodies
                Entry::ClassDef(..) => {} // handled in scope setup
            }
        }
        Ok(Value::Object(map))
    }

    #[async_recursion(?Send)]
    async fn eval_expr(&mut self, expr: &Expr, scope: &Scope, depth: usize) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::Eval("maximum recursion depth exceeded".into()));
        }
        match expr {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::String(s) => Ok(Value::String(s.clone())),
            Expr::StringInterpolation(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        StringInterpPart::Literal(s) => result.push_str(s),
                        StringInterpPart::Expr(e) => {
                            let val = self.eval_expr(e, scope, depth + 1).await?;
                            result.push_str(&value_to_display(&val));
                        }
                    }
                }
                Ok(Value::String(result))
            }
            Expr::Ident(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| Error::Eval(format!("undefined variable: {name}"))),
            Expr::Lambda(params, body) => {
                // Capture current scope values
                let captured = scope.flatten();
                Ok(Value::Lambda(params.clone(), (**body).clone(), captured))
            }
            Expr::New(type_name, entries) => {
                match type_name.as_deref() {
                    Some("Listing") => {
                        let mut items = Vec::new();
                        for entry in entries {
                            match entry {
                                Entry::Elem(e) => {
                                    items.push(self.eval_expr(e, scope, depth + 1).await?);
                                }
                                Entry::Property(p) if p.value.is_some() => {
                                    items.push(
                                        self.eval_expr(p.value.as_ref().unwrap(), scope, depth + 1)
                                            .await?,
                                    );
                                }
                                Entry::Spread(e) => {
                                    let v = self.eval_expr(e, scope, depth + 1).await?;
                                    match v {
                                        Value::List(l) => items.extend(l),
                                        Value::Object(m) => items.extend(m.into_values()),
                                        other => items.push(other),
                                    }
                                }
                                Entry::ForGenerator(fgen) => {
                                    let collection =
                                        self.eval_expr(&fgen.collection, scope, depth + 1).await?;
                                    for (k, v) in collection_to_items(collection) {
                                        let mut iter_scope = scope.child();
                                        iter_scope.set(fgen.val_var.clone(), v);
                                        if let Some(key_var) = &fgen.key_var {
                                            iter_scope.set(key_var.clone(), k);
                                        }
                                        for sub in &fgen.body {
                                            if let Entry::Elem(e) = sub {
                                                items.push(
                                                    self.eval_expr(e, &iter_scope, depth + 1)
                                                        .await?,
                                                );
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(Value::List(items))
                    }
                    Some("Mapping") | Some("Map") => {
                        let mut map = IndexMap::new();
                        self.eval_mapping_entries(entries, scope, depth, &mut map)
                            .await?;
                        Ok(Value::Object(map))
                    }
                    _ => {
                        // Check if type name matches a class in scope
                        let base = type_name.as_ref().and_then(|name| scope.get(name)).cloned();
                        let overlay = self.eval_entries(entries, scope, depth + 1).await?;
                        if let Some(Value::Object(base_map)) = base {
                            let mut merged = base_map;
                            if let Value::Object(overlay_map) = overlay {
                                merged.extend(overlay_map);
                            }
                            Ok(Value::Object(merged))
                        } else {
                            Ok(overlay)
                        }
                    }
                }
            }
            Expr::ObjectBody(entries) => self.eval_entries(entries, scope, depth + 1).await,
            Expr::Field(obj_expr, field) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
                // Built-in properties
                match (&obj, field.as_str()) {
                    (Value::List(items), "length") => return Ok(Value::Int(items.len() as i64)),
                    (Value::List(items), "isEmpty") => return Ok(Value::Bool(items.is_empty())),
                    (Value::List(items), "first") => {
                        return items
                            .first()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::List(items), "last") => {
                        return items
                            .last()
                            .cloned()
                            .ok_or_else(|| Error::Eval("empty list".into()));
                    }
                    (Value::String(s), "length") => {
                        return Ok(Value::Int(s.chars().count() as i64));
                    }
                    (Value::String(s), "isEmpty") => return Ok(Value::Bool(s.is_empty())),
                    (Value::Object(map), "length") => return Ok(Value::Int(map.len() as i64)),
                    (Value::Object(map), "isEmpty") => return Ok(Value::Bool(map.is_empty())),
                    (Value::Object(map), "keys") => {
                        return Ok(Value::List(
                            map.keys().map(|k| Value::String(k.clone())).collect(),
                        ));
                    }
                    (Value::Object(map), "values") => {
                        return Ok(Value::List(map.values().cloned().collect()));
                    }
                    // Duration and DataSize units on numbers
                    (
                        Value::Int(_) | Value::Float(_),
                        "ns" | "us" | "ms" | "s" | "min" | "h" | "d" | "b" | "kb" | "mb" | "gb"
                        | "tb" | "pb" | "kib" | "mib" | "gib" | "tib" | "pib",
                    ) => {
                        return Ok(make_unit_object(obj, field));
                    }
                    _ => {}
                }
                match &obj {
                    Value::Object(map) => map
                        .get(field)
                        .cloned()
                        .ok_or_else(|| Error::Eval(format!("field not found: {field}"))),
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::NullSafeField(obj_expr, field) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
                match &obj {
                    Value::Null => Ok(Value::Null),
                    Value::Object(map) => Ok(map.get(field).cloned().unwrap_or(Value::Null)),
                    _ => Err(Error::Eval(format!(
                        "cannot access field '{field}' on {}",
                        value_type_name(&obj)
                    ))),
                }
            }
            Expr::Index(obj_expr, key_expr) => {
                let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
                let key = self.eval_expr(key_expr, scope, depth + 1).await?;
                let key_str = value_to_key(&key)?;
                match obj {
                    Value::Object(map) => map
                        .get(&key_str)
                        .cloned()
                        .ok_or_else(|| Error::Eval(format!("key not found: {key_str}"))),
                    _ => Err(Error::Eval("cannot index non-object".into())),
                }
            }
            Expr::Call(func_expr, args) => self.eval_call(func_expr, args, scope, depth).await,
            Expr::If(cond, then_expr, else_expr) => {
                let c = self.eval_expr(cond, scope, depth + 1).await?;
                if is_truthy(&c) {
                    self.eval_expr(then_expr, scope, depth + 1).await
                } else {
                    self.eval_expr(else_expr, scope, depth + 1).await
                }
            }
            Expr::Let(name, val_expr, body_expr) => {
                let val = self.eval_expr(val_expr, scope, depth + 1).await?;
                let mut child = scope.child();
                child.set(name.clone(), val);
                self.eval_expr(body_expr, &child, depth + 1).await
            }
            Expr::Binop(op, left, right) => self.eval_binop(*op, left, right, scope, depth).await,
            Expr::Unop(op, operand) => {
                let v = self.eval_expr(operand, scope, depth + 1).await?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => Err(Error::Eval("cannot negate non-number".into())),
                    },
                    UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
                    UnOp::NonNull => {
                        if matches!(v, Value::Null) {
                            Err(Error::Eval(
                                "non-null assertion failed: value is null".into(),
                            ))
                        } else {
                            Ok(v)
                        }
                    }
                }
            }
            Expr::Is(expr, _ty) => {
                // Simplified: just evaluate the expression, ignore type check
                self.eval_expr(expr, scope, depth + 1).await
            }
            Expr::As(expr, _ty) => self.eval_expr(expr, scope, depth + 1).await,
            Expr::Throw(msg_expr) => {
                let msg = self.eval_expr(msg_expr, scope, depth + 1).await?;
                Err(Error::Eval(format!("throw: {}", value_to_display(&msg))))
            }
            Expr::Trace(expr) => {
                let v = self.eval_expr(expr, scope, depth + 1).await?;
                eprintln!("[pklr trace] {}", value_to_display(&v));
                Ok(v)
            }
            Expr::Read(uri_expr) => {
                let uri = self.eval_expr(uri_expr, scope, depth + 1).await?;
                Err(Error::Unsupported(format!(
                    "read() not supported: {}",
                    value_to_display(&uri)
                )))
            }
        }
    }

    #[async_recursion(?Send)]
    async fn eval_call(
        &mut self,
        func_expr: &Expr,
        args: &[Expr],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value> {
        // Handle method calls: obj.method(args)
        if let Expr::Field(obj_expr, method) = func_expr {
            let obj = self.eval_expr(obj_expr, scope, depth + 1).await?;
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1).await?);
            }
            if let Some(result) = self
                .eval_method_call(&obj, method, &evaled_args, depth)
                .await?
            {
                return Ok(result);
            }
        }

        // Handle built-in functions: List(), Listing(), Map()
        if let Expr::Ident(name) = func_expr {
            match name.as_str() {
                "List" | "Listing" => {
                    let mut items = Vec::new();
                    for a in args {
                        items.push(self.eval_expr(a, scope, depth + 1).await?);
                    }
                    return Ok(Value::List(items));
                }
                "Set" => {
                    let mut items = Vec::new();
                    for a in args {
                        items.push(self.eval_expr(a, scope, depth + 1).await?);
                    }
                    return Ok(Value::List(items)); // treat Set as List
                }
                "Map" => {
                    // Map(k1, v1, k2, v2, ...)
                    let mut map = IndexMap::new();
                    let mut evaled = Vec::new();
                    for a in args {
                        evaled.push(self.eval_expr(a, scope, depth + 1).await?);
                    }
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

        // Evaluate the function expression
        let func_val = self.eval_expr(func_expr, scope, depth + 1).await?;

        // Lambda call
        if let Value::Lambda(params, body, captured) = func_val {
            let mut call_scope = Scope::default();
            // Restore captured scope
            for (k, v) in captured {
                call_scope.set(k, v);
            }
            // Bind arguments to parameters
            let mut evaled_args = Vec::new();
            for a in args {
                evaled_args.push(self.eval_expr(a, scope, depth + 1).await?);
            }
            for (param, arg) in params.iter().zip(evaled_args) {
                call_scope.set(param.clone(), arg);
            }
            return self.eval_expr(&body, &call_scope, depth + 1).await;
        }

        // Plain call with no args on an object — return the object
        if args.is_empty() {
            return Ok(func_val);
        }
        Err(Error::Eval("cannot call non-function".into()))
    }

    #[async_recursion(?Send)]
    async fn eval_method_call(
        &mut self,
        obj: &Value,
        method: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        match (obj, method) {
            // String methods
            (Value::String(s), "contains") => {
                let arg = require_str_arg(args, 0, "contains")?;
                Ok(Some(Value::Bool(s.contains(arg))))
            }
            (Value::String(s), "startsWith") => {
                let arg = require_str_arg(args, 0, "startsWith")?;
                Ok(Some(Value::Bool(s.starts_with(arg))))
            }
            (Value::String(s), "endsWith") => {
                let arg = require_str_arg(args, 0, "endsWith")?;
                Ok(Some(Value::Bool(s.ends_with(arg))))
            }
            (Value::String(s), "replaceAll") => {
                let from = require_str_arg(args, 0, "replaceAll")?;
                let to = require_str_arg(args, 1, "replaceAll")?;
                Ok(Some(Value::String(s.replace(from, to))))
            }
            (Value::String(s), "split") => {
                let sep = require_str_arg(args, 0, "split")?;
                Ok(Some(Value::List(
                    s.split(sep).map(|p| Value::String(p.to_string())).collect(),
                )))
            }
            (Value::String(s), "trim") => Ok(Some(Value::String(s.trim().to_string()))),
            (Value::String(s), "trimStart") => Ok(Some(Value::String(s.trim_start().to_string()))),
            (Value::String(s), "trimEnd") => Ok(Some(Value::String(s.trim_end().to_string()))),
            (Value::String(s), "toUpperCase") => Ok(Some(Value::String(s.to_uppercase()))),
            (Value::String(s), "toLowerCase") => Ok(Some(Value::String(s.to_lowercase()))),
            (Value::String(s), "toInt") => s
                .parse::<i64>()
                .map(|n| Some(Value::Int(n)))
                .map_err(|_| Error::Eval(format!("cannot convert '{s}' to Int"))),

            // List methods
            (Value::List(items), "contains") => {
                let arg = args.first().cloned().unwrap_or(Value::Null);
                Ok(Some(Value::Bool(items.contains(&arg))))
            }
            (Value::List(items), "toList") => Ok(Some(Value::List(items.clone()))),
            (Value::List(items), "toSet") => {
                let mut seen = Vec::new();
                for item in items {
                    if !seen.contains(item) {
                        seen.push(item.clone());
                    }
                }
                Ok(Some(Value::List(seen)))
            }
            (Value::List(items), "map") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("map requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    result.push(
                        self.invoke_lambda(lambda, std::slice::from_ref(item), depth)
                            .await?,
                    );
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "flatMap") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("flatMap requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    let val = self
                        .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                        .await?;
                    if let Value::List(inner) = val {
                        result.extend(inner);
                    } else {
                        result.push(val);
                    }
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "filter") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("filter requires a function argument".into()))?;
                let mut result = Vec::new();
                for item in items {
                    let cond = self
                        .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                        .await?;
                    if is_truthy(&cond) {
                        result.push(item.clone());
                    }
                }
                Ok(Some(Value::List(result)))
            }
            (Value::List(items), "fold") => {
                let init = args
                    .first()
                    .ok_or_else(|| Error::Eval("fold requires initial value".into()))?
                    .clone();
                let lambda = args
                    .get(1)
                    .ok_or_else(|| Error::Eval("fold requires a function argument".into()))?;
                let mut acc = init;
                for item in items {
                    acc = self
                        .invoke_lambda(lambda, &[acc, item.clone()], depth)
                        .await?;
                }
                Ok(Some(acc))
            }
            (Value::List(items), "any") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("any requires a function argument".into()))?;
                for item in items {
                    if is_truthy(
                        &self
                            .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                            .await?,
                    ) {
                        return Ok(Some(Value::Bool(true)));
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            (Value::List(items), "every") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("every requires a function argument".into()))?;
                for item in items {
                    if !is_truthy(
                        &self
                            .invoke_lambda(lambda, std::slice::from_ref(item), depth)
                            .await?,
                    ) {
                        return Ok(Some(Value::Bool(false)));
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            (Value::List(items), "join") => {
                let sep = args.first().and_then(|v| v.as_str()).unwrap_or(",");
                let s: Vec<String> = items.iter().map(value_to_display).collect();
                Ok(Some(Value::String(s.join(sep))))
            }
            (Value::List(items), "reverse") => {
                let mut rev = items.clone();
                rev.reverse();
                Ok(Some(Value::List(rev)))
            }

            // Object/Mapping methods
            (Value::Object(map), "containsKey") => {
                let key = args.first().and_then(|v| v.as_str()).unwrap_or("");
                Ok(Some(Value::Bool(map.contains_key(key))))
            }
            (Value::Object(map), "toMap") => Ok(Some(Value::Object(map.clone()))),
            (Value::Object(map), "mapValues") => {
                let lambda = args
                    .first()
                    .ok_or_else(|| Error::Eval("mapValues requires a function".into()))?;
                let mut result = IndexMap::new();
                for (k, v) in map {
                    let new_v = self
                        .invoke_lambda(lambda, &[Value::String(k.clone()), v.clone()], depth)
                        .await?;
                    result.insert(k.clone(), new_v);
                }
                Ok(Some(Value::Object(result)))
            }
            (Value::Object(_), "toList") | (Value::Object(_), "toDynamic") => Ok(Some(obj.clone())),

            // Int/Float methods
            (Value::Int(n), "toString") => Ok(Some(Value::String(n.to_string()))),
            (Value::Float(f), "toString") => Ok(Some(Value::String(f.to_string()))),
            (Value::Bool(b), "toString") => Ok(Some(Value::String(b.to_string()))),

            // Lambda.apply()
            (Value::Lambda(params, body, captured), "apply") => {
                let mut call_scope = Scope::default();
                for (k, v) in captured {
                    call_scope.set(k.clone(), v.clone());
                }
                for (param, arg) in params.iter().zip(args.iter()) {
                    call_scope.set(param.clone(), arg.clone());
                }
                Ok(Some(self.eval_expr(body, &call_scope, depth + 1).await?))
            }

            _ => Ok(None), // not a known method
        }
    }

    #[async_recursion(?Send)]
    async fn invoke_lambda(
        &mut self,
        lambda: &Value,
        args: &[Value],
        depth: usize,
    ) -> Result<Value> {
        if let Value::Lambda(params, body, captured) = lambda {
            let mut scope = Scope::default();
            for (k, v) in captured {
                scope.set(k.clone(), v.clone());
            }
            for (param, arg) in params.iter().zip(args.iter()) {
                scope.set(param.clone(), arg.clone());
            }
            self.eval_expr(body, &scope, depth + 1).await
        } else {
            Err(Error::Eval("expected a function".into()))
        }
    }

    #[async_recursion(?Send)]
    async fn eval_binop(
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
            let base = self.eval_expr(left, scope, depth + 1).await?;
            let overlay = self.eval_entries(entries, scope, depth + 1).await?;
            return Ok(merge_values(base, overlay));
        }

        let l = self.eval_expr(left, scope, depth + 1).await?;
        let r = self.eval_expr(right, scope, depth + 1).await?;
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
            BinOp::IntDiv => arithmetic(
                l,
                r,
                |a, b| {
                    if b == 0 {
                        Err(Error::Eval("division by zero".into()))
                    } else {
                        Ok(a / b)
                    }
                },
                |a, b| Ok((a / b).floor()),
            ),
            BinOp::Pow => arithmetic(
                l,
                r,
                |a, b| {
                    if b < 0 {
                        Err(Error::Eval(
                            "integer exponentiation with negative exponent is not supported".into(),
                        ))
                    } else {
                        Ok(a.pow(b as u32))
                    }
                },
                |a, b| Ok(a.powf(b)),
            ),
            BinOp::NullCoalesce => {
                if matches!(l, Value::Null) {
                    Ok(r)
                } else {
                    Ok(l)
                }
            }
            BinOp::Pipe => {
                // x |> f  is equivalent to  f(x)
                match r {
                    Value::Lambda(params, body, captured) => {
                        if params.len() != 1 {
                            return Err(Error::Eval(format!(
                                "pipe operator requires a single-parameter function, got {}",
                                params.len()
                            )));
                        }
                        let mut call_scope = Scope::default();
                        for (k, v) in captured {
                            call_scope.set(k, v);
                        }
                        call_scope.set(params[0].clone(), l);
                        self.eval_expr(&body, &call_scope, depth + 1).await
                    }
                    _ => Err(Error::Eval(
                        "pipe operator requires a function on the right side".into(),
                    )),
                }
            }
        }
    }

    #[async_recursion(?Send)]
    async fn eval_mapping_entries(
        &mut self,
        entries: &[crate::parser::Entry],
        scope: &Scope,
        depth: usize,
        map: &mut IndexMap<String, Value>,
    ) -> Result<()> {
        for entry in entries {
            match entry {
                Entry::DynProperty(key_expr, val_expr) => {
                    let key = self.eval_expr(key_expr, scope, depth + 1).await?;
                    let val = self.eval_expr(val_expr, scope, depth + 1).await?;
                    map.insert(value_to_key(&key)?, val);
                }
                Entry::Property(prop) if has_modifier(&prop.modifiers, Modifier::Local) => {
                    // skip locals in mapping
                }
                Entry::Spread(e) => {
                    let v = self.eval_expr(e, scope, depth + 1).await?;
                    if let Value::Object(m) = v {
                        map.extend(m);
                    }
                }
                Entry::ForGenerator(fgen) => {
                    let collection = self.eval_expr(&fgen.collection, scope, depth + 1).await?;
                    for (k, v) in collection_to_items(collection) {
                        let mut iter_scope = scope.child();
                        iter_scope.set(fgen.val_var.clone(), v);
                        if let Some(kv) = &fgen.key_var {
                            iter_scope.set(kv.clone(), k);
                        }
                        self.eval_mapping_entries(&fgen.body, &iter_scope, depth + 1, map)
                            .await?;
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

    fn flatten(&self) -> IndexMap<String, Value> {
        let mut result = self
            .parent
            .as_ref()
            .map(|p| p.flatten())
            .unwrap_or_default();
        result.extend(self.vars.clone());
        result
    }
}

// --- Helpers ---

fn has_modifier(mods: &[Modifier], target: Modifier) -> bool {
    mods.contains(&target)
}

fn require_str_arg<'a>(args: &'a [Value], idx: usize, method: &str) -> Result<&'a str> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(Error::Eval(format!(
            "{method}() requires a String argument, got {}",
            value_type_name(other)
        ))),
        None => Err(Error::Eval(format!(
            "{method}() requires {} argument(s)",
            idx + 1
        ))),
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "Boolean",
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::String(_) => "String",
        Value::Object(_) => "Object",
        Value::List(_) => "List",
        Value::Lambda(..) => "Function",
    }
}

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

fn make_unit_object(value: Value, unit: &str) -> Value {
    let mut map = IndexMap::new();
    map.insert("value".to_string(), value);
    map.insert("unit".to_string(), Value::String(unit.to_string()));
    Value::Object(map)
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
