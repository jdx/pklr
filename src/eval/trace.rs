//! `trace(expr)` messages, in pkl's default (`compact`) trace mode:
//! `pkl: TRACE: <source> = <value> (<module uri>, line <n>)` on stderr.

use super::*;
use crate::eval::render::{Kind, display_value, java_double_to_string, kind_of};
use crate::eval::stdlib::render::quote_string;
use crate::parser::TraceSite;
use crate::value::mapping_storage_value;

impl Evaluator {
    pub(super) fn trace(&mut self, site: &TraceSite, value: &Value) {
        let uri = if site.module.contains("://") {
            site.module.clone()
        } else {
            let path = self.host_absolute_path(PathBuf::from(&site.module));
            file_uri(&path)
        };
        eprintln!("{}", trace_message(site, value, &uri));
    }
}

fn trace_message(site: &TraceSite, value: &Value, uri: &str) -> String {
    format!(
        "pkl: TRACE: {} = {} ({uri}, line {})",
        site.source,
        trace_value(value),
        site.line
    )
}

/// Render `value` on one line, in the style of pkl's compact trace output.
pub(super) fn trace_value(value: &Value) -> String {
    let mut out = String::new();
    write_trace_value(value, &mut out, true);
    out
}

fn write_trace_value(value: &Value, out: &mut String, top: bool) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(&b.to_string()),
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&java_double_to_string(*f)),
        Value::String(s) => write_trace_string(s, out),
        Value::List(items) => {
            if items.kind() == ListKind::Listing {
                out.push_str("new Listing { ");
            } else {
                out.push_str(match items.kind() {
                    ListKind::List => "List(",
                    ListKind::Set => "Set(",
                    ListKind::Listing => unreachable!(),
                });
            }
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(if items.kind() == ListKind::Listing {
                        "; "
                    } else {
                        ", "
                    });
                }
                write_trace_value(item, out, true);
            }
            if items.kind() == ListKind::Listing {
                out.push_str(" }");
            } else {
                out.push(')');
            }
        }
        Value::Object(map, source) => {
            if matches!(kind_of(value), Kind::Duration | Kind::DataSize) {
                out.push_str(&display_value(value));
                return;
            }
            if top {
                let type_name = source
                    .as_ref()
                    .and_then(|source| source.type_name.as_deref())
                    .unwrap_or("Dynamic");
                out.push_str("new ");
                out.push_str(type_name);
                out.push(' ');
            }
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            for (i, (key, member)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str("; ");
                }
                if key.starts_with(crate::value::MAPPING_KEY_PREFIX) {
                    out.push('[');
                    write_trace_value(&mapping_storage_value(key), out, true);
                    out.push(']');
                } else if is_identifier(key) {
                    out.push_str(key);
                } else {
                    out.push('[');
                    write_trace_string(key, out);
                    out.push(']');
                }
                if matches!(member, Value::Object(..)) {
                    out.push(' ');
                    write_trace_value(member, out, false);
                } else {
                    out.push_str(" = ");
                    write_trace_value(member, out, true);
                }
            }
            out.push_str(" }");
        }
        Value::Lambda(..) => out.push_str(&crate::eval::stdlib::render_value(value)),
        Value::Regex(regex) => {
            out.push_str("Regex(");
            out.push_str(&quote_string(regex.pattern(), true));
            out.push(')');
        }
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn write_trace_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Regex;

    #[test]
    fn renders_values_like_pkl() {
        assert_eq!(
            trace_value(&Value::String("a\"b\nc".into())),
            r#""a\"b\nc""#
        );
        assert_eq!(trace_value(&Value::Float(2.0)), "2.0");
        assert_eq!(trace_value(&Value::Float(1e7)), "1.0E7");
        assert_eq!(
            trace_value(&Value::List(vec![Value::Int(1), Value::Int(2)].into())),
            "List(1, 2)"
        );
        assert_eq!(
            trace_value(&Value::Regex(Arc::new(Regex::new(r"\d+").unwrap()))),
            r##"Regex(#"\d+"#)"##
        );
        assert_eq!(
            trace_value(&Value::List(ListValue::new(
                ListKind::Listing,
                vec![Value::Int(1), Value::Int(2)]
            ))),
            "new Listing { 1; 2 }"
        );
        let mut mapping = ObjectMap::default();
        let key = crate::value::mapping_storage_key(&Value::Int(1)).unwrap();
        mapping.insert(key, Value::String("one".into()));
        assert_eq!(
            trace_value(&Value::Object(Arc::new(mapping), None)),
            r#"new Dynamic { [1] = "one" }"#
        );
        let mut duration = ObjectMap::default();
        duration.insert("value".into(), Value::Int(5));
        duration.insert("unit".into(), Value::String("min".into()));
        assert_eq!(
            trace_value(&Value::Object(Arc::new(duration), None)),
            "5.min"
        );
        let mut inner = ObjectMap::default();
        inner.insert("name".into(), Value::String("Parrot".into()));
        let mut outer = ObjectMap::default();
        outer.insert("Parrot".into(), Value::Object(Arc::new(inner), None));
        outer.insert("a b".into(), Value::Int(1));
        assert_eq!(
            trace_value(&Value::Object(Arc::new(outer), None)),
            r#"new Dynamic { Parrot { name = "Parrot" }; ["a b"] = 1 }"#
        );
        assert_eq!(
            trace_message(
                &TraceSite {
                    source: "Regex(#\"\\d+\"#)".into(),
                    module: "ignored.pkl".into(),
                    line: 4,
                },
                &Value::Regex(Arc::new(Regex::new(r"\d+").unwrap())),
                "file:///tmp/main.pkl",
            ),
            r##"pkl: TRACE: Regex(#"\d+"#) = Regex(#"\d+"#) (file:///tmp/main.pkl, line 4)"##
        );
    }
}
