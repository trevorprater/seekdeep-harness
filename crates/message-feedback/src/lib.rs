//! Durable lifecycle-bound message feedback domain and its storage declaration.

use indexmap::IndexMap;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::MessageId;
use seekdeep_storage_domain::{DomainSpec, ValueSchema, define_domain, domain_table};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Closed rating vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFeedbackRating {
    /// The response was helpful.
    Positive,
    /// The response was not helpful.
    Negative,
}

/// Opaque item version token.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageFeedbackVersion(pub String);

impl std::fmt::Display for MessageFeedbackVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One current feedback item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFeedbackItem {
    /// Target assistant-message identity.
    pub message_id: MessageId,
    /// Overall judgment.
    pub rating: MessageFeedbackRating,
    /// Optional non-blank explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Equality-only token replaced by every material create or update.
    pub version: MessageFeedbackVersion,
    /// Host-assigned creation time in Unix epoch milliseconds.
    pub created_at: u64,
    /// Host-assigned time of the most recent material update.
    pub updated_at: u64,
}

/// Persisted Session fields that fence a sidecar row to one log lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFeedbackSessionIdentity {
    /// Session creation timestamp distinguishing one lifecycle.
    pub created_at: u64,
    /// Working directory, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// One whole-Session sidecar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageFeedbackRow {
    /// Log lifecycle the items belong to.
    pub session: MessageFeedbackSessionIdentity,
    /// Feedback items in first-creation order.
    pub items: Vec<MessageFeedbackItem>,
}

/// Validates one durable sidecar row: duplicate message ids or versions are
/// ambiguous and must be rejected before a write.
///
/// # Errors
///
/// Returns the first duplicate-message or duplicate-version diagnostic.
pub fn validate_message_feedback_row(row: &MessageFeedbackRow) -> anyhow::Result<()> {
    let mut message_ids = std::collections::HashSet::new();
    let mut versions = std::collections::HashSet::new();
    for item in &row.items {
        anyhow::ensure!(
            message_ids.insert(item.message_id.clone()),
            "duplicate message feedback id '{}'",
            item.message_id
        );
        anyhow::ensure!(
            versions.insert(item.version.clone()),
            "duplicate message feedback version '{}'",
            item.version
        );
        anyhow::ensure!(
            item.updated_at >= item.created_at,
            "message feedback updatedAt must not precede createdAt"
        );
    }
    Ok(())
}

/// The message-feedback domain declaration.
///
/// # Panics
///
/// Panics only on an invalid hard-coded domain declaration.
#[must_use]
pub fn message_feedback_domain_spec() -> DomainSpec {
    let spec = DomainSpec {
        name: "message_feedback".to_owned(),
        version: 0,
        global: None,
        tables: IndexMap::from([(
            "sessions".to_owned(),
            domain_table(ValueSchema::serde::<MessageFeedbackRow>()),
        )]),
    };
    define_domain(spec).expect("valid domain spec")
}

/// Projects a session header onto the identity fields a sidecar is bound to.
#[must_use]
pub fn identity_of(
    header: &seekdeep_core::session::SessionHeader,
) -> MessageFeedbackSessionIdentity {
    MessageFeedbackSessionIdentity {
        created_at: header.created_at,
        cwd: header.cwd.clone(),
    }
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-message-feedback", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    fn item(id: &str, version: &str) -> MessageFeedbackItem {
        MessageFeedbackItem {
            message_id: MessageId::new(id),
            rating: MessageFeedbackRating::Positive,
            note: None,
            version: MessageFeedbackVersion(version.to_owned()),
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn row_validation_rejects_duplicate_message_ids_and_versions() {
        let ok = MessageFeedbackRow {
            session: MessageFeedbackSessionIdentity {
                created_at: 1,
                cwd: None,
            },
            items: vec![item("a", "v1"), item("b", "v2")],
        };
        assert!(validate_message_feedback_row(&ok).is_ok());

        let dup_id = MessageFeedbackRow {
            session: ok.session.clone(),
            items: vec![item("a", "v1"), item("a", "v2")],
        };
        assert!(validate_message_feedback_row(&dup_id).is_err());

        let dup_version = MessageFeedbackRow {
            session: ok.session.clone(),
            items: vec![item("a", "v1"), item("b", "v1")],
        };
        assert!(validate_message_feedback_row(&dup_version).is_err());
    }

    #[test]
    fn domain_spec_declares_one_sessions_table_at_version_zero() {
        let spec = message_feedback_domain_spec();
        assert_eq!(spec.name, "message_feedback");
        assert_eq!(spec.version, 0);
        assert!(spec.tables.contains_key("sessions"));
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("register");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose");
        register_invariant(&registry).expect("replacement");
    }
}
