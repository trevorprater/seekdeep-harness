//! Behavioral mirror of `packages/api/gateway/tests/gateway.client.spec.ts`.

use std::{collections::VecDeque, sync::Arc};

use parking_lot::Mutex;
use seekdeep_api_gateway::client::{
    CLIENT_CONNECTION, ClientConnection, ClientConnectionFuture, ClientConnectionHandle,
    ClientRemoteArgument, ClientRemoteService,
};
use seekdeep_client_connection::RpcResult;
use seekdeep_cordis::{
    Context,
    fiber::{EffectHandle, Fiber},
};
use seekdeep_llm::AbortSignal;
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterDescriptor, InvocationParameterSource,
    InvocationReceiver, InvocationScope, RemoteFailure, RemoteResult, TypertBoundaryValue,
    TypertClientContextBinder, TypertClientRemote as _, TypertCodec, TypertContextRegistry as _,
    TypertRemoteContribution, TypertRemoteRegistry as _, TypertSchema,
};
use seekdeep_typert_registry::TypertRegistry;
use serde_json::{Map, Value, json};

#[derive(Clone, Debug)]
struct RecordedCall {
    channel: String,
    endpoint: String,
    payload: Value,
    signal: AbortSignal,
}

type CallHandler = Arc<dyn Fn(RecordedCall) -> ClientConnectionFuture + Send + Sync>;

struct MockConnection {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    handler: CallHandler,
}

impl ClientConnection for MockConnection {
    fn call(
        &self,
        channel: &str,
        endpoint: &str,
        payload: Value,
        signal: AbortSignal,
    ) -> ClientConnectionFuture {
        let call = RecordedCall {
            channel: channel.to_owned(),
            endpoint: endpoint.to_owned(),
            payload,
            signal,
        };
        self.calls.lock().push(call.clone());
        (self.handler)(call)
    }
}

struct Fixture {
    context: Context,
    registry: Arc<TypertRegistry>,
    client: Arc<ClientRemoteService>,
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    connection_effect: EffectHandle,
}

fn fixture(handler: CallHandler) -> Fixture {
    let context = Context::new();
    let registry = TypertRegistry::new();
    registry.provide(&context).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let connection = ClientConnectionHandle::new(Arc::new(MockConnection {
        calls: calls.clone(),
        handler,
    }));
    let connection_effect = context.provide(CLIENT_CONNECTION, connection).unwrap();
    let client = ClientRemoteService::install(&context).unwrap();
    Fixture {
        context,
        registry,
        client,
        calls,
        connection_effect,
    }
}

fn responding(results: Vec<RpcResult<Value>>) -> Fixture {
    let results = Arc::new(Mutex::new(VecDeque::from(results)));
    fixture(Arc::new(move |_call| {
        let result = results
            .lock()
            .pop_front()
            .unwrap_or_else(|| RpcResult::Success {
                value: Some(json!({"ref": "goal-default"})),
            });
        Box::pin(async move { Ok(result) })
    }))
}

fn successful(value: Value) -> RpcResult<Value> {
    RpcResult::Success { value: Some(value) }
}

fn boundary(value: Value) -> ClientRemoteArgument {
    ClientRemoteArgument::Boundary(TypertBoundaryValue::Json(value))
}

fn undefined() -> ClientRemoteArgument {
    ClientRemoteArgument::Boundary(TypertBoundaryValue::Undefined)
}

#[derive(Debug)]
struct NonEmptyStringSchema;

impl TypertSchema for NonEmptyStringSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(
            value
                .as_json()
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "expected non-empty string"
        );
        Ok(value)
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"type": "string", "minLength": 1}))
    }
}

#[derive(Debug)]
struct RequestSchema;

impl TypertSchema for RequestSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(
            value
                .as_json()
                .and_then(Value::as_object)
                .is_some_and(|object| {
                    object.len() == 1
                        && object
                            .get("objective")
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.is_empty())
                }),
            "expected objective request"
        );
        Ok(value)
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"type": "object"}))
    }
}

#[derive(Debug)]
enum ResultSchema {
    Create,
    Rename,
}

impl TypertSchema for ResultSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        let object = value
            .as_json()
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("expected result object"))?;
        match self {
            Self::Create => anyhow::ensure!(
                object.len() == 1
                    && object
                        .get("ref")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty()),
                "expected create result"
            ),
            Self::Rename => anyhow::ensure!(
                object.len() == 1 && object.get("renamed") == Some(&Value::Bool(true)),
                "expected rename result"
            ),
        }
        Ok(value)
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"type": "object"}))
    }
}

#[derive(Debug)]
struct MaybeSchema;

impl TypertSchema for MaybeSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(
            value.is_undefined()
                || value
                    .as_json()
                    .is_some_and(|value| value.is_null() || value.is_string()),
            "expected string, null, or undefined"
        );
        Ok(value)
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"type": ["string", "null"]}))
    }
}

fn strict(symbol: &str, schema: Arc<dyn TypertSchema>) -> TypertCodec {
    TypertCodec::Strict {
        type_symbol: symbol.to_owned(),
        schema,
    }
}

fn parameter(
    name: &str,
    wire: &str,
    source: InvocationParameterSource,
    lookup: Option<&str>,
    codec: TypertCodec,
) -> InvocationParameterDescriptor {
    InvocationParameterDescriptor {
        name: name.to_owned(),
        wire: wire.to_owned(),
        source,
        lookup: lookup.map(str::to_owned),
        codec,
        accepts_undefined: None,
    }
}

fn direct_descriptor() -> InvocationDescriptor {
    InvocationDescriptor {
        id: "@fixture/probe#probe/create".to_owned(),
        service: "probe".to_owned(),
        namespace: "probe".to_owned(),
        method: "create".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: Some(InvocationScope {
            context: "fixture".to_owned(),
            wire: "agentId".to_owned(),
        }),
        parameters: vec![
            parameter(
                "agent",
                "agentId",
                InvocationParameterSource::Lookup,
                Some("fixture"),
                strict("@fixture#AgentId", Arc::new(NonEmptyStringSchema)),
            ),
            parameter(
                "request",
                "request",
                InvocationParameterSource::Json,
                None,
                strict("@fixture#CreateRequest", Arc::new(RequestSchema)),
            ),
        ],
        cancellation: true,
        result: strict("@fixture#CreateResult", Arc::new(ResultSchema::Create)),
        source_location: None,
    }
}

fn context_descriptor() -> InvocationDescriptor {
    InvocationDescriptor {
        id: "@fixture/probe#probe/rename".to_owned(),
        service: "probe".to_owned(),
        namespace: "probe".to_owned(),
        method: "rename".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Context {
            context: "fixture".to_owned(),
            wire: "agentId".to_owned(),
            codec: strict("@fixture#AgentId", Arc::new(NonEmptyStringSchema)),
        },
        scope: None,
        parameters: vec![parameter(
            "request",
            "request",
            InvocationParameterSource::Json,
            None,
            strict("@fixture#RenameRequest", Arc::new(RequestSchema)),
        )],
        cancellation: false,
        result: strict("@fixture#RenameResult", Arc::new(ResultSchema::Rename)),
        source_location: None,
    }
}

fn maybe_descriptor() -> InvocationDescriptor {
    let codec = strict("@fixture#MaybeValue", Arc::new(MaybeSchema));
    let mut value = parameter(
        "value",
        "value",
        InvocationParameterSource::Json,
        None,
        codec.clone(),
    );
    value.accepts_undefined = Some(true);
    InvocationDescriptor {
        id: "@fixture/probe#probe/maybe".to_owned(),
        service: "probe".to_owned(),
        namespace: "probe".to_owned(),
        method: "maybe".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: None,
        parameters: vec![value],
        cancellation: false,
        result: codec,
        source_location: None,
    }
}

fn contribution(package: &str, descriptors: Vec<InvocationDescriptor>) -> TypertRemoteContribution {
    TypertRemoteContribution {
        package: package.to_owned(),
        descriptors,
    }
}

fn register_client_identity(fixture: &Fixture) -> EffectHandle {
    fixture
        .registry
        .contexts()
        .register_client(
            &fixture.context,
            "fixture",
            TypertClientContextBinder {
                identity: Arc::new(|context| context.meta("fixtureId")),
            },
        )
        .unwrap()
}

fn assert_success(result: RemoteResult<TypertBoundaryValue>, expected: &TypertBoundaryValue) {
    let RemoteResult::Success { value } = result else {
        panic!("expected successful Remote result");
    };
    assert_eq!(&value, expected);
}

fn assert_first_direct_call(fixture: &Fixture) {
    let calls = fixture.calls.lock();
    assert_eq!(calls[0].channel, "/api");
    assert_eq!(calls[0].endpoint, "probe/create");
    assert_eq!(
        calls[0].payload,
        json!({"args": {
            "agentId": "agent-1",
            "request": {"objective": "ship"},
        }})
    );
}

fn assert_internal(result: RemoteResult<TypertBoundaryValue>, message: &str) {
    let RemoteResult::Failure { error } = result else {
        panic!("expected internal Remote failure");
    };
    assert_eq!(error.code, "internal");
    assert_eq!(error.message, message);
    assert!(error.details.is_empty());
}

async fn invoke_probe(
    fixture: &Fixture,
    caller: &Context,
    method: &str,
    arguments: Vec<ClientRemoteArgument>,
) -> anyhow::Result<RemoteResult<TypertBoundaryValue>> {
    fixture
        .client
        .invoke(caller, "probe", method, arguments)
        .await
}

#[tokio::test]
async fn mounts_direct_methods_validates_boundaries_and_withdraws_retained_handles() {
    let fixture = responding(vec![
        successful(json!({"ref": "goal-1"})),
        successful(json!({"ref": "goal-1"})),
        successful(json!({"ref": 1})),
    ]);
    let mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/probe", vec![direct_descriptor()]),
        )
        .await
        .unwrap();
    let retained = fixture
        .client
        .method(&fixture.context, "probe", "create")
        .unwrap();

    let result = invoke_probe(
        &fixture,
        &fixture.context,
        "create",
        vec![
            boundary(json!("agent-1")),
            boundary(json!({"objective": "ship"})),
        ],
    )
    .await
    .unwrap();
    assert_success(result, &TypertBoundaryValue::json(json!({"ref": "goal-1"})));
    assert_first_direct_call(&fixture);

    let caller_signal = AbortSignal::default();
    let result = invoke_probe(
        &fixture,
        &fixture.context,
        "create",
        vec![
            boundary(json!("agent-1")),
            boundary(json!({"objective": "cancel me"})),
            ClientRemoteArgument::Signal(Some(caller_signal.clone())),
        ],
    )
    .await
    .unwrap();
    assert_success(result, &TypertBoundaryValue::json(json!({"ref": "goal-1"})));
    let combined = fixture.calls.lock()[1].signal.clone();
    assert_ne!(combined, caller_signal);
    caller_signal.abort_with_reason(json!("caller cancelled"));
    assert!(combined.is_aborted());
    assert_eq!(combined.reason(), Some(json!("caller cancelled")));

    let error = invoke_probe(
        &fixture,
        &fixture.context,
        "create",
        vec![boundary(json!("")), boundary(json!({"objective": "ship"}))],
    )
    .await
    .expect_err("empty Agent id unexpectedly passed");
    assert!(error.to_string().contains("rejected \"agentId\""));

    let result = invoke_probe(
        &fixture,
        &fixture.context,
        "create",
        vec![
            boundary(json!("agent-1")),
            boundary(json!({"objective": "ship"})),
        ],
    )
    .await
    .unwrap();
    assert_internal(
        result,
        "client api: probe/create failed: client api: probe/create rejected \"result\"",
    );

    mount.dispose().await.unwrap();
    assert!(fixture.client.namespace("probe").is_none());
    assert!(
        fixture
            .context
            .get_named::<seekdeep_api_gateway::client::RemoteNamespaceService>("remote.probe")
            .is_none()
    );
    assert!(fixture.registry.remotes().list().is_empty());
    let result = retained
        .invoke(vec![
            boundary(json!("agent-1")),
            boundary(json!({"objective": "ship"})),
        ])
        .await
        .unwrap();
    assert_internal(
        result,
        "client api: Remote method probe/create is no longer mounted",
    );
}

#[tokio::test]
async fn omitted_undefined_and_explicit_null_keep_distinct_carrier_shapes() {
    let fixture = responding(vec![
        RpcResult::Success { value: None },
        RpcResult::Success {
            value: Some(Value::Null),
        },
    ]);
    let mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/maybe", vec![maybe_descriptor()]),
        )
        .await
        .unwrap();

    assert_success(
        fixture
            .client
            .invoke(&fixture.context, "probe", "maybe", vec![undefined()])
            .await
            .unwrap(),
        &TypertBoundaryValue::Undefined,
    );
    assert_success(
        fixture
            .client
            .invoke(
                &fixture.context,
                "probe",
                "maybe",
                vec![boundary(Value::Null)],
            )
            .await
            .unwrap(),
        &TypertBoundaryValue::json(Value::Null),
    );
    {
        let calls = fixture.calls.lock();
        assert_eq!(calls[0].payload, json!({"args": {}}));
        assert_eq!(calls[1].payload, json!({"args": {"value": null}}));
    }
    mount.dispose().await.unwrap();
}

#[tokio::test]
async fn direct_lookup_scope_and_context_receiver_use_the_calling_context_identity() {
    let fixture = responding(vec![
        successful(json!({"ref": "goal-2"})),
        successful(json!({"renamed": true})),
    ]);
    register_client_identity(&fixture);
    let caller = fixture.context.with_meta("fixtureId", json!("agent-2"));
    let mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution(
                "@fixture/scoped",
                vec![direct_descriptor(), context_descriptor()],
            ),
        )
        .await
        .unwrap();

    assert_success(
        fixture
            .client
            .invoke(
                &caller,
                "probe",
                "create",
                vec![boundary(json!({"objective": "ship scoped"}))],
            )
            .await
            .unwrap(),
        &TypertBoundaryValue::json(json!({"ref": "goal-2"})),
    );
    assert_success(
        fixture
            .client
            .invoke(
                &caller,
                "probe",
                "rename",
                vec![boundary(json!({"objective": "land"}))],
            )
            .await
            .unwrap(),
        &TypertBoundaryValue::json(json!({"renamed": true})),
    );
    {
        let calls = fixture.calls.lock();
        assert_eq!(
            calls[0].payload,
            json!({"args": {
                "agentId": "agent-2",
                "request": {"objective": "ship scoped"},
            }})
        );
        assert_eq!(
            calls[1].payload,
            json!({"args": {
                "agentId": "agent-2",
                "request": {"objective": "land"},
            }})
        );
    }

    let error = fixture
        .client
        .invoke(
            &fixture.context,
            "probe",
            "rename",
            vec![boundary(json!({"objective": "land"}))],
        )
        .await
        .expect_err("unscoped Context receiver unexpectedly resolved");
    assert!(error.to_string().contains("requires a \"fixture\" Context"));
    mount.dispose().await.unwrap();
}

#[tokio::test]
async fn scoped_alias_falls_back_to_direct_arity_outside_a_bound_context() {
    let fixture = responding(Vec::new());
    register_client_identity(&fixture);
    let mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/probe", vec![direct_descriptor()]),
        )
        .await
        .unwrap();

    let error = fixture
        .client
        .invoke(
            &fixture.context,
            "probe",
            "create",
            vec![boundary(json!({"objective": "wrong scope"}))],
        )
        .await
        .expect_err("root call unexpectedly selected scoped projection");
    assert!(
        error
            .to_string()
            .contains("expected 2 business argument(s) plus an optional AbortSignal, got 1")
    );
    mount.dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_weak_descriptors_and_namespace_or_method_collisions_before_registration() {
    let fixture = responding(Vec::new());
    let mut weak = direct_descriptor();
    weak.result = TypertCodec::SrcJson;
    let error = fixture
        .client
        .mount(&fixture.context, contribution("@fixture/weak", vec![weak]))
        .await
        .expect_err("weak descriptor unexpectedly mounted");
    assert!(error.to_string().contains("has no strict codec"));

    let mut remote_conflict = direct_descriptor();
    remote_conflict.namespace = "$mount".to_owned();
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/remote-conflict", vec![remote_conflict]),
        )
        .await
        .expect_err("Remote service namespace conflict unexpectedly mounted");
    assert!(
        error
            .to_string()
            .contains("conflicts with the Remote service")
    );

    let existing = fixture
        .context
        .provide_named("remote.typert", Arc::new(1_u64))
        .unwrap();
    let mut existing_conflict = context_descriptor();
    existing_conflict.namespace = "typert".to_owned();
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/existing-conflict", vec![existing_conflict]),
        )
        .await
        .expect_err("existing namespace conflict unexpectedly mounted");
    assert!(
        error
            .to_string()
            .contains("conflicts with an existing Remote namespace")
    );
    existing.dispose().await.unwrap();

    let mut method_conflict = context_descriptor();
    method_conflict.id = "@fixture/probe#probe/remove".to_owned();
    method_conflict.method = "remove".to_owned();
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/method-conflict", vec![method_conflict]),
        )
        .await
        .expect_err("namespace method conflict unexpectedly mounted");
    assert!(
        error
            .to_string()
            .contains("conflicts with its namespace service")
    );
    assert!(fixture.registry.remotes().list().is_empty());
}

#[tokio::test]
async fn rejects_duplicate_and_live_direct_or_scoped_variants_atomically() {
    let fixture = responding(Vec::new());
    let direct = direct_descriptor();
    let mut direct_again = direct.clone();
    direct_again.id = "@fixture/probe#probe/create-again".to_owned();
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution(
                "@fixture/direct-duplicates",
                vec![direct.clone(), direct_again],
            ),
        )
        .await
        .expect_err("duplicate direct methods unexpectedly mounted");
    assert!(error.to_string().contains("repeats direct method"));

    let scoped = context_descriptor();
    let mut scoped_again = scoped.clone();
    scoped_again.id = "@fixture/probe#probe/rename-again".to_owned();
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution(
                "@fixture/scoped-duplicates",
                vec![scoped.clone(), scoped_again],
            ),
        )
        .await
        .expect_err("duplicate scoped methods unexpectedly mounted");
    assert!(error.to_string().contains("repeats scoped method"));
    assert!(fixture.registry.remotes().list().is_empty());

    let direct_mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/direct-live", vec![direct.clone()]),
        )
        .await
        .unwrap();
    let mut other_direct = direct;
    other_direct.id = "@fixture/other#probe/create".to_owned();
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/direct-conflict", vec![other_direct]),
        )
        .await
        .expect_err("live direct conflict unexpectedly mounted");
    assert!(
        error
            .to_string()
            .contains("direct method probe/create is already mounted")
    );
    direct_mount.dispose().await.unwrap();

    let scoped_mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/scoped-live", vec![scoped.clone()]),
        )
        .await
        .unwrap();
    let mut other_scoped = scoped;
    other_scoped.id = "@fixture/other#probe/rename".to_owned();
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/scoped-conflict", vec![other_scoped]),
        )
        .await
        .expect_err("live scoped conflict unexpectedly mounted");
    assert!(
        error
            .to_string()
            .contains("scoped method probe/rename is already mounted")
    );
    scoped_mount.dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_weak_parameter_context_codecs_and_malformed_scope_projection() {
    let fixture = responding(Vec::new());
    let mut weak_parameter = direct_descriptor();
    weak_parameter.parameters[0].codec = TypertCodec::SrcJson;
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/weak-parameter", vec![weak_parameter]),
        )
        .await
        .expect_err("weak parameter unexpectedly mounted");
    assert!(error.to_string().contains("has no strict codec"));

    let mut weak_context = context_descriptor();
    let InvocationReceiver::Context { codec, .. } = &mut weak_context.invocation else {
        panic!("fixture descriptor lost Context receiver");
    };
    *codec = TypertCodec::SrcJson;
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/weak-context", vec![weak_context]),
        )
        .await
        .expect_err("weak Context codec unexpectedly mounted");
    assert!(error.to_string().contains("has no strict codec"));

    let mut malformed = direct_descriptor();
    malformed.scope = Some(InvocationScope {
        context: "fixture".to_owned(),
        wire: "missingId".to_owned(),
    });
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/malformed-scope", vec![malformed]),
        )
        .await
        .expect_err("malformed scope unexpectedly mounted");
    assert!(
        error
            .to_string()
            .contains("scope must select its only lookup parameter")
    );

    let mut ambiguous = direct_descriptor();
    ambiguous.parameters.push(parameter(
        "other",
        "otherId",
        InvocationParameterSource::Lookup,
        Some("fixture"),
        strict("@fixture#AgentId", Arc::new(NonEmptyStringSchema)),
    ));
    let error = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/ambiguous-scope", vec![ambiguous]),
        )
        .await
        .expect_err("ambiguous scope unexpectedly mounted");
    assert!(
        error
            .to_string()
            .contains("scope must select its only lookup parameter")
    );
}

#[tokio::test]
async fn validates_arity_binders_and_live_connection_before_carrier_dispatch() {
    let fixture = responding(Vec::new());
    let mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution(
                "@fixture/probe",
                vec![direct_descriptor(), context_descriptor()],
            ),
        )
        .await
        .unwrap();

    let error = fixture
        .client
        .invoke(
            &fixture.context,
            "probe",
            "create",
            vec![boundary(json!("agent-1"))],
        )
        .await
        .expect_err("short invocation unexpectedly dispatched");
    assert!(
        error
            .to_string()
            .contains("expected 2 business argument(s)")
    );
    let error = fixture
        .client
        .invoke(
            &fixture.context,
            "probe",
            "create",
            vec![
                boundary(json!("agent-1")),
                boundary(json!({"objective": "ship"})),
                undefined(),
                boundary(json!("extra")),
            ],
        )
        .await
        .expect_err("long invocation unexpectedly dispatched");
    assert!(error.to_string().contains("got 4"));
    let error = fixture
        .client
        .invoke(&fixture.context, "probe", "rename", Vec::new())
        .await
        .expect_err("zero-arity scoped call unexpectedly dispatched");
    assert!(error.to_string().contains("expected 1 argument(s), got 0"));
    let error = fixture
        .client
        .invoke(
            &fixture.context,
            "probe",
            "rename",
            vec![boundary(json!({"objective": "ship"}))],
        )
        .await
        .expect_err("missing binder unexpectedly dispatched");
    assert!(error.to_string().contains("no Client Context binder"));

    fixture.connection_effect.dispose().await.unwrap();
    let error = fixture
        .client
        .invoke(
            &fixture.context,
            "probe",
            "create",
            vec![
                boundary(json!("agent-1")),
                boundary(json!({"objective": "ship"})),
            ],
        )
        .await
        .expect_err("missing Connection unexpectedly dispatched");
    assert!(error.to_string().contains("no active Connection"));
    mount.dispose().await.unwrap();
}

#[tokio::test]
async fn withdrawing_pending_invocation_returns_withdrawn_and_namespace_waits_for_last_method() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let receiver = Arc::new(Mutex::new(Some(receiver)));
    let pending_fixture = fixture(Arc::new(move |_call| {
        let receiver = receiver
            .lock()
            .take()
            .expect("fixture pending call may run only once");
        Box::pin(async move { receiver.await.map_err(Into::into) })
    }));
    let mut archive = direct_descriptor();
    archive.id = "@fixture/probe#probe/archive".to_owned();
    archive.method = "archive".to_owned();
    archive.scope = None;
    let first = pending_fixture
        .client
        .mount(
            &pending_fixture.context,
            contribution("@fixture/create", vec![direct_descriptor()]),
        )
        .await
        .unwrap();
    let second = pending_fixture
        .client
        .mount(
            &pending_fixture.context,
            contribution("@fixture/archive", vec![archive]),
        )
        .await
        .unwrap();

    let retained = pending_fixture
        .client
        .method(&pending_fixture.context, "probe", "create")
        .unwrap();
    let invocation = tokio::spawn(async move {
        retained
            .invoke(vec![
                boundary(json!("agent-1")),
                boundary(json!({"objective": "ship"})),
            ])
            .await
    });
    tokio::task::yield_now().await;
    first.dispose().await.unwrap();
    assert!(pending_fixture.client.namespace("probe").is_some());
    sender
        .send(successful(json!({"ref": "goal-1"})))
        .expect("pending receiver disappeared");
    let result = invocation.await.unwrap().unwrap();
    assert_internal(
        result,
        "client api: Remote method probe/create is no longer mounted",
    );

    second.dispose().await.unwrap();
    assert!(pending_fixture.client.namespace("probe").is_none());
}

#[tokio::test]
async fn prototype_named_wire_is_an_own_map_entry() {
    let fixture = responding(vec![successful(json!({"ref": "goal-1"}))]);
    let mut descriptor = direct_descriptor();
    descriptor.id = "@fixture/probe#probe/prototype".to_owned();
    descriptor.method = "prototype".to_owned();
    descriptor.scope = None;
    descriptor.parameters = vec![parameter(
        "value",
        "__proto__",
        InvocationParameterSource::Json,
        None,
        strict("@fixture#PrototypeValue", Arc::new(NonEmptyStringSchema)),
    )];
    let mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/prototype", vec![descriptor]),
        )
        .await
        .unwrap();
    assert_success(
        fixture
            .client
            .invoke(
                &fixture.context,
                "probe",
                "prototype",
                vec![boundary(json!("wire-value"))],
            )
            .await
            .unwrap(),
        &TypertBoundaryValue::json(json!({"ref": "goal-1"})),
    );
    let payload = fixture.calls.lock()[0].payload.clone();
    assert_eq!(payload["args"]["__proto__"], json!("wire-value"));
    assert_eq!(payload["args"].as_object().unwrap().len(), 1);
    mount.dispose().await.unwrap();
}

#[tokio::test]
async fn namespace_is_released_for_a_replacement_after_last_scoped_method_leaves() {
    let fixture = responding(Vec::new());
    let mount = fixture
        .client
        .mount(
            &fixture.context,
            contribution("@fixture/scoped", vec![context_descriptor()]),
        )
        .await
        .unwrap();
    assert!(fixture.context.has_named("remote.probe"));
    mount.dispose().await.unwrap();
    assert!(!fixture.context.has_named("remote.probe"));

    let replacement = fixture
        .context
        .provide_named("remote.probe", Arc::new("replacement".to_owned()))
        .unwrap();
    assert!(fixture.context.has_named("remote.probe"));
    replacement.dispose().await.unwrap();
}

#[tokio::test]
async fn host_failures_are_verbatim_and_transport_throws_fold_into_remote_failure() {
    let host_failure = RemoteFailure {
        code: "agent-busy".to_owned(),
        message: "host failed".to_owned(),
        details: Map::new(),
    };
    let expected = host_failure.clone();
    let host_fixture = fixture(Arc::new(move |_call| {
        let failure = host_failure.clone();
        Box::pin(async move {
            Ok(RpcResult::Failure {
                error: seekdeep_client_connection::RpcError {
                    code: failure.code,
                    message: failure.message,
                    details: failure.details,
                },
            })
        })
    }));
    let mount = host_fixture
        .client
        .mount(
            &host_fixture.context,
            contribution("@fixture/probe", vec![direct_descriptor()]),
        )
        .await
        .unwrap();
    assert_eq!(
        host_fixture
            .client
            .invoke(
                &host_fixture.context,
                "probe",
                "create",
                vec![
                    boundary(json!("agent-1")),
                    boundary(json!({"objective": "ship"})),
                ],
            )
            .await
            .unwrap(),
        RemoteResult::Failure { error: expected }
    );
    mount.dispose().await.unwrap();

    let offline_fixture = fixture(Arc::new(|_call| {
        Box::pin(async { Err(anyhow::anyhow!("carrier offline")) })
    }));
    let mount = offline_fixture
        .client
        .mount(
            &offline_fixture.context,
            contribution("@fixture/offline", vec![direct_descriptor()]),
        )
        .await
        .unwrap();
    let result = offline_fixture
        .client
        .invoke(
            &offline_fixture.context,
            "probe",
            "create",
            vec![
                boundary(json!("agent-1")),
                boundary(json!({"objective": "ship"})),
            ],
        )
        .await
        .unwrap();
    assert_internal(result, "client api: probe/create failed: carrier offline");
    mount.dispose().await.unwrap();
}

#[tokio::test]
async fn event_subscriptions_are_owned_distinct_snapshot_delivered_and_failure_contained() {
    let fixture = responding(Vec::new());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let shared_seen = seen.clone();
    let listener: seekdeep_typert_protocol::TypertRemoteEventListener = Arc::new(move |args| {
        let seen = shared_seen.clone();
        Box::pin(async move {
            seen.lock().push(args[0].as_str().unwrap().to_owned());
            Ok(())
        })
    });
    let first = fixture
        .client
        .on(&fixture.context, "fixture/changed", listener.clone())
        .unwrap();
    let _second = fixture
        .client
        .on(&fixture.context, "fixture/changed", listener)
        .unwrap();
    let failing = fixture
        .client
        .on(
            &fixture.context,
            "fixture/changed",
            Arc::new(|_args| Box::pin(async { anyhow::bail!("listener failed") })),
        )
        .unwrap();
    fixture
        .client
        .dispatch("fixture/changed", vec![json!("both")]);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    let mut both = seen.lock().clone();
    both.sort();
    assert_eq!(both, ["both", "both"]);

    first.dispose().await.unwrap();
    first.dispose().await.unwrap();
    failing.dispose().await.unwrap();
    fixture
        .client
        .dispatch("fixture/changed", vec![json!("survivor")]);
    fixture.client.dispatch("fixture/idle", vec![json!(1)]);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(seen.lock().last().map(String::as_str), Some("survivor"));
    assert_eq!(seen.lock().len(), 3);
}

#[tokio::test]
async fn event_subscription_leaves_with_its_calling_fiber() {
    let fixture = responding(Vec::new());
    let fiber = Fiber::active_child("remote event subscriber");
    let subscriber = fixture.context.with_fiber(fiber.clone());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let listener_seen = seen.clone();
    fixture
        .client
        .on(
            &subscriber,
            "fixture/changed",
            Arc::new(move |args| {
                let seen = listener_seen.clone();
                Box::pin(async move {
                    seen.lock().push(args[0].as_str().unwrap().to_owned());
                    Ok(())
                })
            }),
        )
        .unwrap();
    fixture
        .client
        .dispatch("fixture/changed", vec![json!("settings")]);
    tokio::task::yield_now().await;
    assert_eq!(*seen.lock(), ["settings"]);

    fiber.dispose().await.unwrap();
    fixture
        .client
        .dispatch("fixture/changed", vec![json!("after disposal")]);
    tokio::task::yield_now().await;
    assert_eq!(*seen.lock(), ["settings"]);
}
