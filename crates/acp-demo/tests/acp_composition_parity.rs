//! ACP app forwarding, optional-stack, and plugin-shape composition parity.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use seekdeep_acp::AcpRuntime;
use seekdeep_acp_demo::{Config, apply_with_runtime, plugin};
use seekdeep_cordis::Context;
use seekdeep_jobs::{JobHooks, JobOutcome, JobStart, JobTerminalStatus};
use seekdeep_shell::{
    CollectedOutput, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcess,
    ShellProcessHandle, ShellProcessRead, ShellProcessStatus, ShellRunResult, ShellService,
};
use seekdeep_skill::{SkillLookupOptions, SkillViewOptions};
use seekdeep_system_prompt::AssembleContext;
use seekdeep_tools::{ToolDefinition, ToolOutputDefinition, assert_supported_json_schema};
use serde_json::{Value, json};

fn config(root: &std::path::Path) -> Config {
    serde_json::from_value(json!({
        "provider":"mock",
        "model":"mock",
        "persistenceRoot":root,
        "persistenceCompression":"none",
        "workspaceContext":false,
        "skills":{"enabled":false},
        "toolBash":false,
        "toolJobs":false,
        "goals":false
    }))
    .unwrap()
}

struct Mounted {
    context: Context,
    runtime: Arc<seekdeep_acp_demo::AcpDemoRuntime>,
    _peer: tokio::io::DuplexStream,
}

impl Mounted {
    async fn open(config: Config, shell: bool) -> Self {
        let context = Context::new();
        if shell {
            let executor: Arc<dyn ShellExecutor> = Arc::new(NoopShell);
            ShellService::new(executor).provide(&context).unwrap();
        }
        let (server, peer) = tokio::io::duplex(128 * 1024);
        let (input, output) = tokio::io::split(server);
        let runtime = apply_with_runtime(
            &context,
            config,
            Some(AcpRuntime {
                input: Box::pin(input),
                output: Box::pin(output),
            }),
        )
        .await
        .unwrap();
        Self {
            context,
            runtime,
            _peer: peer,
        }
    }

    async fn close(self) {
        self.context.fiber().dispose().await.unwrap();
    }
}

#[derive(Debug)]
struct SettledProcess;

#[async_trait]
impl ShellProcess for SettledProcess {
    fn status(&self) -> ShellProcessStatus {
        ShellProcessStatus::Completed
    }

    fn exit_code(&self) -> Option<i32> {
        Some(0)
    }

    fn signal(&self) -> Option<seekdeep_shell::ProcessSignal> {
        None
    }

    fn sandbox(&self) -> Option<seekdeep_shell::ShellSandboxInfo> {
        None
    }

    async fn done(&self) {}

    fn read_output(&self) -> ShellProcessRead {
        ShellProcessRead::default()
    }

    fn kill(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct NoopShell;

#[async_trait]
impl ShellExecutor for NoopShell {
    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request.workdir.unwrap_or_else(|| PathBuf::from("/")),
            timeout_ms: request.timeout_ms.unwrap_or(1_000.0),
            stdout_max_bytes: request.stdout_max_bytes.unwrap_or(64_000.0),
            signal: request.signal,
            stdin: request.stdin,
            env: request.env,
            seekdeep_env: request.seekdeep_env,
            sandbox_policy: request.sandbox_policy,
        })
    }

    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        Ok(ShellRunResult {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: spec.timeout_ms,
            stdout: CollectedOutput::default(),
            stderr: CollectedOutput::default(),
            sandbox: None,
        })
    }

    fn start(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        Ok(Arc::new(SettledProcess))
    }
}

#[derive(Debug)]
struct PendingState {
    cancelled: AtomicBool,
    changed: tokio::sync::Notify,
}

struct PendingHooks(Arc<PendingState>);

impl JobHooks for PendingHooks {
    fn cancel(&self, _reason: Option<&str>) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }

    fn done(&self) -> BoxFuture<'static, anyhow::Result<JobOutcome>> {
        let state = self.0.clone();
        Box::pin(async move {
            loop {
                if state.cancelled.load(Ordering::Acquire) {
                    return Ok(JobOutcome {
                        status: JobTerminalStatus::Killed,
                        detail: None,
                        output: None,
                    });
                }
                state.changed.notified().await;
            }
        })
    }
}

fn pending(label: &str) -> JobStart {
    let state = Arc::new(PendingState {
        cancelled: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
    });
    JobStart {
        kind: "probe".to_owned(),
        label: label.to_owned(),
        output_limit_bytes: None,
        owner: None,
        run: Box::new(move || Box::new(PendingHooks(state))),
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> &str {
    payload.downcast_ref::<String>().map_or_else(
        || payload.downcast_ref::<&str>().copied().unwrap_or(""),
        String::as_str,
    )
}

async fn wait_for_tool(runtime: &seekdeep_acp_demo::AcpDemoRuntime, name: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.spine.tools.get(name, None).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn explicit_goal_opt_out_and_default_persistence_config_are_exact() {
    let root = tempfile::tempdir().unwrap();
    let mounted = Mounted::open(config(root.path()), false).await;
    assert!(mounted.context.get(seekdeep_goal::GOAL).is_none());
    assert!(mounted.runtime.spine.tools.get("get_goal", None).is_none());
    mounted.close().await;

    let defaulted: Config = serde_json::from_value(json!({
        "provider":"mock",
        "model":"mock",
        "workspaceContext":false
    }))
    .unwrap();
    assert_eq!(defaulted.persistence_root, "./.sessions");
    assert_eq!(defaulted.persona, None);
}

#[tokio::test]
async fn default_and_explicit_skill_configs_reach_the_spine() {
    let root = tempfile::tempdir().unwrap();
    let mut defaulted = config(root.path());
    defaulted.skills = None;
    defaulted.seekdeep_home = Some(root.path().join(".seekdeep").to_string_lossy().into_owned());
    let mounted = Mounted::open(defaulted, false).await;
    assert!(mounted.context.get(seekdeep_skill::SKILLS).is_some());
    assert!(mounted.runtime.spine.tools.get("skill", None).is_some());
    mounted.close().await;

    let root = tempfile::tempdir().unwrap();
    let custom = root.path().join("custom");
    std::fs::create_dir(&custom).unwrap();
    std::fs::write(
        custom.join("acp-skill.md"),
        "---\nname: acp-skill\ndescription: ACP skill\n---\n\nbody\n",
    )
    .unwrap();
    let mut configured = config(root.path());
    configured.seekdeep_home = Some(root.path().join(".seekdeep").to_string_lossy().into_owned());
    configured.skills = Some(
        serde_json::from_value(json!({
            "filesystem":{
                "seekdeepHome":root.path().join(".seekdeep"),
                "agentsHome":root.path().join(".agents"),
                "customSkillDirs":[custom],
                "includeDefaultRoots":false,
                "watch":false
            },
            "tool":{"catalogDescriptionMaxLength":6}
        }))
        .unwrap(),
    );
    let mounted = Mounted::open(configured, false).await;
    let skills = mounted
        .context
        .get(seekdeep_skill::SKILLS)
        .unwrap()
        .list(&SkillViewOptions {
            lookup: SkillLookupOptions {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                signal: None,
            },
            scope: None,
        })
        .await
        .unwrap();
    assert_eq!(
        skills
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>(),
        ["acp-skill"]
    );
    mounted.close().await;
}

#[tokio::test]
async fn max_parallelism_and_job_admission_are_forwarded() {
    let root = tempfile::tempdir().unwrap();
    let mut configured = config(root.path());
    configured.max_parallel_tool_calls = Some(3);
    configured.jobs = Some(seekdeep_jobs_local::Config {
        max_concurrent_jobs_per_owner: Some(1),
    });
    configured.tool_jobs = None;
    let mounted = Mounted::open(configured, false).await;
    wait_for_tool(&mounted.runtime, "job_output").await;
    assert_eq!(
        mounted.runtime.spine.agent_loop.max_parallel_tool_calls(),
        3
    );
    let jobs = mounted.context.get(seekdeep_jobs::JOBS).unwrap();
    let first = jobs.start(pending("hold configured slot"));
    let rejected = catch_unwind(AssertUnwindSafe(|| {
        jobs.start(pending("blocked configured task"));
    }))
    .unwrap_err();
    assert!(panic_text(rejected.as_ref()).contains("limit: 1"));
    jobs.kill(&first, None, Some("test complete")).unwrap();
    mounted.close().await;
}

#[tokio::test]
async fn tool_configs_and_tool_order_cross_the_acp_boundary() {
    let root = tempfile::tempdir().unwrap();
    let mut configured = config(root.path());
    configured.goals = None;
    configured.tool_order = Some(vec![
        "zulu".to_owned(),
        seekdeep_system_prompt::TOOL_ORDER_REST.to_owned(),
    ]);
    configured.tool_bash = Some(seekdeep_agent_spine_demo::OptionalFeature::Config(
        seekdeep_tool_bash::Config {
            enable_run_in_background: Some(false),
        },
    ));
    configured.tool_jobs = Some(seekdeep_agent_spine_demo::OptionalFeature::Config(
        seekdeep_tool_jobs::Config {
            wait_timeout_ms: Some(7.0),
            max_wait_timeout_ms: Some(11.0),
            ..seekdeep_tool_jobs::Config::default()
        },
    ));
    let mounted = Mounted::open(configured, true).await;
    wait_for_tool(&mounted.runtime, "bash").await;
    let bash = mounted
        .runtime
        .spine
        .tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == "bash")
        .unwrap();
    assert!(
        !bash
            .parameters
            .get("properties")
            .and_then(Value::as_object)
            .unwrap()
            .contains_key("run_in_background")
    );
    for name in ["alpha", "zulu"] {
        mounted
            .runtime
            .spine
            .tools
            .register(
                &mounted.context,
                ToolDefinition::new(
                    name,
                    name,
                    serde_json::Map::new(),
                    ToolOutputDefinition::new(
                        Arc::new(assert_supported_json_schema(json!({})).unwrap()),
                        Arc::new(|_, _| Ok(Vec::new())),
                    ),
                    Arc::new(|_, _| Box::pin(async { Ok(Value::Null) })),
                ),
            )
            .unwrap();
    }
    let assembly = mounted
        .context
        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
        .unwrap()
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    assert_eq!(
        &assembly
            .tools
            .iter()
            .map(|schema| schema.name.as_str())
            .collect::<Vec<_>>()[..2],
        ["zulu", "alpha"]
    );
    for name in ["create_goal", "get_goal", "update_goal", "job_output"] {
        assert!(assembly.tools.iter().any(|schema| schema.name == name));
    }
    mounted.close().await;
}

#[test]
fn plugin_shape_has_no_parent_dependency_and_config_fails_closed() {
    let definition = plugin();
    assert_eq!(definition.name(), "acp-demo");
    assert!(definition.inject().is_empty());
    assert!(serde_json::from_value::<Config>(json!({})).is_err());
    assert!(serde_json::from_value::<Config>(json!({"provider":"p","model":"m"})).is_err());
    let serialized = serde_json::to_value(config(std::path::Path::new("/tmp/acp"))).unwrap();
    assert_eq!(serialized["workspaceContext"], false);
    assert_eq!(serialized["persistenceCompression"], "none");
}
