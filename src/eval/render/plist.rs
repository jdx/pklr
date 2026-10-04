//! `PListRenderer`, after pkl-core's `PListRendererNodes`.

use super::{
    Kind, StringRenderer, Walk, cannot_render_non_string_key, cannot_render_type, display_value,
    java_double_to_string, kind_of, render_directive_text,
};
use crate::error::{Error, Result};
use crate::value::Value;

const NAME: &str = "XML property list";

pub(crate) struct PList<'a> {
    walk: Walk<'a>,
    out: String,
    indent: String,
    curr_indent: String,
}

impl<'a> PList<'a> {
    pub(crate) fn new(walk: Walk<'a>, indent: &str) -> Self {
        PList {
            walk,
            out: String::new(),
            indent: indent.to_string(),
            curr_indent: String::new(),
        }
    }

    pub(crate) fn render(mut self, value: &Value, document: bool) -> Result<String> {
        let converted = self.convert_top_level(value)?;
        if document {
            let kind = self.walk.top_kind.unwrap_or_else(|| kind_of(&converted));
            if !(kind.is_object() || matches!(kind, Kind::Listing | Kind::RenderDirective)) {
                return Err(Error::Eval(format!(
                    "The top-level value of an XML property list must have type `Typed`, `Dynamic`, `Listing`, `Mapping`, `List`, `Set`, or `Map`, but got type `{}`.\nValue: {}",
                    kind.pkl_class(&converted),
                    display_value(&converted)
                )));
            }
            self.out.push_str(concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
                "<plist version=\"1.0\">\n",
            ));
            self.visit(&converted)?;
            self.out.push_str("\n</plist>\n");
        } else {
            self.visit(&converted)?;
        }
        Ok(self.out)
    }

    fn end(&mut self, tag: &str, is_empty: bool) {
        let len = self.curr_indent.len() - self.indent.len();
        self.curr_indent.truncate(len);
        self.out.push_str(&self.curr_indent);
        self.out.push('<');
        if !is_empty {
            self.out.push('/');
        }
        self.out.push_str(tag);
        if is_empty {
            self.out.push('/');
        }
        self.out.push('>');
    }

    fn key(&mut self, name: &str) {
        self.out.push_str(&self.curr_indent);
        self.out.push_str("<key>");
        escape_xml_text(name, &mut self.out);
        self.out.push_str("</key>\n");
        self.out.push_str(&self.curr_indent);
    }
}

/// Escape text for an XML text node.
pub(crate) fn escape_xml_text(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            ch => out.push(ch),
        }
    }
}

impl<'a> StringRenderer<'a> for PList<'a> {
    fn walk(&mut self) -> &mut Walk<'a> {
        &mut self.walk
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn visit_null(&mut self) -> Result<()> {
        Err(cannot_render_type(&Value::Null, Kind::Null, NAME))
    }

    fn visit_bool(&mut self, value: bool) -> Result<()> {
        self.out
            .push_str(if value { "<true/>" } else { "<false/>" });
        Ok(())
    }

    fn visit_int(&mut self, value: i64) -> Result<()> {
        self.out.push_str(&format!("<integer>{value}</integer>"));
        Ok(())
    }

    fn visit_float(&mut self, value: f64) -> Result<()> {
        self.out.push_str("<real>");
        if value.is_nan() {
            self.out.push_str("nan");
        } else if value == f64::INFINITY {
            self.out.push_str("+infinity");
        } else if value == f64::NEG_INFINITY {
            self.out.push_str("-infinity");
        } else {
            self.out.push_str(&java_double_to_string(value));
        }
        self.out.push_str("</real>");
        Ok(())
    }

    fn visit_string(&mut self, value: &str) -> Result<()> {
        self.out.push_str("<string>");
        escape_xml_text(value, &mut self.out);
        self.out.push_str("</string>");
        Ok(())
    }

    fn visit_render_directive(&mut self, text: &str) -> Result<()> {
        self.out.push_str(text);
        Ok(())
    }

    fn visit_other(&mut self, value: &Value, kind: Kind) -> Result<()> {
        match kind {
            Kind::Duration | Kind::DataSize | Kind::Regex => Err(Error::Eval(format!(
                "Cannot render value of type `{}` as {NAME}.\nValue: {}",
                kind.pkl_class(value),
                display_value(value)
            ))),
            _ => Err(cannot_render_type(value, kind, NAME)),
        }
    }

    fn start_object(&mut self, _kind: Kind) -> Result<()> {
        self.curr_indent.push_str(&self.indent);
        Ok(())
    }

    fn end_object(&mut self, _kind: Kind, is_empty: bool) -> Result<()> {
        self.end("dict", is_empty);
        Ok(())
    }

    fn start_listing(&mut self) -> Result<()> {
        self.curr_indent.push_str(&self.indent);
        Ok(())
    }

    fn end_listing(&mut self, is_empty: bool) -> Result<()> {
        self.end("array", is_empty);
        Ok(())
    }

    fn visit_element(&mut self, _index: usize, value: &Value, is_first: bool) -> Result<()> {
        if is_first {
            self.out.push_str("<array>\n");
        }
        self.out.push_str(&self.curr_indent);
        self.visit(value)?;
        self.out.push('\n');
        Ok(())
    }

    fn visit_entry_key(&mut self, key: &Value, is_first: bool) -> Result<()> {
        if is_first {
            self.out.push_str("<dict>\n");
        }
        let text = match (kind_of(key), key) {
            (Kind::RenderDirective, _) => render_directive_text(key)?,
            (_, Value::String(s)) => s.clone(),
            _ => return Err(cannot_render_non_string_key(key, NAME)),
        };
        self.key(&text);
        Ok(())
    }

    fn visit_entry_value(&mut self, value: &Value) -> Result<()> {
        self.visit(value)?;
        self.out.push('\n');
        Ok(())
    }

    fn visit_property(&mut self, name: &str, value: &Value, is_first: bool) -> Result<()> {
        if is_first {
            self.out.push_str("<dict>\n");
        }
        self.key(name);
        self.visit(value)?;
        self.out.push('\n');
        Ok(())
    }
}
