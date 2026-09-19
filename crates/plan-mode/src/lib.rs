//! Plan mode: logged per-agent collaboration state.

pub mod client;
pub mod index;
pub mod invariant;
pub mod types;

pub use index::{
    APPROVE_LABEL, EXIT_DESCRIPTION, EXIT_PLAN_MODE, INJECT, KEEP_PLANNING_LABEL, NAME, PLAN_MODE,
    PlanGetResult, PlanModeConfig, PlanModeController, PlanSetOutcome, REVIEW_ID, first_heading,
    fold_plan_mode, has_open_turn, plan_mode_at_last_header, plugin, resolve_config,
};
pub use types::PlanProjection;
