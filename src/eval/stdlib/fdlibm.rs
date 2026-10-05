//! `pkl:math`'s IEEE-754 functions.
//!
//! These wrappers use `libm` 0.2.16 (MIT OR Apache-2.0), whose implementations
//! retain the original Sun/FreeBSD fdlibm notices where applicable. Pkl uses
//! Java `StrictMath`; the only semantic adjustment needed by its `pow` wrapper
//! is the Java result for an infinite exponent and a base whose magnitude is
//! exactly one.

#[inline]
pub(crate) fn sin(x: f64) -> f64 {
    libm::sin(x)
}
#[inline]
pub(crate) fn cos(x: f64) -> f64 {
    libm::cos(x)
}
#[inline]
pub(crate) fn tan(x: f64) -> f64 {
    libm::tan(x)
}
#[inline]
pub(crate) fn asin(x: f64) -> f64 {
    libm::asin(x)
}
#[inline]
pub(crate) fn acos(x: f64) -> f64 {
    libm::acos(x)
}
#[inline]
pub(crate) fn atan(x: f64) -> f64 {
    libm::atan(x)
}
#[inline]
pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}
#[inline]
pub(crate) fn cbrt(x: f64) -> f64 {
    libm::cbrt(x)
}

/// Java `StrictMath.pow`, including its special `±1 ^ ±∞` result.
#[inline]
pub(crate) fn pow(x: f64, y: f64) -> f64 {
    if y.is_infinite() && x.abs() == 1.0 {
        f64::NAN
    } else {
        libm::pow(x, y)
    }
}

#[inline]
pub(crate) fn exp(x: f64) -> f64 {
    libm::exp(x)
}
#[inline]
pub(crate) fn log(x: f64) -> f64 {
    libm::log(x)
}
#[inline]
pub(crate) fn log10(x: f64) -> f64 {
    libm::log10(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow_matches_java_at_one_and_infinity() {
        assert!(pow(1.0, f64::INFINITY).is_nan());
        assert!(pow(-1.0, f64::NEG_INFINITY).is_nan());
    }
}
