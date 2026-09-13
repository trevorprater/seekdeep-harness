//! Typert Remote projection of the dynamic Cordis authority.

use std::sync::Arc;

use seekdeep_agent::Agent;
use seekdeep_typert_protocol::{
    RemoteInvocationMarker, RemoteMethodMarker, TypertBoundaryValue, TypertHostArgument,
    TypertInvocableService, TypertInvocationFuture,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    ApprovalRequestId, CordisDynamicPluginId, CordisDynamicPluginRunId, CordisErrorDetails,
    CordisInspectFailureReason, CordisInspectProviderManifest, CordisInspectQueryResolution,
    CordisInspectRequestId, DYNAMIC_CORDIS_RUNNER, DynamicCordisRenderFailure,
    DynamicCordisRunResolution, DynamicCordisRunner,
};

impl TypertInvocableService for DynamicCordisRunner {
    fn service_key(&self) -> &'static str {
        DYNAMIC_CORDIS_RUNNER.name()
    }

    fn namespace(&self) -> &'static str {
        DYNAMIC_CORDIS_RUNNER.name()
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        [
            ("undefine_from_panel", "undefineFromPanel"),
            ("run_host_half_remote", "runHostHalf"),
            ("get_client_code", "getClientCode"),
            ("resolve_request_run", "resolveRequestRun"),
            ("settle_user_run", "settleUserRun"),
            ("stop_from_panel", "stopFromPanel"),
            ("sync_inspect_manifest", "syncInspectManifest"),
            ("resolve_inspect_query", "resolveInspectQuery"),
            ("inventory", "inventory"),
            ("report_render_failure", "reportRenderFailure"),
            ("report_client_guard_failure", "reportClientGuardFailure"),
            ("invoke_remote", "invoke"),
        ]
        .into_iter()
        .map(|(method, exported)| RemoteMethodMarker {
            method: method.to_owned(),
            export_name: (method != exported).then(|| exported.to_owned()),
            invocation: RemoteInvocationMarker::Direct,
        })
        .collect()
    }

    fn parameter_names(&self, implementation: &str) -> Option<Vec<String>> {
        let names: &[&str] = match implementation {
            "undefine_from_panel" | "stop_from_panel" => &["agent", "pluginId"],
            "run_host_half_remote" => &[
                "agent",
                "pluginId",
                "packageId",
                "mode",
                "requestId",
                "approveFutureVersions",
            ],
            "get_client_code" => &["agent", "pluginId", "pluginRunId"],
            "resolve_request_run" => &["requestId", "resolution"],
            "settle_user_run" => &["agent", "pluginId", "resolution"],
            "sync_inspect_manifest" => &["providers"],
            "resolve_inspect_query" => &["agent", "requestId", "resolution"],
            "inventory" => &[],
            "report_render_failure" | "report_client_guard_failure" => {
                &["agent", "pluginId", "pluginRunId", "failure"]
            }
            "invoke_remote" => &["pluginId", "pluginRunId", "method", "args"],
            _ => return None,
        };
        Some(names.iter().map(|name| (*name).to_owned()).collect())
    }

    fn has_method(&self, implementation: &str) -> bool {
        self.parameter_names(implementation).is_some()
    }

    #[allow(clippy::too_many_lines)]
    fn invoke(
        self: Arc<Self>,
        implementation: &str,
        arguments: Vec<TypertHostArgument>,
    ) -> TypertInvocationFuture {
        let implementation = implementation.to_owned();
        Box::pin(async move {
            match implementation.as_str() {
                "undefine_from_panel" => {
                    exact_arity(&arguments, 2, "dynamicCordisRunner/undefineFromPanel")?;
                    let session = agent_session(&arguments, 0)?;
                    let plugin_id = json_argument(&arguments, 1)?;
                    json_boundary(self.undefine_from_panel(&session, &plugin_id).await)
                }
                "run_host_half_remote" => {
                    exact_arity(&arguments, 6, "dynamicCordisRunner/runHostHalf")?;
                    let session = agent_session(&arguments, 0)?;
                    let plugin_id = json_argument(&arguments, 1)?;
                    let package_id = json_argument(&arguments, 2)?;
                    let mode = json_argument(&arguments, 3)?;
                    let request_id: Option<ApprovalRequestId> = json_argument(&arguments, 4)?;
                    let approve_future_versions = json_argument(&arguments, 5)?;
                    let result = if let Some(request_id) = request_id {
                        self.run_host_half_for_request(
                            &session,
                            &plugin_id,
                            &package_id,
                            mode,
                            &request_id,
                            approve_future_versions,
                        )
                        .await
                    } else {
                        self.run_host_half(&session, &plugin_id, &package_id, mode)
                            .await
                    };
                    json_boundary(result)
                }
                "get_client_code" => {
                    exact_arity(&arguments, 3, "dynamicCordisRunner/getClientCode")?;
                    let session = agent_session(&arguments, 0)?;
                    let plugin_id = json_argument(&arguments, 1)?;
                    let plugin_run_id = json_argument(&arguments, 2)?;
                    json_boundary(self.get_client_code(&session, &plugin_id, &plugin_run_id)?)
                }
                "resolve_request_run" => {
                    exact_arity(&arguments, 2, "dynamicCordisRunner/resolveRequestRun")?;
                    let request_id = json_argument(&arguments, 0)?;
                    let resolution = parse_run_resolution(&json_value(&arguments, 1)?)?;
                    json_boundary(self.resolve_request_run(&request_id, &resolution).await)
                }
                "settle_user_run" => {
                    exact_arity(&arguments, 3, "dynamicCordisRunner/settleUserRun")?;
                    let session = agent_session(&arguments, 0)?;
                    let plugin_id = json_argument(&arguments, 1)?;
                    let resolution = parse_run_resolution(&json_value(&arguments, 2)?)?;
                    json_boundary(
                        self.settle_user_run(&session, &plugin_id, &resolution)
                            .await,
                    )
                }
                "stop_from_panel" => {
                    exact_arity(&arguments, 2, "dynamicCordisRunner/stopFromPanel")?;
                    let session = agent_session(&arguments, 0)?;
                    let plugin_id = json_argument(&arguments, 1)?;
                    json_boundary(self.stop_from_panel(&session, &plugin_id).await)
                }
                "sync_inspect_manifest" => {
                    exact_arity(&arguments, 1, "dynamicCordisRunner/syncInspectManifest")?;
                    let providers: Vec<CordisInspectProviderManifest> =
                        json_argument(&arguments, 0)?;
                    self.sync_inspect_manifest(&providers)?;
                    Ok(TypertBoundaryValue::Json(Value::Null))
                }
                "resolve_inspect_query" => {
                    exact_arity(&arguments, 3, "dynamicCordisRunner/resolveInspectQuery")?;
                    let session = agent_session(&arguments, 0)?;
                    let request_id: CordisInspectRequestId = json_argument(&arguments, 1)?;
                    let resolution = parse_inspect_resolution(&json_value(&arguments, 2)?)?;
                    json_boundary(self.resolve_inspect_query(&session, &request_id, resolution))
                }
                "inventory" => {
                    exact_arity(&arguments, 0, "dynamicCordisRunner/inventory")?;
                    json_boundary(self.inventory())
                }
                "report_render_failure" => {
                    exact_arity(&arguments, 4, "dynamicCordisRunner/reportRenderFailure")?;
                    let session = agent_session(&arguments, 0)?;
                    let plugin_id = json_argument(&arguments, 1)?;
                    let plugin_run_id = json_argument(&arguments, 2)?;
                    let failure: DynamicCordisRenderFailure = json_argument(&arguments, 3)?;
                    self.report_render_failure(&session, &plugin_id, &plugin_run_id, &failure);
                    Ok(TypertBoundaryValue::Json(Value::Null))
                }
                "report_client_guard_failure" => {
                    exact_arity(
                        &arguments,
                        4,
                        "dynamicCordisRunner/reportClientGuardFailure",
                    )?;
                    let session = agent_session(&arguments, 0)?;
                    let plugin_id = json_argument(&arguments, 1)?;
                    let plugin_run_id = json_argument(&arguments, 2)?;
                    let failure: CordisErrorDetails = json_argument(&arguments, 3)?;
                    self.report_client_guard_failure(
                        &session,
                        &plugin_id,
                        &plugin_run_id,
                        &failure,
                    );
                    Ok(TypertBoundaryValue::Json(Value::Null))
                }
                "invoke_remote" => {
                    exact_arity(&arguments, 4, "dynamicCordisRunner/invoke")?;
                    let plugin_id: CordisDynamicPluginId = json_argument(&arguments, 0)?;
                    let plugin_run_id: CordisDynamicPluginRunId = json_argument(&arguments, 1)?;
                    let method: String = json_argument(&arguments, 2)?;
                    let args: Value = json_argument(&arguments, 3)?;
                    json_boundary(
                        DynamicCordisRunner::invoke(
                            self.as_ref(),
                            &plugin_id,
                            &plugin_run_id,
                            &method,
                            args,
                        )
                        .await,
                    )
                }
                _ => anyhow::bail!("dynamicCordisRunner has no callable method {implementation:?}"),
            }
        })
    }
}

fn exact_arity(
    arguments: &[TypertHostArgument],
    expected: usize,
    endpoint: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        arguments.len() == expected,
        "{endpoint} expects {expected} argument(s), got {}",
        arguments.len()
    );
    Ok(())
}

fn agent_session(
    arguments: &[TypertHostArgument],
    index: usize,
) -> anyhow::Result<seekdeep_llm::SessionId> {
    let TypertHostArgument::Lookup(agent) = arguments
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("dynamicCordisRunner omitted Agent argument"))?
    else {
        anyhow::bail!("dynamicCordisRunner expected an Agent lookup argument")
    };
    let agent = agent
        .clone()
        .downcast::<Agent>()
        .map_err(|_| anyhow::anyhow!("dynamicCordisRunner lookup argument is not an Agent"))?;
    Ok(agent.id().clone())
}

fn json_argument<T: DeserializeOwned>(
    arguments: &[TypertHostArgument],
    index: usize,
) -> anyhow::Result<T> {
    let TypertHostArgument::Boundary(TypertBoundaryValue::Json(value)) = arguments
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("dynamicCordisRunner omitted argument {index}"))?
    else {
        anyhow::bail!("dynamicCordisRunner argument {index} is not JSON")
    };
    Ok(serde_json::from_value(value.clone())?)
}

fn json_value(arguments: &[TypertHostArgument], index: usize) -> anyhow::Result<Value> {
    let TypertHostArgument::Boundary(TypertBoundaryValue::Json(value)) = arguments
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("dynamicCordisRunner omitted argument {index}"))?
    else {
        anyhow::bail!("dynamicCordisRunner argument {index} is not JSON")
    };
    Ok(value.clone())
}

fn parse_run_resolution(value: &Value) -> anyhow::Result<DynamicCordisRunResolution> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("dynamic Cordis run resolution must be an object"))?;
    let ok = object
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("dynamic Cordis run resolution requires boolean ok"))?;
    if ok {
        return Ok(DynamicCordisRunResolution::Success {
            plugin_run_id: serde_json::from_value(required_value(object, "pluginRunId")?)?,
            waiting_for: object
                .get("waitingFor")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?,
        });
    }
    Ok(DynamicCordisRunResolution::Failure {
        reason: serde_json::from_value(required_value(object, "reason")?)?,
        plugin_run_id: optional_value(object, "pluginRunId")?,
        started_here: optional_value(object, "startedHere")?,
        message: optional_value(object, "message")?,
        stack: optional_value(object, "stack")?,
    })
}

fn parse_inspect_resolution(value: &Value) -> anyhow::Result<CordisInspectQueryResolution> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Cordis inspect resolution must be an object"))?;
    let ok = object
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("Cordis inspect resolution requires boolean ok"))?;
    if ok {
        return Ok(CordisInspectQueryResolution::Success {
            data: required_value(object, "data")?,
        });
    }
    Ok(CordisInspectQueryResolution::Failure {
        reason: serde_json::from_value::<CordisInspectFailureReason>(required_value(
            object, "reason",
        )?)?,
        message: serde_json::from_value(required_value(object, "message")?)?,
    })
}

fn required_value(object: &serde_json::Map<String, Value>, field: &str) -> anyhow::Result<Value> {
    object
        .get(field)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("dynamic Cordis resolution omitted {field:?}"))
}

fn optional_value<T: DeserializeOwned>(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> anyhow::Result<Option<T>> {
    object
        .get(field)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}

fn json_boundary(value: impl Serialize) -> anyhow::Result<TypertBoundaryValue> {
    Ok(TypertBoundaryValue::Json(serde_json::to_value(value)?))
}
