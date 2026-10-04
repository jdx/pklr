use super::*;

// ============================================================
// String API (pkl:base String, Regex, RegexMatch)
// ============================================================

#[test]
fn string_lengths_count_code_points() {
    let json = eval(
        r##"
s = "🙈🙉a"
length = s.length
lastIndex = s.lastIndex
chars = s.chars
codePoints = s.codePoints
reversed = s.reverse()
second = s[1]
getOrNull = s.getOrNull(3)
sub = s.substring(1, 3)
"##,
    );
    assert_eq!(json["length"], 3);
    assert_eq!(json["lastIndex"], 2);
    assert_eq!(json["chars"], serde_json::json!(["🙈", "🙉", "a"]));
    assert_eq!(json["codePoints"], serde_json::json!([128584, 128585, 97]));
    assert_eq!(json["reversed"], "a🙉🙈");
    assert_eq!(json["second"], "🙉");
    assert_eq!(json["getOrNull"], serde_json::Value::Null);
    assert_eq!(json["sub"], "🙉a");
}

#[test]
fn string_index_errors_match_pkl() {
    assert!(
        eval_fails(r#"x = "abcdefg".substring(2, 8)"#)
            .contains("Character index `8` is out of range `2`..`7`.\nString: \"abcdefg\"")
    );
    assert!(
        eval_fails(r#"x = "abc"[3]"#).contains("Character index `3` is out of range `0`..`2`.")
    );
    assert!(eval_fails(r#"x = "abcdefg".indexOf("cdx")"#).contains(
        "String does not contain a match for literal pattern.\nString : \"abcdefg\"\nPattern: \"cdx\""
    ));
    assert!(
        eval_fails(r#"x = "abc".take(-1)"#).contains("Expected a positive number, but got `-1`.")
    );
}

#[test]
fn string_search_and_slice_methods() {
    let json = eval(
        r##"
s = "abxabyabz"
indexOf = s.indexOf("ab")
lastIndexOf = s.lastIndexOf("ab")
regexIndex = s.indexOf(Regex("b[yz]"))
missing = s.indexOfOrNull("q")
take = s.take(4)
takeLast = s.takeLast(100)
drop = s.drop(7)
dropLast = s.dropLast(3)
takeWhile = s.takeWhile((c) -> c != "y")
dropLastWhile = s.dropLastWhile((c) -> c != "y")
startsWith = s.startsWith(Regex("a.x"))
endsWith = s.endsWith(Regex("b."))
contains = s.contains(Regex("x.b"))
matches = s.matches(Regex("(ab.)*"))
"##,
    );
    assert_eq!(json["indexOf"], 0);
    assert_eq!(json["lastIndexOf"], 6);
    assert_eq!(json["regexIndex"], 4);
    assert_eq!(json["missing"], serde_json::Value::Null);
    assert_eq!(json["take"], "abxa");
    assert_eq!(json["takeLast"], "abxabyabz");
    assert_eq!(json["drop"], "bz");
    assert_eq!(json["dropLast"], "abxaby");
    assert_eq!(json["takeWhile"], "abxab");
    assert_eq!(json["dropLastWhile"], "abxaby");
    assert_eq!(json["startsWith"], true);
    assert_eq!(json["endsWith"], true);
    assert_eq!(json["contains"], true);
    assert_eq!(json["matches"], true);
}

#[test]
fn string_predicates_must_return_booleans() {
    assert!(
        eval_fails(r#"x = "abc".takeWhile((c) -> 42)"#)
            .contains("Expected value of type `Boolean`, but got type `Int`.\nValue: 42")
    );
}

#[test]
fn string_split_follows_java_semantics() {
    let json = eval(
        r##"
trailing = "a,b,,".split(",")
leading = ",a".split(",")
empty = "".split(",")
chars = "abc".split("")
regex = "a1b22c".split(Regex(#"\d+"#))
limited = "a,b,c".splitLimit(",", 2)
"##,
    );
    assert_eq!(json["trailing"], serde_json::json!(["a", "b"]));
    assert_eq!(json["leading"], serde_json::json!(["", "a"]));
    assert_eq!(json["empty"], serde_json::json!([""]));
    assert_eq!(json["chars"], serde_json::json!(["a", "b", "c"]));
    assert_eq!(json["regex"], serde_json::json!(["a", "b", "c"]));
    assert_eq!(json["limited"], serde_json::json!(["a", "b,c"]));
    assert!(
        eval_fails(r#"x = "abc".splitLimit(",", 0)"#)
            .contains("Type constraint `this > 0` violated.\nValue: 0")
    );
}

#[test]
fn string_regex_replacements_expand_groups() {
    let json = eval(
        r##"
first = "aabbcc".replaceFirst(Regex("(b+)"), "[$1]")
last = "abab".replaceLast(Regex("a(b)"), "<$1>")
all = "a1b2".replaceAll(Regex(#"(\d)"#), #"\$$1"#)
mapped = "aabbccaabbcc".replaceAllMapped(Regex("(b+)c"), (m) -> ">>\(m)<<")
groups = "aabbccaabbcc".replaceFirstMapped(Regex("(b+)c"), (m) -> m.groups.last.value)
literal = "a.b.c".replaceAll(".", "$")
"##,
    );
    assert_eq!(json["first"], "aa[bb]cc");
    assert_eq!(json["last"], "ab<b>");
    assert_eq!(json["all"], "a$1b$2");
    assert_eq!(json["mapped"], "aa>>bbc<<caa>>bbc<<c");
    assert_eq!(json["groups"], "aabbcaabbcc");
    assert_eq!(json["literal"], "a$b$c");
    assert!(
        eval_fails(r#"x = "abcc".replaceAll(Regex("cc"), "$4")"#)
            .contains("Error replacing matches for regex `cc` with `$4`: `No group 4`")
    );
}

#[test]
fn string_case_trim_and_padding() {
    let json = eval(
        r##"
upper = "straße".toUpperCase()
capitalize = "ǆemal".capitalize()
decapitalize = "ABC".decapitalize()
trim = "\t abc \n".trim()
trimStart = "  abc  ".trimStart()
trimEnd = "  abc  ".trimEnd()
padStart = "7".padStart(3, "0")
padEnd = "ab".padEnd(4, "-")
repeat = "ab".repeat(3)
blank = " \t\n".isBlank
"##,
    );
    assert_eq!(json["upper"], "STRASSE");
    assert_eq!(json["capitalize"], "ǅemal");
    assert_eq!(json["decapitalize"], "aBC");
    assert_eq!(json["trim"], "abc");
    assert_eq!(json["trimStart"], "abc  ");
    assert_eq!(json["trimEnd"], "  abc");
    assert_eq!(json["padStart"], "007");
    assert_eq!(json["padEnd"], "ab--");
    assert_eq!(json["repeat"], "ababab");
    assert_eq!(json["blank"], true);
    assert!(
        eval_fails(r#"x = "ab".repeat(-1)"#)
            .contains("Type constraint `isPositive` violated.\nValue: -1")
    );
}

#[test]
fn string_conversions_accept_digit_separators() {
    let json = eval(
        r##"
int = "1_000".toInt()
float = "1_000.5e1_0".toFloat()
badInt = "_1".toIntOrNull()
bool = "TRUE".toBoolean()
badBool = "yes".toBooleanOrNull()
"##,
    );
    assert_eq!(json["int"], 1000);
    assert_eq!(json["float"], 1000.5e10);
    assert_eq!(json["badInt"], serde_json::Value::Null);
    assert_eq!(json["bool"], true);
    assert_eq!(json["badBool"], serde_json::Value::Null);
    assert!(
        eval_fails(r#"x = "1.2".toInt()"#)
            .contains("Cannot parse string as `Int`.\nString: \"1.2\"")
    );
}

#[test]
fn string_hashes_and_base64() {
    let json = eval(
        r##"
md5 = "abc".md5
sha1 = "abc".sha1
sha256 = "abc".sha256
sha256Int = "abc".sha256Int
base64 = "Hello".base64
decoded = "SGVsbG8".base64Decoded
isBase64 = "SGVsbG8=".isBase64
notBase64 = "~~~".isBase64
"##,
    );
    assert_eq!(json["md5"], "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(json["sha1"], "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(
        json["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(json["sha256Int"], -1_527_000_031_757_436_742_i64);
    assert_eq!(json["base64"], "SGVsbG8=");
    assert_eq!(json["decoded"], "Hello");
    assert_eq!(json["isBase64"], true);
    assert_eq!(json["notBase64"], false);
}

#[test]
fn string_is_regex_and_glob_pattern() {
    let json = eval(
        r##"
regex = "a(b".isRegex
glob = "**/*.{json,yaml}".isGlobPattern
badGlob = "{a,{b}}".isGlobPattern
"##,
    );
    assert_eq!(json["regex"], false);
    assert_eq!(json["glob"], true);
    assert_eq!(json["badGlob"], false);
}

#[test]
fn to_string_uses_pkl_formatting() {
    let json = eval(
        r##"
floats = "\(1.0) \(1e7) \(0.0001) \(-0.0) \(123.456)"
list = List(1, "a", 2.5).toString()
regex = Regex(#"a\d"#).toString()
object = new Dynamic { a = 1; b = "x" }.toString()
"##,
    );
    assert_eq!(json["floats"], "1.0 1.0E7 1.0E-4 -0.0 123.456");
    assert_eq!(json["list"], r#"List(1, "a", 2.5)"#);
    assert_eq!(json["regex"], r##"Regex(#"a\d"#)"##);
    assert_eq!(json["object"], r#"new Dynamic { a = 1; b = "x" }"#);
}

#[test]
fn regex_find_matches_reports_groups() {
    let json = eval(
        r##"
local re = Regex(#"(abc)|(def)"#)
groupCount = re.groupCount
pattern = re.pattern
matches = re.findMatchesIn("xxxabcxxxdef").map((m) -> m.value)
groups = re.findMatchesIn("xxxabc").first.groups.map((g) -> g?.start)
entire = Regex(#"(\d+)\w+"#).matchEntire("123abc").groups.last.value
noMatch = Regex(#"\d+"#).matchEntire("123abc")
empties = Regex("").findMatchesIn("ab").map((m) -> m.start)
utf16 = Regex("b").findMatchesIn("😀b").first.start
"##,
    );
    assert_eq!(json["groupCount"], 2);
    assert_eq!(json["pattern"], "(abc)|(def)");
    assert_eq!(json["matches"], serde_json::json!(["abc", "def"]));
    assert_eq!(json["groups"], serde_json::json!([3, 3, null]));
    assert_eq!(json["entire"], "123");
    assert_eq!(json["noMatch"], serde_json::Value::Null);
    assert_eq!(json["empties"], serde_json::json!([0, 1, 2]));
    // Match positions count UTF-16 code units, as on the JVM.
    assert_eq!(json["utf16"], 2);
}

#[test]
fn regex_syntax_errors_are_reported() {
    assert!(eval_fails(r#"x = Regex("a(b")"#).contains("Syntax error in regex `a(b`"));
}

#[test]
fn regex_cannot_be_rendered_as_json() {
    let temp = TestTempDir::new("pklr_test_regex_json");
    let path = temp.path().join("test.pkl");
    std::fs::write(&path, "glob = Regex(\"a.*\")\n").unwrap();
    let error = pklr::eval_to_json(&path).unwrap_err().to_string();
    assert!(
        error.contains("Cannot render value of type `Regex` as JSON.\nValue: Regex(\"a.*\")"),
        "{error}"
    );
}

#[test]
fn regex_converter_renders_regex_as_json() {
    let temp = TestTempDir::new("pklr_test_regex_converter");
    let path = temp.path().join("test.pkl");
    std::fs::write(
        &path,
        r##"
glob = Regex(#"^.*\.json$"#)
output {
  renderer {
    converters {
      [Regex] = (r) -> new Mapping {
        ["_type"] = "regex"
        ["pattern"] = r.pattern
      }
    }
  }
}
"##,
    )
    .unwrap();
    let json = pklr::eval_to_json(&path).unwrap();
    assert_eq!(
        json["glob"],
        serde_json::json!({"_type": "regex", "pattern": r"^.*\.json$"})
    );
}
