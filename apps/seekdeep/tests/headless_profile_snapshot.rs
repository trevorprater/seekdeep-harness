//! Exact source headless-profile log through the complete compiled profile tree.

#![cfg(not(windows))]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep::profile_boot::{boot_profile, compose_profile_at, framework_profile_catalog};
use seekdeep_acp_snapshot::{
    NormalizeContext, NormalizeOptions, normalize_session_log, scrub_request_headers,
};
use seekdeep_agent::AgentEvent;
use seekdeep_agent_loop::AgentRequestEvent;
use seekdeep_app_boot::BootPrepare;
use seekdeep_cmdline::{CmdlineHost, provide_cmdline};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_core::{
    session::{Session, SessionHeader},
    session_store::SessionStore,
};
use seekdeep_headless::{HeadlessOutput, plugin_with_output};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LLM,
    LlmAdapter, LlmCallConfig, LlmModelReasoningInfo, LlmReasoningEffortInfo, LlmResolvedModelInfo,
    ModelId, ProviderId, ReasoningEffortId, StreamChunk, TokenUsage,
};
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};
use seekdeep_typert_loader::TypertArtifactRegistry;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
    SEEKDEEP_LAUNCH_ENVIRONMENT, create_launch_environment_snapshot,
};
use serde_json::json;

const TASK: &str = "Prove the product headless profile path with one real tool round trip.";
const ANSWER: &str = "CLI tool round trip complete: CLI_TOOL_ROUND_TRIP\n";

#[derive(Debug, Default)]
struct RecordingOutput {
    stdout: Mutex<String>,
    stderr: Mutex<String>,
}

impl HeadlessOutput for RecordingOutput {
    fn write_stdout(&self, text: &str) -> anyhow::Result<()> {
        self.stdout.lock().push_str(text);
        Ok(())
    }

    fn write_stderr(&self, text: &str) -> anyhow::Result<()> {
        self.stderr.lock().push_str(text);
        Ok(())
    }
}

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
                efforts: ["off", "high"]
                    .into_iter()
                    .map(|id| LlmReasoningEffortInfo {
                        id: ReasoningEffortId::new(id),
                        name: if id == "off" { "Off" } else { "High" }.to_owned(),
                        description: None,
                    })
                    .collect(),
                default_effort: Some(ReasoningEffortId::new("high")),
            }),
        })
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let tool_text = options.messages.last().and_then(|message| {
            message.content().iter().find_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(
                    content
                        .iter()
                        .filter_map(|block| {
                            if let ContentBlock::Text { text } = block {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
        });
        let chunks = if let Some(tool_text) = tool_text {
            let reply = format!("CLI tool round trip complete: {}", tool_text.trim());
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                },
                StreamChunk::TextDelta {
                    index: 0,
                    text: reply.clone(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text { text: reply },
                },
                StreamChunk::Usage {
                    usage: TokenUsage {
                        input_tokens: 7,
                        output_tokens: 5,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        reasoning_tokens: Some(1),
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                },
            ]
        } else {
            let arguments = json!({
                "command":"printf CLI_TOOL_ROUND_TRIP",
                "description":"Prove the CLI tool round trip."
            })
            .to_string();
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "tool-call".to_owned(),
                },
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: CallId::new("cli-smoke-call"),
                    name: Some("bash".to_owned()),
                    arguments_delta: arguments.clone(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("cli-smoke-call"),
                        name: "bash".to_owned(),
                        arguments,
                    },
                },
                StreamChunk::Usage {
                    usage: TokenUsage {
                        input_tokens: 11,
                        output_tokens: 3,
                        cache_read_tokens: Some(2),
                        cache_write_tokens: None,
                        reasoning_tokens: None,
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                },
            ]
        };
        AdapterStream::new(stream::iter(chunks.into_iter().map(Ok)))
    }
}

fn cli_mock_plugin() -> Plugin {
    Plugin::new("cli-mock-llm", ["llm"], |context, _| {
        Box::pin(async move {
            let registration = Arc::new(
                context
                    .get(LLM)
                    .unwrap()
                    .register_adapter(&["cli-mock".to_owned()], Arc::new(CliMockAdapter))?,
            );
            context.own(EffectHandle::new("CLI snapshot adapter", move || {
                Box::pin(async move { registration.dispose().await })
            }))?;
            context.events().on_waterfall(
                &context,
                "agent/request",
                |_, args, next| {
                    let step = args
                        .get::<AgentEvent<AgentRequestEvent>>(0)
                        .unwrap()
                        .payload
                        .step;
                    Box::pin(async move {
                        let reply = next.run().await?;
                        let mut config = reply
                            .downcast::<LlmCallConfig>()
                            .map(|config| (*config).clone())
                            .ok_or_else(|| {
                                anyhow::anyhow!("agent/request did not return config")
                            })?;
                        if step == 2 {
                            config.reasoning_effort = Some(ReasoningEffortId::new("off"));
                        }
                        Ok(EventReply::Value(Arc::new(config)))
                    })
                },
                EventOptions {
                    global: true,
                    prepend: false,
                },
            )?;
            Ok(())
        })
    })
}

struct WorkingDirectory(PathBuf);

impl WorkingDirectory {
    fn enter(path: &Path) -> std::io::Result<Self> {
        let previous = std::env::current_dir()?;
        std::env::set_current_dir(path)?;
        Ok(Self(previous))
    }
}

impl Drop for WorkingDirectory {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore fixture process working directory");
    }
}

fn isolated_environment(home: &Path) -> LaunchEnvironmentSnapshot {
    create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([
            (
                "SEEKDEEP_HOME".to_owned(),
                home.to_string_lossy().into_owned(),
            ),
            (
                "SEEKDEEP_AGENTS_HOME".to_owned(),
                home.join("agents").to_string_lossy().into_owned(),
            ),
            (
                "SEEKDEEP_PERMISSION_MODE".to_owned(),
                "danger-full-access".to_owned(),
            ),
            ("SEEKDEEP_TELEMETRY_DISABLED".to_owned(), "1".to_owned()),
            ("SEEKDEEP_TOOLS_MODE".to_owned(), "native".to_owned()),
        ]),
    }])
}

fn prepare_profile(
    environment: LaunchEnvironmentSnapshot,
    exit_code: &Arc<AtomicI32>,
    changed: &Arc<tokio::sync::Notify>,
    headers: &Arc<Mutex<Vec<SessionHeader>>>,
) -> BootPrepare {
    Arc::new({
        let exit_code = exit_code.clone();
        let changed = changed.clone();
        let headers = headers.clone();
        move |context| {
            let environment = environment.clone();
            let exit_code = exit_code.clone();
            let changed = changed.clone();
            let headers = headers.clone();
            Box::pin(async move {
                context.provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(environment))?;
                TypertArtifactRegistry::install(&context)?;
                provide_cmdline(
                    &context,
                    CmdlineHost::new([TASK], move |code| {
                        exit_code.store(code, Ordering::Release);
                        changed.notify_waiters();
                        Ok(())
                    }),
                )?;
                context.events().on_sync(
                    &context,
                    "session/created",
                    move |_, args| {
                        headers
                            .lock()
                            .push(args.get::<Session>(0).unwrap().header().clone());
                        Ok(EventReply::Undefined)
                    },
                    EventOptions {
                        global: true,
                        prepend: false,
                    },
                )?;
                Ok(())
            })
        }
    })
}

// This integration-test executable has one test: its process cwd belongs to the fixture.
#[tokio::test]
async fn complete_profile_matches_source_stdout_and_cold_session_log() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    let home = temporary.path().join("home");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&home)?;
    let workspace = workspace.canonicalize()?;
    let _cwd = WorkingDirectory::enter(&workspace)?;
    let environment = isolated_environment(&home);
    let output = Arc::new(RecordingOutput::default());
    let catalog = framework_profile_catalog(&workspace, &home, &environment)?;
    catalog.register_named("./snapshot-fixtures/cli-mock-llm.ts", cli_mock_plugin())?;
    catalog.register_named(
        "headless-snapshot-output",
        plugin_with_output(output.clone()),
    )?;
    // Explicit homes mirror the source process environment without mutating this
    // test process's environment. Runner IO is captured; the service tree stays mounted.
    let output_overlay = temporary.path().join("output.patch.yml");
    std::fs::write(
        &output_overlay,
        format!(
            concat!(
                "- id: skill-filesystem\n  config: {{ seekdeepHome: {home}, agentsHome: {agents_home} }}\n",
                "- id: headless-runner\n  disabled: true\n",
                "- insert:\n    - id: snapshot-runner\n      name: headless-snapshot-output\n",
                "      inject: [headlessStartup]\n      config:\n        task: !!js ctx.headlessStartup.task\n",
            ),
            home = serde_json::to_string(&home)?,
            agents_home = serde_json::to_string(&home.join("agents"))?
        ),
    )?;
    let plan = compose_profile_at(
        "headless",
        &[
            repository.join("examples/headless-agent/tests/fixtures/headless-profile.cordis.yml"),
            output_overlay,
        ],
        &workspace,
        &home,
        &home.join("profiles/.seekdeep-installation/package.json"),
        &seekdeep::profile_boot::shipped_preset_root(),
        Some("1"),
    )?;
    let exit_code = Arc::new(AtomicI32::new(-1));
    let changed = Arc::new(tokio::sync::Notify::new());
    let headers = Arc::new(Mutex::new(Vec::<SessionHeader>::new()));
    let prepare = prepare_profile(environment, &exit_code, &changed, &headers);
    let application = boot_profile(plan, &catalog, Some(prepare)).await?;
    let completion = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let notified = changed.notified();
            if exit_code.load(Ordering::Acquire) != -1 {
                break;
            }
            notified.await;
        }
    })
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(10), application.dispose()).await;
    completion?;
    cleanup??;
    assert_eq!(&*output.stdout.lock(), ANSWER);
    assert_eq!(&*output.stderr.lock(), "");
    assert_eq!(exit_code.load(Ordering::Acquire), 0);
    let headers = headers.lock().clone();
    assert_eq!(headers.len(), 1);
    let cold_context = Context::new();
    let cold = JsonlSessionPersistence::new(
        SessionStore::install(&cold_context)?,
        JsonlConfig::new(home.join("sessions")),
    )?;
    let raw = cold.read_raw(&headers[0].id, None).await;
    cold_context.root_fiber().dispose().await?;
    let raw = raw?.expect("persisted product-profile log");
    let normalized = scrub_request_headers(&normalize_session_log(
        &raw.content,
        &NormalizeContext {
            session_ids: vec![headers[0].id.to_string()],
            cwd: workspace.to_string_lossy().into_owned(),
            cwd_aliases: Vec::new(),
        },
        NormalizeOptions::default(),
    )?)?;
    let expected =
        std::fs::read_to_string(repository.join(
            "examples/headless-agent/tests/snapshots/headless-profile/session.expected.jsonl",
        ))?;
    for (index, (actual, expected)) in normalized
        .split_inclusive('\n')
        .zip(expected.split_inclusive('\n'))
        .enumerate()
    {
        assert_eq!(actual, expected, "product-profile log line {}", index + 1);
    }
    assert_eq!(
        normalized.split_inclusive('\n').count(),
        expected.split_inclusive('\n').count()
    );
    Ok(())
}
