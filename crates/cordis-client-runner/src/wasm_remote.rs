//! Generated-Remote compatibility bindings for the Rust/WASM Client runner.

use std::sync::Arc;

use futures::{FutureExt, future::BoxFuture};
use js_sys::{Function, Promise, Reflect};
use seekdeep_cordis_dynamic_types::{
    ApprovalRequestId, CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    CordisErrorDetails, CordisInspectProviderManifest, CordisInspectQueryResolution,
    CordisInspectRequestId, DynamicCordisClientSource, DynamicCordisHostHalfResult,
    DynamicCordisInvokeErrorCode, DynamicCordisInvokeResult, DynamicCordisResolveAck,
    DynamicCordisRunMode, DynamicCordisRunResolution, DynamicCordisRunResponse,
    DynamicCordisRunSuccessStatus,
};
use seekdeep_identity::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::future_to_promise;

use crate::{
    ClientCordisInspectHost, ClientHostCallError, ClientOrchestratorLog, ClientOrchestratorLogger,
    CordisRunHostSeam, RenderFailureReporter, call_method, host_wire_failure, js_anyhow,
    promise_result, unwrap_host_invoke,
};

/// Folded Remote namespace used by Host-to-Client orchestration.
#[derive(Clone, Debug)]
pub struct WasmCordisRunHost {
    namespace: JsValue,
}

impl WasmCordisRunHost {
    /// Wraps one generated `remote.dynamicCordisRunner` namespace.
    #[must_use]
    pub fn new(namespace: JsValue) -> Self {
        Self { namespace }
    }
}

impl CordisRunHostSeam for WasmCordisRunHost {
    fn run_host_half(
        &self,
        plan: crate::CordisUserRunRequest,
        request_id: Option<ApprovalRequestId>,
        approve_future_versions: bool,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisHostHalfResult>> {
        let namespace = self.namespace.clone();
        async move {
            let value = remote_value(
                namespace,
                "runHostHalf",
                vec![
                    string(plan.agent_id.as_str()),
                    string(plan.plugin_id.as_str()),
                    string(plan.package_id.as_str()),
                    to_js(&plan.mode)?,
                    request_id.map_or(JsValue::NULL, |id| string(id.as_str())),
                    JsValue::from_bool(approve_future_versions),
                ],
            )
            .await?;
            parse_host_half(value)
        }
        .boxed()
    }

    fn get_client_code(
        &self,
        agent_id: SessionId,
        plugin_id: CordisDynamicPluginId,
        plugin_run_id: CordisDynamicPluginRunId,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisClientSource>> {
        let namespace = self.namespace.clone();
        async move {
            let value = remote_value(
                namespace,
                "getClientCode",
                vec![
                    string(agent_id.as_str()),
                    string(plugin_id.as_str()),
                    string(plugin_run_id.as_str()),
                ],
            )
            .await?;
            from_js(value)
        }
        .boxed()
    }

    fn resolve_request_run(
        &self,
        request_id: ApprovalRequestId,
        resolution: DynamicCordisRunResolution,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisResolveAck>> {
        let namespace = self.namespace.clone();
        async move {
            let value = remote_value(
                namespace,
                "resolveRequestRun",
                vec![string(request_id.as_str()), to_js(&resolution)?],
            )
            .await?;
            from_js(value)
        }
        .boxed()
    }

    fn settle_user_run(
        &self,
        agent_id: SessionId,
        plugin_id: CordisDynamicPluginId,
        resolution: DynamicCordisRunResolution,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisRunResponse>> {
        let namespace = self.namespace.clone();
        async move {
            let value = remote_value(
                namespace,
                "settleUserRun",
                vec![
                    string(agent_id.as_str()),
                    string(plugin_id.as_str()),
                    to_js(&resolution)?,
                ],
            )
            .await?;
            parse_run_response(value)
        }
        .boxed()
    }
}

/// Folded Remote namespace used by the Client Inspect registry.
#[derive(Clone, Debug)]
pub struct WasmClientInspectHost {
    namespace: JsValue,
}

impl WasmClientInspectHost {
    /// Wraps one generated namespace.
    #[must_use]
    pub fn new(namespace: JsValue) -> Self {
        Self { namespace }
    }
}

impl ClientCordisInspectHost for WasmClientInspectHost {
    fn sync(
        &self,
        providers: Vec<CordisInspectProviderManifest>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let namespace = self.namespace.clone();
        async move {
            remote_value(namespace, "syncInspectManifest", vec![to_js(&providers)?]).await?;
            Ok(())
        }
        .boxed()
    }

    fn resolve(
        &self,
        session_id: SessionId,
        request_id: CordisInspectRequestId,
        resolution: CordisInspectQueryResolution,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let namespace = self.namespace.clone();
        async move {
            remote_value(
                namespace,
                "resolveInspectQuery",
                vec![
                    string(session_id.as_str()),
                    string(request_id.as_str()),
                    to_js(&resolution)?,
                ],
            )
            .await?;
            Ok(())
        }
        .boxed()
    }
}

/// Builds the exact-run `host.call` callback supplied to the evaluator.
pub fn wasm_host_invoke(namespace: JsValue) -> Function {
    let callback = Closure::wrap(Box::new(
        move |plugin_id: String,
              plugin_run_id: String,
              method: String,
              arguments: JsValue|
              -> Promise {
            let namespace = namespace.clone();
            future_to_promise(async move {
                let plugin_id = CordisDynamicPluginId::new(plugin_id);
                let result = remote_value(
                    namespace,
                    "invoke",
                    vec![
                        string(plugin_id.as_str()),
                        string(&plugin_run_id),
                        string(&method),
                        arguments,
                    ],
                )
                .await
                .map_err(|error| {
                    js_sys::Error::new(&host_wire_failure(&plugin_id, &method, &error.to_string()))
                })?;
                let result = parse_invoke(result).map_err(|error| {
                    js_sys::Error::new(&host_wire_failure(&plugin_id, &method, &error.to_string()))
                })?;
                match unwrap_host_invoke(&plugin_id, &method, result) {
                    Ok(value) => to_js(&value).map_err(|error| -> JsValue {
                        js_sys::Error::new(&host_wire_failure(
                            &plugin_id,
                            &method,
                            &error.to_string(),
                        ))
                        .into()
                    }),
                    Err(error) => Err(host_call_js_error(error)),
                }
            })
        },
    )
        as Box<dyn FnMut(String, String, String, JsValue) -> Promise>);
    callback.into_js_value().unchecked_into()
}

/// Builds the fire-and-forget Client guard failure reporter.
pub fn wasm_guard_reporter(namespace: JsValue) -> Function {
    let callback = Closure::wrap(Box::new(
        move |agent_id: String, plugin_id: String, plugin_run_id: String, failure: JsValue| {
            let namespace = namespace.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(error) = remote_value(
                    namespace,
                    "reportClientGuardFailure",
                    vec![
                        string(&agent_id),
                        string(&plugin_id),
                        string(&plugin_run_id),
                        failure,
                    ],
                )
                .await
                {
                    log_report_failure("guard", &plugin_id, &error.to_string());
                }
            });
        },
    ) as Box<dyn FnMut(String, String, String, JsValue)>);
    callback.into_js_value().unchecked_into()
}

/// Builds the typed fire-and-forget render failure reporter.
#[must_use]
pub fn wasm_render_reporter(namespace: JsValue) -> RenderFailureReporter {
    Arc::new(move |agent_id, plugin_id, plugin_run_id, failure| {
        let namespace = namespace.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let arguments = match to_js(&failure) {
                Ok(failure) => vec![
                    string(agent_id.as_str()),
                    string(plugin_id.as_str()),
                    string(plugin_run_id.as_str()),
                    failure,
                ],
                Err(error) => {
                    log_report_failure("render", plugin_id.as_str(), &error.to_string());
                    return;
                }
            };
            if let Err(error) = remote_value(namespace, "reportRenderFailure", arguments).await {
                log_report_failure("render", plugin_id.as_str(), &error.to_string());
            }
        });
    })
}

/// Browser-console logger matching the source orchestration outlets.
#[must_use]
pub fn wasm_orchestrator_logger() -> ClientOrchestratorLogger {
    Arc::new(|record| match record {
        ClientOrchestratorLog::ClientActivationFailed {
            plugin_id,
            package_id,
            plugin_run_id,
            message,
        } => web_sys::console::error_2(
            &string(&format!(
                "[cordis-client-runner] Client activation {plugin_id}/{package_id} ({plugin_run_id}) failed:"
            )),
            &string(&message),
        ),
        ClientOrchestratorLog::AnswerFailed {
            request_id,
            message,
        } => web_sys::console::error_2(
            &string(&format!(
                "[cordis-client-runner] answering run request {request_id} failed:"
            )),
            &string(&message),
        ),
    })
}

async fn remote_value(
    namespace: JsValue,
    method: &'static str,
    arguments: Vec<JsValue>,
) -> anyhow::Result<JsValue> {
    let returned =
        call_method(&namespace, method, &arguments).map_err(|error| js_anyhow(&error))?;
    let returned = Promise::resolve(&returned);
    let answered = promise_result(&returned)
        .await
        .map_err(|error| js_anyhow(&error))?;
    match Reflect::get(&answered, &string("ok"))
        .map_err(|error| js_anyhow(&error))?
        .as_bool()
    {
        Some(true) => Reflect::get(&answered, &string("value")).map_err(|error| js_anyhow(&error)),
        Some(false) => {
            let error =
                Reflect::get(&answered, &string("error")).map_err(|error| js_anyhow(&error))?;
            let code = Reflect::get(&error, &string("code"))
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_else(|| "remote-error".to_owned());
            let message = Reflect::get(&error, &string("message"))
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_else(|| format!("{error:?}"));
            anyhow::bail!("{code}: {message}")
        }
        _ => anyhow::bail!("dynamicCordisRunner/{method} returned a malformed RemoteResult"),
    }
}

fn host_call_js_error(error: ClientHostCallError) -> JsValue {
    let js_error = js_sys::Error::new(&error.message);
    if let Some(host_stack) = error.host_stack {
        let own_stack = Reflect::get(&js_error, &string("stack"))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_else(|| error.message.clone());
        let _ = Reflect::set(
            &js_error,
            &string("stack"),
            &string(&format!("{own_stack}\nHost stack:\n{host_stack}")),
        );
    }
    js_error.into()
}

fn log_report_failure(kind: &str, plugin_id: &str, failure: &str) {
    web_sys::console::error_2(
        &string(&format!(
            "[cordis-client-runner] reporting a {kind} failure of {plugin_id} failed:"
        )),
        &string(failure),
    );
}

fn string(value: &str) -> JsValue {
    JsValue::from_str(value)
}

fn to_js(value: &impl Serialize) -> anyhow::Result<JsValue> {
    to_js_json(value).map_err(Into::into)
}

pub(crate) fn to_js_json(value: &impl Serialize) -> Result<JsValue, serde_wasm_bindgen::Error> {
    value.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
}

fn from_js<T: for<'de> Deserialize<'de>>(value: JsValue) -> anyhow::Result<T> {
    serde_wasm_bindgen::from_value(value).map_err(Into::into)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostHalfSuccess {
    ok: bool,
    plugin_id: CordisDynamicPluginId,
    package_id: CordisDynamicPackageId,
    plugin_run_id: CordisDynamicPluginRunId,
    waiting_for: Vec<String>,
    started_here: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostHalfFailure {
    ok: bool,
    message: String,
    stack: Option<String>,
}

fn parse_host_half(value: JsValue) -> anyhow::Result<DynamicCordisHostHalfResult> {
    let json: Value = from_js(value)?;
    if json.get("ok").and_then(Value::as_bool) == Some(true) {
        let value: HostHalfSuccess = serde_json::from_value(json)?;
        anyhow::ensure!(value.ok, "Host half success has ok=false");
        Ok(DynamicCordisHostHalfResult::Success {
            plugin_id: value.plugin_id,
            package_id: value.package_id,
            plugin_run_id: value.plugin_run_id,
            waiting_for: value.waiting_for,
            started_here: value.started_here,
        })
    } else {
        let value: HostHalfFailure = serde_json::from_value(json)?;
        anyhow::ensure!(!value.ok, "Host half failure has ok=true");
        Ok(DynamicCordisHostHalfResult::Failure(CordisErrorDetails {
            message: value.message,
            stack: value.stack,
        }))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InvokeSuccess {
    ok: bool,
    value: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InvokeFailure {
    ok: bool,
    code: DynamicCordisInvokeErrorCode,
    message: String,
    stack: Option<String>,
}

fn parse_invoke(value: JsValue) -> anyhow::Result<DynamicCordisInvokeResult> {
    let json: Value = from_js(value)?;
    if json.get("ok").and_then(Value::as_bool) == Some(true) {
        let value: InvokeSuccess = serde_json::from_value(json)?;
        anyhow::ensure!(value.ok, "invoke success has ok=false");
        Ok(DynamicCordisInvokeResult::Success { value: value.value })
    } else {
        let value: InvokeFailure = serde_json::from_value(json)?;
        anyhow::ensure!(!value.ok, "invoke failure has ok=true");
        Ok(DynamicCordisInvokeResult::Failure {
            code: value.code,
            error: CordisErrorDetails {
                message: value.message,
                stack: value.stack,
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunSuccess {
    ok: bool,
    status: DynamicCordisRunSuccessStatus,
    plugin_id: CordisDynamicPluginId,
    package_id: CordisDynamicPackageId,
    plugin_run_id: CordisDynamicPluginRunId,
    waiting_for: Vec<String>,
    client_waiting_for: Option<Vec<String>>,
    current_package_id: Option<CordisDynamicPackageId>,
    next_package_id: Option<CordisDynamicPackageId>,
    mode: DynamicCordisRunMode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunFailure {
    ok: bool,
    reason: seekdeep_cordis_dynamic_types::DynamicCordisRunFailureReason,
    message: String,
    stack: Option<String>,
}

fn parse_run_response(value: JsValue) -> anyhow::Result<DynamicCordisRunResponse> {
    let json: Value = from_js(value)?;
    if json.get("ok").and_then(Value::as_bool) == Some(true) {
        let value: RunSuccess = serde_json::from_value(json)?;
        anyhow::ensure!(value.ok, "run success has ok=false");
        Ok(DynamicCordisRunResponse::Success {
            status: value.status,
            plugin_id: value.plugin_id,
            package_id: value.package_id,
            plugin_run_id: value.plugin_run_id,
            waiting_for: value.waiting_for,
            client_waiting_for: value.client_waiting_for,
            current_package_id: value.current_package_id,
            next_package_id: value.next_package_id,
            mode: value.mode,
        })
    } else {
        let value: RunFailure = serde_json::from_value(json)?;
        anyhow::ensure!(!value.ok, "run failure has ok=true");
        Ok(DynamicCordisRunResponse::Failure {
            reason: value.reason,
            message: value.message,
            stack: value.stack,
        })
    }
}
