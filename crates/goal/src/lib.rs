//! The goal domain: pure types, host-side vocabulary, and replay fold.

pub mod client;
pub mod domain;
pub mod fold;
pub mod index;
pub mod invariant;
pub mod runtime;
pub mod types;

pub use domain::{
    FoldedGoal, GoalChangeKind, GoalChangeMeta, GoalChanged, GoalClearChangeMeta,
    GoalClearOperation, GoalErrorCode, GoalMessageSource, GoalOperation, GoalSnapshotChangeMeta,
    GoalSourceKind,
};
pub use types::{
    CreateGoalRequest, CreateGoalResult, EditGoalRequest, GoalActivation, GoalBlockReason, GoalId,
    GoalPhase, GoalProjection, GoalRef, GoalSnapshot, GoalView,
};
