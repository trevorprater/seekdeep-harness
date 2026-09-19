//! First-party Client Inspect provider manifests and owner-query routing.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_cordis_dynamic_types::{CordisInspectMethodManifest, CordisInspectProviderManifest};
use serde_json::{Value, json};

use crate::{
    CLIENT_BUILTIN_INSPECTION, ClientCordisInspectProviderRegistration, LiveSlotNode,
    query_client_event_api, query_client_service_api, query_client_slots,
};

/// Static catalog query narrowed to one optional exact owner key.
pub type ClientCatalogQuery =
    Arc<dyn Fn(Option<String>) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
/// Live provider query with no input.
pub type ClientNoInputQuery =
    Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
/// Live Slot registry subtree query.
pub type ClientSlotQuery = Arc<
    dyn Fn(Option<String>) -> BoxFuture<'static, anyhow::Result<Vec<LiveSlotNode>>> + Send + Sync,
>;

/// Owner-provided data sources behind the first-party provider directory.
pub struct ClientInspectProviderSources {
    /// Static Client Service catalog.
    pub services: ClientCatalogQuery,
    /// Static Client Event catalog.
    pub events: ClientCatalogQuery,
    /// Static plus live Slot subtree query.
    pub slots: ClientSlotQuery,
    /// Live Theme token query.
    pub theme: ClientNoInputQuery,
}

impl std::fmt::Debug for ClientInspectProviderSources {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientInspectProviderSources")
            .finish_non_exhaustive()
    }
}

/// Constructs Service, Event, Builtin, Slots, and Theme providers in source order.
#[must_use]
pub fn client_inspect_providers(
    sources: ClientInspectProviderSources,
) -> Vec<ClientCordisInspectProviderRegistration> {
    vec![
        catalog_registration(
            "Service",
            "Progressive Client Service discovery: compact capability/signature directory, then one exact coding contract.",
            "listService",
            "service",
            "Exact Service key. Omit it for the compact Service and method-signature directory.",
            "Compact Service directory, or one exact Service contract with only its referenced type declarations.",
            sources.services,
        ),
        catalog_registration(
            "Event",
            "Progressive Client Event discovery: compact listener directory, then one exact event contract.",
            "listEvents",
            "event",
            "Exact Event name. Omit it for the compact Event and listener-signature directory.",
            "Compact Event directory, or one exact Event contract with only its referenced type declarations.",
            sources.events,
        ),
        builtin_registration(),
        slots_registration(sources.slots),
        no_input_registration(
            "Theme",
            "Current theme token names and light/dark override requirements.",
            "listTokens",
            sources.theme,
        ),
    ]
}

/// Builds generated Service/Event queries plus injected live Slots and Theme owners.
#[must_use]
pub fn generated_client_inspect_sources(
    slots: ClientSlotQuery,
    theme: ClientNoInputQuery,
) -> ClientInspectProviderSources {
    ClientInspectProviderSources {
        services: Arc::new(|exact| {
            Box::pin(async move { query_client_service_api(exact.as_deref()) })
        }),
        events: Arc::new(|exact| Box::pin(async move { query_client_event_api(exact.as_deref()) })),
        slots,
        theme,
    }
}

fn slots_registration(query: ClientSlotQuery) -> ClientCordisInspectProviderRegistration {
    let description = "Progressive live Slot inspection: compact purpose/topology trees plus one exact Slot contract.";
    ClientCordisInspectProviderRegistration {
        manifest: CordisInspectProviderManifest {
            id: "Slots".to_owned(),
            description: description.to_owned(),
            methods: vec![CordisInspectMethodManifest {
                name: "listSubTree".to_owned(),
                description: "Return compact live Slot trees for navigation. With root, also return the selected Slot's full contract and occupants.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "root": {
                            "type": "string",
                            "description": "Exact live Slot key. When supplied, selected contains the full contract for this Slot."
                        }
                    },
                    "additionalProperties": false,
                }),
                output_schema: json!({
                    "description": "Compact purpose/topology trees. With root, selected also contains that Slot's full contract and live occupants."
                }),
            }],
        },
        query: Arc::new(move |method, input, _| {
            if method != "listSubTree" {
                return Box::pin(async move {
                    anyhow::bail!("unknown Slots inspect method {method:?}")
                });
            }
            let root = read_exact(input.as_ref(), "root");
            let query = query.clone();
            Box::pin(async move {
                let trees = query(root.clone()).await?;
                Ok(query_client_slots(root.as_deref(), &trees))
            })
        }),
    }
}

fn catalog_registration(
    id: &str,
    description: &str,
    method: &str,
    field: &str,
    field_description: &str,
    output_description: &str,
    query: ClientCatalogQuery,
) -> ClientCordisInspectProviderRegistration {
    let input_schema = json!({
        "type": "object",
        "properties": {
            field: {"type": "string", "description": field_description}
        },
        "additionalProperties": false,
    });
    let mut input_schema = input_schema.as_object().cloned().expect("literal object");
    input_schema.insert(
        "properties".to_owned(),
        Value::Object(serde_json::Map::from_iter([(
            field.to_owned(),
            json!({"type": "string", "description": field_description}),
        )])),
    );
    let manifest = CordisInspectProviderManifest {
        id: id.to_owned(),
        description: description.to_owned(),
        methods: vec![CordisInspectMethodManifest {
            name: method.to_owned(),
            description: description.to_owned(),
            input_schema: Value::Object(input_schema),
            output_schema: json!({"description": output_description}),
        }],
    };
    let expected_method = method.to_owned();
    let provider_id = id.to_owned();
    let field = field.to_owned();
    ClientCordisInspectProviderRegistration {
        manifest,
        query: Arc::new(move |requested, input, _| {
            if requested != expected_method {
                let provider_id = provider_id.clone();
                return Box::pin(async move {
                    anyhow::bail!("unknown {provider_id} inspect method {requested:?}")
                });
            }
            let exact = read_exact(input.as_ref(), &field);
            query(exact)
        }),
    }
}

fn no_input_registration(
    id: &str,
    description: &str,
    method: &str,
    query: ClientNoInputQuery,
) -> ClientCordisInspectProviderRegistration {
    let expected_method = method.to_owned();
    let provider_id = id.to_owned();
    ClientCordisInspectProviderRegistration {
        manifest: CordisInspectProviderManifest {
            id: id.to_owned(),
            description: description.to_owned(),
            methods: vec![CordisInspectMethodManifest {
                name: method.to_owned(),
                description: description.to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                output_schema: json!({
                    "description": "JSON data owned by this inspect provider."
                }),
            }],
        },
        query: Arc::new(move |requested, _, _| {
            if requested != expected_method {
                let provider_id = provider_id.clone();
                return Box::pin(async move {
                    anyhow::bail!("unknown {provider_id} inspect method {requested:?}")
                });
            }
            query()
        }),
    }
}

fn builtin_registration() -> ClientCordisInspectProviderRegistration {
    no_input_registration(
        "Builtin",
        "Plain-JavaScript symbols available to a dynamic Client half.",
        "listBuiltins",
        Arc::new(|| {
            Box::pin(async {
                Ok(json!({
                    "builtins": CLIENT_BUILTIN_INSPECTION.iter().map(|entry| json!({
                        "name": entry.name,
                        "description": entry.description,
                        "signatures": entry.signatures,
                    })).collect::<Vec<_>>(),
                    "referencedTypes": [],
                }))
            })
        }),
    )
}

fn read_exact(input: Option<&Value>, field: &str) -> Option<String> {
    input?.as_object()?.get(field)?.as_str().map(str::to_owned)
}
