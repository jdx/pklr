//! `pkl:jsonnet`'s `Renderer`, after pkl-core's `jsonnet.RendererNodes`.

use super::{
    Kind, StringRenderer, Walk, cannot_render_non_string_key, java_double_to_string, kind_of,
    render_directive_text, typed_class_is,
};
use crate::error::Result;
use crate::value::Value;

const RESERVED: &[&str] = &[
    "assert",
    "else",
    "error",
    "false",
    "for",
    "function",
    "if",
    "import",
    "importstr",
    "in",
    "local",
    "null",
    "self",
    "super",
    "tailstrict",
    "then",
    "true",
];

pub(crate) struct Jsonnet<'a> {
    walk: Walk<'a>,
    out: String,
    indent: String,
    curr_indent: String,
    inline: bool,
}

impl<'a> Jsonnet<'a> {
    pub(crate) fn new(walk: Walk<'a>, indent: &str) -> Self {
        Jsonnet {
            walk,
            out: String::new(),
            indent: indent.to_string(),
            curr_indent: String::new(),
            inline: indent.is_empty(),
        }
    }

    pub(crate) fn render(mut self, value: &Value, document: bool) -> Result<String> {
        let converted = self.convert_top_level(value)?;
        self.visit(&converted)?;
        if document {
            self.out.push('\n');
        }
        Ok(self.out)
    }

    fn separator(&mut self) {
        self.out.push(if self.inline { ' ' } else { '\n' });
        self.out.push_str(&self.curr_indent);
    }

    fn begin(&mut self, open: char) {
        self.out.push(open);
        self.curr_indent.push_str(&self.indent);
    }

    fn end_value(&mut self, is_empty: bool) {
        let len = self.curr_indent.len() - self.indent.len();
        self.curr_indent.truncate(len);
        if !is_empty && !self.inline {
            self.out.push_str(",\n");
            self.out.push_str(&self.curr_indent);
        }
    }

    fn field_name(&mut self, key: &str) {
        let mut chars = key.chars();
        let is_id = chars
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
            && chars.all(|c| c == '_' || c.is_ascii_alphanumeric());
        if is_id && !RESERVED.contains(&key) {
            self.out.push_str(key);
        } else {
            self.string(key);
        }
        self.out.push_str(": ");
    }

    fn string(&mut self, value: &str) {
        let first_line_starts_with_text = value
            .trim_start_matches('\n')
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace());
        if self.inline || !value.contains('\n') || !first_line_starts_with_text {
            self.quoted(value);
        } else {
            self.out.push_str("|||\n");
            for line in java_lines(value) {
                self.out.push_str(&self.curr_indent);
                self.out.push_str(&self.indent);
                self.out.push_str(line);
                self.out.push('\n');
            }
            self.out.push_str(&self.curr_indent);
            self.out.push_str("|||");
        }
    }

    fn quoted(&mut self, value: &str) {
        let quote = if value.contains('\'') && !value.contains('"') {
            '"'
        } else {
            '\''
        };
        self.out.push(quote);
        for ch in value.chars() {
            match ch {
                '\\' => self.out.push_str("\\\\"),
                '\u{8}' => self.out.push_str("\\b"),
                '\u{c}' => self.out.push_str("\\f"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                ch if ch == quote => {
                    self.out.push('\\');
                    self.out.push(ch);
                }
                ch if (ch as u32) < 0x20 || (0x80..=0x9f).contains(&(ch as u32)) => {
                    self.out.push_str(&format!("\\u{:04x}", ch as u32));
                }
                ch => self.out.push(ch),
            }
        }
        self.out.push(quote);
    }
}

/// The lines of `value` as Java's `String.lines()` splits them.
fn java_lines(value: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        match rest.find(['\n', '\r']) {
            Some(index) => {
                lines.push(&rest[..index]);
                let skip = if rest[index..].starts_with("\r\n") {
                    2
                } else {
                    1
                };
                rest = &rest[index + skip..];
            }
            None => {
                lines.push(rest);
                break;
            }
        }
    }
    lines
}

impl<'a> StringRenderer<'a> for Jsonnet<'a> {
    fn walk(&mut self) -> &mut Walk<'a> {
        &mut self.walk
    }

    fn name(&self) -> &'static str {
        "Jsonnet"
    }

    fn visit_typed(&mut self, value: &Value) -> Result<bool> {
        let Value::Object(map, _) = value else {
            return Ok(false);
        };
        if typed_class_is(value, "pkl:jsonnet", "ImportStrClass") {
            self.out.push_str("importstr ");
            let path = map.get("path").and_then(Value::as_str).unwrap_or_default();
            self.quoted(path);
            return Ok(true);
        }
        if typed_class_is(value, "pkl:jsonnet", "ExtVarClass") {
            self.out.push_str("std.extVar(");
            let name = map.get("name").and_then(Value::as_str).unwrap_or_default();
            self.string(name);
            self.out.push(')');
            return Ok(true);
        }
        Ok(false)
    }

    fn visit_null(&mut self) -> Result<()> {
        self.out.push_str("null");
        Ok(())
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
        self.string(value);
        Ok(())
    }

    fn visit_render_directive(&mut self, text: &str) -> Result<()> {
        self.out.push_str(text);
        Ok(())
    }

    fn start_object(&mut self, _kind: Kind) -> Result<()> {
        self.begin('{');
        Ok(())
    }

    fn end_object(&mut self, _kind: Kind, is_empty: bool) -> Result<()> {
        self.end_value(is_empty);
        self.out
            .push_str(if !is_empty && self.inline { " }" } else { "}" });
        Ok(())
    }

    fn start_listing(&mut self) -> Result<()> {
        self.begin('[');
        Ok(())
    }

    fn end_listing(&mut self, is_empty: bool) -> Result<()> {
        self.end_value(is_empty);
        self.out.push(']');
        Ok(())
    }

    fn visit_element(&mut self, _index: usize, value: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.out.push(',');
        }
        // inline arrays have no separator after the bracket: [1, 2, 3]
        if !is_first || !self.inline {
            self.separator();
        }
        self.visit(value)
    }

    fn visit_entry_key(&mut self, key: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.out.push(',');
        }
        self.separator();
        match (kind_of(key), key) {
            (_, Value::String(s)) => self.field_name(s),
            (Kind::RenderDirective, _) => {
                let text = render_directive_text(key)?;
                self.out.push_str(&text);
                self.out.push_str(": ");
            }
            _ => return Err(cannot_render_non_string_key(key, "Jsonnet")),
        }
        Ok(())
    }

    fn visit_property(&mut self, name: &str, value: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.out.push(',');
        }
        self.separator();
        self.field_name(name);
        self.visit(value)
    }
}
