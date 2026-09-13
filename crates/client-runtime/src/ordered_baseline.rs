//! Authoritative baseline reconciliation that retains established visible order.

use std::{collections::HashMap, hash::Hash};

/// Merges baseline values while keeping the relative order of identities already visible.
#[must_use]
pub fn merge_ordered_baseline<T, K>(
    current: &[T],
    baseline: &[T],
    key_of: impl Fn(&T) -> K,
) -> Vec<T>
where
    T: Clone,
    K: Clone + Eq + Hash,
{
    let baseline_by_key = baseline
        .iter()
        .map(|value| (key_of(value), value.clone()))
        .collect::<HashMap<_, _>>();
    let mut merged = current
        .iter()
        .filter_map(|value| baseline_by_key.get(&key_of(value)).cloned())
        .collect::<Vec<_>>();
    let mut merged_keys = merged.iter().map(&key_of).collect::<Vec<_>>();
    for (index, value) in baseline.iter().enumerate() {
        let key = key_of(value);
        if merged_keys.contains(&key) {
            continue;
        }
        let insertion = baseline[index + 1..]
            .iter()
            .find_map(|candidate| {
                let candidate = key_of(candidate);
                merged.iter().position(|item| key_of(item) == candidate)
            })
            .unwrap_or(merged.len());
        merged.insert(insertion, value.clone());
        merged_keys.insert(insertion, key);
    }
    merged
}
