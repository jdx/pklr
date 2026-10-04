//! `YamlRenderer`, after pkl-core's `YamlRendererNodes` and `YamlEmitter`.

use super::{
    Kind, Settings, StringRenderer, Walk, java_double_to_string, kind_of, render_directive_text,
};
use crate::error::{Error, Result};
use crate::value::Value;

/// Which YAML version strings are quoted for (`YamlRenderer.mode`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Compat,
    Yaml11,
    Yaml12,
}

pub(crate) struct Yaml<'a> {
    walk: Walk<'a>,
    out: String,
    indent: String,
    element_indent: String,
    curr_indent: String,
    mode: Mode,
    is_stream: bool,
}

impl<'a> Yaml<'a> {
    pub(crate) fn new(walk: Walk<'a>, settings: &Settings) -> Self {
        let indent = " ".repeat(settings.yaml_indent_width);
        Yaml {
            walk,
            out: String::new(),
            element_indent: indent[1..].to_string(),
            indent,
            curr_indent: String::new(),
            mode: match settings.yaml_mode.as_str() {
                "1.1" => Mode::Yaml11,
                "1.2" => Mode::Yaml12,
                _ => Mode::Compat,
            },
            is_stream: settings.yaml_is_stream,
        }
    }

    pub(crate) fn render(mut self, value: &Value, document: bool) -> Result<String> {
        let converted = self.convert_top_level(value)?;
        if document {
            if self.is_stream {
                self.visit_stream(&converted)?;
            } else {
                self.visit(&converted)?;
            }
            self.start_new_line();
        } else {
            self.visit(&converted)?;
        }
        Ok(self.out)
    }

    fn visit_stream(&mut self, value: &Value) -> Result<()> {
        let Value::List(items) = value else {
            return Err(Error::Eval(format!(
                "The top-level value of a YAML stream must have type `Listing`, `List`, or `Set`, but got type `{}`.",
                kind_of(value).pkl_class(value)
            )));
        };
        // The documents are rendered as they are, without converters.
        self.walk.top_kind = None;
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                self.start_new_line();
                self.out.push_str("---");
            }
            self.visit(item)?;
        }
        Ok(())
    }

    fn has_enclosing_sequence(&self) -> bool {
        self.walk.enclosing == Some(Kind::Listing)
    }

    fn increase_indent(&mut self) {
        self.curr_indent.push_str(&self.indent);
    }

    fn decrease_indent(&mut self) {
        let len = self.curr_indent.len() - self.indent.len();
        self.curr_indent.truncate(len);
    }

    fn start_new_line(&mut self) {
        if self.out.is_empty() {
            return;
        }
        if !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out.push_str(&self.curr_indent);
    }

    fn undo_start_new_line(&mut self) {
        if self.out.is_empty() {
            return;
        }
        let len = self.out.len() - self.curr_indent.len() - 1;
        self.out.truncate(len);
    }

    fn space(&mut self) {
        if !self.out.is_empty() {
            self.out.push(' ');
        }
    }

    fn emit_double(&mut self, value: f64) {
        if value.is_nan() {
            self.out.push_str(".NaN");
        } else if value == f64::INFINITY {
            self.out.push_str(".Inf");
        } else if value == f64::NEG_INFINITY {
            self.out.push_str("-.Inf");
        } else {
            self.out.push_str(&java_double_to_string(value));
        }
    }

    /// Emit a string scalar, quoting it as little as the mode allows.
    fn emit_string(&mut self, s: &str, is_key: bool) {
        if self.is_reserved_word(s) {
            emit_single_quoted(&mut self.out, s);
            return;
        }
        let chars: Vec<char> = s.chars().collect();
        let length = chars.len();
        let mut needs_escaping = false;
        let mut needs_quoting = false;
        let mut has_non_number_char = false;
        let mut has_newline = false;
        let mut colon_index = None;

        match chars[0] {
            '\n' => has_newline = true,
            '\'' | '!' | '%' | '&' | '*' | '{' | '}' | '[' | ']' | ',' | '#' | '|' | '>' | '@'
            | '`' | '"' | ' ' => needs_quoting = true,
            '-' | ':' | '?' => needs_quoting = length == 1 || chars[1] == ' ',
            '0'..='9' | '+' | '.' | 'o' => {}
            first => {
                needs_escaping = (first as u32) < 0x20;
                has_non_number_char = true;
            }
        }

        for i in 1..length {
            match chars[i] {
                '\n' => has_newline = true,
                '\'' => has_non_number_char = true,
                ' ' => {
                    needs_quoting = needs_quoting || i == length - 1;
                    has_non_number_char = true;
                }
                '[' | ']' | '{' | '}' | ',' => {
                    needs_quoting = needs_quoting || is_key;
                    has_non_number_char = true;
                }
                '#' => {
                    needs_quoting = needs_quoting || chars[i - 1] == ' ';
                    has_non_number_char = true;
                }
                ':' => {
                    if colon_index.is_none() {
                        colon_index = Some(i);
                    }
                    needs_quoting =
                        needs_quoting || i == length - 1 || (i + 1 < length && chars[i + 1] == ' ');
                }
                '0'..='9' | 'A'..='F' | 'a'..='f' | '+' | '-' | '_' | '.' | 'o' | 'x' => {}
                ch => {
                    needs_escaping = needs_escaping || (ch as u32) < 0x20;
                    has_non_number_char = true;
                }
            }
        }

        let pos = self.out.len();
        if needs_escaping {
            self.out.push('"');
            escape_yaml(s, &mut self.out);
            self.out.push('"');
        } else if has_newline {
            self.emit_multiline(s);
        } else if needs_quoting || (!has_non_number_char && self.is_number(s, colon_index)) {
            emit_single_quoted(&mut self.out, s);
        } else {
            self.out.push_str(s);
        }

        // A key longer than 1024 characters or spanning lines must be explicit.
        if is_key
            && (self.out[pos..].encode_utf16().count() > 1024 || (!needs_escaping && has_newline))
        {
            self.out.insert_str(pos, "? ");
            self.out.push('\n');
            self.out.push_str(&self.curr_indent);
        }
    }

    fn emit_multiline(&mut self, s: &str) {
        self.curr_indent.push_str(&self.indent);
        self.out.push('|');
        if s.starts_with(' ') {
            self.out.push_str(&self.indent.len().to_string());
        }
        if let Some(rest) = s.strip_suffix('\n') {
            if rest.is_empty() || rest.ends_with('\n') {
                self.out.push('+');
            }
        } else {
            self.out.push('-');
        }
        self.out.push('\n');

        for line in s.split_inclusive('\n') {
            if line == "\n" {
                // no indent before an empty line
                self.out.push('\n');
            } else {
                self.out.push_str(&self.curr_indent);
                self.out.push_str(line);
            }
        }
        self.decrease_indent();
    }

    fn is_reserved_word(&self, s: &str) -> bool {
        if s.len() > 5 {
            return false;
        }
        let yaml12 = matches!(
            s,
            "" | "~"
                | "null"
                | "Null"
                | "NULL"
                | ".nan"
                | ".NaN"
                | ".NAN"
                | ".inf"
                | ".Inf"
                | ".INF"
                | "+.inf"
                | "+.Inf"
                | "+.INF"
                | "-.inf"
                | "-.Inf"
                | "-.INF"
                | "true"
                | "True"
                | "TRUE"
                | "false"
                | "False"
                | "FALSE"
        );
        yaml12
            || (self.mode != Mode::Yaml12
                && matches!(
                    s,
                    "on" | "On"
                        | "ON"
                        | "off"
                        | "Off"
                        | "OFF"
                        | "y"
                        | "Y"
                        | "yes"
                        | "Yes"
                        | "YES"
                        | "n"
                        | "N"
                        | "no"
                        | "No"
                        | "NO"
                ))
    }

    fn is_number(&self, s: &str, colon_index: Option<usize>) -> bool {
        // Only called for strings of number characters, which are ASCII.
        let b = s.as_bytes();
        match self.mode {
            Mode::Yaml12 => yaml12_is_number(b),
            Mode::Yaml11 | Mode::Compat => {
                let compat = self.mode == Mode::Compat;
                let length = b.len();
                if length == 1 {
                    return b[0].is_ascii_digit() || b[0] == b'.';
                }
                let offset = usize::from(matches!(b[0], b'+' | b'-'));
                if let Some(colon) = colon_index {
                    return is_sexagesimal(b, offset, colon);
                }
                match b.get(offset) {
                    Some(b'o') => return all(b, offset + 1, is_octal_or_underscore),
                    Some(b'0') => {
                        if offset == length - 1 {
                            return true;
                        }
                        match b[offset + 1] {
                            b'b' => return all(b, offset + 2, is_binary_or_underscore),
                            b'o' if compat => return all(b, offset + 2, is_octal_or_underscore),
                            b'x' => return all(b, offset + 2, is_hex_or_underscore),
                            _ => {}
                        }
                    }
                    _ => {}
                }
                if compat {
                    decimal_number(b, offset, true)
                } else {
                    yaml11_decimal_number(b, offset)
                }
            }
        }
    }
}

fn emit_single_quoted(out: &mut String, s: &str) {
    out.push('\'');
    out.push_str(&s.replace('\'', "''"));
    out.push('\'');
}

/// Escape `value` for a double-quoted YAML scalar, as pkl-core's
/// `YamlEscaper` does.
fn escape_yaml(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '\0' => out.push_str("\\0"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\u{1b}' => out.push_str("\\e"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{85}' => out.push_str("\\N"),
            '\u{a0}' => out.push_str("\\_"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", ch as u32)),
            ch => out.push(ch),
        }
    }
}

fn all(b: &[u8], start: usize, pred: fn(u8) -> bool) -> bool {
    start < b.len() && b[start..].iter().all(|&c| pred(c))
}

fn is_decimal_or_underscore(c: u8) -> bool {
    c.is_ascii_digit() || c == b'_'
}

fn is_binary_or_underscore(c: u8) -> bool {
    matches!(c, b'0' | b'1' | b'_')
}

fn is_octal_or_underscore(c: u8) -> bool {
    matches!(c, b'0'..=b'7' | b'_')
}

fn is_hex_or_underscore(c: u8) -> bool {
    c.is_ascii_hexdigit() || c == b'_'
}

/// YAML 1.1's decimal float: an exponent needs a sign.
fn yaml11_decimal_number(b: &[u8], start: usize) -> bool {
    let length = b.len();
    let mut index = start;
    while index < length && is_decimal_or_underscore(b[index]) {
        index += 1;
    }
    if index == length {
        return true;
    }
    if b[index] != b'.' {
        return false;
    }
    index += 1;
    while index < length && is_decimal_or_underscore(b[index]) {
        index += 1;
    }
    if index == length {
        return true;
    }
    if !matches!(b[index], b'e' | b'E') {
        return false;
    }
    index += 1;
    if index + 1 >= length {
        return false;
    }
    if !matches!(b[index], b'-' | b'+') {
        return false;
    }
    index += 1;
    while index < length && b[index].is_ascii_digit() {
        index += 1;
    }
    index == length
}

/// YAML 1.2's decimal number, also allowing underscores in compat mode.
fn decimal_number(b: &[u8], start: usize, underscores: bool) -> bool {
    let digit = |c: u8| c.is_ascii_digit() || (underscores && c == b'_');
    let length = b.len();
    let mut index = start;
    while index < length && digit(b[index]) {
        index += 1;
    }
    if index == length {
        return true;
    }
    if b[index] == b'.' {
        index += 1;
        while index < length && digit(b[index]) {
            index += 1;
        }
        if index == length {
            return true;
        }
    }
    if matches!(b[index], b'e' | b'E') {
        index += 1;
        if index == length {
            return false;
        }
        if matches!(b[index], b'-' | b'+') {
            index += 1;
            if index == length {
                return false;
            }
        }
        while index < length && b[index].is_ascii_digit() {
            index += 1;
        }
    }
    index == length
}

fn yaml12_is_number(b: &[u8]) -> bool {
    let length = b.len();
    if length == 1 {
        return b[0].is_ascii_digit();
    }
    match b[0] {
        b'0' => match b[1] {
            b'o' => all(b, 2, |c| matches!(c, b'0'..=b'7')),
            b'x' => all(b, 2, |c| c.is_ascii_hexdigit()),
            _ => decimal_number(b, 1, false),
        },
        b'-' | b'+' => decimal_number(b, 1, false),
        _ => decimal_number(b, 0, false),
    }
}

fn is_sexagesimal(b: &[u8], start: usize, colon: usize) -> bool {
    let length = b.len();
    if !matches!(b.get(start), Some(b'1'..=b'9')) {
        return false;
    }
    if !b[start + 1..colon]
        .iter()
        .all(|&c| is_decimal_or_underscore(c))
    {
        return false;
    }
    let mut state = 1;
    let mut i = colon + 1;
    while i < length {
        match state {
            0 => match b[i] {
                b':' => state = 1,
                b'.' => state = 3,
                _ => return false,
            },
            1 => match b[i] {
                b'0'..=b'5' => state = 2,
                b'6'..=b'9' => state = 0,
                _ => return false,
            },
            2 => match b[i] {
                b':' => state = 1,
                b'.' => state = 3,
                b'0'..=b'9' => state = 0,
                _ => return false,
            },
            _ => return b[i..].iter().all(|&c| is_decimal_or_underscore(c)),
        }
        i += 1;
    }
    state != 1
}

impl<'a> StringRenderer<'a> for Yaml<'a> {
    fn walk(&mut self) -> &mut Walk<'a> {
        &mut self.walk
    }

    fn name(&self) -> &'static str {
        "YAML"
    }

    fn visit_null(&mut self) -> Result<()> {
        self.space();
        self.out.push_str("null");
        Ok(())
    }

    fn visit_bool(&mut self, value: bool) -> Result<()> {
        self.space();
        self.out.push_str(if value { "true" } else { "false" });
        Ok(())
    }

    fn visit_int(&mut self, value: i64) -> Result<()> {
        self.space();
        self.out.push_str(&value.to_string());
        Ok(())
    }

    fn visit_float(&mut self, value: f64) -> Result<()> {
        self.space();
        self.emit_double(value);
        Ok(())
    }

    fn visit_string(&mut self, value: &str) -> Result<()> {
        self.space();
        self.emit_string(value, false);
        Ok(())
    }

    fn visit_render_directive(&mut self, text: &str) -> Result<()> {
        self.out.push_str(text);
        Ok(())
    }

    fn start_object(&mut self, _kind: Kind) -> Result<()> {
        if self.walk.enclosing.is_some() {
            self.increase_indent();
        }
        if self.has_enclosing_sequence() {
            self.out.push_str(&self.element_indent);
        } else {
            self.start_new_line();
        }
        Ok(())
    }

    fn end_object(&mut self, _kind: Kind, is_empty: bool) -> Result<()> {
        if is_empty {
            if self.has_enclosing_sequence() {
                self.out.push_str("{}");
            } else {
                self.undo_start_new_line();
                self.out.push_str(" {}");
            }
        }
        if self.walk.enclosing.is_some() {
            self.decrease_indent();
        }
        Ok(())
    }

    fn start_listing(&mut self) -> Result<()> {
        if self.has_enclosing_sequence() {
            self.increase_indent();
            self.out.push_str(&self.element_indent);
        } else {
            self.start_new_line();
        }
        Ok(())
    }

    fn end_listing(&mut self, is_empty: bool) -> Result<()> {
        let has_enclosing_sequence = self.has_enclosing_sequence();
        if is_empty {
            if has_enclosing_sequence {
                self.out.push_str("[]");
            } else {
                self.undo_start_new_line();
                self.out.push_str(" []");
            }
        }
        if has_enclosing_sequence {
            self.decrease_indent();
        }
        Ok(())
    }

    fn visit_element(&mut self, _index: usize, value: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.start_new_line();
        }
        self.out.push('-');
        self.visit(value)
    }

    fn visit_entry_key(&mut self, key: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.start_new_line();
        }
        match (kind_of(key), key) {
            (_, Value::String(s)) => self.emit_string(s, true),
            (_, Value::Int(n)) => self.out.push_str(&n.to_string()),
            (_, Value::Float(f)) => self.emit_double(*f),
            (_, Value::Bool(b)) => self.out.push_str(if *b { "true" } else { "false" }),
            (Kind::Null, _) => self.out.push_str("null"),
            (Kind::RenderDirective, _) => {
                let text = render_directive_text(key)?;
                self.out.push_str(&text);
            }
            _ => {
                return Err(Error::Eval(format!(
                    "Cannot render object with non-scalar key as YAML.\nKey   : {}",
                    super::display_value(key)
                )));
            }
        }
        self.out.push(':');
        Ok(())
    }

    fn visit_property(&mut self, name: &str, value: &Value, is_first: bool) -> Result<()> {
        if !is_first {
            self.start_new_line();
        }
        self.emit_string(name, true);
        self.out.push(':');
        self.visit(value)
    }
}
