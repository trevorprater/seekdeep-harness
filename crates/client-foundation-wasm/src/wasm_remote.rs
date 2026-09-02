//! Browser Client Remote registry, scoped invocation, and event delivery.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};
use web_sys::{AbortController, AbortSignal};

#[derive(Clone)]
struct MountToken {
    active: Rc<Cell<bool>>,
    abort: AbortController,
}

impl MountToken {
    fn new() -> Result<Self, JsValue> {
        Ok(Self {
            active: Rc::new(Cell::new(true)),
            abort: AbortController::new()?,
        })
    }

    fn withdraw(&self) -> bool {
        if self.active.replace(false) {
            self.abort.abort();
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
struct RemoteParameter {
    wire: String,
    source: String,
    lookup: Option<String>,
    codec: JsValue,
}

#[derive(Clone)]
struct ScopedProjection {
    context: String,
    wire: String,
    codec: JsValue,
    parameter_index: Option<usize>,
}

#[derive(Clone)]
struct RemoteDescriptor {
    namespace: String,
    method: String,
    direct: bool,
    scoped: Option<ScopedProjection>,
    parameters: Vec<RemoteParameter>,
    cancellation: bool,
    result: JsValue,
}

impl RemoteDescriptor {
    fn endpoint(&self) -> String {
        format!("{}/{}", self.namespace, self.method)
    }
}

#[derive(Clone)]
struct InstalledMethod {
    descriptor: RemoteDescriptor,
    token: MountToken,
}

#[derive(Clone)]
struct ScopedMethod {
    method: InstalledMethod,
    projection: ScopedProjection,
}

#[derive(Clone, Default)]
struct MethodRecord {
    direct: Option<InstalledMethod>,
    scoped: Option<ScopedMethod>,
}

#[derive(Clone, Copy)]
enum MethodKind {
    Direct,
    Scoped,
}

#[derive(Clone)]
struct InstalledVariant {
    endpoint: String,
    kind: MethodKind,
    token: MountToken,
}

struct NamespaceHandle {
    service: JsValue,
    dispose: Function,
}

struct EventSubscription {
    id: u64,
    event: String,
    listener: Function,
}

struct BrowserRemoteState {
    owner_context: JsValue,
    rpc: JsValue,
    typert: JsValue,
    namespace_factory: Function,
    methods: RefCell<HashMap<String, MethodRecord>>,
    namespaces: RefCell<HashMap<String, NamespaceHandle>>,
    subscriptions: RefCell<Vec<EventSubscription>>,
    next_subscription: Cell<u64>,
}

/// Rust-owned browser projection of the generated Client Remote service.
#[wasm_bindgen]
pub struct WasmClientRemoteCore {
    state: Rc<BrowserRemoteState>,
}

#[wasm_bindgen]
impl WasmClientRemoteCore {
    /// Creates one Client Remote core over the mounted Connection and Typert services.
    ///
    /// # Errors
    ///
    /// Returns malformed service or namespace-factory failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        owner_context: JsValue,
        connection: JsValue,
        typert: JsValue,
        namespace_factory: Function,
    ) -> Result<Self, JsValue> {
        let rpc = required(&connection, "rpc", "Client connection")?;
        required_function(&rpc, "call", "Client connection rpc")?;
        required(&typert, "contexts", "Client Typert registry")?;
        required(&typert, "remotes", "Client Typert registry")?;
        Ok(Self {
            state: Rc::new(BrowserRemoteState {
                owner_context,
                rpc,
                typert,
                namespace_factory,
                methods: RefCell::new(HashMap::new()),
                namespaces: RefCell::new(HashMap::new()),
                subscriptions: RefCell::new(Vec::new()),
                next_subscription: Cell::new(0),
            }),
        })
    }

    /// Mounts one generated Remote contribution and returns its asynchronous disposer.
    #[allow(clippy::needless_pass_by_value)]
    pub fn mount(&self, caller: JsValue, contribution: JsValue) -> Promise {
        let state = self.state.clone();
        future_to_promise(async move {
            let descriptors = parse_contribution(&contribution)?;
            validate_contribution(&state, &descriptors)?;
            let remotes = required(&state.typert, "remotes", "Client Typert registry")?;
            let registry_disposer = required_function(&remotes, "register", "Typert remotes")?
                .call1(&remotes, &contribution)?
                .dyn_into::<Function>()?;
            let mut installed = Vec::new();
            for descriptor in descriptors {
                let token = MountToken::new()?;
                if let Err(error) = install_descriptor(&state, &descriptor, token, &mut installed) {
                    cleanup_mount(&state, installed, Some(registry_disposer)).await;
                    return Err(error);
                }
            }
            let closed = Rc::new(Cell::new(false));
            let dispose_state = state;
            let cleanup = Closure::wrap(Box::new(move || -> Promise {
                if closed.replace(true) {
                    return Promise::resolve(&JsValue::UNDEFINED);
                }
                let state = dispose_state.clone();
                let variants = installed.clone();
                let registry = registry_disposer.clone();
                future_to_promise(async move {
                    cleanup_mount(&state, variants, Some(registry)).await;
                    Ok(JsValue::UNDEFINED)
                })
            }) as Box<dyn FnMut() -> Promise>);
            let cleanup = cleanup.into_js_value();
            let owned_cleanup = cleanup.clone();
            let setup = Closure::wrap(
                Box::new(move || owned_cleanup.clone()) as Box<dyn FnMut() -> JsValue>
            );
            match call_method(
                &caller,
                "effect",
                &[
                    setup.into_js_value(),
                    JsValue::from_str("api-gateway.client.$mount()"),
                ],
            ) {
                Ok(owned) => Ok(owned),
                Err(error) => {
                    if let Ok(cleanup) = cleanup.dyn_into::<Function>()
                        && let Ok(result) = cleanup.call0(&JsValue::UNDEFINED)
                    {
                        let _ = JsFuture::from(Promise::resolve(&result)).await;
                    }
                    Err(error)
                }
            }
        })
    }

    /// Invokes the currently mounted direct or caller-scoped method variant.
    #[allow(clippy::needless_pass_by_value)]
    pub fn invoke(
        &self,
        caller: JsValue,
        namespace: String,
        method: String,
        values: Array,
    ) -> Promise {
        invoke_remote(self.state.clone(), caller, namespace, method, values)
    }

    /// Subscribes one caller-owned listener to a forwarded Remote event.
    ///
    /// # Errors
    ///
    /// Returns when the subscription id space is exhausted.
    #[wasm_bindgen(js_name = on)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn subscribe(
        &self,
        caller: JsValue,
        event: String,
        listener: Function,
    ) -> Result<Function, JsValue> {
        let id = self
            .state
            .next_subscription
            .get()
            .checked_add(1)
            .ok_or_else(|| js_sys::Error::new("Client Remote subscription ids exhausted"))?;
        self.state.next_subscription.set(id);
        self.state
            .subscriptions
            .borrow_mut()
            .push(EventSubscription {
                id,
                event: event.clone(),
                listener,
            });
        let state = Rc::downgrade(&self.state);
        let cleanup = Closure::wrap(Box::new(move || {
            if let Some(state) = state.upgrade() {
                state
                    .subscriptions
                    .borrow_mut()
                    .retain(|subscription| subscription.id != id);
            }
        }) as Box<dyn FnMut()>)
        .into_js_value();
        let owned_cleanup = cleanup.clone();
        let setup =
            Closure::wrap(Box::new(move || owned_cleanup.clone()) as Box<dyn FnMut() -> JsValue>);
        match call_method(
            &caller,
            "effect",
            &[
                setup.into_js_value(),
                JsValue::from_str(&format!("api-gateway.client.$on({event:?})")),
            ],
        ) {
            Ok(owned) => owned.dyn_into::<Function>(),
            Err(error) => {
                if let Ok(cleanup) = cleanup.dyn_into::<Function>() {
                    let _ = cleanup.call0(&JsValue::UNDEFINED);
                }
                Err(error)
            }
        }
    }

    /// Delivers one forwarded Remote event to a registration-order snapshot.
    #[allow(clippy::needless_pass_by_value)]
    pub fn dispatch(&self, event: String, arguments: JsValue) {
        let arguments = if Array::is_array(&arguments) {
            Array::from(&arguments)
        } else {
            Array::new()
        };
        let listeners = self
            .state
            .subscriptions
            .borrow()
            .iter()
            .filter(|subscription| subscription.event == event)
            .map(|subscription| subscription.listener.clone())
            .collect::<Vec<_>>();
        for listener in listeners {
            match listener.apply(&JsValue::UNDEFINED, &arguments) {
                Ok(result) => contain_listener_promise(&event, result),
                Err(error) => report_listener_failure(&event, &error),
            }
        }
    }
}

fn parse_contribution(value: &JsValue) -> Result<Vec<RemoteDescriptor>, JsValue> {
    required_string(value, "package", "Remote contribution")?;
    let descriptors = required(value, "descriptors", "Remote contribution")?;
    if !Array::is_array(&descriptors) {
        return Err(
            js_sys::TypeError::new("Remote contribution descriptors must be an array").into(),
        );
    }
    Array::from(&descriptors)
        .iter()
        .map(|descriptor| parse_descriptor(&descriptor))
        .collect()
}

fn parse_descriptor(value: &JsValue) -> Result<RemoteDescriptor, JsValue> {
    let namespace = required_string(value, "namespace", "Remote descriptor")?;
    let method = required_string(value, "method", "Remote descriptor")?;
    let endpoint = format!("{namespace}/{method}");
    if namespace.is_empty() || method.is_empty() {
        return Err(js_sys::Error::new("Remote namespace and method must be non-empty").into());
    }
    let invocation = required(value, "invocation", "Remote descriptor")?;
    let invocation_kind = required_string(&invocation, "kind", "Remote invocation")?;
    let direct = invocation_kind == "direct";
    let parameters_value = required(value, "parameters", "Remote descriptor")?;
    if !Array::is_array(&parameters_value) {
        return Err(js_sys::TypeError::new(&format!(
            "client api: generated Remote {endpoint} parameters must be an array"
        ))
        .into());
    }
    let parameters = Array::from(&parameters_value)
        .iter()
        .map(|parameter| {
            let wire = required_string(&parameter, "wire", "Remote parameter")?;
            let source = required_string(&parameter, "source", "Remote parameter")?;
            let lookup = optional_string(&parameter, "lookup")?;
            let codec = required(&parameter, "codec", "Remote parameter")?;
            require_strict_codec(&codec, &endpoint, &wire)?;
            Ok(RemoteParameter {
                wire,
                source,
                lookup,
                codec,
            })
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    let result = required(value, "result", "Remote descriptor")?;
    require_strict_codec(&result, &endpoint, "result")?;
    let scoped = if invocation_kind == "context" {
        let context = required_string(&invocation, "context", "Remote invocation")?;
        let wire = required_string(&invocation, "wire", "Remote invocation")?;
        let codec = required(&invocation, "codec", "Remote invocation")?;
        require_strict_codec(&codec, &endpoint, &wire)?;
        Some(ScopedProjection {
            context,
            wire,
            codec,
            parameter_index: None,
        })
    } else if invocation_kind == "direct" {
        parse_scope(value, &parameters, &endpoint)?
    } else {
        return Err(js_sys::Error::new(&format!(
            "client api: generated Remote {endpoint} has unknown invocation kind {invocation_kind:?}"
        ))
        .into());
    };
    if !direct && scoped.is_none() {
        return Err(js_sys::Error::new(&format!(
            "client api: generated Remote {endpoint} installs no callable variant"
        ))
        .into());
    }
    Ok(RemoteDescriptor {
        namespace,
        method,
        direct,
        scoped,
        parameters,
        cancellation: optional_bool(value, "cancellation")?.unwrap_or(false),
        result,
    })
}

fn parse_scope(
    descriptor: &JsValue,
    parameters: &[RemoteParameter],
    endpoint: &str,
) -> Result<Option<ScopedProjection>, JsValue> {
    let scope = Reflect::get(descriptor, &JsValue::from_str("scope"))?;
    if scope.is_undefined() || scope.is_null() {
        return Ok(None);
    }
    let context = required_string(&scope, "context", "Remote scope")?;
    let wire = required_string(&scope, "wire", "Remote scope")?;
    let matches = parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| {
            parameter.source == "lookup"
                && parameter.wire == wire
                && parameter.lookup.as_deref() == Some(context.as_str())
        })
        .collect::<Vec<_>>();
    let lookup_count = parameters
        .iter()
        .filter(|parameter| parameter.source == "lookup")
        .count();
    if lookup_count != 1 || matches.len() != 1 {
        return Err(js_sys::Error::new(&format!(
            "client api: generated Remote {endpoint} scope must select its only lookup parameter"
        ))
        .into());
    }
    let (parameter_index, parameter) = matches[0];
    Ok(Some(ScopedProjection {
        context,
        wire,
        codec: parameter.codec.clone(),
        parameter_index: Some(parameter_index),
    }))
}

fn validate_contribution(
    state: &Rc<BrowserRemoteState>,
    descriptors: &[RemoteDescriptor],
) -> Result<(), JsValue> {
    let methods = state.methods.borrow();
    let mut direct = HashSet::new();
    let mut scoped = HashSet::new();
    for descriptor in descriptors {
        if remote_service_reserved(&descriptor.namespace) {
            return Err(js_sys::Error::new(&format!(
                "client api: namespace {:?} conflicts with the Remote service",
                descriptor.namespace
            ))
            .into());
        }
        if namespace_method_reserved(&descriptor.method) {
            return Err(js_sys::Error::new(&format!(
                "client api: method {:?} conflicts with its namespace service",
                descriptor.endpoint()
            ))
            .into());
        }
        let endpoint = descriptor.endpoint();
        let current = methods.get(&endpoint);
        if descriptor.direct {
            if !direct.insert(endpoint.clone()) {
                return Err(js_sys::Error::new(&format!(
                    "client api: contribution repeats direct method {endpoint}"
                ))
                .into());
            }
            if current.is_some_and(|record| record.direct.is_some()) {
                return Err(js_sys::Error::new(&format!(
                    "client api: direct method {endpoint} is already mounted"
                ))
                .into());
            }
        }
        if descriptor.scoped.is_some() {
            if !scoped.insert(endpoint.clone()) {
                return Err(js_sys::Error::new(&format!(
                    "client api: contribution repeats scoped method {endpoint}"
                ))
                .into());
            }
            if current.is_some_and(|record| record.scoped.is_some()) {
                return Err(js_sys::Error::new(&format!(
                    "client api: scoped method {endpoint} is already mounted"
                ))
                .into());
            }
        }
    }
    Ok(())
}

fn install_descriptor(
    state: &Rc<BrowserRemoteState>,
    descriptor: &RemoteDescriptor,
    token: MountToken,
    installed: &mut Vec<InstalledVariant>,
) -> Result<(), JsValue> {
    ensure_namespace(state, &descriptor.namespace)?;
    let endpoint = descriptor.endpoint();
    let was_empty = !state.methods.borrow().contains_key(&endpoint);
    {
        let mut methods = state.methods.borrow_mut();
        let record = methods.entry(endpoint.clone()).or_default();
        if descriptor.direct {
            record.direct = Some(InstalledMethod {
                descriptor: descriptor.clone(),
                token: token.clone(),
            });
            installed.push(InstalledVariant {
                endpoint: endpoint.clone(),
                kind: MethodKind::Direct,
                token: token.clone(),
            });
        }
        if let Some(projection) = descriptor.scoped.clone() {
            record.scoped = Some(ScopedMethod {
                method: InstalledMethod {
                    descriptor: descriptor.clone(),
                    token: token.clone(),
                },
                projection,
            });
            installed.push(InstalledVariant {
                endpoint: endpoint.clone(),
                kind: MethodKind::Scoped,
                token,
            });
        }
    }
    if was_empty {
        let namespace = state.namespaces.borrow();
        let service = &namespace
            .get(&descriptor.namespace)
            .expect("namespace installed before method")
            .service;
        call_method(service, "install", &[JsValue::from_str(&descriptor.method)])?;
    }
    Ok(())
}

fn ensure_namespace(state: &Rc<BrowserRemoteState>, name: &str) -> Result<(), JsValue> {
    if state.namespaces.borrow().contains_key(name) {
        return Ok(());
    }
    let weak = Rc::downgrade(state);
    let namespace = name.to_owned();
    let invoke = Closure::wrap(Box::new(
        move |caller: JsValue, method: String, values: Array| -> Promise {
            let Some(state) = weak.upgrade() else {
                return Promise::reject(&js_sys::Error::new(&format!(
                    "client api: Remote method {namespace}/{method} is no longer mounted"
                )));
            };
            invoke_remote(state, caller, namespace.clone(), method, values)
        },
    ) as Box<dyn FnMut(JsValue, String, Array) -> Promise>);
    let handle = state.namespace_factory.call3(
        &JsValue::UNDEFINED,
        &state.owner_context,
        &JsValue::from_str(name),
        &invoke.into_js_value(),
    )?;
    let service = required(&handle, "service", "Remote namespace factory result")?;
    let dispose = required_function(&handle, "dispose", "Remote namespace factory result")?;
    state
        .namespaces
        .borrow_mut()
        .insert(name.to_owned(), NamespaceHandle { service, dispose });
    Ok(())
}

async fn cleanup_mount(
    state: &Rc<BrowserRemoteState>,
    installed: Vec<InstalledVariant>,
    registry_disposer: Option<Function>,
) {
    let mut namespace_disposers = Vec::new();
    for variant in installed.into_iter().rev() {
        variant.token.withdraw();
        let mut remove_method = None;
        {
            let mut methods = state.methods.borrow_mut();
            let Some(record) = methods.get_mut(&variant.endpoint) else {
                continue;
            };
            let matches = match variant.kind {
                MethodKind::Direct => record.direct.as_ref().is_some_and(|current| {
                    Rc::ptr_eq(&current.token.active, &variant.token.active)
                }),
                MethodKind::Scoped => record.scoped.as_ref().is_some_and(|current| {
                    Rc::ptr_eq(&current.method.token.active, &variant.token.active)
                }),
            };
            if !matches {
                continue;
            }
            match variant.kind {
                MethodKind::Direct => record.direct = None,
                MethodKind::Scoped => record.scoped = None,
            }
            if record.direct.is_none() && record.scoped.is_none() {
                methods.remove(&variant.endpoint);
                remove_method = variant
                    .endpoint
                    .split_once('/')
                    .map(|(namespace, method)| (namespace.to_owned(), method.to_owned()));
            }
        }
        let Some((namespace, method)) = remove_method else {
            continue;
        };
        if let Some(handle) = state.namespaces.borrow().get(&namespace) {
            let _ = call_method(&handle.service, "remove", &[JsValue::from_str(&method)]);
        }
        let namespace_empty = !state
            .methods
            .borrow()
            .keys()
            .any(|endpoint| endpoint.starts_with(&format!("{namespace}/")));
        if namespace_empty && let Some(handle) = state.namespaces.borrow_mut().remove(&namespace) {
            namespace_disposers.push(handle.dispose);
        }
    }
    for disposer in namespace_disposers {
        if let Ok(result) = disposer.call0(&JsValue::UNDEFINED) {
            let _ = JsFuture::from(Promise::resolve(&result)).await;
        }
    }
    if let Some(disposer) = registry_disposer
        && let Ok(result) = disposer.call0(&JsValue::UNDEFINED)
    {
        let _ = JsFuture::from(Promise::resolve(&result)).await;
    }
}

fn invoke_remote(
    state: Rc<BrowserRemoteState>,
    caller: JsValue,
    namespace: String,
    method: String,
    values: Array,
) -> Promise {
    future_to_promise(async move {
        let endpoint = format!("{namespace}/{method}");
        let record = state
            .methods
            .borrow()
            .get(&endpoint)
            .cloned()
            .ok_or_else(|| {
                js_sys::Error::new(&format!(
                    "client api: Remote method {endpoint} is no longer mounted"
                ))
            })?;
        if let Some(scoped) = &record.scoped
            && let Some(identity) = bound_context_identity(&state, &scoped.projection, &caller)?
        {
            return invoke_descriptor(
                &state,
                &caller,
                &scoped.method,
                Some(&scoped.projection),
                &values,
                Some(identity),
            )
            .await;
        }
        if let Some(direct) = &record.direct {
            return invoke_descriptor(&state, &caller, direct, None, &values, None).await;
        }
        let scoped = record.scoped.ok_or_else(|| {
            js_sys::Error::new(&format!(
                "client api: Remote method {endpoint} is no longer mounted"
            ))
        })?;
        invoke_descriptor(
            &state,
            &caller,
            &scoped.method,
            Some(&scoped.projection),
            &values,
            None,
        )
        .await
    })
}

fn bound_context_identity(
    state: &BrowserRemoteState,
    projection: &ScopedProjection,
    caller: &JsValue,
) -> Result<Option<JsValue>, JsValue> {
    let contexts = required(&state.typert, "contexts", "Client Typert registry")?;
    let binder = required_function(&contexts, "getClient", "Typert contexts")?
        .call1(&contexts, &JsValue::from_str(&projection.context))?;
    if binder.is_undefined() || binder.is_null() {
        return Ok(None);
    }
    let identity =
        required_function(&binder, "identity", "Client Context binder")?.call1(&binder, caller)?;
    Ok((!identity.is_undefined()).then_some(identity))
}

#[allow(clippy::too_many_lines)]
async fn invoke_descriptor(
    state: &BrowserRemoteState,
    caller: &JsValue,
    method: &InstalledMethod,
    projection: Option<&ScopedProjection>,
    values: &Array,
    bound_identity: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    let descriptor = &method.descriptor;
    let endpoint = descriptor.endpoint();
    if !method.token.active.get() {
        return internal_failure(&format!(
            "client api: Remote method {endpoint} is no longer mounted"
        ));
    }
    let expected = descriptor.parameters.len()
        - usize::from(projection.and_then(|value| value.parameter_index).is_some());
    let actual = usize::try_from(values.length()).unwrap_or(usize::MAX);
    let has_signal = descriptor.cancellation && actual == expected.saturating_add(1);
    if actual != expected && !has_signal {
        let contract = if descriptor.cancellation {
            format!("{expected} business argument(s) plus an optional AbortSignal")
        } else {
            format!("{expected} argument(s)")
        };
        return Err(js_sys::Error::new(&format!(
            "client api: {endpoint} expected {contract}, got {actual}"
        ))
        .into());
    }
    let null_prototype: Object = JsValue::NULL.unchecked_into();
    let args = Object::create(&null_prototype);
    if let Some(projection) = projection {
        let identity = match bound_identity {
            Some(identity) => identity,
            None => bound_context_identity(state, projection, caller)?.ok_or_else(|| {
                js_sys::Error::new(&format!(
                    "client api: {endpoint} requires a {:?} Context",
                    projection.context
                ))
            })?,
        };
        let identity = parse_codec(&projection.codec, &identity, &endpoint, &projection.wire)?;
        if !identity.is_undefined() {
            Reflect::set(&args, &JsValue::from_str(&projection.wire), &identity)?;
        }
    }
    let mut value_index = 0_u32;
    for (parameter_index, parameter) in descriptor.parameters.iter().enumerate() {
        if projection.and_then(|value| value.parameter_index) == Some(parameter_index) {
            continue;
        }
        let value = values.get(value_index);
        if value.is_instance_of::<AbortSignal>() {
            return Err(js_sys::Error::new(&format!(
                "client api: {endpoint} rejected {:?}",
                parameter.wire
            ))
            .into());
        }
        let value = parse_codec(&parameter.codec, &value, &endpoint, &parameter.wire)?;
        if !value.is_undefined() {
            Reflect::set(&args, &JsValue::from_str(&parameter.wire), &value)?;
        }
        value_index = value_index.saturating_add(1);
    }
    let caller_signal = if has_signal {
        let signal = values.get(u32::try_from(expected).unwrap_or(u32::MAX));
        if signal.is_undefined() {
            None
        } else if signal.is_instance_of::<AbortSignal>() {
            Some(signal)
        } else {
            return Err(js_sys::Error::new(&format!(
                "client api: {endpoint} expected an optional AbortSignal as its final argument"
            ))
            .into());
        }
    } else {
        None
    };
    let signal = fused_signal(&method.token.abort.signal(), caller_signal.as_ref())?;
    let payload = object(&[("args", args.into())])?;
    let response = match call_method(
        &state.rpc,
        "call",
        &[
            JsValue::from_str("/api"),
            JsValue::from_str(&endpoint),
            payload.into(),
            signal,
        ],
    ) {
        Ok(response) => match JsFuture::from(Promise::resolve(&response)).await {
            Ok(response) => response,
            Err(error) => {
                return internal_failure(&format!(
                    "client api: {endpoint} failed: {}",
                    js_error_text(&error)
                ));
            }
        },
        Err(error) => {
            return internal_failure(&format!(
                "client api: {endpoint} failed: {}",
                js_error_text(&error)
            ));
        }
    };
    if !method.token.active.get() {
        return internal_failure(&format!(
            "client api: Remote method {endpoint} is no longer mounted"
        ));
    }
    if Reflect::get(&response, &JsValue::from_str("ok"))?.as_bool() != Some(true) {
        return Ok(response);
    }
    let value = Reflect::get(&response, &JsValue::from_str("value"))?;
    let value = match parse_codec(&descriptor.result, &value, &endpoint, "result") {
        Ok(value) => value,
        Err(error) => {
            return internal_failure(&format!(
                "client api: {endpoint} failed: {}",
                js_error_text(&error)
            ));
        }
    };
    object(&[("ok", JsValue::TRUE), ("value", value)]).map(Into::into)
}

fn parse_codec(
    codec: &JsValue,
    value: &JsValue,
    endpoint: &str,
    field: &str,
) -> Result<JsValue, JsValue> {
    require_strict_codec(codec, endpoint, field)?;
    let schema = required(codec, "schema", "Remote codec")?;
    required_function(&schema, "parse", "Remote codec schema")?
        .call1(&schema, value)
        .map_err(|error| {
            let wrapped = js_sys::Error::new(&format!(
                "client api: {endpoint} rejected {field:?}: {}",
                js_error_text(&error)
            ));
            wrapped.into()
        })
}

fn require_strict_codec(codec: &JsValue, endpoint: &str, field: &str) -> Result<(), JsValue> {
    if Reflect::get(codec, &JsValue::from_str("mode"))?
        .as_string()
        .as_deref()
        != Some("strict")
    {
        return Err(js_sys::Error::new(&format!(
            "client api: generated Remote {endpoint} field {field:?} has no strict codec"
        ))
        .into());
    }
    let schema = required(codec, "schema", "Remote codec")?;
    required_function(&schema, "parse", "Remote codec schema")?;
    Ok(())
}

fn fused_signal(token: &AbortSignal, caller: Option<&JsValue>) -> Result<JsValue, JsValue> {
    let Some(caller) = caller else {
        return Ok(token.clone().into());
    };
    let constructor = required(&js_sys::global(), "AbortSignal", "globalThis")?;
    let any = required_function(&constructor, "any", "AbortSignal")?;
    any.call1(
        &constructor,
        &Array::of2(&token.clone().into(), caller).into(),
    )
}

fn internal_failure(message: &str) -> Result<JsValue, JsValue> {
    let details = Object::new();
    let error = object(&[
        ("code", JsValue::from_str("internal")),
        ("message", JsValue::from_str(message)),
        ("details", details.into()),
    ])?;
    object(&[("ok", JsValue::FALSE), ("error", error.into())]).map(Into::into)
}

fn contain_listener_promise(event: &str, result: JsValue) {
    if !result.is_instance_of::<Promise>() {
        return;
    }
    let event = event.to_owned();
    let failure = Closure::once(move |error: JsValue| report_listener_failure(&event, &error));
    let _ = Promise::from(result).catch(&failure);
    drop(failure.into_js_value());
}

fn report_listener_failure(event: &str, error: &JsValue) {
    web_sys::console::error_1(&JsValue::from_str(&format!(
        "client api: Remote event {event:?} listener threw: {}",
        js_error_text(error)
    )));
}

fn remote_service_reserved(name: &str) -> bool {
    matches!(
        name,
        "core"
            | "mount"
            | "namespace"
            | "method"
            | "invoke"
            | "subscribe"
            | "deliver"
            | "$mount"
            | "$on"
            | "$dispatch"
            | "toString"
            | "valueOf"
            | "hasOwnProperty"
            | "constructor"
            | "__proto__"
    )
}

fn namespace_method_reserved(name: &str) -> bool {
    matches!(
        name,
        "ctx"
            | "install"
            | "remove"
            | "name"
            | "namespace"
            | "invokeRemote"
            | "toString"
            | "toLocaleString"
            | "valueOf"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "constructor"
            | "__proto__"
    )
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_sys::TypeError::new(&format!("{owner} is missing {key}")).into())
    } else {
        Ok(property)
    }
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} {key} must be a function")).into())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Ok(None)
    } else {
        property
            .as_string()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
    }
}

fn optional_bool(value: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Ok(None)
    } else {
        property
            .as_bool()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a boolean")).into())
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let arguments: Array = arguments.iter().cloned().collect();
    required_function(value, name, "object")?.apply(value, &arguments)
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry)?;
    }
    Ok(value)
}

fn js_error_text(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "unknown JavaScript failure".to_owned())
}
