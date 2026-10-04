//! Pkl's text forms of values: what `toString()` and string interpolation
//! produce, and how error messages quote values. Ports pkl-core's
//! `VmValueRenderer` (single-line mode) and `ValueFormatter`.

use crate::value::{ObjectSource, Value};

/// Java's `Double.toString`, which pkl uses to print a `Float`: the shortest
/// digits that round-trip, in plain notation for magnitudes in
/// `[1e-3, 1e7)` and in `1.0E7` form otherwise, always with a fraction digit.
pub(crate) fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    // `{:e}` prints the shortest round-trip digits, e.g. `1.2345e7` or `5e-324`.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exponent) = sci.split_once('e').expect("`{:e}` has an exponent");
    let exponent: i32 = exponent.parse().expect("`{:e}` exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if f < 0.0 {
        out.push('-');
    }
    let abs = f.abs();
    if (1e-3..1e7).contains(&abs) {
        if exponent >= 0 {
            let int_len = exponent as usize + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                out.extend(std::iter::repeat_n('0', int_len - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        } else {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-exponent - 1) as usize));
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        out.push('.');
        if digits.len() > 1 {
            out.push_str(&digits[1..]);
        } else {
            out.push('0');
        }
        out.push('E');
        out.push_str(&exponent.to_string());
    }
    out
}

/// A pkl string literal for `s`. With `custom_delimiters`, the literal uses
/// as many `#`s as needed to avoid escaping backslashes and quotes, as
/// `Regex.toString()` does; otherwise they are escaped.
pub(crate) fn quote_string(s: &str, custom_delimiters: bool) -> String {
    let pounds = if custom_delimiters {
        "#".repeat(pound_count(s))
    } else {
        String::new()
    };
    let escape = format!("\\{pounds}");
    let mut out = String::with_capacity(s.len() + 2);
    out.push_str(&pounds);
    out.push('"');
    let mut rest = s;
    if custom_delimiters {
        if s == "\"" {
            out.push_str(&escape);
            out.push('"');
            rest = "";
        } else if let Some(tail) = s.strip_prefix("\"\"") {
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
            '\\' if !custom_delimiters => out.push_str("\\\\"),
            '"' if !custom_delimiters => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out.push_str(&pounds);
    out
}

/// How many `#`s a single-line custom-delimited literal for `s` needs: one
/// more than the longest run of `#`s after a quote or backslash.
fn pound_count(s: &str) -> usize {
    #[derive(PartialEq)]
    enum Context {
        Other,
        Quote,
        Backslash,
    }
    let mut context = Context::Other;
    let (mut current, mut max) = (0, 0);
    for ch in s.chars() {
        match ch {
            '\\' | '"' => {
                context = if ch == '\\' {
                    Context::Backslash
                } else {
                    Context::Quote
                };
                current = 1;
                max = max.max(current);
            }
            '#' if context != Context::Other => {
                current += 1;
                max = max.max(current);
            }
            _ => context = Context::Other,
        }
    }
    max
}

/// Whether `name` can be written as a member name without backticks.
pub(crate) fn is_regular_identifier(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "_",
        "abstract",
        "amends",
        "as",
        "case",
        "class",
        "const",
        "delete",
        "else",
        "extends",
        "external",
        "false",
        "fixed",
        "for",
        "function",
        "hidden",
        "if",
        "import",
        "in",
        "is",
        "let",
        "local",
        "module",
        "new",
        "nothing",
        "null",
        "open",
        "out",
        "outer",
        "override",
        "protected",
        "read",
        "record",
        "super",
        "switch",
        "this",
        "throw",
        "trace",
        "true",
        "typealias",
        "unknown",
        "vararg",
        "when",
    ];
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    !KEYWORDS.contains(&name)
        && (first == '$' || first == '_' || first.is_alphabetic())
        && chars.all(|c| c == '$' || c == '_' || c.is_alphanumeric())
}

/// The result of `value.toString()`: strings as they are, every other value
/// in its single-line source form.
pub(crate) fn to_pkl_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.to_string(),
        _ => render_value(value),
    }
}

/// A value's single-line source form, e.g. `List(1, "a")` or
/// `new Dynamic { a = 1 }`, as pkl prints values in `toString()` and errors.
pub(crate) fn render_value(value: &Value) -> String {
    let mut out = String::new();
    render(value, true, &mut out);
    out
}

/// `render_value` cut to `limit` characters (ending in `...`), as error
/// messages quote values.
pub(crate) fn render_value_limited(value: &Value, limit: usize) -> String {
    let rendered = render_value(value);
    if rendered.chars().count() < limit {
        return rendered;
    }
    let mut cut: String = rendered.chars().take(limit.saturating_sub(3)).collect();
    cut.push_str("...");
    cut
}

/// The class name `new <Name> {...}` shows for an object.
fn object_class_name(source: &Option<std::sync::Arc<ObjectSource>>) -> &str {
    source
        .as_deref()
        .and_then(ObjectSource::type_name)
        .unwrap_or("Dynamic")
}

/// Render `value`. `explicit` is false for an object that is a property's
/// value, which pkl writes without its `new <Class>` prefix.
fn render(value: &Value, explicit: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&format_float(*f)),
        Value::String(s) => out.push_str(&quote_string(s, false)),
        Value::List(items) => {
            out.push_str("List(");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render(item, true, out);
            }
            out.push(')');
        }
        Value::Object(map, source) => {
            if explicit {
                out.push_str("new ");
                out.push_str(object_class_name(source));
                out.push(' ');
            }
            out.push('{');
            let mut first = true;
            for (key, member) in map.iter() {
                if matches!(member, Value::Lambda(..)) {
                    continue;
                }
                out.push_str(if first { " " } else { "; " });
                first = false;
                if is_regular_identifier(key) {
                    out.push_str(key);
                } else {
                    out.push('[');
                    out.push_str(&quote_string(key, false));
                    out.push(']');
                }
                out.push_str(if matches!(member, Value::Object(..)) {
                    " "
                } else {
                    " = "
                });
                render(member, false, out);
            }
            if !first {
                out.push(' ');
            }
            out.push('}');
        }
        Value::Lambda(..) => {
            out.push_str("new ");
            out.push_str(value.type_name());
            out.push_str(" {}");
        }
        Value::Regex(regex) => {
            out.push_str("Regex(");
            out.push_str(&quote_string(regex.pattern(), true));
            out.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_like_java() {
        for (f, s) in [
            (1.0, "1.0"),
            (1e20, "1.0E20"),
            (1e-7, "1.0E-7"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (123456789.0, "1.23456789E8"),
            (1234567.0, "1234567.0"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-0.0, "-0.0"),
            (2.5e-5, "2.5E-5"),
            (100.0, "100.0"),
            (1e7, "1.0E7"),
            (9999999.0, "9999999.0"),
            (-1.5, "-1.5"),
            (f64::MAX, "1.7976931348623157E308"),
        ] {
            assert_eq!(format_float(f), s, "{f}");
        }
    }

    #[test]
    fn custom_delimiters_cover_backslashes_and_quotes() {
        assert_eq!(quote_string(r"a\d", true), r##"#"a\d"#"##);
        assert_eq!(quote_string("abc", true), r#""abc""#);
        assert_eq!(quote_string("a\"#b", true), "##\"a\"#b\"##");
        assert_eq!(quote_string("a\"b\\", false), r#""a\"b\\""#);
    }
}
