//! Compiled runtime plugins selected by the embedded Python runtime manifest.

use std::path::Path;

use seekdeep_cordis::Plugin;
use seekdeep_loader::PluginCatalog;
use serde_json::Value;

type PluginFactory = fn() -> Plugin;

pub(crate) const FACTORIES: &[(&str, PluginFactory)] = &[
    ("cordis-plugin-timer", seekdeep_cordis_timer::plugin),
    ("seekdeep-acp", seekdeep_acp::plugin),
    ("seekdeep-agent", seekdeep_agent::plugin),
    (
        "seekdeep-agent-instructions",
        seekdeep_agent_instructions::plugin,
    ),
    ("seekdeep-agent-loop", seekdeep_agent_loop::plugin),
    (
        "seekdeep-agent-spine-demo",
        seekdeep_agent_spine_demo::plugin,
    ),
    ("seekdeep-bash-local", seekdeep_bash_local::plugin),
    (
        "seekdeep-code-runtime-worker-thread",
        seekdeep_code_runtime_worker_thread::plugin,
    ),
    ("seekdeep-command-goal", seekdeep_command_goal::plugin),
    ("seekdeep-commands", seekdeep_commands::plugin),
    (
        "seekdeep-compaction-basic",
        seekdeep_compaction_basic::plugin,
    ),
    (
        "seekdeep-compaction-tool-result-pruner",
        seekdeep_compaction_tool_result_pruner::plugin,
    ),
    (
        "seekdeep-cordis-host-runner",
        seekdeep_cordis_host_runner::plugin,
    ),
    ("seekdeep-fs-local", seekdeep_fs_local::plugin),
    (
        "seekdeep-fs-observation-policy",
        seekdeep_fs_observation_policy::plugin,
    ),
    ("seekdeep-fs-sandbox", seekdeep_fs_sandbox::plugin),
    ("seekdeep-goal", seekdeep_goal::plugin),
    (
        "seekdeep-goal-round-driver",
        seekdeep_goal_round_driver::plugin,
    ),
    (
        "seekdeep-hooks-claude-code",
        seekdeep_hooks_claude_code::plugin,
    ),
    ("seekdeep-hooks-codex", seekdeep_hooks_codex::plugin),
    ("seekdeep-invariants", invariants_plugin),
    (
        "seekdeep-jobs-local",
        seekdeep_jobs_local::LocalJobRegistry::plugin,
    ),
    ("seekdeep-llm", seekdeep_llm::plugin),
    ("seekdeep-llm-deepseek", seekdeep_llm_deepseek::plugin),
    ("seekdeep-llm-pi-ai", seekdeep_llm_pi_ai::plugin),
    ("seekdeep-llm-retry", seekdeep_llm_retry::plugin),
    (
        "seekdeep-permission-presets",
        seekdeep_permission_presets::plugin,
    ),
    ("seekdeep-plan-mode", seekdeep_plan_mode::plugin),
    (
        "seekdeep-repeat-tool-reminder",
        seekdeep_repeat_tool_reminder::plugin,
    ),
    ("seekdeep-sandbox-local", seekdeep_sandbox_local::plugin),
    ("seekdeep-sandbox-policy", seekdeep_sandbox_policy::plugin),
    (
        "seekdeep-sdk-jsonrpc-server",
        seekdeep_sdk_server::deferred_plugin,
    ),
    ("seekdeep-session", seekdeep_core::session_store::plugin),
    (
        "seekdeep-session-checkpoint-policy",
        seekdeep_session_checkpoint_policy::plugin,
    ),
    (
        "seekdeep-session-persistence-jsonl",
        seekdeep_session_persistence_jsonl::plugin,
    ),
    (
        "seekdeep-session-persistence-sqlite",
        seekdeep_session_persistence_sqlite::plugin,
    ),
    (
        "seekdeep-session-projection",
        seekdeep_session_projection::plugin,
    ),
    (
        "seekdeep-session-query-sqlite",
        seekdeep_session_query_sqlite::plugin,
    ),
    (
        "seekdeep-session-reference",
        seekdeep_session_reference::plugin,
    ),
    ("seekdeep-session-title", seekdeep_session_title::plugin),
    ("seekdeep-shell-env", seekdeep_shell_env::plugin),
    ("seekdeep-skill", seekdeep_skill::plugin),
    (
        "seekdeep-skill-filesystem",
        seekdeep_skill_filesystem::plugin,
    ),
    ("seekdeep-subagent", seekdeep_subagent::plugin),
    ("seekdeep-subagent-acp", seekdeep_subagent_acp::plugin),
    (
        "seekdeep-subagent-fork-in-process",
        seekdeep_subagent_fork_in_process::plugin,
    ),
    (
        "seekdeep-subagent-spawn-in-process",
        seekdeep_subagent_spawn_in_process::plugin,
    ),
    (
        "seekdeep-subprocess-local",
        seekdeep_subprocess_local::plugin,
    ),
    ("seekdeep-system-prompt", seekdeep_system_prompt::plugin),
    ("seekdeep-terminal", seekdeep_terminal::plugin),
    ("seekdeep-terminal-bash", seekdeep_terminal_bash::plugin),
    ("seekdeep-token-meter", seekdeep_token_meter::plugin),
    ("seekdeep-tool-ask-user", seekdeep_tool_ask_user::plugin),
    ("seekdeep-tool-bash", seekdeep_tool_bash::plugin),
    (
        "seekdeep-tool-bash-persistent",
        seekdeep_tool_bash_persistent::plugin,
    ),
    (
        "seekdeep-tool-call-timeout-policy",
        seekdeep_tool_timeout_policy::plugin,
    ),
    ("seekdeep-tool-cordis", seekdeep_tool_cordis::plugin),
    ("seekdeep-tool-fs", seekdeep_tool_fs::plugin),
    ("seekdeep-tool-goal", seekdeep_tool_goal::index::plugin),
    ("seekdeep-tool-jobs", seekdeep_tool_jobs::index::plugin),
    ("seekdeep-tool-skill", seekdeep_tool_skill::plugin),
    (
        "seekdeep-tool-str-replace-editor",
        seekdeep_tool_str_replace_editor::plugin,
    ),
    ("seekdeep-tool-subagent", seekdeep_tool_subagent::plugin),
    (
        "seekdeep-tool-subagent-control",
        seekdeep_tool_subagent_control::plugin,
    ),
    ("seekdeep-tool-todo", seekdeep_tool_todo::plugin),
    ("seekdeep-tool-web", seekdeep_tool_web::plugin),
    ("seekdeep-tool-workflow", seekdeep_tool_workflow::plugin),
    ("seekdeep-tools", seekdeep_tools::plugin),
    ("seekdeep-user-approval", seekdeep_user_approval::plugin),
    ("seekdeep-user-questions", seekdeep_user_questions::plugin),
    ("seekdeep-web", seekdeep_web::plugin),
    ("seekdeep-web-fetch-http", seekdeep_web_fetch_http::plugin),
    (
        "seekdeep-web-search-deepseek",
        seekdeep_web_search_deepseek::plugin,
    ),
    ("seekdeep-web-search-exa", seekdeep_web_search_exa::plugin),
    (
        "seekdeep-web-search-perplexity",
        seekdeep_web_search_perplexity::plugin,
    ),
    (
        "seekdeep-workflow-worker-thread",
        seekdeep_workflow_worker_thread::plugin,
    ),
];

/// Register only manifest-owned concrete plugins; registration does not mount them.
pub(crate) fn register(
    catalog: &PluginCatalog,
    development: bool,
    integrated_worker: Option<&Path>,
) -> anyhow::Result<()> {
    let manifest: Value =
        serde_json::from_str(include_str!("../../../python/sdk-runtime/package.json"))?;
    let dependencies = manifest
        .get("dependencies")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("runtime closure manifest has no dependency object"))?;
    for &(name, factory) in FACTORIES {
        if dependencies.contains_key(&format!("@seekdeep-ai/{name}")) {
            let plugin = match (name, integrated_worker) {
                ("seekdeep-workflow-worker-thread", Some(path)) => {
                    seekdeep_workflow_worker_thread::plugin_with_integrated_worker_path(
                        path.to_owned(),
                    )
                }
                _ => factory(),
            };
            register_aliases(catalog, name, plugin)?;
        }
    }
    if dependencies.contains_key("@seekdeep-ai/seekdeep-tool-subagent-control") {
        register_aliases(
            catalog,
            "seekdeep-tool-subagent-control/list-agents",
            seekdeep_tool_subagent_control::list_plugin(),
        )?;
    }
    if development {
        register_aliases(
            catalog,
            "seekdeep-llm-replay",
            seekdeep_llm_replay::plugin(),
        )?;
    }
    Ok(())
}

fn register_aliases(catalog: &PluginCatalog, name: &str, plugin: Plugin) -> anyhow::Result<()> {
    catalog.register_named(name, plugin.clone())?;
    catalog.register_named(&format!("@seekdeep-ai/{name}"), plugin)?;
    Ok(())
}

fn invariants_plugin() -> Plugin {
    Plugin::new(
        "invariants",
        std::iter::empty::<String>(),
        |context, config| {
            Box::pin(async move {
                let config = serde_json::from_value(config)?;
                seekdeep_invariants::InvariantRegistry::install(&context, &config)?;
                Ok(())
            })
        },
    )
}
