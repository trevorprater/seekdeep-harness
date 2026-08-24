//! Runtime-harvested model-facing tool-schema catalog.

use std::{collections::BTreeMap, path::Path, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionId},
    session_store::SessionStore,
};
use seekdeep_llm::ToolSchema;
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_subagent::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest,
    SubagentCapabilities, SubagentProvider, SubagentRun, SubagentRuntime,
};
use seekdeep_tools::{ToolPresentationMode, ToolRuntimeConfig};
use seekdeep_workflow::{WorkflowEngine, WorkflowEngineService, WorkflowRun, WorkflowStartRequest};

type Mount = fn(Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>>;

/// One hand-maintained package boot recipe and its rendered deployment metadata.
pub struct ToolPackage {
    /// Product-facing package identity.
    pub package: &'static str,
    /// Pinned-source package leaf checked by the completeness scan.
    pub source_dir: &'static str,
    /// Shared Rust implementation source when every tool has one owner.
    pub source: &'static str,
    /// Per-tool source overrides for split implementations.
    pub source_overrides: &'static [(&'static str, &'static str)],
    /// Runtime services needed by the package.
    pub requires: &'static [&'static str],
    /// Durable or process-local effects produced by calls.
    pub writes: &'static [&'static str],
    /// Additional names used by shipped composition.
    pub shipped_names: &'static [&'static str],
    /// Deployment fact that schema harvest alone cannot express.
    pub note: Option<&'static str>,
    /// Tool-registry presentation mode used while harvesting this package.
    mode: ToolPresentationMode,
    /// Package-specific runtime boot recipe.
    mount: Mount,
}

impl ToolPackage {
    fn source_for(&self, tool_name: &str) -> anyhow::Result<&'static str> {
        self.source_overrides
            .iter()
            .find_map(|(name, source)| (*name == tool_name).then_some(*source))
            .or_else(|| (!self.source.is_empty()).then_some(self.source))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "tool-catalog: {} has no source mapping for harvested tool {tool_name}",
                    self.package
                )
            })
    }
}

/// One harvested package and the exact schemas registered by its boot.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogPackage {
    /// Product-facing package identity.
    pub package: String,
    /// Source attribution by model-visible tool name.
    pub sources: BTreeMap<String, String>,
    /// Runtime services needed by the package.
    pub requires: Vec<String>,
    /// Durable or process-local effects produced by calls.
    pub writes: Vec<String>,
    /// Additional names used by shipped composition.
    pub shipped_names: Vec<String>,
    /// Exact model-facing schemas.
    pub schemas: Vec<ToolSchema>,
    /// Deployment fact that schema harvest alone cannot express.
    pub note: Option<String>,
}

/// Complete model-facing catalog in manifest order.
pub type ToolCatalog = Vec<CatalogPackage>;

fn mount_ask_user(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_user_questions::install(&context)?;
        seekdeep_tool_ask_user::apply(&context)?;
        Ok(None)
    })
}

fn mount_nothing(_context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async { Ok(None) })
}

fn mount_plan_mode(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_plan_mode::PlanModeController::install(
            &context,
            &seekdeep_plan_mode::PlanModeConfig {
                section: "Tool catalog schema harvest.".to_owned(),
            },
        )?;
        Ok(None)
    })
}

fn install_subprocess(
    context: &Context,
) -> anyhow::Result<Arc<seekdeep_subprocess_local::LocalSubprocessRuntime>> {
    seekdeep_subprocess_local::LocalSubprocessRuntime::install_runtime(
        context,
        Arc::new(
            seekdeep_subprocess_local::LocalSubprocessRuntime::with_spill_dir(
                "target/xtask/tool-catalog/spill",
            ),
        ),
    )
}

fn mount_bash(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_subprocess(&context)?;
        seekdeep_shell_env::apply(&context, &seekdeep_shell_env::ShellEnvConfig::default())?;
        seekdeep_bash_local::apply(&context, seekdeep_bash_local::Config::default()).await?;
        seekdeep_tool_bash::apply(&context, seekdeep_tool_bash::Config::default())?;
        Ok(None)
    })
}

fn mount_pwsh(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_subprocess(&context)?;
        seekdeep_shell_env::apply(&context, &seekdeep_shell_env::ShellEnvConfig::default())?;
        seekdeep_pwsh_local::apply(&context, seekdeep_pwsh_local::Config::default()).await?;
        seekdeep_tool_pwsh::apply(&context, seekdeep_tool_pwsh::Config::default())?;
        Ok(None)
    })
}

fn mount_cordis(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        let _runner = seekdeep_cordis_host_runner::DynamicCordisRunner::install(&context, 5_000);
        seekdeep_tool_cordis::apply(&context)?;
        Ok(None)
    })
}

fn mount_persistent_bash(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_terminal::TerminalSessionService::install(&context).await?;
        seekdeep_tool_bash_persistent::apply(
            &context,
            seekdeep_tool_bash_persistent::Config::default(),
        )?;
        Ok(None)
    })
}

fn install_filesystem(context: &Context) -> anyhow::Result<()> {
    seekdeep_fs_local::LocalFileSystem::install(context, seekdeep_fs_local::Config::default())?;
    Ok(())
}

fn mount_str_replace(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_filesystem(&context)?;
        seekdeep_tool_str_replace_editor::apply(
            &context,
            &seekdeep_tool_str_replace_editor::Config::default(),
        )?;
        Ok(None)
    })
}

fn mount_fs(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_filesystem(&context)?;
        seekdeep_attachment_local::install(
            &context,
            &seekdeep_attachment_local::LocalAttachmentConfig {
                seekdeep_home: Some("target/xtask/tool-catalog/home".into()),
                ..seekdeep_attachment_local::LocalAttachmentConfig::default()
            },
        )?;
        seekdeep_tool_fs::apply(&context, &seekdeep_tool_fs::Config::default())?;
        Ok(None)
    })
}

fn mount_fs_search(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_subprocess(&context)?;
        seekdeep_tool_fs_search::apply(
            &context,
            &seekdeep_tool_fs_search::Config {
                sample_over_cap_glob_results: Some(true),
                ..seekdeep_tool_fs_search::Config::default()
            },
        )?;
        Ok(None)
    })
}

fn mount_terminal(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_terminal::TerminalSessionService::install(&context).await?;
        seekdeep_tool_terminal::apply(&context, seekdeep_tool_terminal::Config::default())?;
        Ok(None)
    })
}

fn install_agents(context: &Context) -> anyhow::Result<Arc<AgentRegistry>> {
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(context)?;
    Ok(agents)
}

fn mount_goal(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_agents(&context)?;
        seekdeep_goal::GoalService::install(&context, seekdeep_goal::Config::default())?;
        seekdeep_tool_goal::apply(&context, &seekdeep_tool_goal::Config::default())?;
        Ok(None)
    })
}

fn scoped_agent(context: &Context, id: &str) -> anyhow::Result<Arc<Agent>> {
    let scope_key = ScopeKey::new();
    let scope = create_scope(context, scope_key, None)?;
    let session_id = SessionId::new(id);
    let session = Session::create(&session_id, None, None)?;
    let inbox = Arc::new(Inbox::new(
        session.clone(),
        Arc::new(NoopInboxNotifications),
    )?);
    Ok(Arc::new(Agent::new(
        session_id,
        AgentOptions::default(),
        session,
        inbox,
        scope.context,
        scope_key,
    )))
}

fn mount_schedule(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        let agent = scoped_agent(&context, "tool-catalog-schedule")?;
        let scope = agent.scope_key();
        let agent_context = agent.context().clone();
        seekdeep_schedule::register_schedule_tools(&context, &agent_context, agent, || {})?;
        Ok(Some(scope))
    })
}

fn mount_lsp(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        let service = seekdeep_lsp::install(&context)?;
        service.await_settled().await?;
        seekdeep_tool_lsp::apply(&context, &seekdeep_tool_lsp::Config::default())?;
        Ok(None)
    })
}

struct CatalogWorkflowEngine;

impl WorkflowEngine for CatalogWorkflowEngine {
    fn start(&self, _request: WorkflowStartRequest) -> anyhow::Result<Arc<dyn WorkflowRun>> {
        anyhow::bail!("tool-catalog workflow engine cannot start a run")
    }
}

fn install_workflow(context: &Context) -> anyhow::Result<()> {
    WorkflowEngineService::new(Arc::new(CatalogWorkflowEngine)).provide(context)?;
    Ok(())
}

struct CatalogSubagentProvider {
    capabilities: SubagentCapabilities,
}

impl CatalogSubagentProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            capabilities: SubagentCapabilities {
                output_schema: true,
                depth_limit: true,
                tool_filter: true,
                persona: true,
            },
        })
    }
}

#[async_trait]
impl SubagentProvider for CatalogSubagentProvider {
    fn name(&self) -> &'static str {
        "catalog"
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        false
    }

    fn supports_continuable(&self) -> bool {
        true
    }

    async fn start(
        &self,
        _request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        anyhow::bail!("tool-catalog subagent provider cannot start a child")
    }

    async fn prepare_continuable(
        &self,
        _request: ContinuableCreateRequest,
    ) -> anyhow::Result<ContinuableCreateSpec> {
        anyhow::bail!("tool-catalog subagent provider cannot prepare a child")
    }
}

fn install_subagents(context: &Context) -> anyhow::Result<Arc<SubagentRuntime>> {
    let runtime = SubagentRuntime::install(context)?;
    runtime.register_provider(CatalogSubagentProvider::new())?;
    Ok(runtime)
}

fn mount_ralph(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_subagents(&context)?;
        install_workflow(&context)?;
        seekdeep_tool_ralph::apply(
            &context,
            &seekdeep_tool_ralph::Config {
                subagent_provider: "catalog".to_owned(),
                ..seekdeep_tool_ralph::Config::default()
            },
        )?;
        Ok(None)
    })
}

fn mount_skill(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_skill::SkillRegistry::install(&context, &seekdeep_skill::Config::default())?;
        seekdeep_tool_skill::apply(&context, &seekdeep_tool_skill::Config::default())?;
        Ok(None)
    })
}

fn mount_session_query(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        SessionStore::install(&context)?;
        let provider = seekdeep_session_query_sqlite::install(
            &context,
            seekdeep_session_query_sqlite::SqliteSessionQueryConfig {
                path: ":memory:".to_owned(),
                ..seekdeep_session_query_sqlite::SqliteSessionQueryConfig::default()
            },
        )?;
        provider.await_settled().await?;
        seekdeep_tool_session_query::apply(
            &context,
            &seekdeep_tool_session_query::Config::default(),
        )?;
        Ok(None)
    })
}

fn mount_subagent(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_subagents(&context)?;
        seekdeep_tool_subagent::apply(
            &context,
            seekdeep_tool_subagent::Config {
                provider: "catalog".to_owned(),
                tool_name: Some("subagent".to_owned()),
                enable_run_in_background: Some(true),
                background_mode: Some(seekdeep_tool_subagent::BackgroundMode::OneShot),
                agent_options: None,
                persona: None,
                tool_filter: None,
                max_depth: Some(seekdeep_tool_subagent::MaxDepth::Numeric(3)),
            },
        )?;
        Ok(None)
    })
}

fn mount_subagent_control(
    context: Context,
) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_agents(&context)?;
        install_subagents(&context)?;
        seekdeep_tool_subagent_control::install_control(&context)?;
        seekdeep_tool_subagent_control::install_list_agents(&context)?;
        Ok(None)
    })
}

fn mount_subagent_report(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_subagents(&context)?;
        let agent = scoped_agent(&context, "tool-catalog-report")?;
        let scope = agent.scope_key();
        seekdeep_tool_subagent_report::install_report_tool(
            agent.context(),
            &context,
            seekdeep_subagent::SubagentReportDelivery::Wakeup,
        )?;
        Ok(Some(scope))
    })
}

fn mount_jobs(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_jobs_local::LocalJobRegistry::new(
            &context,
            seekdeep_jobs_local::Config::default(),
        )?;
        seekdeep_tool_jobs::apply(&context, &seekdeep_tool_jobs::Config::default())?;
        Ok(None)
    })
}

fn mount_todo(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_tool_todo::apply(
            &context,
            seekdeep_tool_todo::Config {
                allow_parallel_in_progress: true,
            },
        )?;
        Ok(None)
    })
}

fn mount_workflow(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        install_workflow(&context)?;
        seekdeep_tool_workflow::apply(&context, &seekdeep_tool_workflow::Config::default())?;
        Ok(None)
    })
}

fn mount_web(context: Context) -> BoxFuture<'static, anyhow::Result<Option<ScopeKey>>> {
    Box::pin(async move {
        seekdeep_web::WebRuntime::new(&context, &seekdeep_web::WebRuntimeConfig::default())?;
        seekdeep_tool_web::apply(&context, seekdeep_tool_web::Config::default())?;
        Ok(None)
    })
}

macro_rules! package {
    ($package:literal, $dir:literal, $source:literal, $requires:expr, $writes:expr, $note:expr, $mount:expr) => {
        ToolPackage {
            package: $package,
            source_dir: $dir,
            source: $source,
            source_overrides: &[],
            requires: $requires,
            writes: $writes,
            shipped_names: &[],
            note: $note,
            mode: ToolPresentationMode::Native,
            mount: $mount,
        }
    };
}

/// Returns the exhaustive shipped tool-package boot manifest.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn tool_packages() -> Vec<ToolPackage> {
    vec![
        package!(
            "@seekdeep-ai/seekdeep-tool-ask-user",
            "tool-ask-user",
            "crates/tool-ask-user/src/lib.rs",
            &["ctx.tools", "ctx.userQuestions"],
            &[
                "tool/call",
                "tool/result after a UI/provider answers the question"
            ],
            Some(
                "ask_user_question pauses the tool call until the active UI provider returns a human answer."
            ),
            mount_ask_user
        ),
        ToolPackage {
            package: "@seekdeep-ai/seekdeep-tools",
            source_dir: "tools",
            source: "crates/tools/src/runtime.rs",
            source_overrides: &[],
            requires: &[
                "ctx.tools",
                "ctx.codeRuntime (execution time)",
                "ctx.systemPrompt",
            ],
            writes: &[
                "tool/call",
                "one tool/code-dispatch-start + tool/code-dispatch pair per bridged sub-call",
                "tool/result",
            ],
            shipped_names: &[],
            note: Some(
                "Owned by the tool registry as a reserved transport outside filterable capability layers under `mode: code` / `mode: both` (see the Code Mode Agent Note). Under `code` it is the registry's only wire contribution; the other visible capabilities are declared in a generated SDK section in the loaded runtime's language, and a program calls them through bindings scheduled under the native concurrency contract (submission-ordered starts and policy; concurrency-safe bodies overlap up to `maxParallelSubCalls`) that re-enter the complete guarded tool pipeline and link each nested execution to this outer result.",
            ),
            mode: ToolPresentationMode::Code,
            mount: mount_nothing,
        },
        package!(
            "@seekdeep-ai/seekdeep-plan-mode",
            "plan-mode",
            "crates/plan-mode/src/index.rs",
            &[
                "ctx.tools",
                "ctx.systemPrompt",
                "ctx.userQuestions (execution time, opportunistic)"
            ],
            &[
                "tool/call",
                "plan/mode inactive on an approved review",
                "tool/result"
            ],
            Some(
                "exit_plan_mode stays in the model-facing schema while planning is inactive so transitions add no tool-catalog churn on top of the plan-policy change. Its execute path rejects calls outside plan mode; in plan mode it presents the plan over the user-questions seam (approve / keep planning with feedback), and approval logs plan mode inactive at the step boundary."
            ),
            mount_plan_mode
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-bash",
            "tool-bash",
            "crates/tool-bash/src/lib.rs",
            &[
                "ctx.tools",
                "ctx.shell",
                "ctx.systemPrompt",
                "ctx.shellEnv",
                "ctx.jobs at call time for run_in_background"
            ],
            &["tool/call", "tool/result"],
            Some(
                "The bash tool is the model-facing consumer of the bash executor seam. A `run_in_background` run registers with the generic `ctx.jobs` runtime and is collected/stopped through the `job_*` tools from `@seekdeep-ai/seekdeep-tool-jobs`; the `enableRunInBackground` config (default true) removes the parameter entirely when disabled."
            ),
            mount_bash
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-pwsh",
            "tool-pwsh",
            "crates/tool-pwsh/src/lib.rs",
            &[
                "ctx.tools",
                "ctx.shell",
                "ctx.systemPrompt",
                "ctx.shellEnv",
                "ctx.jobs at call time for run_in_background"
            ],
            &["tool/call", "tool/result"],
            Some(
                "The pwsh tool is the PowerShell-dialect consumer of the bash executor seam for Windows compositions (a PowerShell executor such as `@seekdeep-ai/seekdeep-pwsh-local` backs `ctx.shell`); it mirrors the bash tool call-for-call minus sandbox controls — `run_in_background` runs register with the generic `ctx.jobs` runtime and are collected/stopped through the `job_*` tools, and the managed `SEEKDEEP_*` environment comes from `@seekdeep-ai/seekdeep-shell-env`. Each call runs in a fresh process (no persistent PTY session), with native `C:\\...` paths and `$env:NAME` variables."
            ),
            mount_pwsh
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-cordis",
            "tool-cordis",
            "crates/tool-cordis/src/index.rs",
            &["ctx.tools", "ctx.dynamicCordisRunner"],
            &[
                "tool/call",
                "tool/result",
                "process-local dynamic package lifecycle"
            ],
            Some(
                "Not in any shipped tree: this deliberate opt-in lets model-authored Cordis compatibility packages reach the real runtime. The toolset injects `ctx.dynamicCordisRunner` from `@seekdeep-ai/seekdeep-cordis-host-runner`; a composition missing it never activates the tools. The runner owns the definition registry and Rust-owned Host compatibility evaluator. A running package may register additional model-visible tools until it stops, is undefined, or the Host process restarts; a complete changed request header logs those tool-set changes."
            ),
            mount_cordis
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-bash-persistent",
            "tool-bash-persistent",
            "crates/tool-bash-persistent/src/lib.rs",
            &[
                "ctx.tools",
                "ctx.terminals",
                "an owning Agent at execution time"
            ],
            &["tool/call", "PTY shell state", "tool/result"],
            Some(
                "One owner-isolated persistent bash tool; deployment composition supplies the PTY backend and may override the model-facing environment description."
            ),
            mount_persistent_bash
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-str-replace-editor",
            "tool-str-replace-editor",
            "crates/tool-str-replace-editor/src/lib.rs",
            &["ctx.tools", "ctx.fs"],
            &[
                "tool/call",
                "fs/observed after view presence/absence, edit absence, or successful mutation",
                "tool/result"
            ],
            Some(
                "Standalone view/create/unique literal replace/line insert tool over the filesystem seam; it composes with any shell or terminal API."
            ),
            mount_str_replace
        ),
        ToolPackage {
            package: "@seekdeep-ai/seekdeep-tool-fs",
            source_dir: "tool-fs",
            source: "",
            source_overrides: &[
                ("edit", "crates/tool-fs/src/edit.rs"),
                ("read", "crates/tool-fs/src/read.rs"),
                ("read_image", "crates/tool-fs/src/read_image.rs"),
                ("write", "crates/tool-fs/src/write.rs"),
            ],
            requires: &[
                "ctx.tools",
                "ctx.fs",
                "ctx.systemPrompt",
                "ctx.attachments (read_image registration)",
                "ctx.llm + an image-capable route (read_image execution)",
            ],
            writes: &[
                "tool/call",
                "fs/write-intent or fs/edit-intent for mutations",
                "fs/observed after read presence/absence or successful file operation",
                "durable attachment (read_image)",
                "tool/result",
            ],
            shipped_names: &[],
            note: Some(
                "The read-before-write/edit policy is added by `@seekdeep-ai/seekdeep-fs-observation-policy` (an `fs/*` event-gate plugin, no schema change); a deployment that loads these tools is expected to also load it. `read_image` is not registered without `ctx.attachments`; its schema is route-independent, and execution refuses unless the exact routed model declares image input.",
            ),
            mode: ToolPresentationMode::Native,
            mount: mount_fs,
        },
        package!(
            "@seekdeep-ai/seekdeep-tool-fs-search",
            "tool-fs-search",
            "crates/tool-fs-search/src/lib.rs",
            &["ctx.tools", "ctx.subprocess", "ctx.systemPrompt"],
            &["tool/call", "tool/result"],
            Some(
                "glob and grep are unconditional discovery tools that spawn the packaged ripgrep binary (`@vscode/ripgrep`) through ctx.subprocess as ordinary foreground calls (never background jobs) — no host `rg` install and no shell layer. The catalog uses `sampleOverCapGlobResults: true`; deployments must choose that behavior explicitly. Capped results save the complete formatted list through the optional ctx.spillStore backend; returned locators are follow-up-readable/searchable when the backend exposes local paths in co-located deployments."
            ),
            mount_fs_search
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-terminal",
            "tool-terminal",
            "crates/tool-terminal/src/lib.rs",
            &[
                "ctx.tools",
                "ctx.terminals",
                "ctx.systemPrompt",
                "ctx.jobs at call time for run_in_background"
            ],
            &["tool/call", "tool/result"],
            Some(
                "The six terminal tools are opt-in and complement one-shot shell/filesystem tools. `terminal_send(run_in_background: true)` registers with `ctx.jobs`; TUI, named key sequences, BEL, resize, auto-start, and cross-agent sharing are absent from the schema."
            ),
            mount_terminal
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-goal",
            "tool-goal",
            "crates/tool-goal/src/index.rs",
            &[
                "ctx.tools",
                "ctx.agents",
                "ctx.goals",
                "ctx.systemPrompt",
                "a calling Agent in an authorized open turn"
            ],
            &["tool/call", "goal/change for mutations", "tool/result"],
            Some(
                "create, edit, pause, and resume require direct-human root authority; complete and blocked also accept the exact current goal round. The default blocked lower bound is three admitted rounds."
            ),
            mount_goal
        ),
        package!(
            "@seekdeep-ai/seekdeep-schedule",
            "schedule",
            "crates/schedule/src/tools.rs",
            &[
                "ctx.tools",
                "ctx.sessions",
                "Session persistence",
                "a future live root Agent"
            ],
            &[
                "tool/call",
                "schedule/change create or delete",
                "tool/result"
            ],
            Some(
                "Registered only inside live root Agent scopes created after the opt-in Schedule plugin loads. Version 1 accepts after_seconds, explicit absolute at, and bounded fixed-rate every_seconds, and discloses session-local delivery; management reads and mutations require the shared Session persistence barrier."
            ),
            mount_schedule
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-lsp",
            "tool-lsp",
            "crates/tool-lsp/src/lib.rs",
            &["ctx.tools", "ctx.lsp", "ctx.systemPrompt"],
            &["tool/call", "tool/result"],
            Some(
                "The lsp tool keeps provider selection and language-server subprocesses behind ctx.lsp, so its model-visible schema stays stable across providers. Requires a registered provider (e.g. `@seekdeep-ai/seekdeep-lsp-stdio`) at runtime; without one, a query returns the structured `LSP_UNAVAILABLE` error rather than changing the schema."
            ),
            mount_lsp
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-ralph",
            "tool-ralph",
            "crates/tool-ralph/src/index.rs",
            &[
                "ctx.tools",
                "ctx.workflowEngine",
                "ctx.subagents",
                "ctx.systemPrompt",
                "a calling Agent (exec.agent parents every fresh round)"
            ],
            &[
                "tool/call",
                "tool/result",
                "workflow and child session events during execution"
            ],
            Some(
                "A fixed foreground workflow starts one fresh structured child per round; the model selects only the immutable objective and an optional round cap."
            ),
            mount_ralph
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-skill",
            "tool-skill",
            "crates/tool-skill/src/lib.rs",
            &["ctx.tools", "ctx.agents", "ctx.skills"],
            &[
                "tool/call",
                "tool/result",
                "user/message replacement catalogs via agent.inject()"
            ],
            None,
            mount_skill
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-session-query",
            "tool-session-query",
            "crates/tool-session-query/src/lib.rs",
            &[
                "ctx.tools",
                "ctx.systemPrompt",
                "ctx.sessionQuery",
                "a calling Agent for workspace authority"
            ],
            &["tool/call", "tool/result"],
            Some(
                "The five read-only tools hide provider cursors and authorize every result from the immutable calling agent session. The package is opt-in; compositions that need enforced deadlines or bounded inline output also mount the generic timeout or spill policies."
            ),
            mount_session_query
        ),
        ToolPackage {
            package: "@seekdeep-ai/seekdeep-tool-subagent",
            source_dir: "tool-subagent",
            source: "crates/tool-subagent/src/lib.rs",
            source_overrides: &[],
            requires: &["ctx.tools", "ctx.subagents", "ctx.systemPrompt"],
            writes: &[
                "tool/call",
                "tool/result",
                "child session events through the chosen provider",
            ],
            shipped_names: &["subagent", "subagent_fork"],
            note: Some(
                "The registered tool name is the load-time `toolName` config (default `subagent`); the schema above is that default. The shipped compositions load this package once per subagent backend, so the model additionally sees `subagent_fork` bound to the fork backend. Each instance's description, `run_in_background` parameter, and system-prompt policy follow its own `backgroundMode` and `enableRunInBackground`, so the two shipped schemas are not identical: `subagent` is `continuable` and defaults omitted calls to background with automatic settlement delivery, while `subagent_fork` stays `one-shot` and defaults them to foreground — see `packages/bundle/base/cordis.patch.yml` and `examples/acp-agent/cordis.yml`.",
            ),
            mode: ToolPresentationMode::Native,
            mount: mount_subagent,
        },
        ToolPackage {
            package: "@seekdeep-ai/seekdeep-tool-subagent-control",
            source_dir: "tool-subagent-control",
            source: "crates/tool-subagent-control/src/lib.rs",
            source_overrides: &[],
            requires: &["ctx.tools", "ctx.subagents", "ctx.agents"],
            writes: &[
                "tool/call",
                "tool/result",
                "child session events through ctx.subagents",
            ],
            shipped_names: &[],
            note: Some(
                "The globally named control tools over continuable background subagents: provider-bound `tool-subagent` instances register distinct delegation tools, while this package registers `send_message`, `interrupt_agent`, and `list_agents` once. `list_agents` reads durable topology through ctx.subagents and overlays live status from the Agent registry.",
            ),
            mode: ToolPresentationMode::Native,
            mount: mount_subagent_control,
        },
        package!(
            "@seekdeep-ai/seekdeep-tool-subagent-report",
            "tool-subagent-report",
            "crates/tool-subagent-report/src/lib.rs",
            &[
                "ctx.subagents",
                "ctx.systemPrompt",
                "a live continuable in-process child Agent"
            ],
            &[
                "tool/call",
                "tool/result",
                "a user-role message in the direct parent session"
            ],
            Some(
                "Registered per continuable in-process child rather than globally, so this schema is visible only inside such a child and survives its global `toolFilter`. The same contribution installs the child-scoped `tool:report` prompt section, which this catalog does not render. The parent-facing `send_message` tool is installed independently."
            ),
            mount_subagent_report
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-jobs",
            "tool-jobs",
            "crates/tool-jobs/src/index.rs",
            &["ctx.tools", "ctx.jobs", "ctx.systemPrompt"],
            &[
                "tool/call",
                "tool/result",
                "user/message via agent.inject() for background completion notices"
            ],
            Some(
                "The kind-agnostic background-job controller: background bash commands, PTY sends, and subagents are read, listed, and killed through the same three tools. Loading the plugin attaches the controller that arms producers' `ctx.jobs.start()`."
            ),
            mount_jobs
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-todo",
            "tool-todo",
            "crates/tool-todo/src/lib.rs",
            &["ctx.tools", "owning Agent session"],
            &["tool/call", "todo/write", "tool/result"],
            Some(
                "todo_write is session-owned state; UIs render the latest todo/write event as a checklist. `allowParallelInProgress` is required with no default, so the catalog states its choice: `true`, whose description invites several `in_progress` items. A deployment choosing `false` receives the same tool with a description asking for exactly one active task."
            ),
            mount_todo
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-workflow",
            "tool-workflow",
            "crates/tool-workflow/src/index.rs",
            &[
                "ctx.tools",
                "ctx.workflowEngine",
                "ctx.systemPrompt",
                "a calling Agent (exec.agent parents the script children)"
            ],
            &["tool/call", "tool/result"],
            None,
            mount_workflow
        ),
        package!(
            "@seekdeep-ai/seekdeep-tool-web",
            "tool-web",
            "crates/tool-web/src/lib.rs",
            &["ctx.tools", "ctx.web", "ctx.systemPrompt"],
            &["tool/call", "tool/result"],
            Some(
                "web_search and web_fetch keep provider selection behind ctx.web so model-visible schemas stay stable across backend swaps."
            ),
            mount_web
        ),
    ]
}

/// Verifies that every pinned-source `tool-*` package has one boot recipe.
///
/// # Errors
///
/// Returns I/O failures or a diagnostic naming every omitted package.
pub fn assert_manifest_complete(
    source_root: &Path,
    packages: &[ToolPackage],
) -> anyhow::Result<()> {
    let mut on_disk = Vec::new();
    for group in std::fs::read_dir(source_root.join("packages"))? {
        let group = group?;
        if !group.file_type()?.is_dir() {
            continue;
        }
        for package in std::fs::read_dir(group.path())? {
            let package = package?;
            if package.file_type()?.is_dir()
                && let Some(name) = package.file_name().to_str()
                && name.starts_with("tool-")
            {
                on_disk.push(name.to_owned());
            }
        }
    }
    on_disk.sort();
    on_disk.dedup();
    let listed = packages
        .iter()
        .map(|package| package.source_dir)
        .collect::<std::collections::HashSet<_>>();
    let missing = on_disk
        .into_iter()
        .filter(|name| !listed.contains(name.as_str()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "tool-catalog: {} tool package(s) not in the boot manifest: {}. Add each package to xtask/src/tool_catalog.rs so its schema is catalogued.",
        missing.len(),
        missing.join(", ")
    );
    Ok(())
}

/// Rejects a package boot that registered no model-facing tool.
///
/// # Errors
///
/// Returns a diagnostic naming the package and its expected dependencies.
pub fn assert_tools_harvested(entry: &ToolPackage, harvested: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        harvested > 0,
        "tool-catalog: {} booted without registering a single tool. Its boot recipe likely omitted a required service: {}.",
        entry.package,
        entry.requires.join(", ")
    );
    Ok(())
}

/// Boots every manifest entry on an isolated context and harvests exact schemas.
///
/// # Errors
///
/// Returns completeness, boot, source-attribution, harvest, or teardown failures.
pub async fn collect_tool_catalog(
    source_root: &Path,
    packages: &[ToolPackage],
) -> anyhow::Result<ToolCatalog> {
    assert_manifest_complete(source_root, packages)?;
    let mut catalog = Vec::with_capacity(packages.len());
    for entry in packages {
        let context = Context::new();
        let result = async {
            let prompt = seekdeep_system_prompt::install(
                &context,
                seekdeep_system_prompt::SystemPromptConfig::default(),
            )?;
            let tools = seekdeep_tools::install(
                &context,
                &prompt,
                ToolRuntimeConfig {
                    mode: entry.mode,
                    ..ToolRuntimeConfig::default()
                },
            )?;
            let scope = (entry.mount)(context.clone()).await?;
            let mut schemas = tools.schemas(scope);
            schemas.sort_by(|left, right| left.name.cmp(&right.name));
            assert_tools_harvested(entry, schemas.len())?;
            let sources = schemas
                .iter()
                .map(|schema| {
                    Ok((
                        schema.name.clone(),
                        entry.source_for(&schema.name)?.to_owned(),
                    ))
                })
                .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
            Ok(CatalogPackage {
                package: entry.package.to_owned(),
                sources,
                requires: entry.requires.iter().map(ToString::to_string).collect(),
                writes: entry.writes.iter().map(ToString::to_string).collect(),
                shipped_names: entry
                    .shipped_names
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                schemas,
                note: entry.note.map(ToOwned::to_owned),
            })
        }
        .await;
        let cleanup = context.fiber().dispose().await;
        match (result, cleanup) {
            (Ok(package), Ok(())) => catalog.push(package),
            (Err(error), Ok(())) => return Err(error),
            (Ok(_), Err(cleanup)) => return Err(cleanup),
            (Err(error), Err(cleanup)) => {
                return Err(anyhow::anyhow!(
                    "{error:#}; tool-catalog context cleanup failed: {cleanup:#}"
                ));
            }
        }
    }
    Ok(catalog)
}

fn code_list(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_owned()
    } else {
        values
            .iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn table_cell(value: Option<&str>) -> String {
    value.map_or_else(
        || "-".to_owned(),
        |value| value.replace('|', "\\|").replace('\n', "<br>"),
    )
}

fn github_slug(value: &str) -> String {
    let slug: String = value
        .chars()
        .filter_map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                Some(character.to_ascii_lowercase())
            } else if character.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect();
    // Existing subsystem links use these explicit IDs; product renaming does
    // not invalidate published cross-document fragments.
    if slug.starts_with("seekdeep-aiseekdeep") {
        slug.replacen("seekdeep-aiseekdeep", "deepseek-aiseekdeep", 1)
    } else {
        slug
    }
}

/// Renders the deterministic English tool catalog.
///
/// # Errors
///
/// Returns only if a schema cannot be serialized or lacks source attribution.
pub fn render(catalog: &[CatalogPackage]) -> anyhow::Result<String> {
    let mut lines = vec![
        "<!-- Generated by `cargo xtask tool-catalog` — do not edit by hand.".to_owned(),
        "     Run `cargo xtask tool-catalog` to regenerate. -->".to_owned(),
        String::new(),
        "# Tool Schema Catalog".to_owned(),
        String::new(),
        "Every model-facing tool a shipped plugin contributes to `ctx.tools`: the `name`, `description`, and JSON-Schema `parameters` the model receives. It complements the [subsystem pages](subsystems/core.md), which document the types and generated Cordis API.".to_owned(),
        String::new(),
        "This generated file is freshness-checked by `cargo xtask tool-catalog --check`. The Rust generator boots each package on an isolated Cordis `Context` and reads `ToolRuntime::schemas()` because runtime-spread enums, composed descriptions, configured names, and raw JSON Schema cannot be recovered faithfully from source syntax. The completeness scan compares the boot manifest with every `tool-*` package in the pinned source checkout. See the [tool-schema-catalog Agent Note](../.agents/notes/implemented/process/2026-07-02-tool-schema-catalog.md).".to_owned(),
        String::new(),
        "Scope: shipped product tools corresponding to the pinned source's `packages/*/tool-*` inventory, plus the tool registry, plan mode, and Schedule. Each package boots with its deployment default unless a required choice is recorded in its note. Example-only tools are excluded.".to_owned(),
        String::new(),
        "## Tool Package Map".to_owned(),
        String::new(),
        "This table connects model-visible tool names to the package and runtime services behind them. Exact JSON Schemas follow in the package sections.".to_owned(),
        String::new(),
        "| Tool package | Model-visible names | Requires | Writes / affects | Shipped aliases | Deployment note |".to_owned(),
        "| --- | --- | --- | --- | --- | --- |".to_owned(),
    ];
    for entry in catalog {
        lines.push(format!(
            "| `{}` | {} | {} | {} | {} | {} |",
            entry.package,
            code_list(
                &entry
                    .schemas
                    .iter()
                    .map(|schema| schema.name.clone())
                    .collect::<Vec<_>>()
            ),
            code_list(&entry.requires),
            code_list(&entry.writes),
            code_list(&entry.shipped_names),
            table_cell(entry.note.as_deref())
        ));
    }
    lines.push(String::new());
    for entry in catalog {
        lines.extend([
            format!("<a id=\"{}\"></a>", github_slug(&entry.package)),
            String::new(),
            format!("## `{}`", entry.package),
            String::new(),
        ]);
        for schema in &entry.schemas {
            lines.push(format!("### `{}`", schema.name));
            lines.push(String::new());
            if !schema.description.is_empty() {
                lines.push(schema.description.clone());
                lines.push(String::new());
            }
            lines.push("```json".to_owned());
            lines.push(serde_json::to_string_pretty(&schema.parameters)?);
            lines.push("```".to_owned());
            lines.push(String::new());
            let source = entry.sources.get(&schema.name).ok_or_else(|| {
                anyhow::anyhow!(
                    "tool-catalog: {} lacks source attribution for {}",
                    entry.package,
                    schema.name
                )
            })?;
            lines.push(format!("Source: [`{source}`](../{source})"));
            lines.push(String::new());
        }
        if let Some(note) = &entry.note {
            lines.push(note.clone());
            lines.push(String::new());
        }
    }
    Ok(lines.join("\n"))
}

/// Regenerates or freshness-checks `docs/tool-catalog.md`.
///
/// # Errors
///
/// Returns catalog boot failures, stale-output diagnostics, or file I/O errors.
pub async fn run(repo_root: &Path, source_root: &Path, check: bool) -> anyhow::Result<()> {
    let catalog = collect_tool_catalog(source_root, &tool_packages()).await?;
    let content = render(&catalog)?;
    let output = repo_root.join("docs/tool-catalog.md");
    if check {
        let committed = std::fs::read_to_string(&output).ok();
        if committed.as_deref() == Some(content.as_str()) {
            println!("tool-catalog: {} is up to date.", output.display());
            return Ok(());
        }
        let committed_lines = committed
            .as_deref()
            .map_or_else(Vec::new, |value| value.lines().collect());
        let generated_lines = content.lines().collect::<Vec<_>>();
        let difference = (0..committed_lines.len().max(generated_lines.len()))
            .find(|index| committed_lines.get(*index) != generated_lines.get(*index));
        let detail = difference.map_or_else(String::new, |index| {
            format!(
                " first difference at line {}\n  committed: {:?}\n  generated: {:?}",
                index + 1,
                committed_lines.get(index),
                generated_lines.get(index)
            )
        });
        anyhow::bail!(
            "tool-catalog: {} is stale; run `cargo xtask tool-catalog` and commit it.{detail}",
            output.display()
        );
    }
    std::fs::write(&output, content)?;
    println!("tool-catalog: wrote {}.", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn inventory_fixture(packages: &[ToolPackage]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("inventory fixture");
        for package in packages
            .iter()
            .filter(|package| package.source_dir.starts_with("tool-"))
        {
            std::fs::create_dir_all(
                root.path()
                    .join("packages/catalog")
                    .join(package.source_dir),
            )
            .expect("create source package fixture");
        }
        root
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn boots_every_shipped_package_and_harvests_exact_tool_names() {
        let packages = tool_packages();
        let inventory = inventory_fixture(&packages);
        let catalog = collect_tool_catalog(inventory.path(), &packages)
            .await
            .expect("collect catalog");
        let mut names = catalog
            .iter()
            .flat_map(|entry| entry.schemas.iter().map(|schema| schema.name.as_str()))
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "ask_user_question",
                "bash",
                "bash",
                "cordis_define",
                "cordis_inspect_list",
                "cordis_inspect_query",
                "cordis_inspect_self",
                "cordis_run",
                "cordis_stop",
                "cordis_undefine",
                "create_goal",
                "edit",
                "exit_plan_mode",
                "get_goal",
                "glob",
                "grep",
                "interrupt_agent",
                "job_kill",
                "job_list",
                "job_output",
                "list_agents",
                "lsp",
                "pwsh",
                "ralph",
                "read",
                "read_image",
                "report",
                "run_code",
                "schedule_create",
                "schedule_delete",
                "schedule_list",
                "send_message",
                "session_event_read",
                "session_event_search",
                "session_event_trace",
                "session_search",
                "session_trace",
                "skill",
                "str_replace_editor",
                "subagent",
                "terminal_close",
                "terminal_list",
                "terminal_open",
                "terminal_read",
                "terminal_send",
                "terminal_signal",
                "todo_write",
                "update_goal",
                "web_fetch",
                "web_search",
                "workflow",
                "write",
            ]
        );
        assert!(catalog.iter().all(|entry| {
            entry
                .schemas
                .iter()
                .all(|schema| schema.parameters.get("type") == Some(&json!("object")))
        }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn harvest_retains_runtime_values_attribution_and_configured_alias_note() {
        let packages = tool_packages();
        let inventory = inventory_fixture(&packages);
        let catalog = collect_tool_catalog(inventory.path(), &packages)
            .await
            .expect("collect catalog");
        let todo = catalog
            .iter()
            .flat_map(|entry| &entry.schemas)
            .find(|schema| schema.name == "todo_write")
            .expect("todo schema");
        assert_eq!(
            todo.parameters["properties"]["todos"]["items"]["properties"]["status"]["enum"],
            json!(["pending", "in_progress", "completed"])
        );
        let bash = catalog
            .iter()
            .find(|entry| entry.package == "@seekdeep-ai/seekdeep-tool-bash")
            .expect("bash package");
        assert_eq!(
            bash.sources.get("bash").map(String::as_str),
            Some("crates/tool-bash/src/lib.rs")
        );
        let control = catalog
            .iter()
            .find(|entry| entry.package == "@seekdeep-ai/seekdeep-tool-subagent-control")
            .expect("control package");
        assert_eq!(control.sources.len(), 3);
        assert!(
            control
                .sources
                .values()
                .all(|source| source == "crates/tool-subagent-control/src/lib.rs")
        );
        let subagent = catalog
            .iter()
            .find(|entry| entry.package == "@seekdeep-ai/seekdeep-tool-subagent")
            .expect("subagent package");
        assert_eq!(subagent.shipped_names, ["subagent", "subagent_fork"]);
        assert!(
            subagent
                .note
                .as_deref()
                .is_some_and(|note| note.contains("subagent_fork"))
        );
    }

    #[test]
    fn completeness_and_empty_harvest_fail_with_actionable_package_diagnostics() {
        let packages = tool_packages();
        let inventory = inventory_fixture(&packages);
        let error = assert_manifest_complete(inventory.path(), &[])
            .expect_err("empty manifest must reject")
            .to_string();
        assert!(error.contains("not in the boot manifest"));
        assert!(error.contains("tool-bash"));
        let error = assert_tools_harvested(&packages[0], 0)
            .expect_err("empty boot must reject")
            .to_string();
        assert!(error.contains("booted without registering a single tool"));
        assert!(error.contains("ctx.userQuestions"));
    }

    #[test]
    fn render_emits_package_tool_schema_and_rust_source() {
        let catalog = vec![CatalogPackage {
            package: "@seekdeep-ai/seekdeep-tool-demo".to_owned(),
            sources: BTreeMap::from([(
                "demo".to_owned(),
                "crates/tool-demo/src/lib.rs".to_owned(),
            )]),
            requires: vec!["ctx.tools".to_owned()],
            writes: vec!["tool/result".to_owned()],
            shipped_names: Vec::new(),
            schemas: vec![ToolSchema {
                name: "demo".to_owned(),
                description: "A demo tool.".to_owned(),
                parameters: json!({"type":"object","properties":{}})
                    .as_object()
                    .expect("object")
                    .clone(),
            }],
            note: None,
        }];
        let markdown = render(&catalog).expect("render");
        assert!(markdown.contains(
            "| `@seekdeep-ai/seekdeep-tool-demo` | `demo` | `ctx.tools` | `tool/result` |"
        ));
        assert!(markdown.contains("## `@seekdeep-ai/seekdeep-tool-demo`"));
        assert!(markdown.contains("### `demo`"));
        assert!(markdown.contains("A demo tool."));
        assert!(markdown.contains("```json"));
        assert!(
            markdown.contains(
                "Source: [`crates/tool-demo/src/lib.rs`](../crates/tool-demo/src/lib.rs)"
            )
        );
    }
}
