//! Registration and real SQLite/JSONL/tool-runtime integration parity.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_core::{
    session::{AppendOptions, SessionEvent, SessionHeader, SessionId, SurfaceOp, SurfaceReplace},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{
    JsonlCompression, JsonlConfig, install as install_persistence,
};
use seekdeep_session_query_sqlite::{SqliteSessionQueryConfig, install as install_query};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_prompt};
use seekdeep_tool_session_query::{Config, apply, config_schema, plugin};
use seekdeep_tools::{
    ToolExecutionInput, ToolRuntime, ToolRuntimeConfig, install as install_tools,
};
use serde_json::{Value, json};

struct Harness {
    context: seekdeep_cordis::Context,
    sessions: Arc<SessionStore>,
    tools: Arc<ToolRuntime>,
    tool_plugin: Arc<seekdeep_cordis::PluginFiber>,
    _temporary: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let temporary = tempfile::tempdir().expect("tempdir");
        let context = seekdeep_cordis::Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let prompt = install_prompt(&context, SystemPromptConfig::default()).expect("prompt");
        let tools = install_tools(&context, &prompt, ToolRuntimeConfig::default()).expect("tools");
        let persistence = install_persistence(
            &context,
            JsonlConfig {
                root: temporary.path().join("sessions"),
                pack_chunks: true,
                compression: JsonlCompression::None,
                write_batch_max_delay_ms: 60_000,
                prepared_session_cache_size: 5,
            },
        )
        .expect("persistence");
        persistence
            .await_settled()
            .await
            .expect("persistence active");
        let query = install_query(
            &context,
            SqliteSessionQueryConfig {
                path: temporary
                    .path()
                    .join("query.db")
                    .to_string_lossy()
                    .into_owned(),
                ..SqliteSessionQueryConfig::default()
            },
        )
        .expect("query");
        query.await_settled().await.expect("query active");
        let tool_plugin = context.plugin(plugin(), json!({})).expect("tool plugin");
        tool_plugin
            .await_settled()
            .await
            .expect("tool plugin active");
        Self {
            context,
            sessions,
            tools,
            tool_plugin,
            _temporary: temporary,
        }
    }

    fn caller(&self, id: &str, cwd: Option<&str>) -> Arc<Agent> {
        let session = self
            .sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    cwd: cwd.map(str::to_owned),
                    created_at: Some(10),
                    ..CreateSessionOptions::default()
                },
            )
            .expect("caller session");
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
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
        agent: &Arc<Agent>,
        name: &str,
        arguments: Value,
    ) -> seekdeep_tools::ToolExecutionResult {
        let mut input = ToolExecutionInput::new(
            CallId::new(format!("{name}-call")),
            name,
            arguments,
            AbortSignal::default(),
        );
        input.agent = Some(agent.clone());
        input.agent_session = Some(agent.session().clone());
        self.tools.execute(input).await
    }
}

#[tokio::test]
async fn plugin_disposal_withdraws_all_five_tools() {
    let harness = Harness::new().await;
    harness.tool_plugin.dispose().await.unwrap();
    for name in [
        "session_search",
        "session_event_search",
        "session_trace",
        "session_event_trace",
        "session_event_read",
    ] {
        assert!(harness.tools.get(name, None).is_none());
    }
}

fn user_event(seq: u64, time: i64, text: &str) -> SessionEvent {
    SessionEvent {
        event_type: "user/message".to_owned(),
        seq,
        time,
        data: json!({
            "id": format!("message-{seq}"),
            "role": "user",
            "source": {"kind": "user"},
            "content": [{"type": "text", "text": text}]
        }),
        source_event_seqs: None,
        surface_op: Some(SurfaceOp::append()),
        ignorable: None,
    }
}

fn text(result: &seekdeep_tools::ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn registers_exact_tool_policy() {
    let harness = Harness::new().await;
    let names = harness
        .tools
        .schemas(None)
        .into_iter()
        .map(|schema| schema.name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "session_event_read",
            "session_event_search",
            "session_event_trace",
            "session_search",
            "session_trace",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    let session_schema = harness
        .tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == "session_search")
        .unwrap();
    assert_eq!(
        session_schema.description,
        "Search prior sessions in the caller workspace and return the strongest matching event from each session."
    );
    assert_eq!(session_schema.parameters["required"], json!(["query"]));
    assert_eq!(
        session_schema.parameters["properties"]["availability"]["items"]["enum"],
        json!(["live", "persisted"])
    );
    for name in ["session_search", "session_event_search"] {
        let definition = harness.tools.get(name, None).expect("search definition");
        assert_eq!(definition.timeout_ms, Some(30_000.0));
        assert!(definition.is_concurrency_safe.is_none());
    }
    for (name, arguments) in [
        ("session_trace", json!({})),
        ("session_event_trace", json!({"seq": 0})),
        ("session_event_read", json!({"seq": 0})),
    ] {
        let definition = harness.tools.get(name, None).expect("read definition");
        assert!(definition.timeout_ms.is_none());
        assert!(definition.is_concurrency_safe.as_ref().unwrap()(&arguments));
    }
}

#[tokio::test]
async fn searches_live_and_persisted_history() {
    let harness = Harness::new().await;
    let persistence = harness
        .context
        .get(SESSION_PERSISTENCE)
        .expect("persistence")
        .persistence();
    let persisted = SessionId::new("persisted");
    persistence
        .create(&SessionHeader {
            version: 0,
            id: persisted.clone(),
            created_at: 1,
            cwd: Some("/work".to_owned()),
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        })
        .await
        .expect("create persisted");
    persistence
        .append(
            &persisted,
            &[user_event(0, 2, "persisted integration needle")],
        )
        .await
        .expect("append persisted");

    let caller = harness.caller("caller", Some("/work"));
    caller
        .session()
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    caller
        .session()
        .append(
            "user/message",
            user_event(0, 0, "live integration needle").data,
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    caller
        .session()
        .append(
            "step/start",
            json!({"turn":1,"step":1}),
            AppendOptions::default(),
        )
        .unwrap();

    let sessions = harness
        .execute(
            &caller,
            "session_search",
            json!({"query":"persisted integration needle"}),
        )
        .await;
    assert!(!sessions.is_error(), "{:?}", sessions.error());
    assert!(text(&sessions).contains("Session persisted"));
    let live = harness
        .execute(
            &caller,
            "session_event_search",
            json!({"query":"live integration needle"}),
        )
        .await;
    assert!(!live.is_error(), "{:?}", live.error());
    assert!(text(&live).contains("seq 1"));
}

#[test]
fn loader_defaults_direct_validation_and_presenters_are_exact() {
    assert_eq!(
        config_schema().resolve(&json!({})).unwrap(),
        json!({"maxSearchResults":100.0,"searchTimeoutMs":30000.0})
    );
    for config in [
        Config {
            max_search_results: Some(0.0),
            search_timeout_ms: None,
        },
        Config {
            max_search_results: Some(9_007_199_254_740_992.0),
            search_timeout_ms: None,
        },
        Config {
            max_search_results: None,
            search_timeout_ms: Some(0.0),
        },
        Config {
            max_search_results: None,
            search_timeout_ms: Some(2_147_483_648.0),
        },
    ] {
        let error = apply(&seekdeep_cordis::Context::new(), &config).unwrap_err();
        assert!(error.to_string().starts_with("tool-session-query:"));
    }
}

#[tokio::test]
async fn fractional_and_pre_epoch_time_bounds_reach_real_sqlite_comparisons() {
    let harness = Harness::new().await;
    let persistence = harness
        .context
        .get(SESSION_PERSISTENCE)
        .unwrap()
        .persistence();
    let id = SessionId::new("fractional");
    persistence
        .create(&SessionHeader {
            version: 0,
            id: id.clone(),
            created_at: 1,
            cwd: Some("/work".to_owned()),
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        })
        .await
        .unwrap();
    persistence
        .append(
            &id,
            &[
                user_event(0, 1_784_851_200_123, "fractional needle"),
                user_event(1, 1_784_851_200_124, "fractional needle"),
                user_event(2, -124, "pre epoch needle"),
                user_event(3, -123, "pre epoch needle"),
            ],
        )
        .await
        .unwrap();
    let caller = harness.caller("fractional-caller", Some("/work"));
    for (arguments, present, absent) in [
        (
            json!({"session_id":id,"query":"fractional needle","time_from":"2026-07-24T00:00:00.12300001Z"}),
            "seq 1",
            "seq 0",
        ),
        (
            json!({"session_id":id,"query":"fractional needle","time_to":"2026-07-24T08:00:00.1239999+08:00"}),
            "seq 0",
            "seq 1",
        ),
        (
            json!({"session_id":id,"query":"pre epoch needle","time_from":"1969-12-31T23:59:59.87600001Z"}),
            "seq 3",
            "seq 2",
        ),
        (
            json!({"session_id":id,"query":"pre epoch needle","time_to":"1969-12-31T19:59:59.8769999-04:00"}),
            "seq 2",
            "seq 3",
        ),
    ] {
        let result = harness
            .execute(&caller, "session_event_search", arguments)
            .await;
        assert!(!result.is_error(), "{:?}", result.error());
        let output = text(&result);
        assert!(output.contains(present), "{output}");
        assert!(!output.contains(absent), "{output}");
    }
}

#[tokio::test]
async fn traces_event_relationships_and_reads_unabridged_neighbor_windows() {
    let harness = Harness::new().await;
    let caller = harness.caller("trace-caller", Some("/work"));
    caller
        .session()
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    caller
        .session()
        .append(
            "user/message",
            json!({"id":"source","role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"source text"}]}),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    caller
        .session()
        .append(
            "assistant/message",
            json!({
                "turn":1,
                "step":1,
                "message":{"id":"replacement","role":"assistant","source":{"kind":"model","provider":"mock","model":"mock"},"content":[{"type":"text","text":"replacement text"}]}
            }),
            AppendOptions {
                source_event_seqs: Some(vec![1]),
                surface_op: Some(SurfaceOp::Replace(SurfaceReplace {
                    op: "replace".to_owned(),
                    start: 1,
                    end: 1,
                })),
                ..AppendOptions::default()
            },
        )
        .unwrap();

    let trace = harness
        .execute(&caller, "session_event_trace", json!({"seq":1}))
        .await;
    assert!(!trace.is_error(), "{:?}", trace.error());
    let trace_text = text(&trace);
    assert!(trace_text.contains("Replaced by: 2"), "{trace_text}");
    assert!(
        trace_text.contains("Direct derived events: 2"),
        "{trace_text}"
    );

    let read = harness
        .execute(
            &caller,
            "session_event_read",
            json!({"seq":1,"before":1,"after":1}),
        )
        .await;
    assert!(!read.is_error(), "{:?}", read.error());
    let read_text = text(&read);
    assert!(read_text.contains("```json"));
    assert!(read_text.contains("source text"));
    assert!(read_text.contains("replacement text"));
    assert!(read_text.contains("Before:"));
    assert!(read_text.contains("After:"));
}
