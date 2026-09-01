//! JSON-compatible browser snapshots converted into the portable tree model.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    ClientWorkspaceView, RuntimeSessionListState, RuntimeSessionSummary, SessionListPhase,
};
use seekdeep_identity::SessionId;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use wasm_bindgen::JsValue;

use crate::SessionSearchResultItem;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserSessionSummary {
    id: SessionId,
    #[serde(default)]
    title: Option<String>,
    display_title: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    agent_preset: Option<String>,
    #[serde(default)]
    parent_id: Option<SessionId>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    pending_interaction: Option<Value>,
    #[serde(default)]
    completed: bool,
    #[serde(default)]
    blank: bool,
    updated_at: i64,
    #[serde(default)]
    projection_values: Option<Value>,
}

impl From<BrowserSessionSummary> for RuntimeSessionSummary {
    fn from(summary: BrowserSessionSummary) -> Self {
        Self {
            id: summary.id,
            title: summary.title,
            display_title: summary.display_title,
            cwd: summary.cwd,
            agent_preset: summary.agent_preset,
            parent_id: summary.parent_id,
            origin: summary.origin,
            running: summary.running,
            pending_interaction: summary.pending_interaction,
            completed: summary.completed,
            blank: summary.blank,
            updated_at: summary.updated_at,
            projection_values: summary.projection_values,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserSessionListState {
    ids: Vec<SessionId>,
    by_id: IndexMap<SessionId, BrowserSessionSummary>,
    #[serde(default)]
    current: Option<SessionId>,
    phase: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserSearchItem {
    session_id: SessionId,
    snippet: String,
}

pub(crate) fn parse_json<T: DeserializeOwned>(value: &JsValue, owner: &str) -> Result<T, JsValue> {
    let source = js_sys::JSON::stringify(value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} must be JSON-compatible")))?;
    serde_json::from_str(&source)
        .map_err(|error| js_sys::TypeError::new(&format!("invalid {owner}: {error}")).into())
}

pub(crate) fn session_list(value: &JsValue) -> Result<RuntimeSessionListState, JsValue> {
    let source: BrowserSessionListState = parse_json(value, "Session list snapshot")?;
    let phase = match source.phase.as_str() {
        "pending" => SessionListPhase::Pending,
        "ready" => SessionListPhase::Ready,
        phase => {
            return Err(
                js_sys::TypeError::new(&format!("unknown Session list phase {phase:?}")).into(),
            );
        }
    };
    Ok(RuntimeSessionListState {
        ids: Rc::new(source.ids),
        by_id: Rc::new(
            source
                .by_id
                .into_iter()
                .map(|(id, summary)| (id, Rc::new(summary.into())))
                .collect(),
        ),
        current: source.current,
        phase,
        subagents_by_parent: Rc::new(IndexMap::new()),
        jobs_by_session: Rc::new(IndexMap::new()),
        current_address: None,
    })
}

pub(crate) fn workspaces(value: &JsValue) -> Result<Vec<Rc<ClientWorkspaceView>>, JsValue> {
    parse_json::<Vec<ClientWorkspaceView>>(value, "Workspace list")
        .map(|workspaces| workspaces.into_iter().map(Rc::new).collect::<Vec<_>>())
}

pub(crate) fn session_ids(value: &JsValue, owner: &str) -> Result<Vec<SessionId>, JsValue> {
    parse_json(value, owner)
}

pub(crate) fn string_lists(
    value: &JsValue,
    owner: &str,
) -> Result<IndexMap<String, Vec<String>>, JsValue> {
    parse_json(value, owner)
}

pub(crate) fn timestamp_accounts(
    value: &JsValue,
    owner: &str,
) -> Result<IndexMap<String, IndexMap<String, i64>>, JsValue> {
    parse_json(value, owner)
}

pub(crate) fn search_items(value: &JsValue) -> Result<Vec<SessionSearchResultItem>, JsValue> {
    parse_json::<Vec<BrowserSearchItem>>(value, "Session search results").map(|items| {
        items
            .into_iter()
            .map(|item| SessionSearchResultItem {
                session_id: item.session_id,
                snippet: item.snippet,
            })
            .collect()
    })
}

pub(crate) fn to_js<T: Serialize>(value: &T, owner: &str) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value)
        .map_err(|error| js_sys::Error::new(&format!("failed to encode {owner}: {error}")).into())
}
