//! Revision-guarded mirror used by the Language settings row.

use std::{cell::RefCell, rc::Rc};

/// One selectable language row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageOptionRow {
    /// Locale id.
    pub id: String,
    /// Self-described label.
    pub label: String,
}

/// Immutable Language-row mirror state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageRowState {
    /// Active locale id.
    pub active: String,
    /// Selectable locales in display order.
    pub options: Rc<Vec<LanguageOptionRow>>,
    /// Service revision; `-1` before first synchronization.
    pub revision: i64,
}

/// Tiny portable mirror behind the browser Store facade.
#[derive(Clone)]
pub struct LanguageRowStore {
    snapshot: Rc<RefCell<Rc<LanguageRowState>>>,
}

impl Default for LanguageRowStore {
    fn default() -> Self {
        Self {
            snapshot: Rc::new(RefCell::new(Rc::new(LanguageRowState {
                active: String::new(),
                options: Rc::new(Vec::new()),
                revision: -1,
            }))),
        }
    }
}

impl LanguageRowStore {
    /// Current immutable mirror snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Rc<LanguageRowState> {
        self.snapshot.borrow().clone()
    }

    /// Mirrors a strictly newer locale snapshot.
    pub fn sync(&self, active: String, options: Vec<LanguageOptionRow>, revision: i64) {
        if revision <= self.snapshot().revision {
            return;
        }
        *self.snapshot.borrow_mut() = Rc::new(LanguageRowState {
            active,
            options: Rc::new(options),
            revision,
        });
    }
}
