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
    Regex::new(pattern)
        .map_err(|message| Error::Eval(format!("Syntax error in regex `{pattern}`: {message}")))
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

pub(super) trait RegexExt {
    fn find_at(&self, text: &str, pos: usize) -> Result<Option<Span>>;
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
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= text.len() {
            let Some(caps) = self
                .compiled()
                .captures_from_pos(text, pos)
                .map_err(engine_error)?
            else {
                break;
            };
            let groups = groups_of(&caps);
            let (start, end) = groups[0].expect("group 0 always matches");
            out.push(groups);
            pos = next_search_start(text, start, end);
        }
        Ok(out)
    }
}

fn groups_of(caps: &fancy_regex::Captures<'_>) -> Groups {
    (0..caps.len())
        .map(|i| caps.get(i).map(|m| (m.start(), m.end())))
        .collect()
}

/// Java's `Matcher.matches`: a match of the whole of `text`.
pub(super) fn matches_entire(regex: &Regex, text: &str) -> Result<Option<Groups>> {
    let anchored = compile(&format!(r"\A(?:{})\z", regex.pattern()))?;
    Ok(anchored
        .compiled()
        .captures(text)
        .map_err(engine_error)?
        .map(|caps| groups_of(&caps)))
}

/// Java's `Matcher.lookingAt`: a match starting at the beginning of `text`.
pub(super) fn looking_at(regex: &Regex, text: &str) -> Result<bool> {
    let anchored = compile(&format!(r"\A(?:{})", regex.pattern()))?;
    anchored.compiled().is_match(text).map_err(engine_error)
}

/// Java's `Pattern.split(text, limit)`: a zero-width match at the start
/// yields no leading empty string, and with `limit == 0` trailing empty
/// strings are dropped.
pub(super) fn java_split(text: &str, regex: &Regex, limit: usize) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut index = 0;
    let mut matched = false;
    for m in regex.find_all(text)? {
        if limit > 0 && parts.len() + 1 >= limit {
            break;
        }
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

/// Replace matches of `regex` in `text` with `replacement`, expanding
/// Java's `$n`, `${name}` and `\x` syntax.
pub(super) fn replace(
    regex: &Regex,
    text: &str,
    which: Which,
    replacement: &str,
) -> Result<String> {
    let all = regex.captures_all(text)?;
    let selected: Vec<Groups> = match which {
        Which::First => all.into_iter().take(1).collect(),
        Which::Last => all.into_iter().last().into_iter().collect(),
        Which::All => all,
    };
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

/// The number of UTF-16 code units before byte offset `offset`, which is
/// how pkl (a JVM program) reports match positions.
fn utf16_offset(text: &str, offset: usize) -> i64 {
    text[..offset].encode_utf16().count() as i64
}

/// A `RegexMatch` for a match's group 0, or for group `index` with no
/// groups of its own.
fn group_match_value(text: &str, (start, end): (usize, usize), groups: Option<&Groups>) -> Value {
    let mut map = ObjectMap::default();
    map.insert("value".into(), Value::String(text[start..end].into()));
    map.insert("start".into(), Value::Int(utf16_offset(text, start)));
    map.insert("end".into(), Value::Int(utf16_offset(text, end)));
    let group_values = match groups {
        Some(groups) => groups
            .iter()
            .map(|group| match group {
                Some(span) => group_match_value(text, *span, None),
                None => Value::Null,
            })
            .collect(),
        None => Vec::new(),
    };
    map.insert("groups".into(), Value::List(group_values.into()));
    typed_object("RegexMatch", map)
}

/// The `RegexMatch` for a match with `groups`. `with_groups` is false for a
/// group's own match, which lists no groups.
pub(super) fn regex_match_value(text: &str, groups: &Groups, with_groups: bool) -> Value {
    let span = groups[0].expect("group 0 always matches");
    group_match_value(text, span, with_groups.then_some(groups))
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
            Ok(if name == "findMatchesIn" {
                Value::List(
                    regex
                        .captures_all(text)?
                        .iter()
                        .map(|groups| regex_match_value(text, groups, true))
                        .collect::<Vec<_>>()
                        .into(),
                )
            } else {
                match matches_entire(regex, text)? {
                    Some(groups) => regex_match_value(text, &groups, true),
                    None => Value::Null,
                }
            })
        })())
    }
}
