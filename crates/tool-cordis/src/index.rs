//! Seven model-facing Cordis inspection and lifecycle tools.

use std::sync::{Arc, OnceLock};

use seekdeep_agent::{AgentEvent, PreStepDecision};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_cordis_host_runner::{
    CORDIS_INSPECT, CordisDynamicPackageId, CordisDynamicPluginId, CordisInspectPlatform,
    DYNAMIC_CORDIS_RUNNER, DynamicCordisCode, DynamicCordisDefineRequest,
    DynamicCordisPluginSelector, DynamicCordisRunMode, DynamicCordisRunResponse,
    DynamicCordisStopFailureReason, DynamicCordisStopResponse, DynamicCordisUndefineReceipt,
};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    TOOLS, ToolArgsError, ToolOutputDefinition, ToolRunContext, assert_supported_json_schema,
    validate_json_schema_value_at,
};
use serde_json::{Value, json};

use crate::{
    cordis_system_prompt,
    present::{
        define_call, inspect_list_call, inspect_query_call, inspect_self_call, run_call, stop_call,
        undefine_call,
    },
    providers::host_inspect_providers,
};

/// Cordis plugin name.
pub const NAME: &str = "tool-cordis";
/// Required services.
pub const INJECT: &[&str] = &[
    "tools",
    "systemPrompt",
    "dynamicCordisRunner",
    "cordisInspect",
];

const DEFINITIONS: &str = include_str!("../data/tool-definitions.json");

fn definitions() -> &'static Value {
    static DEFINITIONS_VALUE: OnceLock<Value> = OnceLock::new();
    DEFINITIONS_VALUE.get_or_init(|| {
        serde_json::from_str(DEFINITIONS).expect("generated tool-cordis definitions must be valid")
    })
}

fn metadata(name: &str) -> &'static Value {
    definitions()["definitions"]
        .as_array()
        .expect("definitions array")
        .iter()
        .find(|definition| definition["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("missing generated Cordis tool {name}"))
}

/// Registers the model prompt, Host providers, tools, and reference injection.
///
/// # Errors
///
/// Returns missing services, provider conflicts, prompt, tool, or listener failures.
pub fn apply(context: &Context) -> anyhow::Result<()> {
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-cordis requires systemPrompt"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-cordis requires tools"))?;
    let runner = context
        .get(DYNAMIC_CORDIS_RUNNER)
        .ok_or_else(|| anyhow::anyhow!("tool-cordis requires dynamicCordisRunner"))?;
    let inspect = context
        .get(CORDIS_INSPECT)
        .ok_or_else(|| anyhow::anyhow!("tool-cordis requires cordisInspect"))?;
    prompt.section(
        context,
        PromptSection::new(
            "tool:cordis",
            115.0,
            PromptText::Static(cordis_system_prompt().to_owned()),
        ),
    )?;
    for provider in host_inspect_providers(context) {
        inspect.register(context, provider)?;
    }
    for name in [
        "cordis_inspect_list",
        "cordis_inspect_query",
        "cordis_inspect_self",
        "cordis_define",
        "cordis_run",
        "cordis_stop",
        "cordis_undefine",
    ] {
        tools.register(
            context,
            tool_definition(context, runner.clone(), inspect.clone(), name)?,
        )?;
    }
    install_reference_injection(context, runner)?;
    Ok(())
}

fn install_reference_injection(
    context: &Context,
    runner: Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
) -> anyhow::Result<()> {
    context.events().on_waterfall(
        context,
        "agent/pre-step",
        move |_, args, next| {
            let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                return Box::pin(async {
                    Err(anyhow::anyhow!("agent/pre-step lacks its payload"))
                });
            };
            let runner = runner.clone();
            Box::pin(async move {
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PreStepDecision>()
                    .map(|decision| (*decision).clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!("agent/pre-step returned an invalid decision")
                    })?;
                let PreStepDecision::Enter { mut messages } = decision else {
                    return Ok(EventReply::Value(Arc::new(decision)));
                };
                let ids = referenced_plugin_ids(&event.payload.messages);
                if ids.is_empty() {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages,
                    })));
                }
                anyhow::ensure!(
                    !event.payload.signal.is_aborted(),
                    "tool-cordis reference injection aborted"
                );
                messages.extend(ids.into_iter().map(|id| {
                    let text = runner
                        .reference(event.agent.id(), &CordisDynamicPluginId::new(&id))
                        .map_or_else(
                            || render_unavailable_reference(&id),
                            |reference| render_reference(&reference),
                        );
                    let mut source = MessageSource::plugin(NAME);
                    source
                        .fields
                        .insert("form".to_owned(), json!("instructions"));
                    UserMessage::new(vec![ContentBlock::Text { text }], source)
                }));
                Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                    messages,
                })))
            })
        },
        EventOptions::default(),
    )?;
    Ok(())
}

fn referenced_plugin_ids(messages: &[UserMessage]) -> Vec<String> {
    let mut ids = Vec::new();
    for message in messages {
        if message.source().kind != "user" {
            continue;
        }
        let text = message
            .content()
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        for token in text.split_whitespace() {
            let Some(id) = token.strip_prefix('@') else {
                continue;
            };
            let Some((prefix, suffix)) = id.rsplit_once('-') else {
                continue;
            };
            if (3..=6).contains(&prefix.len())
                && prefix.bytes().all(|byte| byte.is_ascii_lowercase())
                && !suffix.is_empty()
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
                && !ids.iter().any(|known| known == id)
            {
                ids.push(id.to_owned());
            }
        }
    }
    ids
}

fn render_reference(reference: &seekdeep_cordis_host_runner::DynamicCordisReference) -> String {
    let mode = if reference.current_package_id.is_some() {
        "update"
    } else {
        "run"
    };
    let summary = json!({
        "pluginId":reference.plugin_id,
        "packageId":reference.package_id,
        "name":reference.name,
        "purpose":reference.purpose,
        "currentPackageId":reference.current_package_id,
        "nextPackageId":reference.next_package_id,
    });
    [
        "<cordis_dynamic_plugin_context>".to_owned(),
        serde_json::to_string_pretty(&summary).expect("JSON summary"),
        String::new(),
        format!("The user explicitly referenced @{}. Use Package {} as the base for this modification.", reference.plugin_id, reference.package_id),
        format!("Before modifying it, call cordis_inspect_self with pluginId=\"{}\" and packageId=\"{}\" to read the exact metadata and source.", reference.plugin_id, reference.package_id),
        format!("Use cordis_define with plugin.kind=\"existing\" and the original pluginId=\"{}\" to append an immutable Package.", reference.plugin_id),
        format!("Do not create a new Plugin for this request. After cordis_define succeeds, call cordis_run mode=\"{mode}\" with the returned packageId."),
        "</cordis_dynamic_plugin_context>".to_owned(),
    ]
    .join("\n")
}

fn render_unavailable_reference(id: &str) -> String {
    [
        "<cordis_dynamic_plugin_context>".to_owned(),
        format!("The user explicitly referenced @{id}, but this Plugin is unavailable in the current Session."),
        "It may have been removed, belong to another Session, or have been lost when the SeekDeep Harness process restarted.".to_owned(),
        "Do not claim that it was updated or silently create a replacement Plugin. Tell the user that the reference is currently unavailable.".to_owned(),
        "</cordis_dynamic_plugin_context>".to_owned(),
    ]
    .join("\n")
}

fn tool_definition(
    context: &Context,
    runner: Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
    inspect: Arc<seekdeep_cordis_host_runner::CordisInspectRegistryService>,
    name: &'static str,
) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let metadata = metadata(name);
    let description = metadata["description"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let parameters = metadata["parameters"].clone();
    let output_schema = metadata["outputSchema"].clone();
    let execution_context = context.clone();
    let parameter_schema = Arc::new(assert_supported_json_schema(parameters.clone())?);
    let output = ToolOutputDefinition::new(
        Arc::new(assert_supported_json_schema(output_schema)?),
        Arc::new(move |_args: &Value, value: &Value| {
            Ok(vec![ContentBlock::Text {
                text: render_value(name, value)?,
            }])
        }),
    );
    let Value::Object(parameter_map) = parameters else {
        anyhow::bail!("generated Cordis parameters must be an object")
    };
    let execution_schema = parameter_schema.clone();
    let mut definition = seekdeep_tools::ToolDefinition::new(
        name,
        description,
        parameter_map,
        output,
        Arc::new(move |args: Value, execution| {
            let violations = validate_json_schema_value_at(&execution_schema, &args, "");
            if !violations.is_empty() {
                return Box::pin(
                    async move { Err(anyhow::Error::new(ToolArgsError::new(violations))) },
                );
            }
            let context = execution_context.clone();
            let runner = runner.clone();
            let inspect = inspect.clone();
            Box::pin(
                async move { execute(&context, &runner, &inspect, name, args, &execution).await },
            )
        }),
    );
    let presentation_schema = parameter_schema;
    definition.present_call = Some(Arc::new(move |args: &Value| {
        validate_json_schema_value_at(&presentation_schema, args, "")
            .is_empty()
            .then(|| present_call(name, args))
            .flatten()
    }));
    Ok(definition)
}

async fn execute(
    _context: &Context,
    runner: &Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
    inspect: &Arc<seekdeep_cordis_host_runner::CordisInspectRegistryService>,
    name: &str,
    args: Value,
    execution: &ToolRunContext,
) -> anyhow::Result<Value> {
    match name {
        "cordis_inspect_list" => Ok(json!({"providers": inspect.list()})),
        "cordis_inspect_query" => {
            let session = require_session(execution)?;
            let platform: CordisInspectPlatform = serde_json::from_value(args["platform"].clone())?;
            let provider = required_string(&args, "provider")?;
            let method = required_string(&args, "method")?;
            let input = args.get("input").cloned();
            let data = inspect
                .query(
                    platform,
                    provider,
                    method,
                    input,
                    session,
                    execution.signal(),
                )
                .await?;
            Ok(json!({
                "platform": platform,
                "provider":provider,
                "method":method,
                "data":data,
            }))
        }
        "cordis_inspect_self" => inspect_self(runner, execution, &args),
        "cordis_define" => define_package(runner, execution, &args),
        "cordis_run" => run_package(runner, execution, &args).await,
        "cordis_stop" => stop_plugin(runner, execution, &args).await,
        "cordis_undefine" => undefine_plugin(runner, execution, &args).await,
        _ => anyhow::bail!("unknown Cordis tool {name}"),
    }
}

fn require_session(execution: &ToolRunContext) -> anyhow::Result<&seekdeep_llm::SessionId> {
    execution
        .agent
        .as_ref()
        .map(|agent| agent.id())
        .ok_or_else(|| anyhow::anyhow!("Cordis dynamic tools require an Agent-backed session"))
}

fn required_string<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("expected JSON string field \"{key}\""))
}

fn define_package(
    runner: &Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
    execution: &ToolRunContext,
    args: &Value,
) -> anyhow::Result<Value> {
    let session = require_session(execution)?.clone();
    let plugin = match args.pointer("/plugin/kind").and_then(Value::as_str) {
        Some("new") => DynamicCordisPluginSelector::New {
            id_prefix: args
                .pointer("/plugin/idPrefix")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        Some("existing") => DynamicCordisPluginSelector::Existing {
            plugin_id: CordisDynamicPluginId::new(
                args.pointer("/plugin/pluginId")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
        },
        _ => anyhow::bail!("invalid Cordis plugin selector"),
    };
    let receipt = runner.define(DynamicCordisDefineRequest {
        session_id: session,
        plugin,
        name: required_string(args, "name")?.to_owned(),
        purpose: required_string(args, "purpose")?.to_owned(),
        code: DynamicCordisCode {
            host: args
                .pointer("/code/host")
                .and_then(Value::as_str)
                .map(str::to_owned),
            client: args
                .pointer("/code/client")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
    })?;
    Ok(json!({
        "pluginId": receipt.plugin_id,
        "packageId": receipt.package_id,
        "name":receipt.name,
        "purpose":receipt.purpose,
        "hasHostHalf":receipt.has_host_half,
        "hasClientHalf":receipt.has_client_half,
    }))
}

async fn run_package(
    runner: &Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
    execution: &ToolRunContext,
    args: &Value,
) -> anyhow::Result<Value> {
    let session = require_session(execution)?;
    let plugin_id = CordisDynamicPluginId::new(required_string(args, "pluginId")?);
    let package_id = CordisDynamicPackageId::new(required_string(args, "packageId")?);
    let mode: DynamicCordisRunMode = serde_json::from_value(args["mode"].clone())?;
    match runner
        .run_with_signal(
            session,
            &plugin_id,
            &package_id,
            mode,
            Some(&execution.signal()),
        )
        .await
    {
        DynamicCordisRunResponse::Failure { message, .. } => anyhow::bail!(message),
        DynamicCordisRunResponse::Success {
            status,
            plugin_run_id,
            waiting_for,
            client_waiting_for,
            current_package_id,
            next_package_id,
            mode,
            ..
        } => Ok(json!({
            "status":status,
            "pluginId":plugin_id,
            "packageId":package_id,
            "pluginRunId":plugin_run_id,
            "mode":mode,
            "currentPackageId":current_package_id,
            "nextPackageId":next_package_id,
            "host":{"status":if waiting_for.is_empty(){"running"}else{"waiting"},"waitingFor":waiting_for},
            "client":{"status":client_waiting_for.as_ref().map_or("absent",|waiting|if waiting.is_empty(){"running"}else{"waiting"}),"waitingFor":client_waiting_for.unwrap_or_default()},
        })),
    }
}

async fn stop_plugin(
    runner: &Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
    execution: &ToolRunContext,
    args: &Value,
) -> anyhow::Result<Value> {
    let plugin_id = CordisDynamicPluginId::new(required_string(args, "pluginId")?);
    match runner.stop(require_session(execution)?, &plugin_id).await {
        DynamicCordisStopResponse::Success
        | DynamicCordisStopResponse::Failure {
            reason: DynamicCordisStopFailureReason::NotRunning,
            ..
        } => {}
        DynamicCordisStopResponse::Failure { message, .. } => anyhow::bail!(message),
    }
    Ok(json!({"pluginId":plugin_id}))
}

async fn undefine_plugin(
    runner: &Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
    execution: &ToolRunContext,
    args: &Value,
) -> anyhow::Result<Value> {
    let plugin_id = CordisDynamicPluginId::new(required_string(args, "pluginId")?);
    match runner
        .undefine(require_session(execution)?, &plugin_id)
        .await
    {
        DynamicCordisUndefineReceipt::Success { was_running } => {
            Ok(json!({"pluginId":plugin_id,"wasRunning":was_running}))
        }
        DynamicCordisUndefineReceipt::PluginMissing { message } => anyhow::bail!(message),
    }
}

fn inspect_self(
    runner: &Arc<seekdeep_cordis_host_runner::DynamicCordisRunner>,
    execution: &ToolRunContext,
    args: &Value,
) -> anyhow::Result<Value> {
    let session = require_session(execution)?;
    let plugin = args.get("pluginId").and_then(Value::as_str);
    let package = args.get("packageId").and_then(Value::as_str);
    if package.is_some() && plugin.is_none() {
        anyhow::bail!("cordis_inspect_self packageId requires pluginId");
    }
    let Some(plugin) = plugin else {
        return Ok(json!({
            "mode":"plugins",
            "plugins":runner.list_plugins(session).iter().map(plugin_summary).collect::<Vec<_>>(),
        }));
    };
    let plugin_id = CordisDynamicPluginId::new(plugin);
    let Some(package) = package else {
        let inspected = runner.inspect_plugin(session, &plugin_id)?;
        return Ok(json!({
            "mode":"plugin",
            "plugin":plugin_summary(&inspected),
            "packages":inspected.packages,
        }));
    };
    let package_id = CordisDynamicPackageId::new(package);
    let inspected = runner.inspect_package(session, &plugin_id, &package_id)?;
    Ok(json!({
        "mode":"package",
        "plugin":reference_summary(&inspected.reference),
        "packageId":package_id,
        "name":inspected.reference.name,
        "purpose":inspected.reference.purpose,
        "code":{"host":inspected.code.host,"client":inspected.code.client},
    }))
}

fn plugin_summary(value: &seekdeep_cordis_host_runner::DynamicCordisPluginInspection) -> Value {
    let mut summary = reference_summary(&value.reference);
    summary["packageCount"] = json!(value.packages.len());
    summary
}

fn reference_summary(value: &seekdeep_cordis_host_runner::DynamicCordisReference) -> Value {
    json!({
        "pluginId":value.plugin_id,
        "name":value.name,
        "state":if value.active_run.is_some(){"running"}else if value.current_package_id.is_some(){"stopped"}else{"defined"},
        "currentPackageId":value.current_package_id,
        "nextPackageId":value.next_package_id,
        "activeRun":value.active_run,
        "latestRun":value.latest_run,
    })
}

fn render_value(name: &str, value: &Value) -> anyhow::Result<String> {
    match name {
        "cordis_define" => Ok(format!(
            "Defined {}/{} ({}); it is not running yet. Use cordis_run to activate this Package.",
            required_string(value, "pluginId")?,
            required_string(value, "packageId")?,
            required_string(value, "name")?,
        )),
        "cordis_run" => Ok(format!(
            "{}/{} is {} ({}).",
            required_string(value, "pluginId")?,
            required_string(value, "packageId")?,
            match value["status"].as_str() {
                Some("awaiting-approval") => "awaiting user approval",
                Some("starting") => "starting asynchronously",
                _ => "running",
            },
            required_string(value, "pluginRunId")?,
        )),
        "cordis_stop" => Ok(format!(
            "Dynamic Plugin {} is stopped; its definition and versions remain.",
            required_string(value, "pluginId")?
        )),
        "cordis_undefine" => Ok(format!(
            "Removed dynamic Plugin {} and all of its Packages.",
            required_string(value, "pluginId")?
        )),
        _ => Ok(serde_json::to_string_pretty(value)?),
    }
}

fn present_call(name: &str, args: &Value) -> Option<seekdeep_tools::ToolCallView> {
    match name {
        "cordis_inspect_list" => Some(inspect_list_call()),
        "cordis_inspect_query" => Some(inspect_query_call(
            args["platform"].as_str()?,
            args["provider"].as_str()?,
            args["method"].as_str()?,
        )),
        "cordis_inspect_self" => Some(inspect_self_call(
            args.get("pluginId").and_then(Value::as_str),
            args.get("packageId").and_then(Value::as_str),
        )),
        "cordis_define" => Some(define_call(
            args.pointer("/plugin/pluginId")
                .and_then(Value::as_str)
                .unwrap_or("new plugin"),
            args["name"].as_str()?,
            args["purpose"].as_str()?,
            &args["code"],
        )),
        "cordis_run" => Some(run_call(
            args["pluginId"].as_str()?,
            args["packageId"].as_str()?,
            args["mode"].as_str() == Some("update"),
        )),
        "cordis_stop" => Some(stop_call(args["pluginId"].as_str()?)),
        "cordis_undefine" => Some(undefine_call(args["pluginId"].as_str()?)),
        _ => None,
    }
}

/// Builds the Loader-compatible plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move { apply(&context) })
    })
}
