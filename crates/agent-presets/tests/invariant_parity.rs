//! Continuous service-leak and model-address invariant cases.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications, assemble_context_for};
use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetRoot, PresetTrust, register_invariant,
};
use seekdeep_cordis::{Context, Plugin, ServiceKey};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use serde_json::{Value, json};

const LATE_SERVICE: ServiceKey<Value> = ServiceKey::new("fixtureLateSvc");

async fn harness() -> (
    Context,
    Arc<AgentPresetRegistry>,
    Arc<SystemPrompt>,
    Arc<Mutex<Option<Context>>>,
    tempfile::TempDir,
) {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt.provide(&context).unwrap();
    let late_context = Arc::new(Mutex::new(None));
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "late-provider",
            Plugin::new("late-provider", std::iter::empty::<&str>(), {
                let late_context = late_context.clone();
                move |plugin_context, _| {
                    *late_context.lock() = Some(plugin_context);
                    Box::pin(async { Ok(()) })
                }
            }),
        )
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("standard");
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(
        directory.join(COMPOSITION_FILE),
        "- id: late\n  name: late-provider\n",
    )
    .await
    .unwrap();
    let roster = AgentPresetRegistry::new(
        &context,
        catalog,
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: "standard".to_owned(),
                roots: vec![PresetRoot {
                    path: root.path().to_string_lossy().into_owned(),
                    trust: PresetTrust::System,
                }],
                include_user_root: false,
            },
            user_root: None,
        },
    )
    .unwrap();
    roster.provide(&context).unwrap();
    let invariants = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&invariants).unwrap();
    registration.await_ready().await.unwrap();
    (context, roster, prompt, late_context, root)
}

fn agent(context: &Context, id: &str) -> Arc<Agent> {
    let session = Session::create(&SessionId::new(id), None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        seekdeep_scope::scope_of(context).unwrap(),
    ))
}

#[tokio::test]
async fn late_process_global_service_is_rejected_and_removed() {
    let (context, roster, _prompt, late_context, _root) = harness().await;
    roster.standing_key_for(Some("standard")).await.unwrap();
    let provider = late_context
        .lock()
        .clone()
        .expect("mounted provider context");
    let error = provider
        .provide(LATE_SERVICE, Arc::new(json!({ "late": true })))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("preset \"standard\" published process-global service(s) [fixtureLateSvc]")
    );
    assert!(context.get(LATE_SERVICE).is_none());
}

#[tokio::test]
async fn model_address_requires_a_joined_agent_but_not_an_agent_free_read() {
    let (context, roster, prompt, _late_context, _root) = harness().await;
    let bare_scope = create_scope(&context, ScopeKey::new(), None).unwrap();
    let bare = agent(&bare_scope.context, "bare");
    let error = prompt
        .assemble(assemble_context_for(&bare, None))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("addressed a model without joining any agent preset")
    );

    roster
        .mount(&bare_scope.context, Some("standard"))
        .await
        .unwrap();
    prompt
        .assemble(assemble_context_for(&bare, None))
        .await
        .unwrap();
    prompt
        .assemble(seekdeep_system_prompt::AssembleContext::default())
        .await
        .unwrap();
    let standing = roster.standing_key_for(Some("standard")).await.unwrap();
    prompt
        .assemble(seekdeep_system_prompt::AssembleContext {
            scope: Some(standing),
            ..Default::default()
        })
        .await
        .unwrap();
}
