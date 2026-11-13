use super::*;

// Expected values are what `pkl eval` 0.32.2 produces for the same source.

#[test]
fn list_and_set_methods() {
    let json = eval(
        r#"
import "pkl:test"
local l = List(3, 1, 2)
sortBy = l.sortBy((x) -> -x)
find = l.find((x) -> x > 1)
findOrNull = l.findOrNull((x) -> x > 5)
getOrNull = l.getOrNull(7)
take = l.take(2)
drop = l.drop(1)
zip = l.zip(List("a", "b", "c")).map((p) -> p.first)
partition = List(1, 2, 3, 4).partition((x) -> x > 2).first
max = l.maxOrNull
minBy = l.minBy((x) -> x)
groupBy = l.groupBy((x) -> x.toString()).toMap().mapValues((k, v) -> v.length)
emptyMin = test.catch(() -> List().min)
"#,
    );
    assert_eq!(json["sortBy"], serde_json::json!([3, 2, 1]));
    assert_eq!(json["find"], 3);
    assert!(json.get("findOrNull").is_none_or(|v| v.is_null()));
    assert!(json.get("getOrNull").is_none_or(|v| v.is_null()));
    assert_eq!(json["take"], serde_json::json!([3, 1]));
    assert_eq!(json["drop"], serde_json::json!([1, 2]));
    assert_eq!(json["zip"], serde_json::json!([3, 1, 2]));
    assert_eq!(json["partition"], serde_json::json!([3, 4]));
    assert_eq!(json["max"], 3);
    assert_eq!(json["minBy"], 1);
    assert_eq!(json["groupBy"], serde_json::json!({"3": 1, "1": 1, "2": 1}));
    assert_eq!(
        json["emptyMin"],
        "Expected a non-empty collection. Collection: List()"
    );
}

#[test]
fn max_reports_the_failing_comparison_with_the_candidate_first() {
    let message = eval_fails("res = List(1, \"a\").max");
    assert!(
        message.contains("Operator `>` is not defined for operand types `String` and `Int`."),
        "{message}"
    );
}

#[test]
fn map_methods() {
    let json = eval(
        r#"
n = Map("a", 1).put("b", 2).remove("a")
o = Map("a", 1, "b", 2).map((k, v) -> Pair(k + "x", v)).keys.toList()
p = Map("a", 1).containsValue(1)
q = Map("a", 1).isNotEmpty
r = Map("a", 1, "b", 2).mapKeys((k, v) -> k + "k").keys.toList()
s = Map("a", 1).flatMap((k, v) -> Map(k, v, "z", 0)).length
t = Map("a", 1, "b", 2).fold(0, (acc, k, v) -> acc + v)
"#,
    );
    assert_eq!(json["n"], serde_json::json!({"b": 2}));
    assert_eq!(json["o"], serde_json::json!(["ax", "bx"]));
    assert_eq!(json["p"], true);
    assert_eq!(json["q"], true);
    assert_eq!(json["r"], serde_json::json!(["ak", "bk"]));
    assert_eq!(json["s"], 2);
    assert_eq!(json["t"], 3);
}

#[test]
fn bytes_and_charsets() {
    let json = eval(
        r#"
size = "abc".encodeToBytes("UTF-8").size.value
md5 = "abc".encodeToBytes("UTF-8").md5
decoded = "YWJj".base64DecodedBytes.decodeToString("UTF-8")
"#,
    );
    assert_eq!(json["size"], 3);
    assert_eq!(json["md5"], "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(json["decoded"], "abc");
}

#[test]
fn listing_members() {
    let json = eval(
        r#"
import "pkl:test"
distinct = new Listing { 1; 2 }.isDistinctBy((x) -> x)
first = test.catch(() -> new Listing {}.first)
single = test.catch(() -> new Listing { 1; 2 }.single)
"#,
    );
    assert_eq!(json["distinct"], true);
    assert_eq!(json["first"], "Expected a non-empty Listing.");
    assert_eq!(json["single"], "Expected a single-element Listing.");
}

#[test]
fn conflicting_member_predicate_and_index_is_rejected() {
    let json = eval(
        r#"
import "pkl:test"
hidden people = new Listing { new { age = 1 }; new { age = 2 } }
res = test.catch(() -> (people) {
  [0] { age = 4 }
  [[age == 1]] { age = 9 }
})
"#,
    );
    assert_eq!(json["res"], "Duplicate definition of member `0`.");
}
