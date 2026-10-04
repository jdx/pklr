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
fn eval_to_json_omits_nulls_only_when_renderer_asks() {
    let (_dir, path) = module_file("pklr_render_json_nulls", "a = null\nb = 1\n");
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"a": null, "b": 1})
    );
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
fn output_renderer_defaults_keep_nulls_when_only_converters_are_set() {
    let (_dir, path) = module_file(
        "pklr_render_converter_keeps_nulls",
        "a = null\nb = 1\noutput { renderer = new JsonRenderer { converters { [Int] = (n) -> n + 1 } } }\n",
    );
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"a": null, "b": 2})
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
fn output_body_sees_default_renderer_and_value() {
    let (_dir, path) = module_file(
        "pklr_render_output_defaults",
        "x = 1\noutput {\n  value = new Dynamic { a = 1 }\n  text = renderer.renderDocument(value)\n}\n",
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
fn render_directive_keys() {
    let json = eval(
        r#"local m = new Mapping { ["key"] = "value"; [new RenderDirective { text = "🔑" }] = 42 }
res = new YamlRenderer {}.renderValue(m)
json = new JsonRenderer { indent = "" }.renderValue(m)"#,
    );
    assert_eq!(json["res"], "key: value\n🔑: 42");
    assert_eq!(json["json"], r#"{"key":"value",🔑:42}"#);
}
