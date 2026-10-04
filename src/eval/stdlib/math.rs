//! The `pkl:math` module.

use super::fdlibm;
use super::numbers::as_f64;
use super::*;

/// The class name of the `pkl:math` module object.
pub(super) const MODULE: &str = "pkl.math";

/// The `pkl:math` module object with its constants. Its functions are
/// answered by [`call`].
pub(super) fn module() -> Value {
    let mut members = ObjectMap::default();
    let ints: [(&str, i64); 12] = [
        ("minInt", i64::MIN),
        ("minInt8", i64::from(i8::MIN)),
        ("minInt16", i64::from(i16::MIN)),
        ("minInt32", i64::from(i32::MIN)),
        ("maxInt", i64::MAX),
        ("maxInt8", i64::from(i8::MAX)),
        ("maxInt16", i64::from(i16::MAX)),
        ("maxInt32", i64::from(i32::MAX)),
        ("maxUInt", i64::MAX),
        ("maxUInt8", i64::from(u8::MAX)),
        ("maxUInt16", i64::from(u16::MAX)),
        ("maxUInt32", i64::from(u32::MAX)),
    ];
    for (name, value) in ints {
        members.insert(name.into(), Value::Int(value));
    }
    let floats = [
        ("minFiniteFloat", -f64::MAX),
        ("maxFiniteFloat", f64::MAX),
        // Java's `Double.MIN_VALUE`, the smallest positive subnormal.
        ("minPositiveFloat", f64::from_bits(1)),
        ("e", std::f64::consts::E),
        ("pi", std::f64::consts::PI),
    ];
    for (name, value) in floats {
        members.insert(name.into(), Value::Float(value));
    }
    typed_object(MODULE, members)
}

fn gcd(mut x: i64, mut y: i64) -> i64 {
    while y != 0 {
        (x, y) = (y, x % y);
    }
    x
}

fn number_arg(args: &[Value], index: usize) -> Result<f64> {
    as_f64(&args[index]).ok_or_else(|| type_mismatch("Number", &args[index]))
}

fn int_arg(args: &[Value], index: usize) -> Result<i64> {
    match &args[index] {
        Value::Int(n) => Ok(*n),
        other => Err(type_mismatch("Int", other)),
    }
}

/// Call the `pkl:math` function `name`, or return `None` if there is none.
pub(super) fn call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    // pkl uses Java's `StrictMath` (fdlibm), whose results can differ from
    // the platform's math library in the last bit.
    let unary: Option<fn(f64) -> f64> = match name {
        "exp" => Some(fdlibm::exp),
        "sqrt" => Some(f64::sqrt),
        "cbrt" => Some(fdlibm::cbrt),
        "log" => Some(fdlibm::log),
        "log2" => Some(|x| fdlibm::log(x) / fdlibm::log(2.0)),
        "log10" => Some(fdlibm::log10),
        "sin" => Some(fdlibm::sin),
        "cos" => Some(fdlibm::cos),
        "tan" => Some(fdlibm::tan),
        "asin" => Some(fdlibm::asin),
        "acos" => Some(fdlibm::acos),
        "atan" => Some(fdlibm::atan),
        _ => None,
    };
    if let Some(f) = unary {
        return Some(check_arity(args, 1).and_then(|()| Ok(Value::Float(f(number_arg(args, 0)?)))));
    }
    let arity = match name {
        "isPowerOfTwo" => 1,
        "atan2" | "gcd" | "lcm" | "min" | "max" => 2,
        _ => return None,
    };
    Some(check_arity(args, arity).and_then(|()| {
        Ok(match name {
            "atan2" => Value::Float(fdlibm::atan2(number_arg(args, 0)?, number_arg(args, 1)?)),
            "gcd" | "lcm" => {
                let (x, y) = (int_arg(args, 0)?, int_arg(args, 1)?);
                for n in [x, y] {
                    if n < 0 {
                        return Err(expected_positive(n));
                    }
                }
                if name == "gcd" {
                    Value::Int(gcd(x, y))
                } else if x == 0 || y == 0 {
                    Value::Int(0)
                } else {
                    (x / gcd(x, y))
                        .checked_mul(y)
                        .map(Value::Int)
                        .ok_or_else(|| Error::Eval("Integer overflow.".into()))?
                }
            }
            "isPowerOfTwo" => Value::Bool(match &args[0] {
                Value::Int(n) => *n > 0 && n & (n - 1) == 0,
                Value::Float(f) => {
                    // A finite positive power of two has only its implicit
                    // (or, when subnormal, a single) significand bit set.
                    *f > 0.0 && f.is_finite() && {
                        let bits = f.to_bits();
                        let significand = bits & ((1 << 52) - 1);
                        if bits >> 52 == 0 {
                            significand.is_power_of_two()
                        } else {
                            significand == 0
                        }
                    }
                }
                other => return Err(type_mismatch("Number", other)),
            }),
            _ => {
                let (a, b) = (&args[0], &args[1]);
                match (a, b) {
                    (Value::Int(x), Value::Int(y)) => {
                        Value::Int(if name == "min" { *x.min(y) } else { *x.max(y) })
                    }
                    _ => {
                        let (x, y) = (number_arg(args, 0)?, number_arg(args, 1)?);
                        Value::Float(java_min_max(name == "min", x, y))
                    }
                }
            }
        })
    }))
}

/// Java's `Math.min`/`max`: `NaN` wins, and `-0.0` is smaller than `0.0`.
fn java_min_max(min: bool, x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 && y == 0.0 {
        let negative = x.is_sign_negative();
        let pick_x = if min { negative } else { !negative };
        return if pick_x { x } else { y };
    }
    if min { x.min(y) } else { x.max(y) }
}
