use std::ops::Index;

use indexmap::IndexMap;
use seekdeep_core::session::JsonValue;
use seekdeep_llm::JsonString;

#[derive(Clone, Default)]
pub(super) struct RawObject(IndexMap<Vec<u16>, JsonValue>);

impl RawObject {
    pub(super) fn from_value(value: &JsonValue) -> Option<Self> {
        Some(Self(
            value
                .object_entries()?
                .into_iter()
                .map(|(key, value)| {
                    (
                        key.to_utf16().expect("JSON object keys are strings"),
                        value.to_owned(),
                    )
                })
                .collect(),
        ))
    }

    pub(super) fn get(&self, key: &str) -> Option<&JsonValue> {
        self.0.get(&key.encode_utf16().collect::<Vec<_>>())
    }

    pub(super) fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub(super) fn remove(&mut self, key: &str) -> Option<JsonValue> {
        self.0.shift_remove(&key.encode_utf16().collect::<Vec<_>>())
    }

    pub(super) fn insert(&mut self, key: impl AsRef<str>, value: impl Into<JsonValue>) {
        self.0
            .insert(key.as_ref().encode_utf16().collect(), value.into());
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &Vec<u16>> {
        self.0.keys()
    }

    pub(super) fn into_value(self) -> JsonValue {
        let mut entries = self.0.into_iter().collect::<Vec<_>>();
        entries.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |index| (0, index)));
        JsonValue::object(
            entries
                .into_iter()
                .map(|(key, value)| (JsonString::from_utf16(&key), value)),
        )
    }
}

fn array_index(key: &[u16]) -> Option<u32> {
    if key.is_empty() || key.len() > 1 && key[0] == u16::from(b'0') {
        return None;
    }
    let mut index = 0u32;
    for unit in key {
        let digit = unit.checked_sub(u16::from(b'0'))?;
        if digit > 9 {
            return None;
        }
        index = index.checked_mul(10)?.checked_add(u32::from(digit))?;
    }
    (index < u32::MAX).then_some(index)
}

impl Index<&str> for RawObject {
    type Output = JsonValue;

    fn index(&self, key: &str) -> &Self::Output {
        self.get(key)
            .expect("validated object contains the named field")
    }
}

pub(super) fn object<const N: usize>(fields: [(&str, JsonValue); N]) -> JsonValue {
    let mut object = RawObject::default();
    for (key, value) in fields {
        object.insert(key, value);
    }
    object.into_value()
}
