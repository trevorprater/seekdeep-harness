//! Loader dependency activation and reversible sandboxed-shell registration.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_sandbox::{ConfinedArgv, SandboxPolicy, SandboxProvider, SandboxService};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::json;

#[derive(Debug)]
struct PassthroughSandbox;

impl SandboxProvider for PassthroughSandbox {
    fn confine(&self, argv: &[String], _policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv> {
        Ok(ConfinedArgv {
            argv: argv.to_vec(),
            enforcement: seekdeep_sandbox::SandboxEnforcement::Full,
            denial_signatures: Vec::new(),
            runner_failure_rules: Vec::new(),
        })
    }
}

#[tokio::test]
async fn plugin_waits_for_all_dependencies_and_disposal_releases_shell_and_settings() {
    let context = Context::new();
    let plugin = context
        .plugin(seekdeep_bash_sandbox::plugin(), json!({}))
        .expect("mount");
    plugin.await_settled().await.expect("pending settles");
    assert!(context.get(seekdeep_shell::SHELL).is_none());

    LocalSubprocessRuntime::install(&context).expect("subprocess");
    SandboxService::new(Arc::new(PassthroughSandbox))
        .provide(&context)
        .expect("sandbox");
    SandboxPolicyService::new(SandboxPolicyConfig::default())
        .expect("policy")
        .provide(&context)
        .expect("provide policy");
    plugin.await_settled().await.expect("activated");
    assert!(context.get(seekdeep_shell::SHELL).is_some());

    plugin.dispose().await.expect("dispose");
    assert!(context.get(seekdeep_shell::SHELL).is_none());
}
