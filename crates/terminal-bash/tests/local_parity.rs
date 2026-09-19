//! Real Bash, native PTY, signal, persistence, and process-tree parity.

#![cfg(unix)]

use std::{process::Command, process::Stdio, sync::Arc, time::Duration};

use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::{
    ConfinedArgv, SandboxEnforcement, SandboxPolicy, SandboxProvider, SandboxService,
};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, install as install_policy};
use seekdeep_scope::ScopeKey;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use seekdeep_terminal::{
    TerminalReadRequest, TerminalSendOperationRef, TerminalSendRequest, TerminalSessionService,
    TerminalSignal, TerminalSpawnRequest, TerminalWaitReason,
};
use seekdeep_terminal_bash::{TerminalBashConfig, apply};
use tempfile::TempDir;

#[derive(Debug, Default)]
struct PassthroughSandbox {
    calls: parking_lot::Mutex<Vec<(Vec<String>, SandboxPolicy)>>,
}

impl SandboxProvider for PassthroughSandbox {
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv> {
        self.calls.lock().push((argv.to_vec(), policy.clone()));
        Ok(ConfinedArgv {
            argv: argv.to_vec(),
            enforcement: SandboxEnforcement::Full,
            denial_signatures: Vec::new(),
            runner_failure_rules: Vec::new(),
        })
    }
}

struct Harness {
    context: Context,
    root: TempDir,
    terminals: Arc<TerminalSessionService>,
    owner: Arc<Agent>,
    sandbox: Arc<PassthroughSandbox>,
}

impl Harness {
    async fn new(mode: seekdeep_sandbox::SandboxMode, timing: Option<(f64, f64, f64)>) -> Self {
        let root = tempfile::Builder::new()
            .prefix("seekdeep-pty-local-")
            .tempdir()
            .unwrap();
        let context = Context::new();
        let registry = Arc::new(AgentRegistry::new(context.clone()));
        registry.provide(&context).unwrap();
        let terminals = TerminalSessionService::install(&context).await.unwrap();
        let sandbox = Arc::new(PassthroughSandbox::default());
        SandboxService::new(sandbox.clone())
            .provide(&context)
            .unwrap();
        install_policy(
            &context,
            SandboxPolicyConfig {
                mode,
                workspace_root: Some(root.path().to_path_buf()),
            },
        )
        .unwrap();
        LocalSubprocessRuntime::install(&context).unwrap();
        let config = TerminalBashConfig {
            poll_interval_ms: 10.0,
            exact_probe_after_ms: 20.0,
            idle_silence_ms: timing.map_or(250.0, |value| value.0),
            handoff_grace_ms: timing.map_or(250.0, |value| value.1),
            timeout_ms: timing.map_or(2_000.0, |value| value.2),
            dispose_grace_ms: 500.0,
            scrollback_lines: 100.0,
            scrollback_max_bytes: 32_768.0,
            max_read_bytes: 16_384.0,
            ..TerminalBashConfig::default()
        };
        apply(&context, &config).unwrap();

        let id = SessionId::new(format!("agent-{}", mode.as_str()));
        let mut header = SessionHeader::new(id.clone());
        header.cwd = Some(root.path().to_str().unwrap().to_owned());
        let session = Session::create(&id, None, Some(header)).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let owner = Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ));
        registry.register(&context, &owner, None).unwrap();
        Self {
            context,
            root,
            terminals,
            owner,
            sandbox,
        }
    }

    async fn spawn(&self) -> seekdeep_terminal::TerminalSpawnResult {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.terminals.spawn(
                self.owner.clone(),
                TerminalSpawnRequest {
                    terminal_type: "shell".into(),
                    name: Some("main".into()),
                    cwd: Some(self.root.path().to_str().unwrap().to_owned()),
                },
                None,
            ),
        )
        .await
        .expect("terminal spawn timeout")
        .expect("terminal spawn")
    }

    fn send(
        &self,
        id: &seekdeep_terminal::TerminalSessionId,
        text: impl Into<String>,
        signal: Option<AbortSignal>,
    ) -> TerminalSendOperationRef {
        self.terminals
            .start_send(
                &self.owner,
                id,
                TerminalSendRequest {
                    text: text.into(),
                    submit: true,
                    signal,
                },
            )
            .unwrap()
    }

    async fn done(
        operation: &TerminalSendOperationRef,
        timeout: Duration,
    ) -> seekdeep_terminal::TerminalSendResult {
        tokio::time::timeout(timeout, operation.done())
            .await
            .expect("send timeout")
            .expect("send result")
    }

    async fn dispose(self) {
        self.context.fiber().dispose().await.unwrap();
    }
}

fn ready_for_next_send(reason: TerminalWaitReason) -> bool {
    matches!(
        reason,
        TerminalWaitReason::StdinRead | TerminalWaitReason::InferredIdle
    )
}

fn process_is_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_until(mut predicate: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate()
}

async fn wait_for_output(
    operation: &TerminalSendOperationRef,
    expected: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut output = String::new();
    while !output.contains(expected) && tokio::time::Instant::now() < deadline {
        output.push_str(&operation.read_output().delta);
        if !output.contains(expected) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    assert!(
        output.contains(expected),
        "missing {expected:?} in {output:?}"
    );
    output
}

#[tokio::test]
async fn persists_cwd_and_environment_scrubs_secrets_and_closes() {
    const SECRET: &str = "SEEKDEEP_TERMINAL_LOCAL_SECRET";
    let harness = Harness::new(seekdeep_sandbox::SandboxMode::DangerFullAccess, None).await;
    let created = harness.spawn().await;
    assert!(
        created.motd.contains("seekdeep> "),
        "startup MOTD was {:?}",
        created.motd
    );

    let first = harness.send(&created.session_id, "export KEEP=ok; cd /", None);
    assert_eq!(
        Harness::done(&first, Duration::from_secs(5))
            .await
            .wait_reason,
        TerminalWaitReason::StdinRead
    );
    let second = harness.send(
        &created.session_id,
        format!("printf 'cwd=%s keep=%s secret=%s\\n' \"$PWD\" \"$KEEP\" \"${{{SECRET}-unset}}\""),
        None,
    );
    let result = Harness::done(&second, Duration::from_secs(5)).await;
    assert!(
        result.viewport.contains("cwd=/ keep=ok secret=unset"),
        "{}",
        result.viewport
    );
    let retained = harness
        .terminals
        .read(
            &harness.owner,
            &created.session_id,
            TerminalReadRequest {
                offset: Some(0.0),
                count: Some(100.0),
            },
        )
        .unwrap();
    assert!(retained.text.contains("cwd=/ keep=ok secret=unset"));
    assert!(
        harness
            .terminals
            .kill(&harness.owner, &created.session_id, None)
            .await
            .unwrap()
    );
    assert!(harness.terminals.list(&harness.owner).is_empty());
    harness.dispose().await;
}

#[tokio::test]
async fn wraps_exact_shell_argv_under_policy_and_unregisters_without_killing_sessions() {
    let harness = Harness::new(seekdeep_sandbox::SandboxMode::WorkspaceWrite, None).await;
    let created = harness.spawn().await;
    let calls = harness.sandbox.calls.lock().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, vec!["/bin/bash", "--noprofile", "--norc", "-i"]);
    assert_eq!(
        calls[0].1.workspace_root,
        std::fs::canonicalize(harness.root.path()).unwrap()
    );
    assert_eq!(
        calls[0].1.session_id.as_ref().map(SessionId::as_str),
        Some("agent-workspace-write")
    );
    // The backend registration is root-owned in this compact harness; the
    // provider-unload behavior is pinned by backend_parity's child-fiber test.
    assert_eq!(harness.terminals.list(&harness.owner).len(), 1);
    harness
        .terminals
        .kill(&harness.owner, &created.session_id, None)
        .await
        .unwrap();
    harness.dispose().await;
}

#[tokio::test]
async fn signals_foreground_and_kills_term_ignoring_background_descendant() {
    let harness = Harness::new(seekdeep_sandbox::SandboxMode::DangerFullAccess, None).await;
    let created = harness.spawn().await;
    let foreground = harness.send(&created.session_id, "sleep 60", None);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        harness
            .terminals
            .signal(&harness.owner, &created.session_id, TerminalSignal::SIGINT)
            .await
            .unwrap()
            .is_delivered()
    );
    assert!(ready_for_next_send(
        Harness::done(&foreground, Duration::from_secs(5))
            .await
            .wait_reason
    ));

    let background = harness.send(
        &created.session_id,
        "sh -c 'trap \"\" TERM; sleep 60' & echo CHILD=$!",
        None,
    );
    let output = Harness::done(&background, Duration::from_secs(5))
        .await
        .viewport;
    let pid = output
        .split("CHILD=")
        .skip(1)
        .find_map(|tail| tail.split_whitespace().next()?.parse::<u32>().ok())
        .unwrap_or_else(|| panic!("missing child pid in {output:?}"));
    assert!(process_is_running(pid));
    harness
        .terminals
        .kill(&harness.owner, &created.session_id, None)
        .await
        .unwrap();
    assert!(wait_until(|| !process_is_running(pid), Duration::from_secs(3)).await);
    harness.dispose().await;
}

#[tokio::test]
async fn quiesces_disowned_descendant_after_natural_shell_exit() {
    let harness = Harness::new(seekdeep_sandbox::SandboxMode::DangerFullAccess, None).await;
    let created = harness.spawn().await;
    let pid_file = harness.root.path().join("disowned.pid");
    let command = format!(
        "sh -c 'trap \"\" TERM; printf \"%s\" \"$$\" > \"$1\"; sleep 60' seekdeep \"{}\" & disown",
        pid_file.display()
    );
    let background = harness.send(&created.session_id, command, None);
    Harness::done(&background, Duration::from_secs(5)).await;
    assert!(wait_until(|| pid_file.exists(), Duration::from_secs(3)).await);
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert!(process_is_running(pid));
    let exit = harness.send(&created.session_id, "exit", None);
    Harness::done(&exit, Duration::from_secs(5)).await;
    assert!(
        wait_until(
            || harness
                .terminals
                .list(&harness.owner)
                .first()
                .is_some_and(|entry| matches!(
                    entry.status,
                    seekdeep_terminal::TerminalSessionStatus::Exited { .. }
                )),
            Duration::from_secs(3)
        )
        .await
    );
    harness
        .terminals
        .kill(&harness.owner, &created.session_id, None)
        .await
        .unwrap();
    assert!(wait_until(|| !process_is_running(pid), Duration::from_secs(3)).await);
    harness.dispose().await;
}

#[tokio::test]
async fn cancels_slow_raw_mode_foreground_with_real_sigint_and_reuses_shell() {
    let harness = Harness::new(
        seekdeep_sandbox::SandboxMode::DangerFullAccess,
        Some((10_000.0, 250.0, 15_000.0)),
    )
    .await;
    let created = harness.spawn().await;
    let signal = AbortSignal::default();
    let command = "python3 -c 'import signal,sys,termios,time; signal.signal(signal.SIGINT, lambda *_: (print(\"SIGINT_SEEN\", flush=True), sys.exit(0))); attrs=termios.tcgetattr(0); attrs[3] &= ~termios.ISIG; termios.tcsetattr(0, termios.TCSANOW, attrs); time.sleep(2.1); print(\"RAW_\" + \"READY\", flush=True); time.sleep(60)'";
    assert!(!command.contains("RAW_READY"));
    let foreground = harness.send(&created.session_id, command, Some(signal.clone()));
    wait_for_output(&foreground, "RAW_READY", Duration::from_secs(15)).await;
    signal.abort();
    assert!(ready_for_next_send(
        Harness::done(&foreground, Duration::from_secs(10))
            .await
            .wait_reason
    ));
    let after = harness.send(&created.session_id, "printf 'AFTER_%s\\n' SIGINT", None);
    wait_for_output(&after, "AFTER_SIGINT", Duration::from_secs(15)).await;
    assert!(ready_for_next_send(
        Harness::done(&after, Duration::from_secs(10))
            .await
            .wait_reason
    ));
    harness
        .terminals
        .kill(&harness.owner, &created.session_id, None)
        .await
        .unwrap();
    harness.dispose().await;
}
