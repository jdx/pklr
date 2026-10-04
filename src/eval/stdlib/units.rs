//! `Duration` and `DataSize` members.

use super::render::{is_mathematical_integer, render_value};
use super::*;
use crate::value::{DataSize, DataSizeUnit, Duration, DurationUnit};

/// The type error pkl reports for a string that is not one of a unit
/// typealias's literals.
fn unit_mismatch<'a>(symbols: impl Iterator<Item = &'a str>, actual: &Value) -> Error {
    let expected = symbols
        .map(|symbol| format!("\"{symbol}\""))
        .collect::<Vec<_>>()
        .join("|");
    match actual {
        Value::String(_) => Error::Eval(format!(
            "Expected value of type `{expected}`, but got `{}`.",
            render_value(actual)
        )),
        _ => type_mismatch(&expected, actual),
    }
}

/// A `DurationUnit` argument.
pub(super) fn duration_unit_arg(value: &Value) -> Result<DurationUnit> {
    match value {
        Value::String(s) => DurationUnit::parse(s),
        _ => None,
    }
    .ok_or_else(|| unit_mismatch(DurationUnit::ALL.iter().map(|u| u.symbol()), value))
}

/// A `DataSizeUnit` argument.
pub(super) fn data_size_unit_arg(value: &Value) -> Result<DataSizeUnit> {
    match value {
        Value::String(s) => DataSizeUnit::parse(s),
        _ => None,
    }
    .ok_or_else(|| unit_mismatch(DataSizeUnit::ALL.iter().map(|u| u.symbol()), value))
}

/// A duration's or data size's `value`: an `Int` when it is a whole number.
fn number_value(value: f64) -> Value {
    if is_mathematical_integer(value) {
        Value::Int(value as i64)
    } else {
        Value::Float(value)
    }
}

/// `Duration.isoString`, ported from pkl's `DurationUtils.toIsoString`.
fn iso_string(duration: &Duration) -> Result<String> {
    let total_seconds = duration.value * (duration.unit.nanos() / 1e9);
    if !total_seconds.is_finite() {
        return Err(Error::Eval(format!(
            "Cannot convert duration `{}` to ISO 8601 duration.",
            render_value(&Value::Duration(*duration))
        )));
    }
    let absolute = total_seconds.abs();
    let hours = (absolute / 3600.0) as i64;
    let minutes = (absolute / 60.0) as i64 % 60;
    let seconds = (absolute % 60.0) as i64;
    let nanos = (absolute * 1_000_000_000.0 - absolute.floor() * 1_000_000_000.0) as i64;
    let mut out = String::new();
    if total_seconds < 0.0 {
        out.push('-');
    }
    out.push_str("PT");
    if hours != 0 {
        out.push_str(&format!("{hours}H"));
    }
    if minutes != 0 {
        out.push_str(&format!("{minutes}M"));
    }
    if seconds != 0 || nanos != 0 || total_seconds == 0.0 {
        out.push_str(&seconds.to_string());
        if nanos != 0 {
            out.push_str(format!(".{nanos:09}").trim_end_matches('0'));
        }
        out.push('S');
    }
    Ok(out)
}

pub(super) fn duration_property(d: &Duration, name: &str) -> Option<Result<Value>> {
    Some(Ok(match name {
        "value" => number_value(d.value),
        "unit" => Value::String(d.unit.symbol().into()),
        "isPositive" => Value::Bool(d.value >= 0.0),
        "isoString" => return Some(iso_string(d).map(|s| Value::String(s.into()))),
        _ => return None,
    }))
}

pub(super) fn data_size_property(d: &DataSize, name: &str) -> Option<Result<Value>> {
    let ordinal = DataSizeUnit::ALL
        .iter()
        .position(|unit| *unit == d.unit)
        .expect("ALL lists every unit");
    Some(Ok(match name {
        "value" => number_value(d.value),
        "unit" => Value::String(d.unit.symbol().into()),
        "isPositive" => Value::Bool(d.value >= 0.0),
        "isBinaryUnit" => Value::Bool(ordinal % 2 == 0),
        "isDecimalUnit" => Value::Bool(ordinal == 0 || ordinal % 2 == 1),
        _ => return None,
    }))
}

pub(super) fn duration_method(d: &Duration, name: &str, args: &[Value]) -> Option<Result<Value>> {
    let arity = match name {
        "toUnit" => 1,
        "isBetween" => 2,
        _ => return None,
    };
    Some(check_arity(args, arity).and_then(|()| {
        Ok(match name {
            "toUnit" => {
                let unit = duration_unit_arg(&args[0])?;
                Value::Duration(Duration {
                    value: d.value_in(unit),
                    unit,
                })
            }
            _ => {
                let this = Value::Duration(*d);
                for bound in args {
                    if !matches!(bound, Value::Duration(_)) {
                        return Err(type_mismatch("Duration", bound));
                    }
                }
                is_between(&this, &args[0], &args[1])?
            }
        })
    }))
}

pub(super) fn data_size_method(d: &DataSize, name: &str, args: &[Value]) -> Option<Result<Value>> {
    let arity = match name {
        "toBinaryUnit" | "toDecimalUnit" => 0,
        "toUnit" => 1,
        "isBetween" => 2,
        _ => return None,
    };
    Some(check_arity(args, arity).and_then(|()| {
        let convert = |unit| {
            Value::DataSize(DataSize {
                value: d.value_in(unit),
                unit,
            })
        };
        use DataSizeUnit::*;
        Ok(match name {
            "toUnit" => convert(data_size_unit_arg(&args[0])?),
            "toBinaryUnit" => match d.unit {
                Kilobytes => convert(Kibibytes),
                Megabytes => convert(Mebibytes),
                Gigabytes => convert(Gibibytes),
                Terabytes => convert(Tebibytes),
                Petabytes => convert(Pebibytes),
                _ => Value::DataSize(*d),
            },
            "toDecimalUnit" => match d.unit {
                Kibibytes => convert(Kilobytes),
                Mebibytes => convert(Megabytes),
                Gibibytes => convert(Gigabytes),
                Tebibytes => convert(Terabytes),
                Pebibytes => convert(Petabytes),
                _ => Value::DataSize(*d),
            },
            _ => {
                let this = Value::DataSize(*d);
                for bound in args {
                    if !matches!(bound, Value::DataSize(_)) {
                        return Err(type_mismatch("DataSize", bound));
                    }
                }
                is_between(&this, &args[0], &args[1])?
            }
        })
    }))
}

/// `start <= this && this <= end` with pkl's unit-aware comparison.
fn is_between(this: &Value, start: &Value, end: &Value) -> Result<Value> {
    let at_least = numbers::binary_op(BinOp::Ge, this, start).expect("units compare")?;
    let at_most = numbers::binary_op(BinOp::Le, this, end).expect("units compare")?;
    Ok(Value::Bool(
        matches!(at_least, Value::Bool(true)) && matches!(at_most, Value::Bool(true)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(value: f64, unit: DurationUnit) -> String {
        iso_string(&Duration { value, unit }).unwrap()
    }

    #[test]
    fn iso_strings_match_pkl() {
        assert_eq!(iso(1.0, DurationUnit::Nanos), "PT0.000000001S");
        assert_eq!(iso(6.6, DurationUnit::Hours), "PT6H36M");
        assert_eq!(iso(7.0, DurationUnit::Days), "PT168H");
        assert_eq!(iso(2000.5, DurationUnit::Millis), "PT2.0005S");
        assert_eq!(iso(-0.0, DurationUnit::Hours), "PT0S");
        assert_eq!(iso(-10.001, DurationUnit::Seconds), "-PT10.001S");
    }
}
