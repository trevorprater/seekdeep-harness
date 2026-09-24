//! Standing-generation mounting, Agent joins, isolation, and switching parity.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetExistsError, PresetMountError, PresetNotWritableError, PresetRoot, PresetTrust,
    UnknownPresetError,
};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, ServiceKey};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::{Scope, ScopeKey, create_scope, scope_parent_of};
use seekdeep_tools::{
    ContentToolFixtureOptions, ToolPresentationMode, ToolRuntime, ToolRuntimeConfig,
    define_content_tool_fixture,
};
use serde_json::{Value, json};

const PRIVATE: ServiceKey<Value> = ServiceKey::new("fixturePrivate");

fn roster(
    context: &Context,
    catalog: PluginCatalog,
    root: &std::path::Path,
    default: &str,
) -> Arc<AgentPresetRegistry> {
    AgentPresetRegistry::new(
        context,
        catalog,
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: default.to_owned(),
                roots: vec![PresetRoot {
                    path: root.to_string_lossy().into_owned(),
                    trust: PresetTrust::User,
                }],
                include_user_root: false,
            },
            user_root: None,
        },
    )
    .unwrap()
}

async fn preset(root: &std::path::Path, id: &str, yaml: &str) {
    let directory = root.join(id);
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(directory.join(COMPOSITION_FILE), yaml)
        .await
        .unwrap();
}

fn agent_scope(context: &Context) -> Scope {
    create_scope(context, ScopeKey::new(), None).unwrap()
}

fn tool_plugin(name: &'static str, tools: Arc<ToolRuntime>, starts: Arc<AtomicUsize>) -> Plugin {
    Plugin::new(name, std::iter::empty::<&str>(), move |context, _| {
        let tools = tools.clone();
        let starts = starts.clone();
        Box::pin(async move {
            starts.fetch_add(1, Ordering::AcqRel);
            let definition = ContentToolFixtureOptions::new(
                name,
                name,
                json!({}),
                Arc::new(|_: Value, _| Box::pin(async { Ok(Vec::new()) })),
            );
            tools.register(&context, define_content_tool_fixture(definition)?)?;
            Ok(())
        })
    })
}

fn tools(context: &Context) -> Arc<ToolRuntime> {
    let tools = ToolRuntime::new(
        context.clone(),
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            max_parallel_sub_calls: 4,
        },
    )
    .unwrap();
    tools.provide(context).unwrap();
    tools
}

#[tokio::test]
async fn default_and_named_presets_scope_tools_and_share_one_standing_mount() {
    let context = Context::new();
    let tools = tools(&context);
    let catalog = PluginCatalog::new();
    let starts_a = Arc::new(AtomicUsize::new(0));
    let starts_b = Arc::new(AtomicUsize::new(0));
    catalog
        .register_named(
            "preset:a",
            tool_plugin("tool-a", tools.clone(), starts_a.clone()),
        )
        .unwrap();
    catalog
        .register_named(
            "preset:b",
            tool_plugin("tool-b", tools.clone(), starts_b.clone()),
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    preset(directory.path(), "a", "- id: tool\n  name: preset:a\n").await;
    preset(directory.path(), "b", "- id: tool\n  name: preset:b\n").await;
    let roster = roster(&context, catalog, directory.path(), "a");
    let first = agent_scope(&context);
    let second = agent_scope(&context);
    let other = agent_scope(&context);
    assert_eq!(roster.mount(&first.context, None).await.unwrap().id, "a");
    assert_eq!(
        roster.mount(&second.context, Some("a")).await.unwrap().id,
        "a"
    );
    roster.mount(&other.context, Some("b")).await.unwrap();
    assert!(tools.get("tool-a", scope_of(&first)).is_some());
    assert!(tools.get("tool-b", scope_of(&first)).is_none());
    assert!(tools.get("tool-a", scope_of(&other)).is_none());
    assert!(tools.get("tool-b", scope_of(&other)).is_some());
    assert_eq!(starts_a.load(Ordering::Acquire), 1);
    assert_eq!(starts_b.load(Ordering::Acquire), 1);
    first.dispose().await.unwrap();
    assert!(tools.get("tool-a", scope_of(&second)).is_some());
}

fn scope_of(scope: &Scope) -> Option<ScopeKey> {
    seekdeep_scope::scope_of(&scope.context)
}

#[tokio::test]
async fn child_joins_the_parents_exact_generation_and_survives_parent_disposal() {
    let context = Context::new();
    let tools = tools(&context);
    let catalog = PluginCatalog::new();
    let starts = Arc::new(AtomicUsize::new(0));
    catalog
        .register_named(
            "preset:a",
            tool_plugin("tool-a", tools.clone(), starts.clone()),
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    preset(directory.path(), "a", "- id: tool\n  name: preset:a\n").await;
    let roster = roster(&context, catalog, directory.path(), "a");
    let parent = agent_scope(&context);
    let child = agent_scope(&context);
    roster.mount(&parent.context, Some("a")).await.unwrap();
    assert_eq!(
        roster
            .compose_from(&child.context, &parent.context)
            .unwrap(),
        Some("a".to_owned())
    );
    assert_eq!(
        scope_parent_of(scope_of(&child).unwrap()),
        scope_parent_of(scope_of(&parent).unwrap())
    );
    parent.dispose().await.unwrap();
    assert!(tools.get("tool-a", scope_of(&child)).is_some());
    assert_eq!(starts.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn recompose_commits_only_after_the_target_mount_succeeds() {
    let context = Context::new();
    let tools = tools(&context);
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "preset:a",
            tool_plugin("tool-a", tools.clone(), Arc::new(AtomicUsize::new(0))),
        )
        .unwrap();
    catalog
        .register_named(
            "preset:b",
            tool_plugin("tool-b", tools.clone(), Arc::new(AtomicUsize::new(0))),
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    preset(directory.path(), "a", "- id: tool\n  name: preset:a\n").await;
    preset(directory.path(), "b", "- id: tool\n  name: preset:b\n").await;
    let roster = roster(&context, catalog, directory.path(), "a");
    let agent = agent_scope(&context);
    roster.mount(&agent.context, Some("a")).await.unwrap();
    let error = roster
        .recompose(&agent.context, "missing")
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<UnknownPresetError>().is_some());
    assert_eq!(roster.composed_preset(&agent.context).as_deref(), Some("a"));
    roster.recompose(&agent.context, "b").await.unwrap();
    assert_eq!(roster.composed_preset(&agent.context).as_deref(), Some("b"));
    assert!(tools.get("tool-a", scope_of(&agent)).is_none());
    assert!(tools.get("tool-b", scope_of(&agent)).is_some());
}

#[tokio::test]
async fn changed_composition_starts_one_new_generation_for_later_agents() {
    let context = Context::new();
    let tools = tools(&context);
    let catalog = PluginCatalog::new();
    let starts_a = Arc::new(AtomicUsize::new(0));
    let starts_b = Arc::new(AtomicUsize::new(0));
    catalog
        .register_named(
            "preset:a",
            tool_plugin("tool-a", tools.clone(), starts_a.clone()),
        )
        .unwrap();
    catalog
        .register_named(
            "preset:b",
            tool_plugin("tool-b", tools.clone(), starts_b.clone()),
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    preset(directory.path(), "a", "- id: tool\n  name: preset:a\n").await;
    let roster = roster(&context, catalog, directory.path(), "a");
    let old = agent_scope(&context);
    roster.mount(&old.context, Some("a")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    tokio::fs::write(
        directory.path().join("a").join(COMPOSITION_FILE),
        "- id: tool\n  name: preset:b\n",
    )
    .await
    .unwrap();
    let later_a = agent_scope(&context);
    let later_b = agent_scope(&context);
    roster.mount(&later_a.context, Some("a")).await.unwrap();
    roster.mount(&later_b.context, Some("a")).await.unwrap();
    assert!(tools.get("tool-a", scope_of(&old)).is_some());
    assert!(tools.get("tool-b", scope_of(&later_a)).is_some());
    assert!(tools.get("tool-b", scope_of(&later_b)).is_some());
    assert_eq!(starts_a.load(Ordering::Acquire), 1);
    assert_eq!(starts_b.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn broken_unknown_and_unscoped_mounts_fail_before_binding() {
    let context = Context::new();
    let catalog = PluginCatalog::new();
    let directory = tempfile::tempdir().unwrap();
    preset(directory.path(), "broken", "name: not-a-list\n").await;
    let roster = roster(&context, catalog, directory.path(), "broken");
    let scope = agent_scope(&context);
    assert!(
        roster
            .mount(&scope.context, Some("broken"))
            .await
            .unwrap_err()
            .downcast_ref::<PresetMountError>()
            .is_some()
    );
    assert!(roster.mount(&context, Some("broken")).await.is_err());
    assert!(roster.standing_key_for(Some("missing")).await.is_err());
}

#[tokio::test]
async fn process_global_services_are_rejected_and_isolated_services_are_addressable() {
    let context = Context::new();
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "provider",
            Plugin::new("provider", std::iter::empty::<&str>(), |context, _| {
                Box::pin(async move {
                    context.provide(PRIVATE, Arc::new(json!({ "value": "private" })))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    preset(
        directory.path(),
        "global",
        "- id: provider\n  name: provider\n",
    )
    .await;
    preset(
        directory.path(),
        "isolated",
        concat!(
            "- id: realm\n",
            "  name: cordis:group\n",
            "  group: true\n",
            "  isolate:\n",
            "    fixturePrivate: true\n",
            "  config:\n",
            "    - id: provider\n",
            "      name: provider\n",
        ),
    )
    .await;
    let roster = roster(&context, catalog, directory.path(), "isolated");
    let global = agent_scope(&context);
    assert!(
        roster
            .mount(&global.context, Some("global"))
            .await
            .unwrap_err()
            .downcast_ref::<PresetMountError>()
            .is_some()
    );
    let isolated = agent_scope(&context);
    roster
        .mount(&isolated.context, Some("isolated"))
        .await
        .unwrap();
    assert!(context.get(PRIVATE).is_none());
    let session = Session::create(&SessionId::new("agent"), None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let agent = Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        isolated.context.clone(),
        scope_of(&isolated).unwrap(),
    );
    assert_eq!(
        roster.service_for(&agent, PRIVATE).unwrap()["value"],
        "private"
    );
}

#[tokio::test]
async fn user_root_is_appended_for_live_authoring_and_can_be_disabled() {
    let context = Context::new();
    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    preset(system.path(), "system", "[]\n").await;
    preset(user.path(), "mine", "[]\n").await;
    let registry = AgentPresetRegistry::new(
        &context,
        PluginCatalog::new(),
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: "system".to_owned(),
                roots: vec![PresetRoot {
                    path: system.path().to_string_lossy().into_owned(),
                    trust: PresetTrust::System,
                }],
                include_user_root: true,
            },
            user_root: Some(user.path().to_path_buf()),
        },
    )
    .unwrap();
    assert!(registry.authorable());
    assert_eq!(registry.roots().len(), 2);
    assert_eq!(
        registry
            .list()
            .await
            .unwrap()
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>(),
        ["system", "mine"]
    );

    let disabled = AgentPresetRegistry::new(
        &context,
        PluginCatalog::new(),
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: "system".to_owned(),
                roots: vec![],
                include_user_root: false,
            },
            user_root: Some(user.path().to_path_buf()),
        },
    )
    .unwrap();
    assert!(!disabled.authorable());
    assert!(disabled.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn roster_copy_read_and_remove_apply_live_discovery_policy() {
    let context = Context::new();
    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    preset(system.path(), "system", "- name: plugin\n").await;
    let registry = AgentPresetRegistry::new(
        &context,
        PluginCatalog::new(),
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: "system".to_owned(),
                roots: vec![PresetRoot {
                    path: system.path().to_string_lossy().into_owned(),
                    trust: PresetTrust::System,
                }],
                include_user_root: true,
            },
            user_root: Some(user.path().to_path_buf()),
        },
    )
    .unwrap();
    assert_eq!(registry.read("system").await.unwrap(), "- name: plugin\n");
    registry.copy("system", "mine", Some("Mine")).await.unwrap();
    let mine = registry.resolve(Some("mine")).await.unwrap();
    assert_eq!(mine.name.as_deref(), Some("Mine"));
    assert_eq!(mine.trust, PresetTrust::User);
    assert!(
        registry
            .copy("system", "mine", None)
            .await
            .unwrap_err()
            .downcast_ref::<PresetExistsError>()
            .is_some()
    );
    registry.remove("mine").await.unwrap();
    assert!(registry.resolve(Some("mine")).await.is_err());
    assert!(
        registry
            .remove("system")
            .await
            .unwrap_err()
            .downcast_ref::<PresetNotWritableError>()
            .is_some()
    );
    assert!(
        registry
            .copy("missing", "copy", None)
            .await
            .unwrap_err()
            .downcast_ref::<UnknownPresetError>()
            .is_some()
    );
}

#[tokio::test]
async fn committed_selection_event_republishes_only_stable_session_and_preset_ids() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let directory = tempfile::tempdir().unwrap();
    preset(directory.path(), "standard", "[]\n").await;
    let _roster = roster(&context, PluginCatalog::new(), directory.path(), "standard");
    let observed = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let observed_listener = observed.clone();
    context
        .events()
        .on_sync(
            &context,
            "agent-preset/selected",
            move |_, args| {
                observed_listener.lock().push((
                    args.get::<SessionId>(0).unwrap().to_string(),
                    (*args.get::<String>(1).unwrap()).clone(),
                ));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("selected")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    session
        .append(
            "agent-preset/selected",
            json!({ "agentPreset": "standard" }),
            AppendOptions::default(),
        )
        .unwrap();
    assert_eq!(
        *observed.lock(),
        [("selected".to_owned(), "standard".to_owned())]
    );
}
