//! Profile layer ordering, privacy switch, framework fallback, and live reload parity.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use seekdeep::profile_boot::{
    boot_profile_with_failure_handler, compose_profile_at, framework_profile_catalog,
    register_profile_framework_plugins, resolve_telemetry_patch, run_profile_process,
};
use seekdeep_app_boot::{compose_entries, init_profile, resolve_profile_dir};
use seekdeep_cmdline::{APP_EXIT, CMDLINE_ARGS};
use seekdeep_cordis::{Context, FiberState, Plugin, ServiceKey};
use seekdeep_cordis_timer::TIMER;
use seekdeep_hmr::HMR;
use seekdeep_loader::{
    PluginCatalog,
    profile_patch::{ProfileNode, render_entry_list_yaml},
};
use seekdeep_util::launch_environment::{LaunchEnvironmentSnapshot, SEEKDEEP_LAUNCH_ENVIRONMENT};
use serde_json::{Value, json};

const CURRENT: ServiceKey<Value> = ServiceKey::new("current");

fn install_anchor(home: &Path) -> std::path::PathBuf {
    home.join("profiles")
        .join(".seekdeep-installation")
        .join("package.json")
}

fn entry<'a>(
    entries: &'a [seekdeep_loader::profile_patch::ProfileEntry],
    id: &str,
) -> &'a seekdeep_loader::profile_patch::ProfileEntry {
    entries
        .iter()
        .find(|entry| entry.id().is_some_and(|entry_id| entry_id.as_str() == id))
        .unwrap_or_else(|| panic!("missing entry {id}"))
}

fn config_field<'a>(
    entry: &'a seekdeep_loader::profile_patch::ProfileEntry,
    key: &str,
) -> &'a ProfileNode {
    entry
        .config()
        .and_then(ProfileNode::as_mapping)
        .and_then(|config| config.get(key))
        .unwrap_or_else(|| panic!("missing config field {key}"))
}

#[test]
fn telemetry_switch_is_nonempty_fail_closed_and_row_aware() {
    assert!(resolve_telemetry_patch(None, true).is_none());
    assert!(resolve_telemetry_patch(Some(""), true).is_none());
    assert!(resolve_telemetry_patch(Some("1"), false).is_none());
    for value in ["1", "0", "false", "anything"] {
        let patch = resolve_telemetry_patch(Some(value), true).unwrap();
        assert_eq!(patch.id().unwrap().as_str(), "session-telemetry-otel");
        assert_eq!(patch.field("disabled"), Some(&ProfileNode::Bool(true)));
    }
}

#[test]
fn web_plan_orders_layers_and_appends_launcher_owned_preset_and_privacy_patches()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let overlay = cwd.join("web.patch.yml");
    std::fs::write(
        &overlay,
        "- id: webserver\n  config: { host: 127.0.0.1, port: 3999 }\n",
    )?;
    let shipped = temporary.path().join("shipped-presets");
    let plan = compose_profile_at(
        "web",
        &[std::path::PathBuf::from("web.patch.yml")],
        &cwd,
        &home,
        &install_anchor(&home),
        &shipped,
        Some("false"),
    )?;
    assert!(plan.row("agent-presets").is_some());
    assert!(plan.row("session-telemetry-otel").is_some());
    assert!(plan.warnings().is_empty());

    let effective = compose_entries(&[plan.all_patches()])?;
    let presets = entry(effective.entries(), "agent-presets");
    assert_eq!(config_field(presets, "default").as_str(), Some("standard"));
    let roots = config_field(presets, "roots").as_sequence().unwrap();
    assert_eq!(roots.len(), 1);
    let root = roots[0].as_mapping().unwrap();
    assert_eq!(
        root["path"].as_str(),
        Some(shipped.to_string_lossy().as_ref())
    );
    assert_eq!(root["trust"].as_str(), Some("system"));
    assert_eq!(
        entry(effective.entries(), "session-telemetry-otel").disabled(),
        Some(&ProfileNode::Bool(true))
    );
    let port = config_field(entry(effective.entries(), "webserver"), "port");
    assert!(matches!(port, ProfileNode::Number(number) if number.as_u64() == Some(3999)));
    Ok(())
}

#[test]
fn compiled_catalog_preflights_the_real_web_tree_to_the_first_unported_host_boundary()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let plan = compose_profile_at(
        "web",
        &[],
        &cwd,
        &home,
        &install_anchor(&home),
        temporary.path(),
        None,
    )?;
    let effective = compose_entries(&[plan.all_patches()])?;
    let source = render_entry_list_yaml(effective.entries())?;
    let catalog = framework_profile_catalog(&cwd, &home, &LaunchEnvironmentSnapshot::default())?;
    let error = catalog.preflight_yaml(&source).unwrap_err().to_string();
    assert!(
        error.contains("@seekdeep-ai/seekdeep-client-ui-goal"),
        "unexpected compiled-catalog frontier: {error}"
    );
    Ok(())
}

async fn eventually(mut predicate: impl FnMut() -> bool, label: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

fn assert_no_refresh_failures(failures: &Arc<Mutex<Vec<String>>>, stage: &str) {
    let failures = failures.lock().unwrap();
    assert!(
        failures.is_empty(),
        "refresh failures {stage}: {failures:?}"
    );
}

fn value_plugin() -> Plugin {
    Plugin::new("value", std::iter::empty::<&str>(), |context, config| {
        Box::pin(async move {
            context.provide(CURRENT, Arc::new(config))?;
            Ok(())
        })
    })
}

#[tokio::test]
async fn minimal_profile_boots_with_watch_only_hmr_and_recomposes_both_user_layers()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let profile_dir = resolve_profile_dir("minimal", &home)?;
    init_profile(&profile_dir, &[])?;
    let profile_patch = profile_dir.join("cordis.patch.yml");
    std::fs::write(
        &profile_patch,
        "- insert:\n    - id: current\n      name: value\n      config: { value: profile-one }\n",
    )?;
    let home_patch = home.join("cordis.patch.yml");
    let plan = compose_profile_at(
        "minimal",
        &[],
        &cwd,
        &home,
        &install_anchor(&home),
        temporary.path(),
        None,
    )?;
    let catalog = PluginCatalog::new();
    register_profile_framework_plugins(&catalog)?;
    catalog.register_named("value", value_plugin())?;
    let failures = Arc::new(Mutex::new(Vec::new()));
    let observed_failures = failures.clone();
    let application = boot_profile_with_failure_handler(
        plan,
        &catalog,
        None,
        Arc::new(move |_, error| {
            observed_failures.lock().unwrap().push(error.to_string());
        }),
    )
    .await?;
    let context: Context = application.context().clone();
    assert_eq!(context.get(CURRENT).unwrap()["value"], json!("profile-one"));
    assert!(context.get(TIMER).is_some());
    assert!(context.get(HMR).is_some());
    assert_no_refresh_failures(&failures, "after boot");

    std::fs::write(&home_patch, "- id: current\n  config: { value: home }\n")?;
    eventually(
        || {
            context
                .get(CURRENT)
                .is_some_and(|value| value["value"] == "home")
        },
        "home patch",
    )
    .await;
    assert_no_refresh_failures(&failures, "after home patch");
    std::fs::write(
        &profile_patch,
        "- insert:\n    - id: current\n      name: value\n      config: { value: profile-two }\n",
    )?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(context.get(CURRENT).unwrap()["value"], json!("home"));
    assert_no_refresh_failures(&failures, "after profile patch under home override");
    std::fs::remove_file(&home_patch)?;
    eventually(
        || {
            context
                .get(CURRENT)
                .is_some_and(|value| value["value"] == "profile-two")
        },
        "fresh profile after home removal",
    )
    .await;
    assert_no_refresh_failures(&failures, "after home removal");

    application.dispose().await?;
    assert_no_refresh_failures(&failures, "after disposal");
    Ok(())
}

#[tokio::test]
async fn app_exit_during_boot_waits_for_publication_and_disposes_before_process_completion()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let profile_dir = resolve_profile_dir("exit-during-boot", &home)?;
    init_profile(&profile_dir, &[])?;
    std::fs::write(
        profile_dir.join("cordis.patch.yml"),
        "- insert:\n    - id: exit\n      name: exit-during-boot\n",
    )?;
    let plan = compose_profile_at(
        "exit-during-boot",
        &[],
        &cwd,
        &home,
        &install_anchor(&home),
        temporary.path(),
        None,
    )?;
    let observed = Arc::new(Mutex::new(None));
    let observed_by_plugin = observed.clone();
    let catalog = PluginCatalog::new();
    register_profile_framework_plugins(&catalog)?;
    catalog.register_named(
        "exit-during-boot",
        Plugin::new(
            "exit-during-boot",
            ["cmdlineArgs", "launchEnvironment", "appExit"],
            move |context, _| {
                let observed = observed_by_plugin.clone();
                Box::pin(async move {
                    let arguments = context
                        .get(CMDLINE_ARGS)
                        .ok_or_else(|| anyhow::anyhow!("cmdline args missing"))?;
                    let has_environment = context.get(SEEKDEEP_LAUNCH_ENVIRONMENT).is_some();
                    *observed.lock().unwrap() = Some((arguments.get().to_vec(), has_environment));
                    context
                        .get(APP_EXIT)
                        .ok_or_else(|| anyhow::anyhow!("app exit missing"))?
                        .request(7)?;
                    Ok(())
                })
            },
        ),
    )?;
    let running = run_profile_process(
        plan,
        &catalog,
        LaunchEnvironmentSnapshot::default(),
        vec!["--probe".to_owned()],
    )
    .await?;
    let context = running.context().clone();
    assert_eq!(running.wait().await?, 7);
    assert_eq!(
        observed.lock().unwrap().as_ref(),
        Some(&(vec!["--probe".to_owned()], true))
    );
    assert_eq!(context.fiber().state(), FiberState::Disposed);
    Ok(())
}

#[tokio::test]
async fn compiled_foundation_catalog_activates_real_services_and_agent_loop() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let profile_dir = resolve_profile_dir("compiled-foundation", &home)?;
    init_profile(&profile_dir, &[])?;
    std::fs::write(
        profile_dir.join("cordis.patch.yml"),
        concat!(
            "- insert:\n",
            "    - { id: llm, name: '@seekdeep-ai/seekdeep-llm' }\n",
            "    - { id: session, name: '@seekdeep-ai/seekdeep-session' }\n",
            "    - { id: typert, name: '@seekdeep-ai/seekdeep-typert-registry' }\n",
            "    - { id: gateway, name: '@seekdeep-ai/seekdeep-api-gateway' }\n",
            "    - { id: questions, name: '@seekdeep-ai/seekdeep-user-questions' }\n",
            "    - { id: agent, name: '@seekdeep-ai/seekdeep-agent' }\n",
            "    - id: default-model\n",
            "      name: '@seekdeep-ai/seekdeep-agent-default-model'\n",
            "      config: { provider: mock, model: model }\n",
            "    - id: prompt\n",
            "      name: '@seekdeep-ai/seekdeep-system-prompt'\n",
            "      config: { persona: '' }\n",
            "    - { id: tools, name: '@seekdeep-ai/seekdeep-tools' }\n",
            "    - id: loop\n",
            "      name: '@seekdeep-ai/seekdeep-agent-loop'\n",
            "      config: { agents: [], maxParallelToolCalls: 3 }\n",
        ),
    )?;
    let plan = compose_profile_at(
        "compiled-foundation",
        &[],
        &cwd,
        &home,
        &install_anchor(&home),
        temporary.path(),
        None,
    )?;
    let catalog = framework_profile_catalog(&cwd, &home, &LaunchEnvironmentSnapshot::default())?;
    let failures = Arc::new(Mutex::new(Vec::new()));
    let observed = failures.clone();
    let application = boot_profile_with_failure_handler(
        plan,
        &catalog,
        None,
        Arc::new(move |_, error| observed.lock().unwrap().push(error.to_string())),
    )
    .await?;
    let context = application.context();
    assert!(context.get(seekdeep_llm::LLM).is_some());
    assert!(
        context
            .get(seekdeep_core::session_store::SESSIONS)
            .is_some()
    );
    assert!(context.get(seekdeep_typert_registry::TYPERT).is_some());
    assert!(context.get(seekdeep_api_gateway::TYPERT_GATEWAY).is_some());
    assert!(context.get(seekdeep_agent::AGENTS).is_some());
    assert_eq!(
        context
            .get(seekdeep_agent_loop::AGENT_LOOP)
            .unwrap()
            .max_parallel_tool_calls(),
        3
    );
    application.dispose().await?;
    assert_no_refresh_failures(&failures, "after compiled foundation disposal");
    Ok(())
}

#[tokio::test]
async fn web_help_disposes_raw_boot_context_while_web_startup_consumers_are_pending()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(&cwd)?;
    let profile_dir = resolve_profile_dir("web-help", &home)?;
    init_profile(&profile_dir, &[])?;
    std::fs::write(
        profile_dir.join("cordis.patch.yml"),
        concat!(
            "- insert:\n",
            "    - id: consumer\n",
            "      name: startup-consumer\n",
            "      inject: [webStartup]\n",
            "    - id: startup\n",
            "      name: '@seekdeep-ai/seekdeep-web-app/startup'\n",
        ),
    )?;
    let plan = compose_profile_at(
        "web-help",
        &[],
        &cwd,
        &home,
        &install_anchor(&home),
        temporary.path(),
        None,
    )?;
    let catalog = framework_profile_catalog(&cwd, &home, &LaunchEnvironmentSnapshot::default())?;
    let consumer_activated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = consumer_activated.clone();
    catalog.register_named(
        "startup-consumer",
        Plugin::new("startup-consumer", ["webStartup"], move |_, _| {
            observed.store(true, std::sync::atomic::Ordering::Release);
            Box::pin(async { Ok(()) })
        }),
    )?;
    let running = run_profile_process(
        plan,
        &catalog,
        LaunchEnvironmentSnapshot::default(),
        vec!["--help".to_owned()],
    )
    .await?;
    let context = running.context().clone();
    assert_eq!(running.wait().await?, 0);
    assert!(!consumer_activated.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(context.fiber().state(), FiberState::Disposed);
    Ok(())
}
