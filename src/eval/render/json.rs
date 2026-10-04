//! `JsonRenderer`, after pkl-core's `JsonRendererNodes`.

use super::{
    Kind, StringRenderer, Walk, cannot_render_non_string_key, java_double_to_string, kind_of,
    render_directive_text,
};
use crate::error::{Error, Result};
use crate::value::Value;

pub(crate) struct Json<'a> {
    walk: Walk<'a>,
    out: String,
    indent: String,
    curr_indent: String,
    separator: &'static str,
}

impl<'a> Json<'a> {
    pub(crate) fn new(walk: Walk<'a>, indent: &str) -> Self {
        Json {
            walk,
            out: String::new(),
            indent: indent.to_string(),
            curr_indent: String::new(),
            separator: if indent.is_empty() { ":" } else { ": " },
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

    fn start_new_line(&mut self) {
        if self.indent.is_empty() {
            return;
        }
        self.out.push('\n');
        self.out.push_str(&self.curr_indent);
    }

    fn begin(&mut self, open: char) {
        self.out.push(open);
        self.curr_indent.push_str(&self.indent);
    }

    fn end(&mut self, close: char, is_empty: bool) {
        let len = self.curr_indent.len() - self.indent.len();
        self.curr_indent.truncate(len);
        if !is_empty {
            self.start_new_line();
        }
        self.out.push(close);
    }
}

/// Escape `value` as pkl-core's `JsonEscaper` does.
pub(crate) fn escape_json(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
}

impl<'a> StringRenderer<'a> for Json<'a> {
    fn walk(&mut self) -> &mut Walk<'a> {
        &mut self.walk
    }

    fn name(&self) -> &'static str {
        "JSON"
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
        if !value.is_finite() {
            return Err(Error::Eval(format!(
                "Cannot render value `{}` as JSON.",
                java_double_to_string(value)
            )));
        }
        self.out.push_str(&java_double_to_string(value));
        Ok(())
    }

    fn visit_string(&mut self, value: &str) -> Result<()> {
        self.out.push('"');
        escape_json(value, &mut self.out);
        self.out.push('"');
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
        self.end('}', is_empty);
        Ok(())
    }

    fn start_listing(&mut self) -> Result<()> {
        self.begin('[');
        Ok(())
    }

    fn end_listing(&mut self, is_empty: bool) -> Result<()> {
        self.end(']', is_empty);
        Ok(())
    }

    fn visit_element(&mut self, _index: usize, value: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.out.push(',');
        }
        self.start_new_line();
        self.visit(value)
    }

    fn visit_entry_key(&mut self, key: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.out.push(',');
        }
        self.start_new_line();
        match (kind_of(key), key) {
            (_, Value::String(s)) => self.visit_string(s)?,
            (Kind::RenderDirective, _) => {
                let text = render_directive_text(key)?;
                self.visit_render_directive(&text)?;
            }
            _ => return Err(cannot_render_non_string_key(key, "JSON")),
        }
        self.out.push_str(self.separator);
        Ok(())
    }

    fn visit_property(&mut self, name: &str, value: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.out.push(',');
        }
        self.start_new_line();
        self.visit_string(name)?;
        self.out.push_str(self.separator);
        self.visit(value)
    }
}
