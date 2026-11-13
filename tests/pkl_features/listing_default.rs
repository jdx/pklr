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

#[test]
fn listing_this_reads_settle_when_amended() {
    let v = eval("a = new Listing { 1; this[0]; this[1]; this[2] }\nb = (a) { [0] = 9 }\n");
    assert_eq!(v["b"], serde_json::json!([9, 9, 9, 9]));
}

#[test]
fn listing_default_sees_body_locals() {
    let v = eval(
        "l = new Listing { local n = 1; default { x = n }; new {} }\nd = l.default.apply(0).x\n",
    );
    assert_eq!(v["d"], 1);
}

#[test]
fn amended_listing_rebuilds_elements_with_the_final_default() {
    let v = eval(
        "a = new Listing { default = (i) -> new Dynamic { n = 1 }; new {} }\nb = (a) { default { n = 2 } }\n",
    );
    assert_eq!(v["b"], serde_json::json!([{"n": 2}]));
}

#[test]
fn replay_keeps_the_parent_default_for_super() {
    let v = eval(
        "a = new Listing { default { n = 1 }; new {} }\nb = (a) { super.default.apply(0) }\nc = (b) { default { n = 2 } }\n",
    );
    assert_eq!(v["c"], serde_json::json!([{"n": 2}, {"n": 1}]));
}

#[test]
fn conditional_element_gets_the_default() {
    let v = eval("l = new Listing { default { n = 1 }; if (true) new { m = 2 } else new {} }\n");
    assert_eq!(v["l"], serde_json::json!([{"n": 1, "m": 2}]));
}

#[test]
fn long_this_chain_settles_when_amended() {
    let v = eval(
        "a = new Listing { 1; this[0]; this[1]; this[2]; this[3]; this[4]; this[5]; this[6]; this[7]; this[8] }\nb = (a) { [0] = 9 }\n",
    );
    assert_eq!(v["b"], serde_json::json!(vec![9; 10]));
}
