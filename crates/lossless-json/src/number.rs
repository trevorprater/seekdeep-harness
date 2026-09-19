//! ECMAScript numbers: one binary64 value that formats and serializes as JavaScript does.

use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::{Add, Sub},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Largest magnitude below which every integer is a distinct binary64 value.
const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;

/// A JSON number as ECMAScript reads it: one binary64 value.
///
/// `Display` follows `Number.prototype.toString` and serialization follows
/// `JSON.stringify`: an integral value prints without a fraction, a value at or
/// beyond 2^53 prints the shortest digits that round-trip (`18446744073709552000`
/// for 2^64), large magnitudes use exponent notation (`1e+300`), and a
/// non-finite value serializes as JSON `null`. Equality treats every NaN as one
/// value and both zeros as equal, and hashing agrees with equality.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonNumber(f64);

impl JsonNumber {
    /// Wraps a binary64 value.
    #[must_use]
    pub const fn new(value: f64) -> Self {
        Self(value)
    }

    /// The binary64 value.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// `Number.isInteger`: finite with no fractional part.
    #[must_use]
    pub fn is_integer(self) -> bool {
        self.0.is_finite() && self.0.fract() == 0.0
    }

    /// An integer of at least one, the shape every read offset, limit, cap,
    /// and line number takes.
    #[must_use]
    pub fn is_positive_integer(self) -> bool {
        self.is_integer() && self.0 >= 1.0
    }

    /// The value as a count, saturating at the `usize` range and treating NaN
    /// and negative values as zero.
    #[must_use]
    pub fn saturating_usize(self) -> usize {
        // A float-to-integer `as` cast saturates at the target range, maps NaN to
        // zero, and truncates the fraction, which is exactly the count a cap bounds.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = self.0 as usize;
        count
    }
}

impl fmt::Display for JsonNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_nan() {
            formatter.write_str("NaN")
        } else if self.0.is_infinite() {
            formatter.write_str(if self.0 > 0.0 {
                "Infinity"
            } else {
                "-Infinity"
            })
        } else {
            formatter.write_str(ryu_js::Buffer::new().format_finite(self.0))
        }
    }
}

impl PartialEq for JsonNumber {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 || (self.0.is_nan() && other.0.is_nan())
    }
}

impl Eq for JsonNumber {}

impl Hash for JsonNumber {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let canonical = if self.0.is_nan() {
            f64::NAN
        } else if self.0 == 0.0 {
            0.0
        } else {
            self.0
        };
        canonical.to_bits().hash(state);
    }
}

impl PartialOrd for JsonNumber {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl PartialEq<f64> for JsonNumber {
    fn eq(&self, other: &f64) -> bool {
        self.0 == *other
    }
}

impl PartialOrd<f64> for JsonNumber {
    fn partial_cmp(&self, other: &f64) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(other)
    }
}

impl Add for JsonNumber {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl Sub for JsonNumber {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl Add<f64> for JsonNumber {
    type Output = Self;

    fn add(self, other: f64) -> Self {
        Self(self.0 + other)
    }
}

impl Sub<f64> for JsonNumber {
    type Output = Self;

    fn sub(self, other: f64) -> Self {
        Self(self.0 - other)
    }
}

impl From<f64> for JsonNumber {
    fn from(value: f64) -> Self {
        Self(value)
    }
}

impl From<JsonNumber> for f64 {
    fn from(value: JsonNumber) -> Self {
        value.0
    }
}

macro_rules! from_exact {
    ($($source:ty),*) => {$(
        impl From<$source> for JsonNumber {
            fn from(value: $source) -> Self {
                Self(f64::from(value))
            }
        }
    )*};
}

macro_rules! from_count {
    ($($source:ty),*) => {$(
        impl From<$source> for JsonNumber {
            // A count the runtime measured is a JavaScript number in the source; beyond
            // 2^53 the conversion rounds exactly as the source's arithmetic would.
            #[allow(clippy::cast_precision_loss)]
            fn from(value: $source) -> Self {
                Self(value as f64)
            }
        }
    )*};
}

from_exact!(u8, u16, u32, i32);
from_count!(u64, usize, i64);

impl Serialize for JsonNumber {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !self.0.is_finite() {
            return serializer.serialize_unit();
        }
        if self.is_integer() && self.0.abs() < EXACT_INTEGER_LIMIT {
            // Every integer below 2^53 is exact, and its JavaScript text is its digits.
            #[allow(clippy::cast_possible_truncation)]
            return serializer.serialize_i64(self.0 as i64);
        }
        serde_json::Number::from_string_unchecked(self.to_string()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonNumber {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A `serde_json::Number` keeps the literal's text, so a magnitude past
        // binary64 reads as an infinity, the way `JSON.parse` reads it.
        let number = serde_json::Number::deserialize(deserializer)?;
        number
            .to_string()
            .parse()
            .map(Self)
            .map_err(|_| serde::de::Error::custom("not a JavaScript number"))
    }
}

#[cfg(test)]
mod tests {
    use super::JsonNumber;

    #[test]
    fn formats_like_javascript() {
        for (value, text) in [
            (0.0, "0"),
            (1.0, "1"),
            (2000.0, "2000"),
            (1.5, "1.5"),
            (1e21, "1e+21"),
            (1e300, "1e+300"),
            (1e-7, "1e-7"),
            (9_007_199_254_740_993.0, "9007199254740992"),
            (18_446_744_073_709_551_616.0, "18446744073709552000"),
            (1_152_921_504_606_846_976.0, "1152921504606847000"),
            (-3.0, "-3"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(JsonNumber::new(value).to_string(), text, "{value}");
        }
    }

    #[test]
    fn serializes_like_json_stringify_and_reads_any_json_number() {
        for (value, json) in [
            (1.0, "1"),
            (2000.0, "2000"),
            (1.5, "1.5"),
            (1e300, "1e+300"),
            (18_446_744_073_709_551_616.0, "18446744073709552000"),
            (-9_007_199_254_740_991.0, "-9007199254740991"),
            (f64::NAN, "null"),
            (f64::INFINITY, "null"),
        ] {
            assert_eq!(
                serde_json::to_string(&JsonNumber::new(value)).unwrap(),
                json
            );
        }
        for (json, value) in [
            ("1", 1.0),
            ("1e0", 1.0),
            ("1e+300", 1e300),
            ("18446744073709551616", 18_446_744_073_709_551_616.0),
            ("18446744073709552000", 18_446_744_073_709_551_616.0),
            ("1e400", f64::INFINITY),
            ("-0", -0.0),
        ] {
            let parsed: JsonNumber = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, JsonNumber::new(value), "{json}");
        }
        assert!(serde_json::from_str::<JsonNumber>("\"1\"").is_err());
        let value: crate::JsonValue = crate::JsonValue::parse("[1e0,2.5e+300]".to_owned()).unwrap();
        let items = value.as_ref().array_items().unwrap();
        assert_eq!(
            items[0].deserialize::<JsonNumber>().unwrap(),
            JsonNumber::new(1.0)
        );
        assert_eq!(
            items[1].deserialize::<JsonNumber>().unwrap(),
            JsonNumber::new(2.5e300)
        );
        assert_eq!(
            crate::JsonValue::from_serialize(&[JsonNumber::new(1e300), JsonNumber::new(7.0)])
                .unwrap()
                .as_raw(),
            "[1e+300,7]"
        );
    }

    #[test]
    fn classifies_integers_and_compares_as_binary64() {
        assert!(JsonNumber::new(1e300).is_positive_integer());
        assert!(JsonNumber::new(18_446_744_073_709_551_616.0).is_positive_integer());
        assert!(JsonNumber::new(0.0).is_integer());
        assert!(!JsonNumber::new(0.0).is_positive_integer());
        assert!(!JsonNumber::new(1.5).is_integer());
        assert!(!JsonNumber::new(f64::INFINITY).is_integer());
        assert!(!JsonNumber::new(f64::NAN).is_integer());
        assert_eq!(JsonNumber::new(1e300) + 1.0, JsonNumber::new(1e300));
        assert_eq!(JsonNumber::new(1e300) - 1.0, JsonNumber::new(1e300));
        assert!(JsonNumber::new(3.0) > JsonNumber::from(2_u64));
        assert!(JsonNumber::new(3.0) > 2.0);
        assert_eq!(JsonNumber::new(f64::NAN), JsonNumber::new(f64::NAN));
        assert_eq!(JsonNumber::new(0.0), JsonNumber::new(-0.0));
        assert_eq!(JsonNumber::new(1e300).saturating_usize(), usize::MAX);
        assert_eq!(JsonNumber::new(-1.0).saturating_usize(), 0);
        assert_eq!(JsonNumber::new(f64::NAN).saturating_usize(), 0);
        assert_eq!(JsonNumber::new(2000.0).saturating_usize(), 2000);
        assert_eq!(JsonNumber::from(usize::MAX).saturating_usize(), usize::MAX);
    }
}
