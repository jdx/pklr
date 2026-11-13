//! The declarations `pkl:reflect` describes: where a class, type alias or
//! member is declared, its doc comment, its annotations and its supertypes.
//! Expected results are what `pkl` 0.32 prints for the same snippets.

use super::*;
use serde_json::json;

const SNIPPET: &str = r#"
import "pkl:reflect"

/// A pet.
///
/// Pets have names.
@Deprecated { message = "use Animal" }
open class Pet {
  /// The pet's name.
  name: String

  hidden age: Int = 1

  /// Feeds the pet.
  function feed(food: String, amount: Int): String = food
}

class Dog extends Pet {
  breed: String
}

typealias Name = String | "none"

/// An optional name.
typealias MaybeName = Name?
"#;

fn with_snippet(tail: &str) -> serde_json::Value {
    eval(&format!("{SNIPPET}\n{tail}"))
}

#[test]
fn a_class_reports_where_it_is_declared() {
    let result = with_snippet(
        r#"
loc = reflect.Class(Pet).location
member = reflect.Class(Pet).properties["name"].location
method = reflect.Class(Pet).methods["feed"].location
"#,
    );
    assert_eq!(
        result["loc"],
        json!({"line": 8, "column": 1, "displayUri": "test.pkl, line 8"})
    );
    assert_eq!(
        result["member"],
        json!({"line": 10, "column": 3, "displayUri": "test.pkl, line 10"})
    );
    assert_eq!(result["method"]["line"], json!(15));
    assert_eq!(result["method"]["column"], json!(3));
}

#[test]
fn a_class_carries_its_doc_comment_and_annotations() {
    let result = with_snippet(
        r#"
doc = reflect.Class(Pet).docComment
none = reflect.Class(Dog).docComment
annotations = reflect.Class(Pet).annotations.map((a) -> a.message)
memberDoc = reflect.Class(Pet).properties["name"].docComment
methodDoc = reflect.Class(Pet).methods["feed"].docComment
"#,
    );
    assert_eq!(result["doc"], json!("A pet.\n\nPets have names."));
    assert_eq!(result["none"], json!(null));
    assert_eq!(result["annotations"], json!(["use Animal"]));
    assert_eq!(result["memberDoc"], json!("The pet's name."));
    assert_eq!(result["methodDoc"], json!("Feeds the pet."));
}

#[test]
fn a_class_reports_its_modifiers_and_method_parameters() {
    let result = with_snippet(
        r#"
modifiers = reflect.Class(Pet).modifiers.toList()
memberModifiers = reflect.Class(Pet).properties["age"].modifiers.toList()
parameters = reflect.Class(Pet).methods["feed"].parameters.keys.toList()
"#,
    );
    assert_eq!(result["modifiers"], json!(["open"]));
    assert_eq!(result["memberModifiers"], json!(["hidden"]));
    assert_eq!(result["parameters"], json!(["food", "amount"]));
}

#[test]
fn the_superclass_of_a_class_is_the_class_it_extends_or_typed() {
    let result = with_snippet(
        r#"
dog = reflect.Class(Dog).superclass.name
pet = reflect.Class(Pet).superclass.name
typed = reflect.Class(Pet).superclass.superclass.name
any = reflect.Class(Pet).superclass.superclass.superclass.name
supertype = reflect.Class(Dog).supertype.referent.name
"#,
    );
    assert_eq!(
        result,
        json!({"dog": "Pet", "pet": "Typed", "typed": "Object", "any": "Any",
               "supertype": "Pet"})
    );
}

#[test]
fn all_properties_and_methods_include_the_inherited_ones() {
    let result = with_snippet(
        r#"
own = reflect.Class(Dog).properties.keys.toList()
all = reflect.Class(Dog).allProperties.keys.toList()
methods = reflect.Class(Dog).allMethods.keys.toList().contains("feed")
base = reflect.Class(Dog).allMethods.keys.toList().contains("getClass")
"#,
    );
    assert_eq!(
        result,
        json!({"own": ["breed"], "all": ["name", "age", "breed"], "methods": true, "base": true})
    );
}

#[test]
fn a_standard_library_class_carries_the_declarations_of_base_pkl() {
    let result = eval(
        r#"
import "pkl:reflect"
c = reflect.Class(Boolean)
name = c.name
modifiers = c.modifiers.toList()
doc = c.docComment.split("\n").first
loc = c.location.displayUri
xor = c.methods["xor"].location.line
xorParameters = c.methods["xor"].parameters.keys.toList()
length = reflect.Class(String).properties["length"].annotations[0].names.toList()
generic = reflect.Class(List).typeParameters.map((p) -> p.name)
"#,
    );
    assert_eq!(
        result,
        json!({
            "c": result["c"],
            "name": "Boolean",
            "modifiers": ["external"],
            "doc": "A boolean value, either [true] or [false].",
            "loc": "https://github.com/apple/pkl/blob/0.32.2/stdlib/base.pkl#L1217",
            "xor": 1227,
            "xorParameters": ["other"],
            "length": ["size", "count"],
            "generic": ["Element"]
        })
    );
}

#[test]
fn a_type_alias_reports_its_declaration_and_referent() {
    let result = with_snippet(
        r#"
alias = reflect.TypeAlias(MaybeName)
name = alias.name
doc = alias.docComment
line = alias.location.line
"#,
    );
    assert_eq!(result["name"], json!("MaybeName"));
    assert_eq!(result["doc"], json!("An optional name."));
    assert_eq!(result["line"], json!(25));
}

#[test]
fn a_type_alias_mirrors_its_target_type() {
    let result = with_snippet(
        r#"
union = reflect.TypeAlias(Name).referent
members = union.members.length
first = union.members[0].referent.name
literal = union.members[1].value
nullable = reflect.TypeAlias(MaybeName).referent.member.referent.name
"#,
    );
    assert_eq!(result["members"], json!(2));
    assert_eq!(result["first"], json!("String"));
    assert_eq!(result["literal"], json!("none"));
    assert_eq!(result["nullable"], json!("Name"));
}

#[test]
fn declared_type_wraps_a_class_with_unknown_type_arguments() {
    let result = eval(
        r#"
import "pkl:reflect"
class Foo {}
foo = reflect.DeclaredType(reflect.Class(Foo))
name = foo.referent.name
arguments = foo.typeArguments.length
list = reflect.DeclaredType(reflect.Class(List)).typeArguments.length
literal = reflect.StringLiteralType("x").value
union = reflect.UnionType(List(foo, foo)).members.length
"#,
    );
    assert_eq!(result["name"], json!("Foo"));
    assert_eq!(result["arguments"], json!(0));
    assert_eq!(result["list"], json!(1));
    assert_eq!(result["literal"], json!("x"));
    assert_eq!(result["union"], json!(2));
}

#[test]
fn a_property_merges_the_modifiers_of_the_one_it_overrides() {
    let result = eval(
        r#"
import "pkl:reflect"
open class A {
  /// The id.
  hidden id: Int = 1
}
class B extends A {
  id = 2
}
own = reflect.Class(B).properties["id"].modifiers.toList()
all = reflect.Class(B).properties["id"].allModifiers.toList()
"#,
    );
    assert_eq!(result["own"], json!([]));
    assert_eq!(result["all"], json!(["hidden"]));
}

/// Evaluates `main` next to the other `files`.
fn with_files(files: &[(&str, &str)], main: &str) -> serde_json::Value {
    let dir = TestTempDir::new("reflect");
    for (name, source) in files {
        std::fs::write(dir.path().join(name), source).unwrap();
    }
    let path = dir.path().join("main.pkl");
    let mut ev = Evaluator::new();
    ev.eval_source(main, &path).unwrap().to_json()
}

#[test]
fn an_imported_alias_is_mirrored_where_its_module_declares_it() {
    let result = with_files(
        &[(
            "lib.pkl",
            "typealias Inner = String | \"x\"\n/// outer doc\ntypealias Outer = Inner?\n",
        )],
        r#"
import "pkl:reflect"
import "lib.pkl"
typealias Mine = lib.Outer
alias = reflect.TypeAlias(Mine).referent.referent
doc = alias.docComment
inner = alias.referent.member.referent.name
"#,
    );
    assert_eq!(result["doc"], json!("outer doc"));
    assert_eq!(result["inner"], json!("Inner"));
}

#[test]
fn a_chain_of_aliases_mirrors_each_alias() {
    let result = with_snippet(
        r#"
class Base {}
typealias First = Base
typealias Second = First?
chain = reflect.TypeAlias(Second).referent.member.referent.name
"#,
    );
    assert_eq!(result["chain"], json!("First"));
}

#[test]
fn a_constrained_generic_mirrors_the_type_it_constrains() {
    let result = with_snippet(
        r#"
typealias Some = List<Int>(length > 0)
referent = reflect.TypeAlias(Some).referent.referent.name
argument = reflect.TypeAlias(Some).referent.typeArguments[0].referent.name
"#,
    );
    assert_eq!(result["referent"], json!("List"));
    assert_eq!(result["argument"], json!("Int"));
}

#[test]
fn annotations_are_listings() {
    let result = with_snippet(
        r#"
cls = reflect.Class(Pet).annotations is Listing
prop = reflect.Class(Pet).properties["name"].annotations is Listing
all = reflect.Class(Pet).properties["name"].allAnnotations is Listing
"#,
    );
    assert_eq!(result, json!({"cls": true, "prop": true, "all": true}));
}

#[test]
fn an_import_only_an_annotation_uses_is_bound() {
    let result = with_files(
        &[("ann.pkl", "message = \"hi\"\n")],
        r#"
import "pkl:reflect"
import "ann.pkl" as ann
@Deprecated { message = ann.message }
class C {}
message = reflect.Class(C).annotations[0].message
"#,
    );
    assert_eq!(result["message"], json!("hi"));
}

#[test]
fn a_nested_class_does_not_stand_for_a_module_level_one() {
    let result = with_snippet(
        r#"
class Outer { class N { a: Int } }
/// top level
class N { b: Int }
doc = reflect.Class(N).docComment
members = reflect.Class(N).properties.keys.toList()
"#,
    );
    assert_eq!(result["doc"], json!("top level"));
    assert_eq!(result["members"], json!(["b"]));
}

#[test]
fn a_method_without_a_body_is_reflected() {
    let result = with_snippet(
        r#"
abstract class Shape {
  /// The area.
  abstract function area(x: Int, y: Int): Number
  name: String
  function perimeter(): Number = 1
}
methods = reflect.Class(Shape).methods.keys.toList()
parameters = reflect.Class(Shape).methods["area"].parameters.keys.toList()
doc = reflect.Class(Shape).methods["area"].docComment
modifiers = reflect.Class(Shape).methods["area"].modifiers.toList()
"#,
    );
    assert_eq!(result["methods"], json!(["area", "perimeter"]));
    assert_eq!(result["parameters"], json!(["x", "y"]));
    assert_eq!(result["doc"], json!("The area."));
    assert_eq!(result["modifiers"], json!(["abstract"]));
}

#[test]
fn the_implicit_parent_of_an_external_class_is_any() {
    let result = with_snippet("parent = reflect.Class(Boolean).superclass.name");
    assert_eq!(result["parent"], json!("Any"));
}

#[test]
fn base_methods_keep_their_whole_parameter_lists() {
    let result = with_snippet(
        r#"
filter = reflect.Class(List).methods["filterNonNull"].parameters.keys.toList()
toMap = reflect.Class(List).methods["toMap"].parameters.keys.toList()
"#,
    );
    assert_eq!(result["filter"], json!([]));
    assert_eq!(result["toMap"], json!(["keyExtractor", "valueExtractor"]));
}

#[test]
fn an_import_shared_with_an_annotation_keeps_the_fields_it_reads() {
    let result = with_files(
        &[("ann.pkl", "message = \"hi\"\nother = 1\n")],
        r#"
import "pkl:reflect"
import "ann.pkl" as ann
used = ann.other
@Deprecated { message = ann.message }
class C {}
message = reflect.Class(C).annotations[0].message
"#,
    );
    assert_eq!(result["message"], json!("hi"));
    assert_eq!(result["used"], json!(1));
}
