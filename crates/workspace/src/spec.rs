//! Durable Workspace domain schemas.

use std::fmt;

use indexmap::IndexMap;
use seekdeep_core::session::SessionId;
use seekdeep_storage_domain::{
    DomainGlobalSpec, DomainSpec, ValueSchema, define_domain, domain_table,
};
use serde::{Deserialize, Serialize};

use crate::WorkspaceId;

/// One durable workspace record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecord {
    /// Canonical immutable directory.
    pub path: String,
    /// Mutable display title.
    pub title: String,
    /// Ordered durable session candidates.
    pub session_ids: Vec<SessionId>,
    /// ISO creation instant.
    pub created_at: String,
    /// ISO last-mutation instant.
    pub updated_at: String,
}

/// Recoverable registry mutation direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PendingOperation {
    /// A record may have landed without entering the order.
    Create,
    /// A record may remain after leaving the order.
    Delete,
}

impl fmt::Display for PendingOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Create => "create",
            Self::Delete => "delete",
        })
    }
}

/// Persisted marker surrounding a record/order two-write mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMutation {
    /// Recovery direction.
    pub operation: PendingOperation,
    /// Provisional or deleting workspace.
    pub workspace_id: WorkspaceId,
}

/// Durable global registry state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDomainState {
    /// Whether one-time history bootstrap completed.
    pub initialized: bool,
    /// Authoritative display order.
    pub workspace_ids: Vec<WorkspaceId>,
    /// Global archive order.
    #[serde(default)]
    pub archived_session_ids: Vec<SessionId>,
    /// Interrupted mutation marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_mutation: Option<PendingMutation>,
}

/// Builds the exact version-2 Workspace domain declaration.
///
/// # Errors
///
/// Returns a declaration validation error if the shared domain contract changes incompatibly.
pub fn workspace_domain_spec() -> anyhow::Result<DomainSpec> {
    define_domain(DomainSpec {
        name: "workspace".to_owned(),
        version: 2,
        global: Some(DomainGlobalSpec {
            schema: ValueSchema::serde::<WorkspaceDomainState>(),
            initial: serde_json::to_value(WorkspaceDomainState {
                initialized: false,
                workspace_ids: Vec::new(),
                archived_session_ids: Vec::new(),
                pending_mutation: None,
            })?,
        }),
        tables: IndexMap::from([(
            "workspaces".to_owned(),
            domain_table(ValueSchema::serde::<WorkspaceRecord>()),
        )]),
    })
}
