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

#[test]
fn is_and_as_check_the_names_nested_in_a_type() {
    for src in [
        "r = 1 is List<Foo>(true)",
        "r = 1 is Int|List<Foo>(true)",
        "r = 1 as Map<String, Foo>(true)",
        "r = 1 is *List<Foo>|Int",
        "typealias T = Int|Foo\nr = 1 is T",
        "typealias T = List<Foo>\nr = 1 is T",
        "typealias T = List<Foo>(true)\nr = 1 is T",
        "typealias T = U?\ntypealias U = Foo\nr = 1 is T",
        "typealias T = *List<Foo>|Int\nr = 1 is T",
        "typealias T = Int|Foo\nr = 1 is List<T>",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("Cannot find type `Foo`."), "{src}: {error}");
    }
}

#[test]
fn a_default_marked_union_variant_is_not_an_unknown_type() {
    let json = eval(
        r#"
a = 1 is *List<Int>|Int
b = 1 is (*Int?|String)
c = List(1) is *List<Int>|Int
d = null is *Int?|String
e = 1 is *List<Int>(isEmpty)|Int
typealias T = *Int|String
f = 1 is T
"#,
    );
    for key in ["a", "b", "c", "d", "e", "f"] {
        assert_eq!(json[key], true, "{key}");
    }
}

#[test]
fn a_name_that_is_no_type_is_rejected() {
    for src in [
        "local Foo = 1\nr = 1 is Foo",
        "local Foo = new Dynamic { a = 1 }\nr = 1 as Foo",
        "foo = 1\nr = 1 is foo",
    ] {
        let error = eval_fails(src);
        assert!(
            error.contains("Expected `") && error.contains("to be a type, but it is not."),
            "{src}: {error}"
        );
    }
    // pkl rejects a type written `module.Foo`, `this.Foo` or `outer.Foo`;
    // pklr accepts one that names a class or alias, and reports the rest.
    for src in [
        "r = 1 is module.Foo",
        "r = 1 is this.Foo",
        "r = 1 is outer.Foo",
        "foo = 1\nr = 1 is module.foo",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("Cannot find type `"), "{src}: {error}");
    }
}

#[test]
fn is_and_as_unknown_match_a_collection() {
    let json = eval(
        r#"
a = List(1) is unknown
b = List(1) as unknown
c = new Listing { 1 } is unknown
d = Set(1) is unknown
e = new Dynamic { 1 } is unknown
f = new Dynamic { 1 } as unknown
g = List(1) is nothing
"#,
    );
    for key in ["a", "c", "d", "e"] {
        assert_eq!(json[key], true, "{key}");
    }
    assert_eq!(json["b"], serde_json::json!([1]));
    assert_eq!(json["g"], false);
}

#[test]
fn an_imported_module_names_its_type_aliases() {
    let temp = TestTempDir::new("pklr_test_is_module_aliases");
    std::fs::write(
        temp.path().join("m.pkl"),
        "typealias A = Int\ntypealias B = String(length > 1)|A\nclass C {}\n",
    )
    .unwrap();
    let eval_main = |body: &str| {
        let path = temp.path().join("main.pkl");
        std::fs::write(&path, format!("import \"m.pkl\"\n{body}\n")).unwrap();
        pklr::eval_to_json(&path)
    };
    assert_eq!(
        eval_main(
            "a = 1 is m.A\nb = \"x\" is m.A\nc = \"xx\" is m.B\nd = 1 as m.B\ne = \"x\" is m.B\nf = new m.C {} is m.A"
        )
        .unwrap(),
        serde_json::json!({"a": true, "b": false, "c": true, "d": 1, "e": false, "f": false})
    );
    let error = eval_main("r = 1 is m.Nope").unwrap_err().to_string();
    assert!(
        error.contains("Cannot find type `Nope` in module `m`."),
        "{error}"
    );
}

#[test]
fn the_names_in_an_imported_type_alias_are_checked() {
    let temp = TestTempDir::new("pklr_test_is_imported_alias_names");
    let write = |name: &str, src: &str| std::fs::write(temp.path().join(name), src).unwrap();
    write(
        "m.pkl",
        "typealias A = Int|Foo\ntypealias B = A\ntypealias G<T> = List<T|Foo>\n\
         typealias C = Int(this > 0)|Bar\ntypealias Ok = Int|String\n",
    );
    write(
        "n.pkl",
        "import \"m.pkl\"\ntypealias D = m.B\ntypealias E = m.Ok\n",
    );
    let eval_main = |body: &str| {
        let path = temp.path().join("main.pkl");
        std::fs::write(
            &path,
            format!("import \"m.pkl\"\nimport \"n.pkl\"\n{body}\n"),
        )
        .unwrap();
        pklr::eval_to_json(&path)
    };
    for (body, name) in [
        ("r = 1 is m.A", "Foo"),
        ("r = \"x\" is m.A", "Foo"),
        ("r = 1 is m.B", "Foo"),
        ("r = 1 is m.G<Int>", "Foo"),
        ("r = 1 is m.C", "Bar"),
        ("r = 1 is n.D", "Foo"),
        ("r = null is m.A?", "Foo"),
        ("r = 1 as m.A", "Foo"),
    ] {
        let error = eval_main(body).unwrap_err().to_string();
        assert!(
            error.contains(&format!("Cannot find type `{name}`.")),
            "{body}: {error}"
        );
    }
    assert_eq!(
        eval_main("b = 1 is m.Ok").unwrap(),
        serde_json::json!({"b": true})
    );
}

#[test]
fn an_inherited_type_alias_resolves_in_the_module_that_declares_it() {
    let temp = TestTempDir::new("pklr_test_is_inherited_alias");
    let write = |name: &str, src: &str| std::fs::write(temp.path().join(name), src).unwrap();
    write("dep.pkl", "class T {}\ntypealias U = Int|Zed\n");
    write("dep2.pkl", "class Other {}\n");
    write(
        "base.pkl",
        "open module base\nimport \"dep.pkl\"\ntypealias A = dep.T\ntypealias B = dep.U\ntypealias E = A|Int\n",
    );
    write("child.pkl", "open module child\nextends \"base.pkl\"\n");
    write("gc.pkl", "extends \"child.pkl\"\n");
    write("ch2.pkl", "amends \"base.pkl\"\n");
    // The same local name is bound to another module here.
    write(
        "shadow.pkl",
        "extends \"base.pkl\"\nimport \"dep2.pkl\" as dep\ntypealias F = dep.T\ntypealias G = A\n",
    );
    let eval_main = |module: &str, body: &str| {
        let path = temp.path().join("main.pkl");
        std::fs::write(&path, format!("import \"{module}.pkl\"\nr = {body}\n")).unwrap();
        pklr::eval_to_json(&path)
    };
    for (module, body, expected) in [
        ("child", "1 is child.A", false),
        ("child", "1 is child.E", true),
        ("gc", "1 is gc.A", false),
        ("ch2", "1 is ch2.A", false),
        ("shadow", "1 is shadow.A", false),
        ("shadow", "1 is shadow.G", false),
    ] {
        assert_eq!(
            eval_main(module, body).unwrap(),
            serde_json::json!({"r": expected}),
            "{body}"
        );
    }
    for (module, body) in [
        ("child", "null is child.B"),
        ("gc", "null is gc.B"),
        ("ch2", "null is ch2.B"),
        ("shadow", "null is shadow.B"),
    ] {
        let error = eval_main(module, body).unwrap_err().to_string();
        assert!(error.contains("Cannot find type `Zed`."), "{body}: {error}");
    }
}

#[test]
fn an_inherited_type_alias_matches_values_in_the_module_that_declares_it() {
    let temp = TestTempDir::new("pklr_test_is_inherited_alias_values");
    let write = |name: &str, src: &str| std::fs::write(temp.path().join(name), src).unwrap();
    write("dep.pkl", "class T {}\ntypealias U = Int\n");
    write("dep2.pkl", "class Other {}\n");
    write(
        "base.pkl",
        "open module base\nimport \"dep.pkl\"\ntypealias A = dep.U\ntypealias B = A\ntypealias K = B|String\n",
    );
    write(
        "child.pkl",
        "open module child\nextends \"base.pkl\"\ntypealias G = B\n",
    );
    write("gc.pkl", "extends \"child.pkl\"\ntypealias H = G\n");
    write("ch2.pkl", "amends \"base.pkl\"\n");
    write(
        "shadow.pkl",
        "extends \"base.pkl\"\nimport \"dep2.pkl\" as dep\ntypealias G = B\n",
    );
    let eval_main = |module: &str, body: &str| {
        let path = temp.path().join("main.pkl");
        std::fs::write(&path, format!("import \"{module}.pkl\"\n{body}\n")).unwrap();
        pklr::eval_to_json(&path).unwrap()
    };
    for (module, name) in [
        ("child", "A"),
        ("child", "B"),
        ("child", "G"),
        ("child", "K"),
        ("gc", "A"),
        ("gc", "H"),
        ("ch2", "A"),
        ("shadow", "G"),
    ] {
        let m = module;
        let json = eval_main(
            module,
            &format!("a = 1 is {m}.{name}\nb = 1 as {m}.{name}\nc = 1.5 is {m}.{name}"),
        );
        assert_eq!(
            json,
            serde_json::json!({"a": true, "b": 1, "c": false}),
            "{m}.{name}"
        );
    }
}

#[test]
fn an_inherited_alias_named_in_the_inheriting_module_resolves_in_the_declaring_one() {
    let temp = TestTempDir::new("pklr_test_is_unqualified_inherited_alias");
    let write = |name: &str, src: &str| std::fs::write(temp.path().join(name), src).unwrap();
    write("dep.pkl", "class T {}\ntypealias U = Int\n");
    write(
        "base.pkl",
        "open module base\nimport \"dep.pkl\"\ntypealias B = dep.U\ntypealias GB = List<dep.U>\nx = 0\n",
    );
    write(
        "child.pkl",
        "extends \"base.pkl\"\ntypealias G = GB\nu1 = 1 is B\nu2 = List(1) is GB\nu3 = 1 as B\nu4 = List(1) is G\nu5 = \"s\" is B\n",
    );
    write("am.pkl", "amends \"base.pkl\"\nx = 1 is B\n");
    let eval = |name: &str| pklr::eval_to_json(&temp.path().join(name)).unwrap();
    assert_eq!(
        eval("child.pkl"),
        serde_json::json!({"x": 0, "u1": true, "u2": true, "u3": 1, "u4": true, "u5": false})
    );
    assert_eq!(eval("am.pkl"), serde_json::json!({"x": true}));
}

#[test]
fn alias_chains_across_modules_are_no_cycle() {
    let temp = TestTempDir::new("pklr_test_is_alias_chain_modules");
    let write = |name: &str, src: &str| std::fs::write(temp.path().join(name), src).unwrap();
    write("n.pkl", "typealias X = B\ntypealias B = Int\n");
    write(
        "m.pkl",
        "import \"n.pkl\"\ntypealias A = B\ntypealias B = n.X\n",
    );
    write(
        "main.pkl",
        "import \"m.pkl\"\nr = 1 is m.A\ns = 1 as m.A\nt = \"x\" is m.A\n",
    );
    assert_eq!(
        pklr::eval_to_json(&temp.path().join("main.pkl")).unwrap(),
        serde_json::json!({"r": true, "s": 1, "t": false})
    );
}

#[test]
fn qualified_standard_library_types_and_quoted_forms_resolve() {
    let json = eval(
        r#"
import "pkl:base" as base
class `Step?` {}
typealias `X?` = Int
a = Regex("x") is base.Regex
b = 1 is base.Int
c = 1 is `X?`(true)
d = 1 is *`X?`|String
e = 1 is `Step?`(true)
f = 1 is List<`Step?`>
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({"a": true, "b": true, "c": true, "d": true, "e": false, "f": false})
    );
}

#[test]
fn type_names_are_read_from_the_type_and_not_its_text() {
    let json = eval(
        r#"
import "pkl:base" as base
class `Step?` {}
typealias L<É> = List<É>
typealias M<É> = List<É>(isEmpty)
a = List(1) is L<Int>
b = List() is M<Int>
c = 1 is List<`Step?`>(true)
d = 1 is *List<`Step?`>|Int
e = 1 is `Step?`(true)
f = 1 is base.Int
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({"a": true, "b": true, "c": false, "d": true, "e": false, "f": true})
    );
    // Only the `pkl:base` import qualifies the types of the standard library.
    for src in [
        "local m = 1\nr = 1 is m.Int",
        "import \"pkl:math\" as math\nr = 1 is math.Int",
        "import \"pkl:math\" as math\nr = 1 as math.Int",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("Cannot find type `Int`"), "{src}: {error}");
    }
}

#[test]
fn a_function_keeps_the_imports_its_type_checks_name() {
    let json = eval(
        r#"
import "pkl:base" as base
function f(x) = x is base.Int
class C { function m(x) = x is base.Int }
a = f(1)
b = f("a")
c = new C {}.m(1)
d = List(1).map((x) -> x is base.Int)
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({"a": true, "b": false, "c": true, "d": [true]})
    );
}

#[test]
fn a_type_name_resolves_to_its_import_whatever_shadows_the_value() {
    let json = eval(
        r#"
import "pkl:base" as base
function f(base) = (x) -> x is base.Int
a = f(new Dynamic {}).apply(1)
b = let (base = new Dynamic {}) (1 is base.Int)
c = new Listing { for (base in List(new Dynamic {})) { 1 is base.Int } }
"#,
    );
    assert_eq!(json, serde_json::json!({"a": true, "b": true, "c": [true]}));
}

#[test]
fn a_quoted_constrained_type_is_the_class_and_the_unquoted_one_is_nullable() {
    // With both `C` and a class named `C?`, backticks make the difference.
    let json = eval(
        r#"
class C {}
class `C?` {}
a = new `C?` {} is `C?`(true)
b = null is `C?`(true)
c = new C {} is `C?`(true)
d = null is C?(true)
e = new C {} is C?(true)
f = new `C?` {} is C?(true)
g = new `C?` {} is `C?`
h = null is `C?`
i = new `C?` {} as `C?`(true)
j = new C {} is C?
k = new `C?` {} is C?
l = null as C?(true)
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({
            "a": true, "b": false, "c": false, "d": true, "e": true, "f": false,
            "g": true, "h": false, "i": {}, "j": true, "k": false, "l": null
        })
    );
}

#[test]
fn a_quoted_alias_name_expands_to_its_type_in_a_constrained_check() {
    let json = eval(
        r#"
typealias L = List<Int>
function f(x) = x is `L`(true)
a = f(List(1))
b = List(1) is `L`(true)
c = List(1) as `L`(true)
d = f(List("a"))
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({"a": true, "b": true, "c": [1], "d": false})
    );
}

#[test]
fn an_expanded_constrained_alias_keeps_whether_its_target_is_a_quoted_name() {
    let json = eval(
        r#"
class C {}
class `C?` {}
typealias L = `C?`
typealias U = C?
typealias G = List<Int>
function f(x) = x is `L`(true)
function g(x) = x is U(true)
function h(x) = x is G(true)
a = f(new `C?` {})
b = f(null)
c = f(new C {})
d = g(null)
e = g(new C {})
f2 = g(new `C?` {})
i = h(List(1))
j = new `C?` {} is `L`(true)
k = null is `L`(true)
l = null is U(true)
m = new `C?` {} as `L`(true)
"#,
    );
    assert_eq!(
        json,
        serde_json::json!({
            "a": true, "b": false, "c": false, "d": true, "e": true, "f2": false,
            "i": true, "j": true, "k": false, "l": true, "m": {}
        })
    );
}

#[test]
fn an_alias_expanded_in_a_constrained_type_keeps_the_use_sites_nullable_suffix() {
    // Every alias target (`C?` quoted and not, `C`, `List<Int>`) used as
    // `L(c)`, `L?(c)`, `L?` and `L`, checked against null and three
    // instances, directly and in a function. The results are pkl 0.32.2's.
    const EXPECTED: &str = "0010101010100010110011001100110001001100110001000001100110010001";
    let mut direct = String::from(
        "class C {}\nclass `C?` {}\ntypealias LQ = `C?`\ntypealias LU = C?\ntypealias LC = C\ntypealias LG = List<Int>\n",
    );
    let mut in_function = direct.clone();
    let mut n = 0;
    for alias in ["LQ", "LU", "LC", "LG"] {
        for usage in [
            format!("{alias}(true)"),
            format!("{alias}?(true)"),
            format!("{alias}?"),
            alias.to_string(),
        ] {
            for value in ["null", "new C {}", "new `C?` {}", "List(1)"] {
                n += 1;
                direct.push_str(&format!("d{n} = {value} is {usage}\n"));
                in_function.push_str(&format!(
                    "function f{n}(x) = x is {usage}\nd{n} = f{n}({value})\n"
                ));
            }
        }
    }
    for (name, src) in [("direct", direct), ("in a function", in_function)] {
        let json = eval(&src);
        let bits: String = (1..=n)
            .map(|i| {
                if json[format!("d{i}")] == true {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        assert_eq!(bits, EXPECTED, "{name}");
    }
    // The casts that succeed also return the value.
    let json = eval(
        "class `C?` {}\ntypealias LQ = `C?`\nfunction f(x) = x as LQ?(true)\na = f(null)\nb = f(new `C?` {})",
    );
    assert_eq!(json, serde_json::json!({"a": null, "b": {}}));
}

#[test]
fn a_constrained_alias_use_matches_pkl_for_every_target_use_and_value() {
    // Targets `C?` (quoted), `C?` (nullable `C`), `X Y` and List<Int>, each
    // used as `L(true)`, `L(false)`, `L?(true)`, `L?(false)`, `L?` and `L`,
    // against null, an instance of each class and a list. The bits are pkl
    // 0.32.2's `is` results.
    const EXPECTED: &str = "001000000010100000001010000100110000000011000000001100011000000100000010010000001001000010000010000010001000001000100001";
    let header = "class C {}\nclass `C?` {}\nclass `X Y` {}\ntypealias LQ = `C?`\ntypealias LU = C?\ntypealias LS = `X Y`\ntypealias LG = List<Int>\n";
    let mut direct = String::from(header);
    let mut in_function = String::from(header);
    let mut casts = String::from(header);
    let mut n = 0;
    let mut cast_cases = Vec::new();
    for alias in ["LQ", "LU", "LS", "LG"] {
        for usage in [
            format!("{alias}(true)"),
            format!("{alias}(false)"),
            format!("{alias}?(true)"),
            format!("{alias}?(false)"),
            format!("{alias}?"),
            alias.to_string(),
        ] {
            for value in ["null", "new C {}", "new `C?` {}", "new `X Y` {}", "List(1)"] {
                n += 1;
                direct.push_str(&format!("d{n} = {value} is {usage}\n"));
                in_function.push_str(&format!(
                    "function f{n}(x) = x is {usage}\nd{n} = f{n}({value})\n"
                ));
                if EXPECTED.as_bytes()[n - 1] == b'1' {
                    casts.push_str(&format!(
                        "function g{n}(x) = x as {usage}\nd{n} = g{n}({value})\n"
                    ));
                } else {
                    cast_cases.push(format!(
                        "{header}function g(x) = x as {usage}\nr = g({value})\n"
                    ));
                }
            }
        }
    }
    for (name, src) in [("direct", direct), ("in a function", in_function)] {
        let json = eval(&src);
        let bits: String = (1..=n)
            .map(|i| {
                if json[format!("d{i}")] == true {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        assert_eq!(bits, EXPECTED, "{name}");
    }
    // A cast that holds returns the value.
    eval(&casts);
    // A cast that fails is an error.
    for src in cast_cases.iter().step_by(7) {
        eval_fails(src);
    }
}

#[test]
fn a_quoted_class_default_is_that_class_and_not_the_one_without_the_question_mark() {
    // `C` and a class named `C?`: the one that fails to build is the one
    // the property is typed with, as in pkl 0.32.2.
    let healthy_quoted = "class C { x = throw(\"bad C\") }\nclass `C?` { y = 1 }\nclass H { p: `C?`(true) }\nr = new H {}.p.y";
    assert_eq!(eval(healthy_quoted), serde_json::json!({"r": 1}));
    let plain = "class C { x = throw(\"bad C\") }\nclass `C?` { y = 1 }\nclass H { p: `C?` }\nr = new H {}.p.y";
    assert_eq!(eval(plain), serde_json::json!({"r": 1}));
    let poisoned_quoted = "class C { x = 1 }\nclass `C?` { y = throw(\"bad quoted\") }\nclass H { p: `C?`(true) }\nr = new H {}.p";
    assert!(eval_fails(poisoned_quoted).contains("bad quoted"));
    // Unquoted `C?(...)` is the nullable `C`, whose default is null.
    let nullable = "class C { x = throw(\"bad C\") }\nclass `C?` { y = 1 }\nclass H { p: C?(true) }\nr = new H {}.p";
    assert_eq!(eval(nullable), serde_json::json!({"r": null}));
}
#[test]
fn an_alias_of_a_non_identifier_quoted_name_keeps_the_use_sites_nullable_suffix() {
    // Results are pkl 0.32.2's, for `LS(c)`, `LS?(c)`, `LS?` and `LS`
    // against null and two instances, directly and in a function.
    const EXPECTED: &str = "001101101001";
    let mut direct = String::from("class C {}\nclass `X Y` {}\ntypealias LS = `X Y`\n");
    let mut in_function = direct.clone();
    let mut n = 0;
    for usage in ["LS(true)", "LS?(true)", "LS?", "LS"] {
        for value in ["null", "new C {}", "new `X Y` {}"] {
            n += 1;
            direct.push_str(&format!("d{n} = {value} is {usage}\n"));
            in_function.push_str(&format!(
                "function f{n}(x) = x is {usage}\nd{n} = f{n}({value})\n"
            ));
        }
    }
    for (name, src) in [("direct", direct), ("in a function", in_function)] {
        let json = eval(&src);
        let bits: String = (1..=n)
            .map(|i| {
                if json[format!("d{i}")] == true {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        assert_eq!(bits, EXPECTED, "{name}");
    }
}

#[test]
fn a_constrained_quoted_alias_resolves_in_a_function_and_when_default_marked() {
    let json = eval(
        r#"
typealias `X Y` = Int
function f(x) = x is `X Y`(true)
a = f(1)
b = f("a")
c = 1 is *`X Y`(true)|String
"#,
    );
    assert_eq!(json, serde_json::json!({"a": true, "b": false, "c": true}));
}

#[test]
fn a_keyword_spelled_type_name_is_not_found() {
    for src in [
        "r = 1 is `as`(true)",
        "r = 1 is *`hidden`|Int",
        "r = 1 is `as`",
    ] {
        let error = eval_fails(src);
        assert!(error.contains("Cannot find type"), "{src}: {error}");
    }
}

#[test]
fn to_dynamic_works_on_a_module_that_carries_a_source() {
    let temp = TestTempDir::new("pklr_test_to_dynamic_module");
    std::fs::write(temp.path().join("m.pkl"), "@Deprecated\nold = 1\nx = 2\n").unwrap();
    let path = temp.path().join("main.pkl");
    std::fs::write(&path, "import \"m.pkl\"\nr = m.toDynamic().x\n").unwrap();
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"r": 2})
    );
}

#[test]
fn the_type_parameters_of_an_alias_are_no_unknown_types() {
    let json = eval(
        r#"
typealias L<T> = List<T>
typealias M<K, V> = Map<K, V>(isEmpty)|Listing<V>
class T {}
a = List(1) is L<Int>
b = Map() is M<String, Int>
c = 1 is M<String, Int>
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], true);
    assert_eq!(json["c"], false);
    let error = eval_fails("typealias L<T> = List<T|Foo>\nr = 1 is L<Int>");
    assert!(error.contains("Cannot find type `Foo`."), "{error}");
}

#[test]
fn a_quoted_type_name_is_resolved_as_written() {
    let json = eval(
        r#"
class `Step?` {}
class `A|B` {}
class `L<T>` {}
typealias `X?` = Int
a = 1 is `Step?`
b = 1 is `A|B`
c = 1 is `L<T>`
d = 1 is `X?`
e = new `Step?` {} is `Step?`
f = 1 is `Step?`?
"#,
    );
    assert_eq!(json["a"], false);
    assert_eq!(json["b"], false);
    assert_eq!(json["c"], false);
    assert_eq!(json["d"], true);
    assert_eq!(json["e"], true);
    assert_eq!(json["f"], false);
    let error = eval_fails("r = 1 is `Nope?`");
    assert!(error.contains("Cannot find"), "{error}");
}

#[test]
fn a_long_alias_chain_is_walked_without_recursion() {
    for length in [3000, 20000] {
        let mut src = String::new();
        for index in 0..length {
            src.push_str(&format!("typealias A{index} = A{}\n", index + 1));
        }
        src.push_str(&format!("typealias A{length} = Int\nr = 1 is A0\n"));
        let json = eval(&src);
        assert_eq!(json["r"], true);
    }
}

#[test]
fn quoted_bases_keep_their_defaults_and_unions_when_amended() {
    // A nullable use of a quoted alias target amends the class named `C?`,
    // not `C`.
    let json = eval(
        "class C { x = 1 }\nclass `C?` { y = 2 }\ntypealias L = `C?`\nclass H { p: L?(true) }\nr = new H { p { y = 5 } }",
    );
    assert_eq!(json, serde_json::json!({"r": {"p": {"y": 5}}}));
    // A quoted built-in is that built-in: its default is the empty listing.
    let json = eval("class H { items: `Listing`(true) }\nr = new H {}.items");
    assert_eq!(json, serde_json::json!({"r": []}));
    // A quoted alias of a union with no default cannot be amended.
    let error = eval_fails(
        "typealias U = Dynamic|String\nclass H { p: `U`(true) }\nr = new H { p { x = 1 } }",
    );
    assert!(error.contains("undefined"), "{error}");
}

#[test]
fn an_imported_class_is_a_type_whatever_value_shadows_the_import() {
    let temp = TestTempDir::new("pklr_test_is_shadowed_import_class");
    std::fs::write(temp.path().join("m.pkl"), "class C {}\ntypealias A = Int\n").unwrap();
    let path = temp.path().join("main.pkl");
    std::fs::write(
        &path,
        r#"import "m.pkl"
local inst = new m.C {}
a = let (m = 0) (inst is m.C)
b = let (m = 0) (1 is m.C)
c = let (m = 0) (inst as m.C)
function f(m) = inst is m.C
d = f(0)
e = new Listing { for (m in List(0)) { inst is m.C } }
g = let (m = 0) (1 is m.A)
"#,
    )
    .unwrap();
    assert_eq!(
        pklr::eval_to_json(&path).unwrap(),
        serde_json::json!({"a": true, "b": false, "c": {}, "d": true, "e": [true], "g": true})
    );
}

#[test]
fn a_module_exports_the_types_of_every_base_it_amends_or_extends() {
    let temp = TestTempDir::new("pklr_test_inherited_exports");
    let write = |name: &str, src: &str| std::fs::write(temp.path().join(name), src).unwrap();
    write(
        "base.pkl",
        "open module base\ntypealias A = Int\nlocal typealias LA = Int\nclass C {}\nlocal class LC {}\nx = 1\n",
    );
    write(
        "mid.pkl",
        "open module mid\nextends \"base.pkl\"\ntypealias B = String\n",
    );
    write("top.pkl", "extends \"mid.pkl\"\n");
    write("amend.pkl", "amends \"base.pkl\"\nx = 2\n");
    let eval = |module: &str, body: &str| {
        let path = temp.path().join("main.pkl");
        std::fs::write(&path, format!("import \"{module}.pkl\"\n{body}\n")).unwrap();
        pklr::eval_to_json(&path)
    };
    // Through two levels of extends, and through amends.
    assert_eq!(
        eval(
            "top",
            "a = 1 is top.A\nb = \"s\" is top.B\nc = new top.C {} is top.C"
        )
        .unwrap(),
        serde_json::json!({"a": true, "b": true, "c": true})
    );
    assert_eq!(
        eval("amend", "a = 1 is amend.A\nc = new amend.C {} is amend.C").unwrap(),
        serde_json::json!({"a": true, "c": true})
    );
    // A local type of a base is no member of the module that inherits it.
    for (module, name) in [("top", "LA"), ("top", "LC"), ("amend", "LA"), ("mid", "LC")] {
        assert!(
            eval(module, &format!("r = 1 is {module}.{name}")).is_err(),
            "{module}.{name}"
        );
    }
}

#[test]
fn a_reused_evaluator_forgets_the_base_a_module_used_to_extend() {
    let temp = TestTempDir::new("pklr_test_reused_evaluator_bases");
    let write = |name: &str, src: &str| std::fs::write(temp.path().join(name), src).unwrap();
    write("base.pkl", "open module base\nclass C {}\n");
    write("m.pkl", "extends \"base.pkl\"\n");
    write("main.pkl", "import \"m.pkl\"\nr = 1 is m.C\n");
    let mut evaluator = Evaluator::new();
    let main = temp.path().join("main.pkl");
    assert_eq!(
        evaluator.eval_file(&main).unwrap().to_json(),
        serde_json::json!({"r": false})
    );
    // The module no longer extends the base and keeps `C` to itself.
    write("m.pkl", "import \"base.pkl\"\nlocal class C {}\nx = base\n");
    let error = evaluator.eval_file(&main).unwrap_err().to_string();
    assert!(error.contains("Cannot find type `C`"), "{error}");
}
