//! `pkl:json`'s `Parser`, after pkl-core's `json.ParserNodes`: a strict
//! JSON parser that builds Pkl values, applying the parser's `converters` to
//! each value as it is completed.

use std::sync::Arc;

use super::render::{Converters, Invoke, Kind, PathPart, kind_of};
use crate::error::{Error, Result};
use crate::value::{ListKind, ListValue, ObjectKind, ObjectMap, ObjectSource, Value};

/// Parse `text` as a JSON document. Objects become `Dynamic`s, or
/// `Mapping`s with `use_mapping`; arrays become `Listing`s.
pub(crate) fn parse_json(
    text: &str,
    use_mapping: bool,
    converters: &Converters,
    invoke: &mut dyn Invoke,
) -> Result<Value> {
    let mut parser = JsonParser {
        src: text.as_bytes(),
        text,
        pos: 0,
        use_mapping,
        converters,
        invoke,
        path: vec![PathPart::TopLevel],
        depth: 0,
    };
    parser.skip_whitespace();
    let value = parser.value()?;
    parser.skip_whitespace();
    if parser.pos < parser.src.len() {
        return Err(parser.error("Unexpected character"));
    }
    parser.convert(value)
}

struct JsonParser<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    use_mapping: bool,
    converters: &'a Converters,
    invoke: &'a mut dyn Invoke,
    path: Vec<PathPart>,
    depth: usize,
}

impl JsonParser<'_> {
    // The upstream parser caps nesting at 1,000. This recursive Rust parser
    // reaches the platform stack limit first, so use a conservative bound to
    // make malformed input return an error rather than aborting the process.
    const MAX_NESTING: usize = 128;

    fn error(&self, message: &str) -> Error {
        let mut end = self.pos.min(self.text.len());
        while end > 0 && !self.text.is_char_boundary(end) {
            end -= 1;
        }
        let line = self.text[..end].bytes().filter(|&b| b == b'\n').count() + 1;
        Error::Eval(format!(
            "Error parsing JSON document.\n\n{message} at line {line}."
        ))
    }

    /// Apply the converter for a parsed value at the current path. A parsed
    /// object is a `Dynamic`, or a `Mapping` with `useMapping`.
    fn convert(&mut self, value: Value) -> Result<Value> {
        if self.converters.is_empty() {
            return Ok(value);
        }
        let kind = match &value {
            Value::Object(..) if self.use_mapping => Kind::Mapping,
            Value::Object(..) => Kind::Dynamic,
            value => kind_of(value),
        };
        match self.converters.find(&value, kind, &self.path) {
            Some(function) => self.invoke.invoke(function, value),
            None => Ok(value),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.src.get(self.pos) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Result<Value> {
        match self.src.get(self.pos) {
            None => Err(self.error("Unexpected end of input")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::String(self.string()?.into())),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("Unexpected character")),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value> {
        if self.src[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("Unexpected character"))
        }
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let from = p.pos;
            while let Some(b'0'..=b'9') = p.src.get(p.pos) {
                p.pos += 1;
            }
            p.pos > from
        };
        if self.src[self.pos] == b'-' {
            self.pos += 1;
        }
        if self.src.get(self.pos) == Some(&b'0') {
            self.pos += 1;
        } else if !digits(self) {
            return Err(self.error("Expected digit"));
        }
        let mut integral = true;
        if self.src.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            integral = false;
            if !digits(self) {
                return Err(self.error("Expected digit"));
            }
        }
        if let Some(b'e' | b'E') = self.src.get(self.pos) {
            self.pos += 1;
            integral = false;
            if let Some(b'+' | b'-') = self.src.get(self.pos) {
                self.pos += 1;
            }
            if !digits(self) {
                return Err(self.error("Expected digit"));
            }
        }
        let text = &self.text[start..self.pos];
        if integral && let Ok(n) = text.parse::<i64>() {
            return Ok(Value::Int(n));
        }
        text.parse::<f64>()
            .map(Value::Float)
            .map_err(|_| self.error(&format!("Cannot parse `{text}` as number")))
    }

    fn string(&mut self) -> Result<String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let rest = &self.text[self.pos..];
            let Some(index) = rest.find(['"', '\\']) else {
                return Err(self.error("Unexpected end of input"));
            };
            let chunk = &rest[..index];
            if chunk.chars().any(|c| (c as u32) < 0x20) {
                return Err(self.error("Expected valid string character"));
            }
            out.push_str(chunk);
            self.pos += index;
            if self.src[self.pos] == b'"' {
                self.pos += 1;
                return Ok(out);
            }
            self.pos += 1; // backslash
            let escape = *self
                .src
                .get(self.pos)
                .ok_or_else(|| self.error("Unexpected end of input"))?;
            self.pos += 1;
            match escape {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let high = self.hex4()?;
                    let ch = if (0xD800..0xDC00).contains(&high)
                        && self.src[self.pos..].starts_with(b"\\u")
                    {
                        let next_escape = self.pos;
                        self.pos += 2;
                        let low = self.hex4()?;
                        if (0xDC00..0xE000).contains(&low) {
                            char::decode_utf16([high, low])
                                .next()
                                .and_then(|c| c.ok())
                                .unwrap_or(char::REPLACEMENT_CHARACTER)
                        } else {
                            // The following escape belongs to the next code
                            // unit; leave it for the next loop iteration.
                            self.pos = next_escape;
                            char::REPLACEMENT_CHARACTER
                        }
                    } else {
                        char::from_u32(u32::from(high)).unwrap_or(char::REPLACEMENT_CHARACTER)
                    };
                    out.push(ch);
                }
                _ => return Err(self.error("Expected valid escape sequence")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u16> {
        let digits = self
            .src
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("Expected hexadecimal digit"))?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return Err(self.error("Expected hexadecimal digit"));
        }
        let digits = std::str::from_utf8(digits).expect("ASCII hexadecimal digits");
        let value = u16::from_str_radix(digits, 16)
            .map_err(|_| self.error("Expected hexadecimal digit"))?;
        self.pos += 4;
        Ok(value)
    }

    fn array(&mut self) -> Result<Value> {
        if self.depth >= Self::MAX_NESTING {
            return Err(self.error("Maximum nesting depth exceeded"));
        }
        self.depth += 1;
        self.pos += 1;
        // Elements are matched by `[*]`, whatever their index.
        self.path.push(PathPart::Element(0));
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.src.get(self.pos) == Some(&b']') {
            self.pos += 1;
        } else {
            loop {
                self.skip_whitespace();
                let item = self.value()?;
                items.push(self.convert(item)?);
                self.skip_whitespace();
                match self.src.get(self.pos) {
                    Some(b',') => self.pos += 1,
                    Some(b']') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Err(self.error("Expected ',' or ']'")),
                }
            }
        }
        self.path.pop();
        self.depth -= 1;
        Ok(Value::List(ListValue::new(ListKind::Listing, items)))
    }

    fn object(&mut self) -> Result<Value> {
        if self.depth >= Self::MAX_NESTING {
            return Err(self.error("Maximum nesting depth exceeded"));
        }
        self.depth += 1;
        self.pos += 1;
        let mut map = ObjectMap::default();
        self.skip_whitespace();
        if self.src.get(self.pos) == Some(&b'}') {
            self.pos += 1;
        } else {
            loop {
                self.skip_whitespace();
                if self.src.get(self.pos) != Some(&b'"') {
                    return Err(self.error("Expected name"));
                }
                let name: Arc<str> = self.string()?.into();
                self.skip_whitespace();
                if self.src.get(self.pos) != Some(&b':') {
                    return Err(self.error("Expected ':'"));
                }
                self.pos += 1;
                self.skip_whitespace();
                self.path.push(PathPart::Property(name.clone()));
                let value = self.value()?;
                let value = self.convert(value)?;
                self.path.pop();
                map.insert(name, value);
                self.skip_whitespace();
                match self.src.get(self.pos) {
                    Some(b',') => self.pos += 1,
                    Some(b'}') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Err(self.error("Expected ',' or '}'")),
                }
            }
        }
        // The object itself is converted by its parent (or as the document).
        let source = ObjectSource {
            entries: Vec::new().into(),
            captured: super::SourceScope::default(),
            body_members: Default::default(),
            is_open: true,
            is_abstract: false,
            type_name: None,
            type_identity: None,
            parent_type_names: Vec::new(),
            parent_type_identities: Vec::new(),
            entry_scopes: Vec::new(),
            evaluated_properties: Vec::new(),
            elements: Vec::new(),
            mapping_value_types: Vec::new(),
            deprecated: Default::default(),
            poisoned_members: None,
            kind: if self.use_mapping {
                ObjectKind::Mapping
            } else {
                ObjectKind::Object
            },
            is_parsed_json: true,
        };
        self.depth -= 1;
        Ok(Value::Object(Arc::new(map), Some(Arc::new(source))))
    }
}
