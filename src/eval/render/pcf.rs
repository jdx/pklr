//! `PcfRenderer`, after pkl-core's `PcfRenderer` and `ValueFormatter`.
//!
//! pklr does not tell `List` from `Listing` or `Map` from `Mapping`, so
//! lists render as `Listing` bodies and maps as `Mapping` bodies.

use super::{Kind, StringRenderer, Walk, display_value, java_double_to_string, kind_of};
use crate::error::{Error, Result};
use crate::value::Value;

pub(crate) struct Pcf<'a> {
    walk: Walk<'a>,
    out: String,
    indent: String,
    curr_indent: String,
    use_custom_delimiters: bool,
    is_document: bool,
    /// For each object being rendered, whether it is the document's
    /// top-level value, which renders without braces.
    open_objects: Vec<bool>,
}

impl<'a> Pcf<'a> {
    pub(crate) fn new(walk: Walk<'a>, indent: &str, use_custom_delimiters: bool) -> Self {
        Pcf {
            walk,
            out: String::new(),
            indent: indent.to_string(),
            curr_indent: String::new(),
            use_custom_delimiters,
            is_document: false,
            open_objects: Vec::new(),
        }
    }

    pub(crate) fn render(mut self, value: &Value, document: bool) -> Result<String> {
        let converted = self.convert_top_level(value)?;
        if document {
            let kind = self.walk.top_kind.unwrap_or_else(|| kind_of(&converted));
            if !matches!(kind, Kind::Typed | Kind::Dynamic | Kind::RenderDirective) {
                return Err(Error::Eval(format!(
                    "The top-level value of a Pcf document must have type `Typed` or `Dynamic`, but got type `{}`.\nValue: {}",
                    kind.pkl_class(&converted),
                    display_value(&converted)
                )));
            }
            self.is_document = true;
        }
        self.visit(&converted)?;
        if document && !self.out.is_empty() {
            self.out.push('\n');
        }
        Ok(self.out)
    }

    fn increase_indent(&mut self) {
        self.curr_indent.push_str(&self.indent);
    }

    fn decrease_indent(&mut self) {
        let len = self.curr_indent.len() - self.indent.len();
        self.curr_indent.truncate(len);
    }

    fn format_string(&mut self, value: &str) {
        let indent = self.curr_indent.clone();
        format_string(
            value,
            &indent,
            true,
            self.use_custom_delimiters,
            &mut self.out,
        );
    }

    /// A value outside an object body: objects get `new`.
    fn visit_standalone(&mut self, value: &Value) -> Result<()> {
        if kind_of(value).is_object() || matches!(value, Value::List(_)) {
            self.out.push_str("new ");
        }
        self.visit(value)
    }

    fn start_body(&mut self) {
        let is_top = self.is_document && self.open_objects.is_empty();
        self.open_objects.push(is_top);
        if !is_top {
            self.increase_indent();
            self.out.push('{');
        }
    }

    fn end_body(&mut self, is_empty: bool) {
        if self.open_objects.pop() == Some(true) {
            return;
        }
        self.decrease_indent();
        if !is_empty {
            self.out.push('\n');
            self.out.push_str(&self.curr_indent);
        }
        self.out.push('}');
    }
}

/// Whether `value` is rendered as an object body, which a property or entry
/// follows with a space rather than ` = `.
fn is_object_like(value: &Value) -> bool {
    let kind = kind_of(value);
    kind.is_object() || matches!(kind, Kind::Listing | Kind::RenderDirective)
}

/// Quote `name` with backticks unless it is a regular identifier.
fn quote_identifier(name: &str) -> String {
    let mut chars = name.chars();
    let regular = match chars.next() {
        Some(first) => {
            (first == '$' || first == '_' || first.is_alphabetic())
                && chars.all(|c| c == '$' || c == '_' || c.is_alphanumeric())
                && !is_keyword(name)
        }
        None => false,
    };
    if regular {
        name.to_string()
    } else {
        format!("`{name}`")
    }
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "_" | "abstract"
            | "amends"
            | "as"
            | "case"
            | "class"
            | "const"
            | "delete"
            | "else"
            | "extends"
            | "external"
            | "false"
            | "fixed"
            | "for"
            | "function"
            | "hidden"
            | "if"
            | "import"
            | "in"
            | "is"
            | "let"
            | "local"
            | "module"
            | "new"
            | "nothing"
            | "null"
            | "open"
            | "out"
            | "outer"
            | "override"
            | "protected"
            | "read"
            | "record"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "trace"
            | "true"
            | "typealias"
            | "unknown"
            | "vararg"
            | "when"
    )
}

/// Format `value` as a Pkl string literal, as pkl-core's `ValueFormatter`
/// does. With `multiline`, a string containing a newline becomes a
/// multiline literal whose lines are indented by `line_indent`.
pub(crate) fn format_string(
    value: &str,
    line_indent: &str,
    multiline: bool,
    custom_delimiters: bool,
    out: &mut String,
) {
    let (is_multiline, pounds_single, pounds_multi) = string_facts(value);
    if multiline && is_multiline {
        let pounds = if custom_delimiters {
            "#".repeat(pounds_multi)
        } else {
            String::new()
        };
        let escape = format!("\\{pounds}");
        out.push_str(&pounds);
        out.push_str("\"\"\"\n");
        out.push_str(line_indent);
        let mut quotes = 0;
        for ch in value.chars() {
            match ch {
                '\n' => {
                    out.push('\n');
                    out.push_str(line_indent);
                    quotes = 0;
                }
                '\r' => {
                    out.push_str(&escape);
                    out.push('r');
                    quotes = 0;
                }
                '\t' => {
                    out.push_str(&escape);
                    out.push('t');
                    quotes = 0;
                }
                '\\' => {
                    out.push_str(if custom_delimiters { "\\" } else { "\\\\" });
                    quotes = 0;
                }
                '"' => {
                    if quotes == 2 && !custom_delimiters {
                        out.push_str("\\\"");
                        quotes = 0;
                    } else {
                        out.push('"');
                        quotes += 1;
                    }
                }
                ch => {
                    out.push(ch);
                    quotes = 0;
                }
            }
        }
        out.push('\n');
        out.push_str(line_indent);
        out.push_str("\"\"\"");
        out.push_str(&pounds);
        return;
    }
    let pounds = if custom_delimiters {
        "#".repeat(pounds_single)
    } else {
        String::new()
    };
    let escape = format!("\\{pounds}");
    out.push_str(&pounds);
    out.push('"');
    let mut rest = value;
    if custom_delimiters {
        if value == "\"" {
            // `#"""#` would start a multiline string
            out.push_str(&escape);
            out.push('"');
            rest = "";
        } else if let Some(tail) = value.strip_prefix("\"\"") {
            out.push('"');
            out.push_str(&escape);
            out.push('"');
            rest = tail;
        }
    }
    for ch in rest.chars() {
        match ch {
            '\n' => {
                out.push_str(&escape);
                out.push('n');
            }
            '\r' => {
                out.push_str(&escape);
                out.push('r');
            }
            '\t' => {
                out.push_str(&escape);
                out.push('t');
            }
            '\\' => out.push_str(if custom_delimiters { "\\" } else { "\\\\" }),
            '"' => out.push_str(if custom_delimiters { "\"" } else { "\\\"" }),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out.push_str(&pounds);
}

/// Whether `value` has a newline, and how many `#`s a custom-delimited
/// single-line and multiline literal of it need.
fn string_facts(value: &str) -> (bool, usize, usize) {
    #[derive(PartialEq)]
    enum Context {
        Other,
        Single,
        Multi,
        Backslash,
    }
    let mut is_multiline = false;
    let mut quotes = 0;
    let mut context = Context::Other;
    let (mut cur_single, mut cur_multi, mut cur_backslash) = (0, 0, 0);
    let (mut max_single, mut max_multi, mut max_backslash) = (0, 0, 0);
    for ch in value.chars() {
        match ch {
            '\\' => {
                context = Context::Backslash;
                cur_backslash = 1;
                max_backslash = max_backslash.max(cur_backslash);
            }
            '"' => {
                quotes += 1;
                if quotes < 3 {
                    context = Context::Single;
                    cur_single = 1;
                    max_single = max_single.max(cur_single);
                } else {
                    context = Context::Multi;
                    cur_multi = 1;
                    max_multi = max_multi.max(cur_multi);
                }
            }
            '#' => {
                quotes = 0;
                match context {
                    Context::Single => {
                        cur_single += 1;
                        max_single = max_single.max(cur_single);
                    }
                    Context::Multi => {
                        cur_multi += 1;
                        max_multi = max_multi.max(cur_multi);
                    }
                    Context::Backslash => {
                        cur_backslash += 1;
                        max_backslash = max_backslash.max(cur_backslash);
                    }
                    Context::Other => {}
                }
            }
            '\n' => {
                is_multiline = true;
                quotes = 0;
                context = Context::Other;
            }
            _ => {
                quotes = 0;
                context = Context::Other;
            }
        }
    }
    (
        is_multiline,
        max_backslash.max(max_single),
        max_backslash.max(max_multi),
    )
}

impl<'a> StringRenderer<'a> for Pcf<'a> {
    fn walk(&mut self) -> &mut Walk<'a> {
        &mut self.walk
    }

    fn name(&self) -> &'static str {
        "Pcf"
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
        self.increase_indent();
        self.format_string(value);
        self.decrease_indent();
        Ok(())
    }

    fn visit_render_directive(&mut self, text: &str) -> Result<()> {
        self.out.push_str(text);
        Ok(())
    }

    fn visit_other(&mut self, value: &Value, kind: Kind) -> Result<()> {
        match (kind, value) {
            (Kind::Duration | Kind::DataSize, _) => self.out.push_str(&display_value(value)),
            (Kind::Regex, Value::Object(map, _)) => {
                let pattern = map
                    .get("pattern")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.out.push_str("Regex(");
                format_string(pattern, "", false, true, &mut self.out);
                self.out.push(')');
            }
            _ => return Err(super::cannot_render_type(value, kind, self.name())),
        }
        Ok(())
    }

    fn start_object(&mut self, _kind: Kind) -> Result<()> {
        self.start_body();
        Ok(())
    }

    fn end_object(&mut self, _kind: Kind, is_empty: bool) -> Result<()> {
        self.end_body(is_empty);
        Ok(())
    }

    fn start_listing(&mut self) -> Result<()> {
        self.start_body();
        Ok(())
    }

    fn end_listing(&mut self, is_empty: bool) -> Result<()> {
        self.end_body(is_empty);
        Ok(())
    }

    fn visit_element(&mut self, _index: usize, value: &Value, _is_first: bool) -> Result<()> {
        self.out.push('\n');
        self.out.push_str(&self.curr_indent);
        match value {
            Value::String(s) => {
                self.format_string(s);
                Ok(())
            }
            _ => self.visit_standalone(value),
        }
    }

    fn visit_entry_key(&mut self, key: &Value, _is_first: bool) -> Result<()> {
        self.out.push('\n');
        self.out.push_str(&self.curr_indent);
        self.out.push('[');
        self.visit_standalone(key)?;
        self.out.push(']');
        Ok(())
    }

    fn visit_entry_value(&mut self, value: &Value) -> Result<()> {
        self.out
            .push_str(if is_object_like(value) { " " } else { " = " });
        self.visit(value)
    }

    fn visit_property(&mut self, name: &str, value: &Value, _is_first: bool) -> Result<()> {
        if !self.out.is_empty() {
            self.out.push('\n');
            self.out.push_str(&self.curr_indent);
        }
        self.out.push_str(&quote_identifier(name));
        self.out
            .push_str(if is_object_like(value) { " " } else { " = " });
        self.visit(value)
    }
}
