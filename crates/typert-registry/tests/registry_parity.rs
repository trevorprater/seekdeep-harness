//! Behavioral mirror of `packages/typert/registry/tests/typert.spec.ts`.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_cordis::{Context, fiber::Fiber};
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterDescriptor, InvocationParameterSource,
    InvocationReceiver, InvocationScope, TypertBoundaryValue, TypertClientContextBinder,
    TypertContextRegistry as _, TypertHostContextProvider, TypertHostObject,
    TypertLocalRegistry as _, TypertLookupProvider, TypertLookupRegistry as _,
    TypertRegistryChangeKind, TypertRemoteContribution, TypertRemoteRegistry as _, TypertSchema,
};
use seekdeep_typert_registry::{
    TypertContribution, TypertDocTag, TypertDocumentation, TypertFace, TypertMemberKind,
    TypertMemberModel, TypertPackageModel, TypertSchemaContribution, TypertSchemaFilter,
    TypertServiceModel, TypertTypeModel, install, typert_endpoint, typert_key, typert_package_key,
};
use serde_json::{Value, json};

#[derive(Debug)]
struct NameSchema;

impl TypertSchema for NameSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(
            value
                .as_json()
                .and_then(Value::as_object)
                .and_then(|object| object.get("name"))
                .is_some_and(Value::is_string),
            "expected object with string name"
        );
        Ok(value)
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        }))
    }
}

#[derive(Debug)]
struct StringSchema;

impl TypertSchema for StringSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(
            value.as_json().is_some_and(Value::is_string),
            "expected string"
        );
        Ok(value)
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"type": "string"}))
    }
}

fn model() -> TypertPackageModel {
    TypertPackageModel {
        services: vec![TypertServiceModel {
            documentation: TypertDocumentation {
                summary: Some("Tool registry and execution pipeline.".to_owned()),
                tags: Vec::new(),
                ..TypertDocumentation::default()
            },
            key: "tools".to_owned(),
            export_name: "ToolRuntime".to_owned(),
            members: vec![TypertMemberModel {
                kind: TypertMemberKind::Method,
                name: "register".to_owned(),
                signature: "register(definition: ToolDefinition): () => void".to_owned(),
                summary: None,
                js_doc: None,
            }],
            types: vec![TypertTypeModel {
                name: "ToolDefinition".to_owned(),
                declaration: "export interface ToolDefinition {}".to_owned(),
            }],
        }],
        events: Vec::new(),
        objects: Vec::new(),
    }
}

fn contribution() -> TypertContribution {
    TypertContribution {
        package: "@deepseek-ai/dsh-tools".to_owned(),
        face: TypertFace::Host,
        schemas: vec![TypertSchemaContribution {
            name: "ToolInput".to_owned(),
            schema: Arc::new(NameSchema),
        }],
        model: model(),
        invocations: Vec::new(),
    }
}

fn invocation(id: &str) -> InvocationDescriptor {
    InvocationDescriptor {
        id: id.to_owned(),
        service: "goals".to_owned(),
        namespace: "goals".to_owned(),
        method: "create".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: None,
        parameters: vec![InvocationParameterDescriptor {
            name: "request".to_owned(),
            wire: "request".to_owned(),
            source: InvocationParameterSource::Json,
            lookup: None,
            codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
            accepts_undefined: None,
        }],
        cancellation: false,
        result: seekdeep_typert_protocol::TypertCodec::SrcJson,
        source_location: None,
    }
}

fn scoped_invocation() -> InvocationDescriptor {
    InvocationDescriptor {
        scope: Some(InvocationScope {
            context: "fixture".to_owned(),
            wire: "agentId".to_owned(),
        }),
        parameters: vec![
            InvocationParameterDescriptor {
                name: "agent".to_owned(),
                wire: "agentId".to_owned(),
                source: InvocationParameterSource::Lookup,
                lookup: Some("fixture".to_owned()),
                codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
                accepts_undefined: None,
            },
            InvocationParameterDescriptor {
                name: "request".to_owned(),
                wire: "request".to_owned(),
                source: InvocationParameterSource::Json,
                lookup: None,
                codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
                accepts_undefined: None,
            },
        ],
        ..invocation("@fixture/remote#goals/create-scoped")
    }
}

#[test]
fn reflection_models_preserve_all_documentation_shapes() {
    let tag = TypertDocTag {
        name: "param".to_owned(),
        argument: Some("value".to_owned()),
        comment: Some("marker".to_owned()),
        text: "@param value marker".to_owned(),
    };
    let encoded = serde_json::to_value(&tag).unwrap();
    assert_eq!(encoded["argument"], "value");
    assert_eq!(
        model().services[0].members[0].kind,
        TypertMemberKind::Method
    );
}

#[tokio::test]
async fn registers_queries_and_withdraws_schemas_separately_from_reflection() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let contribution = contribution();
    let schema = contribution.schemas[0].schema.clone();
    let dispose = registry.register(&context, &contribution).unwrap();

    assert_eq!(
        typert_key("@deepseek-ai/dsh-tools", "ToolInput"),
        "@deepseek-ai/dsh-tools#ToolInput"
    );
    assert_eq!(
        typert_package_key("@deepseek-ai/dsh-tools", TypertFace::Host),
        "@deepseek-ai/dsh-tools#host"
    );
    let record = registry.get("@deepseek-ai/dsh-tools#ToolInput").unwrap();
    assert!(Arc::ptr_eq(&record.schema, &schema));
    assert_eq!(
        registry
            .get_package("@deepseek-ai/dsh-tools", TypertFace::Host)
            .unwrap()
            .model
            .services[0]
            .key,
        "tools"
    );
    assert_eq!(registry.list(&TypertSchemaFilter::default()).len(), 1);
    assert_eq!(
        registry
            .list_packages(&TypertSchemaFilter {
                face: Some(TypertFace::Host),
                ..TypertSchemaFilter::default()
            })
            .len(),
        1
    );

    dispose.dispose().await.unwrap();
    assert!(registry.get("@deepseek-ai/dsh-tools#ToolInput").is_none());
    assert!(
        registry
            .get_package("@deepseek-ai/dsh-tools", TypertFace::Host)
            .is_none()
    );
}

#[tokio::test]
async fn registration_follows_exact_calling_fiber_lifecycle() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let fiber = Fiber::active_child("contributor");
    let child = context.with_fiber(fiber.clone());
    registry.register(&child, &contribution()).unwrap();
    assert!(registry.get("@deepseek-ai/dsh-tools#ToolInput").is_some());
    fiber.dispose().await.unwrap();
    assert!(registry.list(&TypertSchemaFilter::default()).is_empty());
}

#[test]
fn rejects_duplicate_faces_and_schema_keys_atomically() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    registry.register(&context, &contribution()).unwrap();
    let error = registry.register(&context, &contribution()).unwrap_err();
    assert!(error.to_string().contains("package face"));

    let mut duplicate = contribution();
    duplicate.package = "@fixture/duplicate".to_owned();
    duplicate.schemas.push(TypertSchemaContribution {
        name: "ToolInput".to_owned(),
        schema: Arc::new(StringSchema),
    });
    let error = registry.register(&context, &duplicate).unwrap_err();
    assert!(error.to_string().contains("schema"));
    assert!(
        registry
            .get_package("@fixture/duplicate", TypertFace::Host)
            .is_none()
    );
}

#[test]
fn rejects_malformed_identities_and_filters_both_views() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    registry.register(&context, &contribution()).unwrap();
    for package in ["", "bad#package"] {
        let mut malformed = contribution();
        malformed.package = package.to_owned();
        assert!(registry.register(&context, &malformed).is_err());
    }
    let mut malformed = contribution();
    malformed.package = "@fixture/schema-name".to_owned();
    malformed.schemas[0].name = "bad#name".to_owned();
    assert!(registry.register(&context, &malformed).is_err());

    for filter in [
        TypertSchemaFilter {
            package: Some("@fixture/absent".to_owned()),
            face: None,
        },
        TypertSchemaFilter {
            package: None,
            face: Some(TypertFace::Client),
        },
    ] {
        assert!(registry.list(&filter).is_empty());
        assert!(registry.list_packages(&filter).is_empty());
    }
}

#[test]
fn resolves_required_schemas_and_projects_fresh_documents() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    registry.register(&context, &contribution()).unwrap();
    assert_eq!(
        registry
            .resolve("@deepseek-ai/dsh-tools#ToolInput")
            .unwrap()
            .name,
        "ToolInput"
    );
    assert!(
        registry
            .resolve("@deepseek-ai/dsh-tools#Missing")
            .unwrap_err()
            .to_string()
            .contains("contributes no schema")
    );
    assert!(
        registry
            .resolve("@fixture/absent#Value")
            .unwrap_err()
            .to_string()
            .contains("no registered contribution")
    );
    assert!(
        registry
            .resolve("invalid")
            .unwrap_err()
            .to_string()
            .contains("expected")
    );
    let mut first = registry
        .to_json_schema("@deepseek-ai/dsh-tools#ToolInput")
        .unwrap();
    first["mutated"] = json!(true);
    let second = registry
        .to_json_schema("@deepseek-ai/dsh-tools#ToolInput")
        .unwrap();
    assert_eq!(second["type"], "object");
    assert!(second.get("mutated").is_none());
}

#[tokio::test]
async fn registers_local_invocations_atomically_and_retains_history() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let descriptor = invocation("@fixture/local#goals/create");
    let mut contribution = contribution();
    contribution.invocations.push(descriptor.clone());
    let changes = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = changes.clone();
    registry
        .local()
        .subscribe(
            &context,
            Arc::new(move |change| {
                seen.lock()
                    .push(format!("{:?}:{}", change.kind, change.key));
            }),
        )
        .unwrap();
    assert!(!registry.local().has_seen("goals/create"));
    let dispose = registry.register(&context, &contribution).unwrap();
    assert_eq!(typert_endpoint(&descriptor), "goals/create");
    assert_eq!(
        registry.local().get("goals/create").unwrap().id,
        descriptor.id
    );
    assert!(registry.local().has_seen("goals/create"));
    assert_eq!(registry.local().list().len(), 1);
    dispose.dispose().await.unwrap();
    assert!(registry.local().list().is_empty());
    assert!(registry.local().has_seen("goals/create"));
    assert_eq!(changes.lock().len(), 2);
}

#[test]
fn rejects_duplicate_invocation_endpoints_and_ids_atomically() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let remote = TypertRemoteContribution {
        package: "@fixture/duplicate-endpoint".to_owned(),
        descriptors: vec![
            invocation("@fixture/remote#first"),
            invocation("@fixture/remote#second"),
        ],
    };
    assert!(
        registry
            .remotes()
            .register(&context, remote)
            .unwrap_err()
            .to_string()
            .contains("endpoint")
    );
    let mut renamed = invocation("@fixture/remote#same");
    renamed.method = "rename".to_owned();
    let remote = TypertRemoteContribution {
        package: "@fixture/duplicate-id".to_owned(),
        descriptors: vec![invocation("@fixture/remote#same"), renamed],
    };
    assert!(
        registry
            .remotes()
            .register(&context, remote)
            .unwrap_err()
            .to_string()
            .contains("invocation id")
    );
}

#[test]
fn rejects_each_untransportable_invocation_method() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    for (index, method) in ["create#v2", "create goal", ".", ".."]
        .into_iter()
        .enumerate()
    {
        let mut descriptor = invocation("@fixture/remote#invalid");
        descriptor.method = method.to_owned();
        let error = registry
            .remotes()
            .register(
                &context,
                TypertRemoteContribution {
                    package: format!("@fixture/invalid-{index}"),
                    descriptors: vec![descriptor],
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("RPC endpoint segment"));
    }
}

#[tokio::test]
async fn mounts_remote_contributions_in_calling_fiber_and_withdraws_exactly() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let fiber = Fiber::active_child("remote");
    let child = context.with_fiber(fiber.clone());
    let contribution = TypertRemoteContribution {
        package: "@fixture/remote".to_owned(),
        descriptors: vec![invocation("@fixture/remote#goals/create")],
    };
    let changes = Arc::new(AtomicUsize::new(0));
    let observed = changes.clone();
    registry
        .remotes()
        .subscribe(
            &context,
            Arc::new(move |change| {
                assert_eq!(change.kind, TypertRegistryChangeKind::Remote);
                observed.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
    registry
        .remotes()
        .register(&child, contribution.clone())
        .unwrap();
    assert!(registry.remotes().get("goals/create").is_some());
    assert!(registry.remotes().register(&context, contribution).is_err());
    fiber.dispose().await.unwrap();
    assert!(registry.remotes().list().is_empty());
    assert_eq!(changes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn direct_scope_must_select_its_unique_lookup_parameter() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let descriptor = scoped_invocation();
    let dispose = registry
        .remotes()
        .register(
            &context,
            TypertRemoteContribution {
                package: "@fixture/scoped".to_owned(),
                descriptors: vec![descriptor.clone()],
            },
        )
        .unwrap();
    dispose.dispose().await.unwrap();

    let mut context_receiver = descriptor.clone();
    context_receiver.invocation = InvocationReceiver::Context {
        context: "fixture".to_owned(),
        wire: "scopeId".to_owned(),
        codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
    };
    let mut missing = descriptor.clone();
    missing.scope.as_mut().unwrap().wire = "missingId".to_owned();
    let mut multiple = descriptor.clone();
    multiple.parameters.push(InvocationParameterDescriptor {
        name: "other".to_owned(),
        wire: "otherId".to_owned(),
        source: InvocationParameterSource::Lookup,
        lookup: Some("fixture".to_owned()),
        codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
        accepts_undefined: None,
    });
    let mut wrong_context = descriptor;
    wrong_context.scope.as_mut().unwrap().context = "other".to_owned();
    for (index, candidate) in [context_receiver, missing, multiple, wrong_context]
        .into_iter()
        .enumerate()
    {
        assert!(
            registry
                .remotes()
                .register(
                    &context,
                    TypertRemoteContribution {
                        package: format!("@fixture/rejected-{index}"),
                        descriptors: vec![candidate],
                    },
                )
                .is_err()
        );
    }
}

#[derive(Debug)]
struct FixtureObject {
    id: String,
}

fn lookup_provider(object: Arc<FixtureObject>) -> TypertLookupProvider {
    TypertLookupProvider {
        parameter: "agent".to_owned(),
        wire: "agentId".to_owned(),
        host_type_symbol: "@fixture/agent#Agent".to_owned(),
        wire_type_symbol: "@fixture/session#SessionId".to_owned(),
        resolve: Arc::new(move |id| {
            let object = object.clone();
            Box::pin(async move {
                Ok(
                    (id.as_json().and_then(Value::as_str) == Some(object.id.as_str()))
                        .then_some(object as TypertHostObject),
                )
            })
        }),
    }
}

#[tokio::test]
async fn registers_lookup_and_context_providers_without_domain_branches() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let object = Arc::new(FixtureObject {
        id: "agent-1".to_owned(),
    });
    let scoped = context.with_meta("agentId", json!("agent-1"));
    let dispose_lookup = registry
        .lookups()
        .register(&context, "fixture", lookup_provider(object.clone()))
        .unwrap();
    let scoped_for_host = scoped.clone();
    let dispose_host = registry
        .contexts()
        .register_host(
            &context,
            "registryFixture",
            TypertHostContextProvider {
                wire: "agentId".to_owned(),
                wire_type_symbol: "@fixture/session#SessionId".to_owned(),
                resolve: Arc::new(move |id| {
                    let scoped = scoped_for_host.clone();
                    Box::pin(async move {
                        Ok((id.as_json() == Some(&json!("agent-1"))).then_some(scoped))
                    })
                }),
            },
        )
        .unwrap();
    let dispose_client = registry
        .contexts()
        .register_client(
            &context,
            "registryFixture",
            TypertClientContextBinder {
                identity: Arc::new(|candidate| candidate.meta("agentId")),
            },
        )
        .unwrap();

    let resolved = (registry.lookups().get("fixture").unwrap().resolve)(json!("agent-1").into())
        .await
        .unwrap()
        .unwrap()
        .downcast::<FixtureObject>()
        .unwrap();
    assert_eq!(resolved.id, "agent-1");
    assert_eq!(registry.lookups().definitions()[0].wire, "agentId");
    assert_eq!(registry.lookups().keys(), ["fixture"]);
    let host = (registry
        .contexts()
        .get_host("registryFixture")
        .unwrap()
        .resolve)(json!("agent-1").into())
    .await
    .unwrap()
    .unwrap();
    assert_eq!(host.meta("agentId"), Some(json!("agent-1")));
    assert_eq!(
        (registry
            .contexts()
            .get_client("registryFixture")
            .unwrap()
            .identity)(&scoped),
        Some(json!("agent-1"))
    );

    dispose_client.dispose().await.unwrap();
    dispose_host.dispose().await.unwrap();
    dispose_lookup.dispose().await.unwrap();
    assert!(registry.lookups().keys().is_empty());
    assert_eq!(registry.lookups().definitions().len(), 1);
    assert!(registry.contexts().get_host("registryFixture").is_none());
}

#[tokio::test]
async fn configured_lookup_resolver_is_independent_of_provider_load_order() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let fallback = Arc::new(FixtureObject {
        id: "fallback".to_owned(),
    });
    let configured = Arc::new(FixtureObject {
        id: "configured".to_owned(),
    });
    let configured_for_resolver = configured.clone();
    let dispose_resolver = registry
        .lookups()
        .configure(
            &context,
            "fixture",
            Arc::new(move |id| {
                let configured = configured_for_resolver.clone();
                Box::pin(async move {
                    Ok(
                        (id.as_json().and_then(Value::as_str) == Some(configured.id.as_str()))
                            .then_some(configured as TypertHostObject),
                    )
                })
            }),
        )
        .unwrap();
    assert!(registry.lookups().get("fixture").is_none());
    let provider = registry
        .lookups()
        .register(&context, "fixture", lookup_provider(fallback.clone()))
        .unwrap();
    let resolved = (registry.lookups().get("fixture").unwrap().resolve)(json!("configured").into())
        .await
        .unwrap()
        .unwrap()
        .downcast::<FixtureObject>()
        .unwrap();
    assert_eq!(resolved.id, "configured");
    assert!(
        registry
            .lookups()
            .configure(
                &context,
                "fixture",
                Arc::new(|_| Box::pin(async { Ok(None) }))
            )
            .is_err()
    );
    provider.dispose().await.unwrap();
    let reloaded = registry
        .lookups()
        .register(&context, "fixture", lookup_provider(fallback.clone()))
        .unwrap();
    dispose_resolver.dispose().await.unwrap();
    let resolved = (registry.lookups().get("fixture").unwrap().resolve)(json!("fallback").into())
        .await
        .unwrap()
        .unwrap()
        .downcast::<FixtureObject>()
        .unwrap();
    assert_eq!(resolved.id, "fallback");
    reloaded.dispose().await.unwrap();
}

#[tokio::test]
async fn configured_host_resolver_is_independent_of_provider_load_order() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let fallback = context.with_meta("kind", json!("fallback"));
    let configured = context.with_meta("kind", json!("configured"));
    let configured_for_resolver = configured.clone();
    let dispose_resolver = registry
        .contexts()
        .configure_host(
            &context,
            "registryFixture",
            Arc::new(move |id| {
                let configured = configured_for_resolver.clone();
                Box::pin(async move {
                    Ok((id.as_json() == Some(&json!("configured"))).then_some(configured))
                })
            }),
        )
        .unwrap();
    assert!(registry.contexts().get_host("registryFixture").is_none());
    let fallback_for_provider = fallback.clone();
    let provider = registry
        .contexts()
        .register_host(
            &context,
            "registryFixture",
            TypertHostContextProvider {
                wire: "agentId".to_owned(),
                wire_type_symbol: "@fixture#AgentId".to_owned(),
                resolve: Arc::new(move |id| {
                    let fallback = fallback_for_provider.clone();
                    Box::pin(async move {
                        Ok((id.as_json() == Some(&json!("fallback"))).then_some(fallback))
                    })
                }),
            },
        )
        .unwrap();
    let resolved = (registry
        .contexts()
        .get_host("registryFixture")
        .unwrap()
        .resolve)(json!("configured").into())
    .await
    .unwrap()
    .unwrap();
    assert_eq!(resolved.meta("kind"), Some(json!("configured")));
    assert!(
        registry
            .contexts()
            .configure_host(
                &context,
                "registryFixture",
                Arc::new(|_| Box::pin(async { Ok(None) }))
            )
            .is_err()
    );
    provider.dispose().await.unwrap();
    let fallback_for_reload = fallback.clone();
    let reloaded = registry
        .contexts()
        .register_host(
            &context,
            "registryFixture",
            TypertHostContextProvider {
                wire: "agentId".to_owned(),
                wire_type_symbol: "@fixture#AgentId".to_owned(),
                resolve: Arc::new(move |id| {
                    let fallback = fallback_for_reload.clone();
                    Box::pin(async move {
                        Ok((id.as_json() == Some(&json!("fallback"))).then_some(fallback))
                    })
                }),
            },
        )
        .unwrap();
    dispose_resolver.dispose().await.unwrap();
    let resolved = (registry
        .contexts()
        .get_host("registryFixture")
        .unwrap()
        .resolve)(json!("fallback").into())
    .await
    .unwrap()
    .unwrap();
    assert_eq!(resolved.meta("kind"), Some(json!("fallback")));
    reloaded.dispose().await.unwrap();
}

#[tokio::test]
async fn publishes_changes_rejects_duplicates_and_disposes_subscriptions() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let changes = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let lookup_changes = changes.clone();
    let lookup_subscription = registry
        .lookups()
        .subscribe(
            &context,
            Arc::new(move |change| lookup_changes.lock().push(change.kind)),
        )
        .unwrap();
    let context_changes = changes.clone();
    let context_subscription = registry
        .contexts()
        .subscribe(
            &context,
            Arc::new(move |change| context_changes.lock().push(change.kind)),
        )
        .unwrap();
    let object = Arc::new(FixtureObject { id: "x".to_owned() });
    let lookup = lookup_provider(object);
    let host = TypertHostContextProvider {
        wire: "agentId".to_owned(),
        wire_type_symbol: "@fixture#AgentId".to_owned(),
        resolve: Arc::new(|_| Box::pin(async { Ok(None) })),
    };
    let client = TypertClientContextBinder {
        identity: Arc::new(|_| None),
    };
    let dispose_lookup = registry
        .lookups()
        .register(&context, "fixture", lookup.clone())
        .unwrap();
    let dispose_host = registry
        .contexts()
        .register_host(&context, "registryFixture", host.clone())
        .unwrap();
    let dispose_client = registry
        .contexts()
        .register_client(&context, "registryFixture", client.clone())
        .unwrap();
    assert!(
        registry
            .lookups()
            .register(&context, "fixture", lookup.clone())
            .is_err()
    );
    assert!(
        registry
            .contexts()
            .register_host(&context, "registryFixture", host)
            .is_err()
    );
    assert!(
        registry
            .contexts()
            .register_client(&context, "registryFixture", client)
            .is_err()
    );
    dispose_lookup.dispose().await.unwrap();
    dispose_host.dispose().await.unwrap();
    dispose_client.dispose().await.unwrap();
    assert_eq!(changes.lock().len(), 6);
    lookup_subscription.dispose().await.unwrap();
    context_subscription.dispose().await.unwrap();
    let mut modified_lookup = lookup;
    modified_lookup.parameter = "session".to_owned();
    assert!(
        registry
            .lookups()
            .register(&context, "fixture", modified_lookup)
            .unwrap_err()
            .to_string()
            .contains("changed its wire declaration")
    );
}

#[tokio::test]
async fn validates_every_representable_invocation_and_provider_boundary() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let strict_schema: Arc<dyn TypertSchema> = Arc::new(StringSchema);
    let mut strict = invocation("@fixture/remote#strict");
    strict.implementation = Some("remoteExportCreate".to_owned());
    strict.parameters[0].codec = seekdeep_typert_protocol::TypertCodec::Strict {
        type_symbol: "@fixture#Value".to_owned(),
        schema: strict_schema.clone(),
    };
    strict.cancellation = true;
    strict.result = seekdeep_typert_protocol::TypertCodec::Strict {
        type_symbol: "@fixture#Value".to_owned(),
        schema: strict_schema,
    };
    let dispose = registry
        .remotes()
        .register(
            &context,
            TypertRemoteContribution {
                package: "@fixture/strict".to_owned(),
                descriptors: vec![strict],
            },
        )
        .unwrap();
    dispose.dispose().await.unwrap();

    for (index, (descriptor, message)) in malformed_invocations().into_iter().enumerate() {
        let error = registry
            .remotes()
            .register(
                &context,
                TypertRemoteContribution {
                    package: format!("@fixture/malformed-{index}"),
                    descriptors: vec![descriptor],
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error:#}");
    }

    let object = Arc::new(FixtureObject { id: "x".to_owned() });
    assert!(
        registry
            .lookups()
            .register(&context, "bad#key", lookup_provider(object.clone()))
            .unwrap_err()
            .to_string()
            .contains("lookup key")
    );
    let mut bad_wire = lookup_provider(object);
    bad_wire.wire = "agent/id".to_owned();
    assert!(
        registry
            .lookups()
            .register(&context, "fixture", bad_wire)
            .unwrap_err()
            .to_string()
            .contains("lookup wire field")
    );
}

fn malformed_invocations() -> Vec<(InvocationDescriptor, &'static str)> {
    let mut malformed = vec![(invocation(""), "invocation id")];
    let mut bad_namespace = invocation("bad-namespace");
    "bad/name".clone_into(&mut bad_namespace.namespace);
    malformed.push((bad_namespace, "namespace"));
    let mut bad_implementation = invocation("bad-implementation");
    bad_implementation.implementation = Some("bad/name".to_owned());
    malformed.push((bad_implementation, "implementation method"));
    let mut duplicate_wire = invocation("duplicate-wire");
    duplicate_wire
        .parameters
        .push(InvocationParameterDescriptor {
            name: "other".to_owned(),
            wire: "request".to_owned(),
            source: InvocationParameterSource::Json,
            lookup: None,
            codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
            accepts_undefined: None,
        });
    malformed.push((duplicate_wire, "repeats wire field"));
    let mut no_lookup = invocation("no-lookup");
    no_lookup.parameters[0] = InvocationParameterDescriptor {
        name: "agent".to_owned(),
        wire: "agentId".to_owned(),
        source: InvocationParameterSource::Lookup,
        lookup: None,
        codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
        accepts_undefined: None,
    };
    malformed.push((no_lookup, "has no lookup key"));
    let mut optional_lookup = scoped_invocation();
    "optional-lookup".clone_into(&mut optional_lookup.id);
    optional_lookup.parameters[0].accepts_undefined = Some(true);
    malformed.push((optional_lookup, "cannot accept undefined"));
    let mut json_lookup = invocation("json-lookup");
    json_lookup.parameters[0].lookup = Some("fixture".to_owned());
    malformed.push((json_lookup, "JSON parameter"));
    let mut context_duplicate = invocation("context-duplicate");
    context_duplicate.invocation = InvocationReceiver::Context {
        context: "registryFixture".to_owned(),
        wire: "request".to_owned(),
        codec: seekdeep_typert_protocol::TypertCodec::SrcJson,
    };
    malformed.push((context_duplicate, "repeats wire field"));
    let mut empty_symbol = invocation("empty-symbol");
    empty_symbol.result = seekdeep_typert_protocol::TypertCodec::Strict {
        type_symbol: String::new(),
        schema: Arc::new(StringSchema),
    };
    malformed.push((empty_symbol, "type symbol"));
    malformed
}

#[test]
fn installs_same_registry_on_client_face() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    assert!(registry.list(&TypertSchemaFilter::default()).is_empty());
}

#[test]
fn contains_panicking_change_listener_and_notifies_later_listeners() {
    let context = Context::new();
    let registry = install(&context).unwrap();
    let observed = Arc::new(AtomicUsize::new(0));
    registry
        .remotes()
        .subscribe(&context, Arc::new(|_| panic!("observer failed")))
        .unwrap();
    let later = observed.clone();
    registry
        .remotes()
        .subscribe(
            &context,
            Arc::new(move |_| {
                later.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
    registry
        .remotes()
        .register(
            &context,
            TypertRemoteContribution {
                package: "@fixture/remote".to_owned(),
                descriptors: vec![invocation("@fixture/remote#goals/create")],
            },
        )
        .unwrap();
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}
