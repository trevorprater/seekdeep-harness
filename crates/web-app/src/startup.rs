//! Web profile command-line provider.

use std::{io::Write as _, sync::Arc};

use seekdeep_cmdline::{APP_EXIT, CMDLINE_ARGS};
use seekdeep_cordis::{Plugin, ServiceKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable Web startup service name.
pub const WEB_STARTUP_SERVICE: &str = "webStartup";
/// Typed JSON service consumed by Loader config expressions.
pub const WEB_STARTUP: ServiceKey<Value> = ServiceKey::new(WEB_STARTUP_SERVICE);
/// Loader plugin identity.
pub const NAME: &str = "web-startup";
/// Startup requires launcher arguments and its bounded exit capability.
pub const INJECT: &[&str] = &["cmdlineArgs", "appExit"];

const HELP: &str = concat!(
    "Usage: seekdeep --profile web [options]\n\n",
    "Serve the SeekDeep Harness browser UI.\n\n",
    "Options:\n",
    "  -h, --help                         show this help\n",
    "  --host <host>                      bind host\n",
    "  --port <port>                      listen port; pass 0 to let the OS pick a free one\n",
    "  --trusted-host <authority...>      extra authority the /api browser-trust fence accepts\n",
    "                                      (host or host:port; repeatable)\n\n",
    "Examples:\n",
    "  seekdeep --profile web                          serve on the composed host and port\n",
    "  seekdeep --profile web --port 8080              serve on another port\n\n",
);

/// Values published for Web profile config expressions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebStartupValues {
    /// Explicit bind host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Explicit listen port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u64>,
    /// Extra trusted Host authorities in invocation order.
    pub trusted_hosts: Vec<String>,
}

/// Pure Web command-line result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebStartupOutcome {
    /// Valid flags publish these values.
    Values(WebStartupValues),
    /// Help or usage failure requests launcher exit.
    Exit {
        /// Selected process status.
        code: i32,
        /// Standard-output text.
        stdout: String,
        /// Standard-error text.
        stderr: String,
    },
}

fn missing(option: &str) -> WebStartupOutcome {
    WebStartupOutcome::Exit {
        code: 1,
        stdout: String::new(),
        stderr: format!("error: option {option:?} argument missing\n"),
    }
}

/// Parses the Web app's inner flags without observing process-global argv.
#[must_use]
pub fn parse_web_startup(arguments: &[String]) -> WebStartupOutcome {
    let mut values = WebStartupValues::default();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-h" | "--help" => {
                return WebStartupOutcome::Exit {
                    code: 0,
                    stdout: HELP.to_owned(),
                    stderr: String::new(),
                };
            }
            "--host" => {
                index += 1;
                let Some(host) = arguments.get(index) else {
                    return missing("--host <host>");
                };
                if host == "0.0.0.0" {
                    return WebStartupOutcome::Exit {
                        code: 1,
                        stdout: String::new(),
                        stderr: "error: --host 0.0.0.0 is intentionally not supported yet for safety: it would expose remote code execution to the network; use 127.0.0.1 instead\n".to_owned(),
                    };
                }
                values.host = Some(host.clone());
            }
            "--port" => {
                index += 1;
                let Some(port) = arguments.get(index) else {
                    return missing("--port <port>");
                };
                if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                    return WebStartupOutcome::Exit {
                        code: 1,
                        stdout: String::new(),
                        stderr: format!(
                            "error: --port must be a number, got {}\n",
                            serde_json::to_string(port).unwrap_or_else(|_| format!("{port:?}"))
                        ),
                    };
                }
                match port.parse() {
                    Ok(port) => values.port = Some(port),
                    Err(_) => {
                        return WebStartupOutcome::Exit {
                            code: 1,
                            stdout: String::new(),
                            stderr: format!("error: --port must be a number, got {port:?}\n"),
                        };
                    }
                }
            }
            "--trusted-host" => {
                let start = index + 1;
                index = start;
                while index < arguments.len() && !arguments[index].starts_with('-') {
                    values.trusted_hosts.push(arguments[index].clone());
                    index += 1;
                }
                if index == start {
                    return missing("--trusted-host <authority...>");
                }
                index = index.saturating_sub(1);
            }
            argument => {
                return WebStartupOutcome::Exit {
                    code: 1,
                    stdout: String::new(),
                    stderr: format!("error: unknown option {argument:?}\n"),
                };
            }
        }
        index += 1;
    }
    WebStartupOutcome::Values(values)
}

/// Builds the Loader-compatible Web startup plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            let arguments = context
                .get(CMDLINE_ARGS)
                .ok_or_else(|| anyhow::anyhow!("web-startup requires cmdlineArgs"))?;
            match parse_web_startup(arguments.get()) {
                WebStartupOutcome::Values(values) => {
                    context.provide(WEB_STARTUP, Arc::new(serde_json::to_value(values)?))?;
                }
                WebStartupOutcome::Exit {
                    code,
                    stdout,
                    stderr,
                } => {
                    std::io::stdout().lock().write_all(stdout.as_bytes())?;
                    std::io::stderr().lock().write_all(stderr.as_bytes())?;
                    context
                        .get(APP_EXIT)
                        .ok_or_else(|| anyhow::anyhow!("web-startup requires appExit"))?
                        .request(code)?;
                }
            }
            Ok(())
        })
    })
}
