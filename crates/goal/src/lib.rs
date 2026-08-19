//! The goal domain: pure types and host-side vocabulary.

pub mod domain;
pub mod types;

pub use domain::{
    FoldedGoal, GoalChangeKind, GoalChanged, GoalClearChangeMeta, GoalClearOperation,
    GoalErrorCode, GoalMessageSource, GoalOperation, GoalSnapshotChangeMeta, GoalSourceKind,
};
pub use types::{
    CreateGoalRequest, CreateGoalResult, EditGoalRequest, GoalActivation, GoalBlockReason, GoalId,
    GoalPhase, GoalProjection, GoalRef, GoalSnapshot, GoalView,
};
