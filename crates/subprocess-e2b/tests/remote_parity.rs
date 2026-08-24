//! Shared E2B remote-control helper parity.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_e2b::{E2bCommandExit, E2bCommandResult, E2bCommands, E2bSandboxNotFound};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess_e2b::remote::{command_environment, signal_remote_groups, wait_tick};

#[derive(Debug, Default)]
struct FakeCommands {
    requests: Mutex<Vec<(String, BTreeMap<String, String>)>>,
    failures: Mutex<Vec<anyhow::Error>>,
}

#[async_trait::async_trait]
impl E2bCommands for FakeCommands {
    async fn run(
        &self,
        command: &str,
        env: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        self.requests.lock().push((command.to_owned(), env));
        if !self.failures.lock().is_empty() {
            return Err(self.failures.lock().remove(0));
        }
        Ok(E2bCommandResult::default())
    }
}

#[tokio::test]
async fn poll_tick_reports_timer_or_cancellation_winner() {
    assert!(wait_tick(1, None).await);
    let aborted = AbortSignal::default();
    aborted.abort();
    assert!(!wait_tick(10_000, Some(&aborted)).await);
    let later = AbortSignal::default();
    let abort = later.clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        abort.abort();
    });
    assert!(!wait_tick(10_000, Some(&later)).await);
}

#[tokio::test]
async fn group_signalling_is_exact_and_tolerates_shared_quiescence_errors() {
    let commands = Arc::new(FakeCommands::default());
    let environment = BTreeMap::from([("MARKER".to_owned(), "one".to_owned())]);
    signal_remote_groups(commands.as_ref(), &environment, &[41, 42], "TERM")
        .await
        .unwrap();
    {
        let requests = commands.requests.lock();
        assert_eq!(requests[0].0, "kill -TERM -- -41 -42");
        assert_eq!(requests[0].1["MARKER"], "one");
        assert!(requests[0].1["HOME"].starts_with("/.seekdeep-e2b-control-"));
    }

    commands.failures.lock().push(
        E2bCommandExit {
            status: 1,
            stderr: "gone".to_owned(),
        }
        .into(),
    );
    signal_remote_groups(commands.as_ref(), &environment, &[41], "KILL")
        .await
        .unwrap();
    commands.failures.lock().push(
        E2bSandboxNotFound {
            message: "sandbox gone".to_owned(),
        }
        .into(),
    );
    signal_remote_groups(commands.as_ref(), &environment, &[41], "KILL")
        .await
        .unwrap();
    commands
        .failures
        .lock()
        .push(anyhow::anyhow!("network unavailable"));
    assert!(
        signal_remote_groups(commands.as_ref(), &environment, &[41], "KILL")
            .await
            .unwrap_err()
            .to_string()
            .contains("network unavailable")
    );
}

#[test]
fn control_environment_overrides_home_last() {
    let environment = command_environment(&BTreeMap::from([
        ("HOME".to_owned(), "/unsafe".to_owned()),
        ("MARKER".to_owned(), "value".to_owned()),
    ]));
    assert_eq!(environment["MARKER"], "value");
    assert!(environment["HOME"].starts_with("/.seekdeep-e2b-control-"));
}
