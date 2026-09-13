//! Validated configuration for the local shell PTY backend.

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Public plugin configuration after defaults are applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalBashConfig {
    /// Backend registry type.
    pub backend_type: String,
    /// Interactive shell executable.
    pub shell_path: String,
    /// Shell arguments.
    pub shell_args: Vec<String>,
    /// Terminal rows.
    pub rows: f64,
    /// Terminal columns.
    pub cols: f64,
    /// Maximum retained logical lines.
    pub scrollback_lines: f64,
    /// Maximum retained UTF-8 bytes.
    pub scrollback_max_bytes: f64,
    /// Maximum bytes returned by one read or settled viewport.
    pub max_read_bytes: f64,
    /// Readiness polling interval.
    pub poll_interval_ms: f64,
    /// Delay before exact process-state probes.
    pub exact_probe_after_ms: f64,
    /// Silence duration yielding inferred idle.
    pub idle_silence_ms: f64,
    /// Grace for shell foreground handoff after a prompt marker.
    pub handoff_grace_ms: f64,
    /// Absolute send wait bound.
    pub timeout_ms: f64,
    /// Grace before teardown escalates to `SIGKILL`.
    pub dispose_grace_ms: f64,
}

impl Default for TerminalBashConfig {
    fn default() -> Self {
        Self {
            backend_type: "shell".to_owned(),
            shell_path: "/bin/bash".to_owned(),
            shell_args: vec![
                "--noprofile".to_owned(),
                "--norc".to_owned(),
                "-i".to_owned(),
            ],
            rows: 40.0,
            cols: 160.0,
            scrollback_lines: 10_000.0,
            scrollback_max_bytes: 4.0 * 1024.0 * 1024.0,
            max_read_bytes: 256.0 * 1024.0,
            poll_interval_ms: 50.0,
            exact_probe_after_ms: 150.0,
            idle_silence_ms: 3_000.0,
            handoff_grace_ms: 500.0,
            timeout_ms: 30_000.0,
            dispose_grace_ms: 3_000.0,
        }
    }
}

/// Configuration validated and converted to native integral bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTerminalBashConfig {
    /// Backend registry type.
    pub backend_type: String,
    /// Interactive shell executable.
    pub shell_path: String,
    /// Shell arguments.
    pub shell_args: Vec<String>,
    /// Terminal rows.
    pub rows: u32,
    /// Terminal columns.
    pub cols: u32,
    /// Maximum retained logical lines.
    pub scrollback_lines: usize,
    /// Maximum retained UTF-8 bytes.
    pub scrollback_max_bytes: usize,
    /// Maximum bytes returned by one read or settled viewport.
    pub max_read_bytes: usize,
    /// Readiness polling interval.
    pub poll_interval_ms: u64,
    /// Delay before exact process-state probes.
    pub exact_probe_after_ms: u64,
    /// Silence duration yielding inferred idle.
    pub idle_silence_ms: u64,
    /// Grace for shell foreground handoff after a prompt marker.
    pub handoff_grace_ms: u64,
    /// Absolute send wait bound.
    pub timeout_ms: u64,
    /// Grace before teardown escalates to `SIGKILL`.
    pub dispose_grace_ms: u64,
}

/// Source-compatible configuration validation failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct TerminalBashConfigError(String);

impl TerminalBashConfig {
    /// Validates every numeric field and the composed buffer/timing bounds.
    ///
    /// # Errors
    ///
    /// Rejects empty names, non-positive/non-safe-integer numbers, an oversized
    /// read cap, a too-short handoff grace, or native dimension overflow.
    pub fn resolve(&self) -> Result<ResolvedTerminalBashConfig, TerminalBashConfigError> {
        if self.backend_type.is_empty() {
            return Err(config_error("backendType must be non-empty"));
        }
        if self.shell_path.is_empty() {
            return Err(config_error("shellPath must be non-empty"));
        }
        for (name, value) in [
            ("rows", self.rows),
            ("cols", self.cols),
            ("scrollbackLines", self.scrollback_lines),
            ("scrollbackMaxBytes", self.scrollback_max_bytes),
            ("maxReadBytes", self.max_read_bytes),
            ("pollIntervalMs", self.poll_interval_ms),
            ("exactProbeAfterMs", self.exact_probe_after_ms),
            ("idleSilenceMs", self.idle_silence_ms),
            ("handoffGraceMs", self.handoff_grace_ms),
            ("timeoutMs", self.timeout_ms),
            ("disposeGraceMs", self.dispose_grace_ms),
        ] {
            if !value.is_finite()
                || value <= 0.0
                || value.fract() != 0.0
                || value > MAX_SAFE_INTEGER
            {
                return Err(config_error(format!(
                    "{name} must be a positive safe integer"
                )));
            }
        }
        if self.max_read_bytes > self.scrollback_max_bytes {
            return Err(config_error(
                "maxReadBytes must not exceed scrollbackMaxBytes",
            ));
        }
        if self.handoff_grace_ms < self.poll_interval_ms {
            return Err(config_error(
                "handoffGraceMs must be at least pollIntervalMs so one readiness poll runs inside the grace window",
            ));
        }
        let rows = u32::try_from(checked_integer(self.rows))
            .map_err(|_| config_error("rows must fit the terminal dimension range"))?;
        let cols = u32::try_from(checked_integer(self.cols))
            .map_err(|_| config_error("cols must fit the terminal dimension range"))?;
        let scrollback_lines = usize::try_from(checked_integer(self.scrollback_lines))
            .map_err(|_| config_error("scrollbackLines must fit the native size range"))?;
        let scrollback_max_bytes = usize::try_from(checked_integer(self.scrollback_max_bytes))
            .map_err(|_| config_error("scrollbackMaxBytes must fit the native size range"))?;
        let max_read_bytes = usize::try_from(checked_integer(self.max_read_bytes))
            .map_err(|_| config_error("maxReadBytes must fit the native size range"))?;
        Ok(ResolvedTerminalBashConfig {
            backend_type: self.backend_type.clone(),
            shell_path: self.shell_path.clone(),
            shell_args: self.shell_args.clone(),
            rows,
            cols,
            scrollback_lines,
            scrollback_max_bytes,
            max_read_bytes,
            poll_interval_ms: checked_integer(self.poll_interval_ms),
            exact_probe_after_ms: checked_integer(self.exact_probe_after_ms),
            idle_silence_ms: checked_integer(self.idle_silence_ms),
            handoff_grace_ms: checked_integer(self.handoff_grace_ms),
            timeout_ms: checked_integer(self.timeout_ms),
            dispose_grace_ms: checked_integer(self.dispose_grace_ms),
        })
    }
}

fn config_error(message: impl Into<String>) -> TerminalBashConfigError {
    TerminalBashConfigError(format!("terminal-bash: {}", message.into()))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn checked_integer(value: f64) -> u64 {
    // `resolve` proves finite, positive, integral, and no larger than 2^53 - 1.
    value as u64
}
