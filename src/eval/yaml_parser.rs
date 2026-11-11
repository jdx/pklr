//! `pkl:yaml`'s `Parser`: a YAML 1.1/1.2 parser that builds Pkl values,
//! applying the parser's `converters` to each node as it is completed.
//!
//! It covers block and flow collections, plain, quoted and block scalars,
//! anchors and aliases, the core tags, directives and multiple documents.
//! Every malformed document fails with Pkl's one message.

use std::collections::HashMap;
use std::sync::Arc;

use super::parsers::parsed_object;
use super::render::{Converters, Invoke, Kind, PathPart, kind_of};
use crate::error::{Error, Result};
use crate::value::{ListKind, ListValue, ObjectMap, Value, mapping_storage_key};

/// The YAML version whose scalar rules apply.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Compat,
    V11,
    V12,
}

impl Mode {
    pub(crate) fn parse(mode: &str) -> Option<Mode> {
        match mode {
            "compat" => Some(Mode::Compat),
            "1.1" => Some(Mode::V11),
            "1.2" => Some(Mode::V12),
            _ => None,
        }
    }
}

/// Parse every document in `text`.
pub(crate) fn parse_yaml(
    text: &str,
    mode: Mode,
    use_mapping: bool,
    converters: &Converters,
    invoke: &mut dyn Invoke,
) -> Result<Vec<Value>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut parser = Yaml {
        s: text.chars().collect(),
        pos: 0,
        mode,
        use_mapping,
        converters,
        invoke,
        path: vec![PathPart::TopLevel],
        anchors: HashMap::new(),
        tag_handles: HashMap::new(),
        in_key: false,
        key_original: None,
    };
    parser.documents()
}

fn fail<T>() -> Result<T> {
    Err(Error::Eval("Error parsing YAML document.".into()))
}

/// Anchor and tag of a node.
#[derive(Default)]
struct Props {
    anchor: Option<String>,
    tag: Option<String>,
}

impl Props {
    fn is_empty(&self) -> bool {
        self.anchor.is_none() && self.tag.is_none()
    }
}

/// A node before its tag and converter apply.
enum Body {
    /// Scalar text; `true` when it was quoted or a block scalar, which
    /// resolves to a string.
    Scalar(String, bool),
    Seq(Vec<Value>),
    Map(Vec<(Value, Value)>),
    /// An alias: the value is final.
    Alias(Value),
}

struct Raw {
    props: Props,
    body: Body,
}

struct Yaml<'a> {
    s: Vec<char>,
    pos: usize,
    mode: Mode,
    use_mapping: bool,
    converters: &'a Converters,
    invoke: &'a mut dyn Invoke,
    path: Vec<PathPart>,
    anchors: HashMap<String, Value>,
    /// Tag handles declared by `%TAG` in the current document.
    tag_handles: HashMap<String, String>,
    /// Set while parsing an explicit `?` key: only class converters apply.
    in_key: bool,
    key_original: Option<Value>,
}

fn is_blank(c: Option<char>) -> bool {
    matches!(c, None | Some(' ' | '\t' | '\n'))
}

impl Yaml<'_> {
    // The recursive descent is bounded to avoid exhausting the stack.
    const MAX_NESTING: usize = 128;

    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.s.get(self.pos + n).copied()
    }

    fn col(&self) -> usize {
        let mut i = self.pos;
        while i > 0 && self.s[i - 1] != '\n' {
            i -= 1;
        }
        self.pos - i
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn at_doc_marker(&self) -> bool {
        self.col() == 0
            && (self.s[self.pos..].starts_with(&['-', '-', '-'][..])
                || self.s[self.pos..].starts_with(&['.', '.', '.'][..]))
            && is_blank(self.peek_at(3))
    }

    fn at_comment(&self) -> bool {
        self.peek() == Some('#') && (self.pos == 0 || is_blank(Some(self.s[self.pos - 1])))
    }

    fn skip_comment(&mut self) {
        while !matches!(self.peek(), None | Some('\n')) {
            self.pos += 1;
        }
    }

    fn at_eol(&self) -> bool {
        matches!(self.peek(), None | Some('\n')) || self.at_comment()
    }

    /// Skip blanks, comments and line breaks up to the next content. Unless
    /// a line break was crossed (or `allow_inline`), content on the current
    /// line is an error.
    fn skip_to_content(&mut self, allow_inline: bool) -> Result<()> {
        let mut moved = allow_inline;
        loop {
            self.skip_spaces();
            if self.at_comment() {
                self.skip_comment();
            }
            match self.peek() {
                Some('\n') => {
                    self.pos += 1;
                    moved = true;
                }
                None => return Ok(()),
                _ if moved => return Ok(()),
                _ => return fail(),
            }
        }
    }

    // ---- documents ----

    fn documents(&mut self) -> Result<Vec<Value>> {
        let mut docs = Vec::new();
        let mut bare_ok = true;
        loop {
            self.skip_to_content(true)?;
            let mut directive = false;
            self.tag_handles.clear();
            while self.col() == 0 && self.peek() == Some('%') {
                let start = self.pos;
                while self.peek().is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
                let line: String = self.s[start..self.pos].iter().collect();
                let mut words = line.split_whitespace();
                if words.next() == Some("%TAG") {
                    if let (Some(handle), Some(prefix)) = (words.next(), words.next()) {
                        self.tag_handles
                            .insert(handle.to_string(), prefix.to_string());
                    } else {
                        return fail();
                    }
                }
                self.skip_to_content(true)?;
                directive = true;
            }
            if self.peek().is_none() {
                if directive {
                    return fail();
                }
                break;
            }
            let explicit = self.at_doc_marker() && self.peek() == Some('-');
            if explicit {
                self.pos += 3;
            } else if directive || !bare_ok {
                return fail();
            } else if self.at_doc_marker() {
                // A stray `...`: an empty stream tail.
                self.pos += 3;
                self.skip_to_content(true)?;
                continue;
            }
            self.anchors.clear();
            self.path.truncate(1);
            let value = self.block_node(-1, false, false, 0)?;
            docs.push(value);
            self.skip_to_content(true)?;
            if self.peek().is_none() {
                break;
            }
            if !self.at_doc_marker() {
                return fail();
            }
            if self.peek() == Some('.') {
                self.pos += 3;
                bare_ok = true;
            } else {
                bare_ok = false;
            }
        }
        Ok(docs)
    }

    // ---- block structure ----

    /// A node after an indicator (or at the start of a document). Its
    /// content must be indented more than `parent`; a block sequence may sit
    /// at `parent` when `seq_ok`. `no_inline_map` rejects a block mapping
    /// that starts on the indicator's line.
    fn block_node(
        &mut self,
        parent: isize,
        seq_ok: bool,
        no_inline_map: bool,
        depth: usize,
    ) -> Result<Value> {
        if depth > Self::MAX_NESTING {
            return fail();
        }
        let mut props = Props::default();
        let mut moved = false;
        let mut own_line = false;
        self.skip_spaces();
        self.read_props(&mut props)?;
        if self.at_eol() {
            own_line = !props.is_empty();
            self.skip_to_content(true)?;
            moved = true;
            let seq_here = self.peek() == Some('-') && is_blank(self.peek_at(1));
            let col = self.col() as isize;
            if self.peek().is_none()
                || self.at_doc_marker()
                || col < parent
                || (col == parent && !(seq_ok && seq_here))
            {
                return self.finish(Raw {
                    props,
                    body: Body::Scalar(String::new(), false),
                });
            }
            if props.is_empty() {
                self.read_props(&mut props)?;
                if self.at_eol() {
                    own_line = !props.is_empty();
                    self.skip_to_content(true)?;
                }
            }
        }
        let c = self.col();
        let body = match self.peek() {
            Some('-') if is_blank(self.peek_at(1)) => Body::Seq(self.block_seq(c, depth)?),
            Some('?') if is_blank(self.peek_at(1)) => Body::Map(self.block_map(c, None, depth)?),
            Some(ind @ ('|' | '>')) => Body::Scalar(self.block_scalar(ind == '>', parent)?, true),
            _ => {
                let mut raw = self.inline_raw(props, false, depth)?;
                if own_line {
                    // Properties on their own line belong to the collection.
                    let props = std::mem::take(&mut raw.props);
                    if self.at_key_sep() {
                        return self.map_from_key(raw, props, c, no_inline_map, moved, depth);
                    }
                    raw.props = props;
                } else if self.at_key_sep() {
                    return self.map_from_key(
                        raw,
                        Props::default(),
                        c,
                        no_inline_map,
                        moved,
                        depth,
                    );
                }
                if let Body::Scalar(text, false) = &mut raw.body {
                    self.extend_plain(text, parent);
                }
                return self.finish(raw);
            }
        };
        self.finish(Raw { props, body })
    }

    fn map_from_key(
        &mut self,
        key: Raw,
        map_props: Props,
        c: usize,
        no_inline_map: bool,
        moved: bool,
        depth: usize,
    ) -> Result<Value> {
        if no_inline_map && !moved {
            return fail();
        }
        let key = self.finish_key(key)?;
        let entries = self.block_map(c, Some(key), depth)?;
        self.finish(Raw {
            props: map_props,
            body: Body::Map(entries),
        })
    }

    /// Whether the cursor is at a `:` that separates a key from its value.
    fn at_key_sep(&mut self) -> bool {
        self.skip_spaces();
        self.peek() == Some(':') && is_blank(self.peek_at(1))
    }

    fn read_props(&mut self, props: &mut Props) -> Result<()> {
        loop {
            match self.peek() {
                Some('&') => {
                    self.pos += 1;
                    let name = self.token();
                    if name.is_empty() {
                        return fail();
                    }
                    props.anchor = Some(name);
                }
                Some('!') => props.tag = Some(self.read_tag()?),
                _ => return Ok(()),
            }
            self.skip_spaces();
            if self.at_eol() {
                return Ok(());
            }
        }
    }

    fn token(&mut self) -> String {
        let start = self.pos;
        while !is_blank(self.peek()) && !matches!(self.peek(), Some(',' | '[' | ']' | '{' | '}')) {
            self.pos += 1;
        }
        self.s[start..self.pos].iter().collect()
    }

    fn read_tag(&mut self) -> Result<String> {
        self.pos += 1;
        if self.peek() == Some('<') {
            let start = self.pos + 1;
            while self.peek().is_some_and(|c| c != '>') {
                self.pos += 1;
            }
            if self.peek() != Some('>') {
                return fail();
            }
            let tag: String = self.s[start..self.pos].iter().collect();
            self.pos += 1;
            return Ok(Self::normalize_tag(tag));
        }
        let name = self.token();
        // `!handle!suffix` (including `!!suffix`) expands through `%TAG`.
        let split = name
            .rfind('!')
            .map(|i| (format!("!{}", &name[..=i]), &name[i + 1..]));
        match split {
            Some((handle, suffix)) => match self.tag_handles.get(&handle) {
                Some(prefix) => Ok(Self::normalize_tag(format!("{prefix}{suffix}"))),
                None if handle == "!!" => Ok(format!("!!{suffix}")),
                None => fail(),
            },
            // `!suffix` expands through a declared primary handle.
            None => match self.tag_handles.get("!") {
                Some(prefix) if !name.is_empty() => {
                    Ok(Self::normalize_tag(format!("{prefix}{name}")))
                }
                _ => Ok(format!("!{name}")),
            },
        }
    }

    fn normalize_tag(tag: String) -> String {
        tag.strip_prefix("tag:yaml.org,2002:")
            .map(|t| format!("!!{t}"))
            .unwrap_or(tag)
    }

    fn block_seq(&mut self, c: usize, depth: usize) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        self.path.push(PathPart::Element(0));
        loop {
            self.pos += 1; // `-`
            items.push(self.block_node(c as isize, false, false, depth + 1)?);
            self.skip_to_content(true)?;
            if self.peek().is_none() || self.at_doc_marker() {
                break;
            }
            let col = self.col();
            if col < c {
                break;
            }
            if col > c {
                return fail();
            }
            if !(self.peek() == Some('-') && is_blank(self.peek_at(1))) {
                break;
            }
        }
        self.path.pop();
        Ok(items)
    }

    fn block_map(
        &mut self,
        c: usize,
        mut first: Option<(Value, Value)>,
        depth: usize,
    ) -> Result<Vec<(Value, Value)>> {
        let mut entries = Vec::new();
        loop {
            let key = match first.take() {
                Some(key) => key,
                None if self.peek() == Some('?') && is_blank(self.peek_at(1)) => {
                    self.pos += 1;
                    self.in_key = true;
                    self.key_original = None;
                    let converted = self.block_node(c as isize, false, false, depth + 1);
                    self.in_key = false;
                    let converted = converted?;
                    let original = self
                        .key_original
                        .take()
                        .unwrap_or_else(|| converted.clone());
                    let key = (converted, original);
                    self.skip_to_content(true)?;
                    if self.col() == c && self.peek() == Some(':') && is_blank(self.peek_at(1)) {
                        entries.push(self.map_value(key, c, depth)?);
                    } else {
                        entries.push((key.0, Value::Null));
                    }
                    if self.next_map_entry(c)? {
                        continue;
                    }
                    break;
                }
                None => {
                    let mut props = Props::default();
                    self.read_props(&mut props)?;
                    let raw = self.inline_raw(props, false, depth)?;
                    if !self.at_key_sep() {
                        return fail();
                    }
                    self.finish_key(raw)?
                }
            };
            self.skip_spaces();
            if self.peek() != Some(':') {
                return fail();
            }
            entries.push(self.map_value(key, c, depth)?);
            if !self.next_map_entry(c)? {
                break;
            }
        }
        Ok(entries)
    }

    /// At `:`: parse the entry's value.
    fn map_value(&mut self, key: (Value, Value), c: usize, depth: usize) -> Result<(Value, Value)> {
        self.pos += 1;
        self.path_push_key(&key.1);
        let value = self.block_node(c as isize, true, true, depth + 1)?;
        self.path.pop();
        Ok((key.0, value))
    }

    /// After an entry: whether another entry of the mapping at `c` follows.
    fn next_map_entry(&mut self, c: usize) -> Result<bool> {
        self.skip_to_content(true)?;
        if self.peek().is_none() || self.at_doc_marker() {
            return Ok(false);
        }
        let col = self.col();
        if col < c {
            return Ok(false);
        }
        if col > c {
            return fail();
        }
        Ok(!(self.peek() == Some('-') && is_blank(self.peek_at(1))))
    }

    fn path_push_key(&mut self, key: &Value) {
        self.path.push(match key {
            Value::String(name) if self.use_mapping => PathPart::Entry(name.clone()),
            Value::String(name) => PathPart::Property(name.clone()),
            other => PathPart::Key(other.clone()),
        });
    }

    // ---- scalars and inline nodes ----

    /// A flow collection, quoted or plain scalar, or alias.
    fn inline_raw(&mut self, mut props: Props, in_flow: bool, depth: usize) -> Result<Raw> {
        if depth > Self::MAX_NESTING {
            return fail();
        }
        let body = match self.peek() {
            Some('*') => {
                self.pos += 1;
                let name = self.token();
                match self.anchors.get(&name) {
                    Some(value) if props.is_empty() => Body::Alias(value.clone()),
                    _ => return fail(),
                }
            }
            Some('[') => Body::Seq(self.flow_seq(depth)?),
            Some('{') => Body::Map(self.flow_map(depth)?),
            Some('"') => Body::Scalar(self.double_quoted()?, true),
            Some('\'') => Body::Scalar(self.single_quoted()?, true),
            Some(c) if "]},#&!|>%@`".contains(c) => return fail(),
            Some(':' | '-' | '?') if is_blank(self.peek_at(1)) => return fail(),
            None | Some('\n') => return fail(),
            _ => {
                let text = self.plain_segment(in_flow);
                if text.is_empty() {
                    return fail();
                }
                let mut text = text;
                if in_flow {
                    self.extend_flow_plain(&mut text);
                }
                Body::Scalar(text, false)
            }
        };
        let props = std::mem::take(&mut props);
        Ok(Raw { props, body })
    }

    /// One line of a plain scalar.
    fn plain_segment(&mut self, in_flow: bool) -> String {
        let start = self.pos;
        loop {
            match self.peek() {
                None | Some('\n') => break,
                Some(':')
                    if is_blank(self.peek_at(1))
                        || (in_flow
                            && matches!(self.peek_at(1), Some(',' | '[' | ']' | '{' | '}'))) =>
                {
                    break;
                }
                Some('#') if self.pos > start && is_blank(Some(self.s[self.pos - 1])) => break,
                Some(',' | '[' | ']' | '{' | '}') if in_flow => break,
                Some(_) => self.pos += 1,
            }
        }
        let text: String = self.s[start..self.pos].iter().collect();
        text.trim_end_matches([' ', '\t']).to_string()
    }

    /// Fold the continuation lines of a plain scalar inside a flow
    /// collection.
    fn extend_flow_plain(&mut self, text: &mut String) {
        loop {
            let save = self.pos;
            self.skip_spaces();
            if self.peek() != Some('\n') {
                self.pos = save;
                return;
            }
            let mut blanks = 0;
            while self.peek() == Some('\n') {
                self.pos += 1;
                self.skip_spaces();
                if self.peek() == Some('\n') {
                    blanks += 1;
                }
            }
            let ends = match self.peek() {
                None | Some(',' | '[' | ']' | '{' | '}') => true,
                Some(':') => is_blank(self.peek_at(1)),
                _ => self.at_comment() || self.at_doc_marker(),
            };
            if ends {
                self.pos = save;
                return;
            }
            let segment = self.plain_segment(true);
            if segment.is_empty() {
                self.pos = save;
                return;
            }
            if blanks == 0 {
                text.push(' ');
            } else {
                text.push_str(&"\n".repeat(blanks));
            }
            text.push_str(&segment);
        }
    }

    /// Fold the continuation lines of a block-context plain scalar.
    fn extend_plain(&mut self, text: &mut String, parent: isize) {
        loop {
            let save = self.pos;
            self.skip_spaces();
            if self.peek() != Some('\n') {
                self.pos = save;
                return;
            }
            let mut blanks = 0;
            let mut q = self.pos;
            loop {
                // At a line break: look at the next line.
                q += 1;
                let mut r = q;
                while matches!(self.s.get(r), Some(' ' | '\t')) {
                    r += 1;
                }
                if self.s.get(r) == Some(&'\n') {
                    blanks += 1;
                    q = r;
                    continue;
                }
                q = r;
                break;
            }
            self.pos = q;
            let ok = self.peek().is_some()
                && (self.col() as isize) > parent
                && !self.at_doc_marker()
                && !self.at_comment()
                && !(self.peek() == Some(':') && is_blank(self.peek_at(1)));
            if !ok {
                self.pos = save;
                return;
            }
            let segment = self.plain_segment(false);
            if segment.is_empty() {
                self.pos = save;
                return;
            }
            if blanks == 0 {
                text.push(' ');
            } else {
                text.push_str(&"\n".repeat(blanks));
            }
            text.push_str(&segment);
        }
    }

    fn double_quoted(&mut self) -> Result<String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return fail(),
                Some('"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\n') => self.fold_quoted_break(&mut out)?,
                Some('\\') => {
                    self.pos += 1;
                    let Some(escape) = self.peek() else {
                        return fail();
                    };
                    self.pos += 1;
                    match escape {
                        '0' => out.push('\0'),
                        'a' => out.push('\u{7}'),
                        'b' => out.push('\u{8}'),
                        't' | '\t' => out.push('\t'),
                        'n' => out.push('\n'),
                        'v' => out.push('\u{b}'),
                        'f' => out.push('\u{c}'),
                        'r' => out.push('\r'),
                        'e' => out.push('\u{1b}'),
                        ' ' => out.push(' '),
                        '"' => out.push('"'),
                        '/' => out.push('/'),
                        '\\' => out.push('\\'),
                        'N' => out.push('\u{85}'),
                        '_' => out.push('\u{a0}'),
                        'L' => out.push('\u{2028}'),
                        'P' => out.push('\u{2029}'),
                        'x' => out.push(self.hex_escape(2)?),
                        'u' => out.push(self.hex_escape(4)?),
                        'U' => out.push(self.hex_escape(8)?),
                        '\n' => {
                            // An escaped line break joins the lines.
                            self.skip_spaces();
                        }
                        _ => return fail(),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn hex_escape(&mut self, digits: usize) -> Result<char> {
        let mut value = 0u32;
        for _ in 0..digits {
            let Some(d) = self.peek().and_then(|c| c.to_digit(16)) else {
                return fail();
            };
            value = value * 16 + d;
            self.pos += 1;
        }
        char::from_u32(value).map_or_else(fail, Ok)
    }

    fn single_quoted(&mut self) -> Result<String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return fail(),
                Some('\'') if self.peek_at(1) == Some('\'') => {
                    out.push('\'');
                    self.pos += 2;
                }
                Some('\'') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\n') => self.fold_quoted_break(&mut out)?,
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// A line break inside a quoted scalar folds to a space, or to newlines
    /// when blank lines follow.
    fn fold_quoted_break(&mut self, out: &mut String) -> Result<()> {
        while out.ends_with([' ', '\t']) {
            out.pop();
        }
        let mut breaks = 0;
        while self.peek() == Some('\n') {
            self.pos += 1;
            breaks += 1;
            self.skip_spaces();
        }
        if self.peek().is_none() {
            return fail();
        }
        if breaks == 1 {
            out.push(' ');
        } else {
            out.push_str(&"\n".repeat(breaks - 1));
        }
        Ok(())
    }

    /// A literal (`|`) or folded (`>`) block scalar. The cursor is at the
    /// indicator and ends at the start of the line after the scalar.
    fn block_scalar(&mut self, folded: bool, parent: isize) -> Result<String> {
        self.pos += 1;
        let mut chomp = '\0';
        let mut explicit = None;
        while let Some(c) = self.peek() {
            match c {
                '+' | '-' if chomp == '\0' => chomp = c,
                '1'..='9' if explicit.is_none() => explicit = c.to_digit(10),
                _ => break,
            }
            self.pos += 1;
        }
        self.skip_spaces();
        if !self.at_eol() {
            return fail();
        }
        self.skip_comment();
        if self.peek() == Some('\n') {
            self.pos += 1;
        }
        let base = parent.max(0) as usize;
        let mut indent = explicit.map(|d| base + d as usize);
        // (text, terminated by a line break)
        let mut lines: Vec<(String, bool)> = Vec::new();
        loop {
            let start = self.pos;
            if start >= self.s.len() {
                break;
            }
            let mut end = start;
            while end < self.s.len() && self.s[end] != '\n' {
                end += 1;
            }
            let line = &self.s[start..end];
            let terminated = end < self.s.len();
            let lead = line.iter().take_while(|&&c| c == ' ').count();
            let blank = lead == line.len();
            if !blank && indent.is_none() {
                if (lead as isize) <= parent {
                    break;
                }
                indent = Some(lead);
            }
            if !blank {
                if lead < indent.unwrap_or(0) {
                    break;
                }
                if lead == 0 && self.at_doc_marker() {
                    break;
                }
            }
            let text: String = match indent {
                Some(n) if line.len() > n => line[n..].iter().collect(),
                _ => String::new(),
            };
            lines.push((text, terminated));
            self.pos = if terminated { end + 1 } else { end };
        }
        // Trailing empty lines, apart from the chomping.
        let mut trailing = 0;
        while lines.last().is_some_and(|(text, _)| text.is_empty()) {
            lines.pop();
            trailing += 1;
        }
        let last_terminated = lines.last().is_none_or(|(_, t)| *t);
        let mut out = String::new();
        if folded {
            let mut prev: Option<&str> = None;
            let mut pending = 0;
            for (text, _) in &lines {
                if text.is_empty() {
                    pending += 1;
                    continue;
                }
                if let Some(prev) = prev {
                    let more = |t: &str| t.starts_with([' ', '\t']);
                    if more(prev) || more(text) {
                        out.push_str(&"\n".repeat(pending + 1));
                    } else if pending == 0 {
                        out.push(' ');
                    } else {
                        out.push_str(&"\n".repeat(pending));
                    }
                } else {
                    out.push_str(&"\n".repeat(pending));
                }
                pending = 0;
                out.push_str(text);
                prev = Some(text);
            }
        } else {
            let joined: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
            out = joined.join("\n");
        }
        if !lines.is_empty() {
            match chomp {
                '-' => {}
                '+' => {
                    if last_terminated {
                        out.push_str(&"\n".repeat(1 + trailing));
                    }
                }
                _ => {
                    if last_terminated {
                        out.push('\n');
                    }
                }
            }
        } else if chomp == '+' {
            out.push_str(&"\n".repeat(trailing));
        }
        Ok(out)
    }

    // ---- flow collections ----

    fn skip_flow_space(&mut self) {
        loop {
            self.skip_spaces();
            if self.at_comment() {
                self.skip_comment();
            }
            if self.peek() == Some('\n') {
                self.pos += 1;
            } else {
                return;
            }
        }
    }

    /// A node inside a flow collection, unfinished.
    fn flow_raw(&mut self, depth: usize) -> Result<Raw> {
        self.skip_flow_space();
        let mut props = Props::default();
        self.read_props(&mut props)?;
        self.skip_flow_space();
        if matches!(self.peek(), Some(',' | ']' | '}' | ':')) {
            return Ok(Raw {
                props,
                body: Body::Scalar(String::new(), false),
            });
        }
        self.inline_raw(props, true, depth)
    }

    fn flow_seq(&mut self, depth: usize) -> Result<Vec<Value>> {
        self.pos += 1;
        let mut items = Vec::new();
        self.path.push(PathPart::Element(0));
        loop {
            self.skip_flow_space();
            match self.peek() {
                None => return fail(),
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            if self.peek() == Some(',') {
                return fail();
            }
            if self.peek() == Some('?') && is_blank(self.peek_at(1)) {
                self.pos += 1;
            }
            let raw = self.flow_raw(depth + 1)?;
            self.skip_flow_space();
            if self.peek() == Some(':') {
                // A single-pair mapping.
                self.pos += 1;
                let key = self.finish_key(raw)?;
                self.path_push_key(&key.1);
                let value = self.flow_value(depth)?;
                self.path.pop();
                let pair = self.finish(Raw {
                    props: Props::default(),
                    body: Body::Map(vec![(key.0, value)]),
                })?;
                items.push(pair);
            } else {
                items.push(self.finish(raw)?);
            }
            self.skip_flow_space();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {}
                _ => return fail(),
            }
        }
        self.path.pop();
        Ok(items)
    }

    /// The value after a flow `:`; absent when a `,` or closer follows.
    fn flow_value(&mut self, depth: usize) -> Result<Value> {
        self.skip_flow_space();
        let raw = self.flow_raw(depth + 1)?;
        self.finish(raw)
    }

    fn flow_map(&mut self, depth: usize) -> Result<Vec<(Value, Value)>> {
        self.pos += 1;
        let mut entries = Vec::new();
        loop {
            self.skip_flow_space();
            match self.peek() {
                None => return fail(),
                Some('}') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            if self.peek() == Some(',') {
                return fail();
            }
            if self.peek() == Some('?') && is_blank(self.peek_at(1)) {
                self.pos += 1;
            }
            let raw = self.flow_raw(depth + 1)?;
            let key = self.finish_key(raw)?;
            self.skip_flow_space();
            let value = if self.peek() == Some(':') {
                self.pos += 1;
                self.path_push_key(&key.1);
                let value = self.flow_value(depth)?;
                self.path.pop();
                value
            } else {
                Value::Null
            };
            entries.push((key.0, value));
            self.skip_flow_space();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {}
                _ => return fail(),
            }
        }
        Ok(entries)
    }

    // ---- values ----

    /// Resolve a node's tag, apply its converter and record its anchor.
    fn finish(&mut self, raw: Raw) -> Result<Value> {
        let Raw { props, body } = raw;
        let value = match body {
            Body::Alias(value) => {
                if self.in_key {
                    self.key_original = Some(value.clone());
                }
                return Ok(value);
            }
            body => self.build(body, props.tag.as_deref())?,
        };
        let value = if self.in_key {
            self.key_original = Some(value.clone());
            self.convert(value, false)?
        } else {
            self.convert(value, true)?
        };
        if let Some(anchor) = props.anchor {
            self.anchors.insert(anchor, value.clone());
        }
        Ok(value)
    }

    /// A key: only class converters apply to it.
    /// The converted key and the original, which names the key in the path
    /// of its value.
    fn finish_key(&mut self, raw: Raw) -> Result<(Value, Value)> {
        let Raw { props, body } = raw;
        let original = match body {
            Body::Alias(value) => return Ok((value.clone(), value)),
            body => self.build(body, props.tag.as_deref())?,
        };
        let value = self.convert(original.clone(), false)?;
        if let Some(anchor) = props.anchor {
            self.anchors.insert(anchor, value.clone());
        }
        Ok((value, original))
    }

    fn convert(&mut self, value: Value, with_path: bool) -> Result<Value> {
        if self.converters.is_empty() {
            return Ok(value);
        }
        let kind = match &value {
            Value::Object(..) if self.use_mapping => Kind::Mapping,
            Value::Object(..) => Kind::Dynamic,
            value => kind_of(value),
        };
        let converters = self.converters;
        let path: &[PathPart] = if with_path { &self.path } else { &[] };
        match converters.find(&value, kind, path) {
            Some(function) => {
                let function = function.clone();
                self.invoke.invoke(&function, value)
            }
            None => Ok(value),
        }
    }

    fn build(&mut self, body: Body, tag: Option<&str>) -> Result<Value> {
        let tag = tag.map(|tag| tag.strip_prefix("!!").unwrap_or(tag));
        // `tag` is now `str` for `!!str`; local tags keep their `!`.
        match body {
            Body::Alias(value) => Ok(value),
            Body::Scalar(text, quoted) => self.scalar(&text, quoted, tag),
            Body::Seq(items) => match tag {
                None | Some("seq") => Ok(Value::List(ListValue::new(ListKind::Listing, items))),
                _ => fail(),
            },
            Body::Map(entries) => match tag {
                None | Some("map") => self.object(entries),
                Some("set") => {
                    let keys = entries.into_iter().map(|(key, _)| key).collect::<Vec<_>>();
                    Ok(Value::List(ListValue::new(ListKind::Listing, keys)))
                }
                _ => fail(),
            },
        }
    }

    fn object(&self, entries: Vec<(Value, Value)>) -> Result<Value> {
        let mut map = ObjectMap::default();
        for (key, value) in entries {
            let name: Arc<str> = match &key {
                Value::String(name) => name.clone(),
                other => match mapping_storage_key(other) {
                    Some(name) => name,
                    None => return fail(),
                },
            };
            if map.insert(name, value).is_some() {
                return fail();
            }
        }
        Ok(parsed_object(map, self.use_mapping))
    }

    fn scalar(&self, text: &str, quoted: bool, tag: Option<&str>) -> Result<Value> {
        match tag {
            None if quoted => Ok(Value::String(text.into())),
            None => Ok(self.resolve_plain(text)),
            Some("str" | "!" | "timestamp" | "binary") => Ok(Value::String(
                if tag == Some("binary") {
                    text.split_whitespace().collect::<String>()
                } else {
                    text.to_string()
                }
                .into(),
            )),
            Some("int") => match self.resolve_plain(text) {
                value @ Value::Int(_) => Ok(value),
                _ => fail(),
            },
            Some("float") => match self.resolve_plain(text) {
                Value::Int(n) => Ok(Value::Float(n as f64)),
                value @ Value::Float(_) => Ok(value),
                _ => fail(),
            },
            Some("bool") => match self.resolve_plain(text) {
                value @ Value::Bool(_) => Ok(value),
                _ => fail(),
            },
            Some("null") => match self.resolve_plain(text) {
                Value::Null => Ok(Value::Null),
                _ => fail(),
            },
            _ => fail(),
        }
    }

    fn resolve_plain(&self, text: &str) -> Value {
        match text {
            "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
            "true" | "True" | "TRUE" => return Value::Bool(true),
            "false" | "False" | "FALSE" => return Value::Bool(false),
            "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" if self.mode != Mode::V12 => {
                return Value::Bool(true);
            }
            "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" if self.mode != Mode::V12 => {
                return Value::Bool(false);
            }
            _ => {}
        }
        self.number(text)
            .unwrap_or_else(|| Value::String(text.into()))
    }

    fn number(&self, text: &str) -> Option<Value> {
        let (negative, body) = match text.as_bytes().first()? {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        match body {
            ".inf" | ".Inf" | ".INF" => {
                return Some(Value::Float(if negative {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }));
            }
            ".nan" | ".NaN" | ".NAN" if text == body => return Some(Value::Float(f64::NAN)),
            _ => {}
        }
        let v12 = self.mode != Mode::V11;
        let v11 = self.mode != Mode::V12;
        let radix = |digits: &str, radix: u32, underscores: bool| -> Option<Option<Value>> {
            let cleaned: String = if underscores {
                digits.chars().filter(|&c| c != '_').collect()
            } else {
                digits.to_string()
            };
            if cleaned.is_empty() || !cleaned.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            if underscores && digits.starts_with('_') {
                return None;
            }
            // Parse with the sign, so the minimum integer fits.
            let signed = if negative {
                format!("-{cleaned}")
            } else {
                cleaned
            };
            Some(i64::from_str_radix(&signed, radix).ok().map(Value::Int))
        };
        let overflow = |parsed: Option<Option<Value>>| -> Option<Value> {
            parsed.map(|v| v.unwrap_or(Value::Float(f64::INFINITY)))
        };
        if let Some(hex) = body.strip_prefix("0x") {
            return overflow(radix(hex, 16, v11));
        }
        if let Some(oct) = body.strip_prefix("0o")
            && v12
        {
            return overflow(radix(oct, 8, false));
        }
        if let Some(bin) = body.strip_prefix("0b")
            && v11
        {
            return overflow(radix(bin, 2, true));
        }
        if body.len() > 1
            && body.starts_with('0')
            && body.chars().all(|c| c.is_ascii_digit() || c == '_')
        {
            if v11 && let Some(parsed) = radix(&body[1..], 8, true) {
                return overflow(Some(parsed));
            }
            if v12 && self.mode == Mode::V12 {
                return overflow(radix(body, 10, false));
            }
        }
        if let Some(parsed) = radix(body, 10, v11) {
            return overflow(Some(parsed));
        }
        // Floats.
        let cleaned: String = if v11 {
            body.chars().filter(|&c| c != '_').collect()
        } else {
            body.to_string()
        };
        let bytes = cleaned.as_bytes();
        let mut i = 0;
        let digits = |i: &mut usize| {
            let from = *i;
            while bytes.get(*i).is_some_and(u8::is_ascii_digit) {
                *i += 1;
            }
            *i - from
        };
        let mut count = digits(&mut i);
        let mut has_marker = false;
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            has_marker = true;
            count += digits(&mut i);
        }
        if count == 0 {
            return None;
        }
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            i += 1;
            has_marker = true;
            if matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if digits(&mut i) == 0 {
                return None;
            }
        }
        if i != bytes.len() || !has_marker {
            return None;
        }
        let value: f64 = cleaned.parse().ok()?;
        Some(Value::Float(if negative { -value } else { value }))
    }
}
