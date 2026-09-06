//! Optional provider layering and lifetime of the source Agent Loop settings section.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, AgentRegistry};
use seekdeep_agent_loop::AGENT_LOOP;
use seekdeep_cordis::{Context, Fiber, PluginFiber};
use seekdeep_core::session_store::SessionStore;
use seekdeep_llm::LlmRuntime;
use seekdeep_settings::{
    SETTINGS, SettingsDocument, SettingsNamespace, SettingsService, SettingsStorage,
};
use seekdeep_system_prompt::SystemPromptConfig;
use seekdeep_tools::{TOOLS, ToolRuntime, ToolRuntimeConfig};
use serde_json::{Map, Value, json};

#[derive(Default)]
struct MemorySettings(Mutex<SettingsDocument>);

#[async_trait]
impl SettingsStorage for MemorySettings {
    fn writable(&self) -> bool {
        true
    }
    fn document_path(&self) -> Option<&Path> {
        None
    }
    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(self.0.lock().clone())
    }
    async fn persist(
        &self,
        ns: &SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        self.0
            .lock()
            .insert(ns.as_str().to_owned(), Value::Object(section.clone()));
        Ok(())
    }
}

struct Bench {
    context: Context,
    provider: Arc<Fiber>,
    consumer: Arc<PluginFiber>,
}

impl Bench {
    async fn boot() -> Self {
        let context = Context::new();
        let provider = Fiber::active_child("settings-provider");
        SettingsService::install(
            &context.with_fiber(provider.clone()),
            Arc::new(MemorySettings::default()),
        )
        .await
        .unwrap();
        SessionStore::install(&context).unwrap();
        LlmRuntime::install(&context).unwrap();
        context
            .provide(AGENTS, Arc::new(AgentRegistry::new(context.clone())))
            .unwrap();
        let prompt =
            seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).unwrap();
        let tools =
            ToolRuntime::new_with_system_prompt(&context, &prompt, ToolRuntimeConfig::default())
                .unwrap();
        context.provide(TOOLS, tools).unwrap();
        let consumer = context
            .plugin(
                seekdeep_agent_loop::plugin(),
                json!({"maxParallelToolCalls":4,"agents":[]}),
            )
            .unwrap();
        consumer.await_settled().await.unwrap();
        Self {
            context,
            provider,
            consumer,
        }
    }

    fn cap(&self) -> usize {
        self.context
            .get(AGENT_LOOP)
            .unwrap()
            .max_parallel_tool_calls()
    }
    fn settings(&self) -> Arc<SettingsService> {
        self.context.get(SETTINGS).unwrap()
    }
    async fn dispose(self) {
        self.consumer.dispose().await.unwrap();
        self.provider.dispose().await.unwrap();
        self.context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn user_cap_layers_over_composition_and_invalid_writes_leave_it_unchanged() {
    let bench = Bench::boot().await;
    let ns = SettingsNamespace::new("agent-loop");
    assert_eq!(bench.cap(), 4);
    bench
        .settings()
        .update(&ns, json!({"maxParallelToolCalls":1}), None)
        .await
        .unwrap();
    assert_eq!(bench.cap(), 1);
    for invalid in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!(1.000_000_000_000_01),
        json!("soon"),
    ] {
        assert!(
            bench
                .settings()
                .update(&ns, json!({"maxParallelToolCalls":invalid}), None)
                .await
                .is_err()
        );
        assert_eq!(bench.cap(), 1);
    }
    bench.dispose().await;
}

#[tokio::test]
async fn only_runtime_cap_is_exposed_and_consumer_disposal_withdraws_it() {
    let bench = Bench::boot().await;
    let settings = bench.settings();
    let description = settings
        .describe(false)
        .into_iter()
        .find(|row| row.ns.as_str() == "agent-loop")
        .expect("loop namespace");
    assert_eq!(description.value, json!({"maxParallelToolCalls":4}));
    bench.consumer.dispose().await.unwrap();
    assert!(
        settings
            .describe(false)
            .iter()
            .all(|row| row.ns.as_str() != "agent-loop")
    );
    bench.dispose().await;
}

#[tokio::test]
async fn provider_detach_restores_the_composition_cap() {
    let bench = Bench::boot().await;
    bench
        .settings()
        .update(
            &SettingsNamespace::new("agent-loop"),
            json!({"maxParallelToolCalls":2}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(bench.cap(), 2);
    bench.provider.dispose().await.unwrap();
    assert_eq!(bench.cap(), 4);
    bench.dispose().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn tool_groups_snapshot_the_live_cap_without_resizing_an_inflight_group() {
    use seekdeep_agent_loop::{CreateAgentOptions, ToolCall, ToolCallBatch, execute_tool_calls};
    use seekdeep_core::session::SessionId;
    use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
    use seekdeep_tools::{ToolDefinition, ToolOutputDefinition, assert_supported_json_schema};

    let bench = Bench::boot().await;
    let handle = bench
        .context
        .get(AGENT_LOOP)
        .unwrap()
        .create_agent(
            &bench.context,
            CreateAgentOptions::new(SessionId::new("live-cap")),
        )
        .await
        .unwrap();
    let runtime = bench.context.get(TOOLS).unwrap();
    let started = Arc::new(Mutex::new(Vec::new()));
    let (release_a, wait_a) = tokio::sync::oneshot::channel::<()>();
    let (release_c, wait_c) = tokio::sync::oneshot::channel::<()>();
    let waits = Arc::new(Mutex::new(std::collections::HashMap::from([
        ("a", wait_a),
        ("c", wait_c),
    ])));
    for name in ["change", "a", "b", "barrier", "c", "d"] {
        let starts = started.clone();
        let waits = waits.clone();
        let settings = bench.settings();
        let mut definition = ToolDefinition::new(
            name,
            name,
            Map::from_iter([("type".to_owned(), json!("object"))]),
            ToolOutputDefinition::new(
                Arc::new(assert_supported_json_schema(json!({"type":"string"})).unwrap()),
                Arc::new(|_, value| {
                    Ok(vec![ContentBlock::Text {
                        text: value.as_str().unwrap().to_owned(),
                    }])
                }),
            ),
            Arc::new(move |_, _| {
                let starts = starts.clone();
                let wait = waits.lock().remove(name);
                let settings = settings.clone();
                Box::pin(async move {
                    starts.lock().push(name);
                    if name == "change" || name == "a" {
                        settings
                            .update(
                                &SettingsNamespace::new("agent-loop"),
                                json!({"maxParallelToolCalls":if name == "change" {1} else {2}}),
                                None,
                            )
                            .await?;
                    }
                    if let Some(wait) = wait {
                        wait.await?;
                    }
                    Ok(json!(name))
                })
            }),
        );
        if !matches!(name, "change" | "barrier") {
            definition = definition.concurrency_safe(Arc::new(|_| true));
        }
        runtime.register(&bench.context, definition).unwrap();
    }
    let calls = ["change", "a", "b", "barrier", "c", "d"].map(|name| ToolCall {
        id: CallId::new(name),
        name: name.to_owned(),
        arguments: "{}".to_owned(),
    });
    let signal = AbortSignal::default();
    let mut run = Box::pin(execute_tool_calls(
        ToolCallBatch {
            runtime: &runtime,
            session: handle.agent.session(),
            agent: Some(&handle.agent),
            agent_scope: Some(handle.agent.scope_key()),
            turn: 1,
            step: 1,
            tool_calls: &calls,
            signal: &signal,
            max_parallel_tool_calls: 4,
        },
        |_| Ok(()),
    ));
    // Manual polling drains runnable work, so absence assertions need no timing window.
    assert!(futures::poll!(&mut run).is_pending());
    assert_eq!(*started.lock(), ["change", "a"]);
    assert_eq!(bench.cap(), 2);
    release_a.send(()).unwrap();
    assert!(futures::poll!(&mut run).is_pending());
    assert_eq!(*started.lock(), ["change", "a", "b", "barrier", "c", "d"]);
    release_c.send(()).unwrap();
    run.await.unwrap();
    let results = handle
        .agent
        .session()
        .events()
        .into_iter()
        .filter(|event| event.event_type == "tool/result")
        .map(|event| {
            event.data["message"]["source"]["callId"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(results, ["change", "a", "b", "barrier", "c", "d"]);
    bench.dispose().await;
}
