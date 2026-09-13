//! Whole-Loader settlement acceptance for the one-shot headless runner plugin.

use std::sync::{
    Arc,
    atomic::{AtomicI32, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{AgentRegistry, ModelSelection};
use seekdeep_agent_default_model::{
    AGENT_DEFAULT_MODEL, AgentDefaultModelConfig, install as install_default_model,
};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices};
use seekdeep_cmdline::{CmdlineHost, provide_cmdline};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_headless::{HeadlessOutput, plugin_with_output};
use seekdeep_llm::{
    AdapterStream, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime, ModelId, ProviderId,
    StreamChunk,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_system_prompt::{SystemPromptConfig, install as install_system_prompt};
use seekdeep_tools::{ToolRuntimeConfig, install as install_tools};

#[derive(Debug)]
struct AnswerAdapter {
    requests: AtomicUsize,
    models: Mutex<Vec<ModelId>>,
}

#[async_trait]
impl LlmAdapter for AnswerAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.fetch_add(1, Ordering::AcqRel);
        self.models.lock().push(options.model);
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "LOADER_SETTLED_HEADLESS".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

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

struct RuntimeHarness {
    agents: Arc<AgentRegistry>,
    loop_: AgentLoop,
    adapter: Arc<AnswerAdapter>,
    _settings_root: tempfile::TempDir,
}

async fn install_runtime(
    context: &Context,
    exit_code: &Arc<AtomicI32>,
    exit_notify: &Arc<tokio::sync::Notify>,
) -> anyhow::Result<RuntimeHarness> {
    let settings_root = tempfile::tempdir()?;
    let settings = context.plugin(
        seekdeep_settings_file::plugin(),
        serde_json::json!({
            "path":settings_root.path().join("settings.yaml"),"watch":false,
        }),
    )?;
    settings.await_settled().await?;
    let sessions = seekdeep_core::session_store::SessionStore::install(context)?;
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(context)?;
    let llm = LlmRuntime::install(context)?;
    let adapter = Arc::new(AnswerAdapter {
        requests: AtomicUsize::new(0),
        models: Mutex::new(Vec::new()),
    });
    llm.register_adapter(&["mock".to_owned()], adapter.clone())?;
    let prompt = install_system_prompt(context, SystemPromptConfig::default())?;
    let tools = install_tools(context, &prompt, ToolRuntimeConfig::default())?;
    let loop_ = AgentLoop::new(
        context.clone(),
        sessions,
        (*agents).clone(),
        AgentLoopServices {
            llm,
            system_prompt: prompt,
            tools,
            max_parallel_tool_calls: 10,
        },
    )?;
    agents.set_factory(Arc::new(loop_.clone()))?;
    let defaults = install_default_model(
        context,
        AgentDefaultModelConfig {
            provider: ProviderId::new("mock"),
            model: ModelId::new("model"),
        },
    )?;
    defaults.await_settled().await?;
    provide_cmdline(
        context,
        CmdlineHost::new(std::iter::empty::<String>(), {
            let exit_code = exit_code.clone();
            let exit_notify = exit_notify.clone();
            move |code| {
                exit_code.store(code, Ordering::Release);
                exit_notify.notify_waiters();
                Ok(())
            }
        }),
    )?;
    Ok(RuntimeHarness {
        agents,
        loop_,
        adapter,
        _settings_root: settings_root,
    })
}

#[tokio::test]
async fn runner_waits_for_every_later_sibling_before_creating_the_agent() -> anyhow::Result<()> {
    let context = Context::new();
    let exit_code = Arc::new(AtomicI32::new(-1));
    let exit_notify = Arc::new(tokio::sync::Notify::new());
    let runtime = install_runtime(&context, &exit_code, &exit_notify).await?;

    let output = Arc::new(RecordingOutput::default());
    let catalog = PluginCatalog::new();
    catalog.register_named("headless", plugin_with_output(output.clone()))?;
    let blocker_started = Arc::new(tokio::sync::Notify::new());
    let blocker_release = Arc::new(tokio::sync::Notify::new());
    catalog.register_named(
        "later-sibling",
        Plugin::new("later-sibling", std::iter::empty::<&str>(), {
            let blocker_started = blocker_started.clone();
            let blocker_release = blocker_release.clone();
            move |_, _| {
                let blocker_started = blocker_started.clone();
                let blocker_release = blocker_release.clone();
                Box::pin(async move {
                    blocker_started.notify_one();
                    blocker_release.notified().await;
                    Ok(())
                })
            }
        }),
    )?;

    let loading = tokio::spawn({
        let catalog = catalog.clone();
        let context = context.clone();
        async move {
            catalog
                .load_yaml(
                    &context,
                    concat!(
                        "- id: headless\n",
                        "  name: headless\n",
                        "  config:\n",
                        "    task: prove settlement\n",
                        "- id: later\n",
                        "  name: later-sibling\n",
                    ),
                )
                .await
        }
    });
    blocker_started.notified().await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let ready = context.registry().values().iter().any(|runtime| {
                runtime.name == seekdeep_headless::NAME
                    && runtime
                        .fibers
                        .iter()
                        .any(|fiber| fiber.fiber().state() == seekdeep_cordis::FiberState::Active)
            });
            if ready {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(runtime.adapter.requests.load(Ordering::Acquire), 0);
    assert!(output.stdout.lock().is_empty());
    assert_eq!(exit_code.load(Ordering::Acquire), -1);

    context
        .get(AGENT_DEFAULT_MODEL)
        .unwrap()
        .save_selection(&ModelSelection {
            provider: ProviderId::new("mock"),
            model: ModelId::new("settled-model"),
            reasoning_effort: None,
        })
        .await?;

    blocker_release.notify_one();
    let composition = loading.await??;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notified = exit_notify.notified();
            if exit_code.load(Ordering::Acquire) != -1 {
                return;
            }
            notified.await;
        }
    })
    .await?;
    assert_eq!(runtime.adapter.requests.load(Ordering::Acquire), 1);
    assert_eq!(
        &*runtime.adapter.models.lock(),
        &[ModelId::new("settled-model")]
    );
    assert_eq!(&*output.stdout.lock(), "LOADER_SETTLED_HEADLESS\n");
    assert!(output.stderr.lock().is_empty());
    assert_eq!(exit_code.load(Ordering::Acquire), 0);

    composition.dispose().await?;
    runtime.loop_.dispose().await?;
    runtime.agents.dispose_initiators().await;
    context.fiber().dispose().await
}

#[test]
fn plugin_metadata_and_config_schema_match_the_source_contract() {
    let plugin = seekdeep_headless::plugin();
    assert_eq!(plugin.name(), "headless-runner");
    assert_eq!(plugin.inject(), ["agentDefaultModel", "agents", "sessions"]);
    assert!(
        seekdeep_headless::config_schema()
            .resolve(&serde_json::json!({}))
            .is_err()
    );
    assert!(
        seekdeep_headless::config_schema()
            .resolve(&serde_json::json!({"task": ""}))
            .is_ok()
    );
}
