//! Pkl's value renderers (`JsonRenderer`, `YamlRenderer` and
//! `PropertiesRenderer` from `pkl:base`) and the conversion of a module's
//! value to JSON for [`crate::eval_to_json`].
//!
//! The renderers follow pkl-core's `AbstractRenderer`: a value is converted
//! (by `converters` keyed by class or by path), then visited, and each member
//! is converted before it is visited in turn.
//!
//! pklr's [`Value`] does not keep every distinction Pkl's renderers make
//! (`List` vs `Listing`, `Map` vs `Mapping`, the elements of a `Dynamic` that
//! also has properties), so [`kind_of`] recovers what it can from an
//! object's [`ObjectSource`].

use std::sync::Arc;

use rustc_hash::FxHashSet as HashSet;

use super::mapping::type_names_match;
use crate::error::{Error, Result};
use crate::parser::{Entry, Modifier};
use crate::value::{ListValue, ObjectKind, ObjectMap, ObjectSource, Value};

pub(crate) mod json;
pub(crate) mod jsonnet;
pub(crate) mod pcf;
pub(crate) mod plist;
pub(crate) mod properties;
pub(crate) mod xml;
mod xml_names;
pub(crate) mod yaml;

/// Prefix of a mapping key that stands for a class (`[String] = ...` in
/// `converters`) rather than a string. pklr's object keys are strings, so a
/// class key is a string no Pkl source can spell.
const CLASS_KEY_PREFIX: &str = "\0class:";

/// The mapping key for the class `name`.
pub(crate) fn class_key(name: &str) -> Arc<str> {
    format!("{CLASS_KEY_PREFIX}{name}").into()
}

/// The class a mapping key made by [`class_key`] stands for.
pub(crate) fn class_key_name(key: &str) -> Option<&str> {
    key.strip_prefix(CLASS_KEY_PREFIX)
}

/// Prefix of a mapping key that stands for a `RenderDirective`.
const DIRECTIVE_KEY_PREFIX: &str = "\0directive:";
/// Prefix of a mapping key that stands for another object or a list.
const VALUE_KEY_PREFIX: &str = "\0value:";
/// The member holding a `RenderDirective` key's text in [`key_value`].
const DIRECTIVE_TEXT: &str = "\0directive";

/// The mapping key for an object or list key: a `RenderDirective` keeps its
/// text, and other values are keyed by their content.
pub(crate) fn object_key(value: &Value) -> Arc<str> {
    match kind_of(value) {
        Kind::RenderDirective => match render_directive_text(value) {
            Ok(text) => format!("{DIRECTIVE_KEY_PREFIX}{text}").into(),
            Err(_) => format!("{VALUE_KEY_PREFIX}{}", value.to_json()).into(),
        },
        _ => format!("{VALUE_KEY_PREFIX}{}", value.to_json()).into(),
    }
}

/// Whether `key` stands for something other than a string.
fn is_non_string_key(key: &str) -> bool {
    if key.starts_with(crate::value::MAPPING_KEY_PREFIX) {
        return !matches!(crate::value::mapping_storage_value(key), Value::String(_));
    }
    key.starts_with('\0')
        && (key.starts_with(CLASS_KEY_PREFIX)
            || key.starts_with(DIRECTIVE_KEY_PREFIX)
            || key.starts_with(VALUE_KEY_PREFIX))
}

/// The value an entry key stands for, as renderers see it: a string, a
/// `RenderDirective`, or an object standing for another non-string key.
fn key_value(key: &Arc<str>) -> Value {
    if key.starts_with(crate::value::MAPPING_KEY_PREFIX) {
        return crate::value::mapping_storage_value(key);
    }
    if !is_non_string_key(key) {
        return Value::String(key.clone());
    }
    let mut map = ObjectMap::default();
    match key.strip_prefix(DIRECTIVE_KEY_PREFIX) {
        Some(text) => map.insert(DIRECTIVE_TEXT.into(), Value::String(text.into())),
        None => map.insert(key.clone(), Value::Null),
    };
    Value::Object(Arc::new(map), None)
}

/// Classes of `pkl:base` that a bare name in a mapping key (`[String]`)
/// refers to.
pub(crate) const BASE_CLASS_NAMES: &[&str] = &[
    "Any",
    "Boolean",
    "Bytes",
    "Class",
    "Collection",
    "DataSize",
    "Duration",
    "Dynamic",
    "Float",
    "Function",
    "Function0",
    "Function1",
    "Function2",
    "Function3",
    "Function4",
    "Function5",
    "Int",
    "IntSeq",
    "List",
    "Listing",
    "Map",
    "Mapping",
    "Module",
    "Null",
    "Number",
    "Object",
    "Pair",
    "Regex",
    "Set",
    "String",
    "TypeAlias",
    "Typed",
];

const DURATION_UNITS: &[&str] = &["ns", "us", "ms", "s", "min", "h", "d"];
const DATA_SIZE_UNITS: &[&str] = &[
    "b", "kb", "mb", "gb", "tb", "pb", "kib", "mib", "gib", "tib", "pib",
];

/// The renderer classes pklr implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RendererKind {
    Pcf,
    Json,
    Yaml,
    Properties,
    PList,
    Jsonnet,
    Xml,
}

impl RendererKind {
    pub(crate) const ALL: &[RendererKind] = &[
        RendererKind::Pcf,
        RendererKind::Json,
        RendererKind::Yaml,
        RendererKind::Properties,
        RendererKind::PList,
        RendererKind::Jsonnet,
        RendererKind::Xml,
    ];

    /// The module and name of the renderer's class.
    fn class(self) -> (&'static str, &'static str) {
        match self {
            RendererKind::Pcf => ("pkl:base", "PcfRenderer"),
            RendererKind::Json => ("pkl:base", "JsonRenderer"),
            RendererKind::Yaml => ("pkl:base", "YamlRenderer"),
            RendererKind::Properties => ("pkl:base", "PropertiesRenderer"),
            RendererKind::PList => ("pkl:base", "PListRenderer"),
            RendererKind::Jsonnet => ("pkl:jsonnet", "Renderer"),
            RendererKind::Xml => ("pkl:xml", "Renderer"),
        }
    }

    /// The unqualified class name used by the existing output setup path.
    pub(crate) fn class_name(self) -> &'static str {
        self.class().1
    }

    /// The type identity instances of this renderer carry (see
    /// `Scope::runtime_type_identity`).
    fn identity(self) -> String {
        let (module, class) = self.class();
        format!("{module}.{class}")
    }
}

/// The renderer kind of a renderer object, including instances of a
/// subclass of a renderer class.
pub(crate) fn renderer_kind(value: &Value) -> Option<RendererKind> {
    let Value::Object(_, Some(source)) = value else {
        return None;
    };
    let by_identity = source
        .type_identity
        .iter()
        .chain(source.parent_type_identities.iter())
        .find_map(|identity| {
            RendererKind::ALL
                .iter()
                .copied()
                .find(|kind| kind.identity() == *identity)
        });
    by_identity.or_else(|| {
        // `pkl:base` renderers are known by name too. Imported renderers must
        // retain their module identity so an unrelated user `Renderer` class
        // cannot be mistaken for a standard-library renderer.
        source
            .type_name
            .iter()
            .chain(source.parent_type_names.iter())
            .find_map(|name| {
                RendererKind::ALL
                    .iter()
                    .copied()
                    .find(|kind| kind.class() == ("pkl:base", name.as_str()))
            })
    })
}

/// Whether `value` is an instance of the class `class` of the standard
/// library module `module` (such as `pkl:xml`'s `Comment`).
pub(crate) fn typed_class_is(value: &Value, module: &str, class: &str) -> bool {
    matches!(
        value,
        Value::Object(_, Some(source))
            if source.type_identity.as_deref().is_some_and(|identity| {
                identity.strip_prefix(module).and_then(|rest| rest.strip_prefix('.')) == Some(class)
            })
    )
}

/// What a pklr [`Value`] is to a renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Null,
    Boolean,
    Int,
    Float,
    String,
    Duration,
    DataSize,
    Regex,
    Function,
    RenderDirective,
    /// A `Listing` or `List`; pklr does not tell them apart.
    Listing,
    /// An object whose members are all entries: a `Mapping` or `Map`.
    Mapping,
    /// An object with properties and entries.
    Dynamic,
    /// An instance of a class (or a module); all members are properties.
    Typed,
}

impl Kind {
    /// The Pkl class name used in error messages.
    fn pkl_class(self, value: &Value) -> String {
        match self {
            Kind::Null => "Null".into(),
            Kind::Boolean => "Boolean".into(),
            Kind::Int => "Int".into(),
            Kind::Float => "Float".into(),
            Kind::String => "String".into(),
            Kind::Duration => "Duration".into(),
            Kind::DataSize => "DataSize".into(),
            Kind::Regex => "Regex".into(),
            Kind::Function => match value {
                Value::Lambda(params, ..) => format!("Function{}", params.len()),
                _ => "Function".into(),
            },
            Kind::RenderDirective => "RenderDirective".into(),
            Kind::Listing => "Listing".into(),
            Kind::Mapping => "Mapping".into(),
            Kind::Dynamic => "Dynamic".into(),
            Kind::Typed => match value {
                Value::Object(_, Some(source)) => {
                    source.type_name.clone().unwrap_or_else(|| "Typed".into())
                }
                _ => "Typed".into(),
            },
        }
    }

    pub(crate) fn is_object(self) -> bool {
        matches!(self, Kind::Mapping | Kind::Dynamic | Kind::Typed)
    }
}

/// Classify `value` for rendering.
pub(crate) fn kind_of(value: &Value) -> Kind {
    match value {
        Value::Null => Kind::Null,
        Value::Bool(_) => Kind::Boolean,
        Value::Int(_) => Kind::Int,
        Value::Float(_) => Kind::Float,
        Value::String(_) => Kind::String,
        Value::Lambda(..) => Kind::Function,
        Value::List(_) => Kind::Listing,
        Value::Object(map, None) => {
            if let Some(kind) = builtin_object_kind(map) {
                kind
            } else {
                Kind::Mapping
            }
        }
        Value::Object(_, Some(source)) => match source.type_name.as_deref() {
            Some("RenderDirective") => Kind::RenderDirective,
            Some("Dynamic") => Kind::Dynamic,
            Some("Mapping" | "Map") => Kind::Mapping,
            Some(_) => Kind::Typed,
            None if source.kind == ObjectKind::Mapping => Kind::Mapping,
            None if source.kind == ObjectKind::Object => Kind::Dynamic,
            None if source.is_metadata_only() => Kind::Typed,
            None => Kind::Dynamic,
        },
    }
}

/// pklr represents durations and data sizes as `{value, unit}` objects and
/// regexes as `{_type = "regex", pattern}` objects without a source.
fn builtin_object_kind(map: &ObjectMap) -> Option<Kind> {
    if map.len() == 1 && map.contains_key(DIRECTIVE_TEXT) {
        return Some(Kind::RenderDirective);
    }
    if map.len() != 2 {
        return None;
    }
    if let (Some(Value::Int(_) | Value::Float(_)), Some(Value::String(unit))) =
        (map.get("value"), map.get("unit"))
    {
        if DURATION_UNITS.contains(&&**unit) {
            return Some(Kind::Duration);
        }
        if DATA_SIZE_UNITS.contains(&&**unit) {
            return Some(Kind::DataSize);
        }
    }
    if let (Some(Value::String(ty)), Some(Value::String(_))) =
        (map.get("_type"), map.get("pattern"))
        && &**ty == "regex"
    {
        return Some(Kind::Regex);
    }
    None
}

/// Whether a member of an object is a property or an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemberKind {
    Property,
    Entry,
}

/// Tells the properties of an object from its entries. A `Dynamic`'s
/// properties are the names its definition declares as properties; every
/// other member came from an entry (`["key"] = ...`) or a spread map.
pub(crate) struct Members<'a> {
    kind: Kind,
    properties: Option<HashSet<&'a str>>,
}

impl<'a> Members<'a> {
    pub(crate) fn new(kind: Kind, source: Option<&'a ObjectSource>) -> Self {
        let properties = (kind == Kind::Dynamic).then(|| {
            source
                .map(|source| {
                    source
                        .entries()
                        .iter()
                        .filter_map(|entry| match entry {
                            Entry::Property(prop) if !prop.modifiers.contains(&Modifier::Local) => {
                                Some(prop.name.as_str())
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        Self { kind, properties }
    }

    pub(crate) fn kind_of(&self, key: &str) -> MemberKind {
        match self.kind {
            Kind::Mapping => MemberKind::Entry,
            Kind::Dynamic
                if !self
                    .properties
                    .as_ref()
                    .is_some_and(|names| names.contains(key)) =>
            {
                MemberKind::Entry
            }
            _ => MemberKind::Property,
        }
    }
}

/// A segment of the path from the rendered value to the one being visited.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PathPart {
    TopLevel,
    Property(Arc<str>),
    /// An entry with a string key.
    Entry(Arc<str>),
    /// An entry whose (converted) key is not a string.
    Key(Value),
    Element(usize),
}

/// A segment of a converter path such as `foo.bar[*]`.
#[derive(Clone, Debug, PartialEq)]
enum PathSpec {
    TopLevel,
    Property(String),
    Entry(String),
    AnyProperty,
    AnyElement,
}

impl PathSpec {
    fn matches(&self, part: &PathPart) -> bool {
        match (self, part) {
            (PathSpec::TopLevel, PathPart::TopLevel) => true,
            (PathSpec::Property(name), PathPart::Property(actual)) => name == &**actual,
            (PathSpec::Entry(key), PathPart::Entry(actual)) => key == &**actual,
            (PathSpec::AnyProperty, PathPart::Property(_)) => true,
            (
                PathSpec::AnyElement,
                PathPart::Element(_) | PathPart::Entry(_) | PathPart::Key(_),
            ) => true,
            _ => false,
        }
    }
}

/// Parse a converter path, innermost segment first, as pkl-core's
/// `PathSpecParser` does.
fn parse_path_spec(spec: &str) -> Result<Vec<PathSpec>> {
    let invalid = || Error::Eval(format!("Converter path `{spec}` has invalid syntax."));
    let chars: Vec<char> = spec.chars().collect();
    let mut result = Vec::new();
    // 0: start or after leading `^`, 1: in property, 2: in element,
    // 3: after `]`, 4: after `.*`, 5: after `[*`
    let mut state = 0;
    let mut start = 0;
    let part = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    for (idx, &ch) in chars.iter().enumerate() {
        match ch {
            '^' => {
                if idx != 0 {
                    return Err(invalid());
                }
                result.push(PathSpec::TopLevel);
                start = 1;
            }
            '.' => {
                match state {
                    1 => {
                        if idx == start {
                            return Err(invalid());
                        }
                        result.push(PathSpec::Property(part(start, idx)));
                    }
                    3 | 4 => {}
                    _ => return Err(invalid()),
                }
                start = idx + 1;
                state = 1;
            }
            '[' => {
                match state {
                    1 => {
                        if idx == start {
                            return Err(invalid());
                        }
                        result.push(PathSpec::Property(part(start, idx)));
                    }
                    0 | 3 | 4 => {}
                    _ => return Err(invalid()),
                }
                start = idx + 1;
                state = 2;
            }
            ']' => {
                match state {
                    2 => {
                        if idx == start {
                            return Err(invalid());
                        }
                        result.push(PathSpec::Entry(part(start, idx)));
                    }
                    5 => {}
                    _ => return Err(invalid()),
                }
                state = 3;
            }
            '*' => {
                state = match state {
                    0 | 1 => {
                        if start != idx {
                            return Err(invalid());
                        }
                        result.push(PathSpec::AnyProperty);
                        4
                    }
                    2 => {
                        if start != idx {
                            return Err(invalid());
                        }
                        result.push(PathSpec::AnyElement);
                        5
                    }
                    _ => return Err(invalid()),
                };
            }
            _ => {
                if state > 2 {
                    return Err(invalid());
                }
                if state == 0 {
                    state = 1;
                }
            }
        }
    }
    match state {
        0 => {
            if result.is_empty() {
                // "" matches the top-level value (deprecated in favor of "^")
                result.push(PathSpec::TopLevel);
            }
        }
        1 => {
            if start == chars.len() {
                return Err(invalid());
            }
            result.push(PathSpec::Property(part(start, chars.len())));
        }
        3 | 4 => {}
        _ => return Err(invalid()),
    }
    result.reverse();
    Ok(result)
}

/// A renderer's `converters`: functions applied to values before they are
/// rendered, keyed by class or by path.
#[derive(Clone, Debug, Default)]
pub(crate) struct Converters {
    by_class: Vec<(String, Value)>,
    by_path: Vec<(Vec<PathSpec>, Value)>,
}

impl Converters {
    /// Read a `converters` mapping. Keys made by [`class_key`] are class
    /// converters; any other key is a path.
    pub(crate) fn from_mapping(converters: Option<&Value>) -> Result<Self> {
        let mut result = Converters::default();
        let Some(Value::Object(map, _)) = converters else {
            return Ok(result);
        };
        for (key, function) in map.iter() {
            if !matches!(function, Value::Lambda(..)) {
                continue;
            }
            match class_key_name(key) {
                Some(class) => result.by_class.push((class.to_string(), function.clone())),
                None => result
                    .by_path
                    .push((parse_path_spec(key)?, function.clone())),
            }
        }
        Ok(result)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_class.is_empty() && self.by_path.is_empty()
    }

    pub(crate) fn has_paths(&self) -> bool {
        !self.by_path.is_empty()
    }

    pub(crate) fn class(&self, name: &str) -> Option<&Value> {
        self.by_class
            .iter()
            .find(|(class, _)| class == name)
            .map(|(_, function)| function)
    }

    /// The converter for `value` at `path` (innermost segment last): a
    /// matching path converter wins over a class converter.
    pub(crate) fn find(&self, value: &Value, kind: Kind, path: &[PathPart]) -> Option<&Value> {
        for (spec, function) in &self.by_path {
            if spec.len() <= path.len()
                && spec
                    .iter()
                    .zip(path.iter().rev())
                    .all(|(spec, part)| spec.matches(part))
            {
                return Some(function);
            }
        }
        if self.by_class.is_empty() {
            return None;
        }
        match kind {
            Kind::Null => self.class("Null"),
            Kind::Boolean => self.class("Boolean"),
            Kind::Int => self.class("Int"),
            Kind::Float => self.class("Float"),
            Kind::String => self.class("String"),
            Kind::Duration => self.class("Duration"),
            Kind::DataSize => self.class("DataSize"),
            Kind::Regex => self.class("Regex"),
            Kind::Function => match value {
                Value::Lambda(params, ..) => self.class(&format!("Function{}", params.len())),
                _ => None,
            },
            Kind::Listing => self.class("Listing").or_else(|| self.class("List")),
            Kind::Mapping => self.class("Mapping").or_else(|| self.class("Map")),
            Kind::Dynamic => self.class("Dynamic"),
            Kind::Typed | Kind::RenderDirective => {
                // Class converters are covariant: the nearest class with a
                // converter wins.
                let source = match value {
                    Value::Object(_, Some(source)) => Some(&**source),
                    _ => None,
                };
                source
                    .into_iter()
                    .flat_map(|source| {
                        source
                            .type_name
                            .iter()
                            .chain(source.parent_type_names.iter())
                    })
                    .find_map(|name| {
                        self.by_class
                            .iter()
                            .find(|(class, _)| type_names_match(class, name))
                            .map(|(_, function)| function)
                    })
                    .or_else(|| {
                        ["Typed", "Object", "Any"]
                            .iter()
                            .find_map(|c| self.class(c))
                    })
            }
        }
    }
}

/// Calls Pkl functions on behalf of the renderers.
pub(crate) trait Invoke {
    fn invoke(&mut self, function: &Value, arg: Value) -> Result<Value>;
}

/// A renderer's settings, read from a renderer object.
pub(crate) struct Settings {
    pub kind: RendererKind,
    pub converters: Converters,
    pub indent: String,
    pub omit_null_properties: bool,
    pub yaml_mode: String,
    pub yaml_indent_width: usize,
    pub yaml_is_stream: bool,
    pub restrict_charset: bool,
    pub use_custom_string_delimiters: bool,
    pub xml_version: String,
    pub xml_root_name: String,
    pub xml_root_attributes: Option<ObjectMap>,
}

impl Settings {
    /// Read the settings of `renderer`. Members missing from it (such as an
    /// untyped `renderer { ... }` amendment) take `kind`'s defaults.
    pub(crate) fn read(kind: RendererKind, renderer: &Value) -> Result<Self> {
        let members = match renderer {
            Value::Object(map, _) => Some(&**map),
            _ => None,
        };
        let get = |name: &str| members.and_then(|map| map.get(name));
        let string = |name: &str, default: &str| -> Result<String> {
            match get(name) {
                None => Ok(default.to_string()),
                Some(Value::String(s)) => Ok(s.to_string()),
                Some(other) => Err(type_mismatch(name, "String", other)),
            }
        };
        let boolean = |name: &str, default: bool| -> Result<bool> {
            match get(name) {
                None => Ok(default),
                Some(Value::Bool(b)) => Ok(*b),
                Some(other) => Err(type_mismatch(name, "Boolean", other)),
            }
        };
        let yaml_indent_width = match get("indentWidth") {
            None => 2,
            Some(Value::Int(n)) if *n > 1 => *n as usize,
            Some(Value::Int(_)) => {
                return Err(Error::Eval("Type constraint `this > 1` violated.".into()));
            }
            Some(other) => return Err(type_mismatch("indentWidth", "Int", other)),
        };
        let yaml_mode = string("mode", "compat")?;
        if !matches!(yaml_mode.as_str(), "compat" | "1.1" | "1.2") {
            return Err(Error::Eval(format!(
                "Expected value of type `\"compat\"|\"1.1\"|\"1.2\"`, but got \"{yaml_mode}\"."
            )));
        }
        let xml_version = string("xmlVersion", "1.0")?;
        if !matches!(xml_version.as_str(), "1.0" | "1.1") {
            return Err(Error::Eval(format!(
                "Expected value of type `\"1.0\"|\"1.1\"`, but got \"{xml_version}\"."
            )));
        }
        Ok(Settings {
            kind,
            converters: Converters::from_mapping(get("converters"))?,
            // Only `jsonnet.Renderer.indent` may be null, meaning no indent.
            indent: match get("indent") {
                Some(Value::Null) if kind == RendererKind::Jsonnet => String::new(),
                _ => string("indent", "  ")?,
            },
            omit_null_properties: boolean("omitNullProperties", kind != RendererKind::Pcf)?,
            yaml_mode,
            yaml_indent_width,
            yaml_is_stream: boolean("isStream", false)?,
            restrict_charset: boolean("restrictCharset", false)?,
            use_custom_string_delimiters: boolean("useCustomStringDelimiters", false)?,
            xml_version,
            xml_root_name: string("rootElementName", "root")?,
            xml_root_attributes: match get("rootElementAttributes") {
                Some(Value::Object(map, _)) => Some((**map).clone()),
                _ => None,
            },
        })
    }

    /// Render `value` as a document (`renderDocument`) or as a value
    /// (`renderValue`). `top_kind` overrides how the top-level value is
    /// classified, for a module's own value.
    pub(crate) fn render(
        &self,
        value: &Value,
        document: bool,
        top_kind: Option<Kind>,
        invoke: &mut dyn Invoke,
    ) -> Result<String> {
        let walk = Walk::new(self, invoke, top_kind);
        match self.kind {
            RendererKind::Pcf => {
                pcf::Pcf::new(walk, &self.indent, self.use_custom_string_delimiters)
                    .render(value, document)
            }
            RendererKind::Json => json::Json::new(walk, &self.indent).render(value, document),
            RendererKind::Yaml => yaml::Yaml::new(walk, self).render(value, document),
            RendererKind::Properties => {
                properties::Properties::new(walk, self.restrict_charset).render(value, document)
            }
            RendererKind::PList => plist::PList::new(walk, &self.indent).render(value, document),
            RendererKind::Jsonnet => {
                jsonnet::Jsonnet::new(walk, &self.indent).render(value, document)
            }
            RendererKind::Xml => xml::Xml::new(
                walk,
                &self.indent,
                &self.xml_version,
                &self.xml_root_name,
                self.xml_root_attributes.clone(),
            )
            .render(value, document),
        }
    }
}

fn type_mismatch(property: &str, expected: &str, got: &Value) -> Error {
    Error::Eval(format!(
        "Expected value of type `{expected}` for `{property}`, but got type `{}`.",
        kind_of(got).pkl_class(got)
    ))
}

/// The state pkl-core's `AbstractRenderer` keeps while visiting a value.
pub(crate) struct Walk<'a> {
    converters: &'a Converters,
    invoke: &'a mut dyn Invoke,
    pub(crate) path: Vec<PathPart>,
    skip_null_properties: bool,
    skip_null_entries: bool,
    /// The kind of the value directly enclosing the one being visited.
    pub(crate) enclosing: Option<Kind>,
    top_kind: Option<Kind>,
}

impl<'a> Walk<'a> {
    fn new(settings: &'a Settings, invoke: &'a mut dyn Invoke, top_kind: Option<Kind>) -> Self {
        let skip_nulls = settings.omit_null_properties;
        Walk {
            converters: &settings.converters,
            invoke,
            path: Vec::new(),
            skip_null_properties: skip_nulls,
            // Pcf renders null entries even when it omits null properties.
            skip_null_entries: skip_nulls && settings.kind != RendererKind::Pcf,
            enclosing: None,
            top_kind,
        }
    }

    /// The kind of `value`, honoring the top-level override once.
    fn kind(&mut self, value: &Value) -> Kind {
        self.top_kind.take().unwrap_or_else(|| kind_of(value))
    }

    /// Apply the converter for `value` at the current path, if any.
    fn convert(&mut self, value: &Value) -> Result<Value> {
        match self.converters.find(value, kind_of(value), &self.path) {
            Some(function) => {
                let function = function.clone();
                self.invoke.invoke(&function, value.clone())
            }
            None => Ok(value.clone()),
        }
    }

    /// Convert an entry key, which only class converters apply to.
    fn convert_key(&mut self, key: &Value) -> Result<Value> {
        match self.converters.find(key, kind_of(key), &[]) {
            Some(function) => {
                let function = function.clone();
                self.invoke.invoke(&function, key.clone())
            }
            None => Ok(key.clone()),
        }
    }
}

/// The hooks of pkl-core's `AbstractRenderer`, with its traversal as
/// provided methods.
pub(crate) trait StringRenderer<'a> {
    fn walk(&mut self) -> &mut Walk<'a>;
    /// The renderer's name in error messages, such as `JSON`.
    fn name(&self) -> &'static str;

    fn visit_null(&mut self) -> Result<()>;
    fn visit_bool(&mut self, value: bool) -> Result<()>;
    fn visit_int(&mut self, value: i64) -> Result<()>;
    fn visit_float(&mut self, value: f64) -> Result<()>;
    fn visit_string(&mut self, value: &str) -> Result<()>;
    fn visit_render_directive(&mut self, text: &str) -> Result<()>;
    /// Render a typed object the format treats specially (such as
    /// `xml.Comment`), returning whether it did.
    fn visit_typed(&mut self, _value: &Value) -> Result<bool> {
        Ok(false)
    }
    /// Durations, data sizes, regexes and functions, which these renderers
    /// cannot render.
    fn visit_other(&mut self, value: &Value, kind: Kind) -> Result<()> {
        Err(cannot_render_type(value, kind, self.name()))
    }
    fn start_object(&mut self, kind: Kind) -> Result<()>;
    fn end_object(&mut self, kind: Kind, is_empty: bool) -> Result<()>;
    fn start_listing(&mut self) -> Result<()>;
    fn end_listing(&mut self, is_empty: bool) -> Result<()>;
    fn visit_element(&mut self, index: usize, value: &Value, is_first: bool) -> Result<()>;
    fn visit_entry_key(&mut self, key: &Value, is_first: bool) -> Result<()>;
    fn visit_entry_value(&mut self, value: &Value) -> Result<()> {
        self.visit(value)
    }
    fn visit_property(&mut self, name: &str, value: &Value, is_first: bool) -> Result<()>;

    /// Start rendering: convert the top-level value.
    fn convert_top_level(&mut self, value: &Value) -> Result<Value> {
        let walk = self.walk();
        walk.path.clear();
        walk.path.push(PathPart::TopLevel);
        let kind = walk.top_kind.unwrap_or_else(|| kind_of(value));
        match walk.converters.find(value, kind, &walk.path) {
            Some(function) => {
                let function = function.clone();
                // The converted value is classified on its own.
                walk.top_kind = None;
                walk.invoke.invoke(&function, value.clone())
            }
            None => Ok(value.clone()),
        }
    }

    fn visit(&mut self, value: &Value) -> Result<()> {
        let kind = self.walk().kind(value);
        if kind == Kind::Typed
            && typed_class_is(value, "pkl:xml", "CommentClass")
            && matches!(value, Value::Object(map, _) if map.get("text").and_then(Value::as_str).is_some_and(|text| text.contains("--") || text.ends_with('-')))
        {
            return Err(Error::Eval(
                "XML comments must not contain `--` or end with `-`.".into(),
            ));
        }
        match (kind, value) {
            (Kind::Null, _) => self.visit_null(),
            (Kind::Boolean, Value::Bool(b)) => self.visit_bool(*b),
            (Kind::Int, Value::Int(n)) => self.visit_int(*n),
            (Kind::Float, Value::Float(f)) => self.visit_float(*f),
            (Kind::String, Value::String(s)) => self.visit_string(s),
            (Kind::RenderDirective, _) => {
                self.visit_render_directive(&render_directive_text(value)?)
            }
            (Kind::Listing, Value::List(items)) => self.visit_listing(items),
            (Kind::Typed, _) if self.visit_typed(value)? => Ok(()),
            (kind, Value::Object(map, source)) if kind.is_object() => {
                self.visit_object(kind, map, source.as_deref())
            }
            (kind, _) => self.visit_other(value, kind),
        }
    }

    fn visit_listing(&mut self, items: &[Value]) -> Result<()> {
        self.start_listing()?;
        let previous = self.walk().enclosing.replace(Kind::Listing);
        for (index, item) in items.iter().enumerate() {
            self.walk().path.push(PathPart::Element(index));
            let converted = self.walk().convert(item)?;
            self.visit_element(index, &converted, index == 0)?;
            self.walk().path.pop();
        }
        self.walk().enclosing = previous;
        self.end_listing(items.is_empty())
    }

    fn visit_object(
        &mut self,
        kind: Kind,
        map: &ObjectMap,
        source: Option<&ObjectSource>,
    ) -> Result<()> {
        self.start_object(kind)?;
        let previous = self.walk().enclosing.replace(kind);
        let members = Members::new(kind, source);
        // Pkl keeps an object body's properties ahead of its entries.
        let mut ordered: Vec<(MemberKind, &Arc<str>, &Value)> = map
            .iter()
            // Methods are stored as function-valued members.
            .filter(|(_, value)| !matches!(value, Value::Lambda(..)))
            .map(|(key, value)| (members.kind_of(key), key, value))
            .collect();
        ordered.sort_by_key(|(member, ..)| *member == MemberKind::Entry);
        let mut is_first = true;
        for (member, key, value) in ordered {
            match member {
                MemberKind::Property => {
                    self.walk().path.push(PathPart::Property(key.clone()));
                    let converted = self.walk().convert(value)?;
                    if !(self.walk().skip_null_properties && converted == Value::Null) {
                        self.visit_property(key, &converted, is_first)?;
                        is_first = false;
                    }
                    self.walk().path.pop();
                }
                MemberKind::Entry => {
                    let key = self.walk().convert_key(&key_value(key))?;
                    self.walk().path.push(match &key {
                        Value::String(s) => PathPart::Entry(s.clone()),
                        other => PathPart::Key(other.clone()),
                    });
                    let converted = self.walk().convert(value)?;
                    if !(self.walk().skip_null_entries && converted == Value::Null) {
                        let value_path = std::mem::take(&mut self.walk().path);
                        let result = self.visit_entry_key(&key, is_first);
                        self.walk().path = value_path;
                        result?;
                        is_first = false;
                        self.visit_entry_value(&converted)?;
                    }
                    self.walk().path.pop();
                }
            }
        }
        self.walk().enclosing = previous;
        self.end_object(kind, is_first)
    }
}

/// The `text` of a `RenderDirective`.
fn render_directive_text(value: &Value) -> Result<Arc<str>> {
    match value {
        Value::Object(map, _) => match map.get("text").or_else(|| map.get(DIRECTIVE_TEXT)) {
            Some(Value::String(text)) => Ok(text.clone()),
            _ => Err(Error::Eval(
                "Expected value of type `String` for `RenderDirective.text`.".into(),
            )),
        },
        _ => Err(Error::Eval("expected a RenderDirective".into())),
    }
}

/// `Cannot render value of type ...`, for a value the renderer has no
/// representation for.
pub(crate) fn cannot_render_type(value: &Value, kind: Kind, name: &str) -> Error {
    Error::Eval(format!(
        "Cannot render value of type `{}` as {name}.\nValue: {}\n\nConsider adding a converter to `output.converters`.",
        kind.pkl_class(value),
        display_value(value),
    ))
}

/// `Cannot render object with non-string key as ...`.
pub(crate) fn cannot_render_non_string_key(key: &Value, name: &str) -> Error {
    Error::Eval(format!(
        "Cannot render object with non-string key as {name}.\nKey   : {}",
        display_value(key)
    ))
}

/// Check that `text` can be emitted in the selected XML version. XML 1.1's
/// restricted controls are allowed only where the renderer can write them as
/// character references.
pub(crate) fn validate_xml_characters(
    text: &str,
    version: &str,
    context: &str,
    allow_xml11_restricted: bool,
) -> Result<()> {
    for ch in text.chars() {
        let code = ch as u32;
        let valid = if version == "1.1" {
            (1..=0xD7FF).contains(&code)
                || (0xE000..=0xFFFD).contains(&code)
                || (0x10000..=0x10FFFF).contains(&code)
        } else {
            matches!(code, 0x9 | 0xA | 0xD)
                || (0x20..=0xD7FF).contains(&code)
                || (0xE000..=0xFFFD).contains(&code)
                || (0x10000..=0x10FFFF).contains(&code)
        };
        if !valid {
            return Err(Error::Eval(format!(
                "Invalid XML {version} character U+{code:04X} in {context}."
            )));
        }
        let restricted_xml11 =
            matches!(code, 0x1..=0x8 | 0xB | 0xC | 0xE..=0x1F | 0x7F..=0x84 | 0x86..=0x9F);
        if version == "1.1" && restricted_xml11 && !allow_xml11_restricted {
            return Err(Error::Eval(format!(
                "XML 1.1 restricted character U+{code:04X} cannot be written in {context}."
            )));
        }
    }
    Ok(())
}

/// A short Pkl rendering of `value` for error messages.
pub(crate) fn display_value(value: &Value) -> String {
    match (kind_of(value), value) {
        (Kind::Null, _) => "null".into(),
        (_, Value::Bool(b)) => b.to_string(),
        (_, Value::Int(n)) => n.to_string(),
        (_, Value::Float(f)) => java_double_to_string(*f),
        (_, Value::String(s)) => format!("{s:?}"),
        (Kind::Duration | Kind::DataSize, Value::Object(map, _)) => {
            let number = match map.get("value") {
                Some(Value::Int(n)) => n.to_string(),
                Some(Value::Float(f)) => java_double_to_string(*f),
                _ => String::new(),
            };
            let unit = map.get("unit").and_then(Value::as_str).unwrap_or_default();
            format!("{number}.{unit}")
        }
        (Kind::Regex, Value::Object(map, _)) => format!(
            "Regex({:?})",
            map.get("pattern")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
        (kind @ Kind::Function, _) => format!("new {} {{}}", kind.pkl_class(value)),
        (_, Value::Object(map, _)) => match map.keys().next() {
            Some(key) => match class_key_name(key) {
                Some(name) => name.to_string(),
                None => "new Dynamic { ... }".into(),
            },
            None => "new Dynamic {}".into(),
        },
        (_, Value::List(_)) => "new Listing { ... }".into(),
        _ => String::new(),
    }
}

/// Format a float as Java's `Double.toString` does, which is how Pkl
/// renders floats: `1.0`, `1.23456789123E8`, `1.0E-5`.
pub(crate) fn java_double_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    // Shortest round-tripping digits and the decimal exponent.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if value < 0.0 { "-" } else { "" };
    if (-3..7).contains(&exponent) {
        let point = exponent + 1;
        let (int_part, frac_part) = if point <= 0 {
            (
                "0".to_string(),
                format!("{}{digits}", "0".repeat((-point) as usize)),
            )
        } else if point as usize >= digits.len() {
            (
                format!("{digits}{}", "0".repeat(point as usize - digits.len())),
                String::new(),
            )
        } else {
            (
                digits[..point as usize].to_string(),
                digits[point as usize..].to_string(),
            )
        };
        let frac_part = if frac_part.is_empty() {
            "0".to_string()
        } else {
            frac_part
        };
        format!("{sign}{int_part}.{frac_part}")
    } else {
        let frac = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{sign}{}.{frac}E{exponent}", &digits[..1])
    }
}

/// Convert a module's output value to JSON for [`crate::eval_to_json`],
/// applying `converters` and refusing what Pkl's `JsonRenderer` refuses.
/// Null properties and entries are kept unless `omit_nulls` (the renderer
/// sets `omitNullProperties`). `top_kind` overrides how the value itself is
/// classified (a module is typed).
#[cfg(feature = "native-io")]
pub(crate) fn to_json(
    value: &Value,
    top_kind: Option<Kind>,
    converters: &Converters,
    omit_nulls: bool,
    invoke: &mut dyn Invoke,
) -> Result<serde_json::Value> {
    let mut state = JsonValue {
        converters,
        invoke,
        path: Vec::new(),
        track_path: converters.has_paths(),
        omit_nulls,
        memo: Default::default(),
    };
    state.push(PathPart::TopLevel);
    let kind = top_kind.unwrap_or_else(|| kind_of(value));
    let (value, kind) = match converters.find(value, kind, &[PathPart::TopLevel]) {
        Some(function) => {
            let converted = state.invoke.invoke(function, value.clone())?;
            let kind = kind_of(&converted);
            (converted, kind)
        }
        None => (value.clone(), kind),
    };
    // Convert first, sharing what no converter changed, then render the
    // converted tree.
    let converted = state.convert_members(&value, kind)?.unwrap_or(value);
    let no_converters = Converters::default();
    let mut render = JsonValue {
        converters: &no_converters,
        invoke: state.invoke,
        path: Vec::new(),
        track_path: false,
        omit_nulls,
        memo: Default::default(),
    };
    render.value(&converted, kind)
}

/// Apply `converters` to `value` and everything under it, as a renderer
/// would before rendering it.
pub(crate) fn convert_value(
    value: &Value,
    top_kind: Option<Kind>,
    converters: &Converters,
    invoke: &mut dyn Invoke,
) -> Result<Value> {
    let mut state = JsonValue {
        converters,
        invoke,
        path: Vec::new(),
        track_path: converters.has_paths(),
        omit_nulls: false,
        memo: Default::default(),
    };
    state.push(PathPart::TopLevel);
    let kind = top_kind.unwrap_or_else(|| kind_of(value));
    let (value, kind) = match converters.find(value, kind, &[PathPart::TopLevel]) {
        Some(function) => {
            let converted = state.invoke.invoke(function, value.clone())?;
            let kind = kind_of(&converted);
            (converted, kind)
        }
        None => (value.clone(), kind),
    };
    Ok(state.convert_members(&value, kind)?.unwrap_or(value))
}

struct JsonValue<'a> {
    converters: &'a Converters,
    invoke: &'a mut dyn Invoke,
    path: Vec<PathPart>,
    track_path: bool,
    /// Whether null members are left out (`to_json` only).
    #[cfg_attr(not(feature = "native-io"), allow(dead_code))]
    omit_nulls: bool,
    /// Converted objects and lists by address (see
    /// [`JsonValue::convert_member`]), each kept with its original so the
    /// address is not reused while the memo is alive.
    memo: rustc_hash::FxHashMap<(usize, usize), (Value, Option<Value>)>,
}

impl JsonValue<'_> {
    fn push(&mut self, part: PathPart) {
        if self.track_path {
            self.path.push(part);
        }
    }

    fn pop(&mut self) {
        if self.track_path {
            self.path.pop();
        }
    }

    /// The converted value, or `None` when no converter applies.
    fn convert_if_needed(&mut self, value: &Value) -> Result<(Option<Value>, Kind)> {
        let kind = kind_of(value);
        if self.converters.is_empty() {
            return Ok((None, kind));
        }
        match self.converters.find(value, kind, &self.path) {
            Some(function) => {
                let converted = self.invoke.invoke(function, value.clone())?;
                let kind = kind_of(&converted);
                Ok((Some(converted), kind))
            }
            None => Ok((None, kind)),
        }
    }

    /// The storage representation of a converted entry key.
    fn entry_storage_key(value: &Value) -> Arc<str> {
        match value {
            Value::String(value) => value.clone(),
            Value::Object(_, _) | Value::List(_) => object_key(value),
            value => crate::value::mapping_storage_key(value)
                .unwrap_or_else(|| display_value(value).into()),
        }
    }

    /// The key an entry key converts to, or `None` if it is unchanged.
    fn convert_entry_key(&mut self, key: &Arc<str>) -> Result<Option<Arc<str>>> {
        let value = key_value(key);
        let kind = kind_of(&value);
        let Some(function) = self.converters.find(&value, kind, &[]) else {
            return Ok(None);
        };
        let converted = self.invoke.invoke(function, value)?;
        let converted = Self::entry_storage_key(&converted);
        Ok((converted != *key).then_some(converted))
    }

    /// Convert `member` and everything under it, or `None` if nothing
    /// changed. A shared object or list (like the same steps under every
    /// hook) converts the same way each time unless converters depend on the
    /// path, so it is converted once.
    fn convert_member(&mut self, member: &Value) -> Result<Option<Value>> {
        let key = match member {
            _ if self.track_path => None,
            Value::Object(map, source) => Some((
                Arc::as_ptr(map) as usize,
                source.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize),
            )),
            Value::List(items) => Some((items.items_ptr() as usize, 0)),
            _ => None,
        };
        if let Some(key) = key
            && let Some((_, converted)) = self.memo.get(&key)
        {
            return Ok(converted.clone());
        }
        let (converted, kind) = self.convert_if_needed(member)?;
        let inner = self.convert_members(converted.as_ref().unwrap_or(member), kind)?;
        let converted = inner.or(converted);
        if let Some(key) = key {
            self.memo.insert(key, (member.clone(), converted.clone()));
        }
        Ok(converted)
    }

    /// Convert the members of `value`. `None` means nothing under it
    /// changed, so the caller can keep sharing `value`.
    fn convert_members(&mut self, value: &Value, kind: Kind) -> Result<Option<Value>> {
        match (kind, value) {
            (Kind::Listing, Value::List(items)) => {
                let mut changed: Option<Vec<Value>> = None;
                for (index, item) in items.iter().enumerate() {
                    self.push(PathPart::Element(index));
                    let converted = self.convert_member(item)?;
                    self.pop();
                    match (&mut changed, converted) {
                        (Some(changed), converted) => {
                            changed.push(converted.unwrap_or_else(|| item.clone()))
                        }
                        (None, Some(converted)) => {
                            let mut list = items[..index].to_vec();
                            list.push(converted);
                            changed = Some(list);
                        }
                        (None, None) => {}
                    }
                }
                Ok(changed.map(|changed| Value::List(ListValue::new(items.kind(), changed))))
            }
            (kind, Value::Object(map, source)) if kind.is_object() => {
                // Entry keys are converted with an empty path before their
                // values. This includes typed keys stored with an internal
                // mapping-key prefix.
                let converts_keys = !self.converters.is_empty();
                let members = (self.track_path || converts_keys)
                    .then(|| Members::new(kind, source.as_deref()));
                let mut changed: Option<ObjectMap> = None;
                for (index, (key, member)) in map.iter().enumerate() {
                    let member_kind = members.as_ref().map(|members| members.kind_of(key));
                    let new_key = if converts_keys && member_kind == Some(MemberKind::Entry) {
                        self.convert_entry_key(key)?
                    } else {
                        None
                    };
                    if self.track_path {
                        let key = new_key.clone().unwrap_or_else(|| key.clone());
                        let key_value = key_value(&key);
                        self.path.push(match member_kind {
                            Some(MemberKind::Entry) => match key_value {
                                Value::String(key) => PathPart::Entry(key),
                                key => PathPart::Key(key),
                            },
                            _ => PathPart::Property(key),
                        });
                    }
                    let converted = if matches!(member, Value::Lambda(..)) {
                        None
                    } else {
                        self.convert_member(member)?
                    };
                    self.pop();
                    if changed.is_none() && (converted.is_some() || new_key.is_some()) {
                        let mut new_map =
                            ObjectMap::with_capacity_and_hasher(map.len(), Default::default());
                        new_map.extend(map.iter().take(index).map(|(k, v)| (k.clone(), v.clone())));
                        changed = Some(new_map);
                    }
                    if let Some(changed) = &mut changed {
                        changed.insert(
                            new_key.unwrap_or_else(|| key.clone()),
                            converted.unwrap_or_else(|| member.clone()),
                        );
                    }
                }
                Ok(changed.map(|map| Value::Object(Arc::new(map), source.clone())))
            }
            _ => Ok(None),
        }
    }

    /// Render a member of an already converted value.
    #[cfg(feature = "native-io")]
    fn member(&mut self, value: &Value) -> Result<(serde_json::Value, Kind)> {
        let kind = kind_of(value);
        Ok((self.value(value, kind)?, kind))
    }

    #[cfg(feature = "native-io")]
    fn value(&mut self, value: &Value, kind: Kind) -> Result<serde_json::Value> {
        if kind == Kind::Typed
            && typed_class_is(value, "pkl:xml", "CommentClass")
            && matches!(value, Value::Object(map, _) if map.get("text").and_then(Value::as_str).is_some_and(|text| text.contains("--") || text.ends_with('-')))
        {
            return Err(Error::Eval(
                "XML comments must not contain `--` or end with `-`.".into(),
            ));
        }
        Ok(match (kind, value) {
            (Kind::Null, _) => serde_json::Value::Null,
            (_, Value::Bool(b)) => serde_json::Value::Bool(*b),
            (_, Value::Int(n)) => serde_json::Value::from(*n),
            (_, Value::Float(f)) => {
                if !f.is_finite() {
                    return Err(Error::Eval(format!(
                        "Cannot render value `{}` as JSON.",
                        java_double_to_string(*f)
                    )));
                }
                serde_json::Value::from(*f)
            }
            (_, Value::String(s)) => serde_json::Value::String(s.to_string()),
            (Kind::RenderDirective, _) => {
                return Err(Error::Eval(
                    "Cannot render a `RenderDirective` as a JSON value.".into(),
                ));
            }
            (Kind::Listing, Value::List(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    self.push(PathPart::Element(index));
                    out.push(self.member(item)?.0);
                    self.pop();
                }
                serde_json::Value::Array(out)
            }
            (kind, Value::Object(map, source)) if kind.is_object() => {
                let members = self
                    .track_path
                    .then(|| Members::new(kind, source.as_deref()));
                let mut out = serde_json::Map::new();
                for (key, member) in map.iter() {
                    if is_non_string_key(key) {
                        return Err(cannot_render_non_string_key(&key_value(key), "JSON"));
                    }
                    // Methods are stored as function-valued members, and an
                    // object without a source cannot tell them from data.
                    if matches!(member, Value::Lambda(..)) {
                        continue;
                    }
                    if let Some(members) = &members {
                        self.path.push(match members.kind_of(key) {
                            MemberKind::Property => PathPart::Property(key.clone()),
                            MemberKind::Entry => PathPart::Entry(key.clone()),
                        });
                    }
                    let (json, kind) = self.member(member)?;
                    if !(self.omit_nulls && kind == Kind::Null) {
                        out.insert(key.to_string(), json);
                    }
                    self.pop();
                }
                serde_json::Value::Object(out)
            }
            (kind, _) => return Err(cannot_render_type(value, kind, "JSON")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_doubles() {
        for (value, expected) in [
            (1.0, "1.0"),
            (1.23, "1.23"),
            (-1.5, "-1.5"),
            (1e10, "1.0E10"),
            (1e7, "1.0E7"),
            (9999999.0, "9999999.0"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (1e-5, "1.0E-5"),
            (123456789.123, "1.23456789123E8"),
            (0.1 + 0.2, "0.30000000000000004"),
            (100.0, "100.0"),
            (-0.0, "-0.0"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(java_double_to_string(value), expected, "{value}");
        }
    }

    #[test]
    fn path_specs() {
        use PathSpec::*;
        assert_eq!(
            parse_path_spec("^apple").unwrap(),
            vec![Property("apple".into()), TopLevel]
        );
        assert_eq!(
            parse_path_spec("hobbies[*]").unwrap(),
            vec![AnyElement, Property("hobbies".into())]
        );
        assert_eq!(
            parse_path_spec("address.*").unwrap(),
            vec![AnyProperty, Property("address".into())]
        );
        assert_eq!(
            parse_path_spec("^[apple]").unwrap(),
            vec![Entry("apple".into()), TopLevel]
        );
        assert_eq!(parse_path_spec("").unwrap(), vec![TopLevel]);
        assert!(parse_path_spec("a..b").is_err());
        assert!(parse_path_spec("a^").is_err());
    }
}
