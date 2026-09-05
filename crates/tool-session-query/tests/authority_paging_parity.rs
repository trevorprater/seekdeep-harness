//! Workspace authority, paging, current-step, and lineage-redaction parity.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_core::{
    session::{AppendOptions, SessionHeader, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_scope::ScopeKey;
use seekdeep_session_query::{
    LogicalProjectionResult, SessionCorpus, SessionEventRecord, SessionEventSearchHit,
    SessionEventSurface, SessionQueryEngine, SessionQueryService, SessionRecord,
    SessionSearchCursor, SessionSearchHit, SessionSearchPage, SessionTitleObservation,
    types::{
        SessionEventSearchPage, SessionEventSearchRequest, SessionSearchExecContext,
        SessionSearchRequest, SessionTitleObservationResult,
    },
};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_prompt};
use seekdeep_tool_session_query::{Config, apply};
use seekdeep_tools::{
    ToolExecutionInput, ToolRuntime, ToolRuntimeConfig, install as install_tools,
};
use serde_json::{Value, json};

#[derive(Default)]
struct ScriptedState {
    session_pages: VecDeque<anyhow::Result<SessionSearchPage<SessionSearchHit>>>,
    event_pages: VecDeque<anyhow::Result<SessionEventSearchPage>>,
    session_requests: Vec<SessionSearchRequest>,
    event_requests: Vec<SessionEventSearchRequest>,
    title_results: Option<Vec<SessionTitleObservationResult>>,
    title_failure: Option<anyhow::Error>,
}

struct ScriptedEngine {
    corpus: Arc<SessionCorpus>,
    state: Mutex<ScriptedState>,
}

impl ScriptedEngine {
    fn new(context: &seekdeep_cordis::Context) -> Arc<Self> {
        Arc::new(Self {
            corpus: SessionCorpus::new(context, 4),
            state: Mutex::new(ScriptedState::default()),
        })
    }

    fn session_page(&self, page: SessionSearchPage<SessionSearchHit>) {
        self.state.lock().session_pages.push_back(Ok(page));
    }

    fn event_page(&self, page: SessionEventSearchPage) {
        self.state.lock().event_pages.push_back(Ok(page));
    }
}

#[async_trait]
impl SessionQueryEngine for ScriptedEngine {
    fn corpus(&self) -> &SessionCorpus {
        &self.corpus
    }

    fn read_window_max(&self) -> u64 {
        50
    }

    async fn search_sessions(
        &self,
        request: SessionSearchRequest,
        _exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionSearchPage<SessionSearchHit>> {
        let mut state = self.state.lock();
        state.session_requests.push(request);
        state.session_pages.pop_front().unwrap_or_else(|| {
            Ok(SessionSearchPage {
                items: vec![],
                next_cursor: None,
            })
        })
    }

    async fn search_events(
        &self,
        request: SessionEventSearchRequest,
        _exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionEventSearchPage> {
        let header = self.corpus.load(&request.session_id, None).await?.header;
        let mut state = self.state.lock();
        state.event_requests.push(request);
        state.event_pages.pop_front().unwrap_or_else(|| {
            Ok(SessionEventSearchPage {
                page: SessionSearchPage {
                    items: vec![],
                    next_cursor: None,
                },
                session: header,
            })
        })
    }

    async fn read_title_snapshots(
        &self,
        session_ids: &[SessionId],
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionTitleObservationResult>> {
        {
            let mut state = self.state.lock();
            if let Some(error) = state.title_failure.take() {
                return Err(error);
            }
            if let Some(results) = state.title_results.take() {
                return Ok(results);
            }
        }
        let mut results = Vec::new();
        for id in session_ids {
            match self.corpus.load(id, None).await {
                Ok(source) => results.push(LogicalProjectionResult::Fulfilled {
                    session_id: id.clone(),
                    value: SessionTitleObservation {
                        session: source.header,
                        title: None,
                    },
                }),
                Err(error) => results.push(LogicalProjectionResult::Rejected {
                    session_id: id.clone(),
                    reason: Arc::new(error),
                }),
            }
        }
        Ok(results)
    }
}

struct Harness {
    context: seekdeep_cordis::Context,
    sessions: Arc<SessionStore>,
    tools: Arc<ToolRuntime>,
    engine: Arc<ScriptedEngine>,
}

impl Harness {
    fn new(max_results: f64) -> Self {
        let context = seekdeep_cordis::Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let prompt = install_prompt(&context, SystemPromptConfig::default()).unwrap();
        let tools = install_tools(&context, &prompt, ToolRuntimeConfig::default()).unwrap();
        let engine = ScriptedEngine::new(&context);
        let erased: Arc<dyn SessionQueryEngine> = engine.clone();
        SessionQueryService::new(erased).provide(&context).unwrap();
        apply(
            &context,
            &Config {
                max_search_results: Some(max_results),
                search_timeout_ms: None,
            },
        )
        .unwrap();
        Self {
            context,
            sessions,
            tools,
            engine,
        }
    }

    fn session(
        &self,
        id: &str,
        cwd: Option<&str>,
        parent: Option<&str>,
    ) -> Arc<seekdeep_core::session::Session> {
        self.sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    cwd: cwd.map(str::to_owned),
                    parent_session: parent.map(SessionId::new),
                    created_at: Some(10),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap()
    }

    fn agent(session: Arc<seekdeep_core::session::Session>) -> Arc<Agent> {
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            seekdeep_cordis::Context::new(),
            ScopeKey::new(),
        ))
    }

    async fn execute(
        &self,
        agent: Option<&Arc<Agent>>,
        name: &str,
        arguments: Value,
    ) -> seekdeep_tools::ToolExecutionResult {
        let mut input = ToolExecutionInput::new(
            CallId::new(format!("{name}-call")),
            name,
            arguments,
            AbortSignal::default(),
        );
        input.agent = agent.cloned();
        input.agent_session = agent.map(|agent| agent.session().clone());
        self.tools.execute(input).await
    }
}

fn code(result: &seekdeep_tools::ToolExecutionResult) -> Option<&str> {
    result.error()?.info.as_ref().map(|info| info.code.as_str())
}

fn output(result: &seekdeep_tools::ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            seekdeep_llm::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn record(header: &SessionHeader, live: bool) -> SessionRecord {
    SessionRecord {
        header: header.clone(),
        live,
        persisted: false,
    }
}

fn hit(header: &SessionHeader, text: &str) -> SessionSearchHit {
    SessionSearchHit {
        record: record(header, true),
        best_match: SessionEventSearchHit {
            record: SessionEventRecord {
                session_id: header.id.clone(),
                seq: 0,
                event_type: "user/message".to_owned(),
                time: 10,
                surface: SessionEventSurface::Current,
            },
            snippet: text.to_owned(),
        },
    }
}

#[tokio::test]
async fn fails_closed_without_agent_or_cross_workspace_authority() {
    let harness = Harness::new(10.0);
    let no_agent = harness.execute(None, "session_trace", json!({})).await;
    assert_eq!(code(&no_agent), Some("SESSION_QUERY_TOOL_MISSING_AGENT"));

    let caller = Harness::agent(harness.session("caller", Some("/work"), None));
    harness.session("hidden", Some("/other"), None);
    let hidden = harness
        .execute(
            Some(&caller),
            "session_event_read",
            json!({"session_id":"hidden","seq":0}),
        )
        .await;
    assert_eq!(code(&hidden), Some("SESSION_QUERY_TOOL_UNAUTHORIZED"));

    let null_cwd = Harness::agent(harness.session("null-cwd", None, None));
    let cross_search = harness
        .execute(Some(&null_cwd), "session_search", json!({"query":"needle"}))
        .await;
    assert_eq!(code(&cross_search), Some("SESSION_QUERY_TOOL_UNAUTHORIZED"));
    let self_trace = harness
        .execute(Some(&null_cwd), "session_trace", json!({}))
        .await;
    assert!(!self_trace.is_error(), "{:?}", self_trace.error());
}

#[tokio::test]
async fn drains_hidden_pages_to_authorized_cap_and_rejects_repeated_cursor() {
    let harness = Harness::new(1.0);
    let caller_session = harness.session("caller", Some("/work"), None);
    let caller = Harness::agent(caller_session.clone());
    let hidden = harness.session("hidden-parent", Some("/other"), None);
    let first = harness.session("first", Some("/work"), Some("hidden-parent"));
    let second = harness.session("second", Some("/work"), None);
    harness.engine.session_page(SessionSearchPage {
        items: vec![
            hit(caller_session.header(), "self"),
            hit(hidden.header(), "hidden"),
        ],
        next_cursor: Some(SessionSearchCursor::new("cursor-1")),
    });
    harness.engine.session_page(SessionSearchPage {
        items: vec![hit(first.header(), "first needle")],
        next_cursor: Some(SessionSearchCursor::new("cursor-2")),
    });
    harness.engine.session_page(SessionSearchPage {
        items: vec![hit(second.header(), "second needle")],
        next_cursor: None,
    });
    let result = harness
        .execute(Some(&caller), "session_search", json!({"query":"needle"}))
        .await;
    assert!(!result.is_error(), "{:?}", result.error());
    let text = output(&result);
    assert!(text.contains("Session first"));
    assert!(text.contains("[outside workspace]"));
    assert!(text.contains("Result cap reached"));
    for hidden in ["Session caller", "Session hidden-parent", "Session second"] {
        assert!(!text.contains(hidden), "{text}");
    }

    let repeated = Harness::new(10.0);
    let repeated_caller = Harness::agent(repeated.session("repeat-caller", Some("/work"), None));
    for _ in 0..2 {
        repeated.engine.session_page(SessionSearchPage {
            items: vec![],
            next_cursor: Some(SessionSearchCursor::new("same")),
        });
    }
    let result = repeated
        .execute(
            Some(&repeated_caller),
            "session_search",
            json!({"query":"needle"}),
        )
        .await;
    assert_eq!(code(&result), Some("SESSION_QUERY_INVALID_CURSOR"));
}

#[tokio::test]
async fn current_session_search_stops_before_latest_step_and_short_circuits_empty_ranges() {
    let harness = Harness::new(10.0);
    let session = harness.session("caller", Some("/work"), None);
    session
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "user/message",
            json!({"id":"m","role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"needle"}]}),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    session
        .append(
            "step/start",
            json!({"turn":1,"step":1}),
            AppendOptions::default(),
        )
        .unwrap();
    let agent = Harness::agent(session.clone());
    let result = harness
        .execute(
            Some(&agent),
            "session_event_search",
            json!({"query":"needle"}),
        )
        .await;
    assert!(!result.is_error(), "{:?}", result.error());
    let calls = {
        let state = harness.engine.state.lock();
        let filters = state
            .event_requests
            .last()
            .unwrap()
            .filters
            .as_ref()
            .unwrap();
        assert!(filters.iter().any(|filter| matches!(
            filter,
            seekdeep_session_query::types::SessionEventMetadataFilter::Seq { to: Some(to), .. }
                if to.value().to_bits() == 1.0_f64.to_bits()
        )));
        state.event_requests.len()
    };
    let empty = harness
        .execute(
            Some(&agent),
            "session_event_search",
            json!({"query":"needle","seq_from":2}),
        )
        .await;
    assert!(!empty.is_error());
    assert!(output(&empty).contains("No prior event matches"));
    assert_eq!(harness.engine.state.lock().event_requests.len(), calls);

    let missing = Harness::new(10.0);
    let missing_agent = Harness::agent(missing.session("missing-step", Some("/work"), None));
    let result = missing
        .execute(
            Some(&missing_agent),
            "session_event_search",
            json!({"query":"needle"}),
        )
        .await;
    assert_eq!(code(&result), Some("SESSION_QUERY_TOOL_NO_CURRENT_STEP"));
}

#[tokio::test]
async fn redacts_ancestor_and_descendant_boundaries_without_leaking_hidden_ids() {
    let harness = Harness::new(10.0);
    harness.session("hidden-root", Some("/other"), None);
    let target = harness.session("target", Some("/work"), Some("hidden-root"));
    harness.session("visible-child", Some("/work"), Some("target"));
    harness.session("hidden-child", Some("/other"), Some("target"));
    harness.session("secret-grandchild", Some("/work"), Some("hidden-child"));
    let agent = Harness::agent(target);
    let result = harness
        .execute(Some(&agent), "session_trace", json!({}))
        .await;
    assert!(!result.is_error(), "{:?}", result.error());
    let text = output(&result);
    assert!(text.contains("[outside workspace boundary]"));
    assert!(text.contains("visible-child"));
    assert!(text.contains("[outside workspace subtree]"));
    for hidden in ["hidden-root", "hidden-child", "secret-grandchild"] {
        assert!(!text.contains(hidden), "{text}");
    }
}

#[tokio::test]
async fn rejects_a_target_observation_that_moves_after_preauthorization() {
    let harness = Harness::new(10.0);
    let caller = Harness::agent(harness.session("caller", Some("/work"), None));
    let target = harness.session("target", Some("/work"), None);
    let mut moved = target.header().clone();
    moved.cwd = Some("/other".to_owned());
    harness.engine.event_page(SessionEventSearchPage {
        page: SessionSearchPage {
            items: vec![],
            next_cursor: None,
        },
        session: moved,
    });
    let result = harness
        .execute(
            Some(&caller),
            "session_event_search",
            json!({"session_id":"target","query":"needle"}),
        )
        .await;
    assert_eq!(code(&result), Some("SESSION_QUERY_TOOL_UNAUTHORIZED"));
}

#[tokio::test]
async fn preserves_stale_cursor_failure_without_transparent_restart() {
    let harness = Harness::new(10.0);
    let caller = Harness::agent(harness.session("caller", Some("/work"), None));
    harness.engine.state.lock().session_pages.push_back(Err(
        seekdeep_session_query::SessionQueryError::new(
            "private stale detail",
            seekdeep_session_query::SessionQueryErrorCode::SessionQueryStaleCursor,
        )
        .into(),
    ));
    let result = harness
        .execute(Some(&caller), "session_search", json!({"query":"needle"}))
        .await;
    assert_eq!(code(&result), Some("SESSION_QUERY_STALE_CURSOR"));
    assert_eq!(harness.engine.state.lock().session_requests.len(), 1);
    assert!(!output(&result).contains("private stale detail"));
}

#[tokio::test]
async fn isolates_per_title_failures_but_not_batch_or_authority_failures() {
    let harness = Harness::new(10.0);
    let caller = Harness::agent(harness.session("caller", Some("/work"), None));
    let target = harness.session("target", Some("/work"), None);
    harness.engine.session_page(SessionSearchPage {
        items: vec![hit(target.header(), "needle")],
        next_cursor: None,
    });
    harness.engine.state.lock().title_results = Some(vec![LogicalProjectionResult::Rejected {
        session_id: target.id().clone(),
        reason: Arc::new(anyhow::Error::from(
            seekdeep_session_query::SessionQueryError::new(
                "private title failure",
                seekdeep_session_query::SessionQueryErrorCode::SessionQueryIndexFailed,
            ),
        )),
    }]);
    let result = harness
        .execute(Some(&caller), "session_search", json!({"query":"needle"}))
        .await;
    assert!(!result.is_error());
    assert!(output(&result).contains("untitled (title unavailable: SESSION_QUERY_INDEX_FAILED)"));
    assert!(!output(&result).contains("private title failure"));

    let batch = Harness::new(10.0);
    let batch_caller = Harness::agent(batch.session("caller", Some("/work"), None));
    let batch_target = batch.session("target", Some("/work"), None);
    batch.engine.session_page(SessionSearchPage {
        items: vec![hit(batch_target.header(), "needle")],
        next_cursor: None,
    });
    batch.engine.state.lock().title_failure = Some(anyhow::anyhow!("private batch failure"));
    let result = batch
        .execute(
            Some(&batch_caller),
            "session_search",
            json!({"query":"needle"}),
        )
        .await;
    assert_eq!(code(&result), Some("SESSION_QUERY_TOOL_FAILED"));
    assert!(!output(&result).contains("private batch failure"));
}

#[test]
fn projects_and_visits_deep_lineage_without_recursive_consumer_traversal() {
    let caller = seekdeep_tool_session_query::workspace_access::Caller {
        id: SessionId::new("caller"),
        header: SessionHeader {
            version: 0,
            id: SessionId::new("caller"),
            created_at: 0,
            cwd: Some("/work".to_owned()),
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        },
        events: Vec::new(),
    };
    let mut node = seekdeep_session_query::SessionLineageNode {
        session: record(
            &SessionHeader {
                id: SessionId::new("deep-9999"),
                ..caller.header.clone()
            },
            true,
        ),
        descendants: Vec::new(),
    };
    for depth in (0..9_999).rev() {
        node = seekdeep_session_query::SessionLineageNode {
            session: record(
                &SessionHeader {
                    id: SessionId::new(format!("deep-{depth}")),
                    ..caller.header.clone()
                },
                true,
            ),
            descendants: vec![node],
        };
    }
    let source = vec![node];
    let projected =
        seekdeep_tool_session_query::workspace_access::authorize_descendants(&source, &caller);
    let visits = seekdeep_tool_session_query::workspace_access::visit_descendants(&projected);
    assert_eq!(visits.len(), 10_000);
    assert_eq!(visits.last().unwrap().depth, 9_999);
    std::mem::forget(visits);
    std::mem::forget(projected);
    std::mem::forget(source);
}
