//! Bash tool adapters shared by foreground and background execution.

use seekdeep_jobs::{JobOutcome, JobTerminalStatus};
use seekdeep_sandbox::{SandboxMode, escalation_hint_marker, sandbox_denial_marker};
use seekdeep_shell::{
    CollectedOutput, ShellProcess, ShellProcessRead, ShellProcessStatus, ShellRunResult,
    ShellSandboxInfo,
};

pub use seekdeep_shell::{ParsedExitStatus, parse_exit_status};

fn stream_text(output: &CollectedOutput) -> String {
    if !output.truncated {
        return output.text.clone();
    }
    format!(
        "{}\n[output truncated; full output: {}]",
        output.text,
        output.spill_path.as_ref().map_or_else(
            || "(unavailable)".to_owned(),
            |path| path.to_string_lossy().into_owned()
        )
    )
}

/// Renders one finished foreground run into model-facing text.
#[must_use]
pub fn render_result(result: &ShellRunResult, escalation_modes: &[SandboxMode]) -> String {
    let out = stream_text(&result.stdout);
    let err = stream_text(&result.stderr);
    let mut body = out;
    if !err.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&err);
    }
    if body.is_empty() {
        body.push_str("(no output)");
    }

    let mut markers = Vec::new();
    if let Some(sandbox) = result.sandbox.as_ref().filter(|sandbox| sandbox.denied) {
        markers.push(sandbox_denial_marker(sandbox.mode));
        if !escalation_modes.is_empty() {
            markers.push(escalation_hint_marker("command"));
        }
    }
    if result.timed_out {
        markers.push(format!("[timed out after {}ms]", result.timeout_ms));
    }
    if let Some(signal) = &result.signal {
        markers.push(format!("[killed by signal: {}]", signal.as_str()));
    } else if result.exit_code != Some(0) {
        markers.push(format!(
            "[exit code: {}]",
            result
                .exit_code
                .map_or_else(|| "null".to_owned(), |code| code.to_string())
        ));
    }
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&markers.join("\n"));
    body
}

/// Renders one incremental background-process read and its settled notices.
#[must_use]
pub fn render_process_read(
    read: &ShellProcessRead,
    sandbox: Option<&ShellSandboxInfo>,
    escalation_modes: &[SandboxMode],
) -> String {
    let mut notices = Vec::new();
    if read.lossy {
        let paths = [
            read.stdout_spill_path.as_ref(),
            read.stderr_spill_path.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
        notices.push(format!(
            "[some output was dropped from memory; full output: {}]",
            if paths.is_empty() {
                "(unavailable)".to_owned()
            } else {
                paths.join(", ")
            }
        ));
    }
    if let Some(sandbox) = sandbox.filter(|sandbox| sandbox.runner_failed == Some(true)) {
        notices.push(format!(
            "[sandbox: the sandbox runner itself failed under {} mode — the command did not run; this is a sandbox problem, not a command failure]",
            sandbox.mode
        ));
    } else if let Some(sandbox) = sandbox.filter(|sandbox| sandbox.denied) {
        notices.push(sandbox_denial_marker(sandbox.mode));
        if !escalation_modes.is_empty() {
            notices.push(escalation_hint_marker("command"));
        }
    }
    if notices.is_empty() {
        return read.delta.clone();
    }
    let separator = if !read.delta.is_empty() && !read.delta.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{}{separator}{}", read.delta, notices.join("\n"))
}

/// Maps a settled background process onto the generic job outcome vocabulary.
///
/// A nonzero command exit is completed, not failed, matching foreground shell
/// rendering. Infrastructure failures remain represented by the Shell handle's
/// current signal-less killed state until that provider contract is widened.
#[must_use]
pub fn process_outcome(process: &dyn ShellProcess) -> JobOutcome {
    if process.status() == ShellProcessStatus::Killed {
        return JobOutcome {
            status: JobTerminalStatus::Killed,
            detail: Some(process.signal().map_or_else(
                || "killed before exit".to_owned(),
                |signal| format!("signal: {}", signal.as_str()),
            )),
            output: None,
        };
    }
    JobOutcome {
        status: JobTerminalStatus::Completed,
        detail: Some(format!("exit code: {}", process.exit_code().unwrap_or(0))),
        output: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use seekdeep_shell::{
        CollectedOutput, ProcessSignal, ShellProcessRead, ShellRunResult, ShellSandboxInfo,
    };

    use super::*;

    #[derive(Debug)]
    struct Process {
        status: ShellProcessStatus,
        exit_code: Option<i32>,
        signal: Option<ProcessSignal>,
    }

    #[async_trait::async_trait]
    impl ShellProcess for Process {
        fn status(&self) -> ShellProcessStatus {
            self.status
        }

        fn exit_code(&self) -> Option<i32> {
            self.exit_code
        }

        fn signal(&self) -> Option<ProcessSignal> {
            self.signal.clone()
        }

        fn sandbox(&self) -> Option<ShellSandboxInfo> {
            None
        }

        async fn done(&self) {}

        fn read_output(&self) -> ShellProcessRead {
            ShellProcessRead::default()
        }

        fn kill(&self) -> bool {
            false
        }
    }

    fn process(
        status: ShellProcessStatus,
        exit_code: Option<i32>,
        signal: Option<&str>,
    ) -> Process {
        Process {
            status,
            exit_code,
            signal: signal.map(ProcessSignal::new),
        }
    }

    #[test]
    fn background_process_outcomes_match_job_status_and_detail_vocabulary() {
        for (process, expected) in [
            (
                process(ShellProcessStatus::Killed, None, Some("SIGKILL")),
                JobOutcome {
                    status: JobTerminalStatus::Killed,
                    detail: Some("signal: SIGKILL".to_owned()),
                    output: None,
                },
            ),
            (
                process(ShellProcessStatus::Killed, None, None),
                JobOutcome {
                    status: JobTerminalStatus::Killed,
                    detail: Some("killed before exit".to_owned()),
                    output: None,
                },
            ),
            (
                process(ShellProcessStatus::Completed, Some(7), None),
                JobOutcome {
                    status: JobTerminalStatus::Completed,
                    detail: Some("exit code: 7".to_owned()),
                    output: None,
                },
            ),
            (
                process(ShellProcessStatus::Running, None, None),
                JobOutcome {
                    status: JobTerminalStatus::Completed,
                    detail: Some("exit code: 0".to_owned()),
                    output: None,
                },
            ),
        ] {
            assert_eq!(process_outcome(&process), expected);
        }
    }

    fn run() -> ShellRunResult {
        ShellRunResult {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: 1_000,
            stdout: CollectedOutput::default(),
            stderr: CollectedOutput::default(),
            sandbox: None,
        }
    }

    fn sandbox(mode: SandboxMode, denied: bool, runner_failed: Option<bool>) -> ShellSandboxInfo {
        ShellSandboxInfo {
            mode,
            denied,
            enforcement: None,
            runner_failed,
        }
    }

    #[test]
    fn foreground_rendering_preserves_sections_markers_and_round_trip_order() {
        let mut result = run();
        result.stderr.text = "err\n".to_owned();
        assert_eq!(render_result(&result, &[]), "[stderr]\nerr\n");
        result.stdout.text = "out".to_owned();
        result.stderr.text = "err".to_owned();
        assert_eq!(render_result(&result, &[]), "out\n[stderr]\nerr");

        let mut exited = run();
        exited.exit_code = Some(7);
        exited.stdout.text = "x".to_owned();
        assert_eq!(render_result(&exited, &[]), "x\n[exit code: 7]");
        assert_eq!(
            parse_exit_status(&render_result(&exited, &[])),
            ParsedExitStatus::Exit {
                body: "x".to_owned(),
                exit_code: 7.0,
            }
        );

        let mut killed = run();
        killed.exit_code = None;
        killed.signal = Some(ProcessSignal::new("SIGTERM"));
        killed.timed_out = true;
        assert_eq!(
            render_result(&killed, &[]),
            "(no output)\n[timed out after 1000ms]\n[killed by signal: SIGTERM]"
        );
        assert_eq!(
            parse_exit_status(&render_result(&killed, &[])),
            ParsedExitStatus::Signal {
                body: "(no output)\n[timed out after 1000ms]".to_owned(),
                signal: "SIGTERM".to_owned(),
            }
        );

        let mut truncated = run();
        truncated.stdout = CollectedOutput {
            text: "tail".to_owned(),
            truncated: true,
            spill_path: None,
        };
        assert_eq!(
            render_result(&truncated, &[]),
            "tail\n[output truncated; full output: (unavailable)]"
        );

        let mut denied = run();
        denied.exit_code = Some(1);
        denied.stderr.text = "denied".to_owned();
        denied.sandbox = Some(sandbox(SandboxMode::ReadOnly, true, None));
        let without_escalation = render_result(&denied, &[]);
        assert!(
            without_escalation
                .ends_with("[sandbox: file access denied under read-only mode]\n[exit code: 1]")
        );
        assert!(
            render_result(&denied, &[SandboxMode::WorkspaceWrite])
                .contains("[sandbox: escalation available")
        );
    }

    #[test]
    fn background_read_rendering_preserves_loss_and_sandbox_notices() {
        let base = ShellProcessRead {
            delta: "out\n".to_owned(),
            lossy: false,
            stdout_spill_path: None,
            stderr_spill_path: None,
        };
        assert_eq!(render_process_read(&base, None, &[]), "out\n");
        let lossy = ShellProcessRead {
            lossy: true,
            stdout_spill_path: Some(PathBuf::from("/spill/out.log")),
            stderr_spill_path: Some(PathBuf::from("/spill/err.log")),
            ..base.clone()
        };
        assert_eq!(
            render_process_read(&lossy, None, &[]),
            "out\n[some output was dropped from memory; full output: /spill/out.log, /spill/err.log]"
        );
        let unavailable = ShellProcessRead {
            delta: "tail".to_owned(),
            lossy: true,
            stdout_spill_path: None,
            stderr_spill_path: None,
        };
        assert_eq!(
            render_process_read(&unavailable, None, &[]),
            "tail\n[some output was dropped from memory; full output: (unavailable)]"
        );
        assert_eq!(
            render_process_read(
                &base,
                Some(&sandbox(SandboxMode::ReadOnly, true, None)),
                &[]
            ),
            "out\n[sandbox: file access denied under read-only mode]"
        );
        let runner = render_process_read(
            &ShellProcessRead::default(),
            Some(&sandbox(SandboxMode::WorkspaceWrite, true, Some(true))),
            &[SandboxMode::DangerFullAccess],
        );
        assert!(runner.contains("sandbox runner itself failed under workspace-write mode"));
        assert!(!runner.contains("file access denied"));
    }
}
