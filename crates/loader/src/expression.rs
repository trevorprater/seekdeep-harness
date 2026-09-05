//! Rust-owned evaluation of source-compatible Loader `!!js` expressions.

use std::{collections::BTreeMap, path::PathBuf};

use boa_engine::{Context as JavaScriptContext, Source};
use seekdeep_cordis::Context;
use serde_json::{Map, Value, json};

use crate::{LoaderError, profile_patch::ProfileNode};

const EXPRESSION_KEY: &str = "__jsExpr";

/// Immutable process facade visible to one Loader expression generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpressionEnvironment {
    environment: BTreeMap<String, String>,
    cwd: PathBuf,
    executable: PathBuf,
    platform: String,
    version: String,
    seekdeep_home: PathBuf,
}

impl ExpressionEnvironment {
    /// Captures process environment, paths, platform, and version once.
    #[must_use]
    pub fn from_process() -> Self {
        let environment = std::env::vars_os()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let executable = std::env::current_exe().unwrap_or_default();
        let platform = match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            other => other,
        }
        .to_owned();
        let seekdeep_home = seekdeep_util::home_paths::resolve_process_seekdeep_home(None)
            .unwrap_or_else(|_| cwd.join(".seekdeep"));
        Self {
            environment,
            cwd,
            executable,
            platform,
            version: "v0.0.0-seekdeep".to_owned(),
            seekdeep_home,
        }
    }

    /// Constructs a deterministic evaluator facade.
    #[must_use]
    pub fn new(
        environment: BTreeMap<String, String>,
        cwd: PathBuf,
        executable: PathBuf,
        platform: impl Into<String>,
        version: impl Into<String>,
        seekdeep_home: PathBuf,
    ) -> Self {
        Self {
            environment,
            cwd,
            executable,
            platform: platform.into(),
            version: version.into(),
            seekdeep_home,
        }
    }

    /// Builds an evaluator facade from the launcher's frozen environment.
    #[must_use]
    pub fn from_launch_environment(
        environment: &seekdeep_util::launch_environment::LaunchEnvironmentSnapshot,
        cwd: PathBuf,
        executable: PathBuf,
        platform: impl Into<String>,
        version: impl Into<String>,
        seekdeep_home: PathBuf,
    ) -> Self {
        Self::new(
            environment.materialized(),
            cwd,
            executable,
            platform,
            version,
            seekdeep_home,
        )
    }

    pub(crate) fn process_facade(&self) -> Value {
        json!({
            "env": self.environment,
            "cwd": self.cwd.to_string_lossy(),
            "execPath": self.executable.to_string_lossy(),
            "platform": self.platform,
            "version": self.version,
            "seekdeepHome": self.seekdeep_home.to_string_lossy(),
        })
    }

    pub(crate) fn evaluate(
        &self,
        context: &Context,
        expression: &str,
    ) -> anyhow::Result<Option<Value>> {
        let services = context.expression_service_snapshot();
        let scope = serde_json::to_string(&services)?;
        let environment = serde_json::to_string(&self.environment)?;
        let expression = serde_json::to_string(expression)?;
        let cwd = serde_json::to_string(&self.cwd.to_string_lossy())?;
        let executable = serde_json::to_string(&self.executable.to_string_lossy())?;
        let platform = serde_json::to_string(&self.platform)?;
        let version = serde_json::to_string(&self.version)?;
        let home = serde_json::to_string(&self.seekdeep_home.to_string_lossy())?;
        let base_url = serde_json::to_string(
            &context
                .meta("loader.base_url")
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
        )?;
        let separator = if self.platform == "win32" {
            "\\\\"
        } else {
            "/"
        };
        let separator = serde_json::to_string(separator)?;
        let program = format!(
            r"
(() => {{
  const ctx = Object.assign(Object.create(null), {scope});
  Object.defineProperty(ctx, 'get', {{ value: name => ctx[name] }});
  const baseUrl = {base_url};
  function URL(input, base = undefined) {{
    input = String(input);
    const root = String(base ?? baseUrl);
    const href = /^[A-Za-z][A-Za-z0-9+.-]*:/.test(input)
      ? input
      : root.replace(/[^/]*$/, '') + input;
    return Object.freeze({{ href, toString: () => href }});
  }}
  const __jsonParse = JSON.parse.bind(JSON);
  Object.defineProperty(JSON, 'parse', {{ value: input => {{
    try {{ return __jsonParse(input); }}
    catch (error) {{ throw new SyntaxError(`${{error.message}}: ${{String(input)}}`); }}
  }} }});
  const __fileURLToPath = value => {{
    const href = String(value && value.href !== undefined ? value.href : value);
    if (!href.startsWith('file:')) throw new TypeError('The URL must be of scheme file');
    return decodeURIComponent(href.replace(/^file:\/\/(?:localhost)?/, ''));
  }};
  const process = Object.freeze({{
    env: Object.freeze({environment}),
    platform: {platform},
    version: {version},
    execPath: {executable},
    cwd: () => {cwd},
    getBuiltinModule: name => {{
      if (name === 'node:url' || name === 'url') return Object.freeze({{ fileURLToPath: __fileURLToPath }});
      throw new Error(`No such built-in module: ${{name}}`);
    }},
  }});
  const __separator = {separator};
  const __normalizePath = parts => {{
    const prefix = parts[0].startsWith(__separator) ? __separator : '';
    const output = [];
    for (const part of parts.join(__separator).split(/[\\/]+/)) {{
      if (!part || part === '.') continue;
      if (part === '..') output.pop(); else output.push(part);
    }}
    return prefix + output.join(__separator);
  }};
  const seekdeepHomePath = (...segments) => __normalizePath([{home}, ...segments.map(String)]);
  return function() {{ with (ctx) {{ return eval({expression}); }} }}.call(ctx);
}})()
"
        );
        let mut javascript = JavaScriptContext::default();
        javascript
            .runtime_limits_mut()
            .set_loop_iteration_limit(1_000_000);
        let result = javascript
            .eval(Source::from_bytes(&program))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        result
            .to_json(&mut javascript)
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }
}

pub(crate) fn interpolate_config(
    environment: &ExpressionEnvironment,
    context: &Context,
    value: &Value,
) -> anyhow::Result<Value> {
    Ok(interpolate_value(environment, context, value)?.unwrap_or(Value::Null))
}

fn interpolate_value(
    environment: &ExpressionEnvironment,
    context: &Context,
    value: &Value,
) -> anyhow::Result<Option<Value>> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| Ok(interpolate_value(environment, context, value)?.unwrap_or(Value::Null)))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
            .map(Some),
        Value::Object(values) if values.contains_key(EXPRESSION_KEY) => {
            let expression = values
                .get(EXPRESSION_KEY)
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("loader JavaScript expression must be a string"))?;
            environment.evaluate(context, expression)
        }
        Value::Object(values) => values
            .iter()
            .filter_map(
                |(name, value)| match interpolate_value(environment, context, value) {
                    Ok(Some(value)) => Some(Ok((name.clone(), value))),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Map<_, _>, anyhow::Error>>()
            .map(Value::Object)
            .map(Some),
        scalar => Ok(Some(scalar.clone())),
    }
}

pub(crate) fn profile_node_to_raw_json(node: &ProfileNode) -> Result<Value, LoaderError> {
    match node {
        ProfileNode::Null => Ok(Value::Null),
        ProfileNode::Bool(value) => Ok(Value::Bool(*value)),
        ProfileNode::Number(value) => serde_json::to_value(value)
            .map_err(|error| LoaderError::InvalidDocument(error.to_string())),
        ProfileNode::String(value) => Ok(Value::String(value.clone())),
        ProfileNode::Sequence(values) => values
            .iter()
            .map(profile_node_to_raw_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        ProfileNode::Mapping(values) => values
            .iter()
            .map(|(name, value)| Ok((name.clone(), profile_node_to_raw_json(value)?)))
            .collect::<Result<Map<_, _>, LoaderError>>()
            .map(Value::Object),
        ProfileNode::JavaScript(expression) => Ok(json!({
            EXPRESSION_KEY: expression.as_str(),
        })),
    }
}

pub(crate) fn javascript_truthy(value: Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => value,
        Value::Number(value) => value
            .as_f64()
            .is_none_or(|value| value != 0.0 && !value.is_nan()),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
