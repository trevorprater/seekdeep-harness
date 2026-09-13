//! Live Cordis inspection joined with generated Host catalog facts.

use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, Fiber, FiberState, PluginFiber};
use seekdeep_cordis_host_runner::DYNAMIC_CORDIS_RUNNER;
use seekdeep_tools::TOOLS;

use crate::api_catalog::{event_api, service_api};

/// Exact lowercase label for each lifecycle state.
#[must_use]
pub const fn state_label(state: FiberState) -> &'static str {
    match state {
        FiberState::Pending => "pending",
        FiberState::Loading => "loading",
        FiberState::Active => "active",
        FiberState::Failed => "failed",
        FiberState::Disposed => "disposed",
        FiberState::Unloading => "unloading",
    }
}

/// Whether `fiber` is `root` or a linked descendant.
#[must_use]
pub fn within_fiber(fiber: &std::sync::Arc<Fiber>, root: &std::sync::Arc<Fiber>) -> bool {
    fiber.is_within(root)
}

/// Lexically ordered services provided by one mount's fiber subtree.
#[must_use]
pub fn provided_services(context: &Context, fiber: &std::sync::Arc<Fiber>) -> Vec<String> {
    let mut services = context
        .service_providers()
        .into_iter()
        .filter(|provider| provider.owner.is_within(fiber))
        .map(|provider| provider.name)
        .collect::<Vec<_>>();
    services.sort();
    services.dedup();
    services
}

/// Declared dependencies that are not currently active.
#[must_use]
pub fn missing_services(context: &Context, fiber: &PluginFiber) -> Vec<String> {
    fiber
        .inject()
        .into_iter()
        .filter(|service| !context.has_named(service))
        .collect()
}

/// One line per registered plugin mount, sorted by display name.
#[must_use]
pub fn describe_plugins(context: &Context) -> Vec<String> {
    let mut fibers = context
        .registry()
        .values()
        .into_iter()
        .flat_map(|runtime| runtime.fibers)
        .collect::<Vec<_>>();
    fibers.sort_by(|left, right| left.plugin_name().cmp(right.plugin_name()));
    fibers
        .into_iter()
        .map(|fiber| {
            format!(
                "- {} [{}]",
                fiber.plugin_name(),
                state_label(fiber.fiber().state())
            )
        })
        .collect()
}

/// Tool names visible to one agent scope.
#[must_use]
pub fn describe_tools(context: &Context, agent: Option<&Agent>) -> Vec<String> {
    context.get(TOOLS).map_or_else(Vec::new, |tools| {
        tools
            .schemas(agent.map(Agent::scope_key))
            .into_iter()
            .map(|schema| format!("- {}", schema.name))
            .collect()
    })
}

/// Live Service registrations with owner/state and catalog coverage.
#[must_use]
pub fn describe_services(context: &Context) -> Vec<String> {
    let catalog = service_api();
    let mut providers = context.service_providers();
    providers.sort_by(|left, right| left.name.cmp(&right.name));
    if providers.is_empty() {
        return vec!["(no services provided)".to_owned()];
    }
    providers
        .into_iter()
        .map(|provider| {
            let state = if provider.owner.state() == FiberState::Active {
                String::new()
            } else {
                format!(", {}", state_label(provider.owner.state()))
            };
            let summary = catalog
                .iter()
                .find(|entry| entry["key"].as_str() == Some(&provider.name))
                .and_then(|entry| entry["summary"].as_str())
                .map(plain_summary)
                .filter(|summary| !summary.is_empty())
                .map(|summary| format!(" — {summary}"))
                .unwrap_or_default();
            format!(
                "- {} (provided by {}{}){}",
                provider.name,
                provider.owner.name(),
                state,
                summary
            )
        })
        .collect()
}

/// Generated Event signatures and waterfall delegation warning.
///
/// # Errors
///
/// Rejects an exact Event name absent from the generated catalog.
pub fn describe_events(name: Option<&str>) -> anyhow::Result<Vec<String>> {
    let selected = name.map_or_else(
        || Ok(event_api().iter().collect::<Vec<_>>()),
        |name| {
            event_api()
                .iter()
                .find(|event| event["name"].as_str() == Some(name))
                .map(|event| vec![event])
                .ok_or_else(|| anyhow::anyhow!("no catalogued event named \"{name}\""))
        },
    )?;
    let mut lines = selected
        .into_iter()
        .flat_map(|event| {
            vec![
                format!(
                    "- {} [{}] — {}",
                    event["name"].as_str().unwrap_or_default(),
                    event["mode"].as_str().unwrap_or_default(),
                    event["summary"].as_str().unwrap_or_default()
                ),
                format!("    {}", event["signature"].as_str().unwrap_or_default()),
            ]
        })
        .collect::<Vec<_>>();
    lines.push("waterfall listeners receive a trailing next() and MUST call it to delegate — returning without next() short-circuits the chain.".to_owned());
    Ok(lines)
}

/// Session-owned dynamic plugin summary.
#[must_use]
pub fn describe_dynamic(context: &Context, agent: Option<&Agent>) -> Vec<String> {
    let rows = agent
        .and_then(|agent| {
            context
                .get(DYNAMIC_CORDIS_RUNNER)
                .map(|runner| (agent, runner))
        })
        .map_or_else(Vec::new, |(agent, runner)| runner.snapshot(agent.id()));
    if rows.is_empty() {
        return vec!["No dynamic Plugins are defined in this session. Definitions live only in this process's memory, so a SeekDeep Harness restart clears them.".to_owned()];
    }
    rows.into_iter()
        .map(|row| {
            format!(
                "- Plugin {}; current: {}; next: {}{}",
                row.plugin_id,
                row.current_package_id
                    .as_ref()
                    .map_or("none", |id| id.as_str()),
                row.next_package_id
                    .as_ref()
                    .map_or("none", |id| id.as_str()),
                row.active_run.as_ref().map_or_else(
                    || "; stopped".to_owned(),
                    |run| format!("; active: {} as {}", run.package_id, run.plugin_run_id)
                )
            )
        })
        .collect()
}

fn plain_summary(summary: &str) -> String {
    summary.replace("{@link ", "").replace('}', "")
}
