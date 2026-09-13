//! Transport and reconnect configuration with eager boundary validation.

use std::collections::BTreeMap;

use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::{Deserialize, Serialize};

const DEFAULT_TOOL_CALL_TIMEOUT_MS: f64 = 60_000.0;
const DEFAULT_STATELESS_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_TIMER_DELAY_MS_INTEGER: u64 = 2_147_483_647;

/// Raw automatic reconnect policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconnectConfig {
    /// Whether a lost generation is replaced automatically.
    pub enabled: Option<bool>,
    /// First delay in milliseconds.
    pub initial_delay_ms: Option<f64>,
    /// Backoff ceiling and stable-uptime reset window in milliseconds.
    pub max_delay_ms: Option<f64>,
    /// Consecutive attempts admitted within one outage.
    pub max_attempts: Option<f64>,
}

/// Fully validated reconnect policy.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedReconnectPolicy {
    /// Whether reconnection is enabled.
    pub enabled: bool,
    /// First delay in milliseconds.
    pub initial_delay_ms: f64,
    /// Backoff ceiling and stability window in milliseconds.
    pub max_delay_ms: f64,
    /// Attempt ceiling.
    pub max_attempts: u64,
}

impl Default for ResolvedReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_delay_ms: 500.0,
            max_delay_ms: 30_000.0,
            max_attempts: 10,
        }
    }
}

/// Resolves defaults and rejects every invalid retry boundary.
///
/// # Errors
///
/// Returns a path-qualified diagnostic for non-finite delays, invalid ordering,
/// or a non-positive integer attempt ceiling.
pub fn resolve_reconnect_policy(
    config: Option<&ReconnectConfig>,
    path: &str,
) -> anyhow::Result<ResolvedReconnectPolicy> {
    let defaults = ResolvedReconnectPolicy::default();
    let enabled = config
        .and_then(|value| value.enabled)
        .unwrap_or(defaults.enabled);
    let initial_delay_ms = config
        .and_then(|value| value.initial_delay_ms)
        .unwrap_or(defaults.initial_delay_ms);
    let max_delay_ms = config
        .and_then(|value| value.max_delay_ms)
        .unwrap_or(defaults.max_delay_ms);
    let max_attempts = config.and_then(|value| value.max_attempts).unwrap_or(10.0);
    anyhow::ensure!(
        initial_delay_ms.is_finite()
            && initial_delay_ms > 0.0
            && initial_delay_ms <= MAX_TIMER_DELAY_MS,
        "{path}.initialDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS_INTEGER}"
    );
    anyhow::ensure!(
        max_delay_ms.is_finite() && max_delay_ms > 0.0 && max_delay_ms <= MAX_TIMER_DELAY_MS,
        "{path}.maxDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS_INTEGER}"
    );
    anyhow::ensure!(
        initial_delay_ms <= max_delay_ms,
        "{path}.initialDelayMs must be less than or equal to maxDelayMs"
    );
    anyhow::ensure!(
        max_attempts.is_finite()
            && max_attempts >= 1.0
            && max_attempts.fract() == 0.0
            && max_attempts <= 9_007_199_254_740_991.0,
        "{path}.maxAttempts must be a positive integer"
    );
    let max_attempts = format!("{max_attempts:.0}")
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("{path}.maxAttempts must be a positive integer"))?;
    Ok(ResolvedReconnectPolicy {
        enabled,
        initial_delay_ms,
        max_delay_ms,
        max_attempts,
    })
}

/// One stdio, sessionful Streamable HTTP, or stateless HTTP MCP endpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Config {
    /// Spawn a child and exchange newline-delimited JSON-RPC over stdio.
    Stdio {
        /// Stable local namespace.
        #[serde(rename = "serverName")]
        server_name: String,
        /// Executable.
        command: String,
        /// Direct arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Explicit environment layered over the scrubbed parent environment.
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Optional child working directory; empty inherits the Host directory.
        #[serde(default)]
        cwd: String,
        /// Per-call timeout.
        #[serde(rename = "toolCallTimeoutMs", default = "default_tool_timeout")]
        tool_call_timeout_ms: f64,
        /// Whether initial failure rejects activation.
        #[serde(rename = "failOnStartupError", default)]
        fail_on_startup_error: bool,
        /// Automatic reconnect policy.
        #[serde(default)]
        reconnect: Option<ReconnectConfig>,
    },
    /// Connect through the sessionful Streamable HTTP protocol.
    StreamableHttp {
        /// Stable local namespace.
        #[serde(rename = "serverName")]
        server_name: String,
        /// MCP endpoint URL.
        url: String,
        /// Extra request headers.
        #[serde(default)]
        headers: BTreeMap<String, String>,
        /// Per-call timeout.
        #[serde(rename = "toolCallTimeoutMs", default = "default_tool_timeout")]
        tool_call_timeout_ms: f64,
        /// Whether initial failure rejects activation.
        #[serde(rename = "failOnStartupError", default)]
        fail_on_startup_error: bool,
        /// Automatic reconnect policy.
        #[serde(default)]
        reconnect: Option<ReconnectConfig>,
    },
    /// Connect through the sessionless 2026-07-28 HTTP protocol.
    StatelessHttp {
        /// Stable local namespace.
        #[serde(rename = "serverName")]
        server_name: String,
        /// MCP endpoint URL.
        url: String,
        /// Extra request headers.
        #[serde(default)]
        headers: BTreeMap<String, String>,
        /// Protocol version stamped into each request metadata envelope.
        #[serde(rename = "protocolVersion", default = "default_stateless_protocol")]
        protocol_version: String,
        /// Per-call timeout.
        #[serde(rename = "toolCallTimeoutMs", default = "default_tool_timeout")]
        tool_call_timeout_ms: f64,
        /// Whether initial failure rejects activation.
        #[serde(rename = "failOnStartupError", default)]
        fail_on_startup_error: bool,
        /// Automatic reconnect policy.
        #[serde(default)]
        reconnect: Option<ReconnectConfig>,
    },
}

impl Config {
    /// Stable server namespace.
    #[must_use]
    pub fn server_name(&self) -> &str {
        match self {
            Self::Stdio { server_name, .. }
            | Self::StreamableHttp { server_name, .. }
            | Self::StatelessHttp { server_name, .. } => server_name,
        }
    }

    /// Per-tool-call timeout.
    #[must_use]
    pub fn tool_call_timeout_ms(&self) -> f64 {
        match self {
            Self::Stdio {
                tool_call_timeout_ms,
                ..
            }
            | Self::StreamableHttp {
                tool_call_timeout_ms,
                ..
            }
            | Self::StatelessHttp {
                tool_call_timeout_ms,
                ..
            } => *tool_call_timeout_ms,
        }
    }

    /// Startup failure policy.
    #[must_use]
    pub fn fail_on_startup_error(&self) -> bool {
        match self {
            Self::Stdio {
                fail_on_startup_error,
                ..
            }
            | Self::StreamableHttp {
                fail_on_startup_error,
                ..
            }
            | Self::StatelessHttp {
                fail_on_startup_error,
                ..
            } => *fail_on_startup_error,
        }
    }

    /// Raw reconnect block.
    #[must_use]
    pub fn reconnect(&self) -> Option<&ReconnectConfig> {
        match self {
            Self::Stdio { reconnect, .. }
            | Self::StreamableHttp { reconnect, .. }
            | Self::StatelessHttp { reconnect, .. } => reconnect.as_ref(),
        }
    }

    /// Validates identity, URL, command, timeout, and reconnect policy.
    ///
    /// # Errors
    ///
    /// Returns the first eagerly resolvable configuration failure.
    pub fn validate(&self) -> anyhow::Result<ResolvedReconnectPolicy> {
        let server_name = self.server_name();
        anyhow::ensure!(
            !server_name.is_empty()
                && server_name.len() <= 32
                && server_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "mcp-client: serverName must match [A-Za-z0-9_-]{{1,32}}"
        );
        anyhow::ensure!(
            self.tool_call_timeout_ms().is_finite()
                && self.tool_call_timeout_ms() > 0.0
                && self.tool_call_timeout_ms() <= MAX_TIMER_DELAY_MS,
            "mcp-client({server_name}): toolCallTimeoutMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS_INTEGER}"
        );
        match self {
            Self::Stdio { command, .. } => {
                anyhow::ensure!(
                    !command.is_empty(),
                    "mcp-client({server_name}): command is required"
                );
            }
            Self::StreamableHttp { url, .. } | Self::StatelessHttp { url, .. } => {
                url::Url::parse(url).map_err(|error| {
                    anyhow::anyhow!("mcp-client({server_name}): invalid url: {error}")
                })?;
            }
        }
        resolve_reconnect_policy(
            self.reconnect(),
            &format!("mcp-client({server_name}): reconnect"),
        )
    }

    /// Validates and writes every reconnect default into the public config value.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::validate`].
    pub fn normalized(mut self) -> anyhow::Result<Self> {
        let policy = self.validate()?;
        let max_attempts = serde_json::to_value(policy.max_attempts)?
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("maxAttempts could not be represented as a number"))?;
        let reconnect = Some(ReconnectConfig {
            enabled: Some(policy.enabled),
            initial_delay_ms: Some(policy.initial_delay_ms),
            max_delay_ms: Some(policy.max_delay_ms),
            max_attempts: Some(max_attempts),
        });
        match &mut self {
            Self::Stdio {
                reconnect: current, ..
            }
            | Self::StreamableHttp {
                reconnect: current, ..
            }
            | Self::StatelessHttp {
                reconnect: current, ..
            } => *current = reconnect,
        }
        Ok(self)
    }
}

fn default_tool_timeout() -> f64 {
    DEFAULT_TOOL_CALL_TIMEOUT_MS
}

fn default_stateless_protocol() -> String {
    DEFAULT_STATELESS_PROTOCOL_VERSION.to_owned()
}
