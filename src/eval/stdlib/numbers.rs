//! `Int`, `Float` and `Boolean` members, and pkl's arithmetic and
//! comparison operators on numbers, durations and data sizes.

use super::fdlibm;
use super::render::{format_float, group_digits};
use super::units::{data_size_unit_arg, duration_unit_arg};
use super::*;
use crate::value::{DataSize, DataSizeUnit, Duration, DurationUnit};

fn integer_overflow() -> Error {
    Error::Eval("Integer overflow.".into())
}

const FLOAT_INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

fn float_to_int_error(x: f64) -> Error {
    if x.is_finite() {
        Error::Eval(format!(
            "Cannot convert Float `{}` to Int because it is too large.",
            format_float(x)
        ))
    } else {
        Error::Eval(format!(
            "Cannot convert non-finite Float `{}` to Int.",
            format_float(x)
        ))
    }
}

/// Java's `(long) x` after checking that `x` (already rounded toward zero)
/// fits, with pkl's errors for values that don't.
pub(super) fn float_to_int(x: f64) -> Result<i64> {
    // Every finite double in [-2^63, 2^63) converts exactly.
    let z = x.trunc();
    if z.is_finite() && (-FLOAT_INT_LIMIT..FLOAT_INT_LIMIT).contains(&z) {
        return Ok(z as i64);
    }
    Err(float_to_int_error(x))
}

/// A number as `f64`, or `None` for a non-number.
pub(super) fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// `Int(this.isBetween(lo, hi))`, the type of several number arguments.
fn int_between(value: i64, lo: i64, hi: i64) -> Result<i64> {
    if (lo..=hi).contains(&value) {
        return Ok(value);
    }
    Err(error_with_values(
        format!("Type constraint `this.isBetween({lo}, {hi})` violated."),
        &[("Value", &Value::Int(value))],
    ))
}

/// Java's `DecimalFormat` with `digits` fraction digits and no grouping.
/// Like Java, it rounds the shortest decimal form of `x` (half to even),
/// consulting the exact binary value only when that form ends in a tie.
fn to_fixed(x: f64, digits: usize) -> String {
    let shortest = format!("{:e}", x.abs());
    if let Some(integer) = java_integer_digits(x, &shortest) {
        let sign = if x < 0.0 { "-" } else { "" };
        let fraction = if digits > 0 {
            format!(".{}", "0".repeat(digits))
        } else {
            String::new()
        };
        return format!("{sign}{integer}{fraction}");
    }
    let significant_digits = shortest
        .split_once('e')
        .expect("`{:e}` has an exponent")
        .0
        .bytes()
        .filter(|digit| *digit != b'.')
        .count();
    let sci = if shortest_decimal_is_midpoint(x.abs(), &shortest) {
        // A shortest decimal exactly on a binary64 interval boundary leaves
        // both adjacent representations viable. FloatingDecimal continues
        // digit generation in that case; the extra digit selects the same
        // side of the interval as Java's DecimalFormat. This is deliberately
        // an interval test rather than a magnitude-specific precision hack.
        if significant_digits > 1 {
            format!("{:.16e}", x.abs())
        } else {
            format!("{:.15e}", x.abs())
        }
    } else {
        shortest
    };
    let (mantissa, exponent) = sci.split_once('e').expect("`{:e}` has an exponent");
    let exponent: i64 = exponent.parse().expect("`{:e}` exponent is an integer");
    let mut sig: Vec<u8> = mantissa.bytes().filter(|b| *b != b'.').collect();
    // `sig` holds 0.d1d2d3... * 10^point.
    let mut point = exponent + 1;
    let keep = point + digits as i64;
    if x != 0.0 && keep < sig.len() as i64 {
        let round_up = if keep < 0 {
            false
        } else {
            let dropped = &sig[keep as usize..];
            match dropped[0] {
                b'6'..=b'9' => true,
                b'5' if dropped[1..].iter().any(|d| *d != b'0') => true,
                b'5' => return format!("{x:.digits$}"),
                _ => false,
            }
        };
        sig.truncate(keep.max(0) as usize);
        if round_up {
            let mut i = sig.len();
            loop {
                if i == 0 {
                    sig.insert(0, b'1');
                    point += 1;
                    break;
                }
                i -= 1;
                if sig[i] == b'9' {
                    sig[i] = b'0';
                } else {
                    sig[i] += 1;
                    break;
                }
            }
        }
    }
    let digit_at = |i: i64| -> char {
        if i >= 0 && (i as usize) < sig.len() && x != 0.0 {
            sig[i as usize] as char
        } else {
            '0'
        }
    };
    let mut out = String::new();
    if x.is_sign_negative() {
        out.push('-');
    }
    if point <= 0 {
        out.push('0');
    } else {
        out.extend((0..point).map(digit_at));
    }
    if digits > 0 {
        out.push('.');
        out.extend((point..point + digits as i64).map(digit_at));
    }
    out
}

/// Whether Ryu's shortest decimal lies exactly halfway between `x` and an
/// adjacent binary64 value. Whole values up to 38 decimal digits fit in
/// `u128`; outside that range DecimalFormat's requested scale (0..20) makes
/// the shortest representation unambiguous for this formatter path.
fn shortest_decimal_is_midpoint(x: f64, shortest: &str) -> bool {
    if !x.is_finite() || x < FLOAT_INT_LIMIT {
        return false;
    }
    let Some((mantissa, exponent)) = shortest.split_once('e') else {
        return false;
    };
    let exponent: i32 = match exponent.parse() {
        Ok(exponent) => exponent,
        Err(_) => return false,
    };
    let digits = mantissa
        .bytes()
        .filter(|byte| *byte != b'.')
        .collect::<Vec<_>>();
    let fractional = mantissa.len() - mantissa.find('.').map_or(mantissa.len(), |dot| dot + 1);
    let Ok(significand) = std::str::from_utf8(&digits)
        .ok()
        .unwrap_or("")
        .parse::<u128>()
    else {
        return false;
    };
    let decimal_power = exponent - fractional as i32;
    if decimal_power < 0 || decimal_power as u32 >= 39 {
        return false;
    }
    let Some(decimal) = significand.checked_mul(10u128.pow(decimal_power as u32)) else {
        return false;
    };
    let bits = x.to_bits();
    let binary_exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let shift = binary_exponent - 52;
    if !(0..128).contains(&shift) {
        return false;
    }
    let significand = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let exact = (significand as u128) << shift;
    let half_ulp = 1u128 << (shift - 1);
    exact.abs_diff(decimal) == half_ulp
}

/// The digits Java's `FloatingDecimal` (behind `DecimalFormat`) produces for
/// a whole number below 2^63: the exact value, with the low digits that the
/// double cannot distinguish rounded away (`5274019902629789696.0` prints
/// as `5274019902629789700`).
fn java_integer_digits(x: f64, shortest: &str) -> Option<String> {
    // `insignificantDigitsForPow2` in OpenJDK's FloatingDecimal.
    const INSIGNIFICANT: [u32; 64] = [
        0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 8, 8, 8,
        9, 9, 9, 9, 10, 10, 10, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 14, 14, 14, 15, 15, 15, 15,
        16, 16, 16, 17, 17, 17, 18, 18, 18, 19,
    ];
    let abs = x.abs();
    if abs.fract() != 0.0 {
        return None;
    }
    if abs >= FLOAT_INT_LIMIT {
        if shortest_decimal_is_exact(abs, shortest) {
            return None;
        }
        let significant_digits = shortest
            .split_once('e')?
            .0
            .bytes()
            .filter(|digit| *digit != b'.')
            .count();
        let bits = abs.to_bits();
        let significand = bits & ((1u64 << 52) - 1);
        let decimal_power_of_two = shortest_significand_is_power_of_two(shortest);
        if significant_digits == 1
            || significand == 0
            || decimal_power_of_two
            || shortest_decimal_is_midpoint(abs, shortest)
        {
            return java_large_integer_digits(
                abs,
                significand == 0,
                decimal_power_of_two && significant_digits > 1,
            );
        }
        return None;
    }
    if abs < 1.0 {
        return None;
    }
    let bin_exp = ((abs.to_bits() >> 52) as i64) - 1023;
    let mut value = abs as u64;
    let p2 = bin_exp - 54;
    if p2 > 1 && INSIGNIFICANT[p2 as usize] > 0 {
        let insignificant = INSIGNIFICANT[p2 as usize];
        let pow10 = 10u64.pow(insignificant);
        let residue = value % pow10;
        value /= pow10;
        if residue >= pow10 / 2 {
            value += 1;
        }
        return Some(format!("{value}{}", "0".repeat(insignificant as usize)));
    }
    Some(value.to_string())
}

fn shortest_decimal_is_exact(x: f64, shortest: &str) -> bool {
    let Some((mantissa, exponent)) = shortest.split_once('e') else {
        return false;
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return false;
    };
    let digits = mantissa.replace('.', "");
    let Ok(significand) = digits.parse::<u128>() else {
        return false;
    };
    let fractional = mantissa.len() - mantissa.find('.').map_or(mantissa.len(), |dot| dot + 1);
    let decimal_power = exponent - fractional as i32;
    if decimal_power < 0 || decimal_power as u32 >= 39 {
        return false;
    }
    let Some(decimal) = significand.checked_mul(10u128.pow(decimal_power as u32)) else {
        return false;
    };
    let bits = x.to_bits();
    let shift = ((bits >> 52) & 0x7ff) as i32 - 1023 - 52;
    if !(0..128).contains(&shift) {
        return false;
    }
    let binary = ((bits & ((1u64 << 52) - 1)) | (1u64 << 52)) as u128;
    (binary << shift) == decimal
}

fn shortest_significand_is_power_of_two(shortest: &str) -> bool {
    let Some((mantissa, _)) = shortest.split_once('e') else {
        return false;
    };
    let digits = mantissa.replace('.', "");
    digits
        .parse::<u128>()
        .is_ok_and(|significand| significand.is_power_of_two())
}

/// The large-whole-number branch of Java's FloatingDecimal interval logic.
/// Above 2^63 a binary64 is integral. Its exact significand still fits in a
/// `u128` through exponent 127, enough to preserve the decimal stopping
/// interval instead of forcing an arbitrary scientific precision.
fn java_large_integer_digits(
    abs: f64,
    power_of_two: bool,
    coarse_decimal_interval: bool,
) -> Option<String> {
    let bits = abs.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let shift = exponent - 52;
    if !(0..=75).contains(&shift) {
        return None;
    }
    let significand = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let exact = (significand as u128) << shift;
    // The decimal stopping interval is one decimal decade no finer than the
    // binary ULP. Rounding the exact significand at that boundary reproduces
    // FloatingDecimal for large integral binary64 values, including powers
    // of two and their adjacent decimal midpoints.
    let mut insignificant = 0u32;
    let mut decade = 1u128;
    let ulp = 1u128 << shift;
    // A normal power of two is at the asymmetric end of its decimal interval:
    // FloatingDecimal uses the half-ULP margin for its stopping decade.
    let ulp = if power_of_two { ulp / 2 } else { ulp };
    while decade.checked_mul(10).is_some_and(|next| next <= ulp) {
        decade *= 10;
        insignificant += 1;
    }
    if coarse_decimal_interval {
        decade = decade.checked_mul(10)?;
        insignificant += 1;
    }
    if insignificant == 0 {
        return Some(exact.to_string());
    }
    let rounded = (exact / decade) + u128::from(exact % decade >= decade / 2);
    Some(format!("{rounded}{}", "0".repeat(insignificant as usize)))
}

/// `Long.toString(n, radix)`.
fn to_radix_string(n: i64, radix: u32) -> String {
    let mut magnitude = n.unsigned_abs();
    if magnitude == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while magnitude > 0 {
        let digit = (magnitude % u64::from(radix)) as u32;
        digits.push(char::from_digit(digit, radix).expect("digit is below radix"));
        magnitude /= u64::from(radix);
    }
    if n < 0 {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

/// The duration or data size `n.<unit>`.
fn unit_value(n: f64, name: &str) -> Option<Value> {
    if let Some(unit) = DurationUnit::parse(name) {
        return Some(Value::Duration(Duration::new(n, unit)));
    }
    DataSizeUnit::parse(name).map(|unit| Value::DataSize(DataSize::new(n, unit)))
}

pub(super) fn int_property(n: i64, name: &str) -> Option<Result<Value>> {
    if let Some(value) = unit_value(n as f64, name) {
        return Some(Ok(value));
    }
    Some(Ok(match name {
        "sign" => Value::Int(n.signum()),
        "abs" => return Some(n.checked_abs().map(Value::Int).ok_or_else(integer_overflow)),
        "ceil" | "floor" => Value::Int(n),
        "inv" => Value::Int(!n),
        "isPositive" => Value::Bool(n >= 0),
        "isFinite" => Value::Bool(true),
        "isInfinite" | "isNaN" => Value::Bool(false),
        "isEven" => Value::Bool(n & 1 == 0),
        "isOdd" => Value::Bool(n & 1 != 0),
        "isNonZero" => Value::Bool(n != 0),
        _ => return None,
    }))
}

pub(super) fn float_property(f: f64, name: &str) -> Option<Result<Value>> {
    if let Some(value) = unit_value(f, name) {
        return Some(Ok(value));
    }
    Some(Ok(match name {
        "sign" => Value::Float(if f == 0.0 || f.is_nan() {
            f
        } else {
            f.signum()
        }),
        "abs" => Value::Float(f.abs()),
        "ceil" => Value::Float(f.ceil()),
        "floor" => Value::Float(f.floor()),
        "isPositive" => Value::Bool(f >= 0.0),
        "isFinite" => Value::Bool(f.is_finite()),
        "isInfinite" => Value::Bool(f.is_infinite()),
        "isNaN" => Value::Bool(f.is_nan()),
        "isNonZero" => Value::Bool(f != 0.0),
        _ => return None,
    }))
}

/// Arity of the `Number` methods shared by `Int` and `Float`.
fn number_method_arity(name: &str) -> Option<usize> {
    Some(match name {
        "round" | "truncate" | "toInt" | "toFloat" => 0,
        "toFixed" | "toDuration" | "toDataSize" => 1,
        "isBetween" => 2,
        _ => return None,
    })
}

/// `isBetween(start, inclusiveEnd)` for numbers.
fn number_is_between(x: f64, n: Option<i64>, args: &Args<'_>) -> Result<Value> {
    let bound = |i| -> Result<(f64, Option<i64>)> {
        match args.value(i)? {
            Value::Int(m) => Ok((*m as f64, Some(*m))),
            Value::Float(f) => Ok((*f, None)),
            other => Err(type_mismatch("Number", other)),
        }
    };
    let (start, start_int) = bound(0)?;
    let (end, end_int) = bound(1)?;
    // Preserve an Int receiver's exact comparison with each Int bound. A
    // Float on one side only requires converting that comparison to doubles;
    // converting both bounds merely because either is a Float loses precision
    // for the other bound above 2^53.
    let after_start = match (n, start_int) {
        (Some(n), Some(start)) => start <= n,
        _ => start <= x,
    };
    let before_end = match (n, end_int) {
        (Some(n), Some(end)) => n <= end,
        _ => x <= end,
    };
    Ok(Value::Bool(after_start && before_end))
}

pub(super) fn int_method(n: i64, name: &str, args: &[Value]) -> Option<Result<Value>> {
    let arity = match name {
        "toChar" => 0,
        "toRadixString" | "shl" | "shr" | "ushr" | "and" | "or" | "xor" => 1,
        _ => number_method_arity(name)?,
    };
    Some(check_arity(args, arity).and_then(|()| {
        let a = Args { method: name, args };
        Ok(match name {
            "round" | "truncate" | "toInt" => Value::Int(n),
            "toFloat" => Value::Float(n as f64),
            "toFixed" => {
                let digits = int_between(a.int(0)?, 0, 20)? as usize;
                if digits == 0 {
                    Value::String(n.to_string().into())
                } else {
                    Value::String(format!("{n}.{}", "0".repeat(digits)).into())
                }
            }
            "toDuration" => {
                Value::Duration(Duration::new(n as f64, duration_unit_arg(a.value(0)?)?))
            }
            "toDataSize" => {
                Value::DataSize(DataSize::new(n as f64, data_size_unit_arg(a.value(0)?)?))
            }
            "isBetween" => number_is_between(n as f64, Some(n), &a)?,
            "toRadixString" => {
                let radix = int_between(a.int(0)?, 2, 36)? as u32;
                Value::String(to_radix_string(n, radix).into())
            }
            "shl" => Value::Int(n.wrapping_shl(a.int(0)? as u32)),
            "shr" => Value::Int(n.wrapping_shr(a.int(0)? as u32)),
            "ushr" => Value::Int((n as u64).wrapping_shr(a.int(0)? as u32) as i64),
            "and" => Value::Int(n & a.int(0)?),
            "or" => Value::Int(n | a.int(0)?),
            "xor" => Value::Int(n ^ a.int(0)?),
            "toChar" => match u32::try_from(n).ok().and_then(char::from_u32) {
                Some(c) => Value::String(c.to_string().into()),
                None => {
                    return Err(Error::Eval(format!(
                        "Decimal `{}` is not a valid Unicode code point.",
                        group_digits(n)
                    )));
                }
            },
            _ => unreachable!("arity table covers {name}"),
        })
    }))
}

pub(super) fn float_method(f: f64, name: &str, args: &[Value]) -> Option<Result<Value>> {
    let arity = number_method_arity(name)?;
    Some(check_arity(args, arity).and_then(|()| {
        let a = Args { method: name, args };
        Ok(match name {
            "round" => Value::Float(f.round_ties_even()),
            "truncate" => Value::Float(f.trunc()),
            "toInt" => Value::Int(float_to_int(f)?),
            "toFloat" => Value::Float(f),
            "toFixed" => {
                let digits = int_between(a.int(0)?, 0, 20)? as usize;
                Value::String(if f.is_finite() {
                    to_fixed(f, digits).into()
                } else {
                    format_float(f).into()
                })
            }
            "toDuration" => Value::Duration(Duration::new(f, duration_unit_arg(a.value(0)?)?)),
            "toDataSize" => Value::DataSize(DataSize::new(f, data_size_unit_arg(a.value(0)?)?)),
            "isBetween" => number_is_between(f, None, &a)?,
            _ => unreachable!("arity table covers {name}"),
        })
    }))
}

pub(super) fn bool_method(b: bool, name: &str, args: &[Value]) -> Option<Result<Value>> {
    if !matches!(name, "xor" | "implies") {
        return None;
    }
    Some(check_arity(args, 1).and_then(|()| {
        let other = match &args[0] {
            Value::Bool(other) => *other,
            other => return Err(type_mismatch("Boolean", other)),
        };
        Ok(Value::Bool(if name == "xor" {
            b ^ other
        } else {
            !b || other
        }))
    }))
}

/// Java's `Double.compare`: `-0.0` sorts before `0.0` and `NaN` after
/// everything.
fn java_double_compare(a: f64, b: f64) -> std::cmp::Ordering {
    match a.partial_cmp(&b) {
        Some(std::cmp::Ordering::Equal) if a == 0.0 => {
            a.is_sign_positive().cmp(&b.is_sign_positive())
        }
        Some(ordering) => ordering,
        None => a.is_nan().cmp(&b.is_nan()),
    }
}

/// Two durations' values in the larger of their units, as pkl compares and
/// combines them, and that unit.
fn common_durations(a: &Duration, b: &Duration) -> (f64, f64, DurationUnit) {
    if a.unit <= b.unit {
        (a.value_in(b.unit), b.value, b.unit)
    } else {
        (a.value, b.value_in(a.unit), a.unit)
    }
}

fn common_data_sizes(a: &DataSize, b: &DataSize) -> (f64, f64, DataSizeUnit) {
    if a.unit <= b.unit {
        (a.value_in(b.unit), b.value, b.unit)
    } else {
        (a.value, b.value_in(a.unit), a.unit)
    }
}

/// `x ~/ y` for floats: the quotient rounded toward zero, as an `Int`.
fn truncating_divide(x: f64, y: f64) -> Result<i64> {
    let quotient = x / y;
    let truncated = quotient.trunc();
    if truncated.is_finite() && (-FLOAT_INT_LIMIT..FLOAT_INT_LIMIT).contains(&truncated) {
        Ok(truncated as i64)
    } else {
        // pkl reports the dividend rather than the overflowing quotient.
        Err(float_to_int_error(x))
    }
}

/// `x ** y` for Ints with a non-negative exponent, failing on overflow.
fn int_pow(x: i64, y: i64) -> Result<i64> {
    if y > i64::from(i32::MAX) {
        return match x {
            0 | 1 => Ok(x),
            -1 => Ok(if y % 2 == 0 { 1 } else { -1 }),
            _ => Err(integer_overflow()),
        };
    }
    x.checked_pow(y as u32).ok_or_else(integer_overflow)
}

fn is_arithmetic_operand(value: &Value) -> bool {
    matches!(
        value,
        Value::Int(_) | Value::Float(_) | Value::Duration(_) | Value::DataSize(_)
    )
}

/// The symbol pkl uses for `op` in error messages.
fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::IntDiv => "~/",
        BinOp::Mod => "%",
        BinOp::Pow => "**",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        _ => "?",
    }
}

/// `l op r` when either operand is a number, duration or data size, or
/// `None` to leave other operands to the caller.
pub(crate) fn binary_op(op: BinOp, l: &Value, r: &Value) -> Option<Result<Value>> {
    use std::cmp::Ordering;
    if !matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::IntDiv
            | BinOp::Mod
            | BinOp::Pow
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
    ) || !(is_arithmetic_operand(l) || is_arithmetic_operand(r))
    {
        return None;
    }
    let undefined = || Err(operator_not_defined(op_symbol(op), l, r));
    let compare = |ordering: Option<Ordering>| -> Result<Value> {
        Ok(Value::Bool(match (op, ordering) {
            (_, None) => false,
            (BinOp::Lt, Some(o)) => o == Ordering::Less,
            (BinOp::Le, Some(o)) => o != Ordering::Greater,
            (BinOp::Gt, Some(o)) => o == Ordering::Greater,
            _ => ordering != Some(Ordering::Less),
        }))
    };
    let duration = |value, unit| Ok(Value::Duration(Duration::new(value, unit)));
    let data_size = |value, unit| Ok(Value::DataSize(DataSize::new(value, unit)));
    Some(match (l, r) {
        (Value::Int(a), Value::Int(b)) => {
            let (a, b) = (*a, *b);
            match op {
                BinOp::Add => a
                    .checked_add(b)
                    .map(Value::Int)
                    .ok_or_else(integer_overflow),
                BinOp::Sub => a
                    .checked_sub(b)
                    .map(Value::Int)
                    .ok_or_else(integer_overflow),
                BinOp::Mul => a
                    .checked_mul(b)
                    .map(Value::Int)
                    .ok_or_else(integer_overflow),
                BinOp::Div => Ok(Value::Float(a as f64 / b as f64)),
                BinOp::IntDiv => {
                    if b == 0 {
                        Err(Error::Eval("Division by zero.".into()))
                    } else {
                        a.checked_div(b)
                            .map(Value::Int)
                            .ok_or_else(integer_overflow)
                    }
                }
                BinOp::Mod => {
                    if b == 0 {
                        Err(Error::Eval("Division by zero.".into()))
                    } else {
                        Ok(Value::Int(a.wrapping_rem(b)))
                    }
                }
                BinOp::Pow if b >= 0 => int_pow(a, b).map(Value::Int),
                BinOp::Pow => Ok(Value::Float(fdlibm::pow(a as f64, b as f64))),
                _ => compare(Some(a.cmp(&b))),
            }
        }
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (a, b) = (as_f64(l).expect("number"), as_f64(r).expect("number"));
            match op {
                BinOp::Add => Ok(Value::Float(a + b)),
                BinOp::Sub => Ok(Value::Float(a - b)),
                BinOp::Mul => Ok(Value::Float(a * b)),
                BinOp::Div => Ok(Value::Float(a / b)),
                BinOp::IntDiv => truncating_divide(a, b).map(Value::Int),
                BinOp::Mod => Ok(Value::Float(a % b)),
                BinOp::Pow => Ok(Value::Float(fdlibm::pow(a, b))),
                _ => compare(a.partial_cmp(&b)),
            }
        }
        (Value::Duration(a), Value::Duration(b)) => {
            let (x, y, unit) = common_durations(a, b);
            match op {
                BinOp::Add => duration(x + y, unit),
                BinOp::Sub => duration(x - y, unit),
                BinOp::Div => Ok(Value::Float(x / y)),
                BinOp::IntDiv => truncating_divide(x, y).map(Value::Int),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    compare(Some(java_double_compare(x, y)))
                }
                _ => undefined(),
            }
        }
        (Value::DataSize(a), Value::DataSize(b)) => {
            let (x, y, unit) = common_data_sizes(a, b);
            match op {
                BinOp::Add => data_size(x + y, unit),
                BinOp::Sub => data_size(x - y, unit),
                BinOp::Div => Ok(Value::Float(x / y)),
                BinOp::IntDiv => truncating_divide(x, y).map(Value::Int),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    compare(Some(java_double_compare(x, y)))
                }
                _ => undefined(),
            }
        }
        (Value::Duration(d), Value::Int(_) | Value::Float(_)) => {
            let n = as_f64(r).expect("number");
            match op {
                BinOp::Mul => duration(d.value * n, d.unit),
                BinOp::Div => duration(d.value / n, d.unit),
                BinOp::IntDiv => {
                    truncating_divide(d.value, n).and_then(|q| duration(q as f64, d.unit))
                }
                BinOp::Mod => duration(d.value % n, d.unit),
                BinOp::Pow => duration(fdlibm::pow(d.value, n), d.unit),
                _ => undefined(),
            }
        }
        (Value::DataSize(d), Value::Int(_) | Value::Float(_)) => {
            let n = as_f64(r).expect("number");
            match op {
                BinOp::Mul => data_size(d.value * n, d.unit),
                BinOp::Div => data_size(d.value / n, d.unit),
                BinOp::IntDiv => {
                    truncating_divide(d.value, n).and_then(|q| data_size(q as f64, d.unit))
                }
                BinOp::Mod => data_size(d.value % n, d.unit),
                BinOp::Pow => data_size(fdlibm::pow(d.value, n), d.unit),
                _ => undefined(),
            }
        }
        (Value::Int(_) | Value::Float(_), Value::Duration(d)) if op == BinOp::Mul => {
            duration(as_f64(l).expect("number") * d.value, d.unit)
        }
        (Value::Int(_) | Value::Float(_), Value::DataSize(d)) if op == BinOp::Mul => {
            data_size(as_f64(l).expect("number") * d.value, d.unit)
        }
        _ => undefined(),
    })
}

/// The left operand of `&&` or `||`, which must be a `Boolean`.
pub(crate) fn logical_left(op: BinOp, left: &Value) -> Result<bool> {
    match left {
        Value::Bool(b) => Ok(*b),
        other => Err(error_with_values(
            format!(
                "Operator `{}` is not defined for left operand type `{}`.",
                logical_symbol(op),
                other.type_name()
            ),
            &[("Left operand", other)],
        )),
    }
}

/// The right operand of `&&` or `||`, evaluated when the left did not
/// decide the result.
pub(crate) fn logical_right(op: BinOp, left: &Value, right: &Value) -> Result<bool> {
    match right {
        Value::Bool(b) => Ok(*b),
        other => Err(operator_not_defined(logical_symbol(op), left, other)),
    }
}

fn logical_symbol(op: BinOp) -> &'static str {
    if matches!(op, BinOp::And) { "&&" } else { "||" }
}

/// `!value`, defined only for a `Boolean`.
pub(crate) fn logical_not(value: &Value) -> Result<Value> {
    match value {
        Value::Bool(b) => Ok(Value::Bool(!b)),
        other => Err(error_with_values(
            format!(
                "Operator `!` is not defined for operand type `{}`.",
                other.type_name()
            ),
            &[("Operand", other)],
        )),
    }
}

/// `-value` for a number, duration or data size.
pub(crate) fn negate(value: &Value) -> Option<Result<Value>> {
    Some(match value {
        Value::Int(n) => n.checked_neg().map(Value::Int).ok_or_else(integer_overflow),
        Value::Float(f) => Ok(Value::Float(-f)),
        Value::Duration(d) => Ok(Value::Duration(Duration::new(-d.value, d.unit))),
        Value::DataSize(d) => Ok(Value::DataSize(DataSize::new(-d.value, d.unit))),
        _ => return None,
    })
}

/// pkl's `==` for durations and data sizes: equal amounts in any units.
pub(crate) fn units_equal(a: &Value, b: &Value) -> Option<bool> {
    match (a, b) {
        (Value::Duration(a), Value::Duration(b)) => {
            Some(a.value_in(DurationUnit::Nanos) == b.value_in(DurationUnit::Nanos))
        }
        (Value::DataSize(a), Value::DataSize(b)) => {
            Some(a.value_in(DataSizeUnit::Bytes) == b.value_in(DataSizeUnit::Bytes))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radix_strings_match_java() {
        assert_eq!(to_radix_string(255, 16), "ff");
        assert_eq!(to_radix_string(-255, 2), "-11111111");
        assert_eq!(to_radix_string(0, 36), "0");
        assert_eq!(to_radix_string(i64::MIN, 16), "-8000000000000000");
    }

    #[test]
    fn to_fixed_rounds_half_even_on_exact_values() {
        assert_eq!(to_fixed(0.125, 2), "0.12");
        assert_eq!(to_fixed(0.375, 2), "0.38");
        assert_eq!(to_fixed(0.15, 1), "0.1");
        assert_eq!(to_fixed(2.5, 0), "2");
        assert_eq!(to_fixed(-0.001, 2), "-0.00");
        assert_eq!(to_fixed(123456789.12345679, 9), "123456789.123456790");
        assert_eq!(to_fixed(0.004, 2), "0.00");
        assert_eq!(to_fixed(0.996, 2), "1.00");
        assert_eq!(to_fixed(99.5, 0), "100");
        assert_eq!(to_fixed(5.27401990262979e18, 1), "5274019902629789700.0");
        assert_eq!(to_fixed(-9.740362900988539e16, 0), "-97403629009885392");
        assert_eq!(to_fixed(0.0, 3), "0.000");
        assert_eq!(to_fixed(1.0e-10, 3), "0.000");
    }

    #[test]
    fn to_fixed_matches_pkl_for_large_binary64_values() {
        for (value, expected) in [
            (9_223_372_036_854_776_000.0, "9223372036854776000"),
            (1e18, "1000000000000000000"),
            (1e19, "10000000000000000000"),
            (9_223_372_036_854_775_000.0, "9223372036854774800"),
            (1.0000000000000002e19, "10000000000000002000"),
            (9_223_372_036_854_776_000.0, "9223372036854776000"),
            (9_223_372_036_854_778_000.0, "9223372036854778000"),
            (1e20, "100000000000000000000"),
            (1e21, "1000000000000000000000"),
            (1e22, "10000000000000000000000"),
            (1e23, "99999999999999990000000"),
            (1.08e23, "108000000000000010000000"),
            (1.16e23, "115999999999999990000000"),
            (1.24e23, "124000000000000010000000"),
            (1.03e23, "103000000000000000000000"),
            (2e23, "199999999999999980000000"),
            (5.902958103587057e20, "590295810358705650000"),
            (1.048576e29, "104857600000000000000000000000"),
            (1.8014398509481984e19, "18014398509481984000"),
        ] {
            assert_eq!(to_fixed(value, 0), expected, "{value:e}");
            assert_eq!(to_fixed(-value, 0), format!("-{expected}"), "-{value:e}");
        }
    }
}
