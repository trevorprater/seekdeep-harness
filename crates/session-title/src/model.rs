//! Durable and provider-facing session-title model types.

use serde::{Deserialize, Serialize};

seekdeep_util::string_brand!(
    /// Stable id of a registered session-title provider.
    pub struct SessionTitleProviderId;
);

/// Exact auxiliary model route that produced a title.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTitleModelProvenance {
    /// Registered LLM provider route.
    pub provider: String,
    /// Provider model id.
    pub model: String,
}

/// Durable ownership record for an accepted session title.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum SessionTitleSource {
    /// Built-in fallback.
    Fallback,
    /// A registered provider.
    Provider {
        /// Provider identity.
        provider: SessionTitleProviderId,
        /// Auxiliary LLM route, when generation used a model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<SessionTitleModelProvenance>,
    },
    /// Explicit user rename.
    User,
}

/// Payload of the log-only session/title event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTitleEventData {
    /// Normalized non-empty title text.
    pub title: String,
    /// Exact human message seqs used; empty for an explicit user rename.
    pub message_seqs: Vec<u64>,
    /// Which source supplied the title.
    pub source: SessionTitleSource,
}

/// Latest folded title plus the title event's durable envelope facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTitleSnapshot {
    /// The title payload.
    #[serde(flatten)]
    pub event: SessionTitleEventData,
    /// Seq of the latest session/title event.
    pub event_seq: u64,
    /// Timestamp of the latest session/title event.
    pub updated_at: u64,
}

/// Required deterministic fallback and accepted-title limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Maximum whitespace-delimited words in the built-in fallback.
    pub fallback_max_words: u64,
    /// Maximum UTF-8 bytes in the built-in fallback.
    pub fallback_max_bytes: u64,
    /// Maximum UTF-8 bytes in any accepted title.
    pub max_title_bytes: u64,
}

/// One eligible human text message exposed to title providers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTitleUserMessage {
    /// Source user/message event seq.
    pub seq: u64,
    /// Exact concatenated text-block content.
    pub text: String,
}

/// Automatic generation cadence owned by a registered provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionTitleAutomaticMode {
    /// First prompt only.
    FirstPrompt,
    /// Every prompt.
    AllPrompts,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_with_source_and_envelope() {
        let snapshot = SessionTitleSnapshot {
            event: SessionTitleEventData {
                title: "Hand picked".to_owned(),
                message_seqs: vec![],
                source: SessionTitleSource::User,
            },
            event_seq: 4,
            updated_at: 1_700_000_000_000_u64,
        };
        let value = serde_json::to_value(&snapshot).expect("serialize");
        assert_eq!(value["title"], "Hand picked");
        assert_eq!(value["source"]["kind"], "user");
        assert_eq!(value["eventSeq"], 4);
        assert_eq!(value["updatedAt"], 1_700_000_000_000_u64);
    }

    #[test]
    fn provider_source_round_trips_with_optional_model() {
        let source = SessionTitleSource::Provider {
            provider: SessionTitleProviderId::new("p1"),
            model: Some(SessionTitleModelProvenance {
                provider: "main-route".to_owned(),
                model: "chat-model".to_owned(),
            }),
        };
        let value = serde_json::to_value(&source).expect("serialize");
        assert_eq!(value["kind"], "provider");
        assert_eq!(value["provider"], "p1");
        assert_eq!(value["model"]["model"], "chat-model");
    }
}
