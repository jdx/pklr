//! `pkl:xml`'s `Renderer`, after pkl-core's `xml.RendererNodes`.

use super::xml_names::{XML10_NAME, XML10_NAME_START, XML11_NAME, XML11_NAME_START};
use super::{
    Kind, StringRenderer, Walk, cannot_render_non_string_key, cannot_render_type,
    java_double_to_string, kind_of, render_directive_text, typed_class_is, validate_xml_characters,
};
use crate::error::{Error, Result};
use crate::value::{ObjectMap, Value};

pub(crate) struct Xml<'a> {
    walk: Walk<'a>,
    out: String,
    indent: String,
    curr_indent: String,
    version: String,
    root_name: String,
    root_attributes: Option<ObjectMap>,
    line_number: usize,
    deferred_key: Option<Value>,
}

impl<'a> Xml<'a> {
    pub(crate) fn new(
        walk: Walk<'a>,
        indent: &str,
        version: &str,
        root_name: &str,
        root_attributes: Option<ObjectMap>,
    ) -> Self {
        Xml {
            walk,
            out: String::new(),
            indent: indent.to_string(),
            curr_indent: String::new(),
            version: version.to_string(),
            root_name: root_name.to_string(),
            root_attributes,
            line_number: 0,
            deferred_key: None,
        }
    }

    pub(crate) fn render(mut self, value: &Value, document: bool) -> Result<String> {
        let converted = self.convert_top_level(value)?;
        if document {
            self.out.push_str("<?xml version=\"");
            let version = self.version.clone();
            self.write_attribute_text(&version)?;
            self.out.push_str("\" encoding=\"UTF-8\"?>");
            if is_element(&converted) {
                self.render_element(&converted)?;
            } else {
                let name = self.root_name.clone();
                let attributes = self.root_attributes.take();
                self.write_element(&name, attributes.as_ref(), &converted, true, true)?;
            }
            self.out.push('\n');
        } else if is_element(&converted) {
            self.render_element(&converted)?;
        } else {
            self.visit(&converted)?;
        }
        Ok(self.out)
    }

    fn start_new_line(&mut self) {
        if self.out.is_empty() {
            return;
        }
        self.line_number += 1;
        self.out.push('\n');
        self.out.push_str(&self.curr_indent);
    }

    fn validate_name(&self, name: &str, kind: &str) -> Result<()> {
        let (start, rest) = if self.version == "1.1" {
            (XML11_NAME_START, XML11_NAME)
        } else {
            (XML10_NAME_START, XML10_NAME)
        };
        let in_table = |table: &[(u16, u16)], unit: u16| {
            table
                .binary_search_by(|&(lo, hi)| {
                    if hi < unit {
                        std::cmp::Ordering::Less
                    } else if lo > unit {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .is_ok()
        };
        let valid = if self.version == "1.1" {
            let mut chars = name.chars();
            let ok = |c: char, table: &[(u16, u16)]| {
                let c = c as u32;
                if c >= 0x10000 {
                    c < 0xF0000
                } else {
                    in_table(table, c as u16)
                }
            };
            chars.next().is_some_and(|c| ok(c, start)) && chars.all(|c| ok(c, rest))
        } else {
            let mut units = name.encode_utf16();
            units.next().is_some_and(|u| in_table(start, u)) && units.all(|u| in_table(rest, u))
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Eval(format!(
                "Invalid XML {} {kind} name: `{name}`",
                self.version
            )))
        }
    }

    fn write_element(
        &mut self,
        name: &str,
        attributes: Option<&ObjectMap>,
        content: &Value,
        block: bool,
        validate: bool,
    ) -> Result<()> {
        if block {
            self.start_new_line();
        }
        if validate {
            self.validate_name(name, "element")?;
        }
        self.out.push('<');
        self.out.push_str(name);
        for (key, value) in attributes.into_iter().flat_map(|map| map.iter()) {
            self.out.push(' ');
            self.validate_name(key, "attribute")?;
            self.out.push_str(key);
            self.out.push_str("=\"");
            let text = match value {
                Value::String(s) => s.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Int(n) => n.to_string(),
                Value::Float(f) => java_double_to_string(*f),
                _ => {
                    return Err(Error::Eval(format!(
                        "Expected value of type `String` or `Boolean` for attribute `{key}` of `{name}`."
                    )));
                }
            };
            self.write_attribute_text(&text)?;
            self.out.push('"');
        }
        self.out.push('>');
        let previous_line = self.line_number;
        self.curr_indent.push_str(&self.indent);
        if is_element(content) {
            let content = element_content(content)?;
            self.visit(&content)?;
        } else {
            self.visit(content)?;
        }
        let len = self.curr_indent.len() - self.indent.len();
        self.curr_indent.truncate(len);
        if previous_line < self.line_number {
            self.start_new_line();
        }
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
        Ok(())
    }

    fn write_text(&mut self, text: &str) -> Result<()> {
        validate_xml_characters(text, &self.version, "XML text", true)?;
        for ch in text.chars() {
            if self.version == "1.1" && is_xml11_restricted(ch) {
                self.out.push_str(&format!("&#x{:X};", ch as u32));
            } else {
                escape_xml_char(ch, &mut self.out);
            }
        }
        Ok(())
    }

    fn write_attribute_text(&mut self, text: &str) -> Result<()> {
        validate_xml_characters(text, &self.version, "XML attribute", true)?;
        for ch in text.chars() {
            if self.version == "1.1" && is_xml11_restricted(ch) {
                self.out.push_str(&format!("&#x{:X};", ch as u32));
            } else if matches!(ch, '\t' | '\n' | '\r') {
                self.out.push_str(match ch {
                    '\t' => "&#x9;",
                    '\n' => "&#xA;",
                    '\r' => "&#xD;",
                    _ => unreachable!(),
                });
            } else {
                escape_xml_char(ch, &mut self.out);
            }
        }
        Ok(())
    }

    fn render_element(&mut self, value: &Value) -> Result<()> {
        let Value::Object(map, _) = value else {
            return Ok(());
        };
        let Some(Value::String(name)) = map.get("name") else {
            return Err(Error::Eval(
                "Expected value of type `String` for `xml.Element` name.".into(),
            ));
        };
        let attributes = match map.get("attributes") {
            Some(Value::Object(attributes, _)) => Some(attributes.clone()),
            _ => None,
        };
        let block = !matches!(map.get("isBlockFormat"), Some(Value::Bool(false)));
        self.write_element(&name.clone(), attributes.as_deref(), value, block, true)
    }

    fn render_inline(&mut self, value: &Value) -> Result<()> {
        if let Value::Object(map, _) = value {
            let inner = map.get("value").cloned().unwrap_or_default();
            self.visit(&inner)?;
        }
        Ok(())
    }

    /// Render a member value that may be an `xml.Element` or `xml.Inline`.
    fn visit_member(&mut self, name: &str, value: &Value, validate: bool) -> Result<()> {
        if is_element(value) {
            self.render_element(value)
        } else if is_inline(value) {
            self.render_inline(value)
        } else {
            self.write_element(name, None, value, true, validate)
        }
    }
}

fn is_element(value: &Value) -> bool {
    matches!(value, Value::Object(map, _) if map.get("_isXmlElement") == Some(&Value::Bool(true)))
}

fn is_inline(value: &Value) -> bool {
    typed_class_is(value, "pkl:xml", "InlineClass")
}

fn is_comment(value: &Value) -> bool {
    typed_class_is(value, "pkl:xml", "CommentClass")
}

fn is_cdata(value: &Value) -> bool {
    typed_class_is(value, "pkl:xml", "CDataClass")
}

/// The name an element of a listing is rendered with: its class name.
fn element_tag(value: &Value) -> String {
    let kind = kind_of(value);
    match (kind, value) {
        (Kind::Typed, Value::Object(_, Some(source))) => source
            .type_name
            .as_deref()
            .map(|name| name.rsplit('.').next().unwrap_or(name).to_string())
            .unwrap_or_else(|| "Typed".into()),
        _ => kind.pkl_class(value),
    }
}

impl<'a> StringRenderer<'a> for Xml<'a> {
    fn walk(&mut self) -> &mut Walk<'a> {
        &mut self.walk
    }

    fn name(&self) -> &'static str {
        "XML"
    }

    fn visit_typed(&mut self, value: &Value) -> Result<bool> {
        let Value::Object(map, _) = value else {
            return Ok(false);
        };
        let text = || map.get("text").and_then(Value::as_str).unwrap_or_default();
        if is_comment(value) {
            if text().contains("--") || text().ends_with('-') {
                return Err(Error::Eval(
                    "XML comments must not contain `--` or end with `-`.".into(),
                ));
            }
            validate_xml_characters(text(), &self.version, "XML comment", false)?;
            if !matches!(map.get("isBlockFormat"), Some(Value::Bool(false))) {
                self.start_new_line();
            }
            self.out.push_str("<!--");
            self.out.push_str(text());
            self.out.push_str("-->");
            return Ok(true);
        }
        if is_cdata(value) {
            validate_xml_characters(text(), &self.version, "XML CDATA", false)?;
            self.out.push_str("<![CDATA[");
            self.out.push_str(&text().replace("]]>", "]]]]><![CDATA[>"));
            self.out.push_str("]]>");
            return Ok(true);
        }
        if is_inline(value) {
            return Err(Error::Eval("`xml.Inline` is not supported here.".into()));
        }
        Ok(false)
    }

    fn visit_null(&mut self) -> Result<()> {
        Err(cannot_render_type(&Value::Null, Kind::Null, "XML"))
    }

    fn visit_bool(&mut self, value: bool) -> Result<()> {
        self.out.push_str(if value { "true" } else { "false" });
        Ok(())
    }

    fn visit_int(&mut self, value: i64) -> Result<()> {
        self.out.push_str(&value.to_string());
        Ok(())
    }

    fn visit_float(&mut self, value: f64) -> Result<()> {
        self.out.push_str(&java_double_to_string(value));
        Ok(())
    }

    fn visit_string(&mut self, value: &str) -> Result<()> {
        self.write_text(value)
    }

    fn visit_render_directive(&mut self, text: &str) -> Result<()> {
        self.out.push_str(text);
        Ok(())
    }

    fn start_object(&mut self, _kind: Kind) -> Result<()> {
        Ok(())
    }

    fn end_object(&mut self, _kind: Kind, _is_empty: bool) -> Result<()> {
        Ok(())
    }

    fn start_listing(&mut self) -> Result<()> {
        Ok(())
    }

    fn end_listing(&mut self, _is_empty: bool) -> Result<()> {
        Ok(())
    }

    fn visit_element(&mut self, _index: usize, value: &Value, _is_first: bool) -> Result<()> {
        let kind = kind_of(value);
        if kind == Kind::Null {
            Ok(())
        } else if is_element(value) {
            self.render_element(value)
        } else if is_inline(value) {
            self.render_inline(value)
        } else if matches!(kind, Kind::String | Kind::Boolean | Kind::Int | Kind::Float)
            || is_comment(value)
            || is_cdata(value)
        {
            self.visit(value)
        } else if kind == Kind::RenderDirective {
            let text = render_directive_text(value)?;
            self.out.push_str(&text);
            Ok(())
        } else {
            let tag = element_tag(value);
            self.write_element(&tag, None, value, true, true)
        }
    }

    fn visit_entry_key(&mut self, key: &Value, _is_first: bool) -> Result<()> {
        self.deferred_key = Some(key.clone());
        Ok(())
    }

    fn visit_entry_value(&mut self, value: &Value) -> Result<()> {
        let key = self.deferred_key.take().unwrap_or_default();
        if is_element(value) || is_inline(value) {
            return self.visit_member("", value, true);
        }
        match (kind_of(&key), &key) {
            (Kind::RenderDirective, _) => {
                let text = render_directive_text(&key)?;
                self.write_element(&text, None, value, true, false)
            }
            (_, Value::String(name)) => self.write_element(name, None, value, true, true),
            _ => Err(cannot_render_non_string_key(&key, "XML")),
        }
    }

    fn visit_property(&mut self, name: &str, value: &Value, _is_first: bool) -> Result<()> {
        self.visit_member(name, value, true)
    }
}

fn is_xml11_restricted(ch: char) -> bool {
    matches!(ch as u32, 0x1..=0x8 | 0xB | 0xC | 0xE..=0x1F | 0x7F..=0x84 | 0x86..=0x9F)
}

fn escape_xml_char(ch: char, out: &mut String) {
    match ch {
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&apos;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '&' => out.push_str("&amp;"),
        ch => out.push(ch),
    }
}

fn element_content(value: &Value) -> Result<Value> {
    let Value::Object(map, source) = value else {
        return Err(Error::Eval("Expected an xml.Element object.".into()));
    };
    let mut content = (**map).clone();
    for key in ["_isXmlElement", "name", "attributes", "isBlockFormat"] {
        content.shift_remove(key);
    }
    Ok(Value::Object(content.into(), source.clone()))
}
