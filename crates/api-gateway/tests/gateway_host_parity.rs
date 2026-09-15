//! Behavioral mirror of `packages/api/gateway/tests/gateway.host.spec.ts`.

use std::{
    collections::HashMap,
    error::Error as _,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_api_gateway::{
    GatewayRpcResult, InvokeRemoteRequest, TypertGatewayError, TypertGatewayErrorCode,
    TypertGatewayService, TypertServiceDirectory, install,
};
use seekdeep_client_connection::{
    ClientRequest, HostConnectionService, HttpMethod, HttpRequest, HttpResponse, RpcId,
};
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterDescriptor, InvocationParameterSource,
    InvocationReceiver, RemoteInvocationMarker, RemoteMethodMarker, TypertBoundaryValue,
    TypertCodec, TypertContextRegistry as _, TypertHostArgument, TypertHostContextProvider,
    TypertInvocableService, TypertInvocationFuture, TypertLookupFailure, TypertLookupProvider,
    TypertLookupRegistry as _, TypertSchema,
};
use seekdeep_typert_registry::{
    TypertContribution, TypertFace, TypertPackageModel, TypertRegistry,
};
use serde_json::{Value, json};

const GOALS: ServiceKey<GoalState> = ServiceKey::new("goals");

#[derive(Debug)]
struct FixtureAgent {
    id: String,
}

#[derive(Debug, Default)]
struct GoalState {
    calls: Mutex<Vec<String>>,
    last_signal: Mutex<Option<AbortSignal>>,
    next_result: Mutex<Option<TypertBoundaryValue>>,
    business_error: Mutex<Option<Arc<FixtureBusinessError>>>,
    marker_reads: AtomicUsize,
}

#[derive(Debug)]
struct FixtureBusinessError(&'static str);

impl fmt::Display for FixtureBusinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for FixtureBusinessError {}

struct GoalService {
    context: Context,
    state: Arc<GoalState>,
}

impl GoalService {
    fn scope(&self) -> String {
        self.context
            .meta("fixtureScope")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "root".to_owned())
    }

    fn record(&self, method: &str) {
        self.state.calls.lock().push(method.to_owned());
    }
}

#[derive(Clone, Copy)]
enum SimpleResult {
    Echo,
    Pong,
}

struct SimpleService {
    service_key: String,
    namespace: String,
    visible_binding: bool,
    markers: Vec<RemoteMethodMarker>,
    parameters: HashMap<String, Vec<String>>,
    callable: bool,
    result: SimpleResult,
}

impl SimpleService {
    fn direct(service_key: &str, namespace: &str, method: &str, parameters: &[&str]) -> Self {
        Self {
            service_key: service_key.to_owned(),
            namespace: namespace.to_owned(),
            visible_binding: true,
            markers: vec![marker(method, None, RemoteInvocationMarker::Direct)],
            parameters: HashMap::from([(
                method.to_owned(),
                parameters.iter().map(|name| (*name).to_owned()).collect(),
            )]),
            callable: true,
            result: SimpleResult::Echo,
        }
    }
}

impl TypertInvocableService for SimpleService {
    fn has_visible_binding(&self) -> bool {
        self.visible_binding
    }

    fn service_key(&self) -> &str {
        &self.service_key
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        self.markers.clone()
    }

    fn parameter_names(&self, implementation: &str) -> Option<Vec<String>> {
        self.parameters.get(implementation).cloned()
    }

    fn has_method(&self, _implementation: &str) -> bool {
        self.callable
    }

    fn invoke(
        self: Arc<Self>,
        _implementation: &str,
        arguments: Vec<TypertHostArgument>,
    ) -> TypertInvocationFuture {
        Box::pin(async move {
            match self.result {
                SimpleResult::Pong => Ok(TypertBoundaryValue::json(json!("pong"))),
                SimpleResult::Echo => {
                    let Some(TypertHostArgument::Boundary(value)) = arguments.first() else {
                        return Ok(TypertBoundaryValue::Undefined);
                    };
                    Ok(value.clone())
                }
            }
        })
    }
}

impl TypertInvocableService for GoalService {
    fn service_key(&self) -> &'static str {
        "goals"
    }

    fn namespace(&self) -> &'static str {
        "goals"
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        self.state.marker_reads.fetch_add(1, Ordering::AcqRel);
        vec![
            marker("create", None, RemoteInvocationMarker::Direct),
            marker(
                "rename",
                None,
                RemoteInvocationMarker::Context {
                    context: "gatewayFixture".to_owned(),
                },
            ),
            marker("passthrough", None, RemoteInvocationMarker::Direct),
            marker("maybe", None, RemoteInvocationMarker::Direct),
            marker("fail", None, RemoteInvocationMarker::Direct),
        ]
    }

    fn parameter_names(&self, implementation: &str) -> Option<Vec<String>> {
        let names: &[&str] = match implementation {
            "create" => &["agent", "request", "signal"],
            "rename" | "fail" | "strictOnly" => &["request"],
            "passthrough" | "maybe" => &["value"],
            _ => return None,
        };
        Some(names.iter().map(|name| (*name).to_owned()).collect())
    }

    fn has_method(&self, implementation: &str) -> bool {
        matches!(
            implementation,
            "create" | "rename" | "passthrough" | "maybe" | "fail" | "strictOnly"
        )
    }

    fn invoke(
        self: Arc<Self>,
        implementation: &str,
        arguments: Vec<TypertHostArgument>,
    ) -> TypertInvocationFuture {
        let implementation = implementation.to_owned();
        Box::pin(async move {
            self.record(&implementation);
            match implementation.as_str() {
                "create" => self.create(&arguments),
                "rename" => self.rename(&arguments),
                "passthrough" | "maybe" => self.passthrough(&arguments),
                "strictOnly" => self.strict_only(&arguments),
                "fail" => self.state.business_error.lock().clone().map_or_else(
                    || Err(anyhow::anyhow!("fixture business failure")),
                    |error| Err(anyhow::Error::new(error)),
                ),
                _ => anyhow::bail!("goals has no callable method {implementation:?}"),
            }
        })
    }
}

impl GoalService {
    fn create(&self, arguments: &[TypertHostArgument]) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(arguments.len() == 3, "create expects three arguments");
        let TypertHostArgument::Lookup(agent) = &arguments[0] else {
            anyhow::bail!("create expected an agent lookup");
        };
        let agent = agent
            .clone()
            .downcast::<FixtureAgent>()
            .map_err(|_| anyhow::anyhow!("create received the wrong lookup type"))?;
        let title = request_title(&arguments[1])?;
        let TypertHostArgument::Signal(signal) = &arguments[2] else {
            anyhow::bail!("create expected a signal");
        };
        *self.state.last_signal.lock() = Some(signal.clone());
        Ok(TypertBoundaryValue::json(json!({
            "agentId": agent.id,
            "title": title,
            "scope": self.scope(),
        })))
    }

    fn rename(&self, arguments: &[TypertHostArgument]) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(arguments.len() == 1, "rename expects one argument");
        Ok(TypertBoundaryValue::json(json!({
            "title": request_title(&arguments[0])?,
            "scope": self.scope(),
        })))
    }

    fn passthrough(&self, arguments: &[TypertHostArgument]) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(arguments.len() == 1, "passthrough expects one argument");
        if let Some(result) = self.state.next_result.lock().clone() {
            return Ok(result);
        }
        let TypertHostArgument::Boundary(value) = &arguments[0] else {
            anyhow::bail!("passthrough expected a boundary value");
        };
        Ok(value.clone())
    }

    fn strict_only(&self, arguments: &[TypertHostArgument]) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(arguments.len() == 1, "strictOnly expects one argument");
        self.passthrough(arguments)
    }
}

fn request_title(argument: &TypertHostArgument) -> anyhow::Result<String> {
    let TypertHostArgument::Boundary(TypertBoundaryValue::Json(Value::Object(request))) = argument
    else {
        anyhow::bail!("expected a request object");
    };
    request
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("expected a title"))
}

fn marker(
    method: &str,
    export_name: Option<&str>,
    invocation: RemoteInvocationMarker,
) -> RemoteMethodMarker {
    RemoteMethodMarker {
        method: method.to_owned(),
        export_name: export_name.map(str::to_owned),
        invocation,
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

#[derive(Debug)]
struct RequestSchema {
    trim: bool,
}

impl TypertSchema for RequestSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        let TypertBoundaryValue::Json(Value::Object(mut object)) = value else {
            anyhow::bail!("expected request object");
        };
        anyhow::ensure!(object.len() == 1, "expected exactly one title field");
        let title = object
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("expected string title"))?;
        if self.trim {
            object.insert("title".to_owned(), Value::String(title.trim().to_owned()));
        }
        Ok(TypertBoundaryValue::Json(Value::Object(object)))
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
            "additionalProperties": false,
        }))
    }
}

#[derive(Debug)]
struct ResultSchema {
    fields: &'static [&'static str],
}

impl TypertSchema for ResultSchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        let Some(object) = value.as_json().and_then(Value::as_object) else {
            anyhow::bail!("expected result object");
        };
        anyhow::ensure!(
            object.len() == self.fields.len()
                && self
                    .fields
                    .iter()
                    .all(|field| object.get(*field).is_some_and(Value::is_string)),
            "expected exact string result fields"
        );
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

fn strict(type_symbol: &str, schema: Arc<dyn TypertSchema>) -> TypertCodec {
    TypertCodec::Strict {
        type_symbol: type_symbol.to_owned(),
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

fn create_descriptor() -> InvocationDescriptor {
    InvocationDescriptor {
        id: "@fixture/gateway#goals/create".to_owned(),
        service: "goals".to_owned(),
        namespace: "goals".to_owned(),
        method: "create".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: None,
        parameters: vec![
            parameter(
                "agent",
                "agentId",
                InvocationParameterSource::Lookup,
                Some("gatewayFixture"),
                strict("@fixture/domain#AgentId", Arc::new(StringSchema)),
            ),
            parameter(
                "request",
                "request",
                InvocationParameterSource::Json,
                None,
                strict(
                    "@fixture/gateway#CreateRequest",
                    Arc::new(RequestSchema { trim: true }),
                ),
            ),
        ],
        cancellation: true,
        result: strict(
            "@fixture/gateway#CreateResult",
            Arc::new(ResultSchema {
                fields: &["agentId", "title", "scope"],
            }),
        ),
        source_location: None,
    }
}

fn rename_descriptor() -> InvocationDescriptor {
    InvocationDescriptor {
        id: "@fixture/gateway#goals/rename".to_owned(),
        service: "goals".to_owned(),
        namespace: "goals".to_owned(),
        method: "rename".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Context {
            context: "gatewayFixture".to_owned(),
            wire: "agentId".to_owned(),
            codec: strict("@fixture/domain#AgentId", Arc::new(StringSchema)),
        },
        scope: None,
        parameters: vec![parameter(
            "request",
            "request",
            InvocationParameterSource::Json,
            None,
            strict(
                "@fixture/gateway#RenameRequest",
                Arc::new(RequestSchema { trim: false }),
            ),
        )],
        cancellation: false,
        result: strict(
            "@fixture/gateway#RenameResult",
            Arc::new(ResultSchema {
                fields: &["title", "scope"],
            }),
        ),
        source_location: None,
    }
}

fn passthrough_descriptor() -> InvocationDescriptor {
    InvocationDescriptor {
        id: "@fixture/gateway#goals/passthrough".to_owned(),
        service: "goals".to_owned(),
        namespace: "goals".to_owned(),
        method: "passthrough".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: None,
        parameters: vec![parameter(
            "value",
            "value",
            InvocationParameterSource::Json,
            None,
            TypertCodec::SrcJson,
        )],
        cancellation: false,
        result: TypertCodec::SrcJson,
        source_location: None,
    }
}

fn strict_only_descriptor() -> InvocationDescriptor {
    let codec = strict(
        "@fixture/gateway#StrictValue",
        Arc::new(RequestSchema { trim: false }),
    );
    InvocationDescriptor {
        id: "@fixture/gateway#goals/strictOnly".to_owned(),
        service: "goals".to_owned(),
        namespace: "goals".to_owned(),
        method: "strictOnly".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: None,
        parameters: vec![parameter(
            "request",
            "request",
            InvocationParameterSource::Json,
            None,
            codec.clone(),
        )],
        cancellation: false,
        result: codec,
        source_location: None,
    }
}

fn maybe_descriptor() -> InvocationDescriptor {
    let codec = strict("@fixture/gateway#MaybeValue", Arc::new(MaybeSchema));
    let mut value = parameter(
        "value",
        "value",
        InvocationParameterSource::Json,
        None,
        codec.clone(),
    );
    value.accepts_undefined = Some(true);
    InvocationDescriptor {
        id: "@fixture/gateway#goals/maybe".to_owned(),
        service: "goals".to_owned(),
        namespace: "goals".to_owned(),
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

struct Fixture {
    context: Context,
    registry: Arc<TypertRegistry>,
    services: Arc<TypertServiceDirectory>,
    gateway: Arc<TypertGatewayService>,
    state: Arc<GoalState>,
    service_effect: EffectHandle,
}

fn setup_gateway() -> (
    Context,
    Arc<TypertRegistry>,
    Arc<TypertServiceDirectory>,
    Arc<TypertGatewayService>,
) {
    let context = Context::new();
    let registry = TypertRegistry::new();
    registry.provide(&context).unwrap();
    let (services, gateway) = install(&context).unwrap();
    (context, registry, services, gateway)
}

fn setup() -> Fixture {
    let (context, registry, services, gateway) = setup_gateway();
    let state = Arc::new(GoalState::default());
    let service_effect = context.provide(GOALS, state.clone()).unwrap();
    services
        .register(
            &context,
            "goals",
            Arc::new(|receiver_context| {
                receiver_context.get(GOALS).map(|state| {
                    Arc::new(GoalService {
                        context: receiver_context.clone(),
                        state,
                    }) as Arc<dyn TypertInvocableService>
                })
            }),
        )
        .unwrap();
    Fixture {
        context,
        registry,
        services,
        gateway,
        state,
        service_effect,
    }
}

fn register_simple(
    context: &Context,
    services: &Arc<TypertServiceDirectory>,
    registered_key: &str,
    service: Arc<SimpleService>,
) -> EffectHandle {
    services
        .register(
            context,
            registered_key,
            Arc::new(move |_context| Some(service.clone() as Arc<dyn TypertInvocableService>)),
        )
        .unwrap()
}

fn register_contribution(
    context: &Context,
    registry: &TypertRegistry,
    package: &str,
    descriptors: Vec<InvocationDescriptor>,
) -> EffectHandle {
    registry
        .register(
            context,
            &TypertContribution {
                package: package.to_owned(),
                face: TypertFace::Host,
                schemas: Vec::new(),
                model: TypertPackageModel::default(),
                invocations: descriptors,
            },
        )
        .unwrap()
}

fn register_strict(fixture: &Fixture, descriptors: Vec<InvocationDescriptor>) -> EffectHandle {
    register_contribution(
        &fixture.context,
        &fixture.registry,
        "@fixture/gateway",
        descriptors,
    )
}

fn agent_provider(agent: Arc<FixtureAgent>) -> TypertLookupProvider {
    TypertLookupProvider {
        parameter: "agent".to_owned(),
        wire: "agentId".to_owned(),
        host_type_symbol: "@fixture/domain#Agent".to_owned(),
        wire_type_symbol: "@fixture/domain#AgentId".to_owned(),
        resolve: Arc::new(move |identity| {
            let agent = agent.clone();
            Box::pin(async move {
                Ok(
                    (identity.as_json().and_then(Value::as_str) == Some(&agent.id))
                        .then_some(agent as Arc<dyn std::any::Any + Send + Sync>),
                )
            })
        }),
    }
}

fn register_agent(fixture: &Fixture, id: &str) -> EffectHandle {
    fixture
        .registry
        .lookups()
        .register(
            &fixture.context,
            "gatewayFixture",
            agent_provider(Arc::new(FixtureAgent { id: id.to_owned() })),
        )
        .unwrap()
}

fn context_provider(context: Context) -> TypertHostContextProvider {
    TypertHostContextProvider {
        wire: "agentId".to_owned(),
        wire_type_symbol: "@fixture/domain#AgentId".to_owned(),
        resolve: Arc::new(move |identity| {
            let context = context.clone();
            Box::pin(async move {
                Ok(
                    (identity.as_json().and_then(Value::as_str) == Some("agent-1"))
                        .then_some(context),
                )
            })
        }),
    }
}

fn request(namespace: &str, method: &str, args: Value) -> InvokeRemoteRequest {
    let Value::Object(args) = args else {
        panic!("fixture request args must be an object");
    };
    InvokeRemoteRequest {
        namespace: namespace.to_owned(),
        method: method.to_owned(),
        args: args
            .into_iter()
            .map(|(key, value)| (key, TypertBoundaryValue::Json(value)))
            .collect::<IndexMap<_, _>>(),
        signal: None,
    }
}

fn expect_code(
    result: anyhow::Result<TypertBoundaryValue>,
    expected: TypertGatewayErrorCode,
) -> TypertGatewayErrorCode {
    let error = result.expect_err("expected TypertGatewayError");
    let gateway = error
        .downcast_ref::<TypertGatewayError>()
        .expect("expected typed Gateway error");
    assert_eq!(gateway.code, expected, "{error:#}");
    gateway.code
}

#[tokio::test]
async fn invokes_strict_direct_with_schema_lookup_signal_and_caller_context() {
    let fixture = setup();
    register_agent(&fixture, "agent-1");
    register_strict(&fixture, vec![create_descriptor()]);
    let caller = fixture
        .context
        .with_meta("fixtureScope", json!("direct-caller"));
    let signal = AbortSignal::default();
    let mut invocation = request(
        "goals",
        "create",
        json!({"agentId": "agent-1", "request": {"title": "  ship  "}}),
    );
    invocation.signal = Some(signal.clone());

    let result = fixture
        .gateway
        .for_context(&caller)
        .invoke(invocation)
        .await
        .unwrap();

    assert_eq!(
        result,
        TypertBoundaryValue::json(json!({
            "agentId": "agent-1",
            "title": "ship",
            "scope": "direct-caller",
        }))
    );
    assert_eq!(*fixture.state.calls.lock(), ["create"]);
    assert_eq!(*fixture.state.last_signal.lock(), Some(signal));
}

#[tokio::test]
async fn resolves_strict_and_src_scoped_receivers_without_business_identity_argument() {
    for strict_mode in [true, false] {
        let fixture = setup();
        let scoped = fixture
            .context
            .with_meta("fixtureScope", json!("agent-scope"));
        fixture
            .registry
            .contexts()
            .register_host(&fixture.context, "gatewayFixture", context_provider(scoped))
            .unwrap();
        if strict_mode {
            register_strict(&fixture, vec![rename_descriptor()]);
        }

        let result = fixture
            .gateway
            .invoke(request(
                "goals",
                "rename",
                json!({"agentId": "agent-1", "request": {"title": "land"}}),
            ))
            .await
            .unwrap();
        assert_eq!(
            result,
            TypertBoundaryValue::json(json!({"title": "land", "scope": "agent-scope"}))
        );
    }
}

#[tokio::test]
async fn derives_src_lookup_json_and_cancellation_parameters() {
    let fixture = setup();
    register_agent(&fixture, "agent-1");
    let caller = fixture
        .context
        .with_meta("fixtureScope", json!("direct-src"));
    let signal = AbortSignal::default();
    let mut invocation = request(
        "goals",
        "create",
        json!({"agentId": "agent-1", "request": {"title": "ship"}}),
    );
    invocation.signal = Some(signal.clone());

    let result = fixture
        .gateway
        .for_context(&caller)
        .invoke(invocation)
        .await
        .unwrap();

    assert_eq!(
        result,
        TypertBoundaryValue::json(json!({
            "agentId": "agent-1",
            "title": "ship",
            "scope": "direct-src",
        }))
    );
    assert_eq!(*fixture.state.last_signal.lock(), Some(signal));
}

#[tokio::test]
async fn src_lookup_declaration_survives_provider_unload_without_downgrade() {
    let fixture = setup();
    let lookup = register_agent(&fixture, "agent-1");
    lookup.dispose().await.unwrap();

    expect_code(
        fixture
            .gateway
            .invoke(request(
                "goals",
                "create",
                json!({"agentId": "agent-1", "request": {"title": "ship"}}),
            ))
            .await,
        TypertGatewayErrorCode::LookupUnavailable,
    );
    assert!(fixture.state.calls.lock().is_empty());
}

#[tokio::test]
async fn re_reads_lookup_service_and_context_providers_on_every_invocation() {
    let fixture = setup();
    let lookup = register_agent(&fixture, "agent-1");
    register_strict(&fixture, vec![create_descriptor()]);
    lookup.dispose().await.unwrap();
    expect_code(
        fixture
            .gateway
            .invoke(request(
                "goals",
                "create",
                json!({"agentId": "agent-1", "request": {"title": "ship"}}),
            ))
            .await,
        TypertGatewayErrorCode::LookupUnavailable,
    );

    register_agent(&fixture, "agent-1");
    fixture.service_effect.dispose().await.unwrap();
    expect_code(
        fixture
            .gateway
            .invoke(request(
                "goals",
                "create",
                json!({"agentId": "agent-1", "request": {"title": "ship"}}),
            ))
            .await,
        TypertGatewayErrorCode::ServiceUnavailable,
    );

    let scoped = fixture.context.with_meta("fixtureScope", json!("scoped"));
    let context_effect = fixture
        .registry
        .contexts()
        .register_host(&fixture.context, "gatewayFixture", context_provider(scoped))
        .unwrap();
    context_effect.dispose().await.unwrap();
    let second = setup();
    register_strict(&second, vec![rename_descriptor()]);
    expect_code(
        second
            .gateway
            .invoke(request(
                "goals",
                "rename",
                json!({"agentId": "agent-1", "request": {"title": "land"}}),
            ))
            .await,
        TypertGatewayErrorCode::ContextUnavailable,
    );
}

#[tokio::test]
async fn strict_definition_history_forbids_src_downgrade_after_disposal_and_reload() {
    let fixture = setup();
    let strict = register_strict(&fixture, vec![passthrough_descriptor()]);
    strict.dispose().await.unwrap();

    expect_code(
        fixture
            .gateway
            .invoke(request(
                "goals",
                "passthrough",
                json!({"value": "would pass through SRC"}),
            ))
            .await,
        TypertGatewayErrorCode::DefinitionUnavailable,
    );

    let reloaded = TypertGatewayService::new(
        &fixture.context,
        fixture.registry.clone(),
        fixture.services.clone(),
    );
    expect_code(
        reloaded
            .invoke(request(
                "goals",
                "passthrough",
                json!({"value": "still forbidden"}),
            ))
            .await,
        TypertGatewayErrorCode::DefinitionUnavailable,
    );
}

#[tokio::test]
async fn exact_arguments_precede_business_invocation_and_src_omission_is_undefined() {
    let fixture = setup();
    register_agent(&fixture, "agent-1");

    for args in [
        json!({"request": {"title": "ship"}}),
        json!({
            "agentId": "agent-1",
            "request": {"title": "ship"},
            "optional": true,
        }),
    ] {
        expect_code(
            fixture
                .gateway
                .invoke(request("goals", "create", args))
                .await,
            TypertGatewayErrorCode::ArgumentsInvalid,
        );
    }
    assert!(fixture.state.calls.lock().is_empty());

    let result = fixture
        .gateway
        .invoke(request("goals", "passthrough", json!({})))
        .await
        .unwrap();
    assert_eq!(result, TypertBoundaryValue::Undefined);
    assert_eq!(*fixture.state.calls.lock(), ["passthrough"]);
}

#[tokio::test]
async fn strict_input_and_result_failures_are_distinct() {
    let fixture = setup();
    register_strict(&fixture, vec![strict_only_descriptor()]);

    expect_code(
        fixture
            .gateway
            .invoke(request(
                "goals",
                "strictOnly",
                json!({"request": {"title": 1}}),
            ))
            .await,
        TypertGatewayErrorCode::InputInvalid,
    );

    *fixture.state.next_result.lock() = Some(TypertBoundaryValue::json(json!({"title": 1})));
    expect_code(
        fixture
            .gateway
            .invoke(request(
                "goals",
                "strictOnly",
                json!({"request": {"title": "ship"}}),
            ))
            .await,
        TypertGatewayErrorCode::ResultInvalid,
    );
}

#[tokio::test]
async fn lookup_failures_missing_identities_and_metadata_mismatch_are_contained() {
    let throwing = setup();
    register_strict(&throwing, vec![create_descriptor()]);
    let mut provider = agent_provider(Arc::new(FixtureAgent {
        id: "agent-1".to_owned(),
    }));
    provider.resolve =
        Arc::new(|_identity| Box::pin(async { Err(anyhow::anyhow!("lookup failed")) }));
    throwing
        .registry
        .lookups()
        .register(&throwing.context, "gatewayFixture", provider)
        .unwrap();
    let failure = throwing
        .gateway
        .invoke(request(
            "goals",
            "create",
            json!({"agentId": "agent-1", "request": {"title": "ship"}}),
        ))
        .await
        .expect_err("throwing lookup unexpectedly resolved");
    let gateway = failure.downcast_ref::<TypertGatewayError>().unwrap();
    assert_eq!(gateway.code, TypertGatewayErrorCode::LookupFailed);
    assert_eq!(gateway.source().unwrap().to_string(), "lookup failed");

    let missing = setup();
    register_strict(&missing, vec![create_descriptor()]);
    let mut provider = agent_provider(Arc::new(FixtureAgent {
        id: "agent-1".to_owned(),
    }));
    provider.resolve = Arc::new(|_identity| Box::pin(async { Ok(None) }));
    missing
        .registry
        .lookups()
        .register(&missing.context, "gatewayFixture", provider)
        .unwrap();
    expect_code(
        missing
            .gateway
            .invoke(request(
                "goals",
                "create",
                json!({"agentId": "agent-1", "request": {"title": "ship"}}),
            ))
            .await,
        TypertGatewayErrorCode::LookupNotFound,
    );

    let mismatch = setup();
    register_strict(&mismatch, vec![create_descriptor()]);
    let mut provider = agent_provider(Arc::new(FixtureAgent {
        id: "agent-1".to_owned(),
    }));
    provider.wire = "differentAgentId".to_owned();
    mismatch
        .registry
        .lookups()
        .register(&mismatch.context, "gatewayFixture", provider)
        .unwrap();
    expect_code(
        mismatch
            .gateway
            .invoke(request(
                "goals",
                "create",
                json!({"agentId": "agent-1", "request": {"title": "ship"}}),
            ))
            .await,
        TypertGatewayErrorCode::ProviderMismatch,
    );
}

#[tokio::test]
async fn context_failures_missing_identities_and_metadata_mismatch_are_contained() {
    let throwing = setup();
    register_strict(&throwing, vec![rename_descriptor()]);
    let mut provider = context_provider(throwing.context.clone());
    provider.resolve =
        Arc::new(|_identity| Box::pin(async { Err(anyhow::anyhow!("provider failed")) }));
    throwing
        .registry
        .contexts()
        .register_host(&throwing.context, "gatewayFixture", provider)
        .unwrap();
    let failure = throwing
        .gateway
        .invoke(request(
            "goals",
            "rename",
            json!({"agentId": "agent-1", "request": {"title": "land"}}),
        ))
        .await
        .expect_err("throwing Context provider unexpectedly resolved");
    let gateway = failure.downcast_ref::<TypertGatewayError>().unwrap();
    assert_eq!(gateway.code, TypertGatewayErrorCode::ContextFailed);
    assert_eq!(gateway.source().unwrap().to_string(), "provider failed");

    let missing = setup();
    register_strict(&missing, vec![rename_descriptor()]);
    let mut provider = context_provider(missing.context.clone());
    provider.resolve = Arc::new(|_identity| Box::pin(async { Ok(None) }));
    missing
        .registry
        .contexts()
        .register_host(&missing.context, "gatewayFixture", provider)
        .unwrap();
    expect_code(
        missing
            .gateway
            .invoke(request(
                "goals",
                "rename",
                json!({"agentId": "agent-1", "request": {"title": "land"}}),
            ))
            .await,
        TypertGatewayErrorCode::ContextNotFound,
    );

    let mismatch = setup();
    register_strict(&mismatch, vec![rename_descriptor()]);
    let mut provider = context_provider(mismatch.context.clone());
    provider.wire = "differentAgentId".to_owned();
    mismatch
        .registry
        .contexts()
        .register_host(&mismatch.context, "gatewayFixture", provider)
        .unwrap();
    expect_code(
        mismatch
            .gateway
            .invoke(request(
                "goals",
                "rename",
                json!({"agentId": "agent-1", "request": {"title": "land"}}),
            ))
            .await,
        TypertGatewayErrorCode::ProviderMismatch,
    );
}

#[tokio::test]
async fn lookup_policy_rejections_preserve_identity_and_rpc_failure_payload() {
    let fixture = setup();
    register_strict(&fixture, vec![create_descriptor()]);
    let policy = json!({
        "code": "agent-busy",
        "message": "session is owned by subagent routing",
        "details": {"reason": "use subagent delivery for this child session"},
    });
    let direct_policy = policy.clone();
    let mut provider = agent_provider(Arc::new(FixtureAgent {
        id: "agent-1".to_owned(),
    }));
    provider.resolve = Arc::new(move |_identity| {
        let failure = direct_policy.clone();
        Box::pin(async move { Err(TypertLookupFailure::new(failure).into()) })
    });
    fixture
        .registry
        .lookups()
        .register(&fixture.context, "gatewayFixture", provider)
        .unwrap();

    let direct = fixture
        .gateway
        .invoke(request(
            "goals",
            "create",
            json!({"agentId": "agent-1", "request": {"title": "ship"}}),
        ))
        .await
        .expect_err("policy rejection unexpectedly resolved");
    assert_eq!(
        direct
            .downcast_ref::<TypertLookupFailure>()
            .expect("policy error identity was wrapped")
            .failure,
        policy
    );

    assert_eq!(
        fixture
            .gateway
            .invoke_rpc(
                "goals/create",
                json!({"args": {
                    "agentId": "agent-1",
                    "request": {"title": "ship"},
                }}),
                AbortSignal::default(),
            )
            .await,
        GatewayRpcResult::Failure {
            error: serde_json::from_value(policy).unwrap(),
        }
    );
}

#[tokio::test]
async fn business_error_identity_is_preserved_and_aborted_rejection_is_cancelled() {
    let fixture = setup();
    let business = Arc::new(FixtureBusinessError("business identity"));
    *fixture.state.business_error.lock() = Some(business.clone());
    let direct = fixture
        .gateway
        .invoke(request(
            "goals",
            "fail",
            json!({"request": {"reason": "fixture"}}),
        ))
        .await
        .expect_err("business failure unexpectedly resolved");
    let recovered = direct
        .downcast_ref::<Arc<FixtureBusinessError>>()
        .expect("business error was wrapped or replaced");
    assert!(Arc::ptr_eq(recovered, &business));

    let signal = AbortSignal::default();
    signal.abort_with_reason(json!("client disconnected"));
    assert_eq!(
        fixture
            .gateway
            .invoke_rpc("goals/fail", json!({"args": {"request": null}}), signal,)
            .await,
        GatewayRpcResult::Failure {
            error: seekdeep_typert_protocol::RemoteFailure {
                code: "cancelled".to_owned(),
                message: "Remote invocation \"goals/fail\" was aborted".to_owned(),
                details: serde_json::Map::new(),
            },
        }
    );
}

#[tokio::test]
async fn src_derives_exported_empty_and_distinct_namespace_methods() {
    let (context, _registry, services, gateway) = setup_gateway();
    let mut exported = SimpleService::direct("exportedMethod", "exported", "run", &["value"]);
    exported.markers = vec![marker(
        "run",
        Some("execute"),
        RemoteInvocationMarker::Direct,
    )];
    register_simple(&context, &services, "exportedMethod", Arc::new(exported));

    let mut empty = SimpleService::direct("emptyMethod", "empty", "ping", &[]);
    empty.result = SimpleResult::Pong;
    register_simple(&context, &services, "emptyMethod", Arc::new(empty));
    register_simple(
        &context,
        &services,
        "inheritedMethod",
        Arc::new(SimpleService::direct(
            "inheritedMethod",
            "inherited",
            "run",
            &["value"],
        )),
    );

    assert_eq!(
        gateway
            .invoke(request("exported", "execute", json!({"value": "ship"}),))
            .await
            .unwrap(),
        TypertBoundaryValue::json(json!("ship"))
    );
    assert_eq!(
        gateway
            .invoke(request("empty", "ping", json!({})))
            .await
            .unwrap(),
        TypertBoundaryValue::json(json!("pong"))
    );
    assert_eq!(
        gateway
            .invoke(request("inherited", "run", json!({"value": "land"})))
            .await
            .unwrap(),
        TypertBoundaryValue::json(json!("land"))
    );
}

#[tokio::test]
async fn ambiguous_src_endpoints_are_sorted_independently_of_registration_order() {
    let (context, _registry, services, gateway) = setup_gateway();
    register_simple(
        &context,
        &services,
        "secondShared",
        Arc::new(SimpleService::direct(
            "secondShared",
            "shared",
            "run",
            &["value"],
        )),
    );
    register_simple(
        &context,
        &services,
        "firstShared",
        Arc::new(SimpleService::direct(
            "firstShared",
            "shared",
            "run",
            &["value"],
        )),
    );

    let error = gateway
        .invoke(request("shared", "run", json!({"value": "ship"})))
        .await
        .expect_err("ambiguous endpoint unexpectedly resolved");
    let gateway = error.downcast_ref::<TypertGatewayError>().unwrap();
    assert_eq!(gateway.code, TypertGatewayErrorCode::AmbiguousEndpoint);
    assert!(gateway.to_string().contains("firstShared, secondShared"));
}

#[tokio::test]
async fn src_rejects_unrepresentable_signatures_and_wire_collisions() {
    let invalid = [
        ("invalid-default", vec!["value = fallback"]),
        ("invalid-destructure", vec!["{ value }"]),
        ("invalid-rest", vec!["...values"]),
        ("invalid-signal", vec!["signal", "value"]),
    ];
    for (namespace, names) in invalid {
        let (context, _registry, services, gateway) = setup_gateway();
        register_simple(
            &context,
            &services,
            namespace,
            Arc::new(SimpleService::direct(namespace, namespace, "run", &names)),
        );
        expect_code(
            gateway
                .invoke(request(namespace, "run", json!({"value": "x"})))
                .await,
            TypertGatewayErrorCode::SignatureInvalid,
        );
    }

    let (context, registry, services, gateway) = setup_gateway();
    registry
        .lookups()
        .register(
            &context,
            "gatewayFixture",
            agent_provider(Arc::new(FixtureAgent {
                id: "agent-1".to_owned(),
            })),
        )
        .unwrap();
    register_simple(
        &context,
        &services,
        "collidingWire",
        Arc::new(SimpleService::direct(
            "collidingWire",
            "colliding-wire",
            "run",
            &["agent", "agentId"],
        )),
    );
    expect_code(
        gateway
            .invoke(request(
                "colliding-wire",
                "run",
                json!({"agentId": "agent-1"}),
            ))
            .await,
        TypertGatewayErrorCode::SignatureInvalid,
    );

    let (context, registry, services, gateway) = setup_gateway();
    registry
        .contexts()
        .register_host(
            &context,
            "gatewayFixture",
            context_provider(context.clone()),
        )
        .unwrap();
    let mut context_wire =
        SimpleService::direct("contextWire", "context-wire", "run", &["agentId"]);
    context_wire.markers = vec![marker(
        "run",
        None,
        RemoteInvocationMarker::Context {
            context: "gatewayFixture".to_owned(),
        },
    )];
    register_simple(&context, &services, "contextWire", Arc::new(context_wire));
    expect_code(
        gateway
            .invoke(request(
                "context-wire",
                "run",
                json!({"agentId": "agent-1"}),
            ))
            .await,
        TypertGatewayErrorCode::SignatureInvalid,
    );
}

#[tokio::test]
async fn src_rejects_multiple_lookup_matches_and_missing_context_provider() {
    let (context, registry, services, gateway) = setup_gateway();
    let provider = agent_provider(Arc::new(FixtureAgent {
        id: "agent-1".to_owned(),
    }));
    registry
        .lookups()
        .register(&context, "gatewayFixture", provider.clone())
        .unwrap();
    registry
        .lookups()
        .register(&context, "gatewayFixtureAlias", provider)
        .unwrap();
    register_simple(
        &context,
        &services,
        "lookupAmbiguous",
        Arc::new(SimpleService::direct(
            "lookupAmbiguous",
            "lookup-ambiguous",
            "run",
            &["agent"],
        )),
    );
    expect_code(
        gateway
            .invoke(request(
                "lookup-ambiguous",
                "run",
                json!({"agentId": "agent-1"}),
            ))
            .await,
        TypertGatewayErrorCode::SignatureInvalid,
    );

    let (context, _registry, services, gateway) = setup_gateway();
    let mut scoped = SimpleService::direct("scoped", "scoped", "run", &["value"]);
    scoped.markers = vec![marker(
        "run",
        None,
        RemoteInvocationMarker::Context {
            context: "gatewayFixture".to_owned(),
        },
    )];
    register_simple(&context, &services, "scoped", Arc::new(scoped));
    expect_code(
        gateway
            .invoke(request(
                "scoped",
                "run",
                json!({"agentId": "agent-1", "value": "x"}),
            ))
            .await,
        TypertGatewayErrorCode::ContextUnavailable,
    );
}

#[tokio::test]
async fn binding_visibility_identity_and_method_availability_are_validated() {
    let (context, _registry, services, gateway) = setup_gateway();
    register_simple(
        &context,
        &services,
        "wrongBinding",
        Arc::new(SimpleService::direct(
            "notWrongBinding",
            "wrong-binding",
            "run",
            &["value"],
        )),
    );
    expect_code(
        gateway
            .invoke(request("wrong-binding", "run", json!({"value": "ship"})))
            .await,
        TypertGatewayErrorCode::BindingInvalid,
    );

    let fixture = setup();
    let mut missing = passthrough_descriptor();
    missing.id = "@fixture/gateway#goals/missing".to_owned();
    missing.method = "missing".to_owned();
    register_strict(&fixture, vec![missing]);
    expect_code(
        fixture
            .gateway
            .invoke(request("goals", "missing", json!({"value": "ship"})))
            .await,
        TypertGatewayErrorCode::MethodUnavailable,
    );

    let (context, registry, services, gateway) = setup_gateway();
    let mut no_binding = SimpleService::direct("noBinding", "no-binding", "run", &["value"]);
    no_binding.visible_binding = false;
    register_simple(&context, &services, "noBinding", Arc::new(no_binding));
    let mut descriptor = passthrough_descriptor();
    descriptor.id = "@fixture/no-binding#no-binding/run".to_owned();
    descriptor.service = "noBinding".to_owned();
    descriptor.namespace = "no-binding".to_owned();
    descriptor.method = "run".to_owned();
    register_contribution(&context, &registry, "@fixture/no-binding", vec![descriptor]);
    expect_code(
        gateway
            .invoke(request("no-binding", "run", json!({"value": "ship"})))
            .await,
        TypertGatewayErrorCode::BindingInvalid,
    );

    let (context, _registry, services, gateway) = setup_gateway();
    let mut missing_source =
        SimpleService::direct("missingSource", "missing-source", "run", &["value"]);
    missing_source.parameters.clear();
    register_simple(
        &context,
        &services,
        "missingSource",
        Arc::new(missing_source),
    );
    expect_code(
        gateway
            .invoke(request("missing-source", "run", json!({"value": "ship"})))
            .await,
        TypertGatewayErrorCode::MethodUnavailable,
    );
}

#[tokio::test]
async fn rpc_payload_validation_and_undefined_null_results_match_carrier_shape() {
    let fixture = setup();
    register_strict(&fixture, vec![maybe_descriptor()]);
    let signal = AbortSignal::default();

    for payload in [
        Value::Null,
        json!([]),
        json!({"args": {}, "extra": true}),
        json!({"only": true}),
        json!({"args": null}),
        json!({"args": []}),
    ] {
        let result = fixture
            .gateway
            .invoke_rpc("goals/maybe", payload, signal.clone())
            .await;
        let GatewayRpcResult::Failure { error } = result else {
            panic!("invalid carrier payload unexpectedly succeeded");
        };
        assert_eq!(error.code, "internal");
        assert!(error.message.contains("plain-object args field"));
    }

    assert_eq!(
        fixture
            .gateway
            .invoke_rpc("goals/maybe", json!({"args": {}}), signal.clone())
            .await,
        GatewayRpcResult::Success { value: None }
    );
    assert_eq!(
        fixture
            .gateway
            .invoke_rpc("goals/maybe", json!({"args": {"value": null}}), signal,)
            .await,
        GatewayRpcResult::Success {
            value: Some(Value::Null)
        }
    );
}

#[tokio::test]
async fn malformed_endpoints_and_absent_invocations_are_contained() {
    let fixture = setup();
    let signal = AbortSignal::default();
    for endpoint in ["goals", "/create", "goals/", "goals/create/extra"] {
        let result = fixture
            .gateway
            .invoke_rpc(endpoint, json!({"args": {}}), signal.clone())
            .await;
        let GatewayRpcResult::Failure { error } = result else {
            panic!("invalid endpoint unexpectedly succeeded");
        };
        assert_eq!(error.code, "internal");
        assert!(error.message.contains("invalid Remote endpoint"));
    }

    expect_code(
        fixture
            .gateway
            .invoke(request("goals", "absent", json!({})))
            .await,
        TypertGatewayErrorCode::InvocationUnavailable,
    );
}

#[tokio::test]
async fn endpoint_claims_include_live_src_and_strict_history_only() {
    let fixture = setup();
    assert!(fixture.gateway.claims_endpoint("goals/create"));
    assert!(!fixture.gateway.claims_endpoint("legacy/list"));
    assert!(!fixture.gateway.claims_endpoint("goals"));
    assert!(!fixture.gateway.claims_endpoint("goals/create/extra"));

    let descriptor = passthrough_descriptor();
    let effect = register_strict(&fixture, vec![descriptor]);
    assert!(fixture.gateway.claims_endpoint("goals/passthrough"));
    effect.dispose().await.unwrap();
    assert!(fixture.gateway.claims_endpoint("goals/passthrough"));
}

#[test]
fn src_claims_cache_until_the_cordis_service_set_changes() {
    const UNRELATED: ServiceKey<usize> = ServiceKey::new("unrelated");
    let fixture = setup();

    assert!(!fixture.gateway.claims_endpoint("legacy/list"));
    assert!(!fixture.gateway.claims_endpoint("legacy/list"));
    assert!(fixture.gateway.claims_endpoint("goals/create"));
    assert!(fixture.gateway.claims_endpoint("goals/create"));
    assert_eq!(fixture.state.marker_reads.load(Ordering::Acquire), 1);

    let _unrelated = fixture
        .context
        .provide(UNRELATED, Arc::new(1_usize))
        .unwrap();
    assert!(!fixture.gateway.claims_endpoint("legacy/list"));
    assert_eq!(fixture.state.marker_reads.load(Ordering::Acquire), 2);
}

#[test]
fn all_gateway_failure_codes_keep_exact_wire_spellings() {
    let spellings = [
        (
            TypertGatewayErrorCode::AmbiguousEndpoint,
            "\"ambiguous-endpoint\"",
        ),
        (
            TypertGatewayErrorCode::ArgumentsInvalid,
            "\"arguments-invalid\"",
        ),
        (
            TypertGatewayErrorCode::BindingInvalid,
            "\"binding-invalid\"",
        ),
        (TypertGatewayErrorCode::ContextFailed, "\"context-failed\""),
        (
            TypertGatewayErrorCode::ContextNotFound,
            "\"context-not-found\"",
        ),
        (
            TypertGatewayErrorCode::ContextUnavailable,
            "\"context-unavailable\"",
        ),
        (
            TypertGatewayErrorCode::DefinitionUnavailable,
            "\"definition-unavailable\"",
        ),
        (TypertGatewayErrorCode::InputInvalid, "\"input-invalid\""),
        (
            TypertGatewayErrorCode::InvocationUnavailable,
            "\"invocation-unavailable\"",
        ),
        (TypertGatewayErrorCode::LookupFailed, "\"lookup-failed\""),
        (
            TypertGatewayErrorCode::LookupNotFound,
            "\"lookup-not-found\"",
        ),
        (
            TypertGatewayErrorCode::LookupUnavailable,
            "\"lookup-unavailable\"",
        ),
        (
            TypertGatewayErrorCode::MethodUnavailable,
            "\"method-unavailable\"",
        ),
        (
            TypertGatewayErrorCode::ProviderMismatch,
            "\"provider-mismatch\"",
        ),
        (TypertGatewayErrorCode::ResultInvalid, "\"result-invalid\""),
        (
            TypertGatewayErrorCode::ServiceUnavailable,
            "\"service-unavailable\"",
        ),
        (
            TypertGatewayErrorCode::SignatureInvalid,
            "\"signature-invalid\"",
        ),
    ];
    for (code, spelling) in spellings {
        assert_eq!(serde_json::to_string(&code).unwrap(), spelling);
    }
}

#[test]
fn lookup_policy_failure_type_remains_distinct() {
    let failure = TypertLookupFailure::new(json!({
        "code": "agent-busy",
        "message": "owned",
        "details": {"reason": "subagent"},
    }));
    assert_eq!(
        failure.failure,
        json!({
            "code": "agent-busy",
            "message": "owned",
            "details": {"reason": "subagent"},
        })
    );
}

#[tokio::test]
async fn installed_gateway_claims_live_src_endpoints_through_real_connection_registry() {
    let context = Context::new();
    let registry = TypertRegistry::new();
    registry.provide(&context).unwrap();
    let connection = HostConnectionService::new(Vec::new()).unwrap();
    connection.provide(&context).unwrap();
    let (services, _gateway) = install(&context).unwrap();
    register_simple(
        &context,
        &services,
        "echoService",
        Arc::new(SimpleService::direct(
            "echoService",
            "echo",
            "read",
            &["value"],
        )),
    );

    let message = ClientRequest::new(
        RpcId::new("gateway-connection"),
        "echo/read",
        json!({ "args": { "value": { "nested": true } } }),
    );
    let mut request = HttpRequest::new(HttpMethod::Post, "/api/echo/read");
    request
        .headers
        .insert("host".to_owned(), "127.0.0.1:3080".to_owned());
    request
        .headers
        .insert("content-type".to_owned(), "application/json".to_owned());
    request.body = serde_json::to_vec(&message).unwrap();
    let response = connection
        .dispatch_shared("/api", request, |_| async {
            HttpResponse::text(404, "fallback")
        })
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body).unwrap(),
        json!({
            "type": "server-response",
            "rpcId": "gateway-connection",
            "result": { "ok": true, "value": { "nested": true } },
        })
    );

    let unclaimed = connection
        .dispatch_shared(
            "/api",
            HttpRequest::new(HttpMethod::Get, "/api/other/read"),
            |_| async { HttpResponse::text(418, "fallback") },
        )
        .await;
    assert_eq!(unclaimed.status, 418);
}

#[tokio::test]
async fn gateway_claim_tracks_late_connection_withdrawal_and_same_instance_reprovision() {
    let (context, _registry, services, _gateway) = setup_gateway();
    register_simple(
        &context,
        &services,
        "echoService",
        Arc::new(SimpleService::direct(
            "echoService",
            "echo",
            "read",
            &["value"],
        )),
    );
    let connection = HostConnectionService::new(Vec::new()).unwrap();
    let provision = connection.provide(&context).unwrap();

    let make_request = || {
        let message = ClientRequest::new(
            RpcId::new("late-connection"),
            "echo/read",
            json!({ "args": { "value": "live" } }),
        );
        let mut request = HttpRequest::new(HttpMethod::Post, "/api/echo/read");
        request
            .headers
            .insert("host".to_owned(), "127.0.0.1:3080".to_owned());
        request
            .headers
            .insert("content-type".to_owned(), "application/json".to_owned());
        request.body = serde_json::to_vec(&message).unwrap();
        request
    };
    assert_eq!(
        connection
            .dispatch_shared("/api", make_request(), |_| async {
                HttpResponse::text(418, "fallback")
            })
            .await
            .status,
        200
    );

    provision.dispose().await.unwrap();
    assert_eq!(
        connection
            .dispatch_shared("/api", make_request(), |_| async {
                HttpResponse::text(418, "fallback")
            })
            .await
            .status,
        418
    );

    connection.provide(&context).unwrap();
    assert_eq!(
        connection
            .dispatch_shared("/api", make_request(), |_| async {
                HttpResponse::text(418, "fallback")
            })
            .await
            .status,
        200
    );
}
