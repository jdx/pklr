//! A module's `output` (`value`, `renderer`, `text`) and the `pkl:base`
//! classes behind it: the renderers, `RenderDirective` and `ModuleOutput`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::remote::resolve_remote_relative;
use super::render::{self, Converters, Invoke, Kind, RendererKind, Settings};
use super::types::resolve_dotted;
use super::{Evaluator, Scope, SourceScope, seed_builtins};
use crate::error::{Error, Result};
use crate::lexer;
use crate::parser::{self, Entry, Expr, Modifier, Module, Property, TypeExpr};
use crate::value::{ObjectMap, ObjectSource, Value};

/// The `pkl:base` classes pklr provides, as Pkl source. Their methods
/// (`renderDocument`, `renderValue`) are implemented natively.
const BASE_CLASSES: &str = r#"
open class ModuleOutput {}

class RenderDirective {
  text: String
}

class PcfRenderer {
  extension = "pcf"
  indent: String = "  "
  omitNullProperties: Boolean = false
  useCustomStringDelimiters: Boolean = false
  converters = new Mapping {}
  convertPropertyTransformers = new Mapping {}
}

class JsonRenderer {
  extension = "json"
  indent: String = "  "
  omitNullProperties: Boolean = true
  converters = new Mapping {}
  convertPropertyTransformers = new Mapping {}
}

class YamlRenderer {
  extension = "yaml"
  mode = "compat"
  indentWidth: Int = 2
  omitNullProperties: Boolean = true
  isStream: Boolean = false
  converters = new Mapping {}
  convertPropertyTransformers = new Mapping {}
}

class PListRenderer {
  extension = "plist"
  indent: String = "  "
  converters = new Mapping {}
  convertPropertyTransformers = new Mapping {}
}

class PropertiesRenderer {
  extension = "properties"
  omitNullProperties: Boolean = true
  restrictCharset: Boolean = false
  converters = new Mapping {}
  convertPropertyTransformers = new Mapping {}
}
"#;

/// Names of the classes defined by [`BASE_CLASSES`].
pub(super) const BUILTIN_CLASS_NAMES: &[&str] = &[
    "ModuleOutput",
    "RenderDirective",
    "PcfRenderer",
    "JsonRenderer",
    "YamlRenderer",
    "PropertiesRenderer",
    "PListRenderer",
];

/// A module that the top-level module amends or extends, for evaluating its
/// `output`.
#[derive(Clone)]
pub(super) struct BaseModule {
    path: PathBuf,
    /// Names the module binds lexically: locals, imports, classes and type
    /// aliases.
    lexical: Vec<String>,
}

/// The names `module` binds lexically: its locals, imports, classes and
/// type aliases.
fn lexical_names(module: &Module) -> Vec<String> {
    let imports = module.imports.iter().map(|import| {
        import.alias.clone().unwrap_or_else(|| {
            let last = import.uri.rsplit(['/', ':']).next().unwrap_or(&import.uri);
            last.strip_suffix(".pkl").unwrap_or(last).to_string()
        })
    });
    let members = module.body.iter().filter_map(|entry| match entry {
        Entry::Property(prop) if prop.modifiers.contains(&Modifier::Local) => {
            Some(prop.name.clone())
        }
        Entry::ClassDef(name, ..) | Entry::TypeAlias(name, _) => Some(name.clone()),
        _ => None,
    });
    imports.chain(members).collect()
}

/// `pkl:jsonnet`, as Pkl source. pklr keeps functions and classes in one
/// namespace, so the classes its functions build are named `...Class`.
const JSONNET_MODULE: &str = r#"
function ImportStr(_path: String) = new ImportStrClass { path = _path }

function ExtVar(_name: String) = new ExtVarClass { name = _name }

class Renderer {
  extension = "jsonnet"
  indent: String? = "  "
  omitNullProperties: Boolean = true
  converters = new Mapping {}
  convertPropertyTransformers = new Mapping {}
}

class ImportStrClass { path: String }
class ExtVarClass { name: String }
"#;

/// `pkl:json`, as Pkl source.
const JSON_MODULE: &str = r#"
class Parser {
  useMapping: Boolean = false
  converters = new Mapping {}
}
"#;

/// `pkl:xml`, as Pkl source.
const XML_MODULE: &str = r#"
class Renderer {
  extension = "xml"
  indent: String = "  "
  xmlVersion: String = "1.0"
  rootElementName: String = "root"
  rootElementAttributes = new Mapping {}
  converters = new Mapping {}
  convertPropertyTransformers = new Mapping {}
}

function Element(_name: String): Dynamic = new Dynamic {
  _isXmlElement = true
  name = _name
  attributes = new Mapping {}
  isBlockFormat = true
}
function Inline(_value) = new InlineClass { value = _value }
class InlineClass { value: Any }
function Comment(_text: String) = new CommentClass { text = _text }
class CommentClass { text: String; isBlockFormat: Boolean = true }
function CData(_text: String) = new CDataClass { text = _text }
class CDataClass { text: String }
"#;

/// Calls converter functions through the evaluator, at the call depth of
/// the rendering that runs them.
struct Invoker<'e>(&'e mut Evaluator, usize);

impl Invoke for Invoker<'_> {
    fn invoke(&mut self, function: &Value, arg: Value) -> Result<Value> {
        self.0.invoke_lambda(function, &[arg], self.1)
    }
}

impl Evaluator {
    /// The template of the builtin class `name`, which `new name { ... }`
    /// amends when no binding of that name is in scope.
    pub(super) fn builtin_class(&mut self, name: &str) -> Result<Option<Value>> {
        let Some(&name) = BUILTIN_CLASS_NAMES.iter().find(|n| **n == name) else {
            return Ok(None);
        };
        if let Some(template) = self.builtin_classes.get(name) {
            return Ok(Some(template.clone()));
        }
        let tokens = lexer::lex_named(BASE_CLASSES, "pkl:base")?;
        let module = parser::parse_named(&tokens, BASE_CLASSES, "pkl:base")?;
        let Some((class_mods, body)) = module.body.iter().find_map(|entry| match entry {
            Entry::ClassDef(class_name, mods, _, body) if class_name == name => Some((mods, body)),
            _ => None,
        }) else {
            return Ok(None);
        };
        let mut scope = Scope {
            type_namespace: Some("pkl:base".into()),
            ..Scope::default()
        };
        seed_builtins(&mut scope);
        let template = self.eval_class_def(name, class_mods, None, body, &scope, 0)?;
        self.builtin_classes.insert(name, template.clone());
        Ok(Some(template))
    }

    /// The value of the standard library module `pkl:name`.
    pub(super) fn stdlib_module(&mut self, name: &str, depth: usize) -> Result<Value> {
        let (key, source) = match name {
            "json" => ("pkl:json", JSON_MODULE),
            "jsonnet" => ("pkl:jsonnet", JSONNET_MODULE),
            "xml" => ("pkl:xml", XML_MODULE),
            _ => return Ok(super::stdlib_module(name)),
        };
        if let Some(module) = self.builtin_classes.get(key) {
            return Ok(module.clone());
        }
        let tokens = lexer::lex_named(source, key)?;
        let module = parser::parse_named(&tokens, source, key)?;
        let value = self.eval_module_with_scope(&module, Path::new(key), depth + 1, None, None)?;
        self.builtin_classes.insert(key, value.clone());
        Ok(value)
    }

    /// The class a mapping key expression such as `[String]` or
    /// `[mod.Person]` names, if it names one.
    pub(super) fn class_key_of(&self, expr: &Expr, scope: &Scope) -> Option<Arc<str>> {
        let dotted = dotted_name(expr)?;
        let last = dotted.rsplit('.').next().unwrap_or(&dotted);
        match resolve_dotted(scope, &dotted) {
            None if !dotted.contains('.')
                && (render::BASE_CLASS_NAMES.contains(&last)
                    || BUILTIN_CLASS_NAMES.contains(&last)) =>
            {
                Some(render::class_key(last))
            }
            // Builtins bound to a marker of their own name (see `seed_builtins`).
            Some(Value::String(marker))
                if &*marker == last && render::BASE_CLASS_NAMES.contains(&last) =>
            {
                Some(render::class_key(last))
            }
            // A class: its template carries the class name.
            Some(Value::Object(_, Some(source)))
                if source
                    .type_name
                    .as_deref()
                    .is_some_and(|name| name.rsplit('.').next() == Some(last)) =>
            {
                source.type_name.as_deref().map(render::class_key)
            }
            _ => None,
        }
    }

    /// Evaluate `renderer.renderDocument(value)` or
    /// `renderer.renderValue(value)` for a builtin renderer.
    pub(super) fn eval_renderer_method(
        &mut self,
        renderer: &Value,
        method: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        if method == "parse" && render::typed_class_is(renderer, "pkl:json", "Parser") {
            return self.eval_json_parse(renderer, args, depth).map(Some);
        }
        let document = match method {
            "renderDocument" => true,
            "renderValue" => false,
            _ => return Ok(None),
        };
        let Some(kind) = render::renderer_kind(renderer) else {
            return Ok(None);
        };
        let [value] = args else {
            return Err(Error::Eval(format!(
                "{method}() expects 1 argument, got {}",
                args.len()
            )));
        };
        let settings = Settings::read(kind, renderer)?;
        let text = settings.render(value, document, None, &mut Invoker(self, depth))?;
        Ok(Some(Value::String(text.into())))
    }

    /// `json.Parser.parse(source)`, where `source` is a string or a resource.
    fn eval_json_parse(&mut self, parser: &Value, args: &[Value], depth: usize) -> Result<Value> {
        let Value::Object(settings, _) = parser else {
            return Err(Error::Eval("expected a json.Parser".into()));
        };
        let text = match args {
            [Value::String(text)] => text.clone(),
            [Value::Object(resource, _)] => match resource.get("text") {
                Some(Value::String(text)) => text.clone(),
                _ => {
                    return Err(Error::Eval(
                        "Expected a `String` or `Resource` to parse.".into(),
                    ));
                }
            },
            _ => {
                return Err(Error::Eval(format!(
                    "parse() expects 1 argument of type `String` or `Resource`, got {}",
                    args.len()
                )));
            }
        };
        let use_mapping = matches!(settings.get("useMapping"), Some(Value::Bool(true)));
        let converters = Converters::from_mapping(settings.get("converters"))?;
        super::parsers::parse_json(&text, use_mapping, &converters, &mut Invoker(self, depth))
    }

    /// Evaluate the top-level module's `output`: the `output` properties of
    /// the modules it amends or extends (furthest base first), then its own,
    /// each amending the one before. Like `ModuleOutput`'s defaults, `value`
    /// starts as the module itself and `renderer` as the renderer for the
    /// requested format.
    pub(super) fn eval_module_output(
        &mut self,
        module: &Module,
        module_map: &Arc<ObjectMap>,
        scope: &Scope,
        path: &Path,
        depth: usize,
    ) -> Result<()> {
        let own = std::mem::take(&mut self.output_props);
        let mut props = self.inherited_output_props(module, path)?;
        props.extend(own.into_iter().map(|prop| (prop, None)));
        self.output_sets_value = false;
        self.output_sets_omit_nulls = props
            .iter()
            .any(|(prop, _)| output_enables_null_omission(prop));
        if props.is_empty() {
            self.module_output = None;
            return Ok(());
        }
        let mut output = self
            .builtin_class("ModuleOutput")?
            .expect("ModuleOutput is a builtin class");
        let renderer = self
            .builtin_class(self.output_format.class_name())?
            .unwrap_or_default();
        if let Value::Object(map, _) = &mut output {
            let map = Arc::make_mut(map);
            map.insert("value".into(), module_object(module_map, path));
            map.insert("renderer".into(), renderer);
        }
        for (prop, base) in &props {
            let base_scope;
            let scope = match base {
                Some(base) => {
                    base_scope = self.base_module_scope(scope, base);
                    &base_scope
                }
                None => scope,
            };
            if let Some(expr) = &prop.value {
                let value = self.eval_expr(expr, scope, depth)?;
                let declared = match &prop.type_ann {
                    Some(TypeExpr::Named(name)) => Some(name.as_str()),
                    Some(_) => Some(""),
                    None => None,
                };
                let actual = match &value {
                    Value::Null => {
                        return Err(Error::Eval(
                            "Expected value of type `ModuleOutput`, but got `null`.".into(),
                        ));
                    }
                    Value::Object(_, Some(source)) => source.type_name.as_deref(),
                    Value::Object(_, None) => None,
                    other => Some(super::value_type_name(other)),
                };
                // `ModuleOutput` is not open, so no subclass can stand in.
                let is_output = |name: Option<&str>| name.is_none_or(|n| n == "ModuleOutput");
                if !is_output(declared) || !is_output(actual) {
                    let got = actual.or(declared).unwrap_or("Dynamic");
                    return Err(Error::Eval(format!(
                        "Expected `output` of module `file://{}` to be of type `ModuleOutput`, but got type `{got}`.",
                        path.display()
                    )));
                }
                self.output_sets_value = true;
                output = value;
            }
            if let Some(body) = &prop.body {
                self.output_sets_value |= body
                    .iter()
                    .any(|entry| matches!(entry, Entry::Property(p) if p.name == "value"));
                // The body reads the members it amends (`renderer`,
                // `value`) by name.
                let mut body_scope = scope.child();
                if let Value::Object(map, _) = &output {
                    for (name, value) in map.iter() {
                        body_scope.set_name(name.clone(), value.clone());
                    }
                }
                output = self.eval_value_amendment(output, body, &body_scope, depth)?;
            }
        }
        self.module_output = Some(output);
        Ok(())
    }

    /// The `output` properties of the modules `module` amends or extends,
    /// furthest base first, each with the path of the module declaring it.
    fn inherited_output_props(
        &mut self,
        module: &Module,
        path: &Path,
    ) -> Result<Vec<(Arc<Property>, Option<BaseModule>)>> {
        let mut levels = Vec::new();
        let mut parent = module.amends.clone().or_else(|| module.extends.clone());
        let mut from = path.to_path_buf();
        while let Some(uri) = parent {
            if levels.len() > self.max_depth {
                break;
            }
            let resolved = resolve_remote_relative(&from, &uri);
            let uri = resolved.as_deref().unwrap_or(&uri);
            let Some((base, source_path)) = self.load_parsed_module(uri, &from)? else {
                break;
            };
            let base_path = PathBuf::from(&source_path);
            let base_module = BaseModule {
                path: base_path.clone(),
                lexical: lexical_names(&base),
            };
            let props: Vec<_> = base
                .body
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Property(prop) if prop.name == "output" => {
                        Some((Arc::clone(prop), Some(base_module.clone())))
                    }
                    _ => None,
                })
                .collect();
            levels.push(props);
            parent = base.amends.clone().or_else(|| base.extends.clone());
            from = base_path;
        }
        Ok(levels.into_iter().rev().flatten().collect())
    }

    /// The scope a base module's `output` is evaluated in: the amending
    /// module's, where the module's properties have their final values, with
    /// the base module's own lexical bindings (locals, imports, classes) over
    /// it, so the amending module cannot shadow them.
    fn base_module_scope(&mut self, scope: &Scope, base: &BaseModule) -> Scope {
        let mut base_scope = scope.child();
        let key = self
            .canonicalize_io(&base.path)
            .ok()
            .filter(|key| self.module_scopes.contains_key(key))
            .unwrap_or_else(|| base.path.clone());
        if let Some(snapshot) = self.module_scopes.get(&key) {
            for (name, value) in &snapshot.values {
                if base.lexical.iter().any(|lexical| lexical == &**name)
                    || scope.get(name).is_none()
                {
                    base_scope.set_name(name.clone(), value.clone());
                }
            }
        }
        base_scope
    }

    /// A member of the module's evaluated `output`.
    fn output_member(&self, name: &str) -> Option<&Value> {
        match &self.module_output {
            Some(Value::Object(map, _)) => map.get(name),
            _ => None,
        }
    }

    /// The module's renderer: `output.renderer`, or `default` (amended by
    /// an untyped `renderer { ... }`).
    fn output_renderer(&self, default: RendererKind) -> (RendererKind, Value) {
        let renderer = self.output_member("renderer").cloned().unwrap_or_default();
        let kind = render::renderer_kind(&renderer).unwrap_or(default);
        (kind, renderer)
    }

    /// Apply the module output renderer's converters to a value tree.
    pub(super) fn apply_output_converters(&mut self, value: Value) -> Result<Value> {
        let (_, renderer) = self.output_renderer(RendererKind::Json);
        let converters = renderer_converters(&renderer)?;
        if converters.is_empty() {
            return Ok(value);
        }
        let top_kind = matches!(value, Value::Object(..)).then_some(Kind::Typed);
        render::convert_value(&value, top_kind, &converters, &mut Invoker(self, 0))
    }

    /// Whether the output renderer has a converter for a scalar value. The
    /// legacy converter cache only handles typed objects, so scalars use the
    /// renderer walker directly.
    pub(super) fn output_has_scalar_converter(&self) -> Result<bool> {
        let (_, renderer) = self.output_renderer(RendererKind::Json);
        let converters = renderer_converters(&renderer)?;
        Ok(["Int", "Float", "Boolean", "String", "Number", "Any"]
            .iter()
            .any(|name| converters.class(name).is_some()))
    }

    /// The value the module renders: `output.value`, or the module itself.
    #[cfg(feature = "native-io")]
    fn output_value(&self, module: Value) -> (Value, Option<Kind>) {
        match self.output_member("value") {
            Some(value) if self.output_sets_value => (value.clone(), None),
            _ => (module, Some(Kind::Typed)),
        }
    }

    /// Evaluate a local pkl file and convert its output value to JSON, as
    /// `pkl eval -f json` renders it (but keeping null properties).
    #[cfg(feature = "native-io")]
    pub(crate) fn eval_file_json_blocking(&mut self, path: &Path) -> Result<serde_json::Value> {
        let module = self.eval_file(path)?;
        self.module_json(module)
    }

    #[cfg(feature = "native-io")]
    fn module_json(&mut self, module: Value) -> Result<serde_json::Value> {
        let (value, top_kind) = self.output_value(module);
        let (_, renderer) = self.output_renderer(RendererKind::Json);
        let converters = renderer_converters(&renderer)?;
        // Null properties stay in the JSON (unlike `pkl eval -f json`) unless
        // the module's renderer asks to omit them.
        let omit_nulls = self.output_sets_omit_nulls;
        render::to_json(
            &value,
            top_kind,
            &converters,
            omit_nulls,
            &mut Invoker(self, 0),
        )
    }

    /// Evaluate a local pkl file and render its `output.text`, as `pkl eval`
    /// prints it.
    #[cfg(feature = "native-io")]
    pub(crate) fn eval_file_text_blocking(&mut self, path: &Path) -> Result<String> {
        self.output_format = RendererKind::Pcf;
        let module = self.eval_file(path);
        self.output_format = RendererKind::Json;
        let module = module?;
        match self.output_member("text") {
            Some(Value::String(text)) => return Ok(text.to_string()),
            Some(other) => {
                return Err(Error::Eval(format!(
                    "Expected value of type `String` for `output.text`, but got type `{}`.",
                    super::value_type_name(other)
                )));
            }
            None => {}
        }
        let (value, top_kind) = self.output_value(module);
        let (kind, renderer) = self.output_renderer(RendererKind::Pcf);
        let settings = Settings::read(kind, &renderer)?;
        settings.render(&value, true, top_kind, &mut Invoker(self, 0))
    }
}

fn renderer_converters(renderer: &Value) -> Result<Converters> {
    match renderer {
        Value::Object(map, _) => Converters::from_mapping(map.get("converters")),
        _ => Ok(Converters::default()),
    }
}

fn output_enables_null_omission(output: &Property) -> bool {
    let Some(body) = &output.body else {
        return false;
    };
    body.iter().any(|entry| {
        let Entry::Property(renderer) = entry else {
            return false;
        };
        if renderer.name != "renderer" {
            return false;
        }
        let entries = renderer
            .body
            .as_deref()
            .or_else(|| match renderer.value.as_ref() {
                Some(Expr::New(_, entries, _)) => Some(entries.as_ref()),
                _ => None,
            });
        entries.is_some_and(|entries| {
            entries.iter().any(|entry| {
                matches!(entry,
                    Entry::Property(prop)
                        if prop.name == "omitNullProperties"
                            && prop.value.as_ref() == Some(&Expr::Bool(true))
                )
            })
        })
    })
}

/// `a.b.C` for an identifier or a chain of field accesses.
fn dotted_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::Field(base, name) => Some(format!("{}.{name}", dotted_name(base)?)),
        _ => None,
    }
}

/// The module itself as a value: typed, with the module's name as its class
/// (as in Pkl), so renderers treat its members as properties.
fn module_object(map: &Arc<ObjectMap>, path: &Path) -> Value {
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Module".into());
    let source = ObjectSource {
        entries: Vec::new().into(),
        captured: SourceScope::default(),
        body_members: Default::default(),
        is_open: true,
        is_abstract: false,
        type_name: Some(name),
        type_identity: None,
        parent_type_names: vec!["Module".into()],
        parent_type_identities: Vec::new(),
        entry_scopes: Vec::new(),
        evaluated_properties: Vec::new(),
        elements: Vec::new(),
        mapping_value_types: Vec::new(),
        deprecated: Default::default(),
        poisoned_members: None,
        kind: crate::value::ObjectKind::Object,
    };
    Value::Object(Arc::clone(map), Some(Arc::new(source)))
}
