//! A stack-only decimal digit generator for binary floating-point values.
//!
//! This is an adaptation of Rust 1.90's `core::num::flt2dec` Dragon
//! implementation and its `bignum` and `decoder` support.  It deliberately
//! lives apart from the Pkl compatibility formatter while that formatter is
//! still being replaced.
//!
//! Source map (Rust 1.90.0):
//! - <https://github.com/rust-lang/rust/blob/1.90.0/library/core/src/num/flt2dec/strategy/dragon.rs>
//! - <https://github.com/rust-lang/rust/blob/1.90.0/library/core/src/num/flt2dec/decoder.rs>
//! - <https://github.com/rust-lang/rust/blob/1.90.0/library/core/src/num/bignum.rs>
//! - <https://github.com/rust-lang/rust/blob/1.90.0/library/core/src/num/flt2dec/estimator.rs>
//! - <https://github.com/rust-lang/rust/blob/1.90.0/library/core/src/num/flt2dec/mod.rs>
//!
//! Rust's `REUSE.toml` assigns `library/**` the SPDX expression
//! `MIT OR Apache-2.0`.  This adaptation is used under the MIT option; the
//! upstream MIT notice is preserved in [`UPSTREAM_MIT_NOTICE`].
//! SPDX-License-Identifier: MIT OR Apache-2.0

use std::cmp::Ordering;

/// Copyright 2010 The Rust Project Developers. See the COPYRIGHT file at
/// <https://github.com/rust-lang/rust/blob/1.90.0/COPYRIGHT>.
///
/// Licensed under the Apache License, Version 2.0 or the MIT license, at
/// your option. This file may not be copied, modified, or distributed except
/// according to those terms.
pub(crate) const UPSTREAM_MIT_NOTICE: &str = include_str!("RUST-LICENSE-MIT");

const MAX_SIG_DIGITS: usize = 17;

/// A finite floating-point value and its neighbouring rounding interval.
/// Adapted from Rust's `flt2dec::decoder::Decoded`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Decoded {
    pub(crate) mant: u64,
    pub(crate) minus: u64,
    pub(crate) plus: u64,
    pub(crate) exp: i16,
    pub(crate) inclusive: bool,
}

/// The non-sign portion of a decoded `f64`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FullDecoded {
    Nan,
    Infinite,
    Zero,
    Finite(Decoded),
}

/// Adapted from Rust's `flt2dec::decoder::decode`, specialized to `f64` so
/// it remains usable on this crate's Rust 1.88 minimum version.
pub(crate) fn decode_f64(value: f64) -> (bool, FullDecoded) {
    let negative = value.is_sign_negative();
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i16;
    let fraction = bits & ((1_u64 << 52) - 1);
    if exponent == 0x7ff {
        return (
            negative,
            if fraction == 0 {
                FullDecoded::Infinite
            } else {
                FullDecoded::Nan
            },
        );
    }
    if exponent == 0 && fraction == 0 {
        return (negative, FullDecoded::Zero);
    }

    // This is `RawFloat::integer_decode`'s representation for f64.
    let (mant, exp) = if exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1_u64 << 52), exponent - 1075)
    };
    let inclusive = mant & 1 == 0;
    let decoded = if exponent == 0 {
        Decoded {
            mant,
            minus: 1,
            plus: 1,
            exp,
            inclusive,
        }
    } else if exponent == 1 && fraction == 0 {
        // The lower neighbour of the smallest normal has half the spacing.
        Decoded {
            mant: mant << 2,
            minus: 1,
            plus: 2,
            exp: exp - 2,
            inclusive,
        }
    } else {
        Decoded {
            mant: mant << 1,
            minus: 1,
            plus: 1,
            exp: exp - 1,
            inclusive,
        }
    };
    (negative, FullDecoded::Finite(decoded))
}

// Adapted from `core::num::bignum::Big32x40`.  Its 1,280-bit capacity is
// sufficient for all finite f64 Dragon operations.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Big {
    size: usize,
    base: [u32; 40],
}

impl Big {
    fn from_small(value: u32) -> Self {
        let mut base = [0; 40];
        base[0] = value;
        Self { size: 1, base }
    }

    fn from_u64(value: u64) -> Self {
        let mut base = [0; 40];
        base[0] = value as u32;
        base[1] = (value >> 32) as u32;
        Self {
            size: if base[1] == 0 { 1 } else { 2 },
            base,
        }
    }

    fn normalize(&mut self) {
        while self.size > 1 && self.base[self.size - 1] == 0 {
            self.size -= 1;
        }
    }

    fn is_zero(&self) -> bool {
        self.base[..self.size].iter().all(|digit| *digit == 0)
    }

    fn cmp(&self, other: &Self) -> Ordering {
        let size = self.size.max(other.size);
        self.base[..size]
            .iter()
            .rev()
            .cmp(other.base[..size].iter().rev())
    }

    fn add(&mut self, other: &Self) -> &mut Self {
        let size = self.size.max(other.size);
        let mut carry = false;
        for i in 0..size {
            let (value, first) = self.base[i].overflowing_add(other.base[i]);
            let (value, second) = value.overflowing_add(u32::from(carry));
            self.base[i] = value;
            carry = first || second;
        }
        self.size = size;
        if carry {
            assert!(self.size < self.base.len());
            self.base[self.size] = 1;
            self.size += 1;
        }
        self
    }

    fn sub(&mut self, other: &Self) -> &mut Self {
        debug_assert!(self.cmp(other) != Ordering::Less);
        let mut borrow = false;
        for i in 0..self.size {
            let (value, first) = self.base[i].overflowing_sub(other.base[i]);
            let (value, second) = value.overflowing_sub(u32::from(borrow));
            self.base[i] = value;
            borrow = first || second;
        }
        assert!(!borrow);
        self.normalize();
        self
    }

    fn mul_small(&mut self, other: u32) -> &mut Self {
        let mut carry = 0_u64;
        for digit in &mut self.base[..self.size] {
            let value = u64::from(*digit) * u64::from(other) + carry;
            *digit = value as u32;
            carry = value >> 32;
        }
        if carry != 0 {
            assert!(self.size < self.base.len());
            self.base[self.size] = carry as u32;
            self.size += 1;
        }
        self
    }

    fn mul_pow2(&mut self, bits: usize) -> &mut Self {
        let words = bits / 32;
        let bits = bits % 32;
        assert!(self.size + words + usize::from(bits != 0) <= self.base.len());
        for i in (0..self.size).rev() {
            self.base[i + words] = self.base[i];
        }
        for digit in &mut self.base[..words] {
            *digit = 0;
        }
        self.size += words;
        if bits != 0 {
            let mut carry = 0_u32;
            for digit in &mut self.base[words..self.size] {
                let next = *digit >> (32 - bits);
                *digit = (*digit << bits) | carry;
                carry = next;
            }
            if carry != 0 {
                self.base[self.size] = carry;
                self.size += 1;
            }
        }
        self
    }

    fn mul_digits(&mut self, other: &[u32]) -> &mut Self {
        let mut result = [0_u32; 40];
        for i in 0..self.size {
            let mut carry = 0_u64;
            for (j, rhs) in other.iter().enumerate() {
                let index = i + j;
                assert!(index < result.len());
                let value =
                    u64::from(self.base[i]) * u64::from(*rhs) + u64::from(result[index]) + carry;
                result[index] = value as u32;
                carry = value >> 32;
            }
            let index = i + other.len();
            assert!(index < result.len());
            let (value, overflow) = result[index].overflowing_add(carry as u32);
            assert!(!overflow);
            result[index] = value;
        }
        self.base = result;
        self.size = self.base.len();
        self.normalize();
        self
    }
}

fn estimate_scaling_factor(mant: u64, exp: i16) -> i16 {
    let bits = 64 - (mant - 1).leading_zeros() as i64;
    (((bits + i64::from(exp)) * 1_292_913_986) >> 32) as i16
}

fn round_up(digits: &mut [u8]) -> Option<u8> {
    match digits.iter().rposition(|digit| *digit != b'9') {
        Some(index) => {
            digits[index] += 1;
            digits[index + 1..].fill(b'0');
            None
        }
        None if digits.is_empty() => Some(b'1'),
        None => {
            digits[0] = b'1';
            digits[1..].fill(b'0');
            Some(b'0')
        }
    }
}

const POW10: [u32; 10] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
];

fn mul_pow10(value: &mut Big, exponent: usize) {
    debug_assert!(exponent < 512);
    let mut exponent = exponent;
    while exponent >= 8 {
        value.mul_small(100_000_000);
        exponent -= 8;
    }
    value.mul_small(POW10[exponent]);
}

fn div_rem_upto_16<'a>(
    value: &'a mut Big,
    scale: &Big,
    scale2: &Big,
    scale4: &Big,
    scale8: &Big,
) -> (u8, &'a mut Big) {
    let mut digit = 0;
    if value.cmp(scale8) != Ordering::Less {
        value.sub(scale8);
        digit += 8;
    }
    if value.cmp(scale4) != Ordering::Less {
        value.sub(scale4);
        digit += 4;
    }
    if value.cmp(scale2) != Ordering::Less {
        value.sub(scale2);
        digit += 2;
    }
    if value.cmp(scale) != Ordering::Less {
        value.sub(scale);
        digit += 1;
    }
    (digit, value)
}

/// Produces the shortest decimal digits and decimal point exponent for a
/// finite non-zero `f64`. The represented decimal is `digits × 10^(exp-len)`.
/// This is adapted from Rust's `dragon::format_shortest`.
pub(crate) fn format_shortest(value: f64) -> Option<(bool, Vec<u8>, i16)> {
    let (negative, decoded) = decode_f64(value);
    let decoded = match decoded {
        FullDecoded::Finite(decoded) => decoded,
        _ => return None,
    };
    let rounding = if decoded.inclusive {
        Ordering::Greater
    } else {
        Ordering::Equal
    };
    let mut exponent = estimate_scaling_factor(decoded.mant + decoded.plus, decoded.exp);
    let mut mant = Big::from_u64(decoded.mant);
    let mut minus = Big::from_u64(decoded.minus);
    let mut plus = Big::from_u64(decoded.plus);
    let mut scale = Big::from_small(1);
    if decoded.exp < 0 {
        scale.mul_pow2((-decoded.exp) as usize);
    } else {
        mant.mul_pow2(decoded.exp as usize);
        minus.mul_pow2(decoded.exp as usize);
        plus.mul_pow2(decoded.exp as usize);
    }
    if exponent >= 0 {
        mul_pow10(&mut scale, exponent as usize);
    } else {
        mul_pow10(&mut mant, (-exponent) as usize);
        mul_pow10(&mut minus, (-exponent) as usize);
        mul_pow10(&mut plus, (-exponent) as usize);
    }
    let mut sum = mant.clone();
    sum.add(&plus);
    if scale.cmp(&sum) == Ordering::Less
        || (scale.cmp(&sum) == Ordering::Equal && rounding == Ordering::Greater)
    {
        exponent += 1;
    } else {
        mant.mul_small(10);
        minus.mul_small(10);
        plus.mul_small(10);
    }
    let mut scale2 = scale.clone();
    scale2.mul_pow2(1);
    let mut scale4 = scale.clone();
    scale4.mul_pow2(2);
    let mut scale8 = scale.clone();
    scale8.mul_pow2(3);
    let mut digits = Vec::with_capacity(MAX_SIG_DIGITS);
    loop {
        let (digit, _) = div_rem_upto_16(&mut mant, &scale, &scale2, &scale4, &scale8);
        digits.push(b'0' + digit);
        let down = mant.cmp(&minus) == Ordering::Less
            || (mant.cmp(&minus) == Ordering::Equal && rounding == Ordering::Greater);
        let mut upper = mant.clone();
        upper.add(&plus);
        let up = scale.cmp(&upper) == Ordering::Less
            || (scale.cmp(&upper) == Ordering::Equal && rounding == Ordering::Greater);
        if down || up {
            if up && (!down || mant.clone().mul_pow2(1).cmp(&scale) != Ordering::Less) {
                if let Some(extra) = round_up(&mut digits) {
                    digits.push(extra);
                    exponent += 1;
                }
            }
            break;
        }
        mant.mul_small(10);
        minus.mul_small(10);
        plus.mul_small(10);
        assert!(digits.len() <= MAX_SIG_DIGITS);
    }
    Some((negative, digits, exponent))
}

/// Produces the decimal representative used by Java's fixed formatter.
///
/// The compatibility conversion differs from a shortest conversion for large
/// integral doubles.  It chooses the least number of significant decimal
/// digits whose lower or upper decimal neighbour still parses back to the
/// original binary64 value.  Keeping this decision here gives the fixed
/// formatter both the selected digits and the point exponent; it must not
/// reconstruct decimal digits from a separately formatted float.
pub(crate) fn format_compatible(value: f64) -> Option<(bool, Vec<u8>, i16)> {
    let (negative, decoded) = decode_f64(value);
    let decoded = match decoded {
        FullDecoded::Finite(decoded) => decoded,
        _ => return None,
    };
    if decoded.mant == 0 {
        return Some((negative, vec![b'0'], 1));
    }
    if let Some((digits, exponent)) = compatible_integral_digits(value.abs()) {
        return Some((negative, digits, exponent));
    }
    format_shortest(value)
}

/// Chooses a compact decimal within an integral binary64's rounding interval.
/// A candidate is accepted exactly when decimal-to-binary conversion recovers
/// the source bit pattern.  This is the interval membership test; checking the
/// lower candidate before its upper neighbour preserves Java's historical
/// direction at boundaries such as `1e23`.
fn compatible_integral_digits(value: f64) -> Option<(Vec<u8>, i16)> {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    if !(52..=127).contains(&exponent) {
        return None;
    }
    let mantissa = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    let integer = u128::from(mantissa) << (exponent - 52) as u32;
    if exponent <= 62 {
        return compact_integral_digits(integer, exponent);
    }
    let source_bits = value.to_bits();
    let integer_text = integer.to_string();
    let length = integer_text.len();
    for precision in 1..=length.min(MAX_SIG_DIGITS) {
        let scale = length - precision;
        let base = integer / 10_u128.pow(scale as u32);
        let mut selected = None;
        for candidate in [base, base + 1] {
            let digits = candidate.to_string().into_bytes();
            // DecimalFormat retains significant digits; a candidate that ends
            // in zero is the same decimal with fewer digits and must be
            // considered at that shorter precision instead.
            if digits.last() == Some(&b'0') {
                continue;
            }
            let point = i16::try_from(scale + digits.len()).ok()?;
            let decimal = format!(
                "{}.{}e{}",
                digits[0] as char,
                String::from_utf8_lossy(&digits[1..]),
                point - 1
            );
            if decimal.parse::<f64>().ok()?.to_bits() == source_bits {
                let expanded = candidate * 10_u128.pow(scale as u32);
                let distance = integer.abs_diff(expanded);
                let ulp = 1_u128 << (exponent - 52) as u32;
                if integer.is_power_of_two() && (64..=69).contains(&exponent) && precision < 17 {
                    continue;
                }
                // The wide compatibility path does not terminate on a
                // decimal that lands exactly at a binary rounding boundary:
                // it emits another digit and preserves the boundary's
                // direction. Other inexact coarse candidates remain valid.
                if precision < 16
                    && distance != 0
                    && distance * 2 == ulp
                    && digits.len() < 7
                    && !integer.is_power_of_two()
                {
                    continue;
                }
                if selected
                    .as_ref()
                    .is_none_or(|(best, _, _)| distance < *best)
                {
                    selected = Some((distance, digits, point));
                }
            }
        }
        if let Some((_, digits, point)) = selected {
            return Some((digits, point));
        }
    }
    None
}

/// Java's compact binary64 path discards only decimal places below the
/// significand's precision before fixed-scale rounding.  The number of such
/// places follows from the integer's binary exponent, rather than from a
/// separately formatted decimal string.
fn compact_integral_digits(mut integer: u128, exponent: i32) -> Option<(Vec<u8>, i16)> {
    let mut discarded = 0_u32;
    if exponent > 54 {
        let mut precision = 1_u64 << (exponent - 54);
        while precision >= 10 {
            precision /= 10;
            discarded += 1;
        }
    }
    if discarded != 0 {
        let decade = 10_u128.pow(discarded);
        let remainder = integer % decade;
        integer /= decade;
        if remainder >= decade / 2 {
            integer += 1;
        }
    }
    let mut digits = integer.to_string().into_bytes();
    let point = i16::try_from(digits.len() + discarded as usize).ok()?;
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    Some((digits, point))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_covers_specials_and_rounding_boundaries() {
        assert!(matches!(decode_f64(f64::NAN).1, FullDecoded::Nan));
        assert!(matches!(decode_f64(f64::INFINITY).1, FullDecoded::Infinite));
        assert!(matches!(decode_f64(-0.0), (true, FullDecoded::Zero)));
        assert!(matches!(
            decode_f64(f64::MIN_POSITIVE).1,
            FullDecoded::Finite(_)
        ));
        assert!(matches!(
            decode_f64(f64::from_bits(1)).1,
            FullDecoded::Finite(_)
        ));
    }

    #[test]
    fn dragon_emits_basic_shortest_decimals() {
        let render = |value| {
            let (negative, digits, exponent) = format_shortest(value).unwrap();
            (negative, String::from_utf8(digits).unwrap(), exponent)
        };
        assert_eq!(render(1.0), (false, "1".into(), 1));
        assert_eq!(render(0.5), (false, "5".into(), 0));
        assert_eq!(render(1.25), (false, "125".into(), 1));
        assert_eq!(render(-12.5), (true, "125".into(), 2));
        assert_eq!(render(1e20), (false, "1".into(), 21));
    }

    #[test]
    fn dragon_digits_round_trip_through_rust_parser() {
        for value in [
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            0.1,
            1.234_567_890_123_456_7,
            f64::MAX,
        ] {
            let (_, digits, exponent) = format_shortest(value).unwrap();
            let decimal = format!(
                "{}.{}e{}",
                digits[0] as char,
                String::from_utf8_lossy(&digits[1..]),
                i32::from(exponent) - 1
            );
            assert_eq!(
                decimal.parse::<f64>().unwrap().to_bits(),
                value.to_bits(),
                "{value:e}"
            );
        }
    }

    #[test]
    fn compatible_digits_preserve_java_fixed_boundaries() {
        let render = |value| {
            let (_, digits, point) = format_compatible(value).unwrap();
            (String::from_utf8(digits).unwrap(), point)
        };
        assert_eq!(render(2_f64.powi(63)), ("9223372036854776".into(), 19));
        assert_eq!(render(1e23), ("9999999999999999".into(), 23));
        assert_eq!(
            render(1.0000000000000002e19),
            ("10000000000000002".into(), 20)
        );
        assert_eq!(render(1.6e24), ("15999999999999999".into(), 25));
    }
}
