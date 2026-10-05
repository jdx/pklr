//! `PropertiesRenderer`, after pkl-core's `PropertiesRendererNodes`.

use super::{
    Kind, PathPart, StringRenderer, Walk, cannot_render_type, java_double_to_string, kind_of,
    render_directive_text,
};
use crate::error::{Error, Result};
use crate::value::Value;

pub(crate) struct Properties<'a> {
    walk: Walk<'a>,
    out: String,
    restrict_charset: bool,
    is_document: bool,
}

impl<'a> Properties<'a> {
    pub(crate) fn new(walk: Walk<'a>, restrict_charset: bool) -> Self {
        Properties {
            walk,
            out: String::new(),
            restrict_charset,
            is_document: false,
        }
    }

    pub(crate) fn render(mut self, value: &Value, document: bool) -> Result<String> {
        let converted = self.convert_top_level(value)?;
        let kind = self.walk.top_kind.unwrap_or_else(|| kind_of(&converted));
        let is_object = kind.is_object() || kind == Kind::RenderDirective;
        if document {
            if !is_object {
                return Err(Error::Eval(format!(
                    "The top-level value of a Java properties file must have type `Typed`, `Dynamic`, `Mapping`, or `Map`, but got type `{}`.\nValue: {}",
                    kind.pkl_class(&converted),
                    super::display_value(&converted)
                )));
            }
            self.is_document = kind != Kind::RenderDirective;
        } else if kind.is_object() {
            return Err(cannot_render_type(&converted, kind, "Properties"));
        }
        self.visit(&converted)?;
        Ok(self.out)
    }

    fn write_key(&mut self) {
        let mut first = true;
        for part in &self.walk.path {
            let text = match part {
                PathPart::TopLevel => continue,
                PathPart::Property(name) | PathPart::Entry(name) => {
                    render_key_or_value(name, true, self.restrict_charset)
                }
                PathPart::Element(index) => index.to_string(),
                PathPart::Key(key) => match kind_of(key) {
                    Kind::RenderDirective => render_directive_text(key)
                        .map(|text| text.to_string())
                        .unwrap_or_default(),
                    _ => render_key_or_value(&scalar_text(key), true, self.restrict_charset),
                },
            };
            if !first {
                self.out.push('.');
            }
            self.out.push_str(&text);
            first = false;
        }
    }

    fn visit_property_value(&mut self, value: &str) {
        if self.is_document {
            self.write_key();
            self.out.push_str(" = ");
        }
        let rendered = render_key_or_value(value, false, self.restrict_charset);
        self.out.push_str(&rendered);
        if self.is_document {
            self.out.push('\n');
        }
    }
}

/// Java's `toString` of a scalar key.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => java_double_to_string(*f),
        Value::String(s) => s.to_string(),
        other => super::display_value(other),
    }
}

/// Escape a key or value as `java.util.Properties#store` does (pkl-core's
/// `PropertiesUtils.renderPropertiesKeyOrValue`).
fn render_key_or_value(value: &str, escape_space: bool, restrict_charset: bool) -> String {
    let mut out = String::with_capacity(value.len());
    if !escape_space && value.starts_with(' ') {
        // a leading space of a value is escaped
        out.push('\\');
    }
    for ch in value.chars() {
        let escaped = match ch {
            '\t' => Some('t'),
            '\n' => Some('n'),
            '\u{c}' => Some('f'),
            '\r' => Some('r'),
            ' ' if escape_space => Some(' '),
            '!' | '#' | ':' | '=' | '\\' => Some(ch),
            _ => None,
        };
        if let Some(escaped) = escaped {
            out.push('\\');
            out.push(escaped);
        } else if restrict_charset && !(' '..='~').contains(&ch) {
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04X}"));
            }
        } else {
            out.push(ch);
        }
    }
    out
}

impl<'a> StringRenderer<'a> for Properties<'a> {
    fn walk(&mut self) -> &mut Walk<'a> {
        &mut self.walk
    }

    fn name(&self) -> &'static str {
        "Properties"
    }

    fn visit_null(&mut self) -> Result<()> {
        if self.is_document {
            self.write_key();
            self.out.push_str(" = \n");
        }
        Ok(())
    }

    fn visit_bool(&mut self, value: bool) -> Result<()> {
        self.visit_property_value(if value { "true" } else { "false" });
        Ok(())
    }

    fn visit_int(&mut self, value: i64) -> Result<()> {
        self.visit_property_value(&value.to_string());
        Ok(())
    }

    fn visit_float(&mut self, value: f64) -> Result<()> {
        self.visit_property_value(&java_double_to_string(value));
        Ok(())
    }

    fn visit_string(&mut self, value: &str) -> Result<()> {
        self.visit_property_value(value);
        Ok(())
    }

    fn visit_render_directive(&mut self, text: &str) -> Result<()> {
        if self.is_document {
            self.write_key();
            self.out.push_str(" = ");
        }
        self.out.push_str(text);
        if self.is_document {
            self.out.push('\n');
        }
        Ok(())
    }

    fn start_object(&mut self, _kind: Kind) -> Result<()> {
        Ok(())
    }

    fn end_object(&mut self, _kind: Kind, _is_empty: bool) -> Result<()> {
        Ok(())
    }

    fn start_listing(&mut self) -> Result<()> {
        Err(cannot_render_type(
            &Value::List(Vec::new().into()),
            Kind::Listing,
            "Properties",
        ))
    }

    fn end_listing(&mut self, _is_empty: bool) -> Result<()> {
        Ok(())
    }

    fn visit_element(&mut self, _index: usize, _value: &Value, _is_first: bool) -> Result<()> {
        Ok(())
    }

    fn visit_entry_key(&mut self, _key: &Value, _is_first: bool) -> Result<()> {
        Ok(())
    }

    fn visit_property(&mut self, _name: &str, value: &Value, _is_first: bool) -> Result<()> {
        self.visit(value)
    }
}
