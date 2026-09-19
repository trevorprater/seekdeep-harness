//! Session and Workspace fixture defaults shared by Client tests.

use std::{any::Any, collections::BTreeMap, rc::Rc};

use seekdeep_client_runtime::{
    ComposerPhase, RuntimeSessionSummary, SessionOpenState, SessionSnapshot,
};
use seekdeep_identity::SessionId;

/// Fixture methods or data grafted onto a Session behavior face.
pub type SessionBehaviorOverrides = BTreeMap<String, Rc<dyn Any>>;

/// Strongly typed mutation applied over [`conversation_snapshot`].
pub type SessionSnapshotOverride = Rc<dyn Fn(&mut SessionSnapshot)>;

/// Strongly typed mutation applied over the list-row defaults derived from a fixture id.
pub type SessionSummaryOverride = Rc<dyn Fn(&mut RuntimeSessionSummary)>;

/// Declarative Session identity plus optional snapshot, summary, and behavior overrides.
pub struct SessionFixture {
    /// Stable Session identity before branding.
    pub id: String,
    /// Optional conversation-snapshot mutation.
    pub snapshot: Option<SessionSnapshotOverride>,
    /// Optional list-summary mutation.
    pub summary: Option<SessionSummaryOverride>,
    /// Extra or replacement behavior members.
    pub behavior: SessionBehaviorOverrides,
}

impl std::fmt::Debug for SessionFixture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionFixture")
            .field("id", &self.id)
            .field("snapshot", &self.snapshot.as_ref().map(|_| "override"))
            .field("summary", &self.summary.as_ref().map(|_| "override"))
            .field("behavior_keys", &self.behavior.keys())
            .finish()
    }
}

impl SessionFixture {
    /// Constructs one fixture with no behavior or state overrides.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            snapshot: None,
            summary: None,
            behavior: BTreeMap::new(),
        }
    }
}

/// Complete quiescent target Session snapshot with an open history window.
#[must_use]
pub fn conversation_snapshot(session_id: SessionId) -> SessionSnapshot {
    SessionSnapshot {
        session_id,
        chat: None,
        pending: Rc::new(Vec::new()),
        queue: Rc::new(Vec::new()),
        running: false,
        subagent: None,
        composer_phase: ComposerPhase::Active,
        removed: false,
        open_state: SessionOpenState::Open,
        open_error: None,
        has_more: false,
        loading_older: false,
        prompt_error: None,
        blank: false,
        last_agent_error: None,
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use js_sys::{Array, Map, Object, Reflect};
    use seekdeep_client_runtime::{empty_chat_snapshot_js, empty_conversation_views_js};
    use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

    /// Complete quiescent source-shaped browser conversation snapshot.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object or empty-snapshot construction failures.
    #[wasm_bindgen(js_name = conversationSnapshot)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn conversation_snapshot_js(session_id: String) -> Result<JsValue, JsValue> {
        object(&[
            ("sessionId", JsValue::from_str(&session_id)),
            ("views", empty_conversation_views_js()?),
            ("chat", empty_chat_snapshot_js()?),
            ("nodes", Array::new().into()),
            ("turnTimings", Map::new().into()),
            ("turnEnds", Map::new().into()),
            ("partial", JsValue::NULL),
            ("runningCalls", Array::new().into()),
            ("pending", Array::new().into()),
            ("queue", Array::new().into()),
            ("running", JsValue::FALSE),
            ("subagent", JsValue::NULL),
            ("composerPhase", JsValue::from_str("active")),
            ("removed", JsValue::FALSE),
            ("openState", JsValue::from_str("open")),
            ("openError", JsValue::NULL),
            ("hasMore", JsValue::FALSE),
            ("loadingOlder", JsValue::FALSE),
            ("promptError", JsValue::NULL),
            ("blank", JsValue::FALSE),
            ("lastAgentError", JsValue::NULL),
        ])
        .map(Into::into)
    }

    /// Ready empty source-shaped browser Workspace list state.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = workspaceListState)]
    pub fn workspace_list_state_js() -> Result<JsValue, JsValue> {
        object(&[
            ("items", Array::new().into()),
            ("archivedSessionIds", Array::new().into()),
            ("state", JsValue::from_str("idle")),
            ("phase", JsValue::from_str("ready")),
            ("error", JsValue::NULL),
            ("baselinesReady", JsValue::TRUE),
            ("recentWorkspaceId", JsValue::UNDEFINED),
        ])
        .map(Into::into)
    }

    fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
        let value = Object::new();
        for (key, entry) in entries {
            Reflect::set(&value, &JsValue::from_str(key), entry)?;
        }
        Ok(value)
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::*;
