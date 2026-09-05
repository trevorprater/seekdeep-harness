//! Deterministic in-process browser fixture transport and mutable fake Host.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt, future::BoxFuture, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_identity::RpcId;
use seekdeep_llm::AbortSignal;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::{
    CLIENT_CONNECTION, ClientConnection, ClientConnectionFuture, ClientConnectionHandle,
    EventFrame, HostDescription, RpcError, RpcResult, ServerResponse, StreamApi,
};

const FIXTURE_SEED: &str = include_str!("../data/fixture-seed.json");
const SEARCH_LIMIT: usize = 20;

/// Successful create-frame ordering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FixtureCreateFrameOrder {
    /// Publish the Session before its Workspace account.
    #[default]
    SessionFirst,
    /// Publish the Workspace account before the Session.
    WorkspaceFirst,
}

/// Deterministic fixture branches used by keyless Web assembly tests.
#[allow(clippy::struct_excessive_bools)] // Mirrors four independent browser query switches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixtureOptions {
    /// Start without resident Workspaces or Sessions.
    pub empty: bool,
    /// Reject every prompt before durable admission.
    pub reject_prompt: bool,
    /// Publish the Session but reject Workspace attachment.
    pub fail_workspace_attach: bool,
    /// Publish and frame the Session, then return a transport failure.
    pub drop_session_create_response: bool,
    /// Order of the two successful create frames.
    pub create_frame_order: FixtureCreateFrameOrder,
}

impl FixtureOptions {
    /// Parses the browser fixture query switches.
    #[must_use]
    pub fn from_query(query: &str) -> Self {
        let values = url::form_urlencoded::parse(query.trim_start_matches('?').as_bytes())
            .into_owned()
            .collect::<HashMap<_, _>>();
        Self {
            empty: values.get("fixture").is_some_and(|value| value == "empty"),
            reject_prompt: values
                .get("fixturePrompt")
                .is_some_and(|value| value == "reject"),
            fail_workspace_attach: values
                .get("fixtureAttach")
                .is_some_and(|value| value == "fail"),
            drop_session_create_response: values
                .get("fixtureSessionCreate")
                .is_some_and(|value| value == "drop-response"),
            create_frame_order: if values
                .get("fixtureFrames")
                .is_some_and(|value| value == "workspace-first")
            {
                FixtureCreateFrameOrder::WorkspaceFirst
            } else {
                FixtureCreateFrameOrder::SessionFirst
            },
        }
    }
}

/// Snapshot of the opt-in reasoning-storm producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureTimingState {
    /// Addressed Session.
    pub session_id: String,
    /// Requested chunk count.
    pub chunk_count: u64,
    /// Chunks emitted per interval.
    pub chunks_per_interval: u64,
    /// Interval duration.
    pub interval_ms: u64,
    /// Chunks emitted so far.
    pub emitted: u64,
    /// Completion marker appended to the final chunk.
    pub marker: String,
    /// Whether emission is still active.
    pub emitting: bool,
}

type EnvelopeListener = Arc<dyn Fn(Vec<Value>) + Send + Sync>;

/// Idempotent full-envelope observation cleanup.
pub struct FixtureEnvelopeSubscription {
    fixture: Weak<FixtureApi>,
    id: u64,
}

impl FixtureEnvelopeSubscription {
    /// Stops future envelope batches.
    pub fn dispose(&self) {
        if let Some(fixture) = self.fixture.upgrade() {
            fixture.envelope_listeners.lock().remove(&self.id);
        }
    }
}

struct Replay {
    signal: AbortSignal,
}

struct FixtureState {
    sessions: Vec<Value>,
    logs: HashMap<String, Vec<Value>>,
    history_projections: Value,
    model_groups: Value,
    model_selections: HashMap<String, Value>,
    host_description: Value,
    workspaces: Vec<Value>,
    archived_session_ids: Vec<String>,
    directories: HashMap<String, Value>,
    settings_description: Value,
    providers: Value,
    presets: BTreeMap<String, (String, String)>,
    default_preset: String,
    skills: Value,
    credentials: BTreeSet<String>,
    attachments: HashMap<String, Value>,
    pending_approval: Option<(RpcId, Value)>,
    pending_question: Option<(RpcId, Value)>,
    next_session: u64,
    next_workspace: u64,
    next_turn: HashMap<String, u64>,
    next_goal: u64,
    next_attachment: u64,
    replays: HashMap<String, Replay>,
    history_delay_ms: u64,
    fail_next_history: bool,
    timing_state: Option<FixtureTimingState>,
}

/// Complete in-process fixture carrier over one mutable state graph.
pub struct FixtureApi {
    weak_self: Weak<Self>,
    options: FixtureOptions,
    state: Mutex<FixtureState>,
    next_rpc: AtomicU64,
    next_listener: AtomicU64,
    next_time: AtomicU64,
    mux_senders: Mutex<Vec<mpsc::UnboundedSender<anyhow::Result<EventFrame>>>>,
    host_senders: Mutex<Vec<mpsc::UnboundedSender<anyhow::Result<EventFrame>>>>,
    envelope_listeners: Mutex<HashMap<u64, EnvelopeListener>>,
}

impl std::fmt::Debug for FixtureApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("FixtureApi")
            .field("sessions", &state.sessions.len())
            .field("workspaces", &state.workspaces.len())
            .field("mux_streams", &self.mux_senders.lock().len())
            .field("host_streams", &self.host_senders.lock().len())
            .finish_non_exhaustive()
    }
}

impl FixtureApi {
    /// Builds a deterministic fixture world from the pinned-source seed.
    #[must_use]
    #[allow(clippy::too_many_lines)] // Seed hydration keeps the source fixture inventory visible.
    pub fn new(options: FixtureOptions) -> Arc<Self> {
        let seed = fixture_seed();
        let fixed_now = seed
            .get("fixedNow")
            .and_then(Value::as_u64)
            .unwrap_or(1_787_718_600_000);
        let sessions = if options.empty {
            Vec::new()
        } else {
            seed.pointer("/sessions/items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let mut logs = HashMap::new();
        logs.insert(
            "fx-alpha".to_owned(),
            seed.pointer("/history/events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        let workspaces = if options.empty {
            Vec::new()
        } else {
            seed.pointer("/workspaces/items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let directories = seed
            .get("directories")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                row.get("path")
                    .and_then(Value::as_str)
                    .map(|path| (path.to_owned(), row.clone()))
            })
            .collect();
        let presets = seed
            .get("presetContents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                Some((
                    row.get("agentPreset")?.as_str()?.to_owned(),
                    (
                        row.get("trust")?.as_str()?.to_owned(),
                        row.get("content")?.as_str()?.to_owned(),
                    ),
                ))
            })
            .collect();
        let pending = seed
            .get("muxFrames")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let pending_approval = pending
            .iter()
            .find(|envelope| {
                envelope.pointer("/payload/type").and_then(Value::as_str)
                    == Some("approval/requested")
            })
            .and_then(pending_frame);
        let pending_question = pending
            .iter()
            .find(|envelope| {
                envelope.pointer("/payload/type").and_then(Value::as_str)
                    == Some("question/requested")
            })
            .and_then(pending_frame);
        Arc::new_cyclic(|weak_self| Self {
            weak_self: weak_self.clone(),
            options,
            state: Mutex::new(FixtureState {
                sessions,
                logs,
                history_projections: seed
                    .pointer("/history/projections/values")
                    .cloned()
                    .unwrap_or_else(empty_projection_values),
                model_groups: seed
                    .pointer("/models/groups")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
                model_selections: HashMap::from([(
                    "fx-alpha".to_owned(),
                    seed.pointer("/models/current")
                        .cloned()
                        .unwrap_or_else(default_model_selection),
                )]),
                host_description: seed.get("host").cloned().unwrap_or_else(|| json!({})),
                workspaces,
                archived_session_ids: Vec::new(),
                directories,
                settings_description: seed.get("settings").cloned().unwrap_or_else(|| json!({})),
                providers: seed.get("providers").cloned().unwrap_or_else(|| json!({})),
                presets,
                default_preset: "standard".to_owned(),
                skills: seed
                    .get("skills")
                    .cloned()
                    .unwrap_or_else(|| json!({ "skills": [] })),
                credentials: BTreeSet::from(["DEEPSEEK_API_KEY".to_owned()]),
                attachments: HashMap::from([(
                    "fixture:image".to_owned(),
                    fixture_image_response(),
                )]),
                pending_approval,
                pending_question,
                next_session: 1,
                next_workspace: 1,
                next_turn: HashMap::from([("fx-alpha".to_owned(), 75)]),
                next_goal: 1,
                next_attachment: 1,
                replays: HashMap::new(),
                history_delay_ms: 0,
                fail_next_history: false,
                timing_state: None,
            }),
            next_rpc: AtomicU64::new(1),
            next_listener: AtomicU64::new(1),
            next_time: AtomicU64::new(fixed_now),
            mux_senders: Mutex::new(Vec::new()),
            host_senders: Mutex::new(Vec::new()),
            envelope_listeners: Mutex::new(HashMap::new()),
        })
    }

    /// Builds the public Connection handle over this fixture carrier.
    #[must_use]
    pub fn connection_handle(self: &Arc<Self>, is_loopback: bool) -> Arc<ClientConnectionHandle> {
        ClientConnectionHandle::with_streams(self.clone(), self.clone(), is_loopback)
    }

    /// Provides a complete fixture-backed Client Connection in the calling fiber.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
        is_loopback: bool,
    ) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(CLIENT_CONNECTION, self.connection_handle(is_loopback))?)
    }

    /// Full-form unary carrier call with request/response tap and exact rpcId echo.
    ///
    /// # Errors
    ///
    /// Returns fixture transport failures or response serialization failures.
    pub async fn unary(
        &self,
        method: &str,
        rpc_id: RpcId,
        payload: Value,
        signal: AbortSignal,
    ) -> anyhow::Result<ServerResponse<Value>> {
        self.publish_envelopes(&[json!({
            "type": "client-request",
            "rpcId": rpc_id,
            "method": method,
            "payload": payload,
        })]);
        let result = self.dispatch(method, payload, signal).await?;
        let response = ServerResponse::new(rpc_id, result);
        self.publish_envelopes(&[serde_json::to_value(&response)?]);
        Ok(response)
    }

    /// Generic Remote endpoint call over the same fixture state graph.
    ///
    /// # Errors
    ///
    /// Rejects unknown channels/endpoints or underlying fixture transport failures.
    pub async fn remote_call(
        &self,
        channel: &str,
        endpoint: &str,
        payload: Value,
    ) -> anyhow::Result<RpcResult<Value>> {
        anyhow::ensure!(
            channel == "/api",
            "fixture connection RPC channel {channel:?} is unavailable"
        );
        match endpoint {
            "commands/list" | "commands/execute" | "goals/create" | "goals/edit"
            | "goals/pause" | "goals/resume" | "goals/complete" | "goals/clear" => {
                self.dispatch(endpoint, payload, AbortSignal::default())
                    .await
            }
            _ => anyhow::bail!("fixture connection RPC endpoint {endpoint:?} is unavailable"),
        }
    }

    /// Observes all four complete envelope forms.
    #[must_use]
    pub fn subscribe_envelopes(
        self: &Arc<Self>,
        listener: EnvelopeListener,
    ) -> FixtureEnvelopeSubscription {
        let id = self.next_listener.fetch_add(1, Ordering::Relaxed);
        self.envelope_listeners.lock().insert(id, listener);
        FixtureEnvelopeSubscription {
            fixture: Arc::downgrade(self),
            id,
        }
    }

    /// Answers one resident approval or question and broadcasts its resolution.
    pub fn respond(&self, message: &Value) -> Value {
        self.publish_envelopes(std::slice::from_ref(message));
        let rpc_id = message.get("rpcId").and_then(Value::as_str);
        let mut state = self.state.lock();
        if state
            .pending_approval
            .as_ref()
            .is_some_and(|(id, _)| Some(id.as_str()) == rpc_id)
        {
            let Some((_, requested)) = state.pending_approval.clone() else {
                return json!({"accepted":false,"reason":"not-pending"});
            };
            let result = message.get("result");
            let approval_id = result.and_then(|value| value.pointer("/value/approvalId"));
            let outcome = result.and_then(|value| value.pointer("/value/outcome"));
            if result
                .and_then(|value| value.get("ok"))
                .and_then(Value::as_bool)
                != Some(true)
                || approval_id != requested.get("approvalId")
                || !matches!(
                    outcome.and_then(Value::as_str),
                    Some("allowed-once" | "rejected")
                )
            {
                return json!({"accepted":false,"reason":"bad-response"});
            }
            state.pending_approval = None;
            let frame = json!({
                "type":"approval/resolved",
                "sessionId":"fx-alpha",
                "approvalId":approval_id,
                "outcome":outcome,
            });
            drop(state);
            self.emit_mux(frame);
            return json!({"accepted":true});
        }
        if state
            .pending_question
            .as_ref()
            .is_some_and(|(id, _)| Some(id.as_str()) == rpc_id)
        {
            let Some((question_id, _)) = state.pending_question.take() else {
                return json!({"accepted":false,"reason":"not-pending"});
            };
            let outcome = if message.pointer("/result/ok").and_then(Value::as_bool) == Some(true) {
                "answered"
            } else {
                "cancelled"
            };
            drop(state);
            self.emit_mux(json!({
                "type":"question/resolved",
                "sessionId":"fx-alpha",
                "questionRpcId":question_id,
                "outcome":outcome,
            }));
            return json!({"accepted":true});
        }
        json!({"accepted":false,"reason":"not-pending"})
    }

    /// Delays history delivery by an exact number of milliseconds.
    pub fn set_history_delay(&self, milliseconds: u64) {
        self.state.lock().history_delay_ms = milliseconds;
    }

    /// Makes the next history request fail at transport level after its delay.
    pub fn fail_next_history(&self) {
        self.state.lock().fail_next_history = true;
    }

    /// Appends and broadcasts one ordinary user message.
    pub fn append_user(&self, session_id: &str, message: &str) {
        self.append_event(session_id, user_message_event(message));
    }

    /// Appends one user message without broadcasting it.
    pub fn append_silent(&self, session_id: &str, message: &str) {
        let mut state = self.state.lock();
        let time = self.next_timestamp();
        let log = state.logs.entry(session_id.to_owned()).or_default();
        let seq = log.len();
        log.push(json!({
            "event":{
                "type":"user/message","surfaceOp":"append","seq":seq,"time":time,
                "data":message_value(message),
            }
        }));
    }

    /// Appends a later durable title revision through raw-event and projection frames.
    pub fn append_title(&self, session_id: &str, title: &str) {
        let event = self.append_event(session_id, json!({
            "type":"session/title",
            "data":{"title":title,"messageSeqs":[],"source":{"kind":"provider","provider":"fixture"}}
        }));
        self.emit_mux(json!({
            "type":"session/projection","sessionId":session_id,"key":"title",
            "value":title,"seq":event.pointer("/event/seq").cloned().unwrap_or(json!(0)),
        }));
    }

    /// Ends every currently open stream without aborting its consumer signal.
    pub fn break_streams(&self) {
        self.mux_senders.lock().clear();
        self.host_senders.lock().clear();
    }

    /// Starts one validated, externally paced reasoning chunk storm.
    ///
    /// # Errors
    ///
    /// Rejects zero-valued parameters or a second concurrently active storm.
    pub fn start_reasoning_chunk_storm(
        self: &Arc<Self>,
        session_id: &str,
        chunk_count: u64,
        chunks_per_interval: u64,
        interval_ms: u64,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            chunk_count > 0,
            "fixture: reasoning chunk count must be a positive safe integer"
        );
        anyhow::ensure!(
            chunks_per_interval > 0,
            "fixture: reasoning chunks per interval must be a positive safe integer"
        );
        anyhow::ensure!(
            interval_ms > 0,
            "fixture: reasoning interval must be a positive safe integer"
        );
        let (turn, marker) = {
            let mut state = self.state.lock();
            anyhow::ensure!(
                !state
                    .timing_state
                    .as_ref()
                    .is_some_and(|state| state.emitting),
                "fixture: reasoning chunk storm already running"
            );
            let turn = state.next_turn.entry(session_id.to_owned()).or_default();
            let current = *turn;
            *turn += 1;
            let marker = format!("REASONING_STRESS_COMPLETE:{current}:{chunk_count}");
            state.timing_state = Some(FixtureTimingState {
                session_id: session_id.to_owned(),
                chunk_count,
                chunks_per_interval,
                interval_ms,
                emitted: 0,
                marker: marker.clone(),
                emitting: true,
            });
            (current, marker)
        };
        self.set_running(session_id, true);
        self.append_event(
            session_id,
            json!({"type":"turn/start","data":{"turn":turn}}),
        );
        self.append_event(
            session_id,
            user_message_event(&format!("Reasoning chunk stress: {chunk_count} chunks.")),
        );
        self.append_event(
            session_id,
            json!({"type":"step/start","data":{"turn":turn,"step":0}}),
        );
        self.append_event(session_id, json!({
            "type":"assistant/chunk",
            "data":{"turn":turn,"step":0,"chunk":{"type":"block-start","index":0,"blockType":"reasoning"}}
        }));
        let fixture = self.clone();
        let session = session_id.to_owned();
        let marker_copy = marker.clone();
        tokio::spawn(async move {
            let mut emitted = 0;
            while emitted < chunk_count {
                let end = (emitted + chunks_per_interval).min(chunk_count);
                for index in emitted..end {
                    let text = if index + 1 == chunk_count {
                        format!("\n{marker_copy}")
                    } else if index % 64 == 63 {
                        "推理\n".to_owned()
                    } else {
                        "推理".to_owned()
                    };
                    fixture.append_event(&session, json!({
                        "type":"assistant/chunk",
                        "data":{"turn":turn,"step":0,"chunk":{"type":"reasoning-delta","index":0,"text":text}}
                    }));
                }
                emitted = end;
                if let Some(state) = fixture.state.lock().timing_state.as_mut() {
                    state.emitted = emitted;
                    state.emitting = emitted < chunk_count;
                }
                if emitted < chunk_count {
                    tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                }
            }
        });
        Ok(marker)
    }

    /// Current reasoning-storm state copy.
    #[must_use]
    pub fn reasoning_chunk_storm_state(&self) -> Option<FixtureTimingState> {
        self.state.lock().timing_state.clone()
    }

    fn publish_envelopes(&self, envelopes: &[Value]) {
        let listeners = self
            .envelope_listeners
            .lock()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener(envelopes.to_vec());
        }
    }

    async fn dispatch(
        &self,
        method: &str,
        payload: Value,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResult<Value>> {
        match method {
            "session.list" => Ok(self.session_list()),
            "session.search" => Ok(self.session_search(&payload, &signal)),
            "session.create" => self.session_create(&payload),
            "session.history" => self.session_history(&payload).await,
            "session.models" => Ok(self.session_models(&payload)),
            "session.selectModel" => Ok(self.select_model(&payload)),
            "session.rename" => Ok(self.session_rename(&payload)),
            "session.fork" => Ok(self.session_fork(&payload)),
            "session.prompt" => Ok(self.session_prompt(&payload)),
            "session.attachment" => Ok(self.session_attachment(&payload)),
            "session.updateQueue" => Ok(failure(
                "queue-item-not-found",
                "fixture has no pending queue item",
                json!({}),
            )),
            "session.cancel" => Ok(self.session_cancel(&payload)),
            "subagent.list" => Ok(success(json!({"entries":[],"parentAvailable":true}))),
            "subagent.history" => Ok(success(json!({"events":[],"hasMore":false}))),
            "subagent.prompt" => Ok(success(
                json!({"messageId":format!("fixture-message-{}", string_at(&payload,"childSessionId").unwrap_or_default())}),
            )),
            "subagent.interrupt" => Ok(success(json!({"accepted":true}))),
            "host.describe" => Ok(success(self.state.lock().host_description.clone())),
            "host.pickDirectory" => Ok(success(json!({"path":"/home/fixture/Documents/project"}))),
            "host.listDirectory" => Ok(self.list_directory(&payload)),
            "host.createDirectory" => Ok(self.create_directory(&payload)),
            "host.openPath" | "settings.openDocument" => Ok(success(json!({"opened":true}))),
            "workspace.list" => Ok(self.workspace_list()),
            "workspace.create" => Ok(self.workspace_create(&payload)),
            "workspace.rename" => Ok(self.workspace_rename(&payload)),
            "workspace.delete" => Ok(self.workspace_delete(&payload)),
            "workspace.insertBefore" => Ok(self.workspace_insert_before(&payload)),
            "workspace.insertSessionBefore" => Ok(self.workspace_insert_session_before(&payload)),
            "workspace.archiveSession" => Ok(self.workspace_archive_session(&payload)),
            "skill.list" => Ok(self.skill_list(&payload)),
            "agentPreset.list" => Ok(self.preset_list()),
            "agentPreset.select" => Ok(self.preset_select(&payload)),
            "agentPreset.read" => Ok(self.preset_read(&payload)),
            "agentPreset.copy" => Ok(self.preset_copy(&payload)),
            "agentPreset.openDocument" => Ok(self.preset_open(&payload)),
            "agentPreset.remove" => Ok(self.preset_remove(&payload)),
            "goal.create" | "goals/create" => Ok(self.goal_create(&payload)),
            "goal.edit" | "goals/edit" => Ok(self.goal_mutate(&payload, "edit")),
            "goal.pause" | "goals/pause" => Ok(self.goal_mutate(&payload, "pause")),
            "goal.resume" | "goals/resume" => Ok(self.goal_mutate(&payload, "resume")),
            "goal.complete" | "goals/complete" => Ok(self.goal_mutate(&payload, "complete")),
            "goal.clear" | "goals/clear" => Ok(self.goal_clear(&payload)),
            "commands/list" => Ok(self.command_list(&payload)),
            "commands/execute" => Ok(self.command_execute(&payload)),
            "settings.describe" => Ok(success(self.state.lock().settings_description.clone())),
            "settings.update" | "settings.replace" | "settings.mutate" => Ok(failure(
                "settings-rejected",
                "fixture: no settings namespaces are registered",
                json!({"ns":payload.get("ns").cloned().unwrap_or(Value::Null)}),
            )),
            "credentials.describe" => Ok(self.credentials_describe(&payload)),
            "credentials.set" => Ok(self.credentials_set(&payload, true)),
            "credentials.unset" => Ok(self.credentials_set(&payload, false)),
            "llm.providers" => Ok(success(self.state.lock().providers.clone())),
            "llm.models" => Ok(success(
                json!({"groups":self.state.lock().model_groups,"failures":[]}),
            )),
            "llm.discoverModels" => Ok(self.discover_models()),
            _ => anyhow::bail!("fixture API method {method:?} is unavailable"),
        }
    }

    fn session_list(&self) -> RpcResult<Value> {
        let mut items = self.state.lock().sessions.clone();
        items.sort_by(|left, right| {
            number_at(right, "updatedAt").total_cmp(&number_at(left, "updatedAt"))
        });
        success(json!({"items":items}))
    }

    fn session_search(&self, payload: &Value, signal: &AbortSignal) -> RpcResult<Value> {
        if signal.is_aborted() {
            return failure("cancelled", "fixture session search was aborted", json!({}));
        }
        let query = string_at(payload, "query").unwrap_or_default();
        let phrase = tokenize(&query);
        if phrase.is_empty() {
            return success(json!({"items":[],"hasMore":false}));
        }
        let state = self.state.lock();
        let mut matches = Vec::new();
        for summary in &state.sessions {
            let Some(id) = summary.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            let Some(log) = state.logs.get(id) else {
                continue;
            };
            let mut best: Option<(usize, usize, u64, String)> = None;
            for entry in log {
                let event = entry.get("event").unwrap_or(entry);
                let text = searchable_event_text(event);
                let document = tokenize_with_spans(&text);
                let Some((count, start, end)) = phrase_match(&document, &phrase) else {
                    continue;
                };
                let time = event
                    .get("time")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let candidate = (
                    count,
                    text.chars().count(),
                    time,
                    search_snippet(&text, start, end),
                );
                if best.as_ref().is_none_or(|current| {
                    candidate.0 > current.0
                        || (candidate.0 == current.0 && candidate.1 < current.1)
                        || (candidate.0 == current.0
                            && candidate.1 == current.1
                            && candidate.2 > current.2)
                }) {
                    best = Some(candidate);
                }
            }
            if let Some((count, length, time, snippet)) = best {
                matches.push((id.to_owned(), count, length, time, snippet));
            }
        }
        matches.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then(left.2.cmp(&right.2))
                .then(right.3.cmp(&left.3))
                .then(left.0.cmp(&right.0))
        });
        let has_more = matches.len() > SEARCH_LIMIT;
        let items = matches
            .into_iter()
            .take(SEARCH_LIMIT)
            .map(|(session_id, _, _, _, snippet)| json!({"sessionId":session_id,"snippet":snippet}))
            .collect::<Vec<_>>();
        success(json!({"items":items,"hasMore":has_more}))
    }

    async fn session_history(&self, payload: &Value) -> anyhow::Result<RpcResult<Value>> {
        let id = string_at(payload, "sessionId").unwrap_or_default();
        let before = payload.get("beforeSeq").and_then(Value::as_i64);
        let max_messages = usize::try_from(
            payload
                .get("maxMessages")
                .and_then(Value::as_u64)
                .unwrap_or(50),
        )
        .unwrap_or(usize::MAX);
        let (log, projections, delay, doomed) = {
            let mut state = self.state.lock();
            let log = state.logs.get(&id).cloned().unwrap_or_default();
            let projections = if before.is_none() {
                Some(if log.is_empty() {
                    empty_projection_values()
                } else {
                    state.history_projections.clone()
                })
            } else {
                None
            };
            let delay = state.history_delay_ms;
            let doomed = state.fail_next_history;
            state.fail_next_history = false;
            (log, projections, delay, doomed)
        };
        let page = page_of(&log, before, max_messages);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if doomed {
            anyhow::bail!("fixture: simulated history transport failure");
        }
        let mut value = page.as_object().cloned().unwrap_or_default();
        if let Some(values) = projections {
            value.insert(
                "projections".to_owned(),
                json!({"asOfSeq":sequence_from_len(log.len()),"values":values}),
            );
        }
        Ok(success(Value::Object(value)))
    }

    fn session_models(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "sessionId").unwrap_or_default();
        let state = self.state.lock();
        success(json!({
            "current":state.model_selections.get(&id).cloned().unwrap_or_else(default_model_selection),
            "routable":true,"groups":state.model_groups,"failures":[],
        }))
    }

    fn select_model(&self, payload: &Value) -> RpcResult<Value> {
        let Some(id) = payload.get("sessionId").and_then(Value::as_str) else {
            return bad_request("sessionId");
        };
        let selected = json!({
            "provider":payload.get("provider").cloned().unwrap_or(Value::Null),
            "model":payload.get("model").cloned().unwrap_or(Value::Null),
            "reasoningEffort":payload.get("reasoningEffort").cloned().unwrap_or(Value::Null),
        });
        let selected = omit_null_fields(selected);
        self.state
            .lock()
            .model_selections
            .insert(id.to_owned(), selected.clone());
        success(json!({"selected":selected}))
    }

    #[allow(clippy::too_many_lines)] // Mirrors the source's publish/attach/reconcile transaction.
    fn session_create(&self, payload: &Value) -> anyhow::Result<RpcResult<Value>> {
        let mut state = self.state.lock();
        let workspace_id = payload.get("workspaceId").and_then(Value::as_str);
        let workspace_index =
            workspace_id.and_then(|id| find_by(&state.workspaces, "workspaceId", id));
        if workspace_id.is_some() && workspace_index.is_none() {
            return Ok(failure(
                "workspace-not-found",
                &format!("no workspace {}", workspace_id.unwrap_or_default()),
                json!({"workspaceId":workspace_id}),
            ));
        }
        let cwd = workspace_index
            .and_then(|index| {
                state.workspaces[index]
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "/tmp/fixture".to_owned());
        let requested = payload
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(id) = requested.as_deref()
            && let Some(index) = find_by(&state.sessions, "sessionId", id)
        {
            let existing_cwd = state.sessions[index].get("cwd").and_then(Value::as_str);
            if existing_cwd != Some(cwd.as_str()) {
                let mut details = json!({"sessionId":id,"requestedCwd":cwd});
                if let Some(existing_cwd) = existing_cwd {
                    details["existingCwd"] = json!(existing_cwd);
                }
                return Ok(failure(
                    "session-conflict",
                    &format!(
                        "session {id} already uses {}",
                        existing_cwd.unwrap_or("no cwd")
                    ),
                    details,
                ));
            }
            if let Some(workspace_index) = workspace_index {
                let contains = state.workspaces[workspace_index]
                    .get("sessionIds")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(id)));
                if !contains {
                    if self.options.fail_workspace_attach {
                        return Ok(workspace_attach_failure(
                            id,
                            workspace_id.unwrap_or_default(),
                        ));
                    }
                    attach_session(
                        &mut state.workspaces[workspace_index],
                        id,
                        self.next_timestamp(),
                    );
                    let frame = json!({"type":"host/workspace-changed","workspace":state.workspaces[workspace_index]});
                    drop(state);
                    self.emit_host(frame);
                }
            }
            return Ok(success(json!({"sessionId":id})));
        }
        let id = requested.unwrap_or_else(|| {
            let id = format!("fx-{}", state.next_session);
            state.next_session += 1;
            id
        });
        let created = json!({
            "sessionId":id,"updatedAt":self.next_timestamp(),"running":false,"blank":true,"cwd":cwd,
        });
        state.sessions.push(created);
        state
            .model_selections
            .insert(id.clone(), default_model_selection());
        let session_frame =
            json!({"type":"host/session-added","sessionId":id,"blank":true,"cwd":cwd});
        let mut workspace_frame = None;
        if let Some(index) = workspace_index {
            if self.options.fail_workspace_attach {
                drop(state);
                self.emit_host(session_frame);
                return Ok(workspace_attach_failure(
                    &id,
                    workspace_id.unwrap_or_default(),
                ));
            }
            attach_session(&mut state.workspaces[index], &id, self.next_timestamp());
            workspace_frame =
                Some(json!({"type":"host/workspace-changed","workspace":state.workspaces[index]}));
        }
        drop(state);
        match (self.options.create_frame_order, workspace_frame) {
            (FixtureCreateFrameOrder::WorkspaceFirst, Some(workspace)) => {
                self.emit_host(workspace);
                self.emit_host(session_frame);
            }
            (_, workspace) => {
                self.emit_host(session_frame);
                if let Some(workspace) = workspace {
                    self.emit_host(workspace);
                }
            }
        }
        if self.options.drop_session_create_response {
            anyhow::bail!("fixture: dropped session.create response after publication");
        }
        Ok(success(json!({"sessionId":id})))
    }

    fn session_rename(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "sessionId").unwrap_or_default();
        if !self.has_session(&id) {
            return session_missing(&id);
        }
        let title = string_at(payload, "title").unwrap_or_default();
        let normalized = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            return failure(
                "title-invalid",
                "session title must contain visible characters",
                json!({"sessionId":id}),
            );
        }
        let entry = self.append_event(&id, json!({"type":"session/title","data":{"title":normalized,"messageSeqs":[],"source":{"kind":"user"}}}));
        let seq = entry.pointer("/event/seq").cloned().unwrap_or(json!(0));
        self.emit_mux(json!({"type":"session/projection","sessionId":id,"key":"title","value":normalized,"seq":seq}));
        success(json!({"title":normalized,"seq":seq}))
    }

    fn session_fork(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "sessionId").unwrap_or_default();
        let mut state = self.state.lock();
        let Some(source_index) = find_by(&state.sessions, "sessionId", &id) else {
            return session_missing(&id);
        };
        let source = state.sessions[source_index].clone();
        let log = state.logs.get(&id).cloned().unwrap_or_default();
        let at = payload.get("atSeq").and_then(Value::as_i64);
        let boundary = log
            .iter()
            .find(|entry| {
                entry.pointer("/event/type").and_then(Value::as_str) == Some("turn/end")
                    && at.is_none_or(|at| {
                        entry
                            .pointer("/event/seq")
                            .and_then(Value::as_i64)
                            .is_some_and(|seq| seq >= at)
                    })
            })
            .or_else(|| {
                (at.is_none() || at.is_some_and(|at| at >= sequence_from_len(log.len())))
                    .then(|| {
                        log.iter().rev().find(|entry| {
                            entry.pointer("/event/type").and_then(Value::as_str) == Some("turn/end")
                        })
                    })
                    .flatten()
            });
        let Some(boundary) = boundary else {
            return failure(
                "fork-unavailable",
                &format!("session {id} has no completed turn"),
                json!({"sessionId":id}),
            );
        };
        let boundary_seq = boundary
            .pointer("/event/seq")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or_default();
        let mut cut = boundary_seq + 1;
        while cut < log.len()
            && log[cut].pointer("/event/type").and_then(Value::as_str) != Some("turn/start")
        {
            cut += 1;
        }
        let child_id = format!("fx-{}", state.next_session);
        state.next_session += 1;
        let mut child = json!({"sessionId":child_id,"updatedAt":self.next_timestamp(),"running":false,"blank":false,"parentSessionId":id});
        if let Some(cwd) = source.get("cwd") {
            child["cwd"] = cwd.clone();
        }
        state.sessions.push(child.clone());
        state.logs.insert(child_id.clone(), log[..cut].to_vec());
        drop(state);
        self.emit_host(json!({"type":"host/session-added","sessionId":child_id,"blank":false,"parentSessionId":id,"cwd":child.get("cwd")}));
        success(json!({"sessionId":child_id}))
    }

    fn session_attachment(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "attachmentId").unwrap_or_default();
        let session_id = string_at(payload, "sessionId").unwrap_or_default();
        let state = self.state.lock();
        let Some(stored) = state.attachments.get(&id).cloned() else {
            return failure(
                "attachment-error",
                "fixture attachment missing",
                json!({"reason":"ATTACHMENT_NOT_FOUND"}),
            );
        };
        let referenced = state
            .logs
            .get(&session_id)
            .is_some_and(|log| log.iter().any(|entry| entry.to_string().contains(&id)));
        if !referenced {
            return failure(
                "attachment-error",
                "fixture attachment is not referenced by this session",
                json!({"reason":"ATTACHMENT_NOT_REFERENCED"}),
            );
        }
        success(stored)
    }

    fn session_prompt(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "sessionId").unwrap_or_default();
        if !self.has_session(&id) {
            return session_missing(&id);
        }
        if self.options.reject_prompt {
            return failure(
                "agent-busy",
                "fixture: prompt rejected before acceptance",
                json!({"reason":"fixture-prompt-rejection"}),
            );
        }
        let mode = string_at(payload, "mode").unwrap_or_default();
        let content = payload
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let text = content
            .iter()
            .filter_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<String>();
        let durable = {
            let mut state = self.state.lock();
            content
                .iter()
                .map(|block| {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        return block.clone();
                    }
                    let data = block.get("data").and_then(Value::as_str).unwrap_or_default();
                    let attachment_id = format!("fixture:upload-{}", state.next_attachment);
                    state.next_attachment += 1;
                    let padding = usize::from(data.ends_with('='))
                        + usize::from(data.ends_with("=="));
                    let bytes = (data.len() * 3 / 4).saturating_sub(padding).max(1);
                    let mut attachment = json!({
                        "attachmentId":attachment_id,
                        "mediaType":block.get("mediaType").cloned().unwrap_or_else(||json!("image/png")),
                        "bytes":bytes,
                        "width":160,
                        "height":90,
                    });
                    if let Some(name) = block.get("name") {
                        attachment["name"] = name.clone();
                    }
                    state.attachments.insert(
                        attachment_id,
                        json!({"attachment":attachment,"data":data}),
                    );
                    json!({"type":"image","attachment":attachment})
                })
                .collect::<Vec<_>>()
        };
        if mode == "steer" && self.state.lock().replays.contains_key(&id) {
            self.append_event(&id, user_content_event(&durable));
            return success(json!({"accepted":true}));
        }
        let turn = {
            let mut state = self.state.lock();
            let turn = state.next_turn.entry(id.clone()).or_default();
            let value = *turn;
            *turn += 1;
            value
        };
        self.set_running(&id, true);
        self.append_event(&id, json!({"type":"turn/start","data":{"turn":turn}}));
        self.append_event(&id, user_content_event(&durable));
        let reply = if text == "render markdown" {
            "# Markdown fixture\n\nAssistant output renders **strong text**, *emphasis*, and `inline code`.".to_owned()
        } else if text == "report model" {
            let selection = self
                .state
                .lock()
                .model_selections
                .get(&id)
                .cloned()
                .unwrap_or_else(default_model_selection);
            format!(
                "当前模型：{}/{}",
                string_at(&selection, "provider").unwrap_or_default(),
                string_at(&selection, "model").unwrap_or_default()
            )
        } else {
            format!("回声：{text}。这是 fixture 的流式回复，用于验证打字机增长与定稿切换。")
        };
        self.start_reply(id, turn, reply);
        success(json!({"accepted":true}))
    }

    fn start_reply(&self, id: String, turn: u64, reply: String) {
        let Some(fixture) = self.weak_self.upgrade() else {
            return;
        };
        let signal = AbortSignal::default();
        self.state.lock().replays.insert(
            id.clone(),
            Replay {
                signal: signal.clone(),
            },
        );
        self.append_event(
            &id,
            json!({"type":"step/start","data":{"turn":turn,"step":0}}),
        );
        self.append_event(&id, json!({"type":"assistant/chunk","data":{"turn":turn,"step":0,"chunk":{"type":"block-start","index":0,"blockType":"text"}}}));
        tokio::spawn(async move {
            let pieces = reply
                .chars()
                .collect::<Vec<_>>()
                .chunks(6)
                .map(|chunk| chunk.iter().collect::<String>())
                .collect::<Vec<_>>();
            let mut complete = String::new();
            let mut aborted = false;
            for piece in pieces {
                tokio::select! {
                    () = signal.cancelled() => { aborted=true; break; }
                    () = tokio::time::sleep(Duration::from_millis(80)) => {}
                }
                complete.push_str(&piece);
                fixture.append_event(&id, json!({"type":"assistant/chunk","data":{"turn":turn,"step":0,"chunk":{"type":"text-delta","index":0,"text":piece}}}));
            }
            if aborted {
                complete.push_str("（已中断）");
            }
            fixture.append_event(&id, json!({"type":"assistant/chunk","data":{"turn":turn,"step":0,"chunk":{"type":"block-end","index":0,"block":{"type":"text","text":complete}}}}));
            fixture.append_event(&id, json!({"type":"assistant/message","surfaceOp":"append","data":{"turn":turn,"step":0,"message":{"role":"assistant","content":[{"type":"text","text":complete}],"source":{"provider":"fixture","model":"fx-1"}}}}));
            fixture.append_event(
                &id,
                json!({"type":"step/end","data":{"turn":turn,"step":0}}),
            );
            fixture.append_event(&id, json!({"type":"turn/end","data":{"turn":turn,"reason":{"kind":if aborted{"cancelled"}else{"completed"}}}}));
            fixture.state.lock().replays.remove(&id);
            fixture.set_running(&id, false);
        });
    }

    fn session_cancel(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "sessionId").unwrap_or_default();
        if let Some(replay) = self.state.lock().replays.get(&id) {
            replay.signal.abort();
        } else {
            self.set_running(&id, false);
        }
        success(json!({"accepted":true}))
    }

    fn list_directory(&self, payload: &Value) -> RpcResult<Value> {
        let path = payload
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("/home/fixture");
        self.state
            .lock()
            .directories
            .get(path)
            .cloned()
            .map_or_else(
                || {
                    failure(
                        "directory-unreadable",
                        &format!("cannot list {path}: not in the fixture tree"),
                        json!({"path":path}),
                    )
                },
                success,
            )
    }

    fn create_directory(&self, payload: &Value) -> RpcResult<Value> {
        let parent = string_at(payload, "path").unwrap_or_default();
        let name = string_at(payload, "name").unwrap_or_default();
        let mut state = self.state.lock();
        let Some(row) = state.directories.get(&parent).cloned() else {
            return failure(
                "directory-create-failed",
                &format!("missing parent {parent}"),
                json!({"path":parent}),
            );
        };
        let target = if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        };
        if row
            .get("entries")
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.get("name").and_then(Value::as_str) == Some(name.as_str()))
            })
        {
            return failure(
                "directory-exists",
                &format!("{target} already exists"),
                json!({"path":target}),
            );
        }
        let empty = json!({"path":target,"home":"/home/fixture","crumbs":crumbs(&target),"entries":[],"truncated":false});
        state.directories.insert(target.clone(), empty);
        if let Some(entries) = state
            .directories
            .get_mut(&parent)
            .and_then(|row| row.get_mut("entries"))
            .and_then(Value::as_array_mut)
        {
            entries.push(json!({
                "name": name,
                "path": target,
                "hidden": name.starts_with('.'),
            }));
            entries.sort_by(|left, right| {
                left.get("name")
                    .and_then(Value::as_str)
                    .cmp(&right.get("name").and_then(Value::as_str))
            });
        }
        success(json!({"path":target}))
    }

    fn workspace_list(&self) -> RpcResult<Value> {
        let state = self.state.lock();
        success(json!({"items":state.workspaces,"archivedSessionIds":state.archived_session_ids}))
    }
    fn workspace_create(&self, payload: &Value) -> RpcResult<Value> {
        let path = string_at(payload, "path").unwrap_or_default();
        let mut state = self.state.lock();
        if let Some(index) = find_by(&state.workspaces, "path", &path) {
            return success(json!({"workspace":state.workspaces[index],"created":false}));
        }
        let now = iso_time(self.next_timestamp());
        let id = format!("fx-ws-{}", state.next_workspace);
        state.next_workspace += 1;
        let workspace = json!({"workspaceId":id,"path":path,"title":path.rsplit('/').find(|part|!part.is_empty()).unwrap_or(&path),"sessionIds":[],"createdAt":now,"updatedAt":now});
        state.workspaces.insert(0, workspace.clone());
        drop(state);
        self.emit_host(json!({"type":"host/workspace-changed","workspace":workspace}));
        success(json!({"workspace":workspace,"created":true}))
    }
    fn workspace_rename(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "workspaceId").unwrap_or_default();
        let title = string_at(payload, "title")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let mut state = self.state.lock();
        let Some(index) = find_by(&state.workspaces, "workspaceId", &id) else {
            return workspace_missing(&id);
        };
        if state.workspaces.iter().enumerate().any(|(i, w)| {
            i != index && w.get("title").and_then(Value::as_str) == Some(title.as_str())
        }) {
            return failure(
                "workspace-name-conflict",
                &format!("workspace name '{title}' is already in use"),
                json!({"name":title}),
            );
        }
        if state.workspaces[index].get("title").and_then(Value::as_str) != Some(title.as_str()) {
            state.workspaces[index]["title"] = json!(title);
            state.workspaces[index]["updatedAt"] = json!(iso_time(self.next_timestamp()));
            let frame =
                json!({"type":"host/workspace-changed","workspace":state.workspaces[index]});
            let value = state.workspaces[index].clone();
            drop(state);
            self.emit_host(frame);
            return success(json!({"workspace":value}));
        }
        success(json!({"workspace":state.workspaces[index]}))
    }
    fn workspace_delete(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "workspaceId").unwrap_or_default();
        let mut state = self.state.lock();
        let Some(index) = find_by(&state.workspaces, "workspaceId", &id) else {
            return workspace_missing(&id);
        };
        state.workspaces.remove(index);
        drop(state);
        self.emit_host(json!({"type":"host/workspace-removed","workspaceId":id}));
        success(json!({"deleted":true}))
    }
    fn workspace_insert_before(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "workspaceId").unwrap_or_default();
        let before = payload
            .get("beforeWorkspaceId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut state = self.state.lock();
        let Some(source) = find_by(&state.workspaces, "workspaceId", &id) else {
            return workspace_missing(&id);
        };
        if let Some(before) = before.as_deref()
            && find_by(&state.workspaces, "workspaceId", before).is_none()
        {
            return workspace_missing(before);
        }
        if before.as_deref() != Some(id.as_str()) {
            let previous = state
                .workspaces
                .iter()
                .filter_map(|workspace| workspace.get("workspaceId").cloned())
                .collect::<Vec<_>>();
            let value = state.workspaces.remove(source);
            let at = before
                .as_deref()
                .and_then(|before| find_by(&state.workspaces, "workspaceId", before))
                .unwrap_or(state.workspaces.len());
            state.workspaces.insert(at, value);
            let ids = state
                .workspaces
                .iter()
                .filter_map(|w| w.get("workspaceId").cloned())
                .collect::<Vec<_>>();
            if ids == previous {
                return success(json!({"workspaceIds":ids}));
            }
            drop(state);
            self.emit_host(json!({"type":"host/workspace-order-changed","workspaceIds":ids}));
            return success(json!({"workspaceIds":ids}));
        }
        success(
            json!({"workspaceIds":state.workspaces.iter().filter_map(|w|w.get("workspaceId").cloned()).collect::<Vec<_>>()}),
        )
    }
    fn workspace_insert_session_before(&self, payload: &Value) -> RpcResult<Value> {
        let wid = string_at(payload, "workspaceId").unwrap_or_default();
        let sid = string_at(payload, "sessionId").unwrap_or_default();
        let before = payload.get("beforeSessionId").and_then(Value::as_str);
        let mut state = self.state.lock();
        let Some(index) = find_by(&state.workspaces, "workspaceId", &wid) else {
            return workspace_missing(&wid);
        };
        let ids = state.workspaces[index]
            .get("sessionIds")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !ids.iter().any(|v| v.as_str() == Some(sid.as_str()))
            || before.is_some_and(|before| !ids.iter().any(|v| v.as_str() == Some(before)))
        {
            return failure(
                "workspace-move-invalid",
                &format!("session or anchor is not accounted by workspace {wid}"),
                json!({"workspaceId":wid,"sessionId":sid}),
            );
        }
        let previous = ids.clone();
        let mut next = ids
            .into_iter()
            .filter(|v| v.as_str() != Some(sid.as_str()))
            .collect::<Vec<_>>();
        let at = before
            .and_then(|before| next.iter().position(|v| v.as_str() == Some(before)))
            .unwrap_or(next.len());
        next.insert(at, json!(sid));
        if next == previous {
            return success(json!({"workspace":state.workspaces[index]}));
        }
        state.workspaces[index]["sessionIds"] = json!(next);
        state.workspaces[index]["updatedAt"] = json!(iso_time(self.next_timestamp()));
        let workspace = state.workspaces[index].clone();
        drop(state);
        self.emit_host(json!({"type":"host/workspace-changed","workspace":workspace}));
        success(json!({"workspace":workspace}))
    }
    fn workspace_archive_session(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "sessionId").unwrap_or_default();
        if !self.has_session(&id) {
            return session_missing(&id);
        }
        let mut state = self.state.lock();
        if !state.archived_session_ids.contains(&id) {
            state.archived_session_ids.push(id.clone());
            let ids = state.archived_session_ids.clone();
            drop(state);
            self.emit_host(
                json!({"type":"host/archived-sessions-changed","archivedSessionIds":ids}),
            );
            return success(json!({"archivedSessionIds":ids}));
        }
        success(json!({"archivedSessionIds":state.archived_session_ids}))
    }

    fn skill_list(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "sessionId")
            .unwrap_or_else(|| string_at(payload, "agentId").unwrap_or_default());
        if !self.has_session(&id) {
            return session_missing(&id);
        }
        success(self.state.lock().skills.clone())
    }
    fn preset_list(&self) -> RpcResult<Value> {
        let state = self.state.lock();
        success(
            json!({"presets":state.presets.iter().map(|(id,(trust,_))|json!({"id":id,"trust":trust,"isDefault":id==&state.default_preset})).collect::<Vec<_>>(),"authorable":true,"hasDocument":true}),
        )
    }
    fn preset_select(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "agentPreset").unwrap_or_default();
        self.state.lock().default_preset.clone_from(&id);
        success(json!({"agentPreset":id}))
    }
    fn preset_read(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "agentPreset").unwrap_or_default();
        self.state.lock().presets.get(&id).map_or_else(
            || {
                failure(
                    "agent-preset-not-found",
                    &format!("unknown agent preset {id:?}"),
                    json!({"agentPreset":id}),
                )
            },
            |(trust, content)| success(json!({"agentPreset":id,"trust":trust,"content":content})),
        )
    }
    fn preset_copy(&self, payload: &Value) -> RpcResult<Value> {
        let from = string_at(payload, "from")
            .unwrap_or_else(|| string_at(payload, "agentPreset").unwrap_or_default());
        let id = string_at(payload, "agentPreset").unwrap_or_default();
        let mut state = self.state.lock();
        let Some((_, content)) = state.presets.get(&from).cloned() else {
            return failure(
                "agent-preset-not-found",
                &format!("unknown agent preset {from:?}"),
                json!({"agentPreset":from}),
            );
        };
        if state.presets.contains_key(&id) {
            return failure(
                "agent-preset-invalid",
                &format!("agent preset {id:?} already exists"),
                json!({"agentPreset":id,"reason":"already exists"}),
            );
        }
        state
            .presets
            .insert(id.clone(), ("user".to_owned(), content));
        success(json!({"agentPreset":id}))
    }
    fn preset_open(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "agentPreset").unwrap_or_default();
        let state = self.state.lock();
        match state.presets.get(&id) {
            Some((trust, _)) if trust == "user" => success(json!({"opened":true})),
            _ => failure(
                "agent-preset-read-only",
                &format!("agent preset {id:?} ships with the deployment"),
                json!({"agentPreset":id}),
            ),
        }
    }
    fn preset_remove(&self, payload: &Value) -> RpcResult<Value> {
        let id = string_at(payload, "agentPreset").unwrap_or_default();
        let mut state = self.state.lock();
        if state
            .presets
            .get(&id)
            .is_some_and(|(trust, _)| trust == "system")
        {
            return failure(
                "agent-preset-read-only",
                &format!("agent preset {id:?} ships with the deployment"),
                json!({"agentPreset":id}),
            );
        }
        state.presets.remove(&id);
        success(json!({}))
    }

    fn credentials_describe(&self, payload: &Value) -> RpcResult<Value> {
        let state = self.state.lock();
        let credentials = payload
            .get("refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|key| {
                let configured = state.credentials.contains(key);
                let mut view = json!({
                    "configured": configured,
                    "writable": true,
                });
                if configured {
                    view["source"] = json!("file");
                }
                (key.to_owned(), view)
            })
            .collect::<Map<_, _>>();
        success(json!({"credentials":credentials}))
    }
    fn credentials_set(&self, payload: &Value, set: bool) -> RpcResult<Value> {
        let key = string_at(payload, "ref").unwrap_or_default();
        if set {
            self.state.lock().credentials.insert(key);
        } else {
            self.state.lock().credentials.remove(&key);
        }
        success(json!({}))
    }
    fn discover_models(&self) -> RpcResult<Value> {
        let state = self.state.lock();
        let models = state
            .model_groups
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| {
                group
                    .get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|model| json!({"id":model.get("id"),"name":model.get("name")}))
            })
            .collect::<Vec<_>>();
        success(json!({"models":models}))
    }

    fn command_list(&self, payload: &Value) -> RpcResult<Value> {
        let id = agent_id(payload);
        if !self.has_session(&id) {
            return session_missing(&id);
        }
        success(json!([
            {"name":"compact","description":"fixture：压缩当前会话上下文"},{"name":"echo","description":"fixture：回显参数","input":{"hint":"text to echo"}},{"name":"goal","description":"set or view the goal for a long-running task","input":{"hint":"<objective>"}},{"name":"permission","description":"Switch the permission preset (sandbox mode + approval policy)","input":{"hint":"<preset>"}},{"name":"plan","description":"Enter or leave plan mode","input":{"hint":"[off|message]"}}
        ]))
    }
    fn command_execute(&self, payload: &Value) -> RpcResult<Value> {
        let id = agent_id(payload);
        if !self.has_session(&id) {
            return session_missing(&id);
        }
        let line = payload
            .pointer("/args/line")
            .and_then(Value::as_str)
            .or_else(|| payload.get("line").and_then(Value::as_str))
            .unwrap_or_default()
            .trim();
        let Some(rest) = line.strip_prefix('/') else {
            return success(Value::Null);
        };
        let mut parts = rest.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        let raw = parts.next().map(|v| format!(" {v}")).unwrap_or_default();
        let text = match name {
            "compact" => Some("fixture：已压缩（假动作）".to_owned()),
            "echo" => Some(raw.trim().to_owned()),
            "goal" => Some(if raw.trim().is_empty() {
                "No goal is set. Usage: /goal <objective>".to_owned()
            } else {
                format!("Goal created: {}", raw.trim())
            }),
            "permission" => Some(format!("preset {}", raw.trim())),
            "plan" => Some(if raw.trim() == "off" {
                "Plan mode off.".to_owned()
            } else {
                "Plan mode on. Use /plan off to leave.".to_owned()
            }),
            _ => None,
        };
        let Some(text) = text else {
            return success(Value::Null);
        };
        let command_id = format!("fx-cmd-{}", self.log_len(&id));
        self.append_event(&id,json!({"type":"command/run","data":{"commandId":command_id,"name":name,"args":raw,"source":{"kind":"user"}}}));
        let result = json!({"kind":"success","text":text});
        self.append_event(&id,json!({"type":"command/done","data":{"commandId":command_id,"kind":"success","text":text}}));
        success(json!({"commandId":command_id,"result":result}))
    }

    fn goal_create(&self, payload: &Value) -> RpcResult<Value> {
        let id = agent_id(payload);
        if !self.has_session(&id) {
            return session_missing(&id);
        }
        let objective = payload
            .pointer("/args/request/objective")
            .and_then(Value::as_str)
            .or_else(|| payload.get("objective").and_then(Value::as_str))
            .unwrap_or_default();
        let mut state = self.state.lock();
        let goal_id = format!("fx-goal-{}", state.next_goal);
        state.next_goal += 1;
        drop(state);
        let goal = json!({"id":goal_id,"revision":1,"objective":objective,"phase":"active","maxGoalRounds":256});
        self.append_event(&id,json!({"type":"goal/change","data":{"kind":"goal/change","version":1,"operation":"create","goal":goal,"roundsStarted":0,"createdAt":self.next_timestamp(),"updatedAt":self.next_timestamp()}}));
        success(json!({"ref":{"id":goal_id,"revision":1}}))
    }
    fn goal_mutate(&self, payload: &Value, operation: &str) -> RpcResult<Value> {
        let id = agent_id(payload);
        let Some(current) = self.current_goal(&id) else {
            return failure("internal", "stale or missing goal revision", json!({}));
        };
        let ref_value = payload.pointer("/args/ref").or_else(|| payload.get("ref"));
        if ref_value.and_then(|v| v.get("id")) != current.get("id")
            || ref_value.and_then(|v| v.get("revision")) != current.get("revision")
        {
            return failure("internal", "stale or missing goal revision", json!({}));
        }
        let phase = match operation {
            "pause" => "paused",
            "resume" => "active",
            "complete" => {
                if current.get("phase").and_then(Value::as_str) == Some("complete") {
                    return failure(
                        "internal",
                        "invalid goal transition from \"complete\"",
                        json!({}),
                    );
                }
                "complete"
            }
            _ => current
                .get("phase")
                .and_then(Value::as_str)
                .unwrap_or("active"),
        };
        let revision = current
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            + 1;
        let mut goal = current.clone();
        goal["revision"] = json!(revision);
        goal["phase"] = json!(phase);
        if let Some(objective) = payload
            .pointer("/args/request/objective")
            .or_else(|| payload.get("objective"))
        {
            goal["objective"] = objective.clone();
        }
        self.append_event(&id,json!({"type":"goal/change","data":{"kind":"goal/change","version":1,"operation":operation,"goal":goal,"roundsStarted":0,"createdAt":self.next_timestamp(),"updatedAt":self.next_timestamp()}}));
        success(json!({"ref":{"id":goal.get("id"),"revision":revision}}))
    }
    fn goal_clear(&self, payload: &Value) -> RpcResult<Value> {
        let id = agent_id(payload);
        let Some(current) = self.current_goal(&id) else {
            return failure("internal", "stale or missing goal revision", json!({}));
        };
        let revision = current
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            + 1;
        self.append_event(&id,json!({"type":"goal/change","data":{"kind":"goal/change","version":1,"operation":"clear","cleared":{"id":current.get("id"),"revision":revision},"clearedAt":self.next_timestamp()}}));
        success(json!({"cleared":true}))
    }
    fn current_goal(&self, id: &str) -> Option<Value> {
        let state = self.state.lock();
        state.logs.get(id)?.iter().rev().find_map(|entry| {
            let event = entry.get("event")?;
            (event.get("type")?.as_str()? == "goal/change")
                .then(|| event.pointer("/data/goal").cloned())
                .flatten()
        })
    }

    fn has_session(&self, id: &str) -> bool {
        find_by(&self.state.lock().sessions, "sessionId", id).is_some()
    }
    fn log_len(&self, id: &str) -> usize {
        self.state.lock().logs.get(id).map_or(0, Vec::len)
    }
    fn append_event(&self, session_id: &str, mut event: Value) -> Value {
        let time = self.next_timestamp();
        let entry = {
            let mut state = self.state.lock();
            let log = state.logs.entry(session_id.to_owned()).or_default();
            let seq = log.len();
            event["seq"] = json!(seq);
            event["time"] = json!(time);
            let entry = json!({"event":event});
            log.push(entry.clone());
            entry
        };
        self.emit_mux(
            json!({"type":"session/event","sessionId":session_id,"event":entry.get("event")}),
        );
        entry
    }
    fn set_running(&self, id: &str, running: bool) {
        let mut state = self.state.lock();
        let Some(index) = find_by(&state.sessions, "sessionId", id) else {
            return;
        };
        if state.sessions[index]
            .get("running")
            .and_then(Value::as_bool)
            == Some(running)
        {
            return;
        }
        state.sessions[index]["running"] = json!(running);
        drop(state);
        self.emit_host(json!({"type":"host/session-status","sessionId":id,"running":running}));
    }
    fn emit_mux(&self, payload: Value) {
        self.emit(&self.mux_senders, payload);
    }
    fn emit_host(&self, payload: Value) {
        self.emit(&self.host_senders, payload);
    }
    fn emit(
        &self,
        senders: &Mutex<Vec<mpsc::UnboundedSender<anyhow::Result<EventFrame>>>>,
        payload: Value,
    ) {
        let frame = EventFrame {
            rpc_id: self.mint_rpc(),
            payload,
        };
        let mut senders = senders.lock();
        senders.retain(|sender| sender.send(Ok(frame.clone())).is_ok());
    }
    fn mint_rpc(&self) -> RpcId {
        RpcId::new(format!(
            "fx-rpc-{}",
            self.next_rpc.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn next_timestamp(&self) -> u64 {
        self.next_time.fetch_add(1, Ordering::Relaxed)
    }
    fn tap_server_frame(&self, frame: &EventFrame) {
        let method = frame
            .payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.publish_envelopes(&[json!({
            "type":"server-request",
            "rpcId":frame.rpc_id,
            "method":method,
            "payload":frame.payload,
        })]);
    }
}

impl ClientConnection for FixtureApi {
    fn call(
        &self,
        channel: &str,
        endpoint: &str,
        payload: Value,
        signal: AbortSignal,
    ) -> ClientConnectionFuture {
        let Some(fixture) = self.weak_self.upgrade() else {
            return async { anyhow::bail!("fixture transport is no longer live") }.boxed();
        };
        let channel = channel.to_owned();
        let endpoint = endpoint.to_owned();
        async move {
            anyhow::ensure!(
                channel == "/api",
                "fixture connection RPC channel {channel:?} is unavailable"
            );
            fixture.dispatch(&endpoint, payload, signal).await
        }
        .boxed()
    }
}

impl StreamApi for FixtureApi {
    fn describe(&self) -> BoxFuture<'static, anyhow::Result<RpcResult<HostDescription>>> {
        let value = self.state.lock().host_description.clone();
        async move { Ok(success(value)) }.boxed()
    }
    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        let Some(fixture) = self.weak_self.upgrade() else {
            return Box::pin(futures::stream::empty());
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        fixture.mux_senders.lock().push(tx);
        Box::pin(async_stream::stream! {
            on_open();
            for frame in fixture.initial_mux_frames() {
                fixture.tap_server_frame(&frame);
                yield Ok(frame);
            }
            loop {
                tokio::select! {
                    () = signal.cancelled() => break,
                    item = rx.recv() => match item {
                        Some(Ok(frame)) => {
                            fixture.tap_server_frame(&frame);
                            yield Ok(frame);
                        }
                        Some(Err(error)) => yield Err(error),
                        None => break,
                    }
                }
            }
        })
    }
    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        let Some(fixture) = self.weak_self.upgrade() else {
            return Box::pin(futures::stream::empty());
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        fixture.host_senders.lock().push(tx);
        Box::pin(async_stream::stream! {
            on_open();
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.tick().await;
            loop {
                tokio::select! {
                    () = signal.cancelled() => break,
                    item = rx.recv() => match item {
                        Some(Ok(frame)) => {
                            fixture.tap_server_frame(&frame);
                            yield Ok(frame);
                        }
                        Some(Err(error)) => yield Err(error),
                        None => break,
                    },
                    _ = interval.tick() => {
                        let running = {
                            let mut state = fixture.state.lock();
                            let Some(index) = find_by(&state.sessions, "sessionId", "fx-gamma") else {
                                continue;
                            };
                            let next = !state.sessions[index]
                                .get("running")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            state.sessions[index]["running"] = json!(next);
                            next
                        };
                        let frame = EventFrame {
                            rpc_id: fixture.mint_rpc(),
                            payload: json!({"type":"host/session-status","sessionId":"fx-gamma","running":running}),
                        };
                        fixture.tap_server_frame(&frame);
                        yield Ok(frame);
                    }
                }
            }
        })
    }
}

impl FixtureApi {
    fn initial_mux_frames(&self) -> Vec<EventFrame> {
        let state = self.state.lock();
        let mut frames = Vec::new();
        for session in &state.sessions {
            if session.get("running").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let id = session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let log = state.logs.get(id).map_or(0, Vec::len);
            let seq = sequence_from_len(log);
            frames.push(EventFrame {
                rpc_id: self.mint_rpc(),
                payload: json!({"type":"session/subscribed","sessionId":id,"lastSeq":seq}),
            });
            for (key, value) in state.history_projections.as_object().into_iter().flatten() {
                frames.push(EventFrame {
                    rpc_id: self.mint_rpc(),
                    payload: json!({"type":"session/projection","sessionId":id,"key":key,"value":value,"seq":seq}),
                });
            }
        }
        if let Some((id, payload)) = &state.pending_approval {
            frames.push(EventFrame {
                rpc_id: id.clone(),
                payload: payload.clone(),
            });
        }
        if let Some((id, payload)) = &state.pending_question {
            frames.push(EventFrame {
                rpc_id: id.clone(),
                payload: payload.clone(),
            });
        }
        frames
    }
}

/// Builds the complete fixture-backed Client Connection handle.
#[must_use]
pub fn fixture_connection(
    options: FixtureOptions,
    is_loopback: bool,
) -> Arc<ClientConnectionHandle> {
    let api = FixtureApi::new(options);
    api.connection_handle(is_loopback)
}

/// Parses fixture query switches, derives page authority, and installs the fixture transport.
///
/// # Errors
///
/// Returns duplicate-service or inactive-owner failures.
pub fn install_fixture_client(
    context: &Context,
    query: &str,
    hostname: Option<&str>,
) -> anyhow::Result<Arc<FixtureApi>> {
    let api = FixtureApi::new(FixtureOptions::from_query(query));
    let is_loopback = hostname.is_none_or(crate::is_loopback_hostname);
    api.provide(context, is_loopback)?;
    Ok(api)
}

fn fixture_seed() -> &'static Value {
    static SEED: OnceLock<Value> = OnceLock::new();
    SEED.get_or_init(|| {
        serde_json::from_str(FIXTURE_SEED).expect("tracked fixture seed is valid JSON")
    })
}
fn pending_frame(envelope: &Value) -> Option<(RpcId, Value)> {
    Some((
        RpcId::new(envelope.get("rpcId")?.as_str()?),
        envelope.get("payload")?.clone(),
    ))
}
fn success(value: Value) -> RpcResult<Value> {
    RpcResult::Success { value: Some(value) }
}
fn failure(code: &str, message: &str, details: Value) -> RpcResult<Value> {
    let details = match details {
        Value::Object(details) => details,
        _ => Map::new(),
    };
    RpcResult::Failure {
        error: RpcError {
            code: code.to_owned(),
            message: message.to_owned(),
            details,
        },
    }
}
fn bad_request(field: &str) -> RpcResult<Value> {
    failure(
        "bad-request",
        &format!("missing {field}"),
        json!({"field":field}),
    )
}
fn session_missing(id: &str) -> RpcResult<Value> {
    failure(
        "session-not-found",
        &format!("no session {id}"),
        json!({"sessionId":id}),
    )
}
fn workspace_missing(id: &str) -> RpcResult<Value> {
    failure(
        "workspace-not-found",
        &format!("no workspace {id}"),
        json!({"workspaceId":id}),
    )
}
fn workspace_attach_failure(session: &str, workspace: &str) -> RpcResult<Value> {
    failure(
        "workspace-attach-failed",
        &format!("fixture rejected Workspace attachment for {session}"),
        json!({"sessionId":session,"workspaceId":workspace}),
    )
}
fn default_model_selection() -> Value {
    json!({"provider":"deepseek-official","model":"deepseek-v4-flash"})
}
fn find_by(rows: &[Value], field: &str, value: &str) -> Option<usize> {
    rows.iter()
        .position(|row| row.get(field).and_then(Value::as_str) == Some(value))
}
fn string_at(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}
fn number_at(value: &Value, field: &str) -> f64 {
    value.get(field).and_then(Value::as_f64).unwrap_or_default()
}
fn agent_id(payload: &Value) -> String {
    payload
        .pointer("/args/agentId")
        .and_then(Value::as_str)
        .or_else(|| payload.get("sessionId").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}
fn omit_null_fields(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    value
}
fn attach_session(workspace: &mut Value, id: &str, time: u64) {
    let ids = workspace
        .get("sessionIds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !ids.iter().any(|value| value.as_str() == Some(id)) {
        workspace["sessionIds"] = Value::Array(std::iter::once(json!(id)).chain(ids).collect());
        workspace["updatedAt"] = json!(iso_time(time));
    }
}
fn iso_time(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    format!("fixture-{seconds}")
}
fn crumbs(path: &str) -> Vec<Value> {
    let mut values = vec![json!({"name":"/","path":"/","hidden":false})];
    let mut current = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        current.push('/');
        current.push_str(part);
        values.push(json!({"name":part,"path":current,"hidden":false}));
    }
    values
}
fn message_value(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}],"source":{"kind":"user"},"role":"user","id":"fixture-message"})
}
fn user_message_event(text: &str) -> Value {
    json!({"type":"user/message","surfaceOp":"append","data":message_value(text)})
}
fn user_content_event(content: &[Value]) -> Value {
    json!({
        "type":"user/message",
        "surfaceOp":"append",
        "data":{"content":content,"source":{"kind":"user"},"role":"user","id":"fixture-message"},
    })
}

fn fixture_image_response() -> Value {
    json!({
        "attachment":{
            "attachmentId":"fixture:image",
            "mediaType":"image/png",
            "bytes":247,
            "width":160,
            "height":90,
            "name":"fixture-image.png",
        },
        "data":"iVBORw0KGgoAAAANSUhEUgAAAKAAAABaCAYAAAA/xl1SAAAAvklEQVR42u3SMQ0AAAjAMIyhELM4AAe8PD1qYFlk9cCXEAEDYkAwIAYEA2JAMCAGBANiQDAgBgQDYkAwIAYEA2JAMCAGBANiQDAgBgQDYkAwIAYEA2JAMCAGxIBCYEAMCAbEgGBADAgGxIBgQAwIBsSAYEAMCAbEgGBADAgGxIBgQAwIBsSAYEAMCAbEgGBADAgGxIAYEAyIAcGAGBAMiAHBgBgQDIgBwYAYEAyIAcGAGBAMiAHBgBgQDIgB4bYWLb6pnOb1xAAAAABJRU5ErkJggg==",
    })
}

fn page_of(log: &[Value], before: Option<i64>, max_messages: usize) -> Value {
    let log_len = i64::try_from(log.len()).unwrap_or(i64::MAX);
    let end = before.map_or(log.len(), |before| {
        usize::try_from(before.clamp(0, log_len)).unwrap_or(log.len())
    });
    let mut start = 0;
    let mut messages = 0;
    for index in (0..end).rev() {
        let kind = log[index].pointer("/event/type").and_then(Value::as_str);
        if matches!(kind, Some("user/message" | "assistant/message")) {
            messages += 1;
        }
        if kind == Some("turn/start") && messages >= max_messages {
            start = index;
            break;
        }
    }
    json!({"events":log[start..end],"hasMore":start>0})
}

fn searchable_event_text(event: &Value) -> String {
    let kind = event.get("type").and_then(Value::as_str);
    if !matches!(kind, Some("user/message" | "assistant/message")) {
        return String::new();
    }
    event
        .pointer("/data/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}
fn fold_char(character: char) -> char {
    match character {
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'ς' => 'σ',
        value => value.to_lowercase().next().unwrap_or(value),
    }
}
fn tokenize(value: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() {
            current.push(fold_char(character));
        } else if !current.is_empty() {
            result.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}
fn tokenize_with_spans(value: &str) -> Vec<(String, usize, usize)> {
    let chars = value.chars().collect::<Vec<_>>();
    let mut result = Vec::new();
    let mut current = String::new();
    let mut start = 0;
    for (index, character) in chars.iter().copied().enumerate() {
        if character.is_alphanumeric() {
            if current.is_empty() {
                start = index;
            }
            current.push(fold_char(character));
        } else if !current.is_empty() {
            result.push((std::mem::take(&mut current), start, index));
        }
    }
    if !current.is_empty() {
        result.push((current, start, chars.len()));
    }
    result
}
fn phrase_match(
    document: &[(String, usize, usize)],
    phrase: &[String],
) -> Option<(usize, usize, usize)> {
    if phrase.is_empty() || phrase.len() > document.len() {
        return None;
    }
    let mut count = 0;
    let mut first = None;
    let mut last = 0;
    for start in 0..=document.len() - phrase.len() {
        if document[start..start + phrase.len()]
            .iter()
            .map(|item| &item.0)
            .eq(phrase.iter())
        {
            count += 1;
            first.get_or_insert(document[start].1);
            last = document[start + phrase.len() - 1].2;
        }
    }
    (count > 0).then(|| (count, first.unwrap_or_default(), last))
}
fn search_snippet(value: &str, match_start: usize, match_end: usize) -> String {
    let chars = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect::<Vec<_>>();
    if chars.len() <= 120 {
        return chars.into_iter().collect();
    }
    let center = match_start.midpoint(match_end);
    let start = center.saturating_sub(60).min(chars.len() - 120);
    let end = (start + 120).min(chars.len());
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        chars[start..end].iter().collect::<String>(),
        if end < chars.len() { "…" } else { "" }
    )
}
fn empty_projection_values() -> Value {
    json!({"todos":null,"permissions":{"options":[{"value":"workspace-write","name":"workspace-write","description":"Write inside the workspace and permitted temporary directories; wider retries require approval."},{"value":"danger-full-access","name":"danger-full-access","description":"Full file access without approval prompts."}],"currentValue":"workspace-write"},"plan":{"active":false,"pending":false},"goal":null,"tokenUsage":{"uncachedInputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0},"contextPressure":{},"contextBreakdown":{"systemTokens":0,"toolsTokens":0,"messageTokens":0},"sessionStats":{"turns":0,"steps":0,"llmMs":0,"toolMs":0,"ttftMs":0,"ttftSteps":0,"decodeMs":0,"decodeTokens":0},"imageLimits":{"maxImageBytes":5_242_880,"maxImagesPerMessage":20,"maxMessageImageBytes":104_857_600,"maxImagePixels":40_000_000,"mediaTypes":["image/png","image/jpeg","image/webp","image/gif"]}})
}

fn sequence_from_len(length: usize) -> i64 {
    i64::try_from(length).unwrap_or(i64::MAX).saturating_sub(1)
}
