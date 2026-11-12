use super::*;

// Every expectation below was compared with `pkl eval` 0.32.2.

#[test]
fn null_safe_access_reads_list_members() {
    let json = eval(
        "local l: List<Int>? = List(1)
local s: Set<Int>? = Set(1)
local li: Listing<Int>? = new Listing { 1 }
single = l?.single
length = l?.length
first = l?.first
isEmpty = l?.isEmpty
rest = l?.rest
lastIndex = l?.lastIndex
setSingle = s?.single
setLength = s?.length
listingLength = li?.length
listingIsEmpty = li?.isEmpty
mapped = l?.map((x) -> x + 1)
",
    );
    assert_eq!(
        json,
        serde_json::json!({
            "single": 1, "length": 1, "first": 1, "isEmpty": false,
            "rest": [], "lastIndex": 0, "setSingle": 1, "setLength": 1,
            "listingLength": 1, "listingIsEmpty": false, "mapped": [2],
        })
    );
}

#[test]
fn null_safe_access_on_a_null_list_is_null() {
    let json = eval(
        "local n: List<Int>? = null
local sn: Set<Int>? = null
local ln: Listing<Int>? = null
a = n?.single
b = n?.length
c = n?.first
d = n?.isEmpty
e = sn?.single
f = ln?.length
g = n?.map((x) -> x)
",
    );
    assert_eq!(
        json,
        serde_json::json!({
            "a": null, "b": null, "c": null, "d": null, "e": null, "f": null, "g": null,
        })
    );
}

#[test]
fn null_safe_access_still_reports_an_invalid_list_member() {
    let message = eval_fails("local e: List<Int>? = List()\nx = e?.single");
    assert!(
        message.contains("Expected a single-element collection"),
        "{message}"
    );
    assert_eq!(
        eval("local e: List<Int>? = List()\nx = e?.firstOrNull")["x"],
        serde_json::Value::Null
    );
}
