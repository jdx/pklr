//! `String` properties and methods. Indexes and lengths count code points,
//! as pkl's do.

use super::regex::{Pattern, RegexExt, java_split};
use super::*;

/// The byte offset of the code point at `index`, or `None` when the string
/// has fewer than `index` code points. `index == length` maps to `s.len()`.
fn byte_offset(s: &str, index: i64) -> Option<usize> {
    if index < 0 {
        return None;
    }
    let index = usize::try_from(index).ok()?;
    if index == 0 {
        return Some(0);
    }
    match s.char_indices().nth(index) {
        Some((offset, _)) => Some(offset),
        None if s.chars().count() == index => Some(s.len()),
        None => None,
    }
}

/// The byte offset `n` code points before the end of `s`, or `None` when
/// `s` has fewer than `n` code points.
fn byte_offset_from_end(s: &str, n: i64) -> Option<usize> {
    let len = s.chars().count() as i64;
    if n > len {
        return None;
    }
    byte_offset(s, len - n)
}

fn code_point_index(s: &str, byte_offset: usize) -> i64 {
    s[..byte_offset].chars().count() as i64
}

fn string_value(s: impl Into<Arc<str>>) -> Value {
    Value::String(s.into())
}

/// Java's `Character.isWhitespace`, which `String.trim()` uses.
fn is_java_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{001C}'..='\u{001F}'
    ) || (c.is_whitespace()
        && !matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}' | '\u{0085}')
        && !c.is_control())
}

/// The Unicode `White_Space` property for BMP characters, which pkl's
/// `isBlank`, `trimStart` and `trimEnd` use.
fn is_white_space(c: char) -> bool {
    c.is_whitespace()
}

fn char_index_out_of_range(index: i64, from: i64, to: i64, s: &str) -> Error {
    error_with_values(
        format!("Character index `{index}` is out of range `{from}`..`{to}`."),
        &[("String", &string_value(s))],
    )
}

/// The byte range of code points `start..exclusive_end`, or the error pkl
/// reports for an index out of range.
fn code_point_range(s: &str, start: i64, exclusive_end: i64) -> Result<(usize, usize)> {
    let len = s.chars().count() as i64;
    let Some(from) = byte_offset(s, start) else {
        return Err(char_index_out_of_range(start, 0, len, s));
    };
    let to = if exclusive_end < start {
        None
    } else {
        byte_offset(&s[from..], exclusive_end - start).map(|offset| from + offset)
    };
    match to {
        Some(to) => Ok((from, to)),
        None => Err(char_index_out_of_range(exclusive_end, start, len, s)),
    }
}

/// Java's `Long.parseLong` after removing pkl's digit separators.
fn parse_int(s: &str) -> Option<i64> {
    let s = remove_underscores(s)?;
    let digits = s.strip_prefix(['+', '-']).unwrap_or(&s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Java's `Double.parseDouble` after removing pkl's digit separators.
fn parse_float(s: &str) -> Option<f64> {
    let s = remove_underscores(s)?;
    java_parse_double(s.trim_matches(|c: char| c <= ' '))
}

/// Java's `Double.parseDouble` grammar: an optional sign, then `NaN`,
/// `Infinity`, a decimal or a hexadecimal (`0x1.8p1`) floating-point
/// literal, which may end in a `f`/`F`/`d`/`D` type suffix.
fn java_parse_double(s: &str) -> Option<f64> {
    let (negative, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let magnitude = match body {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        _ => {
            let body = body.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(body);
            match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
                Some(hex) => parse_hex_float(hex)?,
                None => parse_decimal_float(body)?,
            }
        }
    };
    Some(if negative { -magnitude } else { magnitude })
}

/// Digits with an optional fraction (`1`, `1.`, `.5`, `1.5`), then an
/// optional exponent; no sign.
fn parse_decimal_float(body: &str) -> Option<f64> {
    let bytes = body.as_bytes();
    let mut i = 0;
    let int_digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    i += int_digits;
    let mut frac_digits = 0;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        frac_digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        i += frac_digits;
    }
    if int_digits + frac_digits == 0 {
        return None;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exp_digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        if exp_digits == 0 {
            return None;
        }
        i += exp_digits;
    }
    if i != bytes.len() {
        return None;
    }
    body.parse().ok()
}

/// The part of a hexadecimal floating-point literal after `0x`: hex digits
/// with an optional fraction, then a required binary exponent `p[+-]digits`.
fn parse_hex_float(body: &str) -> Option<f64> {
    let (mantissa, exponent) = body.split_once(['p', 'P'])?;
    let exponent = exponent.strip_prefix('+').unwrap_or(exponent);
    let exponent_digits = exponent.strip_prefix('-').unwrap_or(exponent);
    if exponent_digits.is_empty() || !exponent_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int_part.is_empty() && frac_part.is_empty()
        || !int_part
            .bytes()
            .chain(frac_part.bytes())
            .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    // Accumulate the significant hex digits exactly in a u128, tracking the
    // binary exponent, then let `f64` round once.
    let mut significand: u128 = 0;
    let mut binary_exponent: i64 = exponent.parse().unwrap_or(if exponent.starts_with('-') {
        i64::MIN / 2
    } else {
        i64::MAX / 2
    });
    for (index, digit) in int_part.bytes().chain(frac_part.bytes()).enumerate() {
        let value = u128::from((digit as char).to_digit(16).expect("checked hex digit"));
        let is_fraction = index >= int_part.len();
        if significand >> 120 == 0 {
            significand = significand * 16 + value;
            if is_fraction {
                binary_exponent = binary_exponent.saturating_sub(4);
            }
        } else {
            // Digits past u128 precision only matter for rounding; keep a
            // sticky bit.
            significand |= u128::from(value != 0);
            if !is_fraction {
                binary_exponent = binary_exponent.saturating_add(4);
            }
        }
    }
    if significand == 0 {
        return Some(0.0);
    }
    // Normalize to 53 significant bits with round-half-even and a sticky bit.
    let bits = 128 - significand.leading_zeros() as i64;
    let mut exponent = binary_exponent + bits - 1;
    if exponent > 1023 {
        return Some(f64::INFINITY);
    }
    // Subnormals keep fewer bits.
    let precision = if exponent < -1022 {
        53 - (-1022 - exponent)
    } else {
        53
    };
    if precision <= 0 {
        // Rounds to zero or the smallest subnormal.
        return Some(if precision == 0 && significand > (1u128 << (bits - 1)) {
            f64::from_bits(1)
        } else {
            0.0
        });
    }
    let shift = bits - precision;
    let mut kept = if shift > 0 {
        let kept = significand >> shift;
        let rest = significand & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        if rest > half || (rest == half && kept & 1 == 1) {
            kept + 1
        } else {
            kept
        }
    } else {
        significand << -shift
    };
    if kept >> precision != 0 {
        kept >>= 1;
        exponent += 1;
        if exponent > 1023 {
            return Some(f64::INFINITY);
        }
    }
    // Scaling by a power of two is exact here; split it so no intermediate
    // underflows before the final (possibly subnormal) result.
    let mut scale = exponent - (precision - 1);
    let mut value = kept as f64;
    if scale < -1022 {
        value *= 2f64.powi((scale + 1022) as i32);
        scale = -1022;
    }
    Some(value * 2f64.powi(scale as i32))
}

/// Remove `_` digit separators the way pkl's number literals allow them:
/// not at the start of the number, of its fraction or of its exponent.
/// Returns `None` (unparseable) for a misplaced separator.
fn remove_underscores(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut number_start = true;
    for c in s.chars() {
        if c != '_' {
            out.push(c);
        } else if number_start {
            return None;
        }
        number_start = matches!(c, '.' | 'e' | 'E');
    }
    Some(out)
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(super) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64_ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Java's `Base64.getDecoder().decode`: padding is optional, but if present
/// it must be complete, and nothing may follow it.
pub(super) fn base64_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let (mut bits, mut nbits, mut count) = (0u32, 0, 0usize);
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            // `xx==` or `xxx=` ends the input.
            let needed = match count % 4 {
                2 => 2,
                3 => 1,
                _ => 0,
            };
            let padding = &bytes[i..];
            if needed == 0 || padding.len() != needed || padding.iter().any(|b| *b != b'=') {
                return Err(format!(
                    "Input byte array has an incorrect ending byte at {i}"
                ));
            }
            return Ok(out);
        }
        let Some(value) = BASE64_ALPHABET.iter().position(|c| *c == b) else {
            return Err(format!("Illegal base64 character {b:x}"));
        };
        bits = (bits << 6) | value as u32;
        nbits += 6;
        count += 1;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    if count % 4 == 1 {
        return Err("Last unit does not have enough valid bits".to_string());
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `pattern` is a glob pattern pkl accepts (`GlobResolver.toRegexString`).
pub(super) fn is_glob_pattern(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut in_group = false;
    let mut i = 0;
    while i < chars.len() {
        let next = chars.get(i + 1).copied();
        match chars[i] {
            '{' if in_group => return false,
            '{' => in_group = true,
            '}' => in_group = false,
            '\\' => {
                if !matches!(next, Some('?' | '*' | '[' | '{' | '\\')) {
                    return false;
                }
                i += 1;
            }
            '[' => {
                // The first character may be `^`, `!` or `]` verbatim.
                if next.is_none() {
                    return false;
                }
                if matches!(next, Some('^' | '!' | ']')) {
                    i += 1;
                }
                i += 1;
                loop {
                    let Some(&current) = chars.get(i) else {
                        return false;
                    };
                    if current == ']' {
                        break;
                    }
                    if current == '[' && matches!(chars.get(i + 1), Some(':' | '=' | '.')) {
                        return false;
                    }
                    if current == '/' {
                        return false;
                    }
                    i += 1;
                    if i == chars.len() {
                        return false;
                    }
                }
            }
            '?' | '*' | '+' | '@' | '!' if next == Some('(') => return false,
            '*' if next == Some('*') => i += 1,
            _ => {}
        }
        i += 1;
    }
    !in_group
}

/// `s[index]`: the character at a code point index.
pub(crate) fn subscript(s: &Arc<str>, index: &Value) -> Result<Value> {
    let Value::Int(index) = index else {
        return Err(operator_not_defined(
            "[]",
            &Value::String(Arc::clone(s)),
            index,
        ));
    };
    match byte_offset(s, *index) {
        Some(offset) if offset < s.len() => {
            let c = s[offset..].chars().next().expect("offset is in range");
            Ok(string_value(c.encode_utf8(&mut [0; 4]) as &str))
        }
        _ => Err(char_index_out_of_range(
            *index,
            0,
            s.chars().count() as i64 - 1,
            s,
        )),
    }
}

pub(super) fn property(s: &Arc<str>, name: &str) -> Option<Result<Value>> {
    let value = match name {
        "length" => Value::Int(s.chars().count() as i64),
        "lastIndex" => Value::Int(s.chars().count() as i64 - 1),
        "isEmpty" => Value::Bool(s.is_empty()),
        "isNotEmpty" => Value::Bool(!s.is_empty()),
        "isBlank" => Value::Bool(s.chars().all(is_white_space)),
        "isNotBlank" => Value::Bool(!s.chars().all(is_white_space)),
        "isRegex" => Value::Bool(super::regex::compile(s).is_ok()),
        "isGlobPattern" => Value::Bool(is_glob_pattern(s)),
        "isBase64" => Value::Bool(base64_decode(s).is_ok()),
        "md5" => {
            use md5::Digest;
            string_value(hex(&md5::Md5::digest(s.as_bytes())))
        }
        "sha1" => {
            use sha1::Digest;
            string_value(hex(&sha1::Sha1::digest(s.as_bytes())))
        }
        "sha256" => {
            use sha2::Digest;
            string_value(hex(&sha2::Sha256::digest(s.as_bytes())))
        }
        "sha256Int" => {
            use sha2::Digest;
            let digest = sha2::Sha256::digest(s.as_bytes());
            // The first 8 bytes as a little-endian long, as pkl computes it.
            let mut first = [0u8; 8];
            first.copy_from_slice(&digest[..8]);
            Value::Int(i64::from_le_bytes(first))
        }
        "base64" => string_value(base64_encode(s.as_bytes())),
        "base64Decoded" => {
            return Some(
                base64_decode(s)
                    .map(|bytes| string_value(String::from_utf8_lossy(&bytes).as_ref()))
                    .map_err(|message| {
                        error_with_values(message, &[("String", &Value::String(Arc::clone(s)))])
                    }),
            );
        }
        "chars" => Value::List(
            s.chars()
                .map(|c| string_value(c.encode_utf8(&mut [0; 4]) as &str))
                .collect::<Vec<_>>()
                .into(),
        ),
        "codePoints" => Value::List(
            s.chars()
                .map(|c| Value::Int(c as i64))
                .collect::<Vec<_>>()
                .into(),
        ),
        _ => return None,
    };
    Some(Ok(value))
}

impl Evaluator {
    pub(super) fn string_method(
        &mut self,
        s: &Arc<str>,
        name: &str,
        args: &[Value],
        depth: usize,
    ) -> Option<Result<Value>> {
        let arity = match name {
            "toUpperCase" | "toLowerCase" | "reverse" | "trim" | "trimStart" | "trimEnd"
            | "capitalize" | "decapitalize" | "toInt" | "toIntOrNull" | "toFloat"
            | "toFloatOrNull" | "toBoolean" | "toBooleanOrNull" => 0,
            "getOrNull" | "repeat" | "contains" | "matches" | "startsWith" | "endsWith"
            | "indexOf" | "indexOfOrNull" | "lastIndexOf" | "lastIndexOfOrNull" | "take"
            | "takeWhile" | "takeLast" | "takeLastWhile" | "drop" | "dropWhile" | "dropLast"
            | "dropLastWhile" | "split" => 1,
            "substring" | "substringOrNull" | "replaceFirst" | "replaceLast" | "replaceAll"
            | "replaceFirstMapped" | "replaceLastMapped" | "replaceAllMapped" | "padStart"
            | "padEnd" | "splitLimit" => 2,
            "replaceRange" => 3,
            _ => return None,
        };
        Some(
            check_arity(args, arity)
                .and_then(|()| self.string_method_checked(s, name, args, depth)),
        )
    }

    fn string_method_checked(
        &mut self,
        s: &Arc<str>,
        name: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Value> {
        let a = Args { method: name, args };
        let this = || Value::String(Arc::clone(s));
        Ok(match name {
            "getOrNull" => {
                let index = a.int(0)?;
                match byte_offset(s, index) {
                    Some(offset) if offset < s.len() => {
                        let c = s[offset..].chars().next().expect("offset is in range");
                        string_value(c.encode_utf8(&mut [0; 4]) as &str)
                    }
                    _ => Value::Null,
                }
            }
            "substring" => {
                let (from, to) = code_point_range(s, a.int(0)?, a.int(1)?)?;
                string_value(&s[from..to])
            }
            "substringOrNull" => match code_point_range(s, a.int(0)?, a.int(1)?) {
                Ok((from, to)) => string_value(&s[from..to]),
                Err(_) => Value::Null,
            },
            "repeat" => {
                let count = a.int(0)?;
                if count < 0 {
                    return Err(error_with_values(
                        "Type constraint `isPositive` violated.",
                        &[("Value", &Value::Int(count))],
                    ));
                }
                // pkl strings hold at most `i32::MAX` UTF-16 code units.
                if (s.encode_utf16().count() as i64)
                    .checked_mul(count)
                    .is_none_or(|n| n > i64::from(i32::MAX))
                {
                    return Err(Error::Eval("Integer overflow.".into()));
                }
                string_value(s.repeat(count as usize))
            }
            "contains" => Value::Bool(match Pattern::from_arg(a.value(0)?)? {
                Pattern::Literal(p) => s.contains(p),
                Pattern::Regex(re) => re.find_at(s, 0)?.is_some(),
            }),
            "matches" => {
                let Value::Regex(re) = a.value(0)? else {
                    return Err(type_mismatch("Regex", a.value(0)?));
                };
                Value::Bool(super::regex::matches_entire(re, s)?.is_some())
            }
            "startsWith" => Value::Bool(match Pattern::from_arg(a.value(0)?)? {
                Pattern::Literal(p) => s.starts_with(p),
                Pattern::Regex(re) => super::regex::looking_at(&re, s)?,
            }),
            "endsWith" => Value::Bool(match Pattern::from_arg(a.value(0)?)? {
                Pattern::Literal(p) => s.ends_with(p),
                Pattern::Regex(re) => super::regex::ends_at(&re, s)?,
            }),
            "indexOf" | "indexOfOrNull" | "lastIndexOf" | "lastIndexOfOrNull" => {
                let pattern = Pattern::from_arg(a.value(0)?)?;
                let last = name.starts_with("last");
                let found = match &pattern {
                    Pattern::Literal(p) => {
                        if last {
                            s.rfind(p)
                        } else {
                            s.find(p)
                        }
                    }
                    Pattern::Regex(re) => {
                        if last {
                            re.find_all(s)?.last().map(|m| m.start())
                        } else {
                            re.find_at(s, 0)?.map(|m| m.start())
                        }
                    }
                };
                match found {
                    Some(offset) => Value::Int(code_point_index(s, offset)),
                    None if name.ends_with("OrNull") => Value::Null,
                    None => {
                        let (message, pattern_value) = match &pattern {
                            Pattern::Literal(p) => (
                                "String does not contain a match for literal pattern.",
                                string_value(*p),
                            ),
                            Pattern::Regex(re) => (
                                "String does not contain a match for regex pattern.",
                                Value::Regex(Arc::clone(re)),
                            ),
                        };
                        return Err(error_with_values(
                            message,
                            &[("String", &this()), ("Pattern", &pattern_value)],
                        ));
                    }
                }
            }
            "take" | "drop" | "takeLast" | "dropLast" => {
                let n = a.int(0)?;
                if n < 0 {
                    return Err(expected_positive(n));
                }
                match name {
                    "take" => string_value(byte_offset(s, n).map_or(&**s, |i| &s[..i])),
                    "drop" => string_value(byte_offset(s, n).map_or("", |i| &s[i..])),
                    "takeLast" => {
                        string_value(byte_offset_from_end(s, n).map_or(&**s, |i| &s[i..]))
                    }
                    _ => string_value(byte_offset_from_end(s, n).map_or("", |i| &s[..i])),
                }
            }
            "takeWhile" | "dropWhile" => {
                let predicate = a.function(0)?;
                let mut end = s.len();
                for (offset, c) in s.char_indices() {
                    let arg = string_value(c.encode_utf8(&mut [0; 4]) as &str);
                    if !self.call_predicate(predicate, &[arg], depth)? {
                        end = offset;
                        break;
                    }
                }
                string_value(if name == "takeWhile" {
                    &s[..end]
                } else {
                    &s[end..]
                })
            }
            "takeLastWhile" | "dropLastWhile" => {
                let predicate = a.function(0)?;
                let mut start = 0;
                for (offset, c) in s.char_indices().rev() {
                    let arg = string_value(c.encode_utf8(&mut [0; 4]) as &str);
                    if !self.call_predicate(predicate, &[arg], depth)? {
                        start = offset + c.len_utf8();
                        break;
                    }
                }
                string_value(if name == "takeLastWhile" {
                    &s[start..]
                } else {
                    &s[..start]
                })
            }
            "replaceFirst" | "replaceLast" | "replaceAll" => {
                let replacement = a.string(1)?;
                match Pattern::from_arg(a.value(0)?)? {
                    Pattern::Literal(p) => {
                        let found = match name {
                            "replaceFirst" => s.find(p),
                            "replaceLast" => s.rfind(p),
                            _ => return Ok(string_value(s.replace(p, replacement))),
                        };
                        match found {
                            Some(i) => string_value(format!(
                                "{}{replacement}{}",
                                &s[..i],
                                &s[i + p.len()..]
                            )),
                            None => this(),
                        }
                    }
                    Pattern::Regex(re) => {
                        let which = match name {
                            "replaceFirst" => super::regex::Which::First,
                            "replaceLast" => super::regex::Which::Last,
                            _ => super::regex::Which::All,
                        };
                        string_value(super::regex::replace(&re, s, which, replacement)?)
                    }
                }
            }
            "replaceFirstMapped" | "replaceLastMapped" | "replaceAllMapped" => {
                let mapper = a.function(1)?;
                let re = match Pattern::from_arg(a.value(0)?)? {
                    Pattern::Literal(p) => super::regex::literal(p)?,
                    Pattern::Regex(re) => re,
                };
                let which = match name {
                    "replaceFirstMapped" => super::regex::Which::First,
                    "replaceLastMapped" => super::regex::Which::Last,
                    _ => super::regex::Which::All,
                };
                let selected = super::regex::select_matches(&re, s, which)?;
                let offsets = super::regex::utf16_offsets(s);
                let mut out = String::with_capacity(s.len());
                let mut last_end = 0;
                for groups in selected {
                    let (start, end) = groups[0].expect("group 0 always matches");
                    out.push_str(&s[last_end..start]);
                    let regex_match = super::regex::regex_match_value(s, &offsets, &groups, true);
                    match self.invoke_lambda(mapper, &[regex_match], depth)? {
                        Value::String(r) => out.push_str(&r),
                        other => return Err(type_mismatch("String", &other)),
                    }
                    last_end = end;
                }
                out.push_str(&s[last_end..]);
                string_value(out)
            }
            "replaceRange" => {
                let (from, to) = code_point_range(s, a.int(0)?, a.int(1)?)?;
                let replacement = a.string(2)?;
                string_value(format!("{}{replacement}{}", &s[..from], &s[to..]))
            }
            "toUpperCase" => string_value(s.to_uppercase()),
            "toLowerCase" => string_value(s.to_lowercase()),
            "reverse" => string_value(s.chars().rev().collect::<String>()),
            "trim" => string_value(s.trim_matches(is_java_whitespace)),
            "trimStart" => string_value(s.trim_start_matches(is_white_space)),
            "trimEnd" => string_value(s.trim_end_matches(is_white_space)),
            "padStart" | "padEnd" => {
                let width = a.int(0)?;
                let fill = a.string(1)?;
                if fill.chars().count() != 1 {
                    return Err(Error::Eval(format!(
                        "Type constraint `length == 1` violated.\nValue: {}",
                        render_value(a.value(1)?)
                    )));
                }
                let length = s.chars().count() as i64;
                if length >= width {
                    this()
                } else {
                    // pkl builds the result in a buffer of `width` UTF-16
                    // code units, which must fit in an Int32.
                    if width > i64::from(i32::MAX) {
                        return Err(Error::Eval(format!(
                            "Int value `{}` is too large (only Int32 supported here).",
                            super::render::group_digits(width)
                        )));
                    }
                    let padding = fill.repeat((width - length) as usize);
                    string_value(if name == "padStart" {
                        format!("{padding}{s}")
                    } else {
                        format!("{s}{padding}")
                    })
                }
            }
            "split" | "splitLimit" => {
                let limit = if name == "splitLimit" {
                    let limit = a.int(1)?;
                    if limit <= 0 {
                        return Err(error_with_values(
                            "Type constraint `this > 0` violated.",
                            &[("Value", &Value::Int(limit))],
                        ));
                    }
                    limit.min(i64::from(i32::MAX)) as usize
                } else {
                    0
                };
                let parts = match Pattern::from_arg(a.value(0)?)? {
                    Pattern::Literal(p) => java_split(s, &*super::regex::literal(p)?, limit)?,
                    Pattern::Regex(re) => java_split(s, &re, limit)?,
                };
                Value::List(
                    parts
                        .into_iter()
                        .map(string_value)
                        .collect::<Vec<_>>()
                        .into(),
                )
            }
            "capitalize" | "decapitalize" => {
                let mut chars = s.chars();
                match chars.next() {
                    None => this(),
                    Some(first) => {
                        let mut out: String = if name == "capitalize" {
                            title_case(first)
                        } else {
                            first.to_lowercase().collect()
                        };
                        out.push_str(chars.as_str());
                        string_value(out)
                    }
                }
            }
            "toInt" | "toIntOrNull" => match parse_int(s) {
                Some(n) => Value::Int(n),
                None if name == "toIntOrNull" => Value::Null,
                None => return Err(cannot_parse(s, "Int")),
            },
            "toFloat" | "toFloatOrNull" => match parse_float(s) {
                Some(f) => Value::Float(f),
                None if name == "toFloatOrNull" => Value::Null,
                None => return Err(cannot_parse(s, "Float")),
            },
            "toBoolean" | "toBooleanOrNull" => {
                if s.eq_ignore_ascii_case("true") {
                    Value::Bool(true)
                } else if s.eq_ignore_ascii_case("false") {
                    Value::Bool(false)
                } else if name == "toBooleanOrNull" {
                    Value::Null
                } else {
                    return Err(cannot_parse(s, "Boolean"));
                }
            }
            _ => unreachable!("string_method checked the method name"),
        })
    }
}

fn cannot_parse(s: &str, ty: &str) -> Error {
    error_with_values(
        format!("Cannot parse string as `{ty}`."),
        &[("String", &string_value(s))],
    )
}

/// Java's `Character.toTitleCase` for the characters whose title case differs
/// from their upper case; upper case otherwise.
fn title_case(c: char) -> String {
    match c {
        'Ǆ' | 'ǅ' | 'ǆ' => "ǅ".into(),
        'Ǉ' | 'ǈ' | 'ǉ' => "ǈ".into(),
        'Ǌ' | 'ǋ' | 'ǌ' => "ǋ".into(),
        'Ǳ' | 'ǲ' | 'ǳ' => "ǲ".into(),
        _ => {
            let mut upper = c.to_uppercase();
            // Title case maps one character to one character.
            match (upper.next(), upper.next()) {
                (Some(u), None) => u.to_string(),
                _ => c.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for s in ["", "a", "ab", "abc", "abcd", "Hello, World!"] {
            let encoded = base64_encode(s.as_bytes());
            assert_eq!(base64_decode(&encoded).unwrap(), s.as_bytes());
        }
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_decode("YWI").unwrap(), b"ab");
        assert!(base64_decode("YWI=a").is_err());
        assert!(base64_decode("Y").is_err());
        assert!(base64_decode("Y!==").is_err());
    }

    #[test]
    fn numbers_parse_like_pkl() {
        assert_eq!(parse_int("1_000"), Some(1000));
        assert_eq!(parse_int("_1"), None);
        assert_eq!(parse_int("+5"), Some(5));
        assert_eq!(parse_int("1.2"), None);
        assert_eq!(parse_float("1_000.5e1_0"), Some(1000.5e10));
        assert_eq!(parse_float("123._34"), None);
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float("++1"), None);
        assert_eq!(parse_float("+-1"), None);
        assert_eq!(parse_float("1e"), None);
        assert_eq!(parse_float("."), None);
        assert_eq!(parse_float("NaNf"), None);
        assert_eq!(parse_float("0x1p3"), Some(8.0));
        assert_eq!(parse_float("0x1.8p1"), Some(3.0));
        assert_eq!(parse_float("-0x.8p-1"), Some(-0.25));
        assert_eq!(parse_float("0x1.fffffffffffffp1023"), Some(f64::MAX));
        assert_eq!(parse_float("0x1p-1074"), Some(f64::from_bits(1)));
        assert_eq!(parse_float("0x1p"), None);
        assert_eq!(parse_float(" 1.5f "), Some(1.5));
        // pkl drops a separator after a sign or inside NaN, as Java's
        // parsing of the result then accepts.
        assert_eq!(parse_float("+_1"), Some(1.0));
        assert_eq!(parse_float("1e+_5"), Some(1e5));
        assert!(parse_float("N_aN").unwrap().is_nan());
    }
}
