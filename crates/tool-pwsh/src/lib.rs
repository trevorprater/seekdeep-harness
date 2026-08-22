//! `PowerShell` tool rendering and background adapters.

use seekdeep_sandbox::SandboxMode;
use seekdeep_shell::{ShellProcessRead, ShellRunResult, ShellSandboxInfo};

pub use seekdeep_tool_bash::process_outcome;

/// Renders one `PowerShell` foreground result using the shared shell marker contract.
#[must_use]
pub fn render_pwsh_result(result: &ShellRunResult, escalation_modes: &[SandboxMode]) -> String {
    seekdeep_tool_bash::render_result(result, escalation_modes)
}

/// Renders one incremental `PowerShell` background-process read.
#[must_use]
pub fn render_pwsh_process_read(
    read: &ShellProcessRead,
    sandbox: Option<&ShellSandboxInfo>,
    escalation_modes: &[SandboxMode],
) -> String {
    seekdeep_tool_bash::render_process_read(read, sandbox, escalation_modes)
}

#[cfg(test)]
mod tests {
    use seekdeep_shell::CollectedOutput;

    use super::*;

    #[test]
    fn pwsh_adapters_share_the_exact_shell_rendering_contract() {
        let result = ShellRunResult {
            exit_code: Some(7),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: 1_000.0,
            stdout: CollectedOutput {
                text: "pwsh".to_owned(),
                ..CollectedOutput::default()
            },
            stderr: CollectedOutput::default(),
            sandbox: None,
        };
        assert_eq!(render_pwsh_result(&result, &[]), "pwsh\n[exit code: 7]");
        assert_eq!(
            render_pwsh_process_read(
                &ShellProcessRead {
                    delta: "tail".to_owned(),
                    lossy: true,
                    ..ShellProcessRead::default()
                },
                None,
                &[],
            ),
            "tail\n[some output was dropped from memory; full output: (unavailable)]"
        );
    }
}
