//! Source: `apps/web/tests/shipped-composition.e2e.ts` and
//! `apps/web/tests/scaffold-hermetic.e2e.ts`. Both suites drive the Web scaffold's Host
//! composition without a browser, so they run against the same composition the keyless Web Host
//! boots (`apps/seekdeep/examples/keyless_web_host.rs`), in-process.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use seekdeep::profile_boot::{
    ProfileBootApplication, boot_profile, compose_profile_at, framework_profile_catalog,
    shipped_preset_root,
};
use seekdeep_agent::{AGENTS, AgentOptions, CreateAgentMeta, CreateAgentOptions};
use seekdeep_agent_presets::{AGENT_PRESETS, AgentPresetRegistry};
use seekdeep_app_boot::BootPrepare;
use seekdeep_cmdline::{CmdlineHost, provide_cmdline};
use seekdeep_commands::{COMMANDS, CommandDescriptor, CommandInputDescriptor};
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, ModelId, ProviderId};
use seekdeep_permission_presets::PERMISSION_PRESETS;
use seekdeep_sandbox::{SandboxMode, canonical_path, writable_roots};
use seekdeep_sandbox_policy::{SANDBOX_POLICY, SandboxPolicyRequest};
use seekdeep_skill::{SKILLS, SkillLookupOptions, SkillViewOptions};
use seekdeep_system_prompt::{AssembleContext, SYSTEM_PROMPT};
use seekdeep_tools::{TOOLS, ToolExecutionInput, ToolExecutionResult};
use seekdeep_typert_loader::TypertArtifactRegistry;
use seekdeep_user_approval::{APPROVAL, ApprovalPolicy};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
    SEEKDEEP_LAUNCH_ENVIRONMENT, create_launch_environment_snapshot,
};
use serde_json::json;

const FILE_REFERENCE_PROMPT: &str =
    "apps/web/tests/snapshots/web-runtime-context/file-reference-prompt.expected.md";

const EXPECTED_TOOLS: [&str; 23] = [
    "ask_user_question",
    "bash",
    "create_goal",
    "edit",
    "exit_plan_mode",
    "get_goal",
    "interrupt_agent",
    "job_kill",
    "job_list",
    "job_output",
    "list_agents",
    "ralph",
    "read",
    "read_image",
    "send_message",
    "skill",
    "subagent",
    "subagent_fork",
    "todo_write",
    "update_goal",
    "web_search",
    "workflow",
    "write",
];

const RIPGREP_TOOLS: [&str; 2] = ["glob", "grep"];

/// The scaffold's Host: the shipped Web profile under the same patch set the keyless Host
/// composes (source `launchWebScaffold()` with no replay), pinned to a temporary home.
struct Scaffold {
    application: ProfileBootApplication,
    workspace_cwd: PathBuf,
}

impl Scaffold {
    fn context(&self) -> &seekdeep_cordis::Context {
        self.application.context()
    }

    /// Source: `scaffold.close()`.
    async fn close(self) -> anyhow::Result<()> {
        self.application.dispose().await
    }
}

fn scaffold_overlay(home: &Path, data: &Path) -> anyhow::Result<PathBuf> {
    let overlay = data.join("scaffold.patch.yml");
    std::fs::write(
        &overlay,
        serde_json::to_string_pretty(&json!([
            {"id":"webserver","config":{"host":"127.0.0.1","port":0}},
            {"id":"web-runtime","config":{"printUrl":false,"surfaceContext":true}},
            {"id":"llm-deepseek","disabled":true},
            {"id":"agent-instructions","disabled":true},
            {"id":"session-title-llm","disabled":true},
            {"id":"session-telemetry-otel","disabled":true},
            {"id":"settings","config":{"seekdeepHome":home}},
            {"id":"credentials","config":{"seekdeepHome":home}},
            {"id":"storage-json","config":{"root":data.join("storages")}},
            {"id":"session-persistence-jsonl","config":{"root":data.join("sessions")}},
            {"id":"session-query-sqlite","config":{"path":":memory:","openAt":"first-search"}},
            {"id":"agent-presets","config":{"default":"standard","roots":[{"path":shipped_preset_root(),"trust":"system"}],"includeUserRoot":false}},
            {"id":"skill-filesystem","config":{"seekdeepHome":home,"agentsHome":home.join("agents"),"bundledSkillDir":home.join("bundled-skills")}},
            {"id":"directory-picker","disabled":true},
            {"insert":[
                {"id":"directory-picker-browse","name":"@seekdeep-ai/seekdeep-host-directory-picker-browse"},
                {"id":"ui-directory-picker-browse","name":"@seekdeep-ai/seekdeep-client-ui-directory-picker-browse"}
            ]}
        ]))?,
    )?;
    Ok(overlay)
}

/// The launch environment the Host reads: the scaffold's own homes plus whatever the caller
/// leaves ambient (source: the scaffold pins `DSH_HOME`/`DSH_AGENTS_HOME` for its Host).
fn scaffold_environment(home: &Path, ambient: &[(&str, &Path)]) -> LaunchEnvironmentSnapshot {
    let mut values = BTreeMap::new();
    for (key, path) in ambient {
        values.insert((*key).to_owned(), path.to_string_lossy().into_owned());
    }
    values.insert(
        "SEEKDEEP_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    );
    values.insert(
        "SEEKDEEP_AGENTS_HOME".to_owned(),
        home.join("agents").to_string_lossy().into_owned(),
    );
    // The source scaffold's `skillRootEnvironment` pins every host-level skill root inside
    // the owned world, the bundled root included.
    values.insert(
        "SEEKDEEP_BUNDLED_SKILL_DIR".to_owned(),
        home.join("bundled-skills").to_string_lossy().into_owned(),
    );
    values.insert("SEEKDEEP_TELEMETRY_DISABLED".to_owned(), "1".to_owned());
    create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values,
    }])
}

async fn launch_web_scaffold(root: &Path, ambient: &[(&str, &Path)]) -> anyhow::Result<Scaffold> {
    let home = root.join("home");
    let data = root.join("data");
    let workspace_cwd = root.join("workspace");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_cwd)?;
    let overlay = scaffold_overlay(&home, &data)?;
    let environment = scaffold_environment(&home, ambient);
    let catalog = framework_profile_catalog(&workspace_cwd, &home, &environment)?;
    let plan = compose_profile_at(
        "web",
        &[overlay],
        &workspace_cwd,
        &home,
        &home.join("profiles/.seekdeep-installation/package.json"),
        &shipped_preset_root(),
        Some("1"),
    )?;
    // The keyless Web Host's boot: the launch environment, typert artifacts, and an inert
    // command line are provided before the profile's plugins load.
    let prepare: BootPrepare = Arc::new(move |context| {
        let environment = environment.clone();
        Box::pin(async move {
            context.provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(environment))?;
            TypertArtifactRegistry::install(&context)?;
            provide_cmdline(
                &context,
                CmdlineHost::new(Vec::<String>::new(), |code| {
                    anyhow::bail!("Web scaffold requested unexpected exit {code}")
                }),
            )?;
            Ok(())
        })
    });
    let application = boot_profile(plan, &catalog, Some(prepare)).await?;
    Ok(Scaffold {
        application,
        workspace_cwd: workspace_cwd.canonicalize()?,
    })
}

/// Source: `setup: agentCtx => ctx.agentPresets.mount(agentCtx)`.
fn preset_setup(roster: Arc<AgentPresetRegistry>) -> seekdeep_agent::AgentSetup {
    Arc::new(move |agent_context| {
        let roster = roster.clone();
        Box::pin(async move {
            roster.mount(&agent_context, None).await?;
            Ok(None)
        })
    })
}

fn content_texts(content: &[ContentBlock]) -> Vec<String> {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text
                .as_str()
                .expect("scaffold text content is UTF-8")
                .to_owned(),
            other => panic!("unexpected content block {other:?}"),
        })
        .collect()
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[tokio::test]
async fn assembles_the_shipped_web_catalog_file_reference_guidance_and_confined_access_default()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let scaffold = launch_web_scaffold(temporary.path(), &[]).await?;
    let context = scaffold.context().clone();
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no tools"))?;
    assert!(tools.schemas(None).is_empty());
    let roster = context
        .get(AGENT_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no agent presets"))?;
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no agents"))?;
    let mut options = CreateAgentOptions::new(SessionId::new("shipped-composition"));
    options.setup = Some(preset_setup(roster.clone()));
    let handle = agents.create(options).await?;
    let outcome: anyhow::Result<()> = async {
        let mut names = tools
            .schemas(Some(handle.agent.scope_key()))
            .into_iter()
            .map(|schema| schema.name)
            .collect::<Vec<_>>();
        names.sort();
        let (ripgrep, catalog): (Vec<_>, Vec<_>) = names
            .into_iter()
            .partition(|name| RIPGREP_TOOLS.contains(&name.as_str()));
        assert_eq!(catalog, EXPECTED_TOOLS);
        assert_eq!(ripgrep, RIPGREP_TOOLS);
        let prompt = context
            .get(SYSTEM_PROMPT)
            .ok_or_else(|| anyhow::anyhow!("scaffold has no system prompt"))?;
        let assembled = prompt
            .assemble(AssembleContext {
                scope: Some(handle.agent.scope_key()),
                ..AssembleContext::default()
            })
            .await?;
        let section = assembled
            .sections
            .iter()
            .find(|section| section.name == "ui:deliverable-file-references");
        let expected = std::fs::read_to_string(repository_root().join(FILE_REFERENCE_PROMPT))?;
        assert_eq!(
            section.map(|section| section.text.as_str()),
            Some(expected.trim_end())
        );
        Ok(())
    }
    .await;
    handle.dispose().await?;
    outcome?;

    let sandbox_policy = context
        .get(SANDBOX_POLICY)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no sandbox policy"))?;
    let roots = writable_roots(&sandbox_policy.resolve(SandboxPolicyRequest {
        session: None,
        mode: Some(SandboxMode::WorkspaceWrite),
    })?);
    for expected in [canonical_path("/tmp"), canonical_path(std::env::temp_dir())] {
        assert!(roots.contains(&expected), "{roots:?} lacks {expected:?}");
    }
    assert_eq!(sandbox_policy.default_mode, SandboxMode::WorkspaceWrite);
    let approval = context
        .get(APPROVAL)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no approval"))?;
    assert_eq!(approval.config().policy, ApprovalPolicy::Ask);
    let permission_presets = context
        .get(PERMISSION_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no permission presets"))?;
    assert_eq!(permission_presets.default_preset(), "workspace-write");

    let mut options = CreateAgentOptions::new(SessionId::new("shipped-command-catalog"));
    options.meta = CreateAgentMeta {
        cwd: Some(scaffold.workspace_cwd.to_string_lossy().into_owned()),
        ..CreateAgentMeta::default()
    };
    options.agent_options = AgentOptions {
        provider: Some(ProviderId::new("deepseek-official")),
        model: Some(ModelId::new("deepseek-v4-flash")),
        ..AgentOptions::default()
    };
    let command_handle = agents.create(options).await?;
    let commands = context
        .get(COMMANDS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no commands"))?;
    let listed = commands.list(&command_handle.agent);
    command_handle.dispose().await?;
    assert!(
        listed.contains(&CommandDescriptor {
            name: "feedback".to_owned(),
            description: "record feedback about this session".to_owned(),
            input: Some(CommandInputDescriptor {
                hint: "<text>".to_owned()
            }),
        }),
        "{listed:?}"
    );
    scaffold.close().await?;
    Ok(())
}

#[tokio::test]
async fn lets_a_preset_producer_reach_the_background_job_registry() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let scaffold = launch_web_scaffold(temporary.path(), &[]).await?;
    let context = scaffold.context().clone();
    let roster = context
        .get(AGENT_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no agent presets"))?;
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no agents"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no tools"))?;
    let mut options = CreateAgentOptions::new(SessionId::new("shipped-background-job"));
    options.meta = CreateAgentMeta {
        cwd: Some(scaffold.workspace_cwd.to_string_lossy().into_owned()),
        ..CreateAgentMeta::default()
    };
    options.setup = Some(preset_setup(roster));
    let handle = agents.create(options).await?;
    let execute = |call_id: &str, name: &str, arguments: serde_json::Value| {
        tools.execute(
            ToolExecutionInput::new(
                CallId::new(call_id),
                name,
                arguments,
                AbortSignal::default(),
            )
            .with_agent(handle.agent.clone()),
        )
    };
    let outcome: anyhow::Result<()> = async {
        let started = execute(
            "shipped-bash-background",
            "bash",
            json!({
                "command": "printf SHIPPED_BACKGROUND_OK",
                "description": "shipped background probe",
                "run_in_background": true,
            }),
        )
        .await;
        let ToolExecutionResult::Success(started) = started else {
            anyhow::bail!("background bash failed: {started:?}");
        };
        assert_eq!(
            content_texts(&started.content),
            ["started background job bash-1"]
        );
        let listed = execute("shipped-task-list", "job_list", json!({})).await;
        let ToolExecutionResult::Success(listed) = listed else {
            anyhow::bail!("job_list failed: {listed:?}");
        };
        let listed = content_texts(&listed.content);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].contains("bash-1 [bash]"), "{listed:?}");
        let collected = execute(
            "shipped-task-output",
            "job_output",
            json!({"job_id": "bash-1", "wait": true}),
        )
        .await;
        let ToolExecutionResult::Success(collected) = collected else {
            anyhow::bail!("job_output failed: {collected:?}");
        };
        let collected = content_texts(&collected.content);
        assert_eq!(collected.len(), 1);
        assert!(
            collected[0].contains("SHIPPED_BACKGROUND_OK"),
            "{collected:?}"
        );
        Ok(())
    }
    .await;
    handle.dispose().await?;
    outcome?;
    scaffold.close().await?;
    Ok(())
}

fn write_skill(root: &Path, name: &str) -> anyhow::Result<()> {
    let bundle = root.join(name);
    std::fs::create_dir_all(&bundle)?;
    std::fs::write(
        bundle.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: Must not enter the Web replay scaffold\n---\n\nAmbient host state.\n"
        ),
    )?;
    Ok(())
}

#[tokio::test]
async fn isolates_replay_skill_discovery_from_every_ambient_host_root() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let ambient = temporary.path().join("ambient");
    let seekdeep_home = ambient.join("seekdeep-home");
    let agents_home = ambient.join("agents-home");
    let bundled = ambient.join("bundled");
    write_skill(&seekdeep_home.join("skills"), "ambient-seekdeep")?;
    write_skill(&agents_home.join("skills"), "ambient-agents")?;
    write_skill(&bundled, "ambient-bundled")?;
    // Source: the ambient roots are set on the test process before the scaffold launches; the
    // scaffold pins its own homes for the Host. Here they enter the launch environment the
    // Host reads, and the composition's own pins must win.
    let scaffold = launch_web_scaffold(
        &temporary.path().join("scaffold"),
        &[
            ("SEEKDEEP_HOME", seekdeep_home.as_path()),
            ("SEEKDEEP_AGENTS_HOME", agents_home.as_path()),
            ("SEEKDEEP_BUNDLED_SKILL_DIR", bundled.as_path()),
        ],
    )
    .await?;
    let context = scaffold.context().clone();
    let roster = context
        .get(AGENT_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no agent presets"))?;
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("scaffold has no agents"))?;
    let mut options = CreateAgentOptions::new(SessionId::new("hermetic-skills"));
    options.setup = Some(preset_setup(roster));
    let handle = agents.create(options).await?;
    let outcome: anyhow::Result<()> = async {
        let skills = context
            .get(SKILLS)
            .ok_or_else(|| anyhow::anyhow!("the composition mounts no skill registry"))?;
        let names = skills
            .list(&SkillViewOptions {
                lookup: SkillLookupOptions {
                    cwd: Some(scaffold.workspace_cwd.to_string_lossy().into_owned()),
                    signal: None,
                },
                scope: Some(handle.agent.scope_key()),
            })
            .await?
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>();
        for ambient in ["ambient-seekdeep", "ambient-agents", "ambient-bundled"] {
            assert!(!names.contains(&ambient.to_owned()), "{names:?}");
        }
        Ok(())
    }
    .await;
    handle.dispose().await?;
    outcome?;
    scaffold.close().await?;
    Ok(())
}
