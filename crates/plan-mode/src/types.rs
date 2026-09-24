//! Pure types of the plan domain.

use serde::{Deserialize, Serialize};

/// The plan projection's wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanProjection {
    /// The logged state in force.
    pub active: bool,
    /// True while a logged plan selection targets another state.
    pub pending: bool,
}
