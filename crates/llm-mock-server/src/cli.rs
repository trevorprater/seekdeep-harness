//! Dependency-free parsing for the standalone mock server process.

use std::{collections::BTreeMap, str::FromStr as _};

use crate::{
    MAX_MOCK_LLM_TIMER_DELAY_MS, MockLlmBehavior, MockLlmRandomWeights, MockLlmServerOptions,
};

/// Listener lifecycle behavior available only through the CLI.
pub const CONNECTION_REFUSED_BEHAVIOR: &str = "connection_refused";
const DEFAULT_LISTEN_DELAY_MS: u64 = 750;

/// Complete command usage.
pub const MOCK_LLM_CLI_USAGE: &str = r"Usage: seekdeep-llm-mock-server [options]

Required:
  --sequence <a,b,...>       Ordered behaviors; connection_refused is allowed first

Listener:
  --host <host>              Default 127.0.0.1
  --port <port>              Default 8000; required and nonzero for connection_refused
  --api-key <token>          Validate exact Bearer token when present
  --listen-delay-ms <ms>     Unavailable interval (default 750 with connection_refused)
  --repeat-last              Repeat the final request behavior after exhaustion
  --seed <uint32>            Reproduce random selections
  --random-weights <a=n,...> Relative weights for concrete behaviors

Response:
  --success-text <text>
  --partial-text <text>
  --reasoning-text <text>
  --chunk-size <count>
  --chunk-delay-ms <ms>
  --disconnect-delay-ms <ms>
  --retry-after-ms <ms>
  --request-id <id>
  --tool-name <name>
  --tool-arguments <json>

Other:
  --help
";

/// Parsed run configuration including the pre-listen unavailable interval.
#[derive(Clone, Debug)]
pub struct MockLlmCliConfig {
    /// Server configuration after removing `connection_refused`.
    pub server: MockLlmServerOptions,
    /// Delay before listener bind.
    pub listen_delay_ms: u64,
    /// Whether the source sequence starts unavailable.
    pub starts_unavailable: bool,
}

/// Help or a validated run request.
#[derive(Clone, Debug)]
pub enum MockLlmCliParseResult {
    /// Print usage and exit successfully.
    Help,
    /// Start the configured server.
    Run(Box<MockLlmCliConfig>),
}

const STRING_OPTIONS: &[&str] = &[
    "sequence",
    "host",
    "port",
    "api-key",
    "listen-delay-ms",
    "seed",
    "random-weights",
    "success-text",
    "partial-text",
    "reasoning-text",
    "chunk-size",
    "chunk-delay-ms",
    "disconnect-delay-ms",
    "retry-after-ms",
    "request-id",
    "tool-name",
    "tool-arguments",
];

fn tokenize(argv: &[String]) -> anyhow::Result<(BTreeMap<String, String>, bool)> {
    let mut values = BTreeMap::new();
    let mut repeat_last = false;
    let mut index = 0;
    while index < argv.len() {
        let argument = &argv[index];
        if argument == "--repeat-last" {
            repeat_last = true;
            index += 1;
            continue;
        }
        let Some(option) = argument.strip_prefix("--") else {
            anyhow::bail!("Unexpected argument {argument:?}");
        };
        let (name, inline) = option
            .split_once('=')
            .map_or((option, None), |(name, value)| (name, Some(value)));
        if !STRING_OPTIONS.contains(&name) {
            anyhow::bail!("Unknown option '--{name}'");
        }
        let value = if let Some(value) = inline {
            value.to_owned()
        } else {
            index += 1;
            argv.get(index)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Option '--{name} <value>' argument missing"))?
        };
        values.insert(name.to_owned(), value);
        index += 1;
    }
    Ok((values, repeat_last))
}

fn number_value(option: &str, value: &str) -> anyhow::Result<f64> {
    let parsed = value.parse::<f64>().map_err(|_| {
        anyhow::anyhow!("seekdeep-llm-mock-server: {option} must be a finite number")
    })?;
    anyhow::ensure!(
        parsed.is_finite(),
        "seekdeep-llm-mock-server: {option} must be a finite number"
    );
    Ok(parsed)
}

fn bounded_integer_value(option: &str, value: &str, min: f64, max: f64) -> anyhow::Result<u64> {
    let parsed = number_value(option, value)?;
    anyhow::ensure!(
        parsed.fract() == 0.0 && (min..=max).contains(&parsed),
        "seekdeep-llm-mock-server: {option} must be an integer between {min:.0} and {max:.0}"
    );
    format!("{parsed:.0}").parse().map_err(Into::into)
}

fn parse_sequence(raw: &str) -> anyhow::Result<(bool, Vec<MockLlmBehavior>)> {
    let entries = raw
        .split(',')
        .map(str::trim)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        entries.iter().all(|entry| !entry.is_empty()),
        "seekdeep-llm-mock-server: --sequence must contain non-empty comma-separated behaviors"
    );
    let starts_unavailable = entries
        .first()
        .is_some_and(|entry| entry == CONNECTION_REFUSED_BEHAVIOR);
    anyhow::ensure!(
        !entries
            .iter()
            .skip(1)
            .any(|entry| entry == CONNECTION_REFUSED_BEHAVIOR),
        "seekdeep-llm-mock-server: connection_refused is allowed only as the first behavior"
    );
    let request_entries = if starts_unavailable {
        &entries[1..]
    } else {
        &entries[..]
    };
    anyhow::ensure!(
        !request_entries.is_empty(),
        "seekdeep-llm-mock-server: connection_refused must be followed by a request behavior"
    );
    request_entries
        .iter()
        .map(|entry| {
            MockLlmBehavior::from_str(entry).map_err(|_| {
                anyhow::anyhow!("seekdeep-llm-mock-server: unknown behavior {entry:?}")
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|sequence| (starts_unavailable, sequence))
}

fn parse_random_weights(raw: &str) -> anyhow::Result<MockLlmRandomWeights> {
    let mut weights = MockLlmRandomWeights::new();
    for entry in raw.split(',') {
        let parts = entry.split('=').collect::<Vec<_>>();
        anyhow::ensure!(
            parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty(),
            "seekdeep-llm-mock-server: --random-weights expects behavior=weight comma-separated entries"
        );
        let scripted = MockLlmBehavior::from_str(parts[0]).map_err(|_| {
            anyhow::anyhow!(
                "seekdeep-llm-mock-server: random weight requires a concrete behavior, got {:?}",
                parts[0]
            )
        })?;
        let Some(behavior) = scripted.concrete() else {
            anyhow::bail!(
                "seekdeep-llm-mock-server: random weight requires a concrete behavior, got {:?}",
                parts[0]
            );
        };
        anyhow::ensure!(
            !weights.contains_key(&behavior),
            "seekdeep-llm-mock-server: duplicate random weight for {:?}",
            parts[0]
        );
        weights.insert(behavior, number_value("--random-weights", parts[1])?);
    }
    Ok(weights)
}

/// Parses standalone server arguments without starting a listener.
///
/// # Errors
///
/// Returns tokenizer, numeric-bound, behavior, or cross-option failures.
pub fn parse_mock_llm_cli_args(argv: &[String]) -> anyhow::Result<MockLlmCliParseResult> {
    if argv.iter().any(|argument| argument == "--help") {
        return Ok(MockLlmCliParseResult::Help);
    }
    let (mut values, repeat_last) = tokenize(argv)?;
    let sequence_raw = values
        .remove("sequence")
        .ok_or_else(|| anyhow::anyhow!("seekdeep-llm-mock-server: --sequence is required"))?;
    let (starts_unavailable, sequence) = parse_sequence(&sequence_raw)?;
    let port = values
        .remove("port")
        .map_or(Ok(8_000.0), |value| number_value("--port", &value))?;
    let listen_delay_ms = values
        .remove("listen-delay-ms")
        .map(|value| {
            bounded_integer_value(
                "--listen-delay-ms",
                &value,
                0.0,
                MAX_MOCK_LLM_TIMER_DELAY_MS,
            )
        })
        .transpose()?;
    anyhow::ensure!(
        !starts_unavailable || port != 0.0,
        "seekdeep-llm-mock-server: connection_refused requires an explicit nonzero --port"
    );
    anyhow::ensure!(
        starts_unavailable || listen_delay_ms.is_none(),
        "seekdeep-llm-mock-server: --listen-delay-ms requires connection_refused first in --sequence"
    );
    let random_seed = values
        .remove("seed")
        .map(|value| number_value("--seed", &value))
        .transpose()?;
    let random_weights = values
        .remove("random-weights")
        .map(|value| parse_random_weights(&value))
        .transpose()?;
    anyhow::ensure!(
        sequence.contains(&MockLlmBehavior::Random)
            || (random_seed.is_none() && random_weights.is_none()),
        "seekdeep-llm-mock-server: --seed and --random-weights require random in --sequence"
    );
    let numeric = |values: &mut BTreeMap<String, String>, name: &str| {
        values
            .remove(name)
            .map(|value| number_value(&format!("--{name}"), &value))
            .transpose()
    };
    let server = MockLlmServerOptions {
        host: values.remove("host"),
        port: Some(port),
        api_key: values.remove("api-key"),
        sequence,
        repeat_last,
        random_seed,
        random_weights,
        success_text: values.remove("success-text"),
        partial_text: values.remove("partial-text"),
        reasoning_text: values.remove("reasoning-text"),
        chunk_size: numeric(&mut values, "chunk-size")?,
        chunk_delay_ms: numeric(&mut values, "chunk-delay-ms")?,
        disconnect_delay_ms: numeric(&mut values, "disconnect-delay-ms")?,
        retry_after_ms: numeric(&mut values, "retry-after-ms")?,
        request_id: values.remove("request-id"),
        tool_name: values.remove("tool-name"),
        tool_arguments: values.remove("tool-arguments"),
        on_event: None,
    };
    Ok(MockLlmCliParseResult::Run(Box::new(MockLlmCliConfig {
        server,
        listen_delay_ms: if starts_unavailable {
            listen_delay_ms.unwrap_or(DEFAULT_LISTEN_DELAY_MS)
        } else {
            0
        },
        starts_unavailable,
    })))
}
