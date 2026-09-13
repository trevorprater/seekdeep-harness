//! Remote environment transport, scrubbing, and explicit overlay policy.

use std::{collections::BTreeMap, fmt::Write as _};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use indexmap::IndexMap;
use seekdeep_e2b::{E2bCommands, e2b_control_envs};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{SubprocessEnvironment, is_safe_ambient_environment_name};

const ENVIRONMENT_PROBE: &str = concat!(
    "set -o pipefail; ",
    "seekdeep_e2b_passwd=\"$(getent passwd \"$(id -u)\")\"; ",
    "IFS=: read -r _ _ _ _ _ seekdeep_e2b_home _ <<<\"$seekdeep_e2b_passwd\"; ",
    "test -n \"$seekdeep_e2b_home\" -a -d \"$seekdeep_e2b_home\"; ",
    "printf '%s' \"$seekdeep_e2b_home\" | base64 -w 0; printf '\\n'; ",
    "env -0 | base64 -w 0",
);

fn remote_environment_entries(raw: &str) -> impl Iterator<Item = (&str, &str)> {
    raw.split('\0').filter_map(|entry| {
        let separator = entry.find('=')?;
        (separator > 0).then(|| (&entry[..separator], &entry[separator + 1..]))
    })
}

/// Reads the remote login home and complete environment through strict base64 framing.
///
/// The ASCII transport keeps arbitrary SDK callback chunking from splitting
/// UTF-8 code points. The returned NUL-delimited environment always carries
/// the passwd-owned login home.
///
/// # Errors
///
/// Returns command, framing, UTF-8, or remote-home validation failures.
pub async fn read_remote_environment(
    commands: &dyn E2bCommands,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<String> {
    let result = commands
        .run(
            ENVIRONMENT_PROBE,
            e2b_control_envs(&BTreeMap::new()),
            signal,
        )
        .await?;
    let lines = result.stdout.trim().lines().collect::<Vec<_>>();
    anyhow::ensure!(
        lines.len() == 2,
        "subprocess-e2b: remote environment transport returned invalid base64"
    );
    let decoded = lines
        .iter()
        .map(|line| STANDARD.decode(line))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            anyhow::anyhow!("subprocess-e2b: remote environment transport returned invalid base64")
        })?;
    let home = String::from_utf8(decoded[0].clone()).map_err(|error| {
        anyhow::anyhow!("subprocess-e2b: remote environment is not valid UTF-8: {error}")
    })?;
    let raw = String::from_utf8(decoded[1].clone()).map_err(|error| {
        anyhow::anyhow!("subprocess-e2b: remote environment is not valid UTF-8: {error}")
    })?;
    anyhow::ensure!(
        home.starts_with('/') && !home.contains('\0'),
        "subprocess-e2b: remote login home is invalid: {}",
        serde_json::to_string(&home)?
    );
    let mut environment = remote_environment_entries(&raw)
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<IndexMap<_, _>>();
    environment.insert("HOME".to_owned(), home);
    Ok(serialize_entries(&environment))
}

/// Removes harness-private and credential-shaped names from a remote environment.
#[must_use]
pub fn scrub_remote_environment(raw: &str) -> IndexMap<String, String> {
    remote_environment_entries(raw)
        .filter(|(name, _)| is_safe_ambient_environment_name(name))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// Builds login-shell overrides that hide every unsafe ambient name during bootstrap.
#[must_use]
pub fn bootstrap_environment(raw: &str) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::from([("TERM".to_owned(), "dumb".to_owned())]);
    for (name, _) in remote_environment_entries(raw) {
        if !is_safe_ambient_environment_name(name) {
            environment.insert(name.to_owned(), String::new());
        }
    }
    environment
}

/// Applies explicit request entries and serializes one environment for `env -i`.
///
/// `None` is a tombstone for an ambient entry. Explicit credential-shaped or
/// `SEEKDEEP_*` names are retained because the request is the caller's opt-in.
///
/// # Errors
///
/// Rejects empty names, `=`, or NUL framing violations.
pub fn serialize_remote_environment(
    raw: &str,
    explicit: Option<&SubprocessEnvironment>,
) -> anyhow::Result<String> {
    let mut environment = scrub_remote_environment(raw);
    for (name, value) in explicit.into_iter().flatten() {
        anyhow::ensure!(
            !name.is_empty()
                && !name.contains('=')
                && !name.contains('\0')
                && value.as_ref().is_none_or(|value| !value.contains('\0')),
            "subprocess-e2b: environment entries require non-empty NUL-free names without = and NUL-free values"
        );
        if let Some(value) = value {
            environment.insert(name.clone(), value.clone());
        } else {
            environment.shift_remove(name);
        }
    }
    Ok(serialize_entries(&environment))
}

fn serialize_entries(environment: &IndexMap<String, String>) -> String {
    environment
        .iter()
        .fold(String::new(), |mut output, (name, value)| {
            write!(output, "{name}={value}\0").expect("writing to a String is infallible");
            output
        })
}
