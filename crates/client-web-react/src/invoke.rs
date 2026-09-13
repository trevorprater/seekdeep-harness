//! Target-portable pending-count transitions for `useInvoke`.

/// In-flight action counter whose observers wake only on boolean pending transitions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InvokeCounter {
    inflight: u64,
}

impl InvokeCounter {
    /// Records one new action and returns whether pending changed.
    pub fn begin(&mut self) -> bool {
        let changed = self.inflight == 0;
        self.inflight = self.inflight.saturating_add(1);
        changed
    }

    /// Records one settled action and returns whether pending changed.
    pub fn finish(&mut self) -> bool {
        if self.inflight == 0 {
            return false;
        }
        self.inflight -= 1;
        self.inflight == 0
    }

    /// Whether at least one action remains unsettled.
    #[must_use]
    pub fn pending(self) -> bool {
        self.inflight > 0
    }

    /// Exact concurrent count.
    #[must_use]
    pub fn inflight(self) -> u64 {
        self.inflight
    }
}
