//! Source differential through mounted tools, real Node workers, and session replay.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_code_runtime::{CodeBindingFailure, CodeJsonString, CodeJsonValue};
use seekdeep_code_runtime_worker_thread::WorkerThreadCodeRuntimeConfig;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId, SurfaceOp},
    session_store::SessionStore,
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, Message};
use seekdeep_scope::ScopeKey;
use seekdeep_session_persistence::SessionPersistence;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, JsonlSessionPersistence};
use seekdeep_system_prompt::SystemPromptConfig;
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, ToolExecutionInput, ToolPresentationMode, ToolResult,
    ToolRuntimeConfig, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

const PROGRAMS: &[&str] = &[
    r#"return {ordinary: [true, null, 5], pair: "😀", literal: "\\ud800"};"#,
    r#"return {"\ud800": "\udfff", literal: "\\ud800", pair: "😀"};"#,
    r#"return await tools.echo({payload: {"\ud800": ["\udfff", "😀", "\\ud800"]}});"#,
    r#"return "\ud800";"#,
    r#"return "\udfff x \ud800";"#,
    r#"return await tools.echo({payload: "\ud800"});"#,
    r#"const value = await tools.echo({payload: JSON.parse('{"__proto__":{"\\ud800":"\\udfff"}}')}); return await tools.echo({payload: value});"#,
    r#"console.log("before"); return "";"#,
    r#"let value = "\ud800"; for (let index = 0; index < 14000; index++) value = [value]; return value;"#,
    r#"console.log("\udfff"); throw "\ud800";"#,
    "try { await tools.reject({}); } catch (error) { return {name: error.name, message: error.message, toolName: error.toolName}; }",
    r#"try { await tools.strict({payload: {"\ud800": 1}}); } catch (error) { return {name: error.name, message: error.message, toolName: error.toolName}; }"#,
    "try { await tools.strict_output({}); } catch (error) { return {name: error.name, message: error.message, toolName: error.toolName}; }",
];

#[derive(Serialize)]
struct Invocation {
    name: &'static str,
    arguments: CodeJsonValue,
}

#[derive(Serialize)]
struct ProgramArguments {
    code: CodeJsonString,
    description: &'static str,
}

fn invocations() -> Vec<Invocation> {
    let mut invocations = PROGRAMS
        .iter()
        .map(|program| Invocation {
            name: "run_code",
            arguments: json!({
                "code":program,
                "description":"Preserve exact JSON string code units"
            })
            .into(),
        })
        .collect::<Vec<_>>();
    invocations.push(Invocation {
        name: "echo",
        arguments: CodeJsonValue::parse(
            r#"{"payload":{"\ud800":["\udfff","😀","\\ud800"]}}"#.to_owned(),
        )
        .unwrap(),
    });
    invocations.push(Invocation {
        name: "echo",
        arguments: CodeJsonValue::parse(r#"{"payload":null,"meta":null}"#.to_owned()).unwrap(),
    });
    for arguments in [
        r#"{"code":"return 1;","description":"\ud800"}"#,
        r#"{"code":"return 1;","description":"\ufeff"}"#,
        r#"{"code":42,"description":"Invalid code type"}"#,
        "42",
    ] {
        invocations.push(Invocation {
            name: "run_code",
            arguments: CodeJsonValue::parse(arguments.to_owned()).unwrap(),
        });
    }
    let original_source_programs: Vec<CodeJsonString> =
        serde_json::from_str(include_str!("fixtures/raw-source-programs.json")).unwrap();
    for code in original_source_programs {
        invocations.push(Invocation {
            name: "run_code",
            arguments: CodeJsonValue::from_serialize(&ProgramArguments {
                code,
                description: "Probe original UTF16 program source",
            })
            .unwrap(),
        });
    }
    invocations
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Event {
    #[serde(rename = "type")]
    event_type: String,
    data: CodeJsonValue,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Observation {
    is_error: bool,
    #[serde(
        default,
        deserialize_with = "seekdeep_code_runtime::json::deserialize_optional"
    )]
    value: Option<CodeJsonValue>,
    error_message: Option<CodeJsonString>,
    error_code: Option<String>,
    #[serde(
        default,
        deserialize_with = "seekdeep_code_runtime::json::deserialize_optional"
    )]
    meta: Option<CodeJsonValue>,
    content: Vec<ContentBlock>,
    calls: Vec<CodeJsonValue>,
    events: Vec<Event>,
}

#[derive(Deserialize)]
struct EchoArguments {
    payload: CodeJsonValue,
    #[serde(
        default,
        deserialize_with = "seekdeep_code_runtime::json::deserialize_optional"
    )]
    meta: Option<CodeJsonValue>,
}

#[derive(Serialize)]
struct DurableToolResult<'a> {
    message: &'a Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<&'a CodeJsonValue>,
}

#[derive(Serialize)]
struct EchoMeta<'a> {
    source: &'static str,
    value: &'a CodeJsonValue,
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "seekdeep-tools-lossless-json-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("unique fixture directory");
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pinned_source() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let pin = std::fs::read_to_string(workspace.join("SOURCE_SNAPSHOT")).unwrap();
    let repository = pin
        .lines()
        .find_map(|line| line.strip_prefix("repository="))
        .unwrap();
    let commit = pin
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .unwrap();
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), commit);
    PathBuf::from(repository)
}

fn source(fixture: &Fixture, invocations: &[Invocation]) -> Vec<Observation> {
    let source = pinned_source();
    let request = fixture.0.join("request.json");
    let output = fixture.0.join("source.json");
    std::fs::write(&request, serde_json::to_vec(invocations).unwrap()).unwrap();
    let status = Command::new(source.join("node_modules/.bin/vitest"))
        .args(["run", "--config"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/lossless-code-json.vitest.config.mjs"),
        )
        .env("SEEKDEEP_SOURCE_ORACLE_ROOT", &source)
        .env("SEEKDEEP_TOOLS_JSON_REQUEST", &request)
        .env("SEEKDEEP_TOOLS_JSON_OUTPUT", &output)
        .current_dir(&source)
        .output()
        .expect("source Vitest runtime");
    assert!(
        status.status.success(),
        "source tools oracle failed:\n{}\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    serde_json::from_slice(&std::fs::read(output).unwrap()).expect("lossless source observation")
}

struct Mounted {
    context: Context,
    persistence_context: Context,
    runtime: Arc<seekdeep_tools::ToolRuntime>,
    writer: Arc<JsonlSessionPersistence>,
    persistence_config: JsonlConfig,
    calls: Arc<Mutex<Vec<CodeJsonValue>>>,
}

impl Mounted {
    fn new(fixture: &Fixture) -> Self {
        let context = Context::new();
        let persistence_context = Context::new();
        let sessions = SessionStore::install(&persistence_context).unwrap();
        let persistence_config = JsonlConfig {
            compression: JsonlCompression::None,
            ..JsonlConfig::new(fixture.0.join("sessions"))
        };
        let writer = JsonlSessionPersistence::new(sessions, persistence_config.clone()).unwrap();
        let prompt =
            seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).unwrap();
        let runtime = seekdeep_tools::install(
            &context,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Both,
                ..Default::default()
            },
        )
        .unwrap();
        seekdeep_code_runtime_worker_thread::install(
            &context,
            &WorkerThreadCodeRuntimeConfig {
                compute_ms: Some(10_000.0),
                max_wall_ms: Some(20_000.0),
                max_output_bytes: Some(1_000_000.0),
                max_old_generation_size_mb: Some(512.0),
            },
        )
        .unwrap();
        assert!(context.get(seekdeep_tools::TOOLS).is_some());
        assert!(context.get(seekdeep_code_runtime::CODE_RUNTIME).is_some());
        let calls = Arc::new(Mutex::new(Vec::new()));
        register_echo(&context, &runtime, calls.clone());
        register_rejection_tools(&context, &runtime);
        Self {
            context,
            persistence_context,
            runtime,
            writer,
            persistence_config,
            calls,
        }
    }

    async fn run(&self, index: usize, invocation: Invocation, expected: Observation) {
        self.calls.lock().clear();
        let id = SessionId::new(format!("lossless-{index}"));
        let session = Session::create(&id, None, None).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session.clone(),
            inbox,
            self.context.clone(),
            ScopeKey::new(),
        ));
        let result = self
            .runtime
            .execute(
                ToolExecutionInput::new(
                    CallId::new("lossless-call"),
                    invocation.name,
                    invocation.arguments,
                    AbortSignal::default(),
                )
                .with_agent(agent),
            )
            .await;
        let actual = Observation {
            is_error: result.is_error(),
            value: result.json_value().cloned(),
            error_message: result.error().map(|error| error.message.clone()),
            error_code: result
                .error()
                .and_then(|error| error.info.as_ref())
                .map(|info| info.code.clone()),
            meta: result.meta().cloned(),
            content: result.content().to_vec(),
            calls: self.calls.lock().clone(),
            events: session
                .events()
                .into_iter()
                .map(|event| Event {
                    event_type: event.event_type,
                    data: event.data,
                })
                .collect(),
        };
        assert_eq!(actual, expected, "source invocation {index}");
        self.persist_result(index, &session, &result).await;
    }

    async fn persist_result(
        &self,
        index: usize,
        session: &Session,
        result: &seekdeep_tools::ToolExecutionResult,
    ) {
        let presented = ToolResult {
            content: result.content().to_vec(),
            is_error: result.is_error(),
            meta: result.meta().cloned(),
        };
        let replayed_presentation: ToolResult = CodeJsonValue::from_serialize(&presented)
            .unwrap()
            .deserialize()
            .unwrap();
        assert_eq!(
            replayed_presentation, presented,
            "replayed presentation invocation {index}"
        );
        let message = Message::tool_result(
            &CallId::new("lossless-call"),
            result.content().to_vec(),
            result.is_error(),
        );
        let encoded = serde_json::to_string(&message).unwrap();
        let decoded: Message = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, message, "model message invocation {index}");
        session
            .append_json(
                "tool/result",
                CodeJsonValue::from_serialize(&DurableToolResult {
                    message: &message,
                    meta: result.meta(),
                })
                .unwrap(),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..Default::default()
                },
            )
            .unwrap();
        let events = session.events();
        self.writer.create(session.header()).await.unwrap();
        self.writer.append(session.id(), &events).await.unwrap();
        let reader_context = Context::new();
        let reader_sessions = SessionStore::install(&reader_context).unwrap();
        let reader =
            JsonlSessionPersistence::new(reader_sessions, self.persistence_config.clone()).unwrap();
        let restored = reader.load(session.id()).await.unwrap();
        assert_eq!(restored.events, events, "JSONL events invocation {index}");
        let replayed =
            Session::create(session.id(), Some(restored.events), Some(restored.meta)).unwrap();
        assert_eq!(&replayed.events()[..events.len()], &events);
        assert_eq!(
            replayed.derive_messages(),
            [message],
            "JSONL model replay invocation {index}"
        );
        reader_context.fiber().dispose().await.unwrap();
    }

    async fn dispose(self) {
        self.context.fiber().dispose().await.unwrap();
        self.persistence_context.fiber().dispose().await.unwrap();
    }
}

fn register_echo(
    context: &Context,
    runtime: &Arc<seekdeep_tools::ToolRuntime>,
    calls: Arc<Mutex<Vec<CodeJsonValue>>>,
) {
    let echo = define_tool(DefineToolOptions::new(
        "echo",
        "Return the supplied JSON payload.",
        json!({"payload":{"type":"json","required":true},"meta":{"type":"json"}}),
        DefineToolOutput::new(
            json!({"type":"json"}),
            Arc::new(|_: &EchoArguments, value: &CodeJsonValue| {
                Ok(vec![ContentBlock::text(value.as_raw())])
            }),
        )
        .presentation_meta_lossless(Arc::new(|arguments, value| {
            if let Some(meta) = &arguments.meta {
                return Ok(meta.clone());
            }
            Ok(CodeJsonValue::from_serialize(&EchoMeta {
                source: "echo",
                value,
            })?)
        })),
        Arc::new(move |arguments: EchoArguments, execution| {
            let calls = calls.clone();
            Box::pin(async move {
                calls.lock().push(execution.arguments.clone());
                Ok(arguments.payload)
            })
        }),
    ))
    .unwrap();
    runtime.register(context, echo).unwrap();
}

fn register_rejection_tools(context: &Context, runtime: &Arc<seekdeep_tools::ToolRuntime>) {
    runtime
        .register(
            context,
            define_tool(DefineToolOptions::new(
                "reject",
                "Reject with a string containing an unpaired code unit.",
                json!({}),
                DefineToolOutput::new(
                    json!({"type":"json"}),
                    Arc::new(|_: &CodeJsonValue, value: &CodeJsonValue| {
                        Ok(vec![ContentBlock::text(value.as_raw())])
                    }),
                ),
                Arc::new(|_: CodeJsonValue, _| {
                    Box::pin(async {
                        Err(anyhow::Error::new(CodeBindingFailure {
                            message: CodeJsonString::from_utf16(&[0xd800]),
                        }))
                    })
                }),
            ))
            .unwrap(),
        )
        .unwrap();
    for (name, description, parameters, output, reply) in [
        (
            "strict",
            "Accept an object without additional properties.",
            json!({"payload":{"type":"object","properties":{},"additionalProperties":false,"required":true}}),
            json!({"type":"json"}),
            CodeJsonValue::from(json!({})),
        ),
        (
            "strict_output",
            "Return a value outside the output declaration.",
            json!({}),
            json!({"type":"object","properties":{},"additionalProperties":false}),
            CodeJsonValue::parse(r#"{"\ud800":1}"#.to_owned()).unwrap(),
        ),
    ] {
        runtime
            .register(
                context,
                define_tool(DefineToolOptions::new(
                    name,
                    description,
                    parameters,
                    DefineToolOutput::new(
                        output,
                        Arc::new(|_: &CodeJsonValue, value: &CodeJsonValue| {
                            Ok(vec![ContentBlock::text(value.as_raw())])
                        }),
                    ),
                    Arc::new(move |_: CodeJsonValue, _| {
                        let reply = reply.clone();
                        Box::pin(async move { Ok(reply) })
                    }),
                ))
                .unwrap(),
            )
            .unwrap();
    }
}

#[tokio::test]
async fn mounted_tools_worker_and_session_replay_preserve_source_json_code_units() {
    let fixture = Fixture::new();
    let invocations = invocations();
    let expected = source(&fixture, &invocations);
    assert_eq!(
        expected.len(),
        invocations.len(),
        "source oracle returned every invocation"
    );
    let mounted = Mounted::new(&fixture);
    for (index, (invocation, expected)) in invocations.into_iter().zip(expected).enumerate() {
        mounted.run(index, invocation, expected).await;
    }
    mounted.dispose().await;
}
