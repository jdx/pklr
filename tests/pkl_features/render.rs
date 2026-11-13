use super::*;

// ============================================================
// Renderers (JsonRenderer, YamlRenderer, PropertiesRenderer, PcfRenderer)
// Expected text is what `pkl eval` 0.32.1 renders.
// ============================================================

const DYNAMIC: &str = r#"
local d = new Dynamic {
  name = "pigeon"
  nested { list = new Listing { 1; "two"; new Dynamic { z = true } }; empty {} }
  ["entry key"] = "v"
  missing = null
  ratio = 1.5
}
"#;

fn render(expr: &str) -> String {
    let json = eval(&format!(
        "{DYNAMIC}\nclass Person {{ name: String; age: Int }}\nres = {expr}"
    ));
    json["res"].as_str().unwrap().to_string()
}

#[test]
fn json_renderer_document() {
    assert_eq!(
        render("new JsonRenderer {}.renderDocument(d)"),
        "{\n  \"name\": \"pigeon\",\n  \"nested\": {\n    \"list\": [\n      1,\n      \"two\",\n      {\n        \"z\": true\n      }\n    ],\n    \"empty\": {}\n  },\n  \"ratio\": 1.5,\n  \"entry key\": \"v\"\n}\n"
    );
}

#[test]
fn json_renderer_settings() {
    assert_eq!(
        render(
            r#"new JsonRenderer { indent = "" }.renderValue(new Person { name = "a\"b"; age = 1 })"#
        ),
        r#"{"name":"a\"b","age":1}"#
    );
    assert_eq!(
        render(
            "new JsonRenderer { omitNullProperties = false }.renderValue(new Dynamic { a = null })"
        ),
        "{\n  \"a\": null\n}"
    );
}

#[test]
fn yaml_and_pcf_renderers_null_property_settings() {
    // Expected text is what `pkl eval` 0.32.2 renders.
    let v = "new Dynamic { a = null; b = 1 }";
    assert_eq!(
        render(&format!(
            "new YamlRenderer {{ omitNullProperties = false }}.renderValue({v})"
        )),
        "a: null\nb: 1"
    );
    assert_eq!(
        render(&format!("new YamlRenderer {{}}.renderValue({v})")),
        "b: 1"
    );
    assert_eq!(
        render(&format!("new PcfRenderer {{}}.renderValue({v})")),
        "{\n  a = null\n  b = 1\n}"
    );
    assert_eq!(
        render(&format!(
            "new PcfRenderer {{ omitNullProperties = true }}.renderValue({v})"
        )),
        "{\n  b = 1\n}"
    );
}

#[test]
fn json_renderer_refuses_durations() {
    let err = eval_fails("res = new JsonRenderer {}.renderValue(1.min)");
    assert!(
        err.contains("Cannot render value of type `Duration` as JSON."),
        "{err}"
    );
}

#[test]
fn yaml_renderer_document() {
    assert_eq!(
        render("new YamlRenderer {}.renderDocument(d)"),
        "name: pigeon\nnested:\n  list:\n  - 1\n  - two\n  - z: true\n  empty: {}\nratio: 1.5\nentry key: v\n"
    );
}

#[test]
fn yaml_renderer_quotes_strings_by_mode() {
    assert_eq!(
        render(
            r#"new YamlRenderer {}.renderValue(List("yes", "0123", "1:30", "- a", "a: b", "", " lead", "tab\there", "multi\nline"))"#
        ),
        "- 'yes'\n- '0123'\n- '1:30'\n- '- a'\n- 'a: b'\n- ''\n- ' lead'\n- \"tab\\there\"\n- |-\n  multi\n  line"
    );
    assert_eq!(
        render(r#"new YamlRenderer { mode = "1.2" }.renderValue(List("yes", "0123", "on"))"#),
        "- yes\n- '0123'\n- on"
    );
}

#[test]
fn yaml_renderer_indent_width_and_stream() {
    assert_eq!(
        render(
            "new YamlRenderer { indentWidth = 4 }.renderDocument(new Dynamic { a { b = List(1, new Dynamic { c = 2 }) } })"
        ),
        "a:\n    b:\n    - 1\n    -   c: 2\n"
    );
    assert_eq!(
        render(
            r#"new YamlRenderer { isStream = true }.renderDocument(List(new Dynamic { a = 1 }, "two"))"#
        ),
        "a: 1\n--- two\n"
    );
}

#[test]
fn properties_renderer() {
    assert_eq!(
        render(
            r#"new PropertiesRenderer {}.renderDocument(new Dynamic { a = 1; b { c = "x y"; d = null; e = 1.5 }; ["k:v"] = "=!" })"#
        ),
        "a = 1\nb.c = x y\nb.e = 1.5\nk\\:v = \\=\\!\n"
    );
    assert_eq!(
        render(
            r#"new PropertiesRenderer { restrictCharset = true }.renderDocument(new Dynamic { a = "é" })"#
        ),
        "a = \\u00E9\n"
    );
}

#[test]
fn pcf_renderer() {
    assert_eq!(
        render("new PcfRenderer {}.renderDocument(d)"),
        "name = \"pigeon\"\nnested {\n  list {\n    1\n    \"two\"\n    new {\n      z = true\n    }\n  }\n  empty {}\n}\nmissing = null\nratio = 1.5\n[\"entry key\"] = \"v\"\n"
    );
    assert_eq!(
        render(r#"new PcfRenderer { useCustomStringDelimiters = true }.renderValue("a\"b\\c")"#),
        r##"#"a"b\c"#"##
    );
}

#[test]
fn renderer_converters_by_class_and_path() {
    assert_eq!(
        render(
            r#"new JsonRenderer {
  indent = ""
  converters {
    [String] = (s) -> s + "!"
    [Person] = (p) -> p.name
    ["^b"] = (_) -> 42
    ["c.*"] = (_) -> "path"
  }
}.renderValue(new Dynamic { a = "x"; b = "y"; c { d = 1 }; e = new Person { name = "p"; age = 1 } })"#
        ),
        r#"{"a":"x!","b":42,"c":{"d":"path"},"e":"p"}"#
    );
}

#[test]
fn render_directive_is_rendered_verbatim() {
    assert_eq!(
        render(
            r#"new YamlRenderer {}.renderDocument(new Dynamic { a = new RenderDirective { text = " raw" } })"#
        ),
        "a: raw\n"
    );
}

// ============================================================
// Module output (output.value, output.renderer, output.text)
// ============================================================

#[cfg(feature = "native-io")]
fn module_file(name: &str, src: &str) -> (TestTempDir, std::path::PathBuf) {
    let dir = TestTempDir::new(name);
    let path = dir.path().join("main.pkl");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

#[cfg(feature = "native-io")]
#[test]
fn output_text_uses_module_renderer() {
    let (_dir, path) = module_file(
        "pklr_render_output_renderer",
        "a = 1\nb { c = \"x\" }\noutput { renderer = new YamlRenderer {} }\n",
    );
    assert_eq!(pklr::eval_to_text(&path).unwrap(), "a: 1\nb:\n  c: x\n");
}

#[cfg(feature = "native-io")]
#[test]
fn output_text_defaults_to_pcf() {
    let (_dir, path) = module_file("pklr_render_output_pcf", "a = 1\nb { c = \"x\" }\n");
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "a = 1\nb {\n  c = \"x\"\n}\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn output_text_and_value() {
    let (_dir, path) = module_file("pklr_render_output_text", "output { text = \"hi\" }\n");
    assert_eq!(pklr::eval_to_text(&path).unwrap(), "hi");
    let (_dir, path) = module_file(
        "pklr_render_output_value",
        "pigeon { name = \"p\" }\noutput { value = pigeon; renderer = new JsonRenderer {} }\n",
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"name\": \"p\"\n}\n"
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"name": "p"})
    );
}

#[cfg(feature = "native-io")]
#[test]
fn eval_to_json_refuses_what_json_renderer_refuses() {
    for (src, message) in [
        (
            "a = 5.min",
            "Cannot render value of type `Duration` as JSON.",
        ),
        (
            "a = 3.mb",
            "Cannot render value of type `DataSize` as JSON.",
        ),
        (
            "a = Regex(\"x\")",
            "Cannot render value of type `Regex` as JSON.",
        ),
        ("a = NaN", "Cannot render value `NaN` as JSON."),
        (
            "a = List((x) -> x)",
            "Cannot render value of type `Function1` as JSON.",
        ),
    ] {
        let (_dir, path) = module_file("pklr_render_json_refuses", src);
        let err = pklr::eval_to_json(&path).unwrap_err().to_string();
        assert!(err.contains(message), "{src}: {err}");
    }
}

#[cfg(feature = "native-io")]
#[test]
fn eval_to_json_omits_null_properties_by_default() {
    // As `pkl eval -f json`: null properties and mapping entries go, null
    // listing elements stay.
    let src = "p: String? = null\nd { a = null; b = 1 }\nm = new Mapping { [\"k\"] = null; [\"j\"] = 1 }\nl = new Listing { null; 1 }\n";
    let expected = serde_json::json!({"d": {"b": 1}, "m": {"j": 1}, "l": [null, 1]});
    let (_dir, path) = module_file("pklr_render_json_nulls", src);
    assert_eq!(pklr::eval_to_json(&path).unwrap(), expected);
    // An explicit renderer with no setting behaves the same.
    let (_dir, path) = module_file(
        "pklr_render_json_explicit_default",
        &format!("{src}output {{ renderer = new JsonRenderer {{}} }}\n"),
    );
    assert_eq!(pklr::eval_to_json(&path).unwrap(), expected);
    let (_dir, path) = module_file(
        "pklr_render_json_omit_nulls",
        "a = null\nb = 1\noutput { renderer = new JsonRenderer { omitNullProperties = true } }\n",
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"b": 1})
    );
}

#[cfg(feature = "native-io")]
#[test]
fn eval_to_json_omits_null_entries_with_non_string_keys() {
    // As pkl: the null check precedes the key check, so a null-valued entry
    // under a non-string key vanishes, while a non-null one still errors.
    let (_dir, path) = module_file(
        "pklr_render_json_null_int_key",
        "m = new Mapping { [1] = null }\nn { a = new Mapping { [2] = null; [\"k\"] = 1 } }\n",
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"m": {}, "n": {"a": {"k": 1}}})
    );
    for src in [
        "m = new Mapping { [1] = null; [2] = 3 }\n",
        "m = new Mapping { [1] = 5 }\n",
        "m = new Mapping { [1] = null }\noutput { renderer = new JsonRenderer { omitNullProperties = false } }\n",
    ] {
        let (_dir, path) = module_file("pklr_render_json_int_key_errors", src);
        let err = pklr::eval_to_json(&path).unwrap_err().to_string();
        assert!(
            err.contains("Cannot render object with non-string key as JSON"),
            "{src}: {err}"
        );
    }
}

#[cfg(feature = "native-io")]
#[test]
fn eval_to_json_keeps_nulls_when_renderer_sets_omit_false() {
    let (_dir, path) = module_file(
        "pklr_render_json_keep_nulls",
        "a = null\nb = 1\nm = new Mapping { [\"k\"] = null }\noutput { renderer = new JsonRenderer { omitNullProperties = false } }\n",
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"a": null, "b": 1, "m": {"k": null}})
    );
}

#[cfg(feature = "native-io")]
#[test]
fn eval_to_json_reads_the_evaluated_omit_setting() {
    // A computed setting and an inherited renderer whose child flips `true` to
    // `false` both keep nulls: the evaluated renderer decides, not the source.
    let expected = serde_json::json!({"a": null, "b": 1});
    let (_dir, path) = module_file(
        "pklr_render_computed_omit_false",
        "local keep = true\na = null\nb = 1\noutput { renderer = new JsonRenderer { omitNullProperties = !keep } }\n",
    );
    assert_eq!(pklr::eval_to_json(&path).unwrap(), expected);
    let (_dir, path) = module_file(
        "pklr_render_inherited_omit_false",
        "local base = new JsonRenderer { omitNullProperties = true }\na = null\nb = 1\noutput { renderer = (base) { omitNullProperties = false } }\n",
    );
    assert_eq!(pklr::eval_to_json(&path).unwrap(), expected);
}

#[cfg(feature = "native-io")]
#[test]
fn output_renderer_defaults_omit_nulls_when_only_converters_are_set() {
    let (_dir, path) = module_file(
        "pklr_render_converter_omits_nulls",
        "a = null\nb = 1\noutput { renderer = new JsonRenderer { converters { [Int] = (n) -> n + 1 } } }\n",
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"b": 2})
    );
}

#[cfg(feature = "native-io")]
#[test]
fn output_must_be_module_output() {
    for (src, message) in [
        (
            "output: String = \"abc\"",
            "to be of type `ModuleOutput`, but got type `String`.",
        ),
        (
            "output = null",
            "Expected value of type `ModuleOutput`, but got `null`.",
        ),
    ] {
        let (_dir, path) = module_file("pklr_render_invalid_output", src);
        let err = pklr::eval_to_json(&path).unwrap_err().to_string();
        assert!(err.contains(message), "{src}: {err}");
    }
}

#[cfg(feature = "native-io")]
#[test]
fn output_path_converters_apply_to_json() {
    let (_dir, path) = module_file(
        "pklr_render_path_converters",
        r#"name = "pigeon"
friends = new Listing { "barn owl"; "parrot" }
hobbies { ["surfing"] { skill = "low" } }
address { street = "Norton St."; zip = 12345 }
output {
  renderer = new JsonRenderer {
    converters {
      ["friends[*]"] = (it) -> it + "x"
      ["hobbies[*]"] = (it) -> it.skill
      ["address.street"] = (it) -> "Other St."
      ["address.*"] = (it) -> "changed"
    }
  }
}
"#,
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({
            "name": "pigeon",
            "friends": ["barn owlx", "parrotx"],
            "hobbies": {"surfing": "low"},
            "address": {"street": "Other St.", "zip": "changed"},
        })
    );
}

#[cfg(feature = "native-io")]
#[test]
fn output_is_inherited_through_the_amends_chain() {
    let dir = TestTempDir::new("pklr_render_output_chain");
    std::fs::write(
        dir.path().join("c.pkl"),
        "local suffix = \"!\"\nx = \"c\"\noutput {\n  renderer = new JsonRenderer {\n    converters { [String] = (s) -> s + suffix }\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("b.pkl"), "amends \"c.pkl\"\nx = \"b\"\n").unwrap();
    std::fs::write(dir.path().join("a.pkl"), "amends \"b.pkl\"\nx = \"a\"\n").unwrap();
    let path = dir.path().join("a.pkl");
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"x\": \"a!\"\n}\n"
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"x": "a!"})
    );
}

#[cfg(feature = "native-io")]
#[test]
fn inherited_output_keeps_base_lexicals_but_reads_final_members() {
    let dir = TestTempDir::new("pklr_render_base_output_scope");
    std::fs::write(dir.path().join("base-lib.pkl"), "label = \"base-import\"\n").unwrap();
    std::fs::write(
        dir.path().join("child-lib.pkl"),
        "label = \"child-import\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("base.pkl"),
        r#"import "pkl:base" as pkl
import "base-lib.pkl" as Lib
local prefix = "base-local"
class Marker { label = "base-class" }
function Regex(pattern: String): Regex = pkl.Regex(pattern)
childValue = "base-member"
output {
  value = prefix + ":" + Lib.label + ":" + (new Marker {}).label + ":" + childValue
  renderer {
    converters {
      [Regex] = (value) -> value.pattern
      [String] = (value) -> value + "!"
    }
  }
}
"#,
    )
    .unwrap();
    let child = dir.path().join("child.pkl");
    std::fs::write(
        &child,
        r#"amends "base.pkl"
import "child-lib.pkl" as Lib
local prefix = "child-local"
local class Marker { label = "child-class" }
childValue = "child-member"
"#,
    )
    .unwrap();

    assert_eq!(
        pklr::eval_to_json(&child).unwrap(),
        serde_json::json!("base-local:base-import:base-class:child-member!")
    );
}

#[cfg(feature = "native-io")]
#[test]
fn inherited_output_captures_transitive_type_aliases() {
    let dir = TestTempDir::new("pklr_render_inherited_output_type_aliases");
    std::fs::write(
        dir.path().join("base.pkl"),
        r#"class Marker { label = "base" }
typealias Rendered = Intermediate
typealias Intermediate = Marker
output {
  renderer {
    converters {
      [Any] = (value) -> if (value is Rendered) "base" else "wrong"
    }
  }
}
"#,
    )
    .unwrap();
    let child = dir.path().join("child.pkl");
    std::fs::write(
        &child,
        "amends \"base.pkl\"\noutput { value = new Marker {} }\n",
    )
    .unwrap();

    assert_eq!(
        pklr::eval_to_json(&child).unwrap(),
        serde_json::json!("base")
    );
}

#[cfg(feature = "native-io")]
#[test]
fn inherited_output_aliases_keep_their_base_scope() {
    let dir = TestTempDir::new("pklr_render_inherited_output_alias_scope");
    std::fs::write(
        dir.path().join("base.pkl"),
        r#"typealias Alias = String
output {
  renderer {
    converters { [String] = (value) -> value as Alias }
  }
}
"#,
    )
    .unwrap();
    let child = dir.path().join("child.pkl");
    std::fs::write(
        &child,
        "amends \"base.pkl\"\nlocal typealias String = Int\noutput { value = \"child\" }\n",
    )
    .unwrap();

    assert_eq!(
        pklr::eval_to_json(&child).unwrap(),
        serde_json::json!("child")
    );
}

#[cfg(feature = "native-io")]
#[test]
fn converter_key_uses_builtin_type_when_a_function_has_its_name() {
    let (_dir, path) = module_file(
        "pklr_render_converter_builtin_function_name",
        r#"import "pkl:base" as base
function Regex(pattern: String): Regex = base.Regex(pattern)
pattern = Regex("x")
output {
  renderer {
    converters {
      [Regex] = (value) -> value.pattern + "!"
    }
  }
}
"#,
    );

    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"pattern": "x!"})
    );
}

#[cfg(feature = "native-io")]
#[test]
fn output_super_text_uses_the_inherited_value_and_renderer() {
    let dir = TestTempDir::new("pklr_render_inherited_super_text");
    std::fs::write(
        dir.path().join("base.pkl"),
        "output {\n  value = new Dynamic { inherited = \"yes\" }\n  renderer = new JsonRenderer {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("child.pkl"),
        "amends \"base.pkl\"\noutput { text = super.text }\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_text(&dir.path().join("child.pkl")).unwrap(),
        "{\n  \"inherited\": \"yes\"\n}\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn output_super_text_preserves_explicit_value_kinds() {
    for (name, output, expected) in [
        (
            "pklr_render_super_text_mapping",
            r#"value = new Mapping { ["a"] = 1 }
  renderer = new JsonRenderer { converters { [Mapping] = (_) -> "mapping" } }"#,
            "\"mapping\"\n",
        ),
        (
            "pklr_render_super_text_listing",
            r#"value = new Listing { 1; 2 }
  renderer = new JsonRenderer { converters { [Listing] = (_) -> "listing" } }"#,
            "\"listing\"\n",
        ),
        (
            "pklr_render_super_text_scalar",
            r#"value = 1
  renderer = new JsonRenderer { converters { [Int] = (_) -> "integer" } }"#,
            "\"integer\"\n",
        ),
    ] {
        let dir = TestTempDir::new(name);
        std::fs::write(
            dir.path().join("base.pkl"),
            format!("output {{\n  {output}\n}}\n"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("child.pkl"),
            "amends \"base.pkl\"\noutput { text = super.text }\n",
        )
        .unwrap();
        assert_eq!(
            pklr::eval_to_text(&dir.path().join("child.pkl")).unwrap(),
            expected,
            "{name}"
        );
    }
}

#[cfg(feature = "native-io")]
#[test]
fn output_super_text_treats_implicit_module_values_as_typed() {
    let dir = TestTempDir::new("pklr_render_super_text_implicit_module");
    std::fs::write(
        dir.path().join("base.pkl"),
        "a = \"module\"\noutput { renderer = new JsonRenderer {} }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("child.pkl"),
        "amends \"base.pkl\"\noutput { text = super.text }\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_text(&dir.path().join("child.pkl")).unwrap(),
        "{\n  \"a\": \"module\"\n}\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn output_super_text_falls_back_for_empty_module_output() {
    let dir = TestTempDir::new("pklr_render_super_text_empty_output");
    std::fs::write(
        dir.path().join("base.pkl"),
        "a = 1\noutput = new ModuleOutput {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("child.pkl"),
        "amends \"base.pkl\"\noutput { text = super.text }\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_text(&dir.path().join("base.pkl")).unwrap(),
        "a = 1\n"
    );
    assert_eq!(
        pklr::eval_to_text(&dir.path().join("child.pkl")).unwrap(),
        "a = 1\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn output_super_text_uses_effective_generated_values() {
    for (name, base, expected) in [
        (
            "pklr_render_super_text_conditional_value",
            r#"a = "implicit"
output {
  when (true) {
    value = "generated"
  }
  renderer = new JsonRenderer {}
}
"#,
            "\"generated\"\n",
        ),
        (
            "pklr_render_super_text_absent_conditional_value",
            r#"a = "implicit"
output {
  when (false) {
    value = "not generated"
  }
  renderer = new JsonRenderer {}
}
"#,
            "{\n  \"a\": \"implicit\"\n}\n",
        ),
        (
            "pklr_render_super_text_spread_value",
            r#"a = "implicit"
local generated = new Dynamic {
  value = "spread"
}
output {
  ...generated
  renderer = new JsonRenderer {}
}
"#,
            "\"spread\"\n",
        ),
        (
            "pklr_render_super_text_for_spread_value",
            r#"a = "implicit"
output {
  for (generated in List(new Dynamic { value = "for" })) {
    ...generated
  }
  renderer = new JsonRenderer {}
}
"#,
            "\"for\"\n",
        ),
    ] {
        let dir = TestTempDir::new(name);
        std::fs::write(dir.path().join("base.pkl"), base).unwrap();
        std::fs::write(
            dir.path().join("child.pkl"),
            "amends \"base.pkl\"\noutput { text = super.text }\n",
        )
        .unwrap();
        for path in [dir.path().join("base.pkl"), dir.path().join("child.pkl")] {
            assert_eq!(
                pklr::eval_to_text(&path).unwrap(),
                expected,
                "{name}: {path:?}"
            );
        }
    }
}

#[cfg(feature = "native-io")]
#[test]
fn output_body_sees_default_renderer_and_value() {
    let (_dir, path) = module_file(
        "pklr_render_output_defaults",
        "x = 1\noutput {\n  value = new Dynamic { a = 1 }\n  text = renderer.renderDocument(value)\n}\n",
    );
    assert_eq!(pklr::eval_to_text(&path).unwrap(), "a = 1\n");
}

#[cfg(feature = "native-io")]
#[test]
fn output_super_text_is_evaluated_lazily() {
    for (name, output) in [
        (
            "pklr_render_output_unreachable_super_text",
            "text = if (false) super.text else \"ok\"",
        ),
        (
            "pklr_render_output_uncalled_super_text_lambda",
            "local delayed = () -> super.text\n  text = \"ok\"",
        ),
    ] {
        let (_dir, path) = module_file(
            name,
            &format!(
                r#"m = new Mapping {{ [1.s] = "value" }}
output {{
  {output}
  renderer = new JsonRenderer {{
    converters {{ [Duration] = (_) -> "duration-key" }}
  }}
}}
"#
            ),
        );
        assert_eq!(
            pklr::eval_to_json(&path).unwrap(),
            serde_json::json!({"m": {"duration-key": "value"}}),
            "{name}"
        );
    }

    let (_dir, path) = module_file(
        "pklr_render_output_called_super_text_lambda",
        "a = 1\noutput {\n  local delayed = () -> super.text\n  text = delayed()\n}\n",
    );
    assert_eq!(pklr::eval_to_text(&path).unwrap(), "a = 1\n");
}

#[cfg(feature = "native-io")]
#[test]
fn eval_to_json_converts_entry_keys() {
    let (_dir, path) = module_file(
        "pklr_render_json_keys",
        r#"m = new Mapping { ["a"] = "b" }
d { x = "y"; ["k"] = "v" }
output {
  renderer = new JsonRenderer {
    converters { [String] = (s) -> s + "!"; ["d[k!]"] = (_) -> 1 }
  }
}
"#,
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"m": {"a!": "b!"}, "d": {"x": "y!", "k!": 1}})
    );
}

#[test]
fn eval_to_json_decodes_non_string_mapping_keys() {
    let mut evaluator = Evaluator::new();
    let value = evaluator
        .eval_source(
            r#"m = new Mapping { [1] = "value" }"#,
            std::path::Path::new("test.pkl"),
        )
        .unwrap();
    assert_eq!(
        value.try_to_json().unwrap(),
        serde_json::json!({"m": {"1": "value"}})
    );
}

#[cfg(feature = "native-io")]
#[test]
fn typed_entry_key_conversion_updates_the_value_path() {
    let (_dir, path) = module_file(
        "pklr_render_json_typed_keys",
        r#"m = new Mapping { [1] = "value" }
output {
  renderer = new JsonRenderer {
    converters { [Int] = (_) -> "one"; ["m[one]"] = (_) -> "converted" }
  }
}
"#,
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"m": {"one": "converted"}})
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"m\": {\n    \"one\": \"converted\"\n  }\n}\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn eval_to_json_rejects_invalid_xml_comments() {
    let (_dir, path) = module_file(
        "pklr_render_json_xml_comment",
        "import \"pkl:xml\"\nx = xml.Comment(\"--\")\n",
    );
    let err = pklr::eval_to_json(&path).unwrap_err().to_string();
    assert!(err.contains("XML comments must not contain `--`"), "{err}");
}

#[test]
fn apply_converters_converts_lists() {
    let mut ev = Evaluator::new();
    let path = std::path::Path::new("test.pkl");
    ev.eval_source(
        "output { renderer { converters { [Int] = (i) -> i + 1 } } }\n",
        path,
    )
    .unwrap();
    let list = pklr::Value::from(serde_json::json!([1, [2]]));
    let converted = ev.apply_converters(list).unwrap();
    assert_eq!(converted.to_json(), serde_json::json!([2, [3]]));
}

#[test]
fn apply_converters_does_not_reenter_converter_through_its_container_result() {
    let mut ev = Evaluator::new();
    let value = ev
        .eval_source(
            r#"
class D {}
d = new D {}
output {
  renderer {
    converters {
      [D] = (value) -> List(value)
    }
  }
}

"#,
            std::path::Path::new("test.pkl"),
        )
        .unwrap();

    // `D -> List(D)` must convert once, not overflow by applying the same
    // converter again to the list child.
    assert_eq!(
        ev.apply_converters(value).unwrap().to_json(),
        serde_json::json!({"d": [{}]})
    );
}

#[test]
fn apply_converters_does_not_reenter_native_scalar_converter_roots() {
    let mut ev = Evaluator::new();
    let value = ev
        .eval_source(
            r#"
d = 1.s
size = 1.kb
nan = (0.0 / 0.0).s
pattern = Regex("x")
output {
  renderer {
    converters {
      [Duration] = (value) -> List(value)
      [DataSize] = (value) -> List(value)
      [Regex] = (value) -> List(value)
    }
  }
}
"#,
            std::path::Path::new("test.pkl"),
        )
        .unwrap();

    // Each `List(value)` contains the original scalar root. Re-applying its
    // converter would recurse forever, but unrelated converters still chain.
    assert_eq!(
        ev.apply_converters(value).unwrap().to_json(),
        serde_json::json!({
            "d": ["1.s"],
            "size": ["1.kb"],
            "nan": ["NaN.s"],
            "pattern": [{"_type": "regex", "pattern": "x"}],
        })
    );
}

/// Quirks of pkl 0.32.1's renderers that pklr keeps.
#[test]
fn renderer_quirks_match_pkl() {
    let json = eval(
        r#"
pcfEntryOnly = new PcfRenderer {}.renderDocument(new Dynamic { ["a"] = 1 })
pcfValue = new PcfRenderer {}.renderValue(new Dynamic { a = 1 })
yamlEmpty = new YamlRenderer {}.renderDocument(new Dynamic {})
yamlStream = new YamlRenderer { isStream = true; converters { [Int] = (i) -> i + 1 } }.renderDocument(List(1, new Dynamic { a = 2 }))
anyConverter = new JsonRenderer { indent = ""; converters { [Any] = (_) -> "any"; [Number] = (_) -> 0 } }.renderValue(List(1, "a"))
"#,
    );
    // A Pcf document starting with an entry starts with a newline.
    assert_eq!(json["pcfEntryOnly"], "\n[\"a\"] = 1\n");
    // renderValue renders an object as a body, without `new`.
    assert_eq!(json["pcfValue"], "{\n  a = 1\n}");
    // An empty top-level YAML mapping keeps its leading space.
    assert_eq!(json["yamlEmpty"], " {}\n");
    // Stream documents are not converted, but their members are.
    assert_eq!(json["yamlStream"], "1\n---\na: 3\n");
    // `Any` and `Number` converters don't apply to primitives.
    assert_eq!(json["anyConverter"], r#"[1,"a"]"#);
}

#[cfg(feature = "native-io")]
#[test]
fn default_output_value_is_the_typed_module() {
    let (_dir, path) = module_file(
        "pklr_render_output_default_value",
        "x = 1\ny { z = 2 }\noutput {\n  text = renderer.renderDocument(value) + \"--\\n\" + new YamlRenderer {}.renderDocument(value)\n}\n",
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "x = 1\ny {\n  z = 2\n}\n--\nx: 1\n'y':\n  z: 2\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn base_output_uses_base_locals_and_final_properties() {
    let dir = TestTempDir::new("pklr_render_output_base_scope");
    std::fs::write(
        dir.path().join("base.pkl"),
        "local suffix = \"!\"\nsuffix2 = \"?\"\nx = \"base\"\noutput {\n  renderer = new JsonRenderer {\n    converters { [String] = (s) -> s + suffix + suffix2 }\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.pkl"),
        "amends \"base.pkl\"\nlocal suffix = \"#\"\nsuffix2 = \"%\"\nx = \"child\"\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&dir.path().join("main.pkl")).unwrap(),
        serde_json::json!({"suffix2": "%!%", "x": "child!%"})
    );
}

// PListRenderer, pkl:jsonnet and pkl:xml.
fn render_with_imports(expr: &str) -> String {
    let json = eval(&format!(
        "import \"pkl:jsonnet\"\nimport \"pkl:xml\"\n{DYNAMIC}\nclass Person {{ name: String; age: Int }}\nres = {expr}"
    ));
    json["res"].as_str().unwrap().to_string()
}

#[test]
fn plist_renderer() {
    assert_eq!(
        render(r#"new PListRenderer {}.renderValue(List(1, 2.5, true, "<&>"))"#),
        "<array>\n  <integer>1</integer>\n  <real>2.5</real>\n  <true/>\n  <string>&lt;&amp;&gt;</string>\n</array>"
    );
    assert_eq!(
        render("new PListRenderer {}.renderDocument(new Dynamic { a = 1; b {} })"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>a</key>\n  <integer>1</integer>\n  <key>b</key>\n    <dict/>\n</dict>\n</plist>\n"
    );
}

#[test]
fn jsonnet_renderer() {
    assert_eq!(
        render_with_imports(
            r#"new jsonnet.Renderer { indent = "" }.renderDocument((d) { q = "it's"; `local` = 1 })"#
        ),
        "{ name: 'pigeon', nested: { list: [1, 'two', { z: true }], empty: {} }, ratio: 1.5, q: \"it's\", 'local': 1, 'entry key': 'v' }\n"
    );
    assert_eq!(
        render_with_imports(
            r#"new jsonnet.Renderer {}.renderDocument(new Dynamic { a = jsonnet.ExtVar("x"); b = jsonnet.ImportStr("f.txt") })"#
        ),
        "{\n  a: std.extVar('x'),\n  b: importstr 'f.txt',\n}\n"
    );
}

#[test]
fn unrelated_renderer_class_is_not_a_jsonnet_renderer() {
    let err = eval_fails("class Renderer {}\nres = new Renderer {}.renderValue(1)");
    assert!(err.contains("unknown method 'renderValue'"), "{err}");
}

#[test]
fn xml_renderer() {
    assert_eq!(
        render_with_imports(
            r#"new xml.Renderer { rootElementName = "people"; rootElementAttributes { ["v"] = 1 } }.renderDocument(new Dynamic { p = new Person { name = "a"; age = 2 }; l = new Listing { new Person { name = "b"; age = 3 }; "x"; xml.Comment("c"); xml.CData("<d>") } })"#
        ),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<people v=\"1\">\n  <p>\n    <name>a</name>\n    <age>2</age>\n  </p>\n  <l>\n    <Person>\n      <name>b</name>\n      <age>3</age>\n    </Person>x\n    <!--c--><![CDATA[<d>]]>\n  </l>\n</people>\n"
    );
    let err = eval_fails(
        "import \"pkl:xml\"\nres = new xml.Renderer {}.renderDocument(new Dynamic { [\"entry key\"] = 1 })",
    );
    assert!(
        err.contains("Invalid XML 1.0 element name: `entry key`"),
        "{err}"
    );
}

#[test]
fn xml_constraints_apply_to_all_renderers() {
    let err = eval_fails(
        "import \"pkl:xml\"\nres = new JsonRenderer {}.renderValue(xml.Comment(\"--\"))",
    );
    assert!(err.contains("XML comments must not contain `--`"), "{err}");
    let err = eval_fails(
        "import \"pkl:xml\"\nres = new xml.Renderer { xmlVersion = \"2.0\" }.renderValue(1)",
    );
    assert!(err.contains("\"1.0\"|\"1.1\""), "{err}");
}

#[test]
fn xml_renderer_preserves_element_content_and_attribute_whitespace() {
    assert_eq!(
        render_with_imports(
            r#"new xml.Renderer {}.renderDocument((xml.Element("parent")) { "before"; (xml.Element("child")) { "nested" }; "after"; ignored = "property" })"#
        ),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<parent>before\n  <child>nested</child>after\n</parent>\n"
    );
    assert_eq!(
        render_with_imports(
            r#"new xml.Renderer { rootElementAttributes { value = "a\tb\nc\rd" } }.renderDocument("x")"#
        ),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<root value=\"a&#x9;b&#xA;c&#xD;d\">x</root>\n"
    );
    assert_eq!(
        render_with_imports(
            r#"new xml.Renderer { xmlVersion = "1.1"; rootElementAttributes { value = "a\u{85}b\u{2028}c" } }.renderDocument("x")"#
        ),
        "<?xml version=\"1.1\" encoding=\"UTF-8\"?>\n<root value=\"a&#x85;b&#x2028;c\">x</root>\n"
    );
    assert_eq!(
        render_with_imports(
            r#"new xml.Renderer {}.renderDocument((xml.Element("parent")) { for (v in List("before", "after")) { v }; ...List("list", "items"); ...new Listing { "listing" }; ...Set("set"); ...new Dynamic { (xml.Element("child")) { "nested" } }; ignored = "property" })"#
        ),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<parent>beforeafterlistitemslistingset\n  <child>nested</child>\n</parent>\n"
    );
}

#[test]
fn xml_renderer_rejects_invalid_comment_and_characters() {
    let err = eval_fails(
        "import \"pkl:xml\"\nres = new xml.Renderer {}.renderValue(xml.Comment(\"trailing-\"))",
    );
    assert!(
        err.contains("must not contain `--` or end with `-`"),
        "{err}"
    );
    let err =
        eval_fails("import \"pkl:xml\"\nres = new xml.Renderer {}.renderValue(\"bad\\u{0}\")");
    assert!(err.contains("Invalid XML 1.0 character U+0000"), "{err}");
    let err = eval_fails("res = new PListRenderer {}.renderValue(\"bad\\u{0}\")");
    assert!(err.contains("Invalid XML 1.0 character U+0000"), "{err}");
    assert_eq!(
        render_with_imports(r#"new xml.Renderer { xmlVersion = "1.1" }.renderValue("a\u{1}b")"#),
        "a&#x1;b"
    );
}

#[test]
fn nullable_indent_is_jsonnet_only() {
    assert_eq!(
        render_with_imports("new jsonnet.Renderer { indent = null }.renderValue(List(1, 2))"),
        "[1, 2]"
    );
    let err = eval_fails("res = new JsonRenderer { indent = null }.renderValue(List(1))");
    assert!(
        err.contains("property 'indent'") && err.contains("Expected value of type `String`"),
        "{err}"
    );
}

#[test]
fn render_directive_keys() {
    let json = eval(
        r#"local m = new Mapping { ["key"] = "value"; [new RenderDirective { text = "🔑" }] = 42 }
res = new YamlRenderer {}.renderValue(m)
json = new JsonRenderer { indent = "" }.renderValue(m)"#,
    );
    assert_eq!(json["res"], "key: value\n🔑: 42");
    assert_eq!(json["json"], r#"{"key":"value",🔑:42}"#);
}

#[test]
fn json_parser() {
    let json = eval(
        r##"import "pkl:json"
local parser = new json.Parser {}
a = parser.parse(#"{ "name": "Pigeon", "tags": ["a", 1, 2.5, true, null], "nested": { "x": "é\n" } }"#)
b = parser.parse("123.0")
c = new json.Parser { useMapping = true }.parse(#"{ "k": 1 }"#).length
dynamicAmended = (new json.Parser {}.parse(#"{ "x": 1 }"#)) { y = 2 }
dynamicSuper = (new json.Parser {}.parse(#"{ "x": 1 }"#)) { fromSuper = super.x; fromSuperLength = super.length }
mappingAmended = (new json.Parser { useMapping = true }.parse(#"{ "x": 1 }"#)) { ["y"] = 2 }
"##,
    );
    assert_eq!(
        json["a"],
        serde_json::json!({"name": "Pigeon", "tags": ["a", 1, 2.5, true, null], "nested": {"x": "é\n"}})
    );
    assert_eq!(json["b"], serde_json::json!(123.0));
    assert_eq!(json["c"], 1);
    assert_eq!(json["dynamicAmended"], serde_json::json!({"x": 1, "y": 2}));
    assert_eq!(
        json["dynamicSuper"],
        serde_json::json!({"x": 1, "fromSuper": 1, "fromSuperLength": 1})
    );
    assert_eq!(json["mappingAmended"], serde_json::json!({"x": 1, "y": 2}));
}

#[test]
fn json_parser_converters_and_errors() {
    let json = eval(
        r##"import "pkl:json"
local parser = new json.Parser { converters { [Int] = (it) -> it + 1; [String] = (it) -> it + "x"; ["tags[*]"] = (_) -> "element" } }
a = parser.parse(#"{ "n": 1, "s": "a", "tags": [1, 2] }"#)
durationLike = new JsonRenderer {}.renderValue(new json.Parser {}.parse(#"{ "value": 1, "unit": "s" }"#))
"##,
    );
    assert_eq!(
        json["a"],
        serde_json::json!({"n": 2, "s": "ax", "tags": ["element", "element"]})
    );
    assert_eq!(
        json["durationLike"],
        "{\n  \"value\": 1,\n  \"unit\": \"s\"\n}"
    );
    for doc in ["0123", "0x1A3", "", "[1,]", "{\"a\" 1}"] {
        let err = eval_fails(&format!(
            "import \"pkl:json\"\nres = new json.Parser {{}}.parse({doc:?})"
        ));
        assert!(err.contains("Error parsing JSON document."), "{doc}: {err}");
    }
    let err = eval_fails("import \"pkl:json\"\nres = new json.Parser {}.parse(\"\\\"\\\\é\")");
    assert!(err.contains("Error parsing JSON document."), "{err}");
    for doc in [r#""\u+123""#, r#""\u12é4""#] {
        let err = eval_fails(&format!(
            "import \"pkl:json\"\nres = new json.Parser {{}}.parse({doc:?})"
        ));
        assert!(err.contains("Expected hexadecimal digit"), "{doc}: {err}");
    }
    let json = eval(
        r#"import "pkl:json"
res = new json.Parser {}.parse("\"\\uD800\\u0041\"")
"#,
    );
    assert_eq!(json["res"], "�A");
    let nested = format!("{}0{}", "[".repeat(1_001), "]".repeat(1_001));
    let err = eval_fails(&format!(
        "import \"pkl:json\"\nres = new json.Parser {{}}.parse({nested:?})"
    ));
    assert!(err.contains("Maximum nesting depth exceeded"), "{err}");
}

#[test]
fn json_parser_caught_error_is_the_message_only() {
    let json = eval(
        r##"import "pkl:json"
import "pkl:test"
local parser = new json.Parser {}
res = List("0123", "b1001", "", "   ", "!@#$%").map((s) -> test.catch(() -> parser.parse(s)))
"##,
    );
    assert_eq!(
        json["res"],
        serde_json::json!(vec!["Error parsing JSON document."; 5])
    );
}

#[test]
fn yaml_parser_documents() {
    let json = eval(
        r##"import "pkl:yaml"
import "pkl:test"
local parser = new yaml.Parser {}
anchors = parser.parse("a: &one hello\nb: *one\nc: [1, 2.5, true, ~]\n")
empty = parser.parse("")
all = parser.parseAll("---\n123\n---\n---\nname: pigeon\n...\n")
none = parser.parseAll("   ")
multiple = test.catch(() -> parser.parse("---\na: 1\n---\nb: 2\n"))
blocks = parser.parse("lit: |\n  one\n  two\nfold: >-\n  one\n  two\nstr: \"a\\tb\"\nseq:\n- x\n- w: 1\n  z: 2\n")
mappingKeys = (new yaml.Parser { useMapping = true }).parse("a: 1").length
"##,
    );
    assert_eq!(
        json["anchors"],
        serde_json::json!({"a": "hello", "b": "hello", "c": [1, 2.5, true, null]})
    );
    assert_eq!(
        json["all"],
        serde_json::json!([123, null, {"name": "pigeon"}])
    );
    assert_eq!(json["none"], serde_json::json!([]));
    assert_eq!(json["multiple"], "Error parsing YAML document.");
    assert_eq!(
        json["blocks"],
        serde_json::json!({"lit": "one\ntwo\n", "fold": "one two", "str": "a\tb", "seq": ["x", {"w": 1, "z": 2}]})
    );
    assert_eq!(json["mappingKeys"], 1);
    assert!(json["empty"].is_null());
}

#[test]
fn yaml_parser_modes() {
    let json = eval(
        r##"import "pkl:yaml"
local doc = "[yes, 0o17, 017, 1_0, 0b1, 0x1f]"
compat = new yaml.Parser {}.parse(doc)
v11 = new yaml.Parser { mode = "1.1" }.parse(doc)
v12 = new yaml.Parser { mode = "1.2" }.parse(doc)
"##,
    );
    assert_eq!(json["compat"], serde_json::json!([true, 15, 15, 10, 1, 31]));
    assert_eq!(
        json["v11"],
        serde_json::json!([true, "0o17", 15, 10, 1, 31])
    );
    assert_eq!(
        json["v12"],
        serde_json::json!(["yes", 15, 17, "1_0", "0b1", 31])
    );
}

#[test]
fn yaml_parser_converters() {
    let json = eval(
        r##"import "pkl:yaml"
local parser = new yaml.Parser {
  converters {
    [String] = (it) -> it + "x"
    [Int] = (it) -> it + 1
    ["list[*]"] = (_) -> 0
    ["a.b"] = (it) -> it * 10
  }
}
res = parser.parse("a: {b: 2}\nanchor: &p 1\nalias: *p\nlist: [5, 6]\n")
"##,
    );
    assert_eq!(
        json["res"],
        serde_json::json!({"ax": {"bx": 20}, "anchorx": 2, "aliasx": 2, "listx": [0, 0]})
    );
}

#[test]
fn yaml_parser_rejects_malformed_documents() {
    for doc in [
        "a: [1",
        "a: 'x",
        "a: *missing",
        "a: 1\na: 2",
        "a: b: c",
        "- 1\nx: 2",
    ] {
        let err = eval_fails(&format!(
            "import \"pkl:yaml\"\nres = new yaml.Parser {{}}.parse({doc:?})"
        ));
        assert!(err.contains("Error parsing YAML document."), "{doc}: {err}");
    }
}

#[test]
fn yaml_parser_flow_edge_cases() {
    let json = eval(
        r##"import "pkl:yaml"
import "pkl:test"
local parser = new yaml.Parser {}
min = parser.parse("-9223372036854775808")
multiline = parser.parse("[multi\n  line, {k: a\n  b}]")
missing = test.catch(() -> parser.parse("[a, , b]"))
userError = test.catch(() -> throw("Error parsing mine.\n\ndetail"))
"##,
    );
    assert_eq!(json["min"], i64::MIN);
    assert_eq!(
        json["multiline"],
        serde_json::json!(["multi line", {"k": "a b"}])
    );
    assert_eq!(json["missing"], "Error parsing YAML document.");
    assert_eq!(json["userError"], "Error parsing mine.\n\ndetail");
}

#[test]
fn yaml_parser_explicit_keys_and_tag_handles() {
    let json = eval(
        r##"import "pkl:yaml"
local parser = new yaml.Parser {
  converters {
    [String] = (it) -> it + "x"
    ["a"] = (it) -> it * 10
  }
}
explicit = parser.parse("? a\n: 2")
implicit = parser.parse("a: 2")
handle = new yaml.Parser {}.parse("%TAG !e! tag:yaml.org,2002:\n--- !e!str 123")
primary = new yaml.Parser {}.parse("%TAG ! tag:yaml.org,2002:\n--- !str 123")
"##,
    );
    assert_eq!(json["explicit"], json["implicit"]);
    assert_eq!(json["handle"], "123");
    assert_eq!(json["primary"], "123");
}

#[test]
fn yaml_parser_limits_collection_aliases() {
    let json = eval(
        r##"import "pkl:yaml"
import "pkl:test"
local text = "a: &x [1]\nb: *x\nc: *x\nd: *x"
scalars = new yaml.Parser { maxCollectionAliases = 0 }.parse("a: &x 1\nb: *x\nc: *x")
within = new yaml.Parser { maxCollectionAliases = 3 }.parse(text)
over = test.catch(() -> new yaml.Parser { maxCollectionAliases = 2 }.parse(text))
"##,
    );
    assert_eq!(json["scalars"], serde_json::json!({"a": 1, "b": 1, "c": 1}));
    assert_eq!(json["within"]["d"], serde_json::json!([1]));
    assert_eq!(json["over"], "Error parsing YAML document.");
}

#[test]
fn yaml_parser_anchored_keys_keep_the_mapping_column() {
    let json = eval(
        r##"import "pkl:yaml"
anchored = new yaml.Parser {}.parse("&x a: 1\nb: 2")
explicit = new yaml.Parser {}.parse("&x a: 1\n? c\n: 3")
aliased = new yaml.Parser {}.parse("&x a: 1\nb: *x")
"##,
    );
    assert_eq!(json["anchored"], serde_json::json!({"a": 1, "b": 2}));
    assert_eq!(json["explicit"], serde_json::json!({"a": 1, "c": 3}));
    assert_eq!(json["aliased"], serde_json::json!({"a": 1, "b": "a"}));
}

// ============================================================
// Values a renderer cannot render: errors match `pkl eval` 0.32.2
// ============================================================

#[test]
fn renderers_reject_pair_class_type_alias_and_int_seq() {
    for (class, name) in [
        ("JsonRenderer", "JSON"),
        ("YamlRenderer", "YAML"),
        ("PListRenderer", "XML property list"),
    ] {
        let json = eval(&format!(
            r#"
import "pkl:test"
class Person {{ name: String }}
typealias Email = String
local r = new {class} {{}}
pair = test.catch(() -> r.renderValue(Pair(1, 2)))
cls = test.catch(() -> r.renderValue(Person))
alias = test.catch(() -> r.renderValue(Email))
seq = test.catch(() -> r.renderValue(IntSeq(1, 4)))
"#
        ));
        let msg = |ty: &str, value: &str| {
            format!("Cannot render value of type `{ty}` as {name}. Value: {value}")
        };
        assert_eq!(json["pair"], msg("Pair", "Pair(1, 2)"), "{class}");
        assert!(
            json["cls"].as_str().unwrap().starts_with(&msg("Class", "")),
            "{class}"
        );
        assert!(
            json["cls"].as_str().unwrap().ends_with("#Person"),
            "{class}"
        );
        assert!(
            json["alias"]
                .as_str()
                .unwrap()
                .starts_with(&msg("TypeAlias", "")),
            "{class}"
        );
        assert!(
            json["alias"].as_str().unwrap().ends_with("#Email"),
            "{class}"
        );
        assert_eq!(json["seq"], msg("IntSeq", "IntSeq(1, 4)"), "{class}");
    }
}

#[test]
fn pair_is_a_value_with_first_and_second() {
    let json = eval(
        r#"
p = Pair(1, "x")
first = p.first
second = p.second
"#,
    );
    assert_eq!(json["first"], 1);
    assert_eq!(json["second"], "x");
}

#[test]
fn renderer_document_errors_name_the_value_class() {
    let json = eval(
        r#"
import "pkl:test"
list = test.catch(() -> new PcfRenderer {}.renderDocument(List("a", "b")))
set = test.catch(() -> new PropertiesRenderer {}.renderDocument(Set("a")))
map = test.catch(() -> new PcfRenderer {}.renderDocument(Map("a", 1)))
mixed = test.catch(() -> new YamlRenderer {}.renderDocument(new Dynamic { foo = "foo"; 5 }))
"#,
    );
    assert_eq!(
        json["list"],
        "The top-level value of a Pcf document must have type `Typed` or `Dynamic`, but got type `List`. Value: List(\"a\", \"b\")"
    );
    assert_eq!(
        json["set"],
        "The top-level value of a Java properties file must have type `Typed`, `Dynamic`, `Mapping`, or `Map`, but got type `Set`. Value: Set(\"a\")"
    );
    assert_eq!(
        json["map"],
        "The top-level value of a Pcf document must have type `Typed` or `Dynamic`, but got type `Map`. Value: Map(\"a\", 1)"
    );
    assert_eq!(
        json["mixed"],
        "Cannot render object with both elements and properties/entries as YAML. Object: new Dynamic { foo = \"foo\"; 5 }"
    );
}

#[test]
fn user_classes_named_pair_or_type_alias_are_ordinary_objects() {
    let json = eval(
        r#"
class Pair { first = 1; second = 2 }
class TypeAlias { name = "x" }
pairOut = new JsonRenderer {}.renderValue(new Pair {})
aliasOut = new JsonRenderer {}.renderValue(new TypeAlias {})
"#,
    );
    assert!(json["pairOut"].as_str().unwrap().contains("\"first\": 1"));
    assert!(
        json["aliasOut"]
            .as_str()
            .unwrap()
            .contains("\"name\": \"x\"")
    );
}

#[test]
fn thrown_text_with_value_lines_is_not_rewritten_by_catch() {
    let json = eval(
        r#"
import "pkl:test"
msg = test.catch(() -> throw("bad\nValue: x"))
"#,
    );
    assert_eq!(json["msg"], "bad\nValue: x");
}

#[test]
fn renderer_errors_show_stdlib_classes_and_empty_maps() {
    let json = eval(
        r#"
import "pkl:test"
cls = test.catch(() -> new JsonRenderer {}.renderValue(Int))
map = test.catch(() -> new PcfRenderer {}.renderDocument(Map()))
mapping = test.catch(() -> new PcfRenderer {}.renderDocument(new Mapping {}))
"#,
    );
    assert_eq!(
        json["cls"],
        "Cannot render value of type `Class` as JSON. Value: Int"
    );
    assert!(json["map"].as_str().unwrap().ends_with("Value: Map()"));
    assert!(
        json["mapping"]
            .as_str()
            .unwrap()
            .ends_with("Value: new Mapping {}")
    );
}

#[test]
fn type_aliases_stay_distinct_and_local_class_bindings_win() {
    let json = eval(
        r#"
typealias A = String
typealias B = Int
class Person {}
same = A == B
count = Set(A, B).length
localName = let (A = Person) A.simpleName
"#,
    );
    assert_eq!(json["same"], false);
    assert_eq!(json["count"], 2);
    assert_eq!(json["localName"], "Person");
}

#[test]
fn type_alias_text_hides_its_identity() {
    let json = eval(
        r#"
typealias Email = String
text = Email.toString()
"#,
    );
    assert_eq!(json["text"], "test#Email");
}

#[test]
fn type_alias_text_keeps_quoted_names_and_property_equals() {
    let json = eval(
        r#"
typealias `contact.email` = String
typealias Email = String
quoted = `contact.email`.toString()
obj = (new Dynamic { alias = Email }).toString()
"#,
    );
    assert_eq!(json["quoted"], "test#contact.email");
    assert_eq!(json["obj"], "new Dynamic { alias = test#Email }");
}

#[test]
fn renderer_errors_do_not_expand_shared_values_exponentially() {
    let json = eval(
        r#"
import "pkl:test"
local w0 = List(1)
local w1 = List(w0, w0)
local w2 = List(w1, w1)
local w3 = List(w2, w2)
local w4 = List(w3, w3)
local w5 = List(w4, w4)
local w6 = List(w5, w5)
local w7 = List(w6, w6)
local w8 = List(w7, w7)
local w9 = List(w8, w8)
local w10 = List(w9, w9)
msg = test.catch(() -> new PcfRenderer {}.renderDocument(w10))
"#,
    );
    assert!(json["msg"].as_str().unwrap().len() < 10_000);
}

#[test]
fn type_alias_values_survive_builtin_bindings_and_lambdas() {
    let json = eval(
        r#"
import "pkl:test"
typealias Email = String
typealias `contact.email` = String
typealias Dyn = Dynamic
local r = new JsonRenderer {}
dyn = test.catch(() -> r.renderValue(Dyn))
"#,
    );
    let dyn_ = json["dyn"].as_str().unwrap();
    assert!(dyn_.contains("`TypeAlias`"), "{dyn_}");
}

#[test]
fn quoted_type_alias_names_show_in_backticks() {
    let json = eval(
        r#"
import "pkl:test"
typealias `contact.email` = String
local r = new JsonRenderer {}
quoted = test.catch(() -> r.renderValue(`contact.email`))
"#,
    );
    let quoted = json["quoted"].as_str().unwrap();
    assert!(quoted.contains("`TypeAlias`"), "{quoted}");
    assert!(quoted.ends_with("#`contact.email`"), "{quoted}");
}

#[test]
fn captured_locals_named_like_an_alias_keep_their_value() {
    let json = eval(
        r#"
typealias A = String
res = let (A = 42) (() -> A + 1).apply()
"#,
    );
    assert_eq!(json["res"], 43);
}

#[test]
fn maps_with_class_keys_display_as_maps() {
    let json = eval(
        r#"
import "pkl:test"
msg = test.catch(() -> new PcfRenderer {}.renderDocument(Map(String, 1)))
"#,
    );
    assert!(
        json["msg"]
            .as_str()
            .unwrap()
            .ends_with("Value: Map(String, 1)"),
        "{}",
        json["msg"]
    );
}

#[test]
fn object_keys_that_mention_class_are_not_class_keys() {
    let json = eval(
        r#"
import "pkl:test"
msg = test.catch(() -> new PcfRenderer {}.renderDocument(Map(new Dynamic { x = "class:" }, 1)))
"#,
    );
    let msg = json["msg"].as_str().unwrap();
    assert!(!msg.contains("Map(Dynamic,"), "{msg}");
    assert!(msg.contains("class:"), "{msg}");
}

#[test]
fn json_render_errors_name_pair_and_type_alias() {
    let json = eval(
        r#"
import "pkl:test"
pairMsg = test.catch(() -> new JsonRenderer {}.renderDocument(Pair(1, 2)))
"#,
    );
    let msg = json["pairMsg"].as_str().unwrap();
    assert!(!msg.contains("`Dynamic`"), "{msg}");
}

// Expected text is what `pkl eval` 0.32.2 renders.

#[test]
fn pcf_renders_collections_as_calls() {
    assert_eq!(
        render(r#"new PcfRenderer {}.renderValue(List(1, "a", new Dynamic { x = 1 }))"#),
        "List(1, \"a\", new {\n  x = 1\n})"
    );
    assert_eq!(
        render(r#"new PcfRenderer {}.renderValue(Map("k", Pair(1, "x"), 2, Set(3)))"#),
        "Map(\"k\", Pair(1, \"x\"), 2, Set(3))"
    );
    assert_eq!(
        render(
            r#"new PcfRenderer {}.renderDocument(new Dynamic { l = List(1, 2); m = Map("a", 1); s = Set(1); p = Pair(1, 2); n = new Listing { 1 }; ["k"] = List(1); q = List(List(1), Map("a", Set(2))) })"#
        ),
        "l = List(1, 2)\nm = Map(\"a\", 1)\ns = Set(1)\np = Pair(1, 2)\nn {\n  1\n}\nq = List(List(1), Map(\"a\", Set(2)))\n[\"k\"] = List(1)\n"
    );
}

#[test]
fn pcf_renders_int_seq_with_its_bounds() {
    assert_eq!(
        render("new PcfRenderer {}.renderValue(IntSeq(1, 4))"),
        "IntSeq(1, 4)"
    );
    assert_eq!(
        render("new PcfRenderer {}.renderValue(IntSeq(1, 6).step(2))"),
        "IntSeq(1, 6).step(2)"
    );
    assert_eq!(
        render("new PcfRenderer {}.renderValue(IntSeq(1, 4).step(-2))"),
        "IntSeq(1, 4).step(-2)"
    );
}

#[test]
fn pcf_renders_a_regex_and_a_dynamic_with_only_elements() {
    assert_eq!(
        render(r#"new PcfRenderer {}.renderValue(Regex("a.*b"))"#),
        "Regex(\"a.*b\")"
    );
    assert_eq!(
        render(r#"new PcfRenderer {}.renderDocument(new Dynamic { new { apple = "yes" } })"#),
        "\nnew {\n  apple = \"yes\"\n}\n"
    );
}

#[test]
fn properties_renderer_names_unrenderable_values_as_pkl_does() {
    let json = eval(
        r#"
import "pkl:test"
res1 = test.catch(() -> new PropertiesRenderer {}.renderValue(List("a", "b")))
res2 = test.catch(() -> new PropertiesRenderer {}.renderValue(Set(1)))
res3 = test.catch(() -> new PropertiesRenderer {}.renderValue(new Dynamic { name = "pigeon"; age = 42 }))
"#,
    );
    assert_eq!(
        json["res1"],
        "Cannot render value of type `List` as Properties. Value: List(\"a\", \"b\")"
    );
    assert_eq!(
        json["res2"],
        "Cannot render value of type `Set` as Properties. Value: Set(1)"
    );
    assert_eq!(
        json["res3"],
        "Cannot render value of type `Dynamic` as Properties. Value: new Dynamic { name = ?; age = ? }"
    );
}

#[test]
fn pcf_applies_converters_inside_collection_calls() {
    assert_eq!(
        render(
            r#"new PcfRenderer { converters { [Int] = (i) -> i + 1 } }.renderValue(List(1, 2))"#
        ),
        "List(2, 3)"
    );
    assert_eq!(
        render(
            r#"new PcfRenderer { converters { [Int] = (i) -> i + 1 } }.renderValue(Map("a", 1))"#
        ),
        "Map(\"a\", 2)"
    );
}

#[test]
fn pcf_collection_members_keep_their_path_for_nested_converters() {
    assert_eq!(
        render(
            r#"new PcfRenderer { converters { ["[*].a"] = (i) -> i + 10 } }.renderValue(List(new Dynamic { a = 1 }))"#
        ),
        "List(new {\n  a = 11\n})"
    );
}

#[test]
fn text_output_with_an_unrelated_path_converter_keeps_a_failed_member_failing() {
    let (_dir, path) = module_file(
        "pklr_render_text_path_converter_failed_member",
        "class P { age: Int }\nitem = new P {}\nother = 1\noutput {\n  renderer = new YamlRenderer { converters { [\"other\"] = (it) -> it } }\n}\n",
    );
    let err = pklr::eval_to_text(&path).unwrap_err().to_string();
    assert!(err.contains("`age`"), "{err}");
}

#[test]
fn json_renderer_int_seq_converter_reads_start_end_step() {
    let json = eval(
        r#"
local seq = IntSeq(1, 7).step(3)
res = List(seq.start, seq.end, seq.step)
"#,
    );
    assert_eq!(json["res"], serde_json::json!([1, 7, 3]));
}

#[test]
fn error_messages_quote_listings_and_maps() {
    let json = eval(
        r#"
import "pkl:test"
res1 = test.catch(() -> new PropertiesRenderer {}.renderValue(new Listing { "a"; "b" }))
res2 = test.catch(() -> new Listing { ...Map("a", 1) })
"#,
    );
    assert_eq!(
        json["res1"],
        "Cannot render value of type `Listing` as Properties. Value: new Listing { \"a\"; \"b\" }"
    );
    assert_eq!(
        json["res2"],
        "Cannot spread value of type `Map` into object of type `Listing`. Value: Map(\"a\", 1)"
    );
}

#[test]
fn amending_a_to_mapping_entry_keeps_its_listing_elements() {
    let json = eval(
        r#"
class Foo { names: Listing<String> }
local foos = Map("bar", new Foo { names { "orig" } }).toMapping()
res = (foos) { ["bar"] { names { "new" } } }
"#,
    );
    assert_eq!(
        json["res"]["bar"]["names"],
        serde_json::json!(["orig", "new"])
    );
}

#[test]
fn amending_a_to_mapping_entry_by_computed_key_and_again() {
    let json = eval(
        r#"
class Foo { names: Listing<String> }
local k = "bar"
local foos = Map("bar", new Foo { names { "orig" } }, "baz", new Foo { names { "z" } }).toMapping()
local once = (foos) { [k] { names { "new" } } }
res = (once) { ["baz"] { names { "again" } } }
"#,
    );
    assert_eq!(
        json["res"]["bar"]["names"],
        serde_json::json!(["orig", "new"])
    );
    assert_eq!(
        json["res"]["baz"]["names"],
        serde_json::json!(["z", "again"])
    );
}

#[test]
fn computed_key_amendment_adds_properties_to_a_dynamic_entry() {
    let json = eval(
        r#"
local k = "a"
local m = Map("a", new Dynamic { 1 }).toMapping()
res = (m) { [k] { foo = 2 } }
"#,
    );
    assert_eq!(json["res"]["a"]["foo"], 2);
}

#[test]
fn mapping_amendment_computed_keys_see_the_bodys_bindings() {
    let json = eval(
        r#"
class Foo { names: Listing<String> }
local m = Map("a", new Foo { names { "orig" } }).toMapping()
res = (m) { local k = "a"; [k] { names { "new" } } }
res2 = (m) { when (false) { [throw("unused")] {} } }
"#,
    );
    assert_eq!(
        json["res"]["a"]["names"],
        serde_json::json!(["orig", "new"])
    );
    assert_eq!(json["res2"]["a"]["names"], serde_json::json!(["orig"]));
}

#[test]
fn computed_key_amends_a_dynamic_entry_in_a_mixed_mapping() {
    let json = eval(
        r#"
class Foo { names: Listing<String> }
local k = "a"
local m = Map("a", new Dynamic { 1 }, "b", new Foo { names { "x" } }).toMapping()
res = (m) { [k] { foo = 2 } }["a"].foo
res2 = (m) { [k] { foo = 2 } }["b"].names
"#,
    );
    assert_eq!(json["res"], 2);
    assert_eq!(json["res2"], serde_json::json!(["x"]));
}

#[test]
fn pcf_renders_object_list_and_regex_mapping_keys_as_values() {
    let text = render(
        r#"new PcfRenderer {}.renderValue(new Mapping { [Pair(1, "a")] = 1; [Regex("b+")] = 2; [List(1, 2)] = 3 })"#,
    );
    assert_eq!(
        text,
        "{\n  [Pair(1, \"a\")] = 1\n  [Regex(\"b+\")] = 2\n  [List(1, 2)] = 3\n}"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn convert_property_annotations_apply_to_module_and_class_properties() {
    let (_dir, path) = module_file(
        "pklr_render_convert_property",
        r#"import "pkl:json"

class Comment extends ConvertProperty {
  text: String
  render = (prop, renderer) ->
    Pair(prop.key, new RenderDirective {
      text = (renderer as ValueRenderer).renderValue(prop.value) + " // " + outer.text
    })
}

@json.Property { name = "FOO" }
foo: String = "a"

@Comment { text = "note" }
bar: Int = 1

nested: Nested

class Nested {
  @json.Property { name = "RENAMED" }
  a: Int = 2
  b: Int = 3
}

output { renderer = new PcfRenderer {} }
"#,
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "foo = \"a\"\nbar 1 // note\nnested {\n  a = 2\n  b = 3\n}\n"
    );
    let (_dir, path) = module_file(
        "pklr_render_convert_property_json",
        "import \"pkl:json\"\n@json.Property { name = \"FOO\" }\nfoo: String = \"a\"\noutput { renderer = new JsonRenderer {} }\n",
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"FOO\": \"a\"\n}\n"
    );
}

#[test]
fn child_override_keeps_the_parent_property_position() {
    assert_eq!(
        render(
            r#"new PcfRenderer {}.renderValue(new Bar {})
open class Foo { a = 1; b = 2; c = 3 }
class Bar extends Foo { b = 20 }"#
        ),
        "{\n  a = 1\n  b = 20\n  c = 3\n}"
    );
}

#[test]
fn renderer_is_checks_distinguish_renderer_classes() {
    assert_eq!(
        render(r#""\(new PcfRenderer {} is JsonRenderer) \(new PcfRenderer {} is PcfRenderer)""#),
        "false true"
    );
}

#[test]
fn pcf_converters_skip_pair_members_and_tell_map_from_mapping() {
    assert_eq!(
        render(
            r#"new PcfRenderer { converters { [Int] = (it) -> it + 1; [Pair] = (it) -> Pair(it.second, it.first) } }.renderValue(List(Pair(1, 2), 5))"#
        ),
        "List(Pair(2, 1), 6)"
    );
    assert_eq!(
        render(
            r#"new PcfRenderer { converters { [Mapping] = (it) -> (it) { ["m"] = 0 } } }.renderValue(Map("a", 1))"#
        ),
        "Map(\"a\", 1)"
    );
}

#[test]
fn untyped_new_for_a_mixin_typed_local_is_a_mixin() {
    let json = eval(
        r#"local m: Mixin<Dynamic> = new { a = 1 }
x = (new Dynamic { b = 0 } |> m).a
y = new Mapping {
  local n: Mixin<ValueRenderer> = new { converters { [String] = (s) -> "y" } }
  ["k"] = (new PcfRenderer {} |> n).converters.length
}["k"]"#,
    );
    assert_eq!(json["x"], 1);
    assert_eq!(json["y"], 1);
}

#[cfg(feature = "native-io")]
#[test]
fn amending_a_module_does_not_repeat_the_elements_of_an_inherited_member() {
    let dir = TestTempDir::new("pklr_render_amend_elements");
    std::fs::write(dir.path().join("base.pkl"), "o { \"p\"; \"q\" }\n").unwrap();
    let path = dir.path().join("main.pkl");
    std::fs::write(
        &path,
        "amends \"base.pkl\"\noutput { renderer = new JsonRenderer {} }\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"o\": [\n    \"p\",\n    \"q\"\n  ]\n}\n"
    );
}

#[cfg(feature = "native-io")]
fn annotated_modules(name: &str, files: &[(&str, &str)]) -> (TestTempDir, std::path::PathBuf) {
    let dir = TestTempDir::new(name);
    for (file, src) in files {
        std::fs::write(dir.path().join(file), src).unwrap();
    }
    let path = dir.path().join("main.pkl");
    (dir, path)
}

#[cfg(feature = "native-io")]
const NAMES: (&str, &str) = ("names.pkl", "label = \"renamed\"\n");

#[cfg(feature = "native-io")]
#[test]
fn import_used_only_in_a_property_annotation_is_bound() {
    let (_dir, path) = annotated_modules(
        "pklr_render_annotation_import",
        &[
            NAMES,
            (
                "main.pkl",
                "import \"pkl:json\"\nimport \"names.pkl\"\n@json.Property { name = names.label }\nx = 1\noutput { renderer = new JsonRenderer {} }\n",
            ),
        ],
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"renamed\": 1\n}\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn module_property_annotations_are_inherited_through_amends() {
    let (_dir, path) = annotated_modules(
        "pklr_render_annotation_amends",
        &[
            NAMES,
            (
                "base.pkl",
                "import \"pkl:json\"\nimport \"names.pkl\"\n@json.Property { name = names.label }\nx = 1\n",
            ),
            (
                "main.pkl",
                "amends \"base.pkl\"\nx = 2\noutput { renderer = new JsonRenderer {} }\n",
            ),
        ],
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"renamed\": 2\n}\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn inherited_class_annotation_keeps_its_declaring_scope() {
    // `names` is imported by the declaring module only.
    let (_dir, path) = annotated_modules(
        "pklr_render_annotation_class_scope",
        &[
            NAMES,
            (
                "base.pkl",
                "import \"pkl:json\"\nimport \"names.pkl\"\nopen class P { @json.Property { name = names.label } a: Int = 1 }\n",
            ),
            (
                "main.pkl",
                "import \"base.pkl\"\nclass Q extends base.P { a = 5 }\noutput { renderer = new JsonRenderer {}; value = new Q {} }\n",
            ),
        ],
    );
    assert_eq!(
        pklr::eval_to_text(&path).unwrap(),
        "{\n  \"renamed\": 5\n}\n"
    );
}

#[cfg(feature = "native-io")]
#[test]
fn json_conversion_applies_property_annotations() {
    let (_dir, path) = annotated_modules(
        "pklr_render_annotation_json",
        &[
            NAMES,
            (
                "main.pkl",
                "import \"pkl:json\"\nimport \"names.pkl\"\nclass C { @json.Property { name = names.label } a: Int = 1; b: Int = 2 }\n@json.Property { name = \"zed\" }\nx = 3\nc = new C {}\n",
            ),
        ],
    );
    let expected = serde_json::json!({"zed": 3, "c": {"renamed": 1, "b": 2}});
    assert_eq!(pklr::eval_to_json(&path).unwrap(), expected);
}

#[test]
fn convert_property_transformer_matches_a_subclass() {
    let json = eval(
        r#"class Up extends ConvertProperty {
  render = (prop, renderer) -> Pair(prop.first.toUpperCase(), prop.second)
}
class C { @Up a: Int = 1 }
res = new JsonRenderer {
  convertPropertyTransformers { [ConvertProperty] = new { render = (prop, r) -> Pair("T" + prop.first, prop.second) } }
}.renderValue(new C {})"#,
    );
    assert_eq!(json["res"], "{\n  \"Ta\": 1\n}");
}

#[test]
fn annotation_that_is_not_a_convert_property_is_not_rendered() {
    let json = eval(
        r#"class Note extends Annotation { render = (p, r) -> Pair("zzz", 9) }
class C { @Note a: Int = 1 }
res = new JsonRenderer {}.renderValue(new C {})"#,
    );
    assert_eq!(json["res"], "{\n  \"a\": 1\n}");
}

#[test]
fn stdlib_annotations_on_a_class_property_are_not_evaluated_for_rendering() {
    let json = eval(
        r#"class C {
  @Since { version = throw("boom") }
  @DocExample { subjects { throw("boom") } }
  a: Int = 1
}
res = new JsonRenderer {}.renderValue(new C {})"#,
    );
    assert_eq!(json["res"], "{\n  \"a\": 1\n}");
}

#[test]
fn stdlib_annotations_on_an_overridden_property_are_not_evaluated() {
    let json = eval(
        r#"open class Parent {
  @Since { version = throw("boom") }
  a: Int = 1
}
class Child extends Parent {
  a: Int = 2
}
res = new JsonRenderer {}.renderValue(new Child {})"#,
    );
    assert_eq!(json["res"], "{\n  \"a\": 2\n}");
}

#[test]
fn json_compound_key_converted_to_a_string_renders() {
    let json = eval(
        r#"res = new JsonRenderer { converters { [List] = (key) -> "items" } }
  .renderValue(new Mapping { [List(1)] = 2 })"#,
    );
    assert_eq!(json["res"], "{\n  \"items\": 2\n}");
    let (_dir, path) = module_file(
        "pklr_render_json_converted_compound_key",
        "output {\n  renderer = new JsonRenderer { converters { [List] = (key) -> \"items\" } }\n  value = new Mapping { [List(1)] = 2 }\n}\n",
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"items": 2})
    );
}

#[test]
fn text_annotation_without_an_output_renderer_sees_a_pcf_renderer() {
    let (_dir, path) = module_file(
        "pklr_render_text_default_renderer_annotation",
        "class Tag extends ConvertProperty {\n  render = (prop, r) -> Pair(\"\\(r.getClass().simpleName)_\\(prop.first)\", prop.second)\n}\nclass C {\n  @Tag {}\n  a: Int = 1\n}\nres = new C {}\n",
    );
    let text = pklr::eval_to_text(&path).unwrap();
    assert_eq!(text, "res {\n  PcfRenderer_a = 1\n}\n");
}

#[test]
fn amended_instance_keeps_the_annotations_of_its_class() {
    let json = eval(
        r#"import "pkl:json"
class C {
  @json.Property { name = "renamed" }
  a: Int = 1
  b: Int = 2
}
base = new C {}
res = new JsonRenderer {}.renderValue(new C { a = 5 })
res2 = new JsonRenderer {}.renderValue((base) { a = 7 })"#,
    );
    assert_eq!(json["res"], "{\n  \"renamed\": 5,\n  \"b\": 2\n}");
    assert_eq!(json["res2"], "{\n  \"renamed\": 7,\n  \"b\": 2\n}");
}

#[cfg(feature = "native-io")]
#[test]
fn inherited_listing_amendment_follows_indirect_reads_of_overridden_members() {
    let (_dir, path) = annotated_modules(
        "pklr_inherited_listing_indirect",
        &[
            ("base.pkl", "a = 1\nb = a\no { b }\n"),
            ("main.pkl", "amends \"base.pkl\"\na = 2\n"),
        ],
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"a": 2, "b": 2, "o": [2]})
    );
}

#[test]
fn xml_and_jsonnet_property_rename_only_their_own_renderer() {
    let json = eval(
        r#"import "pkl:xml"
import "pkl:jsonnet"
class X { @xml.Property { name = "xn" } a: Int = 1 }
class J { @jsonnet.Property { name = "jn" } a: Int = 1 }
r1 = new JsonRenderer {}.renderValue(new X {})
r2 = new PcfRenderer {}.renderValue(new J {})
r3 = new xml.Renderer {}.renderValue(new J {})
r4 = new jsonnet.Renderer {}.renderValue(new X {})"#,
    );
    assert_eq!(json["r1"], "{\n  \"a\": 1\n}");
    assert_eq!(json["r2"], "{\n  a = 1\n}");
    assert_eq!(json["r3"], "<a>1</a>");
    assert_eq!(json["r4"], "{\n  a: 1,\n}");
}

// Each amending module's body-only declaration of a member adds to the
// elements the earlier ones made; a later one must not drop them when a
// member it reads is overridden further down the chain.
#[cfg(feature = "native-io")]
#[test]
fn body_only_declarations_accumulate_across_the_amends_chain() {
    let dir = TestTempDir::new("pklr_body_only_chain");
    std::fs::write(dir.path().join("base.pkl"), "a = 1\no { \"base\" }\n").unwrap();
    std::fs::write(
        dir.path().join("mid.pkl"),
        "amends \"base.pkl\"\no { module.a }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("main.pkl"), "amends \"mid.pkl\"\na = 2\n").unwrap();
    assert_eq!(
        pklr::eval_to_text(&dir.path().join("main.pkl")).unwrap(),
        "a = 2\no {\n  \"base\"\n  2\n}\n"
    );
}

// Classes of the same name in different modules, such as `json.Property` and
// `yaml.Property`, are different classes: a converter keyed by one does not
// apply to the other's instances, while one keyed by a parent still reaches
// its subclasses.
#[cfg(feature = "native-io")]
#[test]
fn class_converters_tell_same_named_classes_apart() {
    let dir = TestTempDir::new("pklr_converter_same_name");
    std::fs::write(dir.path().join("ma.pkl"), "open class Foo { x: Int = 1 }\n").unwrap();
    std::fs::write(dir.path().join("mb.pkl"), "class Foo { x: Int = 2 }\n").unwrap();
    std::fs::write(
        dir.path().join("main.pkl"),
        "import \"ma.pkl\"\nimport \"mb.pkl\"\nclass Sub extends ma.Foo {}\n\
         a = new ma.Foo {}\nb = new mb.Foo {}\ns = new Sub {}\n\
         output { renderer = new JsonRenderer { converters { [ma.Foo] = (_) -> \"A\" } } }\n",
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&dir.path().join("main.pkl")).unwrap(),
        serde_json::json!({"a": "A", "b": {"x": 2}, "s": "A"})
    );
}
