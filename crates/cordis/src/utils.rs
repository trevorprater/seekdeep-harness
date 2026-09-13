//! Identity-owned disposable collections and Rust error composition.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use parking_lot::Mutex;
use serde_json::Value;

struct DisposableState<T> {
    sequence: u64,
    values: BTreeMap<u64, Arc<T>>,
    latest: HashMap<usize, u64>,
}

impl<T> Default for DisposableState<T> {
    fn default() -> Self {
        Self {
            sequence: 0,
            values: BTreeMap::new(),
            latest: HashMap::new(),
        }
    }
}

/// Ordered identity collection with exact returned-handle deletion.
pub struct DisposableList<T> {
    state: Arc<Mutex<DisposableState<T>>>,
}

impl<T> Clone for DisposableList<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl<T> Default for DisposableList<T> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(DisposableState::default())),
        }
    }
}

impl<T> std::fmt::Debug for DisposableList<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DisposableList")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl<T> DisposableList<T> {
    /// Creates an empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of registrations, including repeated values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.lock().values.len()
    }

    /// Whether no registrations remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends one identity and returns its exact removal handle.
    pub fn push(&self, value: Arc<T>) -> DisposableListHandle<T> {
        let identity = Arc::as_ptr(&value).cast::<()>() as usize;
        let mut state = self.state.lock();
        state.sequence += 1;
        let sequence = state.sequence;
        state.values.insert(sequence, value);
        state.latest.insert(identity, sequence);
        DisposableListHandle {
            state: Arc::downgrade(&self.state),
            sequence,
        }
    }

    /// Removes the most recently pushed registration for this identity.
    pub fn delete(&self, value: &Arc<T>) -> bool {
        let identity = Arc::as_ptr(value).cast::<()>() as usize;
        let mut state = self.state.lock();
        let Some(sequence) = state.latest.get(&identity).copied() else {
            return false;
        };
        state.values.remove(&sequence).is_some()
    }

    /// Insertion-order snapshot.
    #[must_use]
    pub fn values(&self) -> Vec<Arc<T>> {
        self.state.lock().values.values().cloned().collect()
    }

    /// Clears the list and returns values in reverse registration order.
    pub fn clear(&self) -> Vec<Arc<T>> {
        let mut state = self.state.lock();
        let values = state.values.values().rev().cloned().collect();
        state.values.clear();
        values
    }
}

/// Exact registration-removal closure counterpart.
pub struct DisposableListHandle<T> {
    state: std::sync::Weak<Mutex<DisposableState<T>>>,
    sequence: u64,
}

impl<T> std::fmt::Debug for DisposableListHandle<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DisposableListHandle")
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl<T> DisposableListHandle<T> {
    /// Removes this exact registration once.
    pub fn dispose(&self) -> bool {
        self.state
            .upgrade()
            .is_some_and(|state| state.lock().values.remove(&self.sequence).is_some())
    }
}

/// JSON-world counterpart of the source's non-null object predicate.
#[must_use]
pub const fn is_json_object_like(value: &Value) -> bool {
    matches!(value, Value::Array(_) | Value::Object(_))
}

/// Preserves a Rust error chain while adding the outer operation boundary.
///
/// # Errors
///
/// Returns the callback error with `outer` appended as context.
pub fn compose_error<T>(
    outer: &str,
    callback: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    callback().map_err(|error| error.context(outer.to_owned()))
}
