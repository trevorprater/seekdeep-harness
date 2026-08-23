//! Per-Session Host-computed projection values and observable key faces.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;

use crate::{Notifier, NotifierScheduler, RuntimeDisposer};

/// One history-tail consistent cut over all carried projection values.
#[derive(Clone, Debug)]
pub struct ProjectionsBaseline<T> {
    /// Sequence reflected by every carried value.
    pub as_of_seq: i64,
    /// Whole current values by projection key.
    pub values: IndexMap<String, Rc<T>>,
}

#[derive(Debug)]
struct ProjectionRow<T> {
    value: Rc<T>,
    seq: i64,
}

struct ProjectionState<T> {
    rows: IndexMap<String, ProjectionRow<T>>,
    values_cache: Option<Rc<IndexMap<String, Rc<T>>>>,
}

impl<T> Default for ProjectionState<T> {
    fn default() -> Self {
        Self {
            rows: IndexMap::new(),
            values_cache: None,
        }
    }
}

/// Identity-stable observable face for one projection key.
pub struct ProjectionFace<T> {
    key: String,
    state: Rc<RefCell<ProjectionState<T>>>,
    notifier: Rc<Notifier>,
}

impl<T> ProjectionFace<T> {
    /// Returns the current whole value, or absence before the key is carried.
    #[must_use]
    pub fn snapshot(&self) -> Option<Rc<T>> {
        self.state
            .borrow()
            .rows
            .get(&self.key)
            .map(|row| row.value.clone())
    }

    /// Subscribes to microtask-batched changes for this exact key.
    #[must_use]
    pub fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        self.notifier.subscribe(listener)
    }
}

/// One Session's finished Host-computed projection values.
pub struct ProjectionValueStore<T> {
    state: Rc<RefCell<ProjectionState<T>>>,
    faces: RefCell<IndexMap<String, Rc<ProjectionFace<T>>>>,
    scheduler: Rc<dyn NotifierScheduler>,
    any_notifier: Rc<Notifier>,
}

impl<T: 'static> ProjectionValueStore<T> {
    /// Creates an empty store with an injected publication scheduler.
    #[must_use]
    pub fn new(scheduler: Rc<dyn NotifierScheduler>) -> Self {
        Self {
            state: Rc::new(RefCell::new(ProjectionState::default())),
            faces: RefCell::new(IndexMap::new()),
            scheduler: scheduler.clone(),
            any_notifier: Notifier::new(Rc::new(|| {}), scheduler),
        }
    }

    /// Returns the identity-stable observable face for one key.
    #[must_use]
    pub fn face_of(&self, key: impl Into<String>) -> Rc<ProjectionFace<T>> {
        let key = key.into();
        if let Some(face) = self.faces.borrow().get(&key) {
            return face.clone();
        }
        let face = Rc::new(ProjectionFace {
            key: key.clone(),
            state: self.state.clone(),
            notifier: Notifier::new(Rc::new(|| {}), self.scheduler.clone()),
        });
        self.faces.borrow_mut().insert(key, face.clone());
        face
    }

    /// Returns one current whole value.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Rc<T>> {
        self.state
            .borrow()
            .rows
            .get(key)
            .map(|row| row.value.clone())
    }

    /// Returns the same aggregate snapshot until a row changes.
    #[must_use]
    pub fn values(&self) -> Rc<IndexMap<String, Rc<T>>> {
        let mut state = self.state.borrow_mut();
        if let Some(cache) = &state.values_cache {
            return cache.clone();
        }
        let values: Rc<IndexMap<String, Rc<T>>> = Rc::new(
            state
                .rows
                .iter()
                .map(|(key, row)| (key.clone(), row.value.clone()))
                .collect(),
        );
        state.values_cache = Some(values.clone());
        values
    }

    /// Subscribes to microtask-batched changes to any key.
    #[must_use]
    pub fn subscribe_any(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        self.any_notifier.subscribe(listener)
    }

    /// Applies one finished value when its sequence is strictly newer.
    pub fn apply(&self, key: impl Into<String>, value: Rc<T>, seq: i64) {
        let key = key.into();
        {
            let mut state = self.state.borrow_mut();
            if state.rows.get(&key).is_some_and(|row| seq <= row.seq) {
                return;
            }
            state.rows.insert(key.clone(), ProjectionRow { value, seq });
        }
        self.changed(&key);
    }

    /// Seeds one consistent history-tail baseline under the same sequence rule.
    pub fn seed(&self, baseline: &ProjectionsBaseline<T>) {
        for (key, value) in &baseline.values {
            self.apply(key.clone(), value.clone(), baseline.as_of_seq);
        }
        let removed = {
            let mut state = self.state.borrow_mut();
            let removed = state
                .rows
                .iter()
                .filter(|(key, row)| {
                    !baseline.values.contains_key(*key) && row.seq <= baseline.as_of_seq
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in &removed {
                state.rows.shift_remove(key);
            }
            removed
        };
        for key in removed {
            self.changed(&key);
        }
    }

    /// Drops values claiming state beyond a replacement generation's durable cut.
    pub fn truncate(&self, last_seq: i64) {
        let removed = {
            let mut state = self.state.borrow_mut();
            let removed = state
                .rows
                .iter()
                .filter(|(_, row)| row.seq > last_seq)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in &removed {
                state.rows.shift_remove(key);
            }
            removed
        };
        for key in removed {
            self.changed(&key);
        }
    }

    fn changed(&self, key: &str) {
        self.state.borrow_mut().values_cache = None;
        let face = self.faces.borrow().get(key).cloned();
        if let Some(face) = face {
            face.notifier.mark_dirty();
        }
        self.any_notifier.mark_dirty();
    }
}
