//! Shipped CLI profile, shell, preset, badge, and memory-overlay contracts.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use seekdeep::profile_boot::{
    compose_profile_at, framework_profile_catalog, run_profile_process, shipped_preset_root,
};
use seekdeep_agent::{AGENTS, CreateAgentMeta, CreateAgentOptions};
use seekdeep_agent_presets::{AGENT_PRESETS, AgentPresetRegistry};
use seekdeep_app_boot::{compose_entries, init_profile, load_overlay_patches, resolve_profile_dir};
use seekdeep_core::session::SessionId;
use seekdeep_loader::profile_patch::{ProfileEntry, ProfileNode, render_entry_list_yaml};
use seekdeep_session_projection::SESSION_PROJECTIONS;
use seekdeep_system_prompt::{AssembleContext, SYSTEM_PROMPT};
use seekdeep_token_meter::TOKEN_METER;
use seekdeep_tools::{TOOLS, ToolRuntime};
use seekdeep_util::launch_environment::LaunchEnvironmentSnapshot;

fn install_anchor(home: &Path) -> PathBuf {
    home.join("profiles")
        .join(".seekdeep-installation")
        .join("package.json")
}

fn entry<'a>(entries: &'a [ProfileEntry], id: &str) -> &'a ProfileEntry {
    entries
        .iter()
        .find(|entry| entry.id().is_some_and(|candidate| candidate.as_str() == id))
        .unwrap_or_else(|| panic!("missing entry {id}"))
}

fn config_field<'a>(entry: &'a ProfileEntry, key: &str) -> &'a ProfileNode {
    entry
        .config()
        .and_then(ProfileNode::as_mapping)
        .and_then(|config| config.get(key))
        .unwrap_or_else(|| panic!("missing config field {key}"))
}

fn expression(entry: &ProfileEntry) -> &str {
    entry
        .disabled()
        .and_then(ProfileNode::as_javascript)
        .map_or_else(
            || {
                panic!(
                    "platform gate must remain a JavaScript expression, got {:?}",
                    entry.disabled()
                )
            },
            seekdeep_loader::profile_patch::JavaScriptExpression::as_str,
        )
}

fn load_entries(path: &Path) -> anyhow::Result<Vec<ProfileEntry>> {
    Ok(load_overlay_patches("shipped-preset-contract", path)?
        .into_iter()
        .map(|patch| ProfileEntry::from_fields(patch.fields().clone()))
        .collect())
}

fn web_plan(root: &Path) -> anyhow::Result<Vec<ProfileEntry>> {
    let home = root.join("home");
    let cwd = root.join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let plan = compose_profile_at(
        "web",
        &[],
        &cwd,
        &home,
        &install_anchor(&home),
        &shipped_preset_root(),
        None,
    )?;
    assert!(plan.warnings().is_empty());
    Ok(compose_entries(&[plan.all_patches()])?.into_parts().0)
}

fn assembled_web_overlay(root: &Path) -> anyhow::Result<PathBuf> {
    let settings = root.join("settings.yaml");
    let storages = root.join("storages");
    std::fs::write(&settings, "{}\n")?;
    let quote = |path: &Path| serde_json::to_string(path.to_string_lossy().as_ref()).unwrap();
    let overlay = root.join("assembled.cordis.yml");
    std::fs::write(
        &overlay,
        format!(
            concat!(
                "- id: settings\n  config: {{ path: {}, watch: false }}\n",
                "- id: storage-json\n  config: {{ root: {} }}\n",
                "- {{ id: webserver, disabled: true }}\n",
                "- {{ id: web-runtime, disabled: true }}\n",
                "- {{ id: session-telemetry-otel, disabled: true }}\n",
                "- {{ id: skill-badge, disabled: false }}\n",
                "- {{ id: modules, disabled: true }}\n",
                "- {{ id: connection, disabled: true }}\n",
                "- {{ id: client-hmr, disabled: true }}\n",
                "- {{ id: directory-picker, disabled: true }}\n",
                "- insert:\n",
                "    - {{ id: directory-picker-browse, name: '@seekdeep-ai/seekdeep-host-directory-picker-browse' }}\n",
                "    - {{ id: ui-directory-picker-browse, name: '@seekdeep-ai/seekdeep-client-ui-directory-picker-browse' }}\n",
                "- id: agent-presets\n",
                "  config:\n",
                "    default: standard\n",
                "    roots: [{{ path: {}, trust: system }}]\n",
                "    includeUserRoot: false\n",
            ),
            quote(&settings),
            quote(&storages),
            quote(&shipped_preset_root()),
        ),
    )?;
    Ok(overlay)
}

fn create_preset_options(roster: Arc<AgentPresetRegistry>, id: &'static str) -> CreateAgentOptions {
    let mut options = CreateAgentOptions::new(SessionId::new(format!("shipped-{id}")));
    options.meta = CreateAgentMeta {
        agent_preset: Some(id.to_owned()),
        ..CreateAgentMeta::default()
    };
    options.setup = Some(Arc::new(move |agent_context| {
        let roster = roster.clone();
        Box::pin(async move {
            roster.mount(&agent_context, Some(id)).await?;
            Ok(None)
        })
    }));
    options
}

fn scoped_tool_names(tools: &ToolRuntime, agent: &seekdeep_agent::Agent) -> Vec<String> {
    tools
        .schemas(Some(agent.scope_key()))
        .into_iter()
        .map(|schema| schema.name)
        .collect()
}

async fn assert_shipped_roster(roster: &AgentPresetRegistry) -> anyhow::Result<()> {
    let mut ids = roster
        .list()
        .await?
        .into_iter()
        .map(|preset| preset.id)
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["code", "cordis", "minimal", "standard"]);
    Ok(())
}

#[test]
fn web_and_base_profiles_pin_lazy_search_badge_and_both_platform_shell_stacks() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let web = web_plan(temporary.path())?;
    assert_eq!(
        config_field(entry(&web, "session-query-sqlite"), "openAt").as_str(),
        Some("never")
    );
    assert_ne!(
        entry(&web, "session-query-sqlite").disabled(),
        Some(&ProfileNode::Bool(true))
    );
    assert_eq!(
        expression(entry(&web, "bash-sandbox")),
        "process.platform === 'win32'"
    );
    assert_eq!(
        expression(entry(&web, "pwsh-sandbox")),
        "process.platform !== 'win32'"
    );
    assert_eq!(
        entry(&web, "tool-bash").disabled(),
        Some(&ProfileNode::Bool(true))
    );
    assert_eq!(
        entry(&web, "tool-pwsh").disabled(),
        Some(&ProfileNode::Bool(true))
    );
    assert_eq!(
        entry(&web, "skill-badge").disabled(),
        Some(&ProfileNode::Bool(true))
    );
    for id in [
        "permission",
        "ui-permission",
        "sandbox",
        "sandbox-policy",
        "fs-sandbox",
        "approval",
    ] {
        assert_ne!(
            entry(&web, id).disabled(),
            Some(&ProfileNode::Bool(true)),
            "{id}"
        );
    }

    let base_home = temporary.path().join("base-home");
    let cwd = temporary.path().join("base-workspace");
    std::fs::create_dir_all(&cwd)?;
    init_profile(
        &resolve_profile_dir("base-only", &base_home)?,
        &["@seekdeep-ai/seekdeep-base"],
    )?;
    let plan = compose_profile_at(
        "base-only",
        &[],
        &cwd,
        &base_home,
        &install_anchor(&base_home),
        &shipped_preset_root(),
        None,
    )?;
    let base = compose_entries(&[plan.all_patches()])?.into_parts().0;
    for (id, expected) in [
        ("bash-sandbox", "process.platform === 'win32'"),
        ("tool-bash", "process.platform === 'win32'"),
        ("pwsh-sandbox", "process.platform !== 'win32'"),
        ("tool-pwsh", "process.platform !== 'win32'"),
    ] {
        assert_eq!(expression(entry(&base, id)), expected, "{id}");
    }
    Ok(())
}

#[test]
fn shipped_presets_gate_shells_and_minimal_omits_model_shell_tools() -> anyhow::Result<()> {
    let root = shipped_preset_root();
    let temporary = tempfile::tempdir()?;
    let cwd = temporary.path().join("workspace");
    let home = temporary.path().join("home");
    std::fs::create_dir_all(&cwd)?;
    let catalog = framework_profile_catalog(&cwd, &home, &LaunchEnvironmentSnapshot::default())?;
    let mut ids = std::fs::read_dir(&root)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["code", "cordis", "minimal", "standard"]);
    for preset in ["standard", "code", "cordis"] {
        let path = root.join(preset).join("agent.cordis.yml");
        let source = std::fs::read_to_string(&path)?;
        catalog.preflight_yaml(&source)?;
        let entries = load_entries(&path)?;
        assert_eq!(
            expression(entry(&entries, "tool-bash")),
            "process.platform === 'win32'",
            "{preset}"
        );
        assert_eq!(
            expression(entry(&entries, "tool-pwsh")),
            "process.platform !== 'win32'",
            "{preset}"
        );
    }
    let minimal_path = root.join("minimal/agent.cordis.yml");
    let minimal_source = std::fs::read_to_string(&minimal_path)?;
    catalog.preflight_yaml(&minimal_source)?;
    let minimal = load_entries(&minimal_path)?;
    assert!(minimal.iter().all(|row| {
        row.id()
            .is_none_or(|id| !matches!(id.as_str(), "tool-bash" | "tool-pwsh"))
    }));
    Ok(())
}

#[test]
fn memory_examples_keep_pins_generic_fields_and_compiled_plugin_resolution() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let example_root = repository.join("examples/mcp-memory");
    let contracts = [
        ("memorix.cordis.yml", "memory-memorix", "memorix", "1.3.0"),
        (
            "mcp-reference-memory.cordis.yml",
            "memory-mcp-reference",
            "reference_memory",
            "2026.7.4",
        ),
        ("engram.cordis.yml", "memory-engram", "engram", "1.20.0"),
    ];
    let temporary = tempfile::tempdir()?;
    let cwd = temporary.path().join("workspace");
    let home = temporary.path().join("home");
    std::fs::create_dir_all(&cwd)?;
    let catalog = framework_profile_catalog(&cwd, &home, &LaunchEnvironmentSnapshot::default())?;
    for (filename, id, server_name, pin) in contracts {
        let path = example_root.join(filename);
        let source = std::fs::read_to_string(&path)?;
        assert!(source.lines().next().is_some_and(|line| line.contains(pin)));
        assert!(!source.contains("DEEPSEEK_API_KEY"));
        assert!(!source.contains("sk-"));
        let patches = load_overlay_patches("memory-mcp-config-test", &path)?;
        assert_eq!(patches.len(), 1);
        let rows = patches[0]
            .insert()
            .and_then(ProfileNode::as_sequence)
            .expect("memory overlay must insert one row");
        assert_eq!(rows.len(), 1);
        let row = ProfileEntry::from_fields(
            rows[0]
                .as_mapping()
                .expect("inserted memory row must be a mapping")
                .clone(),
        );
        assert_eq!(
            row.id()
                .as_ref()
                .map(seekdeep_loader::profile_patch::ProfileEntryId::as_str),
            Some(id)
        );
        assert_eq!(row.name(), Some("@seekdeep-ai/seekdeep-mcp-client"));
        assert_eq!(config_field(&row, "serverName").as_str(), Some(server_name));
        assert_eq!(config_field(&row, "transport").as_str(), Some("stdio"));
        let tools = ProfileEntry::from_fields(indexmap::IndexMap::from([
            ("id".to_owned(), ProfileNode::String("tools".to_owned())),
            (
                "name".to_owned(),
                ProfileNode::String("@seekdeep-ai/seekdeep-tools".to_owned()),
            ),
        ]));
        catalog.preflight_yaml(&render_entry_list_yaml(&[tools, row])?)?;
    }
    Ok(())
}

#[test]
fn compiled_catalog_resolves_adaptive_picker_children_and_badge_surface() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let cwd = temporary.path().join("workspace");
    let home = temporary.path().join("home");
    std::fs::create_dir_all(&cwd)?;
    let catalog = framework_profile_catalog(&cwd, &home, &LaunchEnvironmentSnapshot::default())?;
    let source = concat!(
        "- { id: browse-backend, name: '@seekdeep-ai/seekdeep-host-directory-picker-browse' }\n",
        "- { id: native-backend, name: '@seekdeep-ai/seekdeep-host-directory-picker-native' }\n",
        "- { id: browse-ui, name: '@seekdeep-ai/seekdeep-client-ui-directory-picker-browse' }\n",
        "- { id: native-ui, name: '@seekdeep-ai/seekdeep-client-ui-directory-picker-native' }\n",
        "- { id: skill, name: '@seekdeep-ai/seekdeep-skill' }\n",
        "- { id: skill-badge, name: '@seekdeep-ai/seekdeep-skill-badge' }\n",
    );
    catalog.preflight_yaml(source)?;
    Ok(())
}

#[tokio::test]
async fn shipped_web_profile_mounts_all_shipped_agents_with_isolated_tools() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let overlay = assembled_web_overlay(temporary.path())?;
    let plan = compose_profile_at(
        "web",
        &[overlay],
        &cwd,
        &home,
        &install_anchor(&home),
        &shipped_preset_root(),
        None,
    )?;
    let catalog = framework_profile_catalog(&cwd, &home, &LaunchEnvironmentSnapshot::default())?;
    let running = run_profile_process(
        plan,
        &catalog,
        LaunchEnvironmentSnapshot::default(),
        Vec::new(),
    )
    .await?;
    let context = running.context().clone();
    assert!(context.get(TOKEN_METER).is_some());
    let projections = context
        .get(SESSION_PROJECTIONS)
        .ok_or_else(|| anyhow::anyhow!("shipped Web projection registry missing"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("shipped Web tools service missing"))?;
    assert!(tools.schemas(None).is_empty());
    let roster = context
        .get(AGENT_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("shipped Web preset roster missing"))?;
    assert_shipped_roster(&roster).await?;
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("shipped Web agent registry missing"))?;
    let minimal = agents
        .create(create_preset_options(roster.clone(), "minimal"))
        .await?;
    let standard = agents
        .create(create_preset_options(roster.clone(), "standard"))
        .await?;
    let code = agents
        .create(create_preset_options(roster.clone(), "code"))
        .await?;
    let cordis = agents
        .create(create_preset_options(roster, "cordis"))
        .await?;
    let mut minimal_tools = scoped_tool_names(&tools, &minimal.agent);
    minimal_tools.sort();
    assert_eq!(minimal_tools, ["bash", "str_replace_editor"]);
    let projected = projections.snapshot(minimal.agent.session())?;
    for key in ["contextBreakdown", "contextPressure", "tokenUsage"] {
        assert!(projected.values.contains_key(key), "missing {key}");
    }
    let standard_tools = scoped_tool_names(&tools, &standard.agent);
    assert!(standard_tools.contains(&"bash".to_owned()));
    assert!(standard_tools.contains(&"skill".to_owned()));
    assert!(standard_tools.len() > 10);
    assert!(!standard_tools.contains(&"cordis_define".to_owned()));
    let cordis_tools = scoped_tool_names(&tools, &cordis.agent);
    assert!(cordis_tools.contains(&"cordis_define".to_owned()));
    assert!(cordis_tools.contains(&"bash".to_owned()));
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("shipped Web system prompt missing"))?;
    let code_prompt = prompt
        .assemble(AssembleContext {
            scope: Some(code.agent.scope_key()),
            ..AssembleContext::default()
        })
        .await?;
    assert_eq!(
        code_prompt
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["run_code"]
    );
    minimal.dispose().await?;
    assert!(
        tools
            .schemas(Some(standard.agent.scope_key()))
            .iter()
            .any(|schema| schema.name == "bash")
    );
    code.dispose().await?;
    cordis.dispose().await?;
    standard.dispose().await?;
    assert!(tools.schemas(None).is_empty());
    assert_eq!(running.shutdown(0).await?, 0);
    Ok(())
}
