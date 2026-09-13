//! Native artifact discovery, validation, caching, and lifecycle parity.

use std::{fmt::Write as _, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, Plugin};
use seekdeep_loader::PluginCatalog;
use seekdeep_typert_loader::{
    TypertArtifactFactory, TypertArtifactRegistry, TypertLoaderConfig, TypertPackageArtifact,
    plugin, validate_typert_manifest,
};
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterDescriptor, InvocationParameterSource,
    InvocationReceiver, TypertBoundaryValue, TypertCodec, TypertLocalRegistry as _, TypertSchema,
};
use seekdeep_typert_registry::{
    TypertContribution, TypertDocumentation, TypertEventModel, TypertFace, TypertMemberKind,
    TypertMemberModel, TypertObjectModel, TypertPackageModel, TypertSchemaContribution,
    TypertSchemaFilter, TypertServiceModel, TypertTypeModel,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;

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

fn strict(symbol: &str) -> TypertCodec {
    TypertCodec::Strict {
        type_symbol: symbol.to_owned(),
        schema: Arc::new(StringSchema),
    }
}

fn invocation(package: &str) -> InvocationDescriptor {
    InvocationDescriptor {
        id: format!("{package}#goals/create"),
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
            codec: strict(&format!("{package}/types#Request")),
            accepts_undefined: None,
        }],
        cancellation: true,
        result: strict(&format!("{package}/types#Result")),
        source_location: None,
    }
}

fn contribution(package: &str, schema_name: &str) -> TypertContribution {
    TypertContribution {
        package: package.to_owned(),
        face: TypertFace::Host,
        schemas: vec![TypertSchemaContribution {
            name: schema_name.to_owned(),
            schema: Arc::new(StringSchema),
        }],
        model: TypertPackageModel::default(),
        invocations: Vec::new(),
    }
}

fn artifact(package: &str, contribution: TypertContribution) -> TypertPackageArtifact {
    let contribution = Arc::new(contribution);
    TypertPackageArtifact {
        package: package.to_owned(),
        host: Some(Arc::new(move || {
            let contribution = contribution.clone();
            Box::pin(async move { Ok((*contribution).clone()) })
        })),
    }
}

fn no_host_artifact(package: &str) -> TypertPackageArtifact {
    TypertPackageArtifact {
        package: package.to_owned(),
        host: None,
    }
}

fn failing_artifact(package: &str, message: &str) -> TypertPackageArtifact {
    let message = message.to_owned();
    TypertPackageArtifact {
        package: package.to_owned(),
        host: Some(Arc::new(move || {
            let message = message.clone();
            Box::pin(async move { anyhow::bail!(message) })
        })),
    }
}

fn dummy_plugin(name: &str) -> Plugin {
    Plugin::new(
        name.to_owned(),
        std::iter::empty::<&str>(),
        |_context, _config| Box::pin(async { Ok(()) }),
    )
}

async fn eventually(mut predicate: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

struct Harness {
    context: Context,
    registry: Arc<seekdeep_typert_registry::TypertRegistry>,
    artifacts: Arc<TypertArtifactRegistry>,
    catalog: PluginCatalog,
}

impl Harness {
    fn new(packages: &[&str]) -> Self {
        let context = Context::new();
        let registry = seekdeep_typert_registry::install(&context).unwrap();
        let artifacts = TypertArtifactRegistry::install(&context).unwrap();
        let catalog = PluginCatalog::new();
        for package in packages {
            catalog
                .register_named(package, dummy_plugin(package))
                .unwrap();
        }
        Self {
            context,
            registry,
            artifacts,
            catalog,
        }
    }

    async fn composition(&self, packages: &[&str]) -> seekdeep_loader::LoadedComposition {
        let yaml = if packages.is_empty() {
            "[]\n".to_owned()
        } else {
            let mut yaml = String::new();
            for package in packages {
                writeln!(yaml, "- name: {package:?}").unwrap();
            }
            yaml
        };
        self.catalog.load_yaml(&self.context, &yaml).await.unwrap()
    }

    async fn mount(
        &self,
        config: TypertLoaderConfig,
    ) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
        let fiber = self
            .context
            .plugin(plugin(), serde_json::to_value(config).unwrap())?;
        fiber.await_settled().await?;
        Ok(fiber)
    }
}

#[tokio::test]
async fn explicit_package_registers_schema_and_invocation_then_withdraws_with_loader() {
    let harness = Harness::new(&[]);
    let mut nested = contribution("@fixture/nested", "Nested");
    nested.invocations.push(invocation("@fixture/nested"));
    harness
        .artifacts
        .register(&harness.context, artifact("@fixture/nested", nested))
        .unwrap();
    let _composition = harness.composition(&[]).await;
    let fiber = harness
        .mount(TypertLoaderConfig {
            packages: vec!["@fixture/nested".to_owned()],
        })
        .await
        .unwrap();
    assert!(harness.registry.get("@fixture/nested#Nested").is_some());
    let descriptor = harness.registry.local().get("goals/create").unwrap();
    assert_eq!(descriptor.id, "@fixture/nested#goals/create");
    assert!(descriptor.cancellation);

    fiber.dispose().await.unwrap();
    assert!(
        harness
            .registry
            .get_package("@fixture/nested", TypertFace::Host)
            .is_none()
    );
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn explicit_missing_and_hostless_packages_fail_together() {
    let harness = Harness::new(&[]);
    harness
        .artifacts
        .register(&harness.context, no_host_artifact("@fixture/plain"))
        .unwrap();
    let _composition = harness.composition(&[]).await;
    let error = harness
        .mount(TypertLoaderConfig {
            packages: vec!["@fixture/missing".to_owned(), "@fixture/plain".to_owned()],
        })
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("2 typert contributor(s) failed to register"));
    assert!(message.contains("configured package \"@fixture/missing\" cannot be resolved"));
    assert!(message.contains("configured package \"@fixture/plain\" does not export \"./typert\""));
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn mounted_entries_register_unmount_and_reactivate_incrementally() {
    let harness = Harness::new(&["@fixture/with", "@fixture/plain", "@fixture/late"]);
    harness
        .artifacts
        .register(
            &harness.context,
            artifact("@fixture/with", contribution("@fixture/with", "Thing")),
        )
        .unwrap();
    harness
        .artifacts
        .register(&harness.context, no_host_artifact("@fixture/plain"))
        .unwrap();
    harness
        .artifacts
        .register(
            &harness.context,
            artifact("@fixture/late", contribution("@fixture/late", "Late")),
        )
        .unwrap();
    let composition = harness
        .composition(&["@fixture/with", "@fixture/plain"])
        .await;
    let _loader = harness.mount(TypertLoaderConfig::default()).await.unwrap();
    assert!(harness.registry.get("@fixture/with#Thing").is_some());
    assert_eq!(
        harness.registry.list(&TypertSchemaFilter::default()).len(),
        1
    );
    let mounted = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.entry_name().as_deref() == Some("@fixture/with"))
        .expect("mounted fixture");
    harness
        .context
        .events()
        .emit(
            &harness.context,
            "internal/plugin",
            &EventArgs::one_shared(mounted.clone()),
        )
        .unwrap();
    harness
        .context
        .events()
        .emit(
            &harness.context,
            "internal/plugin",
            &EventArgs::one_shared(mounted),
        )
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        harness.registry.list(&TypertSchemaFilter::default()).len(),
        1
    );

    composition
        .update_yaml("- name: \"@fixture/plain\"\n")
        .await
        .unwrap();
    eventually(
        || harness.registry.get("@fixture/with#Thing").is_none(),
        "unmounted contribution remained",
    )
    .await;

    composition
        .update_yaml("- name: \"@fixture/with\"\n- name: \"@fixture/late\"\n")
        .await
        .unwrap();
    eventually(
        || {
            harness.registry.get("@fixture/with#Thing").is_some()
                && harness.registry.get("@fixture/late#Late").is_some()
        },
        "remounted or late contribution missing",
    )
    .await;
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn negative_discovery_verdict_is_cached_until_loader_restart() {
    let harness = Harness::new(&["virtual-plugin"]);
    let composition = harness.composition(&["virtual-plugin"]).await;
    let _loader = harness.mount(TypertLoaderConfig::default()).await.unwrap();
    assert!(
        harness
            .registry
            .get_package("virtual-plugin", TypertFace::Host)
            .is_none()
    );
    harness
        .artifacts
        .register(
            &harness.context,
            artifact("virtual-plugin", contribution("virtual-plugin", "Late")),
        )
        .unwrap();
    composition.update_yaml("[]\n").await.unwrap();
    composition
        .update_yaml("- name: \"virtual-plugin\"\n")
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert!(harness.registry.get("virtual-plugin#Late").is_none());
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn disposal_drops_an_in_flight_artifact_before_publication() {
    let harness = Harness::new(&["@fixture/pending"]);
    let (started_tx, started_rx) = oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let (release_tx, release_rx) = oneshot::channel();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let factory: TypertArtifactFactory = Arc::new({
        let started_tx = started_tx.clone();
        let release_rx = release_rx.clone();
        move || {
            let started_tx = started_tx.clone();
            let release_rx = release_rx.clone();
            Box::pin(async move {
                if let Some(started) = started_tx.lock().take() {
                    let _ = started.send(());
                }
                let receiver = release_rx.lock().await.take().expect("one load");
                let _ = receiver.await;
                Ok(contribution("@fixture/pending", "Pending"))
            })
        }
    });
    harness
        .artifacts
        .register(
            &harness.context,
            TypertPackageArtifact {
                package: "@fixture/pending".to_owned(),
                host: Some(factory),
            },
        )
        .unwrap();
    let composition = harness.composition(&[]).await;
    let loader = harness.mount(TypertLoaderConfig::default()).await.unwrap();
    composition
        .update_yaml("- name: \"@fixture/pending\"\n")
        .await
        .unwrap();
    started_rx.await.unwrap();
    loader.dispose().await.unwrap();
    let _ = release_tx.send(());
    tokio::task::yield_now().await;
    assert!(harness.registry.get("@fixture/pending#Pending").is_none());
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn activation_aggregates_broken_artifacts_while_steady_state_contains_them() {
    let activation = Harness::new(&["@fixture/import", "@fixture/malformed"]);
    activation
        .artifacts
        .register(
            &activation.context,
            failing_artifact("@fixture/import", "import broke"),
        )
        .unwrap();
    let mut malformed = contribution("@fixture/malformed", "Schema");
    malformed.schemas[0].name.clear();
    activation
        .artifacts
        .register(
            &activation.context,
            artifact("@fixture/malformed", malformed),
        )
        .unwrap();
    let _composition = activation
        .composition(&["@fixture/import", "@fixture/malformed"])
        .await;
    let error = activation
        .mount(TypertLoaderConfig::default())
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("2 typert contributor(s) failed to register"));
    assert!(message.contains("import broke"));
    assert!(message.contains("missing or empty name"));
    activation.context.root_fiber().dispose().await.unwrap();

    let steady = Harness::new(&["@fixture/steady"]);
    steady
        .artifacts
        .register(
            &steady.context,
            failing_artifact("@fixture/steady", "register failed"),
        )
        .unwrap();
    let composition = steady.composition(&[]).await;
    let loader = steady.mount(TypertLoaderConfig::default()).await.unwrap();
    composition
        .update_yaml("- name: \"@fixture/steady\"\n")
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert!(
        steady
            .registry
            .get_package("@fixture/steady", TypertFace::Host)
            .is_none()
    );
    assert_eq!(loader.fiber().state(), seekdeep_cordis::FiberState::Active);
    steady.context.root_fiber().dispose().await.unwrap();
}

#[test]
fn manifest_validation_rejects_ownership_face_model_and_invocation_defects() {
    let mut valid = contribution("pkg", "Schema");
    valid.invocations.push(invocation("pkg"));
    validate_typert_manifest("pkg", &valid).unwrap();

    let mut wrong_owner = valid.clone();
    wrong_owner.package = "other".to_owned();
    assert!(
        validate_typert_manifest("pkg", &wrong_owner)
            .unwrap_err()
            .to_string()
            .contains("must be owned by the package")
    );
    let mut client = valid.clone();
    client.face = TypertFace::Client;
    assert!(
        validate_typert_manifest("pkg", &client)
            .unwrap_err()
            .to_string()
            .contains("TYPERT.face is not \"host\"")
    );
    let mut empty_schema = valid.clone();
    empty_schema.schemas[0].name.clear();
    assert!(
        validate_typert_manifest("pkg", &empty_schema)
            .unwrap_err()
            .to_string()
            .contains("missing or empty name")
    );
    let mut invalid_invocation = valid.clone();
    invalid_invocation.invocations[0].method.clear();
    assert!(
        format!(
            "{:#}",
            validate_typert_manifest("pkg", &invalid_invocation).unwrap_err()
        )
        .contains("invalid invocation method")
    );
}

#[test]
fn manifest_validation_covers_service_event_object_member_and_type_fields() {
    let documentation = TypertDocumentation::default();
    let member = TypertMemberModel {
        kind: TypertMemberKind::Method,
        name: "member".to_owned(),
        signature: "member(): void".to_owned(),
        summary: None,
        js_doc: None,
    };
    let type_ = TypertTypeModel {
        name: "Value".to_owned(),
        declaration: "export interface Value {}".to_owned(),
    };
    let mut value = contribution("pkg", "Schema");
    value.model = TypertPackageModel {
        services: vec![TypertServiceModel {
            documentation: documentation.clone(),
            key: "service".to_owned(),
            export_name: "Service".to_owned(),
            members: vec![member.clone()],
            types: vec![type_.clone()],
        }],
        events: vec![TypertEventModel {
            documentation: documentation.clone(),
            name: "event/name".to_owned(),
            mode: Some("emit".to_owned()),
            signature: "'event/name'(): void".to_owned(),
        }],
        objects: vec![TypertObjectModel {
            documentation,
            name: "Object".to_owned(),
            export_name: "Object".to_owned(),
            members: vec![member],
            types: vec![type_],
        }],
    };
    validate_typert_manifest("pkg", &value).unwrap();
    value.model.services[0].members[0].signature.clear();
    assert!(
        validate_typert_manifest("pkg", &value)
            .unwrap_err()
            .to_string()
            .contains("missing or empty signature")
    );
    value.model.services[0].members[0].signature = "member(): void".to_owned();
    value.model.objects[0].types[0].declaration.clear();
    assert!(
        validate_typert_manifest("pkg", &value)
            .unwrap_err()
            .to_string()
            .contains("missing or empty declaration")
    );
}
