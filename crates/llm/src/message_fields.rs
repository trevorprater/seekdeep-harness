//! Object-valued message extensions with exact JSON strings and keys.

use std::ops::Index;

use seekdeep_lossless_json::{JsonRef, JsonString, JsonValue};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};

/// An insertion-ordered JSON object containing opaque message fields.
///
/// Keys retain every UTF-16 code unit. Repeated keys replace their value while
/// keeping the first position. Nested values retain their serialized JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageFields(JsonValue);

impl MessageFields {
    /// Creates an empty object.
    #[must_use]
    pub fn new() -> Self {
        Self(JsonValue::object(
            std::iter::empty::<(JsonString, JsonValue)>(),
        ))
    }

    /// Borrows the authoritative JSON object.
    #[must_use]
    pub const fn as_json(&self) -> &JsonValue {
        &self.0
    }

    /// Transfers ownership of the JSON object.
    #[must_use]
    pub fn into_json(self) -> JsonValue {
        self.0
    }

    /// Borrows a named field, distinguishing absence from a present null value.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.0.get_value(key)
    }

    /// Borrows a field whose key may contain unpaired UTF-16 surrogates.
    #[must_use]
    pub fn get_key(&self, key: &JsonString) -> Option<JsonRef<'_>> {
        self.0
            .object_entries()?
            .into_iter()
            .rev()
            .find_map(|(name, value)| {
                (name.to_utf16().as_deref() == Some(key.utf16_units())).then_some(value)
            })
    }

    /// Whether the object owns the named field, including null values.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Number of distinct fields.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().len()
    }

    /// Whether the object has no fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns owned keys and values in insertion order.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "construction and mutation maintain a validated JSON object"
    )]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (JsonString, JsonValue)> + '_ {
        self.0
            .object_entries()
            .expect("message fields are a JSON object")
            .into_iter()
            .map(|(key, value)| {
                (
                    key.deserialize().expect("object keys are JSON strings"),
                    value.to_owned(),
                )
            })
    }

    /// Returns owned keys in insertion order, retaining their exact code units.
    #[must_use]
    pub fn keys(&self) -> impl ExactSizeIterator<Item = JsonString> + '_ {
        self.iter().map(|(key, _)| key)
    }

    /// Returns owned field values in insertion order.
    #[must_use]
    pub fn values(&self) -> impl ExactSizeIterator<Item = JsonValue> + '_ {
        self.iter().map(|(_, value)| value)
    }

    /// Replaces or appends a field and returns its previous value.
    /// Existing fields retain their position.
    #[expect(
        clippy::missing_panics_doc,
        reason = "construction and mutation maintain a validated JSON object"
    )]
    pub fn insert<K: Into<JsonString>, V: Into<JsonValue>>(
        &mut self,
        key: K,
        value: V,
    ) -> Option<JsonValue> {
        self.0
            .insert(key, value.into())
            .expect("message fields are a JSON object")
    }

    /// Removes a field without reordering the remaining fields.
    #[expect(
        clippy::missing_panics_doc,
        reason = "construction and mutation maintain a validated JSON object"
    )]
    pub fn remove<K: Into<JsonString>>(&mut self, key: K) -> Option<JsonValue> {
        self.0
            .remove(key)
            .expect("message fields are a JSON object")
    }

    /// Removes a field without reordering the remaining fields.
    pub fn shift_remove<K: Into<JsonString>>(&mut self, key: K) -> Option<JsonValue> {
        self.remove(key)
    }
}

impl Default for MessageFields {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Map<String, Value>> for MessageFields {
    fn from(value: Map<String, Value>) -> Self {
        Self(Value::Object(value).into())
    }
}

impl TryFrom<JsonValue> for MessageFields {
    type Error = serde_json::Error;

    fn try_from(value: JsonValue) -> Result<Self, Self::Error> {
        let entries = value
            .object_entries()
            .ok_or_else(|| Self::Error::custom("message fields must be a JSON object"))?;
        let fields = entries
            .into_iter()
            .map(|(key, value)| Ok((key.deserialize::<JsonString>()?, value.to_owned())))
            .collect::<Result<Vec<_>, Self::Error>>()?;
        Ok(fields.into_iter().collect())
    }
}

impl<K: Into<JsonString>, V: Into<JsonValue>> FromIterator<(K, V)> for MessageFields {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self(JsonValue::object(
            iter.into_iter().map(|(key, value)| (key, value.into())),
        ))
    }
}

impl<K: Into<JsonString>, V: Into<JsonValue>> Extend<(K, V)> for MessageFields {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T) {
        let fields = self
            .iter()
            .chain(
                iter.into_iter()
                    .map(|(key, value)| (key.into(), value.into())),
            )
            .collect::<Self>();
        *self = fields;
    }
}

impl<K: AsRef<str>> Index<K> for MessageFields {
    type Output = JsonValue;

    fn index(&self, key: K) -> &Self::Output {
        self.get(key.as_ref()).expect("no entry found for key")
    }
}

impl Serialize for MessageFields {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MessageFields {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(<JsonValue as Deserialize>::deserialize(deserializer)?)
            .map_err(D::Error::custom)
    }
}

impl From<MessageFields> for JsonValue {
    fn from(value: MessageFields) -> Self {
        value.into_json()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rejects_non_objects_and_keeps_explicit_null_fields() {
        for raw in ["null", "[]", "7", "true", r#""text""#] {
            assert!(serde_json::from_str::<MessageFields>(raw).is_err());
        }
        let fields: MessageFields = serde_json::from_str(r#"{"value":null}"#).unwrap();
        assert!(fields.contains_key("value"));
        assert!(fields.get("value").unwrap().is_null());
        assert!(fields.get("missing").is_none());
    }

    #[test]
    fn mutations_preserve_unicode_keys_nested_tokens_and_field_order() {
        let mut fields: MessageFields = serde_json::from_str(
            r#"{"first":1,"\ud800":{"n":1.2300,"text":"\udfff"},"literal":"\\ud800","first":2}"#,
        )
        .unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(
            fields.as_json().as_raw(),
            r#"{"first":2,"\ud800":{"n":1.2300,"text":"\udfff"},"literal":"\\ud800"}"#
        );
        let key = JsonString::from_utf16(&[0xd800]);
        assert_eq!(
            fields.get_key(&key).unwrap().as_raw(),
            r#"{"n":1.2300,"text":"\udfff"}"#
        );
        let original = fields.clone();
        assert_eq!(fields.insert("first", json!(3)), Some(json!(2).into()));
        assert_eq!(
            fields.shift_remove("literal"),
            Some(json!("\\ud800").into())
        );
        fields.extend([("tail", json!(true))]);
        assert_eq!(
            fields.keys().map(|key| key.to_utf16()).collect::<Vec<_>>(),
            [
                "first".encode_utf16().collect(),
                vec![0xd800],
                "tail".encode_utf16().collect()
            ]
        );
        assert_eq!(original["first"], 2);
        assert_eq!(fields.values().count(), fields.len());
        assert!(fields.remove(key).is_some());
        assert!(fields.remove("missing").is_none());
        assert_eq!(
            serde_json::to_string(&fields).unwrap(),
            r#"{"first":3,"tail":true}"#
        );
    }
}
