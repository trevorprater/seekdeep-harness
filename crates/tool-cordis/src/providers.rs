//! First-party Host inspect providers over generated catalogs and live tools.

use std::{future::Future, sync::Arc};

use futures::FutureExt as _;
use seekdeep_agent::AGENTS;
use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    CordisInspectMethodManifest, CordisInspectProviderManifest, HOST_BUILTIN_INSPECTION,
    HostCordisInspectProviderRegistration, HostCordisInspectQueryContext,
};
use seekdeep_tools::TOOLS;
use serde_json::{Value, json};

use crate::api_catalog::{query_host_event_api, query_service_api};

/// Constructs the four source Host inspection providers.
#[must_use]
pub fn host_inspect_providers(context: &Context) -> Vec<HostCordisInspectProviderRegistration> {
    vec![
        registration(
            "Service",
            "Progressive Host Service discovery: compact capability/signature directory, then one exact coding contract.",
            "listService",
            |input, _| async move { query_service_api(read_exact(input.as_ref(), "service")) },
            exact_input(
                "service",
                "Exact Service key. Omit it for the compact Service and method-signature directory.",
            ),
        ),
        registration(
            "Event",
            "Progressive Host Event discovery: compact listener directory, then one exact event contract.",
            "listEvents",
            |input, _| async move { query_host_event_api(read_exact(input.as_ref(), "event")) },
            exact_input(
                "event",
                "Exact Event name. Omit it for the compact Event and listener-signature directory.",
            ),
        ),
        registration(
            "Builtin",
            "Plain-JavaScript symbols available to a dynamic Host half.",
            "listBuiltins",
            |_, _| async move {
                Ok(json!({
                    "builtins": HOST_BUILTIN_INSPECTION.iter().map(|builtin| json!({
                        "name": builtin.name,
                        "description": builtin.description,
                        "signatures": builtin.signatures,
                    })).collect::<Vec<_>>(),
                    "referencedTypes": [],
                }))
            },
            empty_input(),
        ),
        tool_registration(context.clone()),
    ]
}

fn registration<F, Fut>(
    id: &str,
    description: &str,
    method: &str,
    query: F,
    input_schema: Value,
) -> HostCordisInspectProviderRegistration
where
    F: Fn(Option<Value>, HostCordisInspectQueryContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<Value>> + Send + 'static,
{
    let id_owned = id.to_owned();
    let method_owned = method.to_owned();
    let query = Arc::new(query);
    HostCordisInspectProviderRegistration {
        manifest: CordisInspectProviderManifest {
            id: id_owned.clone(),
            description: description.to_owned(),
            methods: vec![CordisInspectMethodManifest {
                name: method_owned.clone(),
                description: description.to_owned(),
                input_schema,
                output_schema: json!({"description":"JSON data owned by this inspect provider."}),
            }],
        },
        query: Arc::new(move |requested, input, context| {
            let query = query.clone();
            let id = id_owned.clone();
            let method = method_owned.clone();
            async move {
                anyhow::ensure!(
                    requested == method,
                    "unknown {id} inspect method \"{requested}\""
                );
                query(input, context).await
            }
            .boxed()
        }),
    }
}

fn tool_registration(context: Context) -> HostCordisInspectProviderRegistration {
    registration(
        "Tool",
        "Tools visible to the requesting Agent, including scoped and dynamic registrations.",
        "listTools",
        move |_, query| {
            let context = context.clone();
            async move {
                let agents = context.get(AGENTS).ok_or_else(|| {
                    anyhow::anyhow!("Tool inspection requires the Agent registry")
                })?;
                let agent = agents
                    .get(&query.session_id)
                    .ok_or_else(|| anyhow::anyhow!("Tool inspection Agent is unavailable"))?;
                let tools = context
                    .get(TOOLS)
                    .ok_or_else(|| anyhow::anyhow!("Tool inspection requires Tools"))?;
                Ok(json!({"tools": tools.schemas(Some(agent.scope_key()))}))
            }
        },
        empty_input(),
    )
}

fn exact_input(field: &str, description: &str) -> Value {
    json!({
        "type":"object",
        "properties": {(field): {"type":"string", "description":description}},
        "additionalProperties":false,
    })
}

fn empty_input() -> Value {
    json!({"type":"object", "properties":{}, "additionalProperties":false})
}

fn read_exact<'a>(input: Option<&'a Value>, field: &str) -> Option<&'a str> {
    input?.as_object()?.get(field)?.as_str()
}
