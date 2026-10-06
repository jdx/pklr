//! `Regex` and `RegexMatch`, and the Java regex semantics pkl's string
//! methods follow: how matches are iterated, `split`, and `$n` replacements.

use super::*;
use crate::value::Regex;

/// A `String | Regex` argument.
pub(super) enum Pattern<'a> {
    Literal(&'a str),
    Regex(Arc<Regex>),
}

impl<'a> Pattern<'a> {
    pub(super) fn from_arg(value: &'a Value) -> Result<Self> {
        match value {
            Value::String(s) => Ok(Pattern::Literal(s)),
            Value::Regex(re) => Ok(Pattern::Regex(Arc::clone(re))),
            other => Err(type_mismatch("String | Regex", other)),
        }
    }
}

/// Compile `pattern`, failing with pkl's message for a syntax error.
pub(crate) fn compile(pattern: &str) -> Result<Regex> {
    let syntax_error =
        |message: String| Error::Eval(format!("Syntax error in regex `{pattern}`: {message}"));
    check_group_names(pattern).map_err(syntax_error)?;
    Regex::new(pattern).map_err(syntax_error)
}

/// Java only accepts group names made of ASCII letters and digits, starting
/// with a letter; the Rust engine also accepts `_`. Skips escapes, `\Q...\E`
/// quotes, character classes and comments while extended mode is active.
fn check_group_names(pattern: &str) -> std::result::Result<(), String> {
    let bytes = pattern.as_bytes();
    let mut class_depth = 0usize;
    let mut extended = false;
    let mut group_modes = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if bytes.get(i + 1) == Some(&b'Q') => {
                i = pattern[i + 2..]
                    .find("\\E")
                    .map_or(bytes.len(), |end| i + 2 + end + 2);
                continue;
            }
            b'\\' => i += 1,
            b'[' => {
                class_depth += 1;
                // A `]` right after `[` or `[^` is a literal.
                if bytes.get(i + 1) == Some(&b'^') {
                    i += 1;
                }
                if bytes.get(i + 1) == Some(&b']') {
                    i += 1;
                }
            }
            b']' if class_depth > 0 => class_depth -= 1,
            b'(' if class_depth == 0 => {
                if let Some((end, scoped, mode)) = inline_extended_mode(bytes, i) {
                    if scoped {
                        group_modes.push(extended);
                    }
                    extended = mode;
                    i = end;
                } else {
                    if bytes[i + 1..].starts_with(b"?<")
                        && !matches!(bytes.get(i + 3), Some(b'=' | b'!'))
                    {
                        let name_start = i + 3;
                        let name_len = bytes[name_start..]
                            .iter()
                            .take_while(|b| b.is_ascii_alphanumeric())
                            .count();
                        if name_len == 0 || !bytes[name_start].is_ascii_alphabetic() {
                            return Err(format!(
                                "capturing group name does not start with a Latin letter near index {name_start}"
                            ));
                        }
                        if bytes.get(name_start + name_len) != Some(&b'>') {
                            return Err(format!(
                                "named capturing group is missing trailing '>' near index {}",
                                name_start + name_len
                            ));
                        }
                    }
                    group_modes.push(extended);
                }
            }
            b')' if class_depth == 0 => {
                if let Some(mode) = group_modes.pop() {
                    extended = mode;
                }
            }
            b'#' if extended && class_depth == 0 => {
                i = pattern[i..].find('\n').map_or(bytes.len(), |end| i + end);
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// If this is an inline flag group that changes `x`, returns its delimiter,
/// whether the flags are scoped by `:`, and the resulting extended-mode state.
fn inline_extended_mode(bytes: &[u8], start: usize) -> Option<(usize, bool, bool)> {
    if bytes.get(start + 1) != Some(&b'?') {
        return None;
    }
    let mut i = start + 2;
    let mut disabled = false;
    let mut mode = None;
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'-' => disabled = true,
            b'x' => mode = Some(!disabled),
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b':' | b')' => return mode.map(|mode| (i, byte == b':', mode)),
            _ => return None,
        }
        i += 1;
    }
    None
}

/// Whether the final part of `pattern` is an extended-mode line comment.
/// The wrapper used by anchored matching needs one newline in exactly this
/// case, so its closing parenthesis is not consumed by the comment.
fn has_trailing_extended_comment(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut class_depth = 0usize;
    let mut extended = false;
    let mut group_modes = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if bytes.get(i + 1) == Some(&b'Q') => {
                i = pattern[i + 2..]
                    .find("\\E")
                    .map_or(bytes.len(), |end| i + 2 + end + 2);
                continue;
            }
            b'\\' => i += 1,
            b'[' => class_depth += 1,
            b']' if class_depth > 0 => class_depth -= 1,
            b'(' if class_depth == 0 => {
                if let Some((end, scoped, mode)) = inline_extended_mode(bytes, i) {
                    if scoped {
                        group_modes.push(extended);
                    }
                    extended = mode;
                    i = end;
                } else {
                    group_modes.push(extended);
                }
            }
            b')' if class_depth == 0 => {
                if let Some(mode) = group_modes.pop() {
                    extended = mode;
                }
            }
            b'#' if extended && class_depth == 0 => {
                if let Some(end) = pattern[i..].find('\n') {
                    i += end;
                } else {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// A regex matching `literal` verbatim.
pub(super) fn literal(literal: &str) -> Result<Arc<Regex>> {
    compile(&fancy_regex::escape(literal)).map(Arc::new)
}

fn engine_error(e: fancy_regex::Error) -> Error {
    Error::Eval(format!("Error matching regex: {e}"))
}

/// The byte ranges of a match's groups; group 0 is the whole match, and a
/// group that did not participate is `None`.
pub(super) type Groups = Vec<Option<(usize, usize)>>;

/// A match's byte range.
#[derive(Clone, Copy)]
pub(super) struct Span {
    start: usize,
    end: usize,
}

impl Span {
    pub(super) fn start(&self) -> usize {
        self.start
    }

    pub(super) fn end(&self) -> usize {
        self.end
    }
}

/// The next byte offset to search from after a match, as Java's
/// `Matcher.find` does: just past the match, or one character further
/// after an empty match.
fn next_search_start(text: &str, start: usize, end: usize) -> usize {
    if start == end {
        end + text[end..].chars().next().map_or(1, char::len_utf8)
    } else {
        end
    }
}

/// The matches of a regex in a text, found one at a time as Java's
/// `Matcher.find` does, so callers that need only the first few stop early.
pub(super) struct Matches<'a> {
    regex: &'a Regex,
    text: &'a str,
    pos: usize,
}

impl Iterator for Matches<'_> {
    type Item = Result<Groups>;

    fn next(&mut self) -> Option<Result<Groups>> {
        if self.pos > self.text.len() {
            return None;
        }
        let caps = match self.regex.compiled().captures_from_pos(self.text, self.pos) {
            Ok(Some(caps)) => caps,
            Ok(None) => {
                self.pos = usize::MAX;
                return None;
            }
            Err(e) => {
                self.pos = usize::MAX;
                return Some(Err(engine_error(e)));
            }
        };
        let groups = groups_of(&caps);
        let (start, end) = groups[0].expect("group 0 always matches");
        self.pos = next_search_start(self.text, start, end);
        Some(Ok(groups))
    }
}

pub(super) trait RegexExt {
    fn find_at(&self, text: &str, pos: usize) -> Result<Option<Span>>;
    fn matches<'a>(&'a self, text: &'a str) -> Matches<'a>;
    fn find_all(&self, text: &str) -> Result<Vec<Span>>;
    fn captures_all(&self, text: &str) -> Result<Vec<Groups>>;
}

impl RegexExt for Regex {
    fn find_at(&self, text: &str, pos: usize) -> Result<Option<Span>> {
        Ok(self
            .compiled()
            .find_from_pos(text, pos)
            .map_err(engine_error)?
            .map(|m| Span {
                start: m.start(),
                end: m.end(),
            }))
    }

    fn matches<'a>(&'a self, text: &'a str) -> Matches<'a> {
        Matches {
            regex: self,
            text,
            pos: 0,
        }
    }

    fn find_all(&self, text: &str) -> Result<Vec<Span>> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= text.len() {
            let Some(m) = self.find_at(text, pos)? else {
                break;
            };
            out.push(m);
            pos = next_search_start(text, m.start, m.end);
        }
        Ok(out)
    }

    fn captures_all(&self, text: &str) -> Result<Vec<Groups>> {
        self.matches(text).collect()
    }
}

fn groups_of(caps: &fancy_regex::Captures<'_, str>) -> Groups {
    (0..caps.len())
        .map(|i| caps.get(i).map(|m| (m.start(), m.end())))
        .collect()
}

/// What to put after a pattern being wrapped in a group: in extended mode a
/// trailing `#` comment would swallow the closing parenthesis, so end it
/// with a newline, which extended mode ignores.
fn comment_end(regex: &Regex) -> &'static str {
    if has_trailing_extended_comment(regex.pattern()) {
        "\n"
    } else {
        ""
    }
}

/// Java's `Matcher.matches`: a match of the whole of `text`.
pub(super) fn matches_entire(regex: &Regex, text: &str) -> Result<Option<Groups>> {
    let anchored = compile(&format!(
        r"\A(?:{}{})\z",
        regex.pattern(),
        comment_end(regex)
    ))?;
    Ok(anchored
        .compiled()
        .captures(text)
        .map_err(engine_error)?
        .map(|caps| groups_of(&caps)))
}

/// Java's `Matcher.lookingAt`: a match starting at the beginning of `text`.
pub(super) fn looking_at(regex: &Regex, text: &str) -> Result<bool> {
    let anchored = compile(&format!(r"\A(?:{}{})", regex.pattern(), comment_end(regex)))?;
    anchored.compiled().is_match(text).map_err(engine_error)
}

/// Whether a match can end at the end of `text`, including an overlapping
/// suffix that a left-to-right sequence of `find` calls would skip.
pub(super) fn ends_at(regex: &Regex, text: &str) -> Result<bool> {
    let anchored = compile(&format!(r"(?:{}{})\z", regex.pattern(), comment_end(regex)))?;
    anchored.compiled().is_match(text).map_err(engine_error)
}

/// Java's `Pattern.split(text, limit)`: a zero-width match at the start
/// yields no leading empty string, and with `limit == 0` trailing empty
/// strings are dropped.
pub(super) fn java_split(text: &str, regex: &Regex, limit: usize) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut index = 0;
    let mut matched = false;
    for groups in regex.matches(text) {
        if limit > 0 && parts.len() + 1 >= limit {
            break;
        }
        let (start, end) = groups?[0].expect("group 0 always matches");
        let m = Span { start, end };
        if m.end == 0 {
            // A zero-width match at the beginning never produces an empty
            // leading substring.
            continue;
        }
        matched = true;
        parts.push(text[index..m.start].to_string());
        index = m.end;
    }
    if !matched {
        return Ok(vec![text.to_string()]);
    }
    parts.push(text[index..].to_string());
    if limit == 0 {
        while parts.last().is_some_and(String::is_empty) {
            parts.pop();
        }
    }
    Ok(parts)
}

/// Which matches `replace` replaces.
pub(super) enum Which {
    First,
    Last,
    All,
}

/// The matches `which` selects, searching no further than needed.
pub(super) fn select_matches(regex: &Regex, text: &str, which: Which) -> Result<Vec<Groups>> {
    match which {
        Which::First => regex.matches(text).take(1).collect(),
        Which::Last => Ok(regex
            .matches(text)
            .last()
            .transpose()?
            .into_iter()
            .collect()),
        Which::All => regex.captures_all(text),
    }
}

/// Replace matches of `regex` in `text` with `replacement`, expanding
/// Java's `$n`, `${name}` and `\x` syntax.
pub(super) fn replace(
    regex: &Regex,
    text: &str,
    which: Which,
    replacement: &str,
) -> Result<String> {
    let selected = select_matches(regex, text, which)?;
    let mut out = String::with_capacity(text.len());
    let mut last_end = 0;
    for groups in &selected {
        let (start, end) = groups[0].expect("group 0 always matches");
        out.push_str(&text[last_end..start]);
        expand_replacement(regex, text, groups, replacement, &mut out).map_err(|message| {
            Error::Eval(format!(
                "Error replacing matches for regex `{}` with `{replacement}`: `{message}`",
                regex.pattern()
            ))
        })?;
        last_end = end;
    }
    out.push_str(&text[last_end..]);
    Ok(out)
}

/// Java's `Matcher.appendReplacement` expansion of `replacement`.
fn expand_replacement(
    regex: &Regex,
    text: &str,
    groups: &Groups,
    replacement: &str,
    out: &mut String,
) -> std::result::Result<(), String> {
    let group_count = groups.len() - 1;
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(escaped) => out.push(escaped),
                None => return Err("character to be escaped is missing".into()),
            },
            '$' => {
                let group = if chars.peek() == Some(&'{') {
                    chars.next();
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) if c.is_ascii_alphanumeric() => name.push(c),
                            _ => {
                                return Err("named capturing group is missing trailing '}'".into());
                            }
                        }
                    }
                    if name.is_empty() {
                        return Err("named capturing group has 0 length name".into());
                    }
                    let Some(index) = regex
                        .compiled()
                        .capture_names()
                        .position(|n| n == Some(name.as_str()))
                    else {
                        return Err(format!("No group with name {{{name}}}"));
                    };
                    index
                } else {
                    let Some(first) = chars.peek().and_then(|c| c.to_digit(10)) else {
                        return Err("Illegal group reference".into());
                    };
                    chars.next();
                    let mut index = first as usize;
                    if index > group_count {
                        return Err(format!("No group {index}"));
                    }
                    while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
                        let next = index * 10 + digit as usize;
                        if next > group_count {
                            break;
                        }
                        index = next;
                        chars.next();
                    }
                    index
                };
                if let Some((start, end)) = groups[group] {
                    out.push_str(&text[start..end]);
                }
            }
            _ => out.push(c),
        }
    }
    Ok(())
}

/// UTF-16 offsets for every UTF-8 character boundary in `text`. Pkl reports
/// match positions in UTF-16 code units, as it runs on the JVM.
pub(super) fn utf16_offsets(text: &str) -> Vec<i64> {
    let mut offsets = vec![0; text.len() + 1];
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        offsets[byte] = units;
        units += c.len_utf16() as i64;
        offsets[byte + c.len_utf8()] = units;
    }
    offsets
}

/// A `RegexMatch` for a match's group 0, or for group `index` with no
/// groups of its own.
fn group_match_value(
    text: &str,
    offsets: &[i64],
    (start, end): (usize, usize),
    groups: Option<&Groups>,
) -> Value {
    let mut map = ObjectMap::default();
    map.insert("value".into(), Value::String(text[start..end].into()));
    map.insert("start".into(), Value::Int(offsets[start]));
    map.insert("end".into(), Value::Int(offsets[end]));
    let group_values = match groups {
        Some(groups) => groups
            .iter()
            .map(|group| match group {
                Some(span) => group_match_value(text, offsets, *span, None),
                None => Value::Null,
            })
            .collect(),
        None => Vec::new(),
    };
    map.insert("groups".into(), Value::List(group_values.into()));
    typed_object("RegexMatch", map)
}

/// Whether `regex` matches the empty string between the two UTF-16 code
/// units of the character `c` at byte `offset`. The units are stood in for by
/// two private-use characters, which Java's classes treat the same way as a
/// lone surrogate (neither a word character nor whitespace).
fn matches_empty_mid_char(regex: &Regex, text: &str, offset: usize, c: char) -> Result<bool> {
    const STAND_IN: char = '\u{E000}';
    let mut probe = String::with_capacity(text.len() + 2);
    probe.push_str(&text[..offset]);
    probe.push(STAND_IN);
    let mid = probe.len();
    probe.push(STAND_IN);
    probe.push_str(&text[offset + c.len_utf8()..]);
    Ok(regex
        .find_at(&probe, mid)?
        .is_some_and(|m| m.start() == mid && m.end() == mid))
}

/// The empty `RegexMatch` Java reports between the two UTF-16 code units of
/// a character, at code unit `position`, for a regex with `groups`.
fn mid_surrogate_empty_match(position: i64, groups: &Groups) -> Value {
    let empty = |groups: Vec<Value>| {
        let mut map = ObjectMap::default();
        map.insert("value".into(), Value::String("".into()));
        map.insert("start".into(), Value::Int(position));
        map.insert("end".into(), Value::Int(position));
        map.insert("groups".into(), Value::List(groups.into()));
        typed_object("RegexMatch", map)
    };
    let group_values = groups
        .iter()
        .map(|group| match group {
            Some(_) => empty(Vec::new()),
            None => Value::Null,
        })
        .collect();
    empty(group_values)
}

/// The `RegexMatch` for a match with `groups`. `with_groups` is false for a
/// group's own match, which lists no groups.
pub(super) fn regex_match_value(
    text: &str,
    offsets: &[i64],
    groups: &Groups,
    with_groups: bool,
) -> Value {
    let span = groups[0].expect("group 0 always matches");
    group_match_value(text, offsets, span, with_groups.then_some(groups))
}

pub(super) fn property(regex: &Arc<Regex>, name: &str) -> Option<Result<Value>> {
    Some(Ok(match name {
        "pattern" => Value::String(regex.pattern().into()),
        "groupCount" => Value::Int(regex.compiled().captures_len() as i64 - 1),
        _ => return None,
    }))
}

impl Evaluator {
    pub(super) fn regex_method(
        &mut self,
        regex: &Arc<Regex>,
        name: &str,
        args: &[Value],
    ) -> Option<Result<Value>> {
        if !matches!(name, "findMatchesIn" | "matchEntire") {
            return None;
        }
        Some((|| {
            check_arity(args, 1)?;
            let text = Args { method: name, args }.string(0)?;
            let offsets = utf16_offsets(text);
            Ok(if name == "findMatchesIn" {
                let mut matches = Vec::new();
                for groups in regex.matches(text) {
                    let groups = groups?;
                    matches.push(regex_match_value(text, &offsets, &groups, true));
                    // After an empty match Java resumes one UTF-16 code unit
                    // later, which inside a surrogate pair is the middle of a
                    // character. The empty match it finds there has no text
                    // a Rust string can slice, so report it directly.
                    let (start, end) = groups[0].expect("group 0 always matches");
                    if start == end
                        && let Some(c) = text[end..].chars().next()
                        && c.len_utf16() == 2
                        && matches_empty_mid_char(regex, text, end, c)?
                    {
                        matches.push(mid_surrogate_empty_match(offsets[end] + 1, &groups));
                    }
                }
                Value::List(matches.into())
            } else {
                match matches_entire(regex, text)? {
                    Some(groups) => regex_match_value(text, &offsets, &groups, true),
                    None => Value::Null,
                }
            })
        })())
    }
}
