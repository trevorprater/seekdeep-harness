//! Nullability and ordered-map helpers.

use indexmap::IndexMap;

/// Returns whether an optional value is absent.
#[must_use]
pub const fn is_nullable<T>(value: &Option<T>) -> bool {
    value.is_none()
}

/// Returns whether an optional value is present.
#[must_use]
pub const fn is_non_nullable<T>(value: &Option<T>) -> bool {
    value.is_some()
}

/// Filters entries while retaining declaration order.
pub fn filter_keys<K, V>(
    object: &IndexMap<K, V>,
    mut predicate: impl FnMut(&K, &V) -> bool,
) -> IndexMap<K, V>
where
    K: Clone + Eq + std::hash::Hash,
    V: Clone,
{
    object
        .iter()
        .filter(|(key, value)| predicate(key, value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Maps values while retaining keys and declaration order.
pub fn map_values<K, T, U>(
    object: &IndexMap<K, T>,
    mut transform: impl FnMut(&T, &K) -> U,
) -> IndexMap<K, U>
where
    K: Clone + Eq + std::hash::Hash,
{
    object
        .iter()
        .map(|(key, value)| (key.clone(), transform(value, key)))
        .collect()
}

/// Selects named keys in the requested order.
pub fn pick<K, V>(object: &IndexMap<K, V>, keys: impl IntoIterator<Item = K>) -> IndexMap<K, V>
where
    K: Clone + Eq + std::hash::Hash,
    V: Clone,
{
    keys.into_iter()
        .filter_map(|key| object.get(&key).cloned().map(|value| (key, value)))
        .collect()
}

/// Returns a shallow copy without selected keys.
pub fn omit<K, V>(object: &IndexMap<K, V>, keys: impl IntoIterator<Item = K>) -> IndexMap<K, V>
where
    K: Clone + Eq + std::hash::Hash,
    V: Clone,
{
    let mut result = object.clone();
    for key in keys {
        result.shift_remove(&key);
    }
    result
}
