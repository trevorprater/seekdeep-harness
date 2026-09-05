//! The goal domain: pure types, host-side vocabulary, and replay fold.

pub mod client;
pub mod domain;
pub mod fold;
pub mod index;
pub mod invariant;
pub mod runtime;
pub mod types;

pub use domain::{
    FoldedGoal, GoalChangeKind, GoalChangeMeta, GoalChanged, GoalChangedEvent, GoalClearChangeMeta,
    GoalClearOperation, GoalErrorCode, GoalMessageSource, GoalOperation, GoalSnapshotChangeMeta,
    GoalSourceKind,
};
pub use index::{
    Config, DEFAULT_MAX_GOAL_ROUNDS, GOAL, GoalEnvironment, GoalService, INJECT, NAME,
    ResolvedConfig, apply_goal_projection, plugin,
};
pub use types::{
    CreateGoalRequest, CreateGoalResult, EditGoalRequest, GoalActivation, GoalBlockReason, GoalId,
    GoalPhase, GoalProjection, GoalRef, GoalSnapshot, GoalView,
};
