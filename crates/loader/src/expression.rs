//! Rust-owned evaluation of source-compatible Loader `!!js` expressions.

use std::{collections::BTreeMap, path::PathBuf};

use boa_engine::{
    Context as JavaScriptContext, JsError, JsNativeError, JsResult, JsString, JsValue,
    NativeFunction, Source, js_string, object::FunctionObjectBuilder, property::PropertyDescriptor,
};
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
        let is_windows = self.platform == "win32";
        let separator = if is_windows { "\\" } else { "/" };
        let separator = serde_json::to_string(separator)?;
        let program = format!(
            r"
(() => {{
  const __nativeFileURLToPath = globalThis.__seekdeep_file_url_to_path__;
  delete globalThis.__seekdeep_file_url_to_path__;
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
    return __nativeFileURLToPath(href, {is_windows}, {platform});
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
            .register_global_builtin_callable(
                js_string!("__seekdeep_file_url_to_path__"),
                3,
                NativeFunction::from_fn_ptr(file_url_to_path),
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        javascript
            .runtime_limits_mut()
            .set_loop_iteration_limit(1_000_000);
        let result = javascript
            .eval(Source::from_bytes(&program))
            .map_err(|error| {
                let message = error
                    .to_opaque(&mut javascript)
                    .to_string(&mut javascript)
                    .map_or_else(|_| error.to_string(), |text| text.to_std_string_escaped());
                anyhow::anyhow!(message)
            })?;
        result
            .to_json(&mut javascript)
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }
}

fn file_url_to_path(
    _this: &JsValue,
    arguments: &[JsValue],
    context: &mut JavaScriptContext,
) -> JsResult<JsValue> {
    let text = arguments
        .first()
        .cloned()
        .unwrap_or_default()
        .to_string(context)?
        .to_std_string_lossy();
    let windows = arguments.get(1).is_some_and(JsValue::to_boolean);
    let platform = arguments
        .get(2)
        .cloned()
        .unwrap_or_default()
        .to_string(context)?
        .to_std_string_lossy();
    let path = match node_file_url_path(&text, windows, &platform) {
        Ok(path) => path,
        Err(error) => return Err(error.into_javascript(context)?),
    };
    Ok(JsString::from(path).into())
}

struct FileUrlError {
    native: JsNativeError,
    code: Option<&'static str>,
    input: Option<String>,
}

impl FileUrlError {
    fn coded(code: &'static str, message: impl Into<String>, input: Option<&str>) -> Self {
        Self {
            native: JsNativeError::typ().with_message(message.into()),
            code: Some(code),
            input: input.map(str::to_owned),
        }
    }

    fn malformed() -> Self {
        Self {
            native: JsNativeError::uri().with_message("URI malformed"),
            code: None,
            input: None,
        }
    }

    fn into_javascript(self, context: &mut JavaScriptContext) -> JsResult<JsError> {
        let object = self.native.to_opaque(context);
        if let Some(code) = self.code {
            object.create_data_property_or_throw(
                js_string!("code"),
                JsString::from(code),
                context,
            )?;
            if code != "ERR_INVALID_URL" {
                let stringify = FunctionObjectBuilder::new(
                    context.realm(),
                    NativeFunction::from_fn_ptr(file_url_error_to_string),
                )
                .name(js_string!("toString"))
                .length(0)
                .build();
                object.define_property_or_throw(
                    js_string!("toString"),
                    PropertyDescriptor::builder()
                        .value(stringify)
                        .writable(true)
                        .enumerable(false)
                        .configurable(true),
                    context,
                )?;
            }
        }
        if let Some(input) = self.input {
            object.create_data_property_or_throw(
                js_string!("input"),
                JsString::from(input),
                context,
            )?;
        }
        Ok(JsError::from_opaque(object.into()))
    }
}

fn file_url_error_to_string(
    this: &JsValue,
    _arguments: &[JsValue],
    context: &mut JavaScriptContext,
) -> JsResult<JsValue> {
    let object = this.as_object().ok_or_else(|| {
        JsNativeError::typ()
            .with_message("Error.prototype.toString called on incompatible receiver")
    })?;
    let name = object
        .get(js_string!("name"), context)?
        .to_string(context)?
        .to_std_string_lossy();
    let code = object
        .get(js_string!("code"), context)?
        .to_string(context)?
        .to_std_string_lossy();
    let message = object
        .get(js_string!("message"), context)?
        .to_string(context)?
        .to_std_string_lossy();
    Ok(JsString::from(format!("{name} [{code}]: {message}")).into())
}

fn node_file_url_path(text: &str, windows: bool, platform: &str) -> Result<String, FileUrlError> {
    let url = url::Url::parse(text)
        .map_err(|_| FileUrlError::coded("ERR_INVALID_URL", "Invalid URL", Some(text)))?;
    if url.scheme() != "file" {
        return Err(FileUrlError::coded(
            "ERR_INVALID_URL_SCHEME",
            "The URL must be of scheme file",
            None,
        ));
    }
    let host = url.host_str().unwrap_or_default();
    if !windows && !host.is_empty() {
        return Err(FileUrlError::coded(
            "ERR_INVALID_FILE_URL_HOST",
            format!("File URL host must be \"localhost\" or empty on {platform}"),
            None,
        ));
    }
    let encoded = url.path();
    let lower = encoded.to_ascii_lowercase();
    if lower.contains("%2f") || windows && lower.contains("%5c") {
        return Err(FileUrlError::coded(
            "ERR_INVALID_FILE_URL_PATH",
            if windows {
                "File URL path must not include encoded \\ or / characters"
            } else {
                "File URL path must not include encoded / characters"
            },
            Some(url.as_str()),
        ));
    }
    for (index, byte) in encoded.bytes().enumerate() {
        if byte == b'%'
            && !encoded
                .as_bytes()
                .get(index + 1..index + 3)
                .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(FileUrlError::malformed());
        }
    }
    let path = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .map_err(|_| FileUrlError::malformed())?;
    if !windows {
        return Ok(path.into_owned());
    }
    let path = path.replace('/', "\\");
    if !host.is_empty() {
        return Ok(format!(r"\\{}{}", idna::domain_to_unicode(host).0, path));
    }
    if !path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
        || path.as_bytes().get(2) != Some(&b':')
    {
        return Err(FileUrlError::coded(
            "ERR_INVALID_FILE_URL_PATH",
            "File URL path must be absolute",
            Some(url.as_str()),
        ));
    }
    Ok(path[1..].to_owned())
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

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        process::{Command, Stdio},
    };

    use super::*;

    #[test]
    fn file_url_paths_and_rejections_match_node_on_both_platform_flavors() {
        let platform = match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            other => other,
        };
        let mut cases = Vec::new();
        let mut actual = Vec::new();
        for windows in [false, true] {
            for text in [
                "file:///tmp/space%20name%23.txt",
                "file:///C:/work/space%20name",
                "file://localhost/C:/work/",
                "file://server/share/name",
                "file://xn--bcher-kva/share/name",
                "file:///C:/work/%F0%9F%98%80",
                "file:///C:/work/name%2Fpart",
                "file:///C:/work/name%5Cpart",
                "file:///relative",
                "file:///C:/%xx",
                "file:///C:/%FF",
                "file:///C:/%00",
                "https://example.test/path",
                "relative",
            ] {
                cases.push(json!({"text":text,"windows":windows}));
                let mut context = JavaScriptContext::default();
                let arguments = [
                    JsString::from(text).into(),
                    JsValue::from(windows),
                    JsString::from(platform).into(),
                ];
                actual.push(
                    match file_url_to_path(&JsValue::undefined(), &arguments, &mut context) {
                        Ok(path) => {
                            json!({"path":path.as_string().unwrap().to_std_string_escaped()})
                        }
                        Err(error) => {
                            let value = error.to_opaque(&mut context);
                            let rendered = value
                                .to_string(&mut context)
                                .unwrap()
                                .to_std_string_escaped();
                            let object = value.as_object().unwrap();
                            let mut fields =
                                Map::from_iter([("error".to_owned(), json!(rendered))]);
                            for key in ["name", "message", "code", "input"] {
                                fields.insert(
                                    key.to_owned(),
                                    object
                                        .get(JsString::from(key), &mut context)
                                        .unwrap()
                                        .as_string()
                                        .map_or(Value::Null, |value| {
                                            json!(value.to_std_string_escaped())
                                        }),
                                );
                            }
                            Value::Object(fields)
                        }
                    },
                );
            }
        }
        let mut node = Command::new("node")
            .args(["--input-type=module", "-e", r"
                import {readFileSync} from 'node:fs';
                import {fileURLToPath} from 'node:url';
                console.log(JSON.stringify(JSON.parse(readFileSync(0, 'utf8')).map(({text, windows}) => {
                    try { return {path:fileURLToPath(text, {windows})}; }
                    catch (error) { return {error:String(error), name:error.name, message:error.message, code:error.code ?? null, input:error.input ?? null}; }
                })));
            "])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        node.stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&cases).unwrap())
            .unwrap();
        let output = node.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(actual, expected);
    }
}
