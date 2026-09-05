//! Real Loader/agent/tool/persistence composition over the native OTLP wire.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use async_trait::async_trait;
use futures::stream;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use regex::Regex;
use seekdeep_agent::{AGENTS, AgentEvent};
use seekdeep_agent_loop::AgentRequestEvent;
use seekdeep_app_boot::{BootOptions, boot};
use seekdeep_command_feedback::record_feedback;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LLM,
    LlmAdapter, LlmCallConfig, LlmFailure, LlmModelReasoningInfo, LlmReasoningEffortInfo,
    LlmResolvedModelInfo, ModelId, ProviderId, ReasoningEffortId, StreamChunk, TokenUsage,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_loader_smoke::{FixtureTurnOptions, run_fixture_turn};
use seekdeep_session_telemetry::{
    SESSION_TELEMETRY, SessionTelemetryRecord, SessionTelemetrySharingStatus,
};
use seekdeep_session_telemetry_otel::{DISABLED_FEEDBACK_WARNING, SessionTelemetryMode};
use serde_json::{Value, json};

const SECRET: &str = "sk-e2efixture1234567890";
const PLACEHOLDER: &str = "[E2E-REDACTED]";

#[derive(Debug)]
struct CliMockAdapter;

#[async_trait]
impl LlmAdapter for CliMockAdapter {
    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        Ok(LlmResolvedModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new(model),
            name: model.to_owned(),
            description: None,
            input_modalities: None,
            context: None,
            default_max_tokens: None,
            reasoning: Some(LlmModelReasoningInfo {
                efforts: vec![
                    LlmReasoningEffortInfo {
                        id: ReasoningEffortId::new("off"),
                        name: "Off".to_owned(),
                        description: None,
                    },
                    LlmReasoningEffortInfo {
                        id: ReasoningEffortId::new("high"),
                        name: "High".to_owned(),
                        description: None,
                    },
                ],
                default_effort: Some(ReasoningEffortId::new("high")),
            }),
        })
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        if std::env::var("SEEKDEEP_CLI_MOCK_FAILURE").as_deref() == Ok("1") {
            return AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
                reason: FinishReason::Error {
                    failure: LlmFailure {
                        message: "CLI mock provider failed".to_owned(),
                        code: "SERVER".to_owned(),
                        status: None,
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                },
                replay_state: None,
            })]));
        }
        let tool_text = options.messages.last().and_then(|message| {
            message.content().iter().find_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(
                    content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
        });
        if let Some(tool_text) = tool_text {
            let reply = format!("CLI tool round trip complete: {}", tool_text.trim());
            return AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                }),
                Ok(StreamChunk::TextDelta {
                    index: 0,
                    text: reply.clone(),
                }),
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text { text: reply },
                }),
                Ok(StreamChunk::Usage {
                    usage: TokenUsage {
                        input_tokens: 7,
                        output_tokens: 5,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        reasoning_tokens: Some(1),
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ]));
        }
        let arguments = json!({
            "command": "printf CLI_TOOL_ROUND_TRIP",
            "description": "Prove the CLI tool round trip."
        })
        .to_string();
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::BlockStart {
                index: 0,
                block_type: "tool-call".to_owned(),
            }),
            Ok(StreamChunk::ToolCallDelta {
                index: 0,
                id: CallId::new("cli-smoke-call"),
                name: Some("bash".to_owned()),
                arguments_delta: arguments.clone(),
            }),
            Ok(StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::ToolCall {
                    id: CallId::new("cli-smoke-call"),
                    name: "bash".to_owned(),
                    arguments,
                },
            }),
            Ok(StreamChunk::Usage {
                usage: TokenUsage {
                    input_tokens: 11,
                    output_tokens: 3,
                    cache_read_tokens: Some(2),
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::ToolCalls,
                replay_state: None,
            }),
        ]))
    }
}

fn cli_mock_plugin() -> Plugin {
    Plugin::new("cli-mock-llm", ["llm"], |context, _| {
        Box::pin(async move {
            let llm = context
                .get(LLM)
                .ok_or_else(|| anyhow::anyhow!("cli-mock-llm requires llm"))?;
            let registration =
                Arc::new(llm.register_adapter(&["cli-mock".to_owned()], Arc::new(CliMockAdapter))?);
            let cleanup = Arc::clone(&registration);
            context.own(EffectHandle::new("cli-mock adapter", move || {
                Box::pin(async move { cleanup.dispose().await })
            }))?;
            context.events().on_waterfall(
                &context,
                "agent/request",
                move |_, args, next| {
                    let step = args
                        .get::<AgentEvent<AgentRequestEvent>>(0)
                        .map(|event| event.payload.step);
                    Box::pin(async move {
                        let reply = next.run().await?;
                        let mut config = reply
                            .downcast::<LlmCallConfig>()
                            .map(|config| (*config).clone())
                            .ok_or_else(|| {
                                anyhow::anyhow!("agent/request did not return config")
                            })?;
                        if step == Some(2) {
                            config.reasoning_effort = Some(ReasoningEffortId::new("off"));
                        }
                        Ok(EventReply::Value(Arc::new(config)))
                    })
                },
                EventOptions {
                    global: true,
                    ..EventOptions::default()
                },
            )?;
            Ok(())
        })
    })
}

fn telemetry_redact_plugin() -> Plugin {
    Plugin::new(
        "telemetry-redact-rule",
        std::iter::empty::<&str>(),
        |context, _| {
            Box::pin(async move {
                context.events().on_waterfall(
                    &context,
                    "session-telemetry/record",
                    move |_, _, next| {
                        Box::pin(async move {
                            let reply = next.run().await?;
                            let mut record = reply
                                .downcast::<SessionTelemetryRecord>()
                                .map(|record| (*record).clone())
                                .ok_or_else(|| {
                                    anyhow::anyhow!("telemetry redaction received no record")
                                })?;
                            record.body = scrub(record.body);
                            Ok(EventReply::Value(Arc::new(record)))
                        })
                    },
                    EventOptions {
                        global: true,
                        ..EventOptions::default()
                    },
                )?;
                Ok(())
            })
        },
    )
}

fn scrub(value: Value) -> Value {
    static SECRET_PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"sk-e2efixture[0-9]+").expect("fixture secret regex"));
    match value {
        Value::String(value) => {
            Value::String(SECRET_PATTERN.replace_all(&value, PLACEHOLDER).into_owned())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(scrub).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, scrub(value)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value,
    }
}

#[derive(Debug)]
struct Collector {
    endpoint: String,
    captures: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::AbortHandle,
}

impl Collector {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind collector");
        let address = listener.local_addr().expect("collector address");
        let captures = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captures);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let connection_sink = Arc::clone(&sink);
                tokio::spawn(async move {
                    let service =
                        service_fn(move |request| capture(request, Arc::clone(&connection_sink)));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        })
        .abort_handle();
        Self {
            endpoint: format!("http://{address}/v1/logs"),
            captures,
            task,
        }
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn capture(
    request: Request<Incoming>,
    captures: Arc<Mutex<Vec<Value>>>,
) -> Result<Response<Full<hyper::body::Bytes>>, hyper::Error> {
    let bytes = request.into_body().collect().await?.to_bytes();
    captures
        .lock()
        .push(serde_json::from_slice(&bytes).expect("OTLP JSON request"));
    Ok(Response::new(Full::new(hyper::body::Bytes::from_static(
        b"{}",
    ))))
}

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    for (name, plugin) in [
        ("fixture:cli-mock-llm", cli_mock_plugin()),
        ("fixture:telemetry-redact-rule", telemetry_redact_plugin()),
        (
            "seekdeep-subprocess-local",
            seekdeep_subprocess_local::plugin(),
        ),
        ("seekdeep-bash-local", seekdeep_bash_local::plugin()),
        (
            "seekdeep-session-telemetry-otel",
            seekdeep_session_telemetry_otel::plugin(),
        ),
        (
            "seekdeep-agent-spine-demo",
            seekdeep_agent_spine_demo::plugin(),
        ),
        (
            "seekdeep-session-persistence-jsonl",
            seekdeep_session_persistence_jsonl::plugin(),
        ),
        (
            "seekdeep-session-checkpoint-policy",
            seekdeep_session_checkpoint_policy::plugin(),
        ),
    ] {
        catalog
            .register_named(name, plugin)
            .expect("register plugin");
    }
    catalog
}

fn config_yaml(mode: &str, endpoint: &str, root: &Path) -> String {
    format!(
        concat!(
            "- id: cli-mock-llm\n",
            "  name: fixture:cli-mock-llm\n",
            "- id: telemetry-redact-rule\n",
            "  name: fixture:telemetry-redact-rule\n",
            "- id: subprocess\n",
            "  name: seekdeep-subprocess-local\n",
            "- id: bash\n",
            "  name: seekdeep-bash-local\n",
            "  config:\n",
            "    cwd: {}\n",
            "- id: session-telemetry-otel\n",
            "  name: seekdeep-session-telemetry-otel\n",
            "  config:\n",
            "    mode: {}\n",
            "    exporter:\n",
            "      url: {}\n",
            "    processor:\n",
            "      scheduledDelayMillis: 60000\n",
            "- id: agent-spine\n",
            "  name: seekdeep-agent-spine-demo\n",
            "  config:\n",
            "    agents:\n",
            "      - id: main\n",
            "        provider: cli-mock\n",
            "        model: cli-mock\n",
            "        cwd: {}\n",
            "    persona: Test the session-telemetry-otel plugin.\n",
            "    workspaceContext: false\n",
            "    skills: {{ enabled: false }}\n",
            "    toolBash: {{ enableRunInBackground: false }}\n",
            "    toolJobs: false\n",
            "- id: persistence\n",
            "  name: seekdeep-session-persistence-jsonl\n",
            "  config:\n",
            "    root: {}\n",
            "    compression: none\n",
            "- id: checkpoint-policy\n",
            "  name: seekdeep-session-checkpoint-policy\n",
        ),
        serde_json::to_string(&root.to_string_lossy()).expect("root string"),
        serde_json::to_string(mode).expect("mode string"),
        serde_json::to_string(endpoint).expect("endpoint string"),
        serde_json::to_string(&root.to_string_lossy()).expect("root string"),
        serde_json::to_string(&root.join(".sessions").to_string_lossy())
            .expect("session root string"),
    )
}

#[derive(Debug)]
struct FixtureOutput {
    captures: Vec<Value>,
    log: String,
    sharing: SessionTelemetrySharingStatus,
}

async fn run_composition(mode: &str) -> FixtureOutput {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let root = std::fs::canonicalize(temporary.path()).expect("canonical workspace");
    let collector = Collector::start().await;
    let config_path = root.join("session-telemetry-otel.cordis.yml");
    std::fs::write(&config_path, config_yaml(mode, &collector.endpoint, &root))
        .expect("write config");
    let application = boot(
        "telemetry-otel-e2e",
        &config_path,
        &catalog(),
        BootOptions::default(),
    )
    .await
    .expect("boot composition");
    let context: &Context = application.context();
    run_fixture_turn(
        context,
        FixtureTurnOptions {
            task: format!("prove telemetry with key {SECRET}"),
            on_event: None,
        },
    )
    .await
    .expect("first fixture turn");
    if mode != "FULL" {
        let roots = context.get(AGENTS).expect("agents").roots();
        let [agent] = roots.as_slice() else {
            panic!("fixture requires one root agent");
        };
        record_feedback(agent.session(), "fixture feedback").expect("record feedback");
        if mode == "FEEDBACK_ONLY" {
            run_fixture_turn(
                context,
                FixtureTurnOptions {
                    task: "post-feedback private suffix".to_owned(),
                    on_event: None,
                },
            )
            .await
            .expect("suffix fixture turn");
        }
    }
    let sharing = context
        .get(SESSION_TELEMETRY)
        .expect("telemetry service")
        .sharing();
    application.dispose().await.expect("dispose composition");
    let captures = collector.captures.lock().clone();
    let log_path = jsonl_files(&root.join(".sessions"))
        .into_iter()
        .next()
        .expect("canonical log");
    let log = std::fs::read_to_string(log_path).expect("read canonical log");
    FixtureOutput {
        captures,
        log,
        sharing,
    }
}

fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn all_records(captures: &[Value]) -> Vec<&Value> {
    captures
        .iter()
        .flat_map(|capture| capture["resourceLogs"].as_array().into_iter().flatten())
        .flat_map(|resource| resource["scopeLogs"].as_array().into_iter().flatten())
        .flat_map(|scope| scope["logRecords"].as_array().into_iter().flatten())
        .collect()
}

fn event_types(captures: &[Value]) -> Vec<&str> {
    all_records(captures)
        .into_iter()
        .flat_map(|record| record["attributes"].as_array().into_iter().flatten())
        .filter(|attribute| attribute["key"] == "event.type")
        .filter_map(|attribute| attribute["value"]["stringValue"].as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_mode_exports_the_redacted_real_tool_turn_and_preserves_the_canonical_log() {
    let output = run_composition("FULL").await;
    assert_eq!(output.sharing, SessionTelemetrySharingStatus::Full);
    let types = event_types(&output.captures);
    for expected in [
        "turn/start",
        "user/message",
        "tool/call",
        "tool/result",
        "assistant/message",
        "turn/end",
    ] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    let wire = serde_json::to_string(&output.captures).expect("serialize captures");
    assert!(!wire.contains(SECRET));
    assert!(wire.contains(PLACEHOLDER));
    assert!(wire.contains("prove telemetry with key"));
    assert!(wire.contains("telemetry.op"));
    assert!(output.log.contains(SECRET));
    assert!(!output.log.contains(PLACEHOLDER));
    assert!(output.log.contains("CLI_TOOL_ROUND_TRIP"));
    assert!(output.log.contains(r#""reasoningEffort":"high""#));
    assert!(output.log.contains(r#""reasoningEffort":"off""#));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_only_exports_the_feedback_prefix_but_not_the_private_suffix() {
    let output = run_composition("FEEDBACK_ONLY").await;
    assert_eq!(output.sharing, SessionTelemetrySharingStatus::FeedbackOnly);
    assert!(event_types(&output.captures).contains(&"feedback/record"));
    let wire = serde_json::to_string(&output.captures).expect("serialize captures");
    assert!(wire.contains("fixture feedback"));
    assert!(wire.contains("prove telemetry with key"));
    assert!(!wire.contains("post-feedback private suffix"));
    assert!(output.log.contains("post-feedback private suffix"));
}

#[derive(Clone, Debug, Default)]
struct LogCapture(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for LogCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        if let Some(message) = visitor.message {
            self.0.lock().push(message);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_mode_keeps_feedback_local_and_emits_the_stable_warning() {
    let logs = LogCapture::default();
    tracing::subscriber::set_global_default(logs.clone()).expect("install test tracing subscriber");
    let output = run_composition("DISABLED").await;
    assert_eq!(
        serde_json::to_value(SessionTelemetryMode::Disabled).expect("serialize mode"),
        "DISABLED"
    );
    assert_eq!(output.sharing, SessionTelemetrySharingStatus::Disabled);
    assert!(output.captures.is_empty());
    assert!(output.log.contains("fixture feedback"));
    let rendered = logs.0.lock().join("\n");
    assert!(rendered.contains(DISABLED_FEEDBACK_WARNING), "{rendered}");
}
