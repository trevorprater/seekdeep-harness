//! Vocabulary for the fs-observation-policy plugin.

/// Opaque observed-state owner identity (the narrowed agent session object).
///
/// The source's structural `FsObservationActor` interface narrows the opaque
/// `fs/*` event actor to `agent.session`; this Rust port pre-narrows that
/// actor to an opaque pointer identity in `observed_owner`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObservedOwner(usize);

impl ObservedOwner {
    /// Wraps an opaque object-identity value.
    #[must_use]
    pub fn new(identity: usize) -> Self {
        Self(identity)
    }
}
