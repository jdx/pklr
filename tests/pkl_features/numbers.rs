use super::*;

// ============================================================
// Int, Float, Boolean, Duration, DataSize and pkl:math
// ============================================================

#[test]
fn int_properties_and_methods() {
    let json = eval(
        r##"
sign = List((-5).sign, 0.sign, 7.sign)
abs = (-5).abs
even = 4.isEven && !3.isEven && 3.isOdd
positive = List(0.isPositive, (-1).isPositive)
between = List(3.isBetween(2, 4), 3.isBetween(3.5, 4), 3.isBetween(2.5, 3))
radix = List(255.toRadixString(16), (-5).toRadixString(2))
bits = List(1.shl(4), (-16).shr(2), (-16).ushr(60), 12.and(10), 12.or(10), 12.xor(10), 5.inv)
toFixed = List(5.toFixed(2), 5.toFixed(0))
char = 128512.toChar()
float = 3.toFloat()
"##,
    );
    assert_eq!(json["sign"], serde_json::json!([-1, 0, 1]));
    assert_eq!(json["abs"], 5);
    assert_eq!(json["even"], true);
    assert_eq!(json["positive"], serde_json::json!([true, false]));
    assert_eq!(json["between"], serde_json::json!([true, false, true]));
    assert_eq!(json["radix"], serde_json::json!(["ff", "-101"]));
    assert_eq!(json["bits"], serde_json::json!([16, -4, 15, 8, 14, 6, -6]));
    assert_eq!(json["toFixed"], serde_json::json!(["5.00", "5"]));
    assert_eq!(json["char"], "😀");
    assert_eq!(json["float"], 3.0);
}

#[test]
fn int_errors_match_pkl() {
    assert!(
        eval_fails(r#"x = 1114112.toChar()"#)
            .contains("Decimal `1,114,112` is not a valid Unicode code point.")
    );
    assert!(
        eval_fails(r#"x = 5.toFixed(21)"#)
            .contains("Type constraint `this.isBetween(0, 20)` violated.\nValue: 21")
    );
    assert!(eval_fails(r#"x = 9223372036854775807 + 1"#).contains("Integer overflow."));
}

#[test]
fn float_properties_and_methods() {
    let json = eval(
        r##"
round = List(2.5.round(), 3.5.round(), (-2.5).round())
truncate = (-2.7).truncate()
toInt = List(2.9.toInt(), (-2.9).toInt())
ceilFloor = List(2.1.ceil, 2.9.floor)
toFixed = List(0.125.toFixed(2), 1.005.toFixed(2), 123456789.12345679.toFixed(9), (-0.001).toFixed(2))
nan = List((0.0 / 0.0).isNaN, (1.0 / 0.0).isInfinite, 1.5.isFinite)
strings = List(1.0.toString(), 1e21.toString(), 0.0001.toString(), (-0.0).toString())
"##,
    );
    assert_eq!(json["round"], serde_json::json!([2.0, 4.0, -2.0]));
    assert_eq!(json["truncate"], -2.0);
    assert_eq!(json["toInt"], serde_json::json!([2, -2]));
    assert_eq!(json["ceilFloor"], serde_json::json!([3.0, 2.0]));
    assert_eq!(
        json["toFixed"],
        serde_json::json!(["0.12", "1.00", "123456789.123456790", "-0.00"])
    );
    assert_eq!(json["nan"], serde_json::json!([true, true, true]));
    assert_eq!(
        json["strings"],
        serde_json::json!(["1.0", "1.0E21", "1.0E-4", "-0.0"])
    );
    assert!(
        eval_fails(r#"x = 1e300.toInt()"#)
            .contains("Cannot convert Float `1.0E300` to Int because it is too large.")
    );
}

#[test]
fn arithmetic_follows_pkl() {
    let json = eval(
        r##"
div = 7 / 2
truncDiv = List(7 ~/ 2, -7 ~/ 2, 7.5 ~/ 2)
rem = List(-7 % 3, 7.5 % 2)
pow = List(2 ** 10, 2 ** -2, 2.0 ** 0.5)
compare = List(1 < 1.5, 2.0 >= 2, (0.0 / 0.0) <= 1)
"##,
    );
    assert_eq!(json["div"], 3.5);
    assert_eq!(json["truncDiv"], serde_json::json!([3, -3, 3]));
    assert_eq!(json["rem"], serde_json::json!([-1, 1.5]));
    assert_eq!(
        json["pow"],
        serde_json::json!([1024, 0.25, std::f64::consts::SQRT_2])
    );
    assert_eq!(json["compare"], serde_json::json!([true, true, false]));
}

#[test]
fn logical_operators_require_booleans() {
    assert!(
        eval_fails(r#"x = 1 && true"#)
            .contains("Operator `&&` is not defined for left operand type `Int`.\nLeft operand: 1")
    );
    assert!(
        eval_fails(r#"x = false || "a""#)
            .contains("Operator `||` is not defined for operand types `Boolean` and `String`.")
    );
    assert!(
        eval_fails(r#"x = !1"#).contains("Operator `!` is not defined for operand type `Int`.")
    );
    let json = eval(r#"x = List(true.xor(false), true.implies(false), false.implies(true))"#);
    assert_eq!(json["x"], serde_json::json!([true, false, true]));
}

#[test]
fn durations_convert_and_compare() {
    let json = eval(
        r##"
value = List(5.min.value, 2.5.s.value, 2.0.h.value)
unit = 5.min.unit
sum = (1.min + 30.s).toString()
diff = (1.h - 90.min).toString()
scaled = List((2 * 3.s).toString(), (3.s / 2).toString(), (7.s ~/ 2).toString(), (7.s % 2).toString())
ratio = 1.h / 30.min
equal = List(1.min == 60.s, 1.min == 61.s, 1.min < 61.s, 1.d > 23.h)
converted = List(90.s.toUnit("min").toString(), 1.5.h.toUnit("min").toString())
iso = List(6.6.h.isoString, 2000.5.ms.isoString, 0.s.isoString, (-10.001).s.isoString)
between = 3.min.isBetween(120.s, 180.s)
negated = (-(5.min)).toString()
fromInt = 3.toDuration("ms").toString()
"##,
    );
    assert_eq!(json["value"], serde_json::json!([5, 2.5, 2]));
    assert_eq!(json["unit"], "min");
    assert_eq!(json["sum"], "1.5.min");
    assert_eq!(json["diff"], "-0.5.h");
    assert_eq!(
        json["scaled"],
        serde_json::json!(["6.s", "1.5.s", "3.s", "1.s"])
    );
    assert_eq!(json["ratio"], 2.0);
    assert_eq!(json["equal"], serde_json::json!([true, false, true, true]));
    assert_eq!(json["converted"], serde_json::json!(["1.5.min", "90.min"]));
    assert_eq!(
        json["iso"],
        serde_json::json!(["PT6H36M", "PT2.0005S", "PT0S", "-PT10.001S"])
    );
    assert_eq!(json["between"], true);
    assert_eq!(json["negated"], "-5.min");
    assert_eq!(json["fromInt"], "3.ms");
    assert!(
        eval_fails(r#"x = 5.toDuration("x")"#).contains(
            r#"Expected value of type `"ns"|"us"|"ms"|"s"|"min"|"h"|"d"`, but got `"x"`."#
        )
    );
    assert!(
        eval_fails(r#"x = 1.min + 1"#)
            .contains("Operator `+` is not defined for operand types `Duration` and `Int`.")
    );
}

#[test]
fn data_sizes_convert_and_compare() {
    let json = eval(
        r##"
sum = (1.kb + 500.b).toString()
binary = List(1.kib.isBinaryUnit, 1.kb.isBinaryUnit, 1.b.isDecimalUnit)
toBinary = 2048.kb.toBinaryUnit().toString()
toDecimal = 1.mib.toDecimalUnit().toString()
equal = List(1.kb == 1000.b, 1.kib == 1000.b, 1.gb > 1.gib)
value = 1.5.mb.value
"##,
    );
    assert_eq!(json["sum"], "1.5.kb");
    assert_eq!(json["binary"], serde_json::json!([true, false, true]));
    assert_eq!(json["toBinary"], "2000.kib");
    assert_eq!(json["toDecimal"], "1.048576.mb");
    assert_eq!(json["equal"], serde_json::json!([true, false, false]));
    assert_eq!(json["value"], 1.5);
}

#[test]
fn durations_cannot_be_rendered_as_json() {
    let temp = TestTempDir::new("pklr_test_duration_json");
    let path = temp.path().join("test.pkl");
    std::fs::write(&path, "timeout = 5.min\n").unwrap();
    let error = pklr::eval_to_json(&path).unwrap_err().to_string();
    assert!(
        error.contains("Cannot render value of type `Duration` as JSON.\nValue: 5.min"),
        "{error}"
    );
}

#[test]
fn math_module() {
    let json = eval(
        r##"
import "pkl:math"
consts = List(math.maxInt, math.minInt32, math.maxUInt8, math.pi, math.e)
fns = List(math.sqrt(16), math.cbrt(27), math.log10(1000), math.exp(0), math.sin(0))
ints = List(math.gcd(12, 18), math.lcm(4, 6), math.min(3, 5), math.max(2.5, 1))
pow2 = List(math.isPowerOfTwo(64), math.isPowerOfTwo(0.25), math.isPowerOfTwo(6))
"##,
    );
    assert_eq!(
        json["consts"],
        serde_json::json!([
            i64::MAX,
            i32::MIN,
            255,
            std::f64::consts::PI,
            std::f64::consts::E
        ])
    );
    assert_eq!(json["fns"], serde_json::json!([4.0, 3.0, 3.0, 1.0, 0.0]));
    assert_eq!(json["ints"], serde_json::json!([6, 12, 3, 2.5]));
    assert_eq!(json["pow2"], serde_json::json!([true, true, false]));
    assert!(
        eval_fails("import \"pkl:math\"\nx = math.gcd(-4, 6)")
            .contains("Expected a positive number, but got `-4`.")
    );
}
