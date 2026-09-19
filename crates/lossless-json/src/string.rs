//! Mutable JSON strings with exact UTF-16 code units.

use std::{
    fmt,
    string::FromUtf16Error,
    sync::{Arc, OnceLock},
};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error as _, MapAccess, Visitor},
};
use serde_json::value::RawValue;

use crate::{JsonValue, encode_utf16};

/// A JavaScript-compatible string whose lone UTF-16 surrogates survive JSON
/// serialization and concatenation.
#[derive(Clone, Default)]
pub struct JsonString {
    inner: Arc<StringData>,
}

#[derive(Default)]
struct StringData {
    units: Vec<u16>,
    scalar: OnceLock<Option<String>>,
    json: OnceLock<String>,
}

impl Clone for StringData {
    fn clone(&self) -> Self {
        Self {
            units: self.units.clone(),
            scalar: OnceLock::new(),
            json: OnceLock::new(),
        }
    }
}

impl JsonString {
    /// Copies exact JavaScript UTF-16 code units.
    #[must_use]
    pub fn from_utf16(units: &[u16]) -> Self {
        Self::from_units(units.to_vec())
    }

    fn from_units(units: Vec<u16>) -> Self {
        Self {
            inner: Arc::new(StringData {
                units,
                ..StringData::default()
            }),
        }
    }

    /// Reads a quoted JSON string and retains its decoded code units.
    ///
    /// # Errors
    ///
    /// Returns a syntax error or rejects a JSON value that is not a string.
    pub fn parse(json: String) -> serde_json::Result<Self> {
        Self::try_from(JsonValue::parse(json)?)
    }

    /// The canonical quoted JSON string, using ECMAScript escaping.
    #[must_use]
    pub fn as_raw(&self) -> &str {
        self.inner
            .json
            .get_or_init(|| encode_utf16(&self.inner.units))
    }

    /// Borrows UTF-8 text only when every UTF-16 code unit is representable.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        self.inner
            .scalar
            .get_or_init(|| String::from_utf16(&self.inner.units).ok())
            .as_deref()
    }

    /// The string's exact JavaScript code units.
    #[must_use]
    pub fn utf16_units(&self) -> &[u16] {
        &self.inner.units
    }

    /// Copies the string's exact JavaScript code units.
    #[must_use]
    pub fn to_utf16(&self) -> Vec<u16> {
        self.inner.units.clone()
    }

    /// Converts to UTF-8 only when that representation preserves the string.
    ///
    /// # Errors
    ///
    /// Returns the first unpaired UTF-16 surrogate.
    pub fn try_into_string(self) -> Result<String, FromUtf16Error> {
        match Arc::try_unwrap(self.inner) {
            Ok(inner) => match inner.scalar.into_inner() {
                Some(Some(text)) => Ok(text),
                Some(None) | None => String::from_utf16(&inner.units),
            },
            Err(shared) => String::from_utf16(&shared.units),
        }
    }

    /// Whether the string contains no code units.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.units.is_empty()
    }

    /// Its JavaScript string length in UTF-16 code units.
    #[must_use]
    pub fn len_utf16(&self) -> usize {
        self.inner.units.len()
    }

    /// Byte length after ECMAScript `TextEncoder` or Node UTF-8 encoding:
    /// valid surrogate pairs use four bytes, and each lone surrogate uses the
    /// three-byte replacement character at that encoding boundary.
    #[must_use]
    pub fn len_utf8(&self) -> usize {
        char::decode_utf16(self.inner.units.iter().copied())
            .map(|point| point.map_or(3, char::len_utf8))
            .sum()
    }

    /// Iterates ECMAScript string code points, retaining lone surrogate units
    /// as their original numeric values.
    pub fn code_points(&self) -> impl Iterator<Item = u32> + '_ {
        char::decode_utf16(self.inner.units.iter().copied()).map(|point| match point {
            Ok(character) => u32::from(character),
            Err(surrogate) => u32::from(surrogate.unpaired_surrogate()),
        })
    }

    /// Counts ECMAScript string code points, combining only valid surrogate pairs.
    #[must_use]
    pub fn code_point_len(&self) -> usize {
        self.code_points().count()
    }

    /// Reconstructs ECMAScript code points, including lone surrogate values.
    /// Returns `None` for a code point above `0x10ffff`.
    #[must_use]
    pub fn from_code_points(points: &[u32]) -> Option<Self> {
        let mut units = Vec::new();
        for &point in points {
            if point <= 0xffff {
                units.push(u16::try_from(point).ok()?);
            } else if point <= 0x0010_ffff {
                let adjusted = point - 0x0001_0000;
                units.push(0xd800 + u16::try_from(adjusted >> 10).ok()?);
                units.push(0xdc00 + u16::try_from(adjusted & 0x3ff).ok()?);
            } else {
                return None;
            }
        }
        Some(Self::from_units(units))
    }

    /// Removes ECMAScript whitespace and line terminators at both ends.
    #[must_use]
    pub fn trim(&self) -> Self {
        let is_space = |unit: &u16| {
            matches!(
                unit,
                0x9..=0xd | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029
                    | 0x202f | 0x205f | 0x3000 | 0xfeff
            )
        };
        let start = self
            .inner
            .units
            .iter()
            .position(|unit| !is_space(unit))
            .unwrap_or(self.inner.units.len());
        let end = self
            .inner
            .units
            .iter()
            .rposition(|unit| !is_space(unit))
            .map_or(start, |index| index + 1);
        if start == 0 && end == self.inner.units.len() {
            self.clone()
        } else {
            Self::from_utf16(&self.inner.units[start..end])
        }
    }

    /// Appends ordinary UTF-8 text without changing existing code units.
    pub fn push_str(&mut self, text: &str) {
        let inner = Arc::make_mut(&mut self.inner);
        inner.scalar.take();
        inner.json.take();
        inner.units.extend(text.encode_utf16());
    }

    /// Appends exact JavaScript code units.
    pub fn push_utf16(&mut self, units: &[u16]) {
        let inner = Arc::make_mut(&mut self.inner);
        inner.scalar.take();
        inner.json.take();
        inner.units.extend_from_slice(units);
    }

    /// Concatenates strings without converting their code units to UTF-8.
    #[must_use]
    pub fn concat(parts: &[&Self]) -> Self {
        let mut result = Self::default();
        for part in parts {
            result.push_utf16(part.utf16_units());
        }
        result
    }

    /// Joins strings with an ordinary text separator.
    #[must_use]
    pub fn join(parts: &[Self], separator: &str) -> Self {
        let separator = separator.encode_utf16().collect::<Vec<_>>();
        let mut result = Self::default();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                result.push_utf16(&separator);
            }
            result.push_utf16(part.utf16_units());
        }
        result
    }

    /// Checks an ordinary text prefix using JavaScript code units.
    #[must_use]
    pub fn starts_with(&self, prefix: &str) -> bool {
        self.inner
            .units
            .starts_with(&prefix.encode_utf16().collect::<Vec<_>>())
    }

    /// Checks an ordinary text suffix using JavaScript code units.
    #[must_use]
    pub fn ends_with(&self, suffix: &str) -> bool {
        self.inner
            .units
            .ends_with(&suffix.encode_utf16().collect::<Vec<_>>())
    }

    /// Searches for ordinary text using JavaScript code units.
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        let needle = needle.encode_utf16().collect::<Vec<_>>();
        needle.is_empty()
            || self
                .inner
                .units
                .windows(needle.len())
                .any(|window| window == needle)
    }
}

impl Serialize for JsonString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let raw: &RawValue = serde_json::from_str(self.as_raw())
            .expect("UTF-16 encoding produces a valid JSON string");
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(<JsonValue as Deserialize>::deserialize(StringInput(
            deserializer,
        ))?)
        .map_err(D::Error::custom)
    }
}

struct StringInput<D>(D);

impl<'de, D: Deserializer<'de>> Deserializer<'de> for StringInput<D> {
    type Error = D::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.0.deserialize_any(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0
            .deserialize_newtype_struct(name, StringVisitor { name, visitor })
    }

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit unit_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

struct StringVisitor<V> {
    name: &'static str,
    visitor: V,
}

impl<'de, V: Visitor<'de>> StringVisitor<V> {
    fn replay<E: serde::de::Error>(self, text: &str) -> Result<V::Value, E> {
        let encoded = serde_json::to_vec(text).map_err(E::custom)?;
        let mut deserializer = serde_json::Deserializer::from_reader(std::io::Cursor::new(encoded));
        deserializer
            .deserialize_newtype_struct(self.name, self.visitor)
            .map_err(E::custom)
    }
}

impl<'de, V: Visitor<'de>> Visitor<'de> for StringVisitor<V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.visitor.expecting(formatter)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        self.visitor.visit_map(map)
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        let text = String::deserialize(deserializer)?;
        self.replay(&text)
    }

    fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
        self.replay(text)
    }

    fn visit_string<E: serde::de::Error>(self, text: String) -> Result<Self::Value, E> {
        self.replay(&text)
    }
}

impl From<String> for JsonString {
    fn from(text: String) -> Self {
        Self {
            inner: Arc::new(StringData {
                units: text.encode_utf16().collect(),
                scalar: OnceLock::from(Some(text)),
                json: OnceLock::new(),
            }),
        }
    }
}

impl From<&str> for JsonString {
    fn from(text: &str) -> Self {
        Self::from(text.to_owned())
    }
}

impl From<JsonString> for JsonValue {
    fn from(text: JsonString) -> Self {
        let json = match Arc::try_unwrap(text.inner) {
            Ok(inner) => inner
                .json
                .into_inner()
                .unwrap_or_else(|| encode_utf16(&inner.units)),
            Err(shared) => shared
                .json
                .get_or_init(|| encode_utf16(&shared.units))
                .clone(),
        };
        Self::parse(json).expect("UTF-16 encoding produces a valid JSON string")
    }
}

impl TryFrom<JsonValue> for JsonString {
    type Error = serde_json::Error;

    fn try_from(value: JsonValue) -> Result<Self, Self::Error> {
        let units = value
            .to_utf16()
            .ok_or_else(|| serde_json::Error::custom("expected a JSON string"))?;
        Ok(Self::from_units(units))
    }
}

impl fmt::Debug for JsonString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("JsonString")
            .field(&self.as_raw())
            .finish()
    }
}

impl PartialEq for JsonString {
    fn eq(&self, other: &Self) -> bool {
        self.inner.units == other.inner.units
    }
}

impl Eq for JsonString {}

impl std::hash::Hash for JsonString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(self.utf16_units(), state);
    }
}

impl PartialEq<str> for JsonString {
    fn eq(&self, other: &str) -> bool {
        self.inner.units.iter().copied().eq(other.encode_utf16())
    }
}

impl PartialEq<&str> for JsonString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl PartialEq<String> for JsonString {
    fn eq(&self, other: &String) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<JsonString> for str {
    fn eq(&self, other: &JsonString) -> bool {
        other == self
    }
}

impl PartialEq<JsonString> for &str {
    fn eq(&self, other: &JsonString) -> bool {
        other == self
    }
}

impl PartialEq<JsonString> for String {
    fn eq(&self, other: &JsonString) -> bool {
        other == self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_and_mutation_preserve_every_code_unit_without_replacement() {
        let mut text = JsonString::parse(r#""\ud800""#.to_owned()).unwrap();
        assert!(text.as_str().is_none());
        assert_eq!(serde_json::to_string(&text).unwrap(), r#""\ud800""#);
        text.push_utf16(&[0xdc00]);
        assert_eq!(text.as_str(), Some("𐀀"));
        assert_eq!(text.as_raw(), "\"𐀀\"");
        text.push_str("\n");
        text.push_utf16(&[0xdfff]);
        assert!(text.as_str().is_none());
        assert_eq!(text.to_utf16(), [0xd800, 0xdc00, 0x0a, 0xdfff]);
        let decoded: JsonString = serde_json::from_str(text.as_raw()).unwrap();
        assert_eq!(decoded, text);
        assert!(JsonString::parse("null".to_owned()).is_err());
        assert!(serde_json::from_str::<JsonString>("{}").is_err());
    }

    #[test]
    fn joins_searches_and_compares_without_scalar_conversion() {
        let high = JsonString::from_utf16(&[0xd800]);
        let low = JsonString::from_utf16(&[0xdc00]);
        assert_eq!(JsonString::concat(&[&high, &low]).as_str(), Some("𐀀"));
        let joined = JsonString::join(&[high, low], " middle ");
        assert_eq!(joined.as_raw(), r#""\ud800 middle \udc00""#);
        assert!(joined.contains("middle"));
        assert!(joined.contains(""));
        assert_eq!(JsonString::from("text"), "text");
        assert_eq!("text".to_owned(), JsonString::from("text"));
    }

    #[test]
    fn trim_and_code_point_operations_match_ecmascript_surrogate_rules() {
        let text =
            JsonString::from_utf16(&[0xfeff, 0xa0, 0xd800, 0x61, 0xd83d, 0xde00, 0xdc00, 0x2029]);
        let trimmed = text.trim();
        assert_eq!(trimmed.to_utf16(), [0xd800, 0x61, 0xd83d, 0xde00, 0xdc00]);
        let points = trimmed.code_points().collect::<Vec<_>>();
        assert_eq!(points, [0xd800, 0x61, 0x0001_f600, 0xdc00]);
        assert_eq!(trimmed.code_point_len(), 4);
        assert_eq!(trimmed.len_utf8(), 11);
        assert_eq!(JsonString::from_code_points(&points), Some(trimmed));
        assert!(JsonString::from_code_points(&[0x0011_0000]).is_none());
        assert_eq!(
            JsonString::from_utf16(&[0x85, 0x20]).trim().to_utf16(),
            [0x85]
        );
    }

    #[test]
    fn ordinary_strings_remain_compatible_with_serde_enum_buffers() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(tag = "kind", rename_all = "lowercase")]
        enum Tagged {
            Text { text: JsonString },
        }
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(untagged)]
        enum Untagged {
            Text { text: JsonString },
            Array(Vec<JsonString>),
        }
        assert_eq!(
            serde_json::from_str::<Tagged>(r#"{"kind":"text","text":"ordinary \u0061"}"#).unwrap(),
            Tagged::Text {
                text: "ordinary a".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Untagged>(r#"["ordinary"]"#).unwrap(),
            Untagged::Array(vec!["ordinary".into()])
        );
        assert!(
            serde_json::from_str::<Tagged>(
                r#"{"kind":"text","text":{"$serde_json::private::RawValue":"\"fake\""}}"#
            )
            .is_err()
        );
        let raw: JsonString = serde_json::from_str(r#""\ud800""#).unwrap();
        assert_eq!(raw.to_utf16(), [0xd800]);
    }
}
