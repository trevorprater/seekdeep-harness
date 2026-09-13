//! Session-maybe adoption and remount bookkeeping.

/// Session identity crossing the renderer boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenderSessionId(String);

impl RenderSessionId {
    /// Creates an identity with its exact runtime spelling.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed runtime spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One session-maybe component incarnation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaybeIncarnation {
    adopted: Option<RenderSessionId>,
    epoch: u64,
}

impl MaybeIncarnation {
    /// Current adopted session, absent for a blank-born incarnation.
    #[must_use]
    pub fn adopted(&self) -> Option<&RenderSessionId> {
        self.adopted.as_ref()
    }

    /// React child key for the current incarnation.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Applies current selection and returns whether the keyed child must remount.
    pub fn transition(&mut self, current: Option<RenderSessionId>) -> bool {
        match (&self.adopted, current) {
            (None, Some(current)) => {
                self.adopted = Some(current);
                false
            }
            (Some(adopted), Some(current)) if adopted != &current => {
                self.adopted = Some(current);
                self.epoch = self.epoch.wrapping_add(1);
                true
            }
            (Some(_), None) => {
                self.adopted = None;
                self.epoch = self.epoch.wrapping_add(1);
                true
            }
            _ => false,
        }
    }
}
