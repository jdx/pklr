//! The declarations of the standard library's `pkl:base` classes that
//! `pkl:reflect` describes: doc comments, modifiers, annotations and where in
//! `base.pkl` each is declared.
//!
//! pklr implements the standard library natively and embeds no pkl source for
//! it, so this table (`reflect_base.txt`, see `scripts/gen-reflect-base.py`)
//! is the one place the declarations live. It is only read when a script
//! reflects a standard library class, and then only as far as the class and
//! its superclasses.

/// The pkl release whose `base.pkl` `reflect_base.txt` was generated from, and
/// the one the source locations of its declarations refer to.
pub(super) const PKL_VERSION: &str = "0.32.2";

const TABLE: &str = include_str!("reflect_base.txt");

/// A type parameter, such as `out Element`.
pub(super) struct TypeParam {
    /// `in`, `out` or nothing.
    pub(super) variance: &'static str,
    pub(super) name: &'static str,
}

/// An annotation of a standard library declaration, such as
/// `@AlsoKnownAs { names { "size"; "count" } }`.
pub(super) struct Annotation {
    pub(super) name: &'static str,
    /// Each property the annotation sets: its name and value.
    pub(super) properties: Vec<(&'static str, AnnotationValue)>,
}

pub(super) enum AnnotationValue {
    String(&'static str),
    /// A listing of strings.
    Strings(Vec<&'static str>),
}

/// A member of a standard library class.
pub(super) struct BaseMember {
    pub(super) is_method: bool,
    pub(super) line: u32,
    pub(super) modifiers: Vec<&'static str>,
    pub(super) name: &'static str,
    pub(super) type_params: Vec<TypeParam>,
    pub(super) params: Vec<&'static str>,
    pub(super) doc: Option<String>,
    pub(super) annotations: Vec<Annotation>,
}

/// A class of `pkl:base`.
pub(super) struct BaseClass {
    pub(super) line: u32,
    pub(super) modifiers: Vec<&'static str>,
    pub(super) name: &'static str,
    pub(super) type_params: Vec<TypeParam>,
    /// The `extends` clause as written, such as `Collection<Element>`.
    pub(super) extends: Option<&'static str>,
    pub(super) doc: Option<String>,
    pub(super) annotations: Vec<Annotation>,
    pub(super) members: Vec<BaseMember>,
}

/// The source location `base.pkl` declarations report.
pub(super) fn display_uri(line: u32) -> String {
    format!("https://github.com/apple/pkl/blob/{PKL_VERSION}/stdlib/base.pkl#L{line}")
}

fn type_params(field: &'static str) -> Vec<TypeParam> {
    field
        .split(',')
        .filter(|param| !param.is_empty())
        .map(|param| {
            let (variance, name) = param.split_once(':').unwrap_or(("", param));
            TypeParam { variance, name }
        })
        .collect()
}

fn list(field: &'static str) -> Vec<&'static str> {
    field.split(',').filter(|item| !item.is_empty()).collect()
}

fn push_doc(doc: &mut Option<String>, text: &str) {
    match doc {
        Some(doc) => {
            doc.push('\n');
            doc.push_str(text);
        }
        None => *doc = Some(text.to_string()),
    }
}

/// The class named `name`, when `pkl:base` declares one.
pub(super) fn find(name: &str) -> Option<BaseClass> {
    let mut class: Option<BaseClass> = None;
    let mut in_member = false;
    for line in TABLE.lines() {
        let fields: Vec<&'static str> = line.split('\t').collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or_default();
        match field(0) {
            "C" => {
                if class.is_some() {
                    break;
                }
                if field(3) != name {
                    continue;
                }
                in_member = false;
                class = Some(BaseClass {
                    line: field(1).parse().unwrap_or_default(),
                    modifiers: list(field(2)),
                    name: field(3),
                    type_params: type_params(field(4)),
                    extends: Some(field(5)).filter(|extends| !extends.is_empty()),
                    doc: None,
                    annotations: Vec::new(),
                    members: Vec::new(),
                });
            }
            kind @ ("P" | "M") => {
                let Some(class) = &mut class else { continue };
                let is_method = kind == "M";
                in_member = true;
                class.members.push(BaseMember {
                    is_method,
                    line: field(1).parse().unwrap_or_default(),
                    modifiers: list(field(2)),
                    name: field(3),
                    type_params: type_params(field(4)),
                    params: list(field(5)),
                    doc: None,
                    annotations: Vec::new(),
                });
            }
            "D" => {
                let Some(class) = &mut class else { continue };
                let target = match class.members.last_mut() {
                    Some(member) if in_member => &mut member.doc,
                    _ => &mut class.doc,
                };
                push_doc(target, field(1));
            }
            "A" => {
                let Some(class) = &mut class else { continue };
                let properties = fields[2..]
                    .iter()
                    .filter_map(|property| {
                        let (key, value) = property.split_once('=')?;
                        Some(match key.strip_suffix("[]") {
                            Some(key) => (
                                key,
                                AnnotationValue::Strings(
                                    value.split('\u{1f}').filter(|v| !v.is_empty()).collect(),
                                ),
                            ),
                            None => (key, AnnotationValue::String(value)),
                        })
                    })
                    .collect();
                let annotation = Annotation {
                    name: field(1),
                    properties,
                };
                match class.members.last_mut() {
                    Some(member) if in_member => member.annotations.push(annotation),
                    _ => class.annotations.push(annotation),
                }
            }
            _ => {}
        }
    }
    class
}
