//! React-free installation contracts between the Slot host and a renderer.

use std::rc::Rc;

use crate::{SlotCore, SlotEntry, SlotName, SlotSpec};

/// Thrown when a retained child-render authorization outlives its declaring entry.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("slot render authorization is stale")]
pub struct StaleAuthorizationError;

/// Thrown when an entry renders a child outside its declaration table.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("slot is outside the declaring entry's children authorization")]
pub struct SlotOwnershipError;

/// Host operations consumed by an installed renderer.
pub trait SlotRendererHost<P, I, X> {
    /// Current runtime spec, or `None` while undeclared.
    fn spec_of(&self, key: &SlotName) -> Option<SlotSpec<I>>;
    /// Raw registration ledger snapshot.
    fn entries_of(&self, key: &SlotName) -> Rc<Vec<Rc<SlotEntry<P, I>>>>;
    /// Active shadowing winners, or every chain entry.
    fn entries_of_slot(&self, key: &SlotName) -> Vec<Rc<SlotEntry<P, I>>>;
    /// Whether a retained entry remains registered.
    fn is_live(&self, entry: &SlotEntry<P, I>) -> bool;
    /// Reports one contained entry failure.
    fn report_entry_error(&self, key: &SlotName, entry: &SlotEntry<P, I>, error: X, abdicate: bool);
}

/// Renderer installed by the browser shell.
pub trait SlotRenderer<P, I, X, Owner, Node> {
    /// Renders the a-priori root Slot over the Host registry.
    fn render_root(&self, host: &dyn SlotRendererHost<P, I, X>, owner: Owner) -> Node;
}

impl<P, I, X> SlotRendererHost<P, I, X> for Rc<SlotCore<P, I, X>>
where
    I: Clone + 'static,
    P: 'static,
    X: 'static,
{
    fn spec_of(&self, key: &SlotName) -> Option<SlotSpec<I>> {
        SlotCore::spec(self, key)
    }

    fn entries_of(&self, key: &SlotName) -> Rc<Vec<Rc<SlotEntry<P, I>>>> {
        SlotCore::entries(self, key)
    }

    fn entries_of_slot(&self, key: &SlotName) -> Vec<Rc<SlotEntry<P, I>>> {
        SlotCore::entries_of_slot(self, key)
    }

    fn is_live(&self, entry: &SlotEntry<P, I>) -> bool {
        SlotCore::is_live(self, entry)
    }

    fn report_entry_error(
        &self,
        key: &SlotName,
        entry: &SlotEntry<P, I>,
        error: X,
        abdicate: bool,
    ) {
        let Some(entry) = self.entry_by_id(entry.id()) else {
            return;
        };
        SlotCore::report_entry_error(self, key, &entry, &error, abdicate);
    }
}
