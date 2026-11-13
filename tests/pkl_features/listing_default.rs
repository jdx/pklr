use super::*;

// Listing `default`, late-bound `this`, `super.default` and `getOrDefault`.
// Every expectation was compared with `pkl eval` 0.32.2.

#[test]
fn listing_default_amends_untyped_elements() {
    let v = eval("l = new Listing {\n  default { n = 1 }\n  new { m = 2 }\n}\n");
    assert_eq!(v["l"][0], serde_json::json!({"n": 1, "m": 2}));
}

#[test]
fn listing_default_lambda_gets_the_index() {
    let v = eval("l = new Listing {\n  default { i -> new Dynamic { idx = i } }\n  new {}\n}\n");
    assert_eq!(v["l"], serde_json::json!([[{"idx": 0}]]));
}

#[test]
fn listing_default_member_reads_back() {
    let v = eval("l = new Listing { default { n = 1 } }\nd = l.default.apply(0).n\n");
    assert_eq!(v["d"], 1);
}

#[test]
fn listing_get_or_default() {
    let v = eval("g = new Listing { 1 }.getOrDefault(0)\n");
    assert_eq!(v["g"], 1);
}

#[test]
fn listing_late_binds_this_when_amended() {
    let v = eval("a = new Listing { 1; this.length }\nb = (a) { 5 }\n");
    assert_eq!(v["a"], serde_json::json!([1, 2]));
    assert_eq!(v["b"], serde_json::json!([1, 3, 5]));
}
