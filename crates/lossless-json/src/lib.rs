//! JSON snapshots that retain every ECMAScript string code unit.

use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    ops::Index,
    sync::{Arc, LazyLock, OnceLock},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, value::RawValue};

mod javascript;
mod pretty;
mod string;
pub use string::JsonString;

/// A validated JSON value, including escaped lone UTF-16 surrogates in strings
/// and object keys. Serialization emits the original JSON value directly.
/// Indexing returns null for absent entries or an incompatible container type;
/// [`JsonValue::get`] distinguishes an absent property from a null value.
pub struct JsonValue {
    snapshot: Arc<JsonSnapshot>,
    cache: OnceLock<Box<JsonCache>>,
}

struct JsonSnapshot {
    raw: Arc<RawValue>,
    start: usize,
    end: usize,
}

#[derive(Default)]
struct JsonCache {
    serde_value: OnceLock<Option<Value>>,
    children: OnceLock<JsonChildren>,
}

enum JsonChildren {
    Scalar,
    Array(Vec<JsonValue>),
    Object(BTreeMap<Vec<u16>, JsonValue>),
}

static NULL: LazyLock<JsonValue> = LazyLock::new(|| JsonValue::from(Value::Null));

/// A borrowed, validated JSON value or string key within a snapshot.
#[derive(Clone, Copy, Debug)]
pub struct JsonRef<'a> {
    raw: &'a str,
}

/// One event in a linear traversal of a validated JSON snapshot.
#[derive(Clone, Copy, Debug)]
pub enum JsonToken<'a> {
    /// Opens an array.
    ArrayStart,
    /// Closes an array.
    ArrayEnd,
    /// Opens an object.
    ObjectStart,
    /// Closes an object.
    ObjectEnd,
    /// An object's string key; its code units need not be Unicode scalars.
    Key(JsonRef<'a>),
    /// A null, boolean, number, or string value.
    Scalar(JsonRef<'a>),
}

/// Iterative JSON traversal independent of application nesting depth.
pub struct JsonTokens<'a> {
    raw: &'a str,
    offset: usize,
}

/// Deserializes a present optional JSON field, retaining explicit null as a
/// value. Use with `#[serde(default)]` so an absent field remains `None`.
///
/// # Errors
///
/// Returns the JSON value's deserialization error.
pub fn deserialize_optional<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<JsonValue>, D::Error> {
    <JsonValue as Deserialize>::deserialize(deserializer).map(Some)
}

/// Deserializes a present optional field without treating JSON null as absence.
/// Pair with `#[serde(default)]` to reserve `None` for a missing field.
///
/// # Errors
///
/// Returns an error when the field cannot be represented by `T`.
pub fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl JsonValue {
    /// Retains a validated JSON value without decoding its strings into UTF-8.
    ///
    /// # Errors
    ///
    /// Returns a JSON syntax error for invalid or trailing input.
    pub fn parse(json: String) -> serde_json::Result<Self> {
        RawValue::from_string(json).map(Self::from_raw)
    }

    /// Parses JSON text whose quoted strings may contain literal lone UTF-16
    /// surrogates, as accepted by ECMAScript `JSON.parse`.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or trailing JSON text.
    pub fn parse_text(text: &JsonString) -> serde_json::Result<Self> {
        javascript::parse(text)
    }

    /// Serializes a value directly to a snapshot, including nested raw JSON.
    ///
    /// # Errors
    ///
    /// Returns an error from the supplied value's serializer.
    pub fn from_serialize<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Self> {
        serde_json::value::to_raw_value(value).map(Self::from_raw)
    }

    fn from_raw(raw: Box<RawValue>) -> Self {
        Self {
            snapshot: Arc::new(JsonSnapshot {
                end: raw.get().len(),
                raw: raw.into(),
                start: 0,
            }),
            cache: OnceLock::new(),
        }
    }

    /// The serialized JSON value, without any additional string quoting.
    #[must_use]
    pub fn as_raw(&self) -> &str {
        &self.snapshot.raw.get()[self.snapshot.start..self.snapshot.end]
    }

    /// Formats every nested container with two-space indentation while
    /// retaining the exact JSON scalar and key tokens.
    #[must_use]
    pub fn to_pretty_string(&self) -> String {
        pretty::format(self)
    }

    /// Formats the parsed value as ECMAScript `JSON.stringify`, including
    /// binary64 numbers, object property ordering, and well-formed strings.
    #[must_use]
    pub fn stringify(&self) -> String {
        javascript::stringify(self.as_ref(), false)
    }

    /// Formats the parsed value as ECMAScript `JSON.stringify(value, null, 2)`.
    #[must_use]
    pub fn stringify_pretty(&self) -> String {
        javascript::stringify(self.as_ref(), true)
    }

    fn child(&self, value: JsonRef<'_>) -> Self {
        let relative = value.raw.as_ptr().addr() - self.as_raw().as_ptr().addr();
        Self {
            snapshot: Arc::new(JsonSnapshot {
                raw: self.snapshot.raw.clone(),
                start: self.snapshot.start + relative,
                end: self.snapshot.start + relative + value.raw.len(),
            }),
            cache: OnceLock::new(),
        }
    }

    fn cache(&self) -> &JsonCache {
        self.cache.get_or_init(Box::default)
    }

    fn children(&self) -> &JsonChildren {
        self.cache().children.get_or_init(|| {
            if let Some(values) = self.array_items() {
                JsonChildren::Array(values.into_iter().map(|value| self.child(value)).collect())
            } else if let Some(entries) = self.object_entries() {
                JsonChildren::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| {
                            (
                                key.to_utf16()
                                    .expect("validated JSON object keys are strings"),
                                self.child(value),
                            )
                        })
                        .collect(),
                )
            } else {
                JsonChildren::Scalar
            }
        })
    }

    /// Borrows a UTF-8 string only when its complete UTF-16 sequence is representable.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        if !self.is_string() {
            return None;
        }
        self.as_serde_json()?.as_str()
    }

    /// Reads a boolean value.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        self.as_ref().as_bool()
    }

    /// Reads an unsigned integer without numeric rounding.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.as_ref().as_u64()
    }

    /// Reads a signed integer without numeric rounding.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.as_ref().as_i64()
    }

    /// Reads a JSON number as an ECMAScript number.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.as_ref().as_f64()
    }

    /// Whether this value is null.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.as_ref().is_null()
    }

    /// Whether this value is a string.
    #[must_use]
    pub fn is_string(&self) -> bool {
        self.as_ref().is_string()
    }

    /// Whether this value is an array.
    #[must_use]
    pub fn is_array(&self) -> bool {
        self.as_ref().is_array()
    }

    /// Whether this value is an object.
    #[must_use]
    pub fn is_object(&self) -> bool {
        self.as_ref().is_object()
    }

    /// Borrows lossless array elements. Each child shares immutable serialized
    /// bytes with the parent and remains independently mutable after cloning.
    #[must_use]
    pub fn as_array(&self) -> Option<&Vec<Self>> {
        match self.children() {
            JsonChildren::Array(values) => Some(values),
            JsonChildren::Scalar | JsonChildren::Object(_) => None,
        }
    }

    /// Borrows the Unicode-scalar representation when that representation can
    /// retain every string and key. Returns `None` when it cannot.
    #[must_use]
    pub fn as_serde_json(&self) -> Option<&Value> {
        self.cache()
            .serde_value
            .get_or_init(|| self.clone().try_into_serde_json().ok())
            .as_ref()
    }

    /// Deserializes this snapshot directly, preserving nested raw JSON fields.
    ///
    /// # Errors
    ///
    /// Returns an error if the destination cannot represent this JSON value.
    pub fn deserialize<'de, T: Deserialize<'de>>(&'de self) -> serde_json::Result<T> {
        serde_json::from_str(self.as_raw())
    }

    /// Borrows the snapshot for structural access without copying subtrees.
    #[must_use]
    pub fn as_ref(&self) -> JsonRef<'_> {
        JsonRef { raw: self.as_raw() }
    }

    /// Traverses the snapshot once, retaining strings and keys as raw JSON.
    #[must_use]
    pub fn tokens(&self) -> JsonTokens<'_> {
        self.as_ref().tokens()
    }

    /// Returns the named object's own value, preserving its JSON representation.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<JsonRef<'_>> {
        self.as_ref().get(key)
    }

    /// Borrows an object's cached lossless child, distinguishing a missing key
    /// from a present null value.
    #[must_use]
    pub fn get_value(&self, key: &str) -> Option<&Self> {
        let JsonChildren::Object(values) = self.children() else {
            return None;
        };
        values.get(&key.encode_utf16().collect::<Vec<_>>())
    }

    /// Resolves a JSON Pointer without converting application strings or keys.
    #[must_use]
    pub fn pointer(&self, pointer: &str) -> Option<JsonRef<'_>> {
        self.as_ref().pointer(pointer)
    }

    /// Borrows an array's immediate values without converting their strings.
    #[must_use]
    pub fn array_items(&self) -> Option<Vec<JsonRef<'_>>> {
        self.as_ref().array_items()
    }

    /// Borrows an object's immediate string keys and values in source order.
    #[must_use]
    pub fn object_entries(&self) -> Option<Vec<(JsonRef<'_>, JsonRef<'_>)>> {
        self.as_ref().object_entries()
    }

    /// Converts a snapshot to the Unicode-scalar JSON representation.
    ///
    /// # Errors
    ///
    /// Returns an error if any string or key contains a lone UTF-16 surrogate,
    /// or a numeric value cannot be represented by the destination.
    #[expect(
        clippy::missing_panics_doc,
        reason = "the traversal only receives validated JSON"
    )]
    pub fn try_into_serde_json(self) -> serde_json::Result<Value> {
        enum Frame {
            Array(Vec<Value>),
            Object {
                values: serde_json::Map<String, Value>,
                key: Option<String>,
            },
        }
        for token in self.tokens() {
            if let JsonToken::Key(value) | JsonToken::Scalar(value) = token {
                let _: Value = serde_json::from_str(value.as_raw())?;
            }
        }
        let mut frames = Vec::<Frame>::new();
        let mut root = None;
        for token in self.tokens() {
            let value = match token {
                JsonToken::ArrayStart => {
                    frames.push(Frame::Array(Vec::new()));
                    continue;
                }
                JsonToken::ObjectStart => {
                    frames.push(Frame::Object {
                        values: serde_json::Map::new(),
                        key: None,
                    });
                    continue;
                }
                JsonToken::Key(key) => {
                    if let Some(Frame::Object { key: slot, .. }) = frames.last_mut() {
                        *slot = Some(
                            serde_json::from_str(key.as_raw())
                                .expect("object key scalar conversion was validated"),
                        );
                    }
                    continue;
                }
                JsonToken::Scalar(value) => {
                    serde_json::from_str(value.as_raw()).expect("scalar conversion was validated")
                }
                JsonToken::ArrayEnd | JsonToken::ObjectEnd => match frames.pop() {
                    Some(Frame::Array(values)) => Value::Array(values),
                    Some(Frame::Object { values, .. }) => Value::Object(values),
                    None => unreachable!("validated JSON containers are balanced"),
                },
            };
            match frames.last_mut() {
                Some(Frame::Array(values)) => values.push(value),
                Some(Frame::Object { values, key }) => {
                    let key = key.take().expect("validated JSON object values have keys");
                    values.insert(key, value);
                }
                None => root = Some(value),
            }
        }
        Ok(root.expect("validated JSON contains one value"))
    }

    /// Encodes a JavaScript string, preserving lone surrogate code units.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "only internally encoded JSON is parsed"
    )]
    pub fn from_utf16(units: &[u16]) -> Self {
        Self::parse(encode_utf16(units)).expect("UTF-16 string encoding produces valid JSON")
    }

    /// Decodes a string root into its exact JavaScript UTF-16 code units.
    /// Returns `None` when this value is not a string.
    #[must_use]
    pub fn to_utf16(&self) -> Option<Vec<u16>> {
        self.as_ref().to_utf16()
    }

    /// Builds an array without reparsing application strings through UTF-8.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "array elements are already validated JSON"
    )]
    pub fn array(items: &[Self]) -> Self {
        let mut json = String::from("[");
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(item.as_raw());
        }
        json.push(']');
        Self::parse(json).expect("validated JSON items form a valid array")
    }

    /// Builds an object in insertion order. Repeated keys replace their value
    /// while keeping their original position.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "keys and values are already validated JSON"
    )]
    pub fn object<K: Into<JsonString>>(entries: impl IntoIterator<Item = (K, Self)>) -> Self {
        let mut indices = BTreeMap::<Vec<u16>, usize>::new();
        let mut values = Vec::<(JsonString, Self)>::new();
        for (key, value) in entries {
            let key = key.into();
            if let Some(index) = indices.get(key.utf16_units()) {
                values[*index].1 = value;
            } else {
                indices.insert(key.to_utf16(), values.len());
                values.push((key, value));
            }
        }
        let mut raw = String::from("{");
        let mut count = 0;
        for (key, value) in values {
            append_entry(&mut raw, &mut count, key.as_raw(), value.as_raw());
        }
        raw.push('}');
        Self::parse(raw).expect("validated entries form a JSON object")
    }

    /// Inserts an object's own value without converting any existing payload
    /// strings. Returns the previous value for that key, if present.
    ///
    /// # Errors
    ///
    /// Returns an error when this value is not an object.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "container mutation takes ownership of inserted values"
    )]
    pub fn insert<K: Into<JsonString>>(
        &mut self,
        key: K,
        value: Self,
    ) -> serde_json::Result<Option<Self>> {
        let entries = self
            .object_entries()
            .ok_or_else(|| expected_type("object"))?;
        let key = key.into();
        let mut raw = String::from("{");
        let mut count = 0;
        let mut previous = None;
        for (existing_key, existing_value) in entries {
            if existing_key.to_utf16().as_deref() == Some(key.utf16_units()) {
                if previous.is_none() {
                    append_entry(&mut raw, &mut count, existing_key.as_raw(), value.as_raw());
                }
                previous = Some(existing_value.to_owned());
            } else {
                append_entry(
                    &mut raw,
                    &mut count,
                    existing_key.as_raw(),
                    existing_value.as_raw(),
                );
            }
        }
        if previous.is_none() {
            append_entry(&mut raw, &mut count, key.as_raw(), value.as_raw());
        }
        raw.push('}');
        *self = Self::parse(raw)?;
        Ok(previous)
    }

    /// Removes an object's own key, retaining every remaining JSON token.
    ///
    /// # Errors
    ///
    /// Returns an error when this value is not an object.
    pub fn remove<K: Into<JsonString>>(&mut self, key: K) -> serde_json::Result<Option<Self>> {
        let entries = self
            .object_entries()
            .ok_or_else(|| expected_type("object"))?;
        let key = key.into();
        let mut raw = String::from("{");
        let mut count = 0;
        let mut previous = None;
        for (existing_key, existing_value) in entries {
            if existing_key.to_utf16().as_deref() == Some(key.utf16_units()) {
                previous = Some(existing_value.to_owned());
            } else {
                append_entry(
                    &mut raw,
                    &mut count,
                    existing_key.as_raw(),
                    existing_value.as_raw(),
                );
            }
        }
        raw.push('}');
        *self = Self::parse(raw)?;
        Ok(previous)
    }

    /// Appends one array value without converting its strings.
    ///
    /// # Errors
    ///
    /// Returns an error when this value is not an array.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "container mutation takes ownership of inserted values"
    )]
    pub fn push(&mut self, value: Self) -> serde_json::Result<()> {
        if !self.as_ref().is_array() {
            return Err(expected_type("array"));
        }
        let mut raw = self.as_raw()[..self.as_raw().len() - 1].to_owned();
        if !raw[1..].trim().is_empty() {
            raw.push(',');
        }
        raw.push_str(value.as_raw());
        raw.push(']');
        *self = Self::parse(raw)?;
        Ok(())
    }
}

fn expected_type(expected: &str) -> serde_json::Error {
    <serde_json::Error as serde::de::Error>::custom(format!("expected a JSON {expected}"))
}

fn append_entry(raw: &mut String, count: &mut usize, key: &str, value: &str) {
    if *count > 0 {
        raw.push(',');
    }
    raw.push_str(key);
    raw.push(':');
    raw.push_str(value);
    *count += 1;
}

impl<'a> JsonRef<'a> {
    /// The serialized JSON value or quoted object key.
    #[must_use]
    pub const fn as_raw(self) -> &'a str {
        self.raw
    }

    /// Makes an independently owned snapshot of this value.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "borrowed JSON always comes from a validated snapshot"
    )]
    pub fn to_owned(self) -> JsonValue {
        JsonValue::parse(self.raw.to_owned()).expect("borrowed JSON is validated")
    }

    /// Whether this value is JSON null.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.raw == "null"
    }

    /// Whether this value is a JSON string.
    #[must_use]
    pub fn is_string(self) -> bool {
        self.raw.starts_with('"')
    }

    /// Reads a boolean value, or returns `None` for another JSON type.
    #[must_use]
    pub fn as_bool(self) -> Option<bool> {
        match self.raw {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// Reads a numeric value as a JavaScript number.
    #[must_use]
    pub fn as_f64(self) -> Option<f64> {
        if !matches!(self.raw.as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
            return None;
        }
        self.raw.parse().ok()
    }

    /// Reads an unsigned integer without rounding through a floating-point value.
    #[must_use]
    pub fn as_u64(self) -> Option<u64> {
        self.raw.parse().ok()
    }

    /// Reads a signed integer without rounding through a floating-point value.
    #[must_use]
    pub fn as_i64(self) -> Option<i64> {
        self.raw.parse().ok()
    }

    /// Deserializes this value directly without copying its serialized bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the destination cannot represent this JSON value.
    pub fn deserialize<T: Deserialize<'a>>(self) -> serde_json::Result<T> {
        serde_json::from_str(self.raw)
    }

    /// Whether this value is an array.
    #[must_use]
    pub fn is_array(self) -> bool {
        self.raw.starts_with('[')
    }

    /// Whether this value is an object.
    #[must_use]
    pub fn is_object(self) -> bool {
        self.raw.starts_with('{')
    }

    /// Decodes a string's exact JavaScript UTF-16 code units.
    #[must_use]
    pub fn to_utf16(self) -> Option<Vec<u16>> {
        let encoded = self.raw.strip_prefix('"')?.strip_suffix('"')?;
        // Every UTF-16 unit occupies at most one UTF-8 byte, so this never reallocates.
        let mut units = Vec::with_capacity(encoded.len());
        let mut rest = encoded;
        // Escapes reach here mostly one at a time, so each pass copies the whole literal run
        // between them instead of walking its characters individually.
        loop {
            let Some(index) = rest.find('\\') else {
                extend_units(&mut units, rest);
                return Some(units);
            };
            extend_units(&mut units, &rest[..index]);
            let escapes = &rest[index + 1..];
            match escapes.chars().next()? {
                '"' => units.push(u16::from(b'"')),
                '\\' => units.push(u16::from(b'\\')),
                '/' => units.push(u16::from(b'/')),
                'b' => units.push(0x8),
                'f' => units.push(0xc),
                'n' => units.push(u16::from(b'\n')),
                'r' => units.push(u16::from(b'\r')),
                't' => units.push(u16::from(b'\t')),
                'u' => {
                    // The four hex digits decode to their raw unit, so a lone surrogate survives.
                    let mut digits = escapes[1..].chars();
                    let mut unit = 0u16;
                    for _ in 0..4 {
                        unit = unit.checked_mul(16)?
                            + u16::try_from(digits.next()?.to_digit(16)?).ok()?;
                    }
                    units.push(unit);
                    let consumed = 1 + escapes[1..]
                        .chars()
                        .take(4)
                        .map(char::len_utf8)
                        .sum::<usize>();
                    rest = &escapes[consumed..];
                    continue;
                }
                _ => return None,
            }
            rest = &escapes[1..];
        }
    }

    /// Traverses this value once without constructing a recursive JSON tree.
    #[must_use]
    pub fn tokens(self) -> JsonTokens<'a> {
        JsonTokens {
            raw: self.raw,
            offset: 0,
        }
    }

    /// Whether this string decodes to exactly `other`.
    ///
    /// Comparing decoded text is what a `String` round trip would do, without the allocation, and
    /// a lone surrogate never equals a `&str` because no `&str` spells one.
    #[must_use]
    pub fn text_equals(self, other: &str) -> bool {
        let Some(body) = self
            .raw
            .strip_prefix('"')
            .and_then(|raw| raw.strip_suffix('"'))
        else {
            return false;
        };
        // Unescaped bodies spell the same text, so equal source bytes settle the comparison.
        if !body.as_bytes().contains(&b'\\') && needs_no_escape(other) {
            return body == other;
        }
        self.to_utf16()
            .is_some_and(|units| units == other.encode_utf16().collect::<Vec<_>>())
    }

    /// Returns an object's named own value. Keys are compared as UTF-16 units.
    #[must_use]
    pub fn get(self, key: &str) -> Option<Self> {
        self.raw.strip_prefix('{')?;
        let plain = needs_no_escape(key);
        let mut units = None;
        let mut found = None;
        let mut offset = 1;
        loop {
            skip_whitespace(self.raw, &mut offset);
            if self.raw.as_bytes()[offset] == b'}' {
                return found;
            }
            let start = offset;
            offset = string_end(self.raw, offset);
            let candidate = &self.raw[start..offset];
            skip_whitespace(self.raw, &mut offset);
            offset += 1;
            skip_whitespace(self.raw, &mut offset);
            let value_start = offset;
            offset = value_end(self.raw, value_start);
            // A body without escapes spells the same string in UTF-8 and UTF-16, so the source
            // bytes settle the comparison. Anything else still decodes first, and the last match
            // wins, exactly as comparing every decoded key would.
            let matched = if plain && !candidate.as_bytes().contains(&b'\\') {
                plain_key_matches(candidate, key)
            } else {
                let decoded = units.get_or_insert_with(|| key.encode_utf16().collect::<Vec<_>>());
                Self { raw: candidate }.to_utf16()? == *decoded
            };
            if matched {
                found = Some(Self {
                    raw: &self.raw[value_start..offset],
                });
            }
            skip_whitespace(self.raw, &mut offset);
            if self.raw.as_bytes()[offset] == b',' {
                offset += 1;
            }
        }
    }

    /// Resolves a JSON Pointer relative to this value.
    #[must_use]
    pub fn pointer(self, pointer: &str) -> Option<Self> {
        if pointer.is_empty() {
            return Some(self);
        }
        let mut current = self;
        for component in pointer.strip_prefix('/')?.split('/') {
            let component = component.replace("~1", "/").replace("~0", "~");
            current = if current.is_object() {
                current.get(&component)?
            } else {
                if component.starts_with('+') || component.starts_with('0') && component.len() > 1 {
                    return None;
                }
                let index = component.parse::<usize>().ok()?;
                *current.array_items()?.get(index)?
            };
        }
        Some(current)
    }

    /// Borrows an array's immediate values in order.
    #[must_use]
    pub fn array_items(self) -> Option<Vec<Self>> {
        self.raw.strip_prefix('[')?;
        let mut values = Vec::new();
        let mut offset = 1;
        loop {
            skip_whitespace(self.raw, &mut offset);
            if self.raw.as_bytes()[offset] == b']' {
                return Some(values);
            }
            let start = offset;
            offset = value_end(self.raw, start);
            values.push(Self {
                raw: &self.raw[start..offset],
            });
            skip_whitespace(self.raw, &mut offset);
            if self.raw.as_bytes()[offset] == b',' {
                offset += 1;
            }
        }
    }

    /// Borrows an object's immediate string keys and values in source order.
    #[must_use]
    pub fn object_entries(self) -> Option<Vec<(Self, Self)>> {
        self.raw.strip_prefix('{')?;
        let mut entries = Vec::new();
        let mut offset = 1;
        loop {
            skip_whitespace(self.raw, &mut offset);
            if self.raw.as_bytes()[offset] == b'}' {
                return Some(entries);
            }
            let start = offset;
            offset = string_end(self.raw, offset);
            let key = Self {
                raw: &self.raw[start..offset],
            };
            skip_whitespace(self.raw, &mut offset);
            offset += 1;
            skip_whitespace(self.raw, &mut offset);
            let start = offset;
            offset = value_end(self.raw, start);
            entries.push((
                key,
                Self {
                    raw: &self.raw[start..offset],
                },
            ));
            skip_whitespace(self.raw, &mut offset);
            if self.raw.as_bytes()[offset] == b',' {
                offset += 1;
            }
        }
    }
}

fn skip_whitespace(raw: &str, offset: &mut usize) {
    while raw
        .as_bytes()
        .get(*offset)
        .is_some_and(u8::is_ascii_whitespace)
    {
        *offset += 1;
    }
}

/// Appends one literal run's UTF-16 units.
///
/// A run of ASCII copies one unit per byte without the width dispatch that encoding a character
/// costs, which is what most of a session's text is.
fn extend_units(units: &mut Vec<u16>, run: &str) {
    if run.is_ascii() {
        units.reserve(run.len());
        for byte in run.as_bytes() {
            units.push(u16::from(*byte));
        }
        return;
    }
    units.extend(run.encode_utf16());
}

/// Whether a text spells itself inside JSON, so no escape separates it from its source bytes.
fn needs_no_escape(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte != b'"' && byte != b'\\' && byte >= 0x20)
}

/// Whether a source key body is exactly `key`'s JSON spelling.
///
/// Both sides are unescaped, so equal source bytes mean equal decoded strings.
fn plain_key_matches(candidate: &str, key: &str) -> bool {
    candidate.len() == key.len() + 2
        && candidate.starts_with('"')
        && candidate.ends_with('"')
        && candidate.as_bytes()[1..=key.len()] == *key.as_bytes()
}

fn encode_utf16(units: &[u16]) -> String {
    let mut json = String::from("\"");
    let mut rest = units;
    while let Some(index) = rest.iter().position(|unit| unit_needs_escape(*unit)) {
        push_plain_units(&mut json, &rest[..index]);
        let unit = rest[index];
        // A high surrogate that pairs with the next unit is one astral scalar, not an escape;
        // only an unpaired one is written back as its own `\uXXXX`.
        if let Some(scalar) = paired_scalar(rest, index) {
            json.push(scalar);
            rest = &rest[index + 2..];
            continue;
        }
        match unit {
            unit if unit == u16::from(b'"') => json.push_str("\\\""),
            unit if unit == u16::from(b'\\') => json.push_str("\\\\"),
            0x8 => json.push_str("\\b"),
            0xc => json.push_str("\\f"),
            0xa => json.push_str("\\n"),
            0xd => json.push_str("\\r"),
            0x9 => json.push_str("\\t"),
            unit => write!(json, "\\u{unit:04x}").expect("writing to a String is infallible"),
        }
        rest = &rest[index + 1..];
    }
    push_plain_units(&mut json, rest);
    json.push('"');
    json
}

/// Whether one UTF-16 unit is escaped by JSON, or cannot stand alone as a Rust scalar.
fn unit_needs_escape(unit: u16) -> bool {
    unit < 0x20
        || unit == u16::from(b'"')
        || unit == u16::from(b'\\')
        || (0xd800..=0xdfff).contains(&unit)
}

/// The scalar a high surrogate and its low partner encode, when the pair is well formed.
fn paired_scalar(units: &[u16], index: usize) -> Option<char> {
    let high = *units.get(index)?;
    let low = *units.get(index + 1)?;
    if !(0xd800..=0xdbff).contains(&high) || !(0xdc00..=0xdfff).contains(&low) {
        return None;
    }
    let scalar = 0x1_0000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
    char::from_u32(scalar)
}

/// Appends a run that carries no escaped and no surrogate unit.
fn push_plain_units(json: &mut String, units: &[u16]) {
    if units.iter().all(|unit| *unit < 0x80) {
        let bytes = units
            .iter()
            .map(|unit| u8::try_from(*unit).expect("an ASCII run holds ASCII units"))
            .collect::<Vec<_>>();
        json.push_str(std::str::from_utf8(&bytes).expect("ASCII units spell ASCII"));
        return;
    }
    json.extend(
        char::decode_utf16(units.iter().copied())
            .map(|character| character.expect("a plain run holds no surrogate")),
    );
}

fn string_end(raw: &str, start: usize) -> usize {
    let bytes = raw.as_bytes();
    let mut index = start + 1;
    while bytes[index] != b'"' {
        index += if bytes[index] == b'\\' { 2 } else { 1 };
    }
    index + 1
}

fn scalar_end(raw: &str, start: usize) -> usize {
    if raw.as_bytes()[start] == b'"' {
        return string_end(raw, start);
    }
    let mut index = start;
    while raw
        .as_bytes()
        .get(index)
        .is_some_and(|byte| !matches!(byte, b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t'))
    {
        index += 1;
    }
    index
}

fn value_end(raw: &str, start: usize) -> usize {
    if !matches!(raw.as_bytes()[start], b'[' | b'{') {
        return scalar_end(raw, start);
    }
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut index = start;
    loop {
        match bytes[index] {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return index + 1;
                }
            }
            b'"' => {
                index = string_end(raw, index);
                continue;
            }
            _ => {}
        }
        index += 1;
    }
}

impl<'a> Iterator for JsonTokens<'a> {
    type Item = JsonToken<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self
            .raw
            .as_bytes()
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b',' | b':'))
        {
            self.offset += 1;
        }
        let first = *self.raw.as_bytes().get(self.offset)?;
        self.offset += 1;
        Some(match first {
            b'[' => JsonToken::ArrayStart,
            b']' => JsonToken::ArrayEnd,
            b'{' => JsonToken::ObjectStart,
            b'}' => JsonToken::ObjectEnd,
            _ => {
                let start = self.offset - 1;
                self.offset = scalar_end(self.raw, start);
                let value = JsonRef {
                    raw: &self.raw[start..self.offset],
                };
                let mut next = self.offset;
                skip_whitespace(self.raw, &mut next);
                if self.raw.as_bytes().get(next) == Some(&b':') {
                    JsonToken::Key(value)
                } else {
                    JsonToken::Scalar(value)
                }
            }
        })
    }
}

impl From<Value> for JsonValue {
    fn from(value: Value) -> Self {
        enum Task {
            Value(Value),
            Key(String),
            Punctuation(char),
        }
        let mut raw = String::new();
        let mut tasks = vec![Task::Value(value)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Punctuation(character) => raw.push(character),
                Task::Key(key) => {
                    raw.push_str(&serde_json::to_string(&key).expect("JSON keys serialize"));
                    raw.push(':');
                }
                Task::Value(Value::Array(values)) => {
                    raw.push('[');
                    tasks.push(Task::Punctuation(']'));
                    for (index, value) in values.into_iter().enumerate().rev() {
                        tasks.push(Task::Value(value));
                        if index > 0 {
                            tasks.push(Task::Punctuation(','));
                        }
                    }
                }
                Task::Value(Value::Object(values)) => {
                    raw.push('{');
                    tasks.push(Task::Punctuation('}'));
                    for (index, (key, value)) in values.into_iter().enumerate().rev() {
                        tasks.push(Task::Value(value));
                        tasks.push(Task::Key(key));
                        if index > 0 {
                            tasks.push(Task::Punctuation(','));
                        }
                    }
                }
                Task::Value(value) => {
                    raw.push_str(&serde_json::to_string(&value).expect("JSON scalars serialize"));
                }
            }
        }
        Self::parse(raw).expect("JSON value encoding produces valid JSON")
    }
}

impl From<serde_json::Map<String, Value>> for JsonValue {
    fn from(value: serde_json::Map<String, Value>) -> Self {
        Self::from(Value::Object(value))
    }
}

impl From<Vec<Value>> for JsonValue {
    fn from(value: Vec<Value>) -> Self {
        Self::from(Value::Array(value))
    }
}

impl Clone for JsonValue {
    fn clone(&self) -> Self {
        Self {
            snapshot: self.snapshot.clone(),
            cache: OnceLock::new(),
        }
    }
}

impl Drop for JsonValue {
    fn drop(&mut self) {
        let mut cached = Vec::new();
        drain_cache(self.cache.take(), &mut cached);
        while let Some(mut value) = cached.pop() {
            drain_cache(value.cache.take(), &mut cached);
        }
    }
}

fn drain_cache(cache: Option<Box<JsonCache>>, pending: &mut Vec<JsonValue>) {
    if let Some(mut cache) = cache {
        take_children(cache.children.take(), pending);
        drain_serde_value(cache.serde_value.take().flatten());
    }
}

fn take_children(children: Option<JsonChildren>, pending: &mut Vec<JsonValue>) {
    match children {
        Some(JsonChildren::Array(values)) => pending.extend(values),
        Some(JsonChildren::Object(values)) => pending.extend(values.into_values()),
        Some(JsonChildren::Scalar) | None => {}
    }
}

fn drain_serde_value(value: Option<Value>) {
    if let Some(value) = value {
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            match value {
                Value::Array(values) => pending.extend(values),
                Value::Object(values) => pending.extend(values.into_iter().map(|(_, value)| value)),
                _ => {}
            }
        }
    }
}

impl Serialize for JsonValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let raw: &RawValue =
            serde_json::from_str(self.as_raw()).expect("JSON snapshot slices are validated");
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<RawValue>::deserialize(deserializer).map(Self::from_raw)
    }
}

impl Index<&str> for JsonValue {
    type Output = Self;

    fn index(&self, key: &str) -> &Self::Output {
        self.get_value(key).unwrap_or(&NULL)
    }
}

impl Index<&String> for JsonValue {
    type Output = Self;

    fn index(&self, key: &String) -> &Self::Output {
        &self[key.as_str()]
    }
}

impl Index<String> for JsonValue {
    type Output = Self;

    fn index(&self, key: String) -> &Self::Output {
        &self[key.as_str()]
    }
}

impl Index<usize> for JsonValue {
    type Output = Self;

    fn index(&self, index: usize) -> &Self::Output {
        let JsonChildren::Array(values) = self.children() else {
            return &NULL;
        };
        values.get(index).unwrap_or(&NULL)
    }
}

impl fmt::Debug for JsonValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("JsonValue")
            .field(&self.as_raw())
            .finish()
    }
}

impl fmt::Display for JsonValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_raw())
    }
}

impl PartialEq for JsonValue {
    fn eq(&self, other: &Self) -> bool {
        self.as_raw() == other.as_raw() || json_equal(self, other)
    }
}

enum EqualityNode<'a> {
    Scalar(JsonRef<'a>),
    Array(Vec<usize>),
    Object(BTreeMap<Vec<u16>, usize>),
}

struct EqualityFrame {
    node: usize,
    key: Option<Vec<u16>>,
}

fn equality_nodes(value: &JsonValue) -> Vec<EqualityNode<'_>> {
    let mut nodes = Vec::new();
    let mut frames = Vec::<EqualityFrame>::new();
    for token in value.tokens() {
        let node = match token {
            JsonToken::ArrayStart => EqualityNode::Array(Vec::new()),
            JsonToken::ObjectStart => EqualityNode::Object(BTreeMap::new()),
            JsonToken::Scalar(value) => EqualityNode::Scalar(value),
            JsonToken::Key(key) => {
                frames
                    .last_mut()
                    .expect("JSON keys belong to an object")
                    .key = key.to_utf16();
                continue;
            }
            JsonToken::ArrayEnd | JsonToken::ObjectEnd => {
                frames.pop();
                continue;
            }
        };
        let index = nodes.len();
        nodes.push(node);
        if let Some(parent) = frames.last_mut() {
            match &mut nodes[parent.node] {
                EqualityNode::Array(values) => values.push(index),
                EqualityNode::Object(values) => {
                    values.insert(
                        parent.key.take().expect("JSON object values have keys"),
                        index,
                    );
                }
                EqualityNode::Scalar(_) => unreachable!("only containers are active JSON frames"),
            }
        }
        if matches!(token, JsonToken::ArrayStart | JsonToken::ObjectStart) {
            frames.push(EqualityFrame {
                node: index,
                key: None,
            });
        }
    }
    nodes
}

fn json_equal(left: &JsonValue, right: &JsonValue) -> bool {
    let left = equality_nodes(left);
    let right = equality_nodes(right);
    let mut pending = vec![(0, 0)];
    while let Some((left_index, right_index)) = pending.pop() {
        match (&left[left_index], &right[right_index]) {
            (EqualityNode::Scalar(left), EqualityNode::Scalar(right)) => {
                if left.is_string() && right.is_string() {
                    if left.to_utf16() != right.to_utf16() {
                        return false;
                    }
                } else if !matches!(
                    (left.deserialize::<Value>(), right.deserialize::<Value>()),
                    (Ok(left), Ok(right)) if left == right
                ) {
                    return false;
                }
            }
            (EqualityNode::Array(left), EqualityNode::Array(right))
                if left.len() == right.len() =>
            {
                pending.extend(left.iter().zip(right).map(|(left, right)| (*left, *right)));
            }
            (EqualityNode::Object(left), EqualityNode::Object(right))
                if left.len() == right.len() =>
            {
                for (key, left) in left {
                    let Some(right) = right.get(key) else {
                        return false;
                    };
                    pending.push((*left, *right));
                }
            }
            _ => return false,
        }
    }
    true
}

impl PartialEq<Value> for JsonValue {
    fn eq(&self, other: &Value) -> bool {
        let nodes = equality_nodes(self);
        let mut pending = vec![(0, other)];
        while let Some((index, value)) = pending.pop() {
            match (&nodes[index], value) {
                (EqualityNode::Scalar(raw), Value::String(text)) => {
                    if raw
                        .to_utf16()
                        .is_none_or(|units| !units.into_iter().eq(text.encode_utf16()))
                    {
                        return false;
                    }
                }
                (EqualityNode::Scalar(raw), Value::Null | Value::Bool(_) | Value::Number(_)) => {
                    if !raw.deserialize::<Value>().is_ok_and(|raw| raw == *value) {
                        return false;
                    }
                }
                (EqualityNode::Array(indices), Value::Array(values))
                    if indices.len() == values.len() =>
                {
                    pending.extend(indices.iter().copied().zip(values));
                }
                (EqualityNode::Object(indices), Value::Object(values))
                    if indices.len() == values.len() =>
                {
                    for (key, index) in indices {
                        let Ok(key) = String::from_utf16(key) else {
                            return false;
                        };
                        let Some(value) = values.get(&key) else {
                            return false;
                        };
                        pending.push((*index, value));
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

impl PartialEq<JsonValue> for Value {
    fn eq(&self, other: &JsonValue) -> bool {
        other == self
    }
}

impl PartialEq<str> for JsonValue {
    fn eq(&self, other: &str) -> bool {
        self.as_ref() == other
    }
}

impl PartialEq<&str> for JsonValue {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl PartialEq<String> for JsonValue {
    fn eq(&self, other: &String) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<JsonString> for JsonValue {
    fn eq(&self, other: &JsonString) -> bool {
        self.to_utf16().as_deref() == Some(other.utf16_units())
    }
}

impl PartialEq<JsonValue> for str {
    fn eq(&self, other: &JsonValue) -> bool {
        other == self
    }
}

impl PartialEq<JsonValue> for &str {
    fn eq(&self, other: &JsonValue) -> bool {
        other == self
    }
}

impl PartialEq<JsonValue> for String {
    fn eq(&self, other: &JsonValue) -> bool {
        other == self
    }
}

impl PartialEq<str> for JsonRef<'_> {
    fn eq(&self, other: &str) -> bool {
        self.to_utf16()
            .is_some_and(|units| units.into_iter().eq(other.encode_utf16()))
    }
}

impl PartialEq<&str> for JsonRef<'_> {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

macro_rules! scalar_comparison {
    ($type:ty, $method:ident, $convert:expr) => {
        impl PartialEq<$type> for JsonValue {
            fn eq(&self, other: &$type) -> bool {
                self.$method()
                    .zip(($convert)(*other))
                    .is_some_and(|(left, right)| left == right)
            }
        }
        impl PartialEq<JsonValue> for $type {
            fn eq(&self, other: &JsonValue) -> bool {
                other == self
            }
        }
    };
}

scalar_comparison!(bool, as_bool, Some);
scalar_comparison!(i8, as_i64, |value| Some(i64::from(value)));
scalar_comparison!(i16, as_i64, |value| Some(i64::from(value)));
scalar_comparison!(i32, as_i64, |value| Some(i64::from(value)));
scalar_comparison!(i64, as_i64, Some);
scalar_comparison!(isize, as_i64, |value| i64::try_from(value).ok());
scalar_comparison!(u8, as_u64, |value| Some(u64::from(value)));
scalar_comparison!(u16, as_u64, |value| Some(u64::from(value)));
scalar_comparison!(u32, as_u64, |value| Some(u64::from(value)));
scalar_comparison!(u64, as_u64, Some);
scalar_comparison!(usize, as_u64, |value| u64::try_from(value).ok());

impl PartialEq<f64> for JsonValue {
    fn eq(&self, other: &f64) -> bool {
        self.as_f64().is_some_and(|value| value == *other)
    }
}

impl PartialEq<JsonValue> for f64 {
    fn eq(&self, other: &JsonValue) -> bool {
        other == self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_decoding_matches_the_source_spelling_for_every_body() {
        for (json, expected) in [
            (
                r#""plain""#,
                vec![
                    u16::from(b'p'),
                    u16::from(b'l'),
                    u16::from(b'a'),
                    u16::from(b'i'),
                    u16::from(b'n'),
                ],
            ),
            (r#""""#, Vec::new()),
            (r#""\u0041\u00e9""#, vec![0x41, 0xe9]),
            (r#""\ud800""#, vec![0xd800]),
            (r#""\ud83d\ude00""#, vec![0xd83d, 0xde00]),
            (r#""😀""#, vec![0xd83d, 0xde00]),
            (
                r#""tab\there""#,
                vec![
                    u16::from(b't'),
                    u16::from(b'a'),
                    u16::from(b'b'),
                    9,
                    u16::from(b'h'),
                    u16::from(b'e'),
                    u16::from(b'r'),
                    u16::from(b'e'),
                ],
            ),
            (
                r#""a\\b""#,
                vec![u16::from(b'a'), u16::from(b'\\'), u16::from(b'b')],
            ),
        ] {
            let value = JsonValue::parse(json.to_owned()).unwrap();
            assert_eq!(value.as_ref().to_utf16(), Some(expected), "{json}");
        }
        let not_a_string = JsonValue::parse("12".to_owned()).unwrap();
        assert_eq!(not_a_string.as_ref().to_utf16(), None);

        // Assistant text is newline- and quote-heavy, so escapes interleave with long runs.
        let text = "first line\nsecond \"quoted\" line\ttabbed\n\u{1f600} done\n";
        let json = serde_json::to_string(text).unwrap();
        let value = JsonValue::parse(json.clone()).unwrap();
        assert_eq!(
            value.as_ref().to_utf16(),
            Some(text.encode_utf16().collect::<Vec<_>>()),
            "{json}"
        );
        assert!(json.contains("\\n") && json.contains("\\\""), "{json}");

        // A malformed escape still rejects the whole body.
        for malformed in [r#""a\q""#, r#""a\u12""#, r#""a\u12g4""#, r#""a\"#] {
            let value = JsonValue::parse(malformed.to_owned());
            if let Ok(value) = value {
                assert_eq!(value.as_ref().to_utf16(), None, "{malformed}");
            }
        }
    }

    #[test]
    fn object_lookup_resolves_escaped_and_duplicate_keys_like_a_decoded_scan() {
        // The last own key that decodes to the requested name wins, escaped spellings included.
        let value = JsonValue::parse(r#"{"a":1,"\u0061":2,"b":3}"#.to_owned()).unwrap();
        assert_eq!(value.as_ref().get("a").unwrap().as_raw(), "2");
        assert_eq!(value.as_ref().get("b").unwrap().as_raw(), "3");
        assert!(value.as_ref().get("c").is_none());

        // A requested name that itself needs escaping still matches its decoded spelling.
        let value = JsonValue::parse(r#"{"a\"b":7}"#.to_owned()).unwrap();
        assert_eq!(value.as_ref().get("a\"b").unwrap().as_raw(), "7");
        assert!(value.as_ref().get(r"a\b").is_none());

        // Non-ASCII names compare as UTF-16 units on both sides.
        let value = JsonValue::parse(r#"{"é":1,"😀":2}"#.to_owned()).unwrap();
        assert_eq!(value.as_ref().get("é").unwrap().as_raw(), "1");
        assert_eq!(value.as_ref().get("😀").unwrap().as_raw(), "2");
        assert!(value.as_ref().get("e").is_none());

        // Whitespace and nesting do not change the answer, and non-objects have no keys.
        let value = JsonValue::parse("  { \"k\" : { \"inner\" : 1 } }  ".to_owned()).unwrap();
        assert_eq!(
            value
                .as_ref()
                .get("k")
                .unwrap()
                .get("inner")
                .unwrap()
                .as_raw(),
            "1"
        );
        assert!(
            JsonValue::parse("[1,2]".to_owned())
                .unwrap()
                .as_ref()
                .get("k")
                .is_none()
        );
    }

    #[test]
    fn text_equality_matches_a_string_round_trip() {
        for (json, other, expected) in [
            (r#""plain""#, "plain", true),
            (r#""plain""#, "other", false),
            (r#""\u0070lain""#, "plain", true),
            (r#""a\nb""#, "a\nb", true),
            (r#""é😀""#, "é😀", true),
            (r#""\u00e9""#, "é", true),
            (r#""\ud83d\ude00""#, "😀", true),
            // No `&str` spells a lone surrogate, so a `String` round trip fails and so does this.
            (r#""\ud800""#, "\u{fffd}", false),
            (r#""a""#, "a\"b", false),
            (r#""""#, "", true),
        ] {
            let value = JsonValue::parse(json.to_owned()).unwrap();
            assert_eq!(
                value.as_ref().text_equals(other),
                expected,
                "{json} vs {other:?}"
            );
        }
        // A non-string never carries text.
        let number = JsonValue::parse("12".to_owned()).unwrap();
        assert!(!number.as_ref().text_equals("12"));
    }

    #[test]
    fn utf16_encoding_escapes_exactly_like_ecmascript_json() {
        let cases: Vec<(Vec<u16>, &str)> = vec![
            (Vec::new(), r#""""#),
            ("plain text".encode_utf16().collect(), r#""plain text""#),
            (vec![u16::from(b'"')], r#""\"""#),
            (vec![u16::from(b'\\')], r#""\\""#),
            (vec![0x8, 0xc, 0xa, 0xd, 0x9], r#""\b\f\n\r\t""#),
            (vec![0x1f, 0x0], r#""\u001f\u0000""#),
            (vec![0xd800], r#""\ud800""#),
            (vec![0xdfff], r#""\udfff""#),
            (vec![0xdc00, 0xd800], r#""\udc00\ud800""#),
            (vec![0xd83d, 0xde00], "\"😀\""),
            (vec![0xd83d, 0xd83d], r#""\ud83d\ud83d""#),
            (vec![0xd83d, 0xde00, 0xd800], "\"😀\\ud800\""),
            (vec![0x41, 0xa, 0xd83d, 0xde00, 0x42], "\"A\\n😀B\""),
            (vec![0xe9, 0x2764], "\"é❤\""),
        ];
        for (units, expected) in cases {
            assert_eq!(encode_utf16(&units), expected, "encode {units:?}");
            let value = JsonValue::parse(encode_utf16(&units)).unwrap();
            assert_eq!(
                value.as_ref().to_utf16(),
                Some(units.clone()),
                "decode {expected}"
            );
        }
    }

    #[test]
    fn retains_lone_surrogates_as_json_strings_and_object_keys() {
        for json in [r#""\ud800""#, r#"{"\ud800":["\udfff","😀","\\ud800"]}"#] {
            let value = JsonValue::parse(json.to_owned()).unwrap();
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            let decoded: JsonValue = serde_json::from_str(json).unwrap();
            assert_eq!(decoded.as_raw(), json);
            assert!(value.try_into_serde_json().is_err());
        }
    }

    #[test]
    fn strings_retain_every_code_unit_and_use_ecmascript_json_escaping() {
        let units = [0xd800, 0x61, 0xdc00, 0xd83d, 0xde00, 0x22, 0x5c, 0x0a, 0];
        let value = JsonValue::from_utf16(&units);
        assert_eq!(value.as_raw(), r#""\ud800a\udc00😀\"\\\n\u0000""#);
        assert_eq!(value.to_utf16().unwrap(), units);
        assert_eq!(JsonValue::from(Value::Null).to_utf16(), None);
    }

    #[test]
    fn raw_json_embeds_with_its_original_type() {
        #[derive(Serialize, Deserialize)]
        struct Envelope {
            value: JsonValue,
        }
        let encoded = r#"{"value":{"\ud800":"\udfff"}}"#;
        let envelope: Envelope = serde_json::from_str(encoded).unwrap();
        assert_eq!(serde_json::to_string(&envelope).unwrap(), encoded);
        for invalid in [r#""\uxxxx""#, "[1,]", "null true"] {
            assert!(JsonValue::parse(invalid.to_owned()).is_err());
        }
    }

    #[test]
    fn structural_access_preserves_keys_strings_and_subtree_boundaries() {
        let raw = r#"{"\ud800":["\udfff",{"brackets":"[}]\"\\"}],"ordinary":true,"null":null,"number":1.5}"#;
        let value = JsonValue::parse(raw.to_owned()).unwrap();
        let entries = value.object_entries().unwrap();
        assert_eq!(entries[0].0.to_utf16().unwrap(), [0xd800]);
        let array = entries[0].1.array_items().unwrap();
        assert_eq!(array[0].to_utf16().unwrap(), [0xdfff]);
        assert_eq!(
            String::from_utf16(&array[1].get("brackets").unwrap().to_utf16().unwrap()).unwrap(),
            "[}]\"\\"
        );
        assert_eq!(value.get("ordinary").unwrap().as_bool(), Some(true));
        assert!(value.get("null").unwrap().is_null());
        assert_eq!(value.get("number").unwrap().as_f64(), Some(1.5));
        assert!(value.get("missing").is_none());
        let string_units = value
            .tokens()
            .filter_map(|token| match token {
                JsonToken::Key(key) | JsonToken::Scalar(key) => key.to_utf16(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(string_units[0], [0xd800]);
        assert_eq!(string_units[1], [0xdfff]);
    }

    #[test]
    fn deeply_nested_json_parses_serializes_and_traverses_without_recursion() {
        let raw = format!("{}\"\\ud800\"{}", "[".repeat(14_000), "]".repeat(14_000));
        let value = JsonValue::parse(raw.clone()).unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), raw);
        assert_eq!(value.tokens().count(), 28_001);
        let child = value.array_items().unwrap()[0];
        assert_eq!(child.as_raw().len(), raw.len() - 2);
        let spaced = JsonValue::parse(raw.replace('[', "[ ")).unwrap();
        assert_eq!(value, spaced);
    }

    #[test]
    fn semantic_equality_decodes_strings_and_keys_and_ignores_object_order() {
        let left = JsonValue::parse(r#"{"\ud800":["\udfff"],"plain":true}"#.to_owned()).unwrap();
        let right =
            JsonValue::parse(r#"{ "plain":true, "\uD800" : ["\uDFFF"] }"#.to_owned()).unwrap();
        assert_eq!(left, right);
        let replacement = JsonValue::parse(r#"{"�":["�"],"plain":true}"#.to_owned()).unwrap();
        assert_ne!(left, replacement);
    }

    #[test]
    fn pointers_keep_subtree_strings_lossless_and_integer_indices_exact() {
        let value =
            JsonValue::parse(r#"{"a/b":{"~key":[{"value":"\ud800"}]}}"#.to_owned()).unwrap();
        assert_eq!(
            value.pointer("/a~1b/~0key/0/value").unwrap().to_utf16(),
            Some(vec![0xd800])
        );
        assert!(value.pointer("/a~1b/~0key/00").is_none());
        assert!(value.pointer("/a~1b/~0key/+0").is_none());
        assert_eq!(value.pointer("").unwrap().as_raw(), value.as_raw());
    }

    #[test]
    fn ordinary_value_conversion_handles_deep_trees_without_recursive_encoding() {
        let mut standard = Value::Bool(true);
        for _ in 0..14_000 {
            standard = Value::Array(vec![standard]);
        }
        let raw = JsonValue::from(standard);
        let standard = raw.clone().try_into_serde_json().unwrap();
        assert_eq!(raw, JsonValue::from(standard));
        assert_eq!(&raw, raw.as_serde_json().unwrap());
        let cloned = raw.clone();
        assert_eq!(cloned, raw);
        let rejected = JsonValue::array(&[raw, JsonValue::from_utf16(&[0xd800])]);
        assert!(rejected.try_into_serde_json().is_err());
    }

    #[test]
    fn ordinary_value_deserializers_and_scalar_caches_remain_compatible() {
        let input = serde_json::json!({"string":"hello", "number":42, "array":[true, null]});
        let raw: JsonValue = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(raw.as_serde_json(), Some(&input));
        let text: JsonString = serde_json::from_value(Value::String("hello".to_owned())).unwrap();
        assert_eq!(text.as_str(), Some("hello"));
        let unpaired = JsonValue::from_utf16(&[0xd800]);
        assert!(unpaired.as_serde_json().is_none());
        assert!(unpaired.try_into_serde_json().is_err());
    }

    #[test]
    fn object_and_array_mutation_keeps_real_json_types_and_unrelated_tokens() {
        let key = JsonString::from_utf16(&[0xd800]);
        let value = JsonValue::from_utf16(&[0xdfff]);
        let mut object = JsonValue::object([(key.clone(), value.clone())]);
        assert_eq!(object.as_raw(), r#"{"\ud800":"\udfff"}"#);
        object
            .insert("messages", JsonValue::array(std::slice::from_ref(&value)))
            .unwrap();
        assert_eq!(
            object.get("messages").unwrap().array_items().unwrap()[0].to_utf16(),
            Some(vec![0xdfff])
        );
        assert_eq!(object.remove(key).unwrap(), Some(value.clone()));
        let mut array = object.remove("messages").unwrap().unwrap();
        array.push(value).unwrap();
        assert_eq!(array.as_raw(), r#"["\udfff","\udfff"]"#);
        assert_eq!(object.as_raw(), "{}");
        let mut quoted_key =
            JsonValue::parse(r#"{"\u0061":"\ud800","other":1}"#.to_owned()).unwrap();
        quoted_key
            .insert("added", JsonValue::from(Value::Null))
            .unwrap();
        assert_eq!(
            quoted_key.as_raw(),
            r#"{"\u0061":"\ud800","other":1,"added":null}"#
        );
        assert!(array.insert("bad", JsonValue::from(Value::Null)).is_err());
    }

    #[test]
    fn indexed_children_serialize_independently_without_changing_their_type() {
        let value = JsonValue::parse(
            r#"{"nested":{"value":"\ud800"},"number":42,"yes":true,"text":"\u0061"}"#.to_owned(),
        )
        .unwrap();
        assert_eq!(value["number"], 42);
        assert_eq!(value["yes"], true);
        assert_eq!(value["text"].as_str(), Some("a"));
        assert!(value["nested"]["value"].as_str().is_none());
        assert!(value["nested"]["value"].is_string());
        assert_eq!(value["nested"]["value"].to_utf16(), Some(vec![0xd800]));
        assert!(value["missing"].is_null());
        assert!(value.get("missing").is_none());
        let mut child = value["nested"].clone();
        child
            .insert("extra", JsonValue::from(Value::Bool(true)))
            .unwrap();
        assert!(value["nested"].get("extra").is_none());
        drop(value);
        assert_eq!(
            serde_json::to_string(&child).unwrap(),
            r#"{"value":"\ud800","extra":true}"#
        );
    }

    #[test]
    fn deeply_cached_index_children_drop_without_recursive_destruction() {
        let raw = format!("{}\"\\ud800\"{}", "[".repeat(14_000), "]".repeat(14_000));
        let value = JsonValue::parse(raw).unwrap();
        let mut nested = &value;
        for _ in 0..14_000 {
            nested = &nested[0];
        }
        assert_eq!(nested.to_utf16(), Some(vec![0xd800]));
        let leaf = nested.clone();
        drop(value);
        assert_eq!(leaf.as_raw(), r#""\ud800""#);
    }
}
