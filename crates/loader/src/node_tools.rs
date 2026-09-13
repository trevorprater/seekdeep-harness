//! Native Tools registrations backed by lossless callbacks in the Node realm.

use std::sync::Arc;

use seekdeep_code_runtime::{CodeBindingFailure, CodeJsonString, CodeJsonValue};
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_tools::{
    TOOLS, ToolDefinition, ToolOutputDefinition, assert_supported_json_schema,
    parameter_schema_spec_to_json_schema,
};
use serde_json::{Value, json};

use crate::node_plugin::NodeRealm;

fn response(value: &Value) -> anyhow::Result<CodeJsonValue> {
    if let Some(error) = value["rawError"].as_str() {
        return Err(CodeBindingFailure {
            message: CodeJsonString::parse(error.to_owned())?,
        }
        .into());
    }
    CodeJsonValue::parse(
        value["raw"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Node Tool callback returned no JSON value"))?
            .to_owned(),
    )
    .map_err(Into::into)
}

fn invoke(
    realm: &NodeRealm,
    activation: u64,
    tool: u64,
    phase: &str,
    args: &CodeJsonValue,
    value: &CodeJsonValue,
) -> anyhow::Result<CodeJsonValue> {
    response(&realm.request(json!({"action":"toolInvoke","activation":activation,"tool":tool,"phase":phase,"argsRaw":args.as_raw(),"valueRaw":value.as_raw()}))?)
}

pub(super) fn register(
    realm: &Arc<NodeRealm>,
    context: &Context,
    activation: u64,
    command: &Value,
) -> anyhow::Result<EffectHandle> {
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("Tool registration requires the tools service"))?;
    let tool = command["tool"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Node Tool identity is missing"))?;
    let parameters = parameter_schema_spec_to_json_schema(command["parameters"].clone())?;
    let Value::Object(parameters) = parameters.into_value() else {
        anyhow::bail!("Tool parameters must compile to an object schema");
    };
    let schema = Arc::new(assert_supported_json_schema(
        command["outputSchema"].clone(),
    )?);
    let execute_realm = realm.clone();
    let execute = Arc::new(move |args: CodeJsonValue, _| {
        let realm = execute_realm.clone();
        Box::pin(async move {
            response(&realm.request_async(json!({"action":"toolInvoke","activation":activation,"tool":tool,"phase":"execute","argsRaw":args.as_raw()})).await?)
        }) as seekdeep_tools::runtime::ToolJsonExecuteFuture
    });
    let render_realm = realm.clone();
    let render = Arc::new(move |args: &CodeJsonValue, value: &CodeJsonValue| {
        crate::javascript_plugin::decode_rendered_content(&invoke(
            &render_realm,
            activation,
            tool,
            "render",
            args,
            value,
        )?)
    });
    let mut output = ToolOutputDefinition::new_lossless(schema, render);
    if command["presentationMeta"] == true {
        let realm = realm.clone();
        output = output.presentation_meta_lossless(Arc::new(move |args, value| {
            invoke(&realm, activation, tool, "presentationMeta", args, value)
        }));
    }
    let mut definition = ToolDefinition::new_lossless(
        command["name"].as_str().unwrap_or_default(),
        command["description"].as_str().unwrap_or_default(),
        parameters,
        output,
        execute,
    );
    definition.timeout_ms = command["timeoutMs"].as_f64();
    if command["concurrency"] == true {
        let realm = realm.clone();
        definition = definition.concurrency_safe_lossless(Arc::new(move |args| {
            invoke(
                &realm,
                activation,
                tool,
                "concurrency",
                args,
                &CodeJsonValue::from(Value::Null),
            )
            .is_ok_and(|value| value.deserialize::<bool>().unwrap_or(false))
        }));
    }
    tools.register(context, definition)
}
