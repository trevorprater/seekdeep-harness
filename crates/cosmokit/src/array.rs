//! Array helpers compatible with the vendored `CosmoKit` behavior.

/// Returns whether every item in `subset` is present in `items`.
#[must_use]
pub fn contain<T: PartialEq>(items: &[T], subset: &[T]) -> bool {
    subset.iter().all(|item| items.contains(item))
}

/// Returns values that appear in both slices, retaining duplicates and order from `left`.
#[must_use]
pub fn intersection<T: PartialEq + Clone>(left: &[T], right: &[T]) -> Vec<T> {
    left.iter()
        .filter(|item| right.contains(item))
        .cloned()
        .collect()
}

/// Returns values from `left` that do not appear in `right`.
#[must_use]
pub fn difference<T: PartialEq + Clone>(left: &[T], right: &[T]) -> Vec<T> {
    left.iter()
        .filter(|item| !right.contains(item))
        .cloned()
        .collect()
}

/// Returns the first-occurrence union of two slices.
#[must_use]
pub fn union<T: PartialEq + Clone>(left: &[T], right: &[T]) -> Vec<T> {
    deduplicate(left.iter().chain(right).cloned())
}

/// Removes duplicate values while preserving first occurrence order.
#[must_use]
pub fn deduplicate<T: PartialEq + Clone>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut output = Vec::new();
    for item in items {
        if !output.contains(&item) {
            output.push(item);
        }
    }
    output
}

/// Removes the first equal item and reports whether it was found.
pub fn remove<T: PartialEq>(items: &mut Vec<T>, item: &T) -> bool {
    let Some(index) = items.iter().position(|candidate| candidate == item) else {
        return false;
    };
    items.remove(index);
    true
}

/// Normalizes an optional scalar to zero or one items.
pub fn make_array<T>(source: Option<T>) -> Vec<T> {
    source.into_iter().collect()
}

/// Rust representation of the source helper's nullish, scalar, or array union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaybeArray<T> {
    /// `null` or `undefined`.
    Nullish,
    /// One scalar value.
    Scalar(T),
    /// An already materialized array.
    Array(Vec<T>),
}

/// Normalizes the complete source union without nesting an existing array.
#[must_use]
pub fn make_array_source<T>(source: MaybeArray<T>) -> Vec<T> {
    match source {
        MaybeArray::Nullish => Vec::new(),
        MaybeArray::Scalar(value) => vec![value],
        MaybeArray::Array(values) => values,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_operations_preserve_javascript_array_order() {
        assert!(contain(&[1, 2, 3], &[3, 1]));
        assert_eq!(intersection(&[2, 1, 2, 3], &[2, 3]), [2, 2, 3]);
        assert_eq!(difference(&[2, 1, 2, 3], &[2]), [1, 3]);
        assert_eq!(union(&[2, 1, 2], &[3, 1]), [2, 1, 3]);
    }

    #[test]
    fn remove_deletes_only_the_first_match() {
        let mut items = vec![1, 2, 1];
        assert!(remove(&mut items, &1));
        assert_eq!(items, [2, 1]);
        assert!(!remove(&mut items, &3));
    }

    #[test]
    fn make_array_preserves_existing_arrays() {
        assert_eq!(
            make_array_source::<i32>(MaybeArray::Nullish),
            Vec::<i32>::new()
        );
        assert_eq!(make_array_source(MaybeArray::Scalar(1)), [1]);
        assert_eq!(make_array_source(MaybeArray::Array(vec![1, 2])), [1, 2]);
    }
}
