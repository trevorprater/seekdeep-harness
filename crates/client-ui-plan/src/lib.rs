//! Plan mode control Rust/WASM semantics.

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-plan";
/// Dictionary namespace.
pub const PLAN_NS: &str = "plan";
/// Key, Simplified Chinese, and English values in source order.
pub const PLAN_LOCALES: [(&str, &str, &str); 4] = [
    (
        "chip.on.aria",
        "plan mode 已开启，按下关闭",
        "Plan mode on, press to turn off",
    ),
    (
        "chip.on.title",
        "plan mode 已开启 — 点击关闭（/plan off）",
        "Plan mode on — click to turn off (/plan off)",
    ),
    (
        "chip.off.aria",
        "plan mode 已关闭，按下开启",
        "Plan mode off, press to turn on",
    ),
    (
        "chip.off.title",
        "plan mode 已关闭 — 点击开启（/plan）",
        "Plan mode off — click to turn on (/plan)",
    ),
];

/// Host-computed Plan projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanProjection {
    /// Durable current mode.
    pub active: bool,
    /// Pending transition bit.
    pub pending: bool,
}

/// Effective target mode while transitions are pending.
#[must_use]
pub const fn effective_plan_target(plan: PlanProjection) -> bool {
    if plan.pending {
        !plan.active
    } else {
        plan.active
    }
}

/// Whether the Plan status chip should exist.
#[must_use]
pub const fn plan_chip_visible(plan: Option<PlanProjection>) -> bool {
    match plan {
        Some(plan) => effective_plan_target(plan),
        None => false,
    }
}

/// Local Plan exit attempt state layered under the authoritative projection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanExitState {
    /// Exit transaction in flight.
    pub leaving: bool,
    /// Admission/transport failure retained while projection still targets Plan.
    pub error: Option<String>,
}

impl PlanExitState {
    /// Begins one admitted exit attempt.
    pub fn begin(&mut self) {
        self.leaving = true;
        self.error = None;
    }

    /// Settles one attempt, retaining failure text and rearming the control.
    pub fn settle(&mut self, failure: Option<String>) {
        self.leaving = false;
        self.error = failure;
    }

    /// Whether owner lock or an in-flight transaction disables the button.
    #[must_use]
    pub const fn disabled(&self, locked: bool) -> bool {
        locked || self.leaving
    }
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
