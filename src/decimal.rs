//! Exact decimal numbers for plugin code: money, pay, days of leave.
//!
//! The kernel sends `decimal` fields as text (`"1500.50"`) so no digit is lost. A [`Decimal`] is
//! that text read as a whole number of the smallest unit plus a scale, so adding, subtracting and
//! comparing are exact and nothing passes through floating point. It reads and writes the same
//! text, so it can be a field of a record type directly.

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Neg, Sub};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// The most digits after the point.
pub const MAX_SCALE: u32 = 9;

/// An exact decimal: `units` of 10^-`scale`.
#[derive(Debug, Clone, Copy)]
pub struct Decimal {
    units: i64,
    scale: u32,
}

fn power(scale: u32) -> i64 {
    10_i64.pow(scale)
}

impl Decimal {
    /// Zero with this many digits after the point.
    pub fn zero(scale: u32) -> Self {
        Self { units: 0, scale: scale.min(MAX_SCALE) }
    }

    /// A whole number.
    pub fn whole(value: i64, scale: u32) -> Result<Self> {
        let scale = scale.min(MAX_SCALE);
        value
            .checked_mul(power(scale))
            .map(|units| Self { units, scale })
            .ok_or_else(|| Error::msg("the number is too large"))
    }

    /// Read `12.34`, `-0.5` or `7`. The result has as many digits after the point as were written;
    /// use [`Decimal::with_scale`] to line it up with a field.
    pub fn parse(text: &str) -> Result<Self> {
        let bad = || Error::msg(format!("`{text}` is not a decimal number like 12.34"));
        let trimmed = text.trim();
        let (negative, unsigned) = match trimmed.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
        };
        let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || fraction.len() > MAX_SCALE as usize
        {
            return Err(bad());
        }
        let digits = format!("{}{fraction}", whole.trim_start_matches('0'));
        if digits.len() > 17 {
            return Err(Error::msg("the number is too large"));
        }
        let magnitude: i64 = if digits.is_empty() { 0 } else { digits.parse().map_err(|_| bad())? };
        Ok(Self { units: if negative { -magnitude } else { magnitude }, scale: fraction.len() as u32 })
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    pub fn is_zero(&self) -> bool {
        self.units == 0
    }

    pub fn is_negative(&self) -> bool {
        self.units < 0
    }

    /// The same value with `scale` digits after the point. Fewer digits than it has are refused
    /// unless the dropped ones are zero: use [`Decimal::round_to`] to round on purpose.
    pub fn with_scale(self, scale: u32) -> Result<Self> {
        let scale = scale.min(MAX_SCALE);
        match scale.cmp(&self.scale) {
            Ordering::Equal => Ok(self),
            Ordering::Greater => self
                .units
                .checked_mul(power(scale - self.scale))
                .map(|units| Self { units, scale })
                .ok_or_else(|| Error::msg("the number is too large")),
            Ordering::Less => {
                let unit = power(self.scale - scale);
                if self.units % unit != 0 {
                    return Err(Error::msg("the number has more digits after the point than allowed"));
                }
                Ok(Self { units: self.units / unit, scale })
            }
        }
    }

    /// Round half away from zero to `scale` digits after the point.
    pub fn round_to(self, scale: u32) -> Self {
        let scale = scale.min(MAX_SCALE);
        if scale >= self.scale {
            return self.with_scale(scale).unwrap_or(self);
        }
        let unit = power(self.scale - scale);
        let half = unit / 2;
        let adjusted = if self.units >= 0 { self.units + half } else { self.units - half };
        Self { units: adjusted / unit, scale }
    }

    /// This times `numerator` over `denominator`, rounded half away from zero to this scale.
    /// For shares: a year's leave times days left over days in the year.
    pub fn times_ratio(self, numerator: i64, denominator: i64) -> Result<Self> {
        if denominator == 0 {
            return Err(Error::msg("cannot divide by zero"));
        }
        let product = i128::from(self.units) * i128::from(numerator);
        let denominator = i128::from(denominator);
        let half = denominator.abs() / 2;
        let rounded = if (product >= 0) == (denominator > 0) { product + half } else { product - half };
        let units = i64::try_from(rounded / denominator).map_err(|_| Error::msg("the number is too large"))?;
        Ok(Self { units, scale: self.scale })
    }

    /// Round to the nearest half (0.5), for days of leave.
    pub fn round_to_half(self) -> Result<Self> {
        let doubled = self.times_ratio(2, 1)?.round_to(0).with_scale(self.scale)?;
        doubled.times_ratio(1, 2)
    }

    /// This times `other`, exact, then rounded half away from zero to `scale` digits. For converting
    /// an amount at a rate.
    pub fn times(self, other: Self, scale: u32) -> Result<Self> {
        let scale = scale.min(MAX_SCALE);
        let product = i128::from(self.units) * i128::from(other.units);
        let digits = self.scale + other.scale;
        let units = if digits > scale {
            let divisor = 10_i128.pow(digits - scale);
            let half = divisor / 2;
            (if product >= 0 { product + half } else { product - half }) / divisor
        } else {
            product * 10_i128.pow(scale - digits)
        };
        i64::try_from(units).map(|units| Self { units, scale }).map_err(|_| Error::msg("the number is too large"))
    }

    /// This divided by `other`, rounded half away from zero to `scale` digits.
    pub fn div(self, other: Self, scale: u32) -> Result<Self> {
        if other.units == 0 {
            return Err(Error::msg("cannot divide by zero"));
        }
        let scale = scale.min(MAX_SCALE);
        // self.units / 10^a  /  (other.units / 10^b)  =  self.units * 10^b / (other.units * 10^a); shift to `scale`.
        let numerator = i128::from(self.units) * 10_i128.pow(other.scale + scale);
        let denominator = i128::from(other.units) * 10_i128.pow(self.scale);
        let half = denominator.abs() / 2;
        let rounded = if (numerator >= 0) == (denominator > 0) { numerator + half } else { numerator - half };
        i64::try_from(rounded / denominator).map(|units| Self { units, scale }).map_err(|_| Error::msg("the number is too large"))
    }

    fn align(self, other: Self) -> Result<(i64, i64, u32)> {
        let scale = self.scale.max(other.scale);
        Ok((self.with_scale(scale)?.units, other.with_scale(scale)?.units, scale))
    }

    pub fn checked_add(self, other: Self) -> Result<Self> {
        let (a, b, scale) = self.align(other)?;
        a.checked_add(b).map(|units| Self { units, scale }).ok_or_else(|| Error::msg("the number is too large"))
    }

    pub fn checked_sub(self, other: Self) -> Result<Self> {
        let (a, b, scale) = self.align(other)?;
        a.checked_sub(b).map(|units| Self { units, scale }).ok_or_else(|| Error::msg("the number is too large"))
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.scale == 0 {
            return write!(f, "{}", self.units);
        }
        let unit = power(self.scale) as u64;
        let magnitude = self.units.unsigned_abs();
        let sign = if self.units < 0 { "-" } else { "" };
        write!(f, "{sign}{}.{:0width$}", magnitude / unit, magnitude % unit, width = self.scale as usize)
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        let scale = self.scale.max(other.scale);
        let left = i128::from(self.units) * i128::from(power(scale - self.scale));
        let right = i128::from(other.units) * i128::from(power(scale - other.scale));
        left.cmp(&right)
    }
}

/// Addition that panics on overflow is not offered: use [`Decimal::checked_add`]. These operators
/// saturate instead, which is only meant for sums of small amounts such as days of leave.
impl Add for Decimal {
    type Output = Decimal;
    fn add(self, other: Decimal) -> Decimal {
        self.checked_add(other).unwrap_or(self)
    }
}

impl Sub for Decimal {
    type Output = Decimal;
    fn sub(self, other: Decimal) -> Decimal {
        self.checked_sub(other).unwrap_or(self)
    }
}

impl Neg for Decimal {
    type Output = Decimal;
    fn neg(self) -> Decimal {
        Decimal { units: -self.units, scale: self.scale }
    }
}

impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::String(text) => Decimal::parse(text).map_err(de::Error::custom),
            serde_json::Value::Number(number) => Decimal::parse(&number.to_string()).map_err(de::Error::custom),
            _ => Err(de::Error::custom("a decimal is text like \"12.34\"")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or(Decimal::zero(0))
    }

    #[test]
    fn parse_and_print_keep_every_digit() {
        assert_eq!(d("12.30").to_string(), "12.30");
        assert_eq!(d("-0.05").to_string(), "-0.05");
        assert_eq!(d("7").to_string(), "7");
        assert!(Decimal::parse("1e5").is_err());
        assert!(Decimal::parse("").is_err());
        assert!(Decimal::parse("1.2.3").is_err());
    }

    #[test]
    fn sums_are_exact() {
        assert_eq!((d("0.10") + d("0.20")).to_string(), "0.30");
        assert_eq!((d("10") - d("0.5")).to_string(), "9.5");
        assert!(d("1.50") == d("1.5"), "scale does not change the value");
        assert!(d("2") > d("1.99"));
        assert!((d("1") - d("2")).is_negative());
        assert_eq!((-d("2.5")).to_string(), "-2.5");
    }

    #[test]
    fn scale_is_lined_up_without_losing_digits() -> Result<()> {
        assert_eq!(d("1.5").with_scale(2)?.to_string(), "1.50");
        assert_eq!(d("1.50").with_scale(1)?.to_string(), "1.5");
        assert!(d("1.55").with_scale(1).is_err());
        assert_eq!(d("1.55").round_to(1).to_string(), "1.6");
        assert_eq!(d("-1.55").round_to(1).to_string(), "-1.6");
        Ok(())
    }

    #[test]
    fn shares_round_half_up() -> Result<()> {
        // 21 days for 100 of 365 days of the year.
        assert_eq!(d("21.00").times_ratio(100, 365)?.to_string(), "5.75");
        assert_eq!(d("1.00").times_ratio(1, 3)?.to_string(), "0.33");
        assert!(d("1").times_ratio(1, 0).is_err());
        assert_eq!(d("5.75").round_to_half()?.to_string(), "6.00");
        assert_eq!(d("5.20").round_to_half()?.to_string(), "5.00");
        assert_eq!(d("5.30").round_to_half()?.to_string(), "5.50");
        Ok(())
    }

    #[test]
    fn it_travels_as_text() -> Result<()> {
        let json = serde_json::to_string(&d("12.34"))?;
        assert_eq!(json, "\"12.34\"");
        let back: Decimal = serde_json::from_str("\"12.34\"")?;
        assert_eq!(back, d("12.34"));
        let from_number: Decimal = serde_json::from_str("1.5")?;
        assert_eq!(from_number, d("1.5"));
        assert!(serde_json::from_str::<Decimal>("true").is_err());
        Ok(())
    }

    #[test]
    fn multiplying_and_dividing_stay_exact_and_round_once() -> Result<()> {
        // 100.00 at a rate of 0.8725 is 87.25; at 1.0049 it is 100.49.
        assert_eq!(d("100.00").times(d("0.8725"), 2)?.to_string(), "87.25");
        assert_eq!(d("19.99").times(d("1.0049"), 2)?.to_string(), "20.09");
        assert_eq!(d("-19.99").times(d("1.0049"), 2)?.to_string(), "-20.09");
        assert_eq!(d("2").times(d("3"), 2)?.to_string(), "6.00");
        // A cross rate: 1.1 per base for one currency and 0.85 for the other.
        assert_eq!(d("0.85").div(d("1.1"), 6)?.to_string(), "0.772727");
        assert!(d("1").div(d("0"), 2).is_err());
        assert_eq!(d("10").div(d("4"), 2)?.to_string(), "2.50");
        Ok(())
    }
}
