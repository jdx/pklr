use super::*;

// Every expectation below was compared with `pkl eval` 0.32.2.

#[test]
fn is_and_as_reject_a_type_that_does_not_exist() {
    for src in [
        "r = 1 is Foo",
        "r = 1 as Foo",
        "r = null is Foo?",
        "r = (1) as Foo?",
        "r = 1 is Int|Foo",
        "r = \"a\" is String|Foo",
        "r = 1 is List<Foo>",
        "r = 1 is Listing<Foo>",
        "r = 1 is Mapping<String, Foo>",
        "r = 1 is (Int) -> Foo",
        "r = 1 is Foo(true)",
        // The type is resolved before the value is evaluated.
        "r = throw(\"left\") is Foo",
        "r = (if (false) 1 else 2) is Foo",
        "r = 1 is Foo.Bar",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("Cannot find"), "{src}: {error}");
    }
    assert!(eval_fails("r = 1 is Foo").contains("Cannot find type `Foo`."));
}

#[test]
fn is_resolves_the_types_pkl_knows() {
    for (expr, expected) in [
        ("1 is Int", true),
        ("1 is Number", true),
        ("1 is Any", true),
        ("1 is unknown", true),
        ("1 is nothing", false),
        ("1 is module", false),
        ("1 is Collection", false),
        ("1 is Pair", false),
        ("1 is IntSeq", false),
        ("1 is Function0", false),
        ("1 is Resource", false),
        ("1 is Module", false),
        ("1 is Typed", false),
        ("1 is Function", false),
        ("1 is Int|String", true),
        ("1 is Int(this > 0)", true),
    ] {
        assert_eq!(eval(&format!("r = {expr}\n"))["r"], expected, "{expr}");
    }
    // These names resolve. pklr does not model the numeric, `Comparable`,
    // `Char` and `Uri` aliases, so only that no error is raised is asserted.
    for name in [
        "Int8",
        "Int16",
        "Int32",
        "UInt",
        "UInt8",
        "UInt16",
        "UInt32",
        "Comparable",
        "NonNull",
        "Char",
        "Uri",
        "Charset",
        "DurationUnit",
        "DataSizeUnit",
        "Mixin",
        "Bytes",
    ] {
        assert!(
            eval(&format!("r = 1 is {name}\n"))["r"].is_boolean(),
            "{name}"
        );
    }
}

#[test]
fn is_still_resolves_declared_types() {
    let json = eval(
        r#"
open class C {}
class D extends C {}
typealias T = Int
a = new C {} is C
b = 1 is C
c = new D {} is C
d = 1 is T
e = new C {} is C?
f = null is C?
g = new C {} is D|C
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({
            "a": true, "b": false, "c": true, "d": true, "e": true, "f": true, "g": true
        })
    );
}

#[test]
fn a_type_is_resolved_only_when_it_is_evaluated() {
    // Like pkl, an unevaluated `is Foo` is no error.
    let json = eval("r = if (false) 1 is Foo else 2\nf = (x) -> x is Foo\n");
    assert_eq!(json["r"], 2);
}

#[test]
fn an_imported_module_names_its_types() {
    let temp = TestTempDir::new("pklr_test_is_module_types");
    std::fs::write(
        temp.path().join("m.pkl"),
        "class C {}\ntypealias A = Int\nx = 1\n",
    )
    .unwrap();
    let eval_main = |body: &str| {
        let path = temp.path().join("main.pkl");
        std::fs::write(&path, format!("import \"m.pkl\"\n{body}\n")).unwrap();
        pklr::eval_to_json(&path)
    };
    assert_eq!(
        eval_main("a = new m.C {} is m.C\nb = 1 is m.C").unwrap(),
        serde_json::json!({"a": true, "b": false})
    );
    let error = eval_main("r = 1 is m.Nope").unwrap_err().to_string();
    assert!(
        error.contains("Cannot find type `Nope` in module `m`."),
        "{error}"
    );
    let error = eval_main("r = 1 is Nope.Foo").unwrap_err().to_string();
    assert!(
        error.contains("Cannot find module import `Nope`."),
        "{error}"
    );
}

#[test]
fn to_dynamic_is_no_method_of_dynamic_or_mapping() {
    for src in [
        "r = new Dynamic { a = 1 }.toDynamic()",
        "r = new Dynamic { 1 }.toDynamic()",
        "r = new Mapping { [\"a\"] = 1 }.toDynamic()",
        "r = new Listing { 1 }.toDynamic()",
        "local d = new Dynamic { a = 1 }\nr = (d) { b = 2 }.toDynamic()",
        "local m = new Mapping { [\"a\"] = 1 }\nr = (m) { [\"b\"] = 2 }.toDynamic()",
        "r = Map(\"a\", 1).toDynamic().toDynamic()",
        "class C { a = 1 }\nr = new C {}.toDynamic().toDynamic()",
        "local d = new Dynamic { a = 1 }\nr = d?.toDynamic()",
        "r = (new Dynamic { a = 1 }) |> (d) -> d.toDynamic()",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("toDynamic"), "{src}: {error}");
    }
}

#[test]
fn to_dynamic_builds_a_dynamic_from_a_class_instance_or_a_map() {
    let json = eval(
        r#"
class C { a = 1 }
r = new C {}.toDynamic()
r2 = Map("a", 1).toDynamic()
r3 = (new C {}.toDynamic()) { b = 3 }
r4 = new Dynamic { a = 1 }.toMap()
r5 = new Mapping { ["a"] = 1 }.toMap()
"#,
    );
    assert_eq!(json["r"], serde_json::json!({"a": 1}));
    assert_eq!(json["r2"], serde_json::json!({"a": 1}));
    assert_eq!(json["r3"], serde_json::json!({"a": 1, "b": 3}));
    assert_eq!(json["r4"], serde_json::json!({"a": 1}));
    assert_eq!(json["r5"], serde_json::json!({"a": 1}));
}

#[test]
fn json_of_a_dynamic_with_elements_and_members_keeps_the_elements() {
    // `pkl eval -f json` rejects a `Dynamic` with elements and members.
    let src = "r = new Dynamic { 1; a = 2 }\n";
    let mut evaluator = Evaluator::new();
    let value = evaluator
        .eval_source(src, std::path::Path::new("test.pkl"))
        .unwrap();
    assert!(
        value
            .try_to_json()
            .unwrap_err()
            .to_string()
            .contains("Cannot render object with both elements and properties/entries as JSON.")
    );
    // The lenient conversion does not drop the elements.
    assert_eq!(
        value.to_json(),
        serde_json::json!({
            "r": {"_type": "dynamic", "elements": [1], "members": {"a": 2}}
        })
    );
    let path =
        std::env::temp_dir().join(format!("pklr_elements_members_{}.pkl", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let error = pklr::eval_to_json(&path).unwrap_err().to_string();
    let _ = std::fs::remove_file(&path);
    assert!(
        error.contains("Cannot render object with both elements"),
        "{error}"
    );
}
