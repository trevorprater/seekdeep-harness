//! Rust-owned execution of file-backed JavaScript Cordis plugins.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Cursor,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use base64::Engine as _;
use boa_engine::{
    Context as JavaScriptContext, JsNativeError, JsObject, JsValue, Module, NativeFunction, Source,
    builtins::promise::PromiseState,
    context::ContextBuilder,
    js_string,
    module::{IdleModuleLoader, ModuleLoader, Referrer, SimpleModuleLoader},
    property::PropertyKey,
};
use futures::future::BoxFuture;
use num_traits::ToPrimitive;
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_cordis_timer::TIMER;
use seekdeep_llm::ContentBlock;
use seekdeep_tools::{
    TOOLS, ToolDefinition, ToolOutputDefinition, assert_supported_json_schema,
    parameter_schema_spec_to_json_schema,
};
use serde_json::Value;

use crate::sandbox_service::{
    SANDBOX_SERVICES, SandboxServiceRegistration, SandboxServiceRegistry,
};

thread_local! {
    static CJS_DEPENDENCIES: RefCell<BTreeSet<PathBuf>> = const { RefCell::new(BTreeSet::new()) };
    static ACTIVE_DYNAMIC_SERVICES: RefCell<BTreeMap<String, Arc<DynamicJavaScriptService>>> = const { RefCell::new(BTreeMap::new()) };
    static DYNAMIC_CONSOLE_TAG: RefCell<String> = const { RefCell::new(String::new()) };
}

struct ActiveDynamicServices;

impl ActiveDynamicServices {
    fn enter(services: &BTreeMap<String, Arc<DynamicJavaScriptService>>) -> Self {
        ACTIVE_DYNAMIC_SERVICES.with(|active| active.borrow_mut().clone_from(services));
        Self
    }
}

impl Drop for ActiveDynamicServices {
    fn drop(&mut self) {
        ACTIVE_DYNAMIC_SERVICES.with(|active| active.borrow_mut().clear());
    }
}

#[derive(Debug)]
enum HostCommand {
    Provide {
        name: String,
        value: Value,
    },
    ProvideDynamic {
        name: String,
        service_id: usize,
        methods: Vec<String>,
        projection: Value,
    },
    On {
        name: String,
        callback_id: usize,
        once: bool,
    },
    Timer {
        callback_id: usize,
        delay_ms: u64,
        repeat: bool,
    },
    RegisterTool(DynamicToolRegistration),
    DisposeRoot,
}

#[derive(Debug)]
struct DynamicToolRegistration {
    tool_id: usize,
    name: String,
    description: String,
    parameters: Value,
    output_schema: Value,
    timeout_ms: Option<f64>,
    has_presentation_meta: bool,
}

struct ActivationRequest {
    id: u64,
    services: BTreeMap<String, Value>,
    config: Value,
    declared: Vec<String>,
    dynamic_services: BTreeMap<String, Arc<DynamicJavaScriptService>>,
    sandboxed: bool,
    reply: tokio::sync::oneshot::Sender<Result<Vec<HostCommand>, String>>,
}

enum WorkerCommand {
    Activate(ActivationRequest),
    Invoke {
        activation_id: Option<u64>,
        method: String,
        args: Value,
        reply: tokio::sync::oneshot::Sender<Result<(Value, Vec<HostCommand>), String>>,
    },
    InvokeCallback {
        activation_id: u64,
        callback_id: usize,
        reply: tokio::sync::oneshot::Sender<Result<Vec<HostCommand>, String>>,
    },
    InvokeTool {
        activation_id: u64,
        tool_id: usize,
        args: Value,
        reply: tokio::sync::oneshot::Sender<Result<(Value, Vec<HostCommand>), String>>,
    },
    RenderTool {
        tool_id: usize,
        args: Value,
        value: Value,
        reply: mpsc::SyncSender<Result<Value, String>>,
    },
    PresentTool {
        tool_id: usize,
        args: Value,
        value: Value,
        reply: mpsc::SyncSender<Result<Value, String>>,
    },
    InvokeService {
        service_id: usize,
        method: String,
        args: Value,
        reply: mpsc::SyncSender<Result<Value, String>>,
    },
    Deactivate {
        id: u64,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}

#[derive(Clone, Debug)]
struct ModuleMetadata {
    name: String,
    inject: Vec<String>,
    handlers: Vec<String>,
    dependencies: BTreeSet<PathBuf>,
}

#[derive(Debug)]
struct RecordingModuleLoader {
    inner: Rc<SimpleModuleLoader>,
    dependencies: Rc<RefCell<BTreeSet<PathBuf>>>,
}

impl ModuleLoader for RecordingModuleLoader {
    async fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        specifier: boa_engine::JsString,
        context: &RefCell<&mut JavaScriptContext>,
    ) -> boa_engine::JsResult<Module> {
        let requested = specifier.to_std_string_escaped();
        if requested.starts_with('.')
            && let Some(parent) = referrer.path().and_then(Path::parent)
        {
            let path = parent.join(&requested).canonicalize().map_err(|error| {
                boa_engine::JsNativeError::typ()
                    .with_message(format!("could not resolve module {requested:?}: {error}"))
            })?;
            if path.extension().and_then(std::ffi::OsStr::to_str) == Some("cjs") {
                if let Some(module) = self.inner.get(&path) {
                    return Ok(module);
                }
                let module = parse_file_module(&path, &mut context.borrow_mut())?;
                self.inner.insert(path.clone(), module.clone());
                self.dependencies.borrow_mut().insert(path);
                return Ok(module);
            }
        }
        let module = self
            .inner
            .clone()
            .load_imported_module(referrer, specifier, context)
            .await?;
        if let Some(path) = module.path() {
            self.dependencies.borrow_mut().insert(path.to_path_buf());
        }
        Ok(module)
    }
}

fn parse_file_module(
    path: &Path,
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<Module> {
    if path.extension().and_then(std::ffi::OsStr::to_str) != Some("cjs") {
        let source = Source::from_filepath(path).map_err(|error| {
            boa_engine::JsNativeError::typ()
                .with_message(format!("failed to read module {}: {error}", path.display()))
        })?;
        return Module::parse(source, None, javascript);
    }
    let source = std::fs::read_to_string(path).map_err(|error| {
        boa_engine::JsNativeError::typ()
            .with_message(format!("failed to read module {}: {error}", path.display()))
    })?;
    let filename = serde_json::to_string(&path.to_string_lossy())
        .map_err(|error| boa_engine::JsNativeError::typ().with_message(error.to_string()))?;
    let directory = serde_json::to_string(
        &path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_string_lossy(),
    )
    .map_err(|error| boa_engine::JsNativeError::typ().with_message(error.to_string()))?;
    let body = serde_json::to_string(&source)
        .map_err(|error| boa_engine::JsNativeError::typ().with_message(error.to_string()))?;
    let wrapped = format!(
        "const module = {{ exports: {{}} }};\nlet exports = module.exports;\nconst execute = Function('module', 'exports', 'require', '__filename', '__dirname', {body});\nexecute(module, exports, specifier => globalThis.__seekdeep_require__(specifier, {filename}), {filename}, {directory});\nexport default module.exports;\nexport const apply = module.exports.apply;\nexport const inject = module.exports.inject;\nexport const name = module.exports.name;\n"
    );
    Module::parse(
        Source::from_reader(Cursor::new(wrapped), Some(path)),
        None,
        javascript,
    )
}

struct ModuleWorker {
    sender: mpsc::Sender<WorkerCommand>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    next_activation: AtomicU64,
    declared: Vec<String>,
    handlers: Vec<String>,
    label: String,
    contexts: parking_lot::Mutex<HashMap<u64, Context>>,
    sandboxed: bool,
}

struct ActivationEffects {
    commands: JsObject,
    disposers: JsObject,
    callbacks: JsObject,
    processed_commands: usize,
    dynamic_services: BTreeMap<String, Arc<DynamicJavaScriptService>>,
}

struct DynamicJavaScriptService {
    backend: DynamicServiceBackend,
    methods: Vec<String>,
    projection: Value,
}

enum DynamicServiceBackend {
    Worker {
        worker: Arc<ModuleWorker>,
        service_id: usize,
    },
    Native {
        registry: Arc<SandboxServiceRegistry>,
        registration: SandboxServiceRegistration,
    },
}

impl std::fmt::Debug for DynamicJavaScriptService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamicJavaScriptService")
            .field("methods", &self.methods)
            .finish_non_exhaustive()
    }
}

impl DynamicJavaScriptService {
    fn call(&self, method: &str, args: Value) -> anyhow::Result<Value> {
        match &self.backend {
            DynamicServiceBackend::Worker { worker, service_id } => {
                let (reply, outcome) = mpsc::sync_channel(1);
                worker
                    .sender
                    .send(WorkerCommand::InvokeService {
                        service_id: *service_id,
                        method: method.to_owned(),
                        args,
                        reply,
                    })
                    .map_err(|_| anyhow::anyhow!("dynamic Service worker is closed"))?;
                outcome
                    .recv()
                    .map_err(|_| {
                        anyhow::anyhow!("dynamic Service worker ended during method call")
                    })?
                    .map_err(anyhow::Error::msg)
            }
            DynamicServiceBackend::Native {
                registry,
                registration,
            } => registry.call(
                registration,
                method,
                args.as_array()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("dynamic Service arguments must be an array"))?,
            ),
        }
    }
}

impl std::fmt::Debug for ModuleWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleWorker")
            .finish_non_exhaustive()
    }
}

impl ModuleWorker {
    fn start(path: &Path, process: Value) -> anyhow::Result<(Arc<Self>, ModuleMetadata)> {
        let path = path.to_path_buf();
        let label = path.to_string_lossy().into_owned();
        let worker_path = path.clone();
        let (commands, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("seekdeep-loader-js".to_owned())
            .spawn(move || worker_main(&worker_path, process, receiver, ready_sender))?;
        let metadata = ready_receiver
            .recv()
            .map_err(|_| anyhow::anyhow!("JavaScript module worker exited during import"))?
            .map_err(anyhow::Error::msg)?;
        Ok((
            Arc::new(Self {
                sender: commands,
                join: Mutex::new(Some(join)),
                next_activation: AtomicU64::new(1),
                declared: metadata.inject.clone(),
                handlers: metadata.handlers.clone(),
                label,
                contexts: parking_lot::Mutex::new(HashMap::new()),
                sandboxed: false,
            }),
            metadata,
        ))
    }

    fn start_body(
        body: String,
        timeout_ms: u64,
        label: String,
    ) -> anyhow::Result<(Arc<Self>, ModuleMetadata)> {
        let (commands, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let worker_label = label.clone();
        let join = thread::Builder::new()
            .name("seekdeep-dynamic-cordis".to_owned())
            .spawn(move || {
                worker_main_body(&body, timeout_ms, &worker_label, receiver, ready_sender);
            })?;
        let metadata = ready_receiver
            .recv()
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker exited during load"))?
            .map_err(anyhow::Error::msg)?;
        Ok((
            Arc::new(Self {
                sender: commands,
                join: Mutex::new(Some(join)),
                next_activation: AtomicU64::new(1),
                declared: metadata.inject.clone(),
                handlers: metadata.handlers.clone(),
                label,
                contexts: parking_lot::Mutex::new(HashMap::new()),
                sandboxed: true,
            }),
            metadata,
        ))
    }

    async fn activate(self: &Arc<Self>, context: &Context, config: Value) -> anyhow::Result<()> {
        let id = self.next_activation.fetch_add(1, Ordering::Relaxed);
        let mut services = context.expression_service_snapshot();
        if let Some(tools) = context.get(TOOLS) {
            services.insert(
                "tools".to_owned(),
                serde_json::to_value(tools.schemas(None))?,
            );
        }
        let mut dynamic_services = services
            .keys()
            .chain(self.declared.iter())
            .filter_map(|name| {
                context
                    .get_named::<DynamicJavaScriptService>(name)
                    .map(|service| (name.clone(), service))
            })
            .collect::<BTreeMap<_, _>>();
        if let Some(registry) = context.get(SANDBOX_SERVICES) {
            for registration in registry.list() {
                if !context.has_named(&registration.name)
                    || dynamic_services.contains_key(&registration.name)
                {
                    continue;
                }
                dynamic_services.insert(
                    registration.name.clone(),
                    Arc::new(DynamicJavaScriptService {
                        backend: DynamicServiceBackend::Native {
                            registry: registry.clone(),
                            registration: registration.clone(),
                        },
                        methods: registration.methods.keys().cloned().collect(),
                        projection: registration.projection.clone(),
                    }),
                );
            }
        }
        let (reply, outcome) = tokio::sync::oneshot::channel();
        self.sender
            .send(WorkerCommand::Activate(ActivationRequest {
                id,
                services,
                config,
                declared: self.declared.clone(),
                dynamic_services,
                sandboxed: self.sandboxed,
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("JavaScript module worker is closed"))?;
        let commands = outcome
            .await
            .map_err(|_| anyhow::anyhow!("JavaScript module worker ended without a result"))?
            .map_err(anyhow::Error::msg)?;

        let worker = Arc::clone(self);
        context.own(EffectHandle::new("JavaScript plugin effects", move || {
            let worker = Arc::clone(&worker);
            Box::pin(async move { worker.deactivate(id).await })
        }))?;
        self.contexts.lock().insert(id, context.clone());
        self.apply_commands(context, id, commands)
    }

    fn apply_commands(
        self: &Arc<Self>,
        context: &Context,
        activation_id: u64,
        commands: Vec<HostCommand>,
    ) -> anyhow::Result<()> {
        for command in commands {
            match command {
                HostCommand::Provide { name, value } => {
                    context.provide_named(&name, Arc::new(value))?;
                }
                HostCommand::ProvideDynamic {
                    name,
                    service_id,
                    methods,
                    projection,
                } => {
                    let service = Arc::new(DynamicJavaScriptService {
                        backend: DynamicServiceBackend::Worker {
                            worker: Arc::clone(self),
                            service_id,
                        },
                        methods,
                        projection: projection.clone(),
                    });
                    context.provide_named_projected(&name, service, projection)?;
                }
                HostCommand::On {
                    name,
                    callback_id,
                    once,
                } => {
                    let worker = Arc::clone(self);
                    let owner = context.clone();
                    let fired = Arc::new(AtomicBool::new(false));
                    context.events().on(
                        context,
                        name,
                        move |_, _| {
                            let worker = Arc::clone(&worker);
                            let owner = owner.clone();
                            let fired = fired.clone();
                            Box::pin(async move {
                                if once && fired.swap(true, Ordering::AcqRel) {
                                    return Ok(EventReply::Undefined);
                                }
                                if let Err(error) = worker
                                    .invoke_callback(&owner, activation_id, callback_id)
                                    .await
                                {
                                    worker.report_guard_failure(&owner, &error);
                                    return Err(error);
                                }
                                Ok(EventReply::Undefined)
                            })
                        },
                        EventOptions::default(),
                    )?;
                }
                HostCommand::Timer {
                    callback_id,
                    delay_ms,
                    repeat,
                } => {
                    let timer = context.get(TIMER).ok_or_else(|| {
                        anyhow::anyhow!("dynamic Host timer command requires inject: ['timer']")
                    })?;
                    let worker = Arc::clone(self);
                    let owner = context.clone();
                    let callback = Arc::new(move || {
                        let worker = worker.clone();
                        let owner = owner.clone();
                        Box::pin(async move {
                            if let Err(error) = worker
                                .invoke_callback(&owner, activation_id, callback_id)
                                .await
                            {
                                worker.report_guard_failure(&owner, &error);
                            }
                        }) as BoxFuture<'static, ()>
                    });
                    if repeat {
                        timer.interval(context, callback, Duration::from_millis(delay_ms))?;
                    } else {
                        timer.timeout(context, callback, Duration::from_millis(delay_ms))?;
                    }
                }
                HostCommand::RegisterTool(registration) => {
                    self.register_dynamic_tool(context, activation_id, registration)?;
                }
                HostCommand::DisposeRoot => {
                    let root = context.root_fiber().clone();
                    tokio::spawn(async move {
                        let _ = root.dispose().await;
                    });
                }
            }
        }
        Ok(())
    }

    fn report_guard_failure(&self, context: &Context, error: &anyhow::Error) {
        if !self.sandboxed {
            return;
        }
        let _ = context.events().emit(
            context,
            "cordis/dynamic-host-guard-failure",
            &EventArgs::one(DynamicHostGuardFailure {
                plugin_id: self.label.clone(),
                message: error.to_string(),
            }),
        );
    }

    fn register_dynamic_tool(
        self: &Arc<Self>,
        context: &Context,
        activation_id: u64,
        registration: DynamicToolRegistration,
    ) -> anyhow::Result<()> {
        let DynamicToolRegistration {
            tool_id,
            name,
            description,
            parameters,
            output_schema,
            timeout_ms,
            has_presentation_meta,
        } = registration;
        let tools = context.get(TOOLS).ok_or_else(|| {
            anyhow::anyhow!("dynamic Host tool registration requires inject: ['tools']")
        })?;
        let parameters = parameter_schema_spec_to_json_schema(parameters)?;
        let Value::Object(parameters) = parameters.into_value() else {
            anyhow::bail!("dynamic Tool parameters must compile to an object schema");
        };
        let output_schema = Arc::new(assert_supported_json_schema(output_schema)?);
        let execute_worker = Arc::clone(self);
        let execute = Arc::new(move |args, _execution| {
            let worker = Arc::clone(&execute_worker);
            Box::pin(async move { worker.invoke_tool(activation_id, tool_id, args).await })
                as seekdeep_tools::runtime::ToolExecuteFuture
        });
        let render_worker = Arc::clone(self);
        let render = Arc::new(move |args: &Value, value: &Value| {
            render_worker.render_tool(tool_id, args.clone(), value.clone())
        });
        let mut output = ToolOutputDefinition::new(output_schema, render);
        if has_presentation_meta {
            let presentation_worker = Arc::clone(self);
            output = output.presentation_meta(Arc::new(move |args: &Value, value: &Value| {
                presentation_worker.present_tool(tool_id, args.clone(), value.clone())
            }));
        }
        let mut definition = ToolDefinition::new(name, description, parameters, output, execute);
        definition.timeout_ms = timeout_ms;
        tools.register(context, definition)?;
        Ok(())
    }

    async fn deactivate(&self, id: u64) -> anyhow::Result<()> {
        self.contexts.lock().remove(&id);
        let (reply, outcome) = tokio::sync::oneshot::channel();
        if self
            .sender
            .send(WorkerCommand::Deactivate { id, reply })
            .is_err()
        {
            return Ok(());
        }
        outcome
            .await
            .map_err(|_| anyhow::anyhow!("JavaScript module worker ended during disposal"))?
            .map_err(anyhow::Error::msg)
    }

    async fn invoke(self: &Arc<Self>, method: &str, args: Value) -> anyhow::Result<Value> {
        let active = self
            .contexts
            .lock()
            .iter()
            .next()
            .map(|(id, context)| (*id, context.clone()));
        let (reply, outcome) = tokio::sync::oneshot::channel();
        self.sender
            .send(WorkerCommand::Invoke {
                activation_id: active.as_ref().map(|(id, _)| *id),
                method: method.to_owned(),
                args,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker is closed"))?;
        let (value, commands) = outcome
            .await
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker ended during handler call"))?
            .map_err(anyhow::Error::msg)?;
        if let Some((activation_id, context)) = active {
            self.apply_commands(&context, activation_id, commands)?;
        }
        Ok(value)
    }

    async fn invoke_tool(
        self: &Arc<Self>,
        activation_id: u64,
        tool_id: usize,
        args: Value,
    ) -> anyhow::Result<Value> {
        let context = self
            .contexts
            .lock()
            .get(&activation_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("dynamic Tool activation is no longer active"))?;
        let (reply, outcome) = tokio::sync::oneshot::channel();
        self.sender
            .send(WorkerCommand::InvokeTool {
                activation_id,
                tool_id,
                args,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker is closed"))?;
        let (value, commands) = outcome
            .await
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker ended during Tool execution"))?
            .map_err(anyhow::Error::msg)?;
        self.apply_commands(&context, activation_id, commands)?;
        Ok(value)
    }

    fn render_tool(
        &self,
        tool_id: usize,
        args: Value,
        value: Value,
    ) -> anyhow::Result<Vec<ContentBlock>> {
        let (reply, outcome) = mpsc::sync_channel(1);
        self.sender
            .send(WorkerCommand::RenderTool {
                tool_id,
                args,
                value,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker is closed"))?;
        let rendered = outcome
            .recv()
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker ended during Tool rendering"))?
            .map_err(anyhow::Error::msg)?;
        decode_rendered_content(&rendered)
    }

    fn present_tool(&self, tool_id: usize, args: Value, value: Value) -> anyhow::Result<Value> {
        let (reply, outcome) = mpsc::sync_channel(1);
        self.sender
            .send(WorkerCommand::PresentTool {
                tool_id,
                args,
                value,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker is closed"))?;
        outcome
            .recv()
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker ended during Tool presentation"))?
            .map_err(anyhow::Error::msg)
    }

    async fn invoke_callback(
        self: &Arc<Self>,
        context: &Context,
        activation_id: u64,
        callback_id: usize,
    ) -> anyhow::Result<()> {
        let (reply, outcome) = tokio::sync::oneshot::channel();
        self.sender
            .send(WorkerCommand::InvokeCallback {
                activation_id,
                callback_id,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker is closed"))?;
        let commands = outcome
            .await
            .map_err(|_| anyhow::anyhow!("dynamic Cordis worker ended during event callback"))?
            .map_err(anyhow::Error::msg)?;
        self.apply_commands(context, activation_id, commands)
    }
}

/// Post-activation Host Guard rejection emitted by a dynamic worker callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicHostGuardFailure {
    /// Stable dynamic Plugin label supplied by the Runner.
    pub plugin_id: String,
    /// Original worker failure text.
    pub message: String,
}

fn describe_return(value: &Value) -> String {
    const LIMIT: usize = 120;
    let json = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    if json.chars().count() <= LIMIT {
        json
    } else {
        format!("{}…", json.chars().take(LIMIT).collect::<String>())
    }
}

fn decode_rendered_content(rendered: &Value) -> anyhow::Result<Vec<ContentBlock>> {
    let invalid = || {
        anyhow::anyhow!(
            "output.render returned {} — it must return an ARRAY of content blocks:\n  ✓ return [{{ type: 'text', text: String(value) }}]",
            describe_return(rendered)
        )
    };
    let values = rendered.as_array().ok_or_else(invalid)?;
    values
        .iter()
        .map(|value| -> anyhow::Result<ContentBlock> {
            let mut fields = value.as_object().cloned().ok_or_else(invalid)?;
            let block_type = fields
                .remove("type")
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(invalid)?;
            Ok(serde_json::from_value(value.clone())
                .unwrap_or(ContentBlock::Unknown { block_type, fields }))
        })
        .collect()
}

/// Exact compiled Host worker behind one dynamic package activation.
pub struct DynamicHostRuntime {
    worker: Arc<ModuleWorker>,
}

impl std::fmt::Debug for DynamicHostRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamicHostRuntime")
            .field("handlers", &self.worker.handlers)
            .finish_non_exhaustive()
    }
}

impl DynamicHostRuntime {
    /// Handler names registered by the model-authored Host body.
    #[must_use]
    pub fn handler_names(&self) -> &[String] {
        &self.worker.handlers
    }

    /// Invokes one registered handler inside its owning interpreter thread.
    ///
    /// # Errors
    ///
    /// Returns handler absence, JavaScript failure, worker failure, or a
    /// non-lossless result.
    pub async fn invoke(&self, method: &str, args: Value) -> anyhow::Result<Value> {
        anyhow::ensure!(
            self.worker
                .handlers
                .iter()
                .any(|registered| registered == method),
            "dynamic Host registered no method \"{method}\""
        );
        self.worker.invoke(method, args).await
    }
}

/// A model-authored plugin plus its exact Host invocation bridge.
#[derive(Debug)]
pub struct LoadedDynamicHostPlugin {
    pub(crate) plugin: Plugin,
    pub(crate) runtime: Arc<DynamicHostRuntime>,
}

impl Drop for ModuleWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(WorkerCommand::Shutdown);
        if let Some(join) = self.join.lock().expect("module worker join lock").take() {
            let _ = join.join();
        }
    }
}

/// Loads one JavaScript module into a dedicated Rust-owned interpreter worker.
pub(crate) struct LoadedPlugin {
    pub(crate) plugin: Plugin,
    pub(crate) dependencies: BTreeSet<PathBuf>,
}

pub(crate) fn load(path: &Path, process: Value) -> anyhow::Result<LoadedPlugin> {
    let (worker, metadata) = ModuleWorker::start(path, process)?;
    let plugin = Plugin::new(metadata.name, metadata.inject, move |context, config| {
        let worker = Arc::clone(&worker);
        Box::pin(async move { worker.activate(&context, config).await })
    });
    Ok(LoadedPlugin {
        plugin,
        dependencies: metadata.dependencies,
    })
}

pub(crate) fn load_body(body: &str, timeout_ms: u64) -> anyhow::Result<Plugin> {
    Ok(load_body_runtime(body, timeout_ms)?.plugin)
}

pub(crate) fn load_body_runtime(
    body: &str,
    timeout_ms: u64,
) -> anyhow::Result<LoadedDynamicHostPlugin> {
    load_body_runtime_named(body, timeout_ms, "dynamic")
}

pub(crate) fn load_body_runtime_named(
    body: &str,
    timeout_ms: u64,
    label: &str,
) -> anyhow::Result<LoadedDynamicHostPlugin> {
    let (worker, metadata) =
        ModuleWorker::start_body(body.to_owned(), timeout_ms, label.to_owned())?;
    let runtime = Arc::new(DynamicHostRuntime {
        worker: worker.clone(),
    });
    let plugin = Plugin::new(metadata.name, metadata.inject, move |context, config| {
        let worker = Arc::clone(&worker);
        Box::pin(async move { worker.activate(&context, config).await })
    });
    Ok(LoadedDynamicHostPlugin { plugin, runtime })
}

fn worker_main(
    path: &Path,
    process: Value,
    commands: mpsc::Receiver<WorkerCommand>,
    ready: mpsc::SyncSender<Result<ModuleMetadata, String>>,
) {
    let initialized = initialize_module(path, &process);
    drop(process);
    worker_loop(initialized, commands, &ready);
    drop(ready);
}

fn worker_main_body(
    body: &str,
    timeout_ms: u64,
    label: &str,
    commands: mpsc::Receiver<WorkerCommand>,
    ready: mpsc::SyncSender<Result<ModuleMetadata, String>>,
) {
    worker_loop(initialize_body(body, timeout_ms, label), commands, &ready);
    drop(ready);
}

fn worker_loop(
    initialized: Result<(JavaScriptContext, JsObject, ModuleMetadata), String>,
    commands: mpsc::Receiver<WorkerCommand>,
    ready: &mpsc::SyncSender<Result<ModuleMetadata, String>>,
) {
    let (mut javascript, apply, metadata) = match initialized {
        Ok(initialized) => initialized,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(metadata));
    let mut effects = HashMap::<u64, ActivationEffects>::new();
    for command in commands {
        match command {
            WorkerCommand::Activate(request) => {
                let result = activate(&mut javascript, &apply, &request, &mut effects);
                let _ = request.reply.send(result);
            }
            WorkerCommand::Invoke {
                activation_id,
                method,
                args,
                reply,
            } => {
                let _active_services = enter_first_activation_services(&effects);
                let result = invoke_handler(
                    &mut javascript,
                    activation_id.and_then(|id| effects.get_mut(&id)),
                    &method,
                    &args,
                );
                let _ = reply.send(result);
            }
            WorkerCommand::InvokeCallback {
                activation_id,
                callback_id,
                reply,
            } => {
                let result = invoke_callback(
                    &mut javascript,
                    effects.get_mut(&activation_id),
                    callback_id,
                );
                let _ = reply.send(result);
            }
            WorkerCommand::InvokeTool {
                activation_id,
                tool_id,
                args,
                reply,
            } => {
                let _active_services = enter_first_activation_services(&effects);
                let result = invoke_tool_execute(
                    &mut javascript,
                    effects.get_mut(&activation_id),
                    tool_id,
                    &args,
                );
                let _ = reply.send(result);
            }
            WorkerCommand::RenderTool {
                tool_id,
                args,
                value,
                reply,
            } => {
                let _active_services = enter_first_activation_services(&effects);
                let result = invoke_tool_render(&mut javascript, tool_id, &args, &value);
                let _ = reply.send(result);
            }
            WorkerCommand::PresentTool {
                tool_id,
                args,
                value,
                reply,
            } => {
                let _active_services = enter_first_activation_services(&effects);
                let result = invoke_tool_presentation(&mut javascript, tool_id, &args, &value);
                let _ = reply.send(result);
            }
            WorkerCommand::InvokeService {
                service_id,
                method,
                args,
                reply,
            } => {
                let result = invoke_service(&mut javascript, service_id, &method, &args);
                let _ = reply.send(result);
            }
            WorkerCommand::Deactivate { id, reply } => {
                let result = effects.remove(&id).map_or(Ok(()), |activation| {
                    deactivate_activation(&mut javascript, &activation)
                });
                let _ = reply.send(result);
            }
            WorkerCommand::Shutdown => {
                for (_, activation) in effects.drain() {
                    let _ = deactivate_activation(&mut javascript, &activation);
                }
                break;
            }
        }
    }
}

fn deactivate_activation(
    javascript: &mut JavaScriptContext,
    activation: &ActivationEffects,
) -> Result<(), String> {
    let disposers = collect_functions(&activation.disposers, javascript)?;
    deactivate(javascript, disposers)
}

fn enter_first_activation_services(
    effects: &HashMap<u64, ActivationEffects>,
) -> Option<ActiveDynamicServices> {
    effects
        .values()
        .next()
        .map(|activation| ActiveDynamicServices::enter(&activation.dynamic_services))
}

fn initialize_module(
    path: &Path,
    process: &Value,
) -> Result<(JavaScriptContext, JsObject, ModuleMetadata), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("plugin path has no directory: {}", path.display()))?;
    let inner = Rc::new(SimpleModuleLoader::new(directory).map_err(|error| error.to_string())?);
    let dependencies = Rc::new(RefCell::new(BTreeSet::new()));
    let loader = Rc::new(RecordingModuleLoader {
        inner: inner.clone(),
        dependencies: dependencies.clone(),
    });
    let mut javascript = ContextBuilder::new()
        .module_loader(loader.clone())
        .build()
        .map_err(|error| error.to_string())?;
    install_process_global(&mut javascript, process)?;
    install_commonjs_require(&mut javascript)?;
    CJS_DEPENDENCIES.with(|dependencies| dependencies.borrow_mut().clear());
    javascript
        .runtime_limits_mut()
        .set_loop_iteration_limit(1_000_000);
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("failed to resolve plugin {}: {error}", path.display()))?;
    let module =
        parse_file_module(&canonical, &mut javascript).map_err(|error| error.to_string())?;
    dependencies.borrow_mut().insert(canonical.clone());
    inner.insert(canonical, module.clone());
    let evaluated = module.load_link_evaluate(&mut javascript);
    javascript.run_jobs().map_err(|error| error.to_string())?;
    match evaluated.state() {
        PromiseState::Fulfilled(_) => {}
        PromiseState::Rejected(error) => {
            return Err(format!(
                "plugin module evaluation failed: {}",
                render_value(&error, &mut javascript)
            ));
        }
        PromiseState::Pending => return Err("plugin module evaluation did not settle".to_owned()),
    }
    let (apply, name, inject) = module_exports(&module, path, &mut javascript)?;
    CJS_DEPENDENCIES.with(|required| dependencies.borrow_mut().extend(required.borrow().clone()));
    let dependencies = dependencies.borrow().clone();
    Ok((
        javascript,
        apply,
        ModuleMetadata {
            name,
            inject,
            handlers: Vec::new(),
            dependencies,
        },
    ))
}

fn initialize_body(
    body: &str,
    timeout_ms: u64,
    label: &str,
) -> Result<(JavaScriptContext, JsObject, ModuleMetadata), String> {
    let mut javascript = ContextBuilder::new()
        .module_loader(Rc::new(IdleModuleLoader))
        .build()
        .map_err(|error| error.to_string())?;
    DYNAMIC_CONSOLE_TAG.with(|tag| tag.replace(label.to_owned()));
    install_sandbox_globals(&mut javascript)?;
    javascript
        .runtime_limits_mut()
        .set_loop_iteration_limit(timeout_ms.max(1).saturating_mul(100_000));
    let source = format!("(async function __seekdeep_dynamic_host__() {{\n{body}\n}})()");
    let returned = javascript
        .eval(Source::from_bytes(&source))
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("loop iteration limit") {
                format!("Script execution timed out after {timeout_ms}ms")
            } else {
                message
            }
        })?;
    javascript.run_jobs().map_err(|error| error.to_string())?;
    let promise = returned
        .as_object()
        .and_then(|object| boa_engine::object::builtins::JsPromise::from_object(object).ok())
        .ok_or_else(|| "dynamic Host factory did not return a promise".to_owned())?;
    let plugin = match promise.state() {
        PromiseState::Fulfilled(plugin) => plugin,
        PromiseState::Rejected(error) => return Err(render_value(&error, &mut javascript)),
        PromiseState::Pending => return Err("dynamic Host body did not settle".to_owned()),
    };
    if plugin.is_undefined() {
        return Err("the Host half returned `undefined` — did you forget `return`?".to_owned());
    }
    let (apply, name, inject) = plugin_value_exports(
        &plugin,
        Path::new("dynamic-cordis-host.mjs"),
        &mut javascript,
    )?;
    let handlers = dynamic_handler_names(&mut javascript)?;
    Ok((
        javascript,
        apply,
        ModuleMetadata {
            name,
            inject,
            handlers,
            dependencies: BTreeSet::new(),
        },
    ))
}

fn plugin_value_exports(
    plugin: &JsValue,
    path: &Path,
    javascript: &mut JavaScriptContext,
) -> Result<(JsObject, String, Vec<String>), String> {
    let object = plugin.as_object().ok_or_else(|| {
        "the Host half must return a Plugin function or an object with apply(ctx)".to_owned()
    })?;
    let apply = if object.is_callable() {
        object.clone()
    } else {
        object
            .get(js_string!("apply"), javascript)
            .map_err(|error| error.to_string())?
            .as_object()
            .filter(JsObject::is_callable)
            .ok_or_else(|| {
                "the Host half must return a Plugin function or an object with apply(ctx)"
                    .to_owned()
            })?
    };
    let inject = object
        .get(js_string!("inject"), javascript)
        .map_err(|error| error.to_string())?
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .map_or_else(Vec::new, |value| match value {
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Value::Object(values) => values.into_iter().map(|(name, _)| name).collect(),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Vec::new(),
        });
    let name = object
        .get(js_string!("name"), javascript)
        .ok()
        .and_then(|value| value.as_string().map(|value| value.to_std_string_escaped()))
        .or_else(|| {
            path.file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "dynamic-cordis-host".to_owned());
    Ok((apply, name, inject))
}

#[allow(
    clippy::too_many_lines,
    reason = "the embedded sandbox bootstrap is reviewed as one security unit"
)]
fn install_sandbox_globals(javascript: &mut JavaScriptContext) -> Result<(), String> {
    javascript
        .register_global_builtin_callable(
            js_string!("btoa"),
            1,
            NativeFunction::from_fn_ptr(sandbox_btoa),
        )
        .map_err(|error| error.to_string())?;
    javascript
        .register_global_builtin_callable(
            js_string!("__seekdeep_utf8_encode__"),
            1,
            NativeFunction::from_fn_ptr(sandbox_utf8_encode),
        )
        .map_err(|error| error.to_string())?;
    javascript
        .register_global_builtin_callable(
            js_string!("__seekdeep_utf8_decode__"),
            1,
            NativeFunction::from_fn_ptr(sandbox_utf8_decode),
        )
        .map_err(|error| error.to_string())?;
    javascript
        .register_global_builtin_callable(
            js_string!("__seekdeep_console__"),
            1,
            NativeFunction::from_fn_ptr(sandbox_console),
        )
        .map_err(|error| error.to_string())?;
    javascript
        .register_global_builtin_callable(
            js_string!("__seekdeep_service_call__"),
            3,
            NativeFunction::from_fn_ptr(dynamic_service_call),
        )
        .map_err(|error| error.to_string())?;
    javascript
        .register_global_builtin_callable(
            js_string!("atob"),
            1,
            NativeFunction::from_fn_ptr(sandbox_atob),
        )
        .map_err(|error| error.to_string())?;
    javascript
        .eval(Source::from_bytes(
            r#"
globalThis.console = Object.freeze({
  log: (...values) => globalThis.__seekdeep_console__('log', ...values),
  info: (...values) => globalThis.__seekdeep_console__('info', ...values),
  warn: (...values) => globalThis.__seekdeep_console__('warn', ...values),
  error: (...values) => globalThis.__seekdeep_console__('error', ...values),
  debug: (...values) => globalThis.__seekdeep_console__('debug', ...values),
});
const unavailable = (name, redirect) => () => { throw new Error(`${name} is not available in the dynamic package sandbox — ${redirect}`); };
globalThis.require = unavailable('require', "Node modules are unavailable. Use the cordis services on ctx instead — e.g. inject: ['fs'] for files, ['web'] for HTTP, ['bash'] for processes; query Service.listService with cordis_inspect_query first.");
const timerRedirect = "Node timers are unavailable. Use the cordis timer service instead: declare inject: ['timer'] on your plugin and call ctx.timeout / ctx.interval after querying Host Service.listService for the exact overloads. Those calls are fiber effects, cleaned up automatically when stopped.";
for (const name of ['setTimeout', 'setInterval', 'setImmediate', 'clearTimeout', 'clearInterval']) globalThis[name] = unavailable(name, timerRedirect);
globalThis.fetch = unavailable('fetch', "Network access goes through the cordis web service: declare inject: ['web'] and call ctx.web (query Host Service.listService with cordis_inspect_query for its methods).");
globalThis.TextEncoder = class TextEncoder {
  encode(value) { return globalThis.__seekdeep_utf8_encode__(String(value)); }
};
globalThis.TextDecoder = class TextDecoder {
  decode(bytes) { return globalThis.__seekdeep_utf8_decode__(Array.from(bytes ?? [])); }
};
globalThis.__seekdeep_handlers__ = Object.create(null);
globalThis.__seekdeep_dynamic_tools__ = [];
globalThis.__seekdeep_provided_services__ = [];
const dynamicToolIds = new WeakMap();
const cloneHandlerJson = (root, path) => {
  const ancestors = new Set();
  const clone = (value, at) => {
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
    if (typeof value === 'number' && Number.isFinite(value) && !Object.is(value, -0)) return value;
    const reject = () => { throw new Error(`${at} must be lossless JSON data (objects, arrays, strings, numbers, booleans, null) — not a class instance, function, Map/Set, Date, or undefined. Return a plain object built from the values you need, or return null when the caller needs no value back.`); };
    if (typeof value !== 'object' || ancestors.has(value)) return reject();
    const array = Array.isArray(value);
    const prototype = Object.getPrototypeOf(value);
    if (array ? prototype !== Array.prototype : prototype !== Object.prototype && prototype !== null) return reject();
    const ownKeys = Reflect.ownKeys(value);
    if (array ? ownKeys.length !== value.length + 1
      : ownKeys.some(key => typeof key !== 'string' || !Object.prototype.propertyIsEnumerable.call(value, key))) return reject();
    ancestors.add(value);
    let output;
    if (array) {
      output = [];
      for (let index = 0; index < value.length; index++) {
        if (!Object.hasOwn(value, index)) return reject();
        output[index] = clone(value[index], `${at}[${index}]`);
      }
    } else {
      output = {};
      for (const [key, item] of Object.entries(value)) output[key] = clone(item, `${at}.${key}`);
    }
    ancestors.delete(value);
    return output;
  };
  return clone(root, path);
};
globalThis.__seekdeep_clone_json__ = cloneHandlerJson;
globalThis.harness = Object.freeze({
  defineTool(definition) {
    if (typeof definition !== 'object' || definition === null || Array.isArray(definition)) {
      throw new Error('harness.defineTool options must be an object');
    }
    if (typeof definition.parameters !== 'object' || definition.parameters === null || Array.isArray(definition.parameters)) {
      throw new Error(`harness.defineTool("${definition.name}") parameters must be an object`);
    }
    if (typeof definition.output !== 'object' || definition.output === null) {
      throw new Error('harness.defineTool output must declare { schema, render, presentationMeta? }');
    }
    if (typeof definition.output.render !== 'function') {
      throw new Error('harness.defineTool output.render must be a function');
    }
    if (definition.output.presentationMeta !== undefined && typeof definition.output.presentationMeta !== 'function') {
      throw new Error('harness.defineTool output.presentationMeta must be a function when present');
    }
    if (typeof definition.execute !== 'function') {
      throw new Error('harness.defineTool execute must be a function');
    }
    if (typeof definition.name !== 'string' || definition.name.length === 0) {
      throw new Error('harness.defineTool name must be a non-empty string');
    }
    if (typeof definition.description !== 'string' || definition.description.length === 0) {
      throw new Error(`harness.defineTool("${definition.name}") needs a description`);
    }
    const toolId = globalThis.__seekdeep_dynamic_tools__.push(definition) - 1;
    dynamicToolIds.set(definition, toolId);
    return definition;
  },
  registerTool(ctx, tool) {
    return ctx.tools.register(tool);
  },
  handle(method, fn) {
    if (typeof method !== 'string' || method.length === 0) {
      throw new Error('harness.handle(method, fn) needs a non-empty string method name');
    }
    if (typeof fn !== 'function') {
      throw new Error(`harness.handle("${method}") needs a handler function as its second argument`);
    }
    const entry = {
      fn: async args => cloneHandlerJson(await fn(args), `harness.handle("${method}") result`),
    };
    globalThis.__seekdeep_handlers__[method] = entry;
    return () => {
      if (globalThis.__seekdeep_handlers__[method] === entry) delete globalThis.__seekdeep_handlers__[method];
    };
  },
});
globalThis.__seekdeep_tool_command__ = tool => {
  const toolId = dynamicToolIds.get(tool);
  if (toolId === undefined) {
    throw new Error('harness.registerTool accepts only a definition returned by harness.defineTool');
  }
  return {
    type: 'registerTool',
    toolId,
    name: tool.name,
    description: tool.description,
    parameters: cloneHandlerJson(tool.parameters, `harness.defineTool("${tool.name}") parameters`),
    outputSchema: cloneHandlerJson(tool.output.schema, `harness.defineTool("${tool.name}") output.schema`),
    hasPresentationMeta: typeof tool.output.presentationMeta === 'function',
    ...(tool.timeoutMs === undefined ? {} : { timeoutMs: tool.timeoutMs }),
  };
};
"#,
        ))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn dynamic_handler_names(javascript: &mut JavaScriptContext) -> Result<Vec<String>, String> {
    javascript
        .eval(Source::from_bytes(
            "Object.keys(globalThis.__seekdeep_handlers__)",
        ))
        .map_err(|error| error.to_string())?
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| "dynamic Host handler registry is not an array".to_owned())?
        .into_iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "dynamic Host handler name is not a string".to_owned())
        })
        .collect()
}

fn sandbox_btoa(
    _this: &JsValue,
    arguments: &[JsValue],
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let input = arguments
        .first()
        .cloned()
        .unwrap_or_else(JsValue::undefined)
        .to_string(javascript)?
        .to_std_string_escaped();
    let encoded = base64::engine::general_purpose::STANDARD.encode(input.as_bytes());
    Ok(JsValue::from(boa_engine::JsString::from(encoded.as_str())))
}

fn sandbox_atob(
    _this: &JsValue,
    arguments: &[JsValue],
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let input = arguments
        .first()
        .cloned()
        .unwrap_or_else(JsValue::undefined)
        .to_string(javascript)?
        .to_std_string_escaped();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|error| JsNativeError::typ().with_message(error.to_string()))?;
    let decoded = String::from_utf8(bytes)
        .map_err(|error| JsNativeError::typ().with_message(error.to_string()))?;
    Ok(JsValue::from(boa_engine::JsString::from(decoded.as_str())))
}

fn sandbox_utf8_encode(
    _this: &JsValue,
    arguments: &[JsValue],
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let text = arguments
        .first()
        .cloned()
        .unwrap_or_else(JsValue::undefined)
        .to_string(javascript)?
        .to_std_string_escaped();
    JsValue::from_json(&serde_json::json!(text.as_bytes()), javascript)
}

fn sandbox_utf8_decode(
    _this: &JsValue,
    arguments: &[JsValue],
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let value = match arguments.first() {
        Some(value) => value.to_json(javascript)?.ok_or_else(|| {
            JsNativeError::typ().with_message("TextDecoder input must be an array of bytes")
        })?,
        None => Value::Array(Vec::new()),
    };
    let bytes = value
        .as_array()
        .ok_or_else(|| JsNativeError::typ().with_message("TextDecoder input must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .and_then(|value| value.to_u8())
                .ok_or_else(|| JsNativeError::typ().with_message("TextDecoder byte is invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(JsValue::from(boa_engine::JsString::from(text.as_ref())))
}

fn sandbox_console(
    _this: &JsValue,
    arguments: &[JsValue],
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let level = arguments
        .first()
        .cloned()
        .unwrap_or_else(JsValue::undefined)
        .to_string(javascript)?
        .to_std_string_escaped();
    let values = arguments
        .iter()
        .skip(1)
        .map(|value| render_value(value, javascript))
        .collect::<Vec<_>>()
        .join(" ");
    let tag = DYNAMIC_CONSOLE_TAG.with(|tag| format!("[cordis:{}]", tag.borrow()));
    if level == "error" {
        eprintln!("{tag} {values}");
    } else {
        println!("{tag} {values}");
    }
    Ok(JsValue::undefined())
}

fn dynamic_service_call(
    _this: &JsValue,
    arguments: &[JsValue],
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let name = arguments
        .first()
        .cloned()
        .unwrap_or_else(JsValue::undefined)
        .to_string(javascript)?
        .to_std_string_escaped();
    let method = arguments
        .get(1)
        .cloned()
        .unwrap_or_else(JsValue::undefined)
        .to_string(javascript)?
        .to_std_string_escaped();
    let args = match arguments.get(2) {
        Some(args) => args.to_json(javascript)?.ok_or_else(|| {
            JsNativeError::typ().with_message("dynamic Service arguments are not JSON")
        })?,
        None => Value::Array(Vec::new()),
    };
    let service = ACTIVE_DYNAMIC_SERVICES
        .with(|services| services.borrow().get(&name).cloned())
        .ok_or_else(|| {
            JsNativeError::error()
                .with_message(format!("dynamic Service \"{name}\" is no longer available"))
        })?;
    let value = service.call(&method, args).map_err(|error| {
        JsNativeError::error().with_message(format!("dynamic Service {name}.{method}: {error}"))
    })?;
    JsValue::from_json(&value, javascript)
}

fn module_exports(
    module: &Module,
    path: &Path,
    javascript: &mut JavaScriptContext,
) -> Result<(JsObject, String, Vec<String>), String> {
    let namespace = module.namespace(javascript);
    let default_export = namespace.get(js_string!("default"), javascript).ok();
    let plugin_object = default_export
        .as_ref()
        .and_then(JsValue::as_object)
        .filter(|object| !object.is_callable());
    let apply = default_export
        .filter(|value| value.as_object().is_some_and(|object| object.is_callable()))
        .or_else(|| {
            namespace
                .get(js_string!("apply"), javascript)
                .ok()
                .filter(|value| value.as_object().is_some_and(|object| object.is_callable()))
        })
        .or_else(|| {
            plugin_object
                .as_ref()
                .and_then(|plugin| plugin.get(js_string!("apply"), javascript).ok())
        })
        .and_then(|value| value.as_object())
        .filter(JsObject::is_callable)
        .ok_or_else(|| "plugin module must export a function as default or apply".to_owned())?;
    let inject_value = namespace
        .get(js_string!("inject"), javascript)
        .ok()
        .filter(|value| !value.is_undefined())
        .or_else(|| {
            plugin_object
                .as_ref()
                .and_then(|plugin| plugin.get(js_string!("inject"), javascript).ok())
        })
        .unwrap_or_else(JsValue::undefined);
    let inject = inject_value
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .map_or_else(Vec::new, |value| match value {
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Value::Object(values) => values.into_iter().map(|(name, _)| name).collect(),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Vec::new(),
        });
    let name_value = namespace
        .get(js_string!("name"), javascript)
        .ok()
        .filter(|value| !value.is_undefined())
        .or_else(|| {
            plugin_object
                .as_ref()
                .and_then(|plugin| plugin.get(js_string!("name"), javascript).ok())
        });
    let name = name_value
        .and_then(|value| value.as_string().map(|value| value.to_std_string_escaped()))
        .or_else(|| {
            path.file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "javascript-plugin".to_owned());
    Ok((apply, name, inject))
}

fn install_process_global(
    javascript: &mut JavaScriptContext,
    process: &Value,
) -> Result<(), String> {
    let environment = process
        .get("env")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::default()));
    let platform = process.get("platform").cloned().unwrap_or(Value::Null);
    let cwd = process.get("cwd").cloned().unwrap_or(Value::Null);
    let executable = process.get("execPath").cloned().unwrap_or(Value::Null);
    let version = process.get("version").cloned().unwrap_or(Value::Null);
    let source = format!(
        "globalThis.process = Object.freeze({{ env: Object.freeze({environment}), platform: {platform}, version: {version}, execPath: {executable}, cwd: () => {cwd} }});",
    );
    javascript
        .eval(Source::from_bytes(&source))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn install_commonjs_require(javascript: &mut JavaScriptContext) -> Result<(), String> {
    javascript
        .register_global_builtin_callable(
            js_string!("__seekdeep_require__"),
            2,
            NativeFunction::from_fn_ptr(commonjs_require),
        )
        .map_err(|error| error.to_string())?;
    javascript
        .eval(Source::from_bytes(
            "globalThis.__seekdeep_cjs_cache__ = Object.create(null);",
        ))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn commonjs_require(
    _this: &JsValue,
    arguments: &[JsValue],
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let specifier = arguments
        .first()
        .ok_or_else(|| JsNativeError::typ().with_message("require specifier is missing"))?
        .to_string(javascript)?
        .to_std_string_escaped();
    let parent = arguments
        .get(1)
        .ok_or_else(|| JsNativeError::typ().with_message("require parent is missing"))?
        .to_string(javascript)?
        .to_std_string_escaped();
    let path = resolve_commonjs_path(&specifier, Path::new(&parent)).map_err(|error| {
        JsNativeError::typ().with_message(format!("cannot require {specifier:?}: {error}"))
    })?;
    CJS_DEPENDENCIES.with(|dependencies| {
        dependencies.borrow_mut().insert(path.clone());
    });
    let cache = javascript
        .global_object()
        .get(js_string!("__seekdeep_cjs_cache__"), javascript)?
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("CommonJS cache is unavailable"))?;
    let key = path.to_string_lossy().into_owned();
    let property = boa_engine::JsString::from(key.as_str());
    let cached = cache.get(property.clone(), javascript)?;
    if !cached.is_undefined() {
        return Ok(cached);
    }
    let value = if path.extension().and_then(std::ffi::OsStr::to_str) == Some("json") {
        let source = std::fs::read_to_string(&path).map_err(|error| {
            JsNativeError::typ().with_message(format!("failed to read {}: {error}", path.display()))
        })?;
        let value: Value = serde_json::from_str(&source).map_err(|error| {
            JsNativeError::syntax()
                .with_message(format!("failed to parse {}: {error}", path.display()))
        })?;
        JsValue::from_json(&value, javascript)?
    } else {
        evaluate_commonjs(&path, javascript)?
    };
    cache.set(property, value.clone(), true, javascript)?;
    Ok(value)
}

fn evaluate_commonjs(
    path: &Path,
    javascript: &mut JavaScriptContext,
) -> boa_engine::JsResult<JsValue> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        JsNativeError::typ().with_message(format!(
            "failed to read CommonJS module {}: {error}",
            path.display()
        ))
    })?;
    let body = serde_json::to_string(&source)
        .map_err(|error| JsNativeError::typ().with_message(error.to_string()))?;
    let filename = serde_json::to_string(&path.to_string_lossy())
        .map_err(|error| JsNativeError::typ().with_message(error.to_string()))?;
    let directory = serde_json::to_string(
        &path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_string_lossy(),
    )
    .map_err(|error| JsNativeError::typ().with_message(error.to_string()))?;
    let program = format!(
        "(() => {{ const module = {{ exports: {{}} }}; const execute = Function('module', 'exports', 'require', '__filename', '__dirname', {body}); execute(module, module.exports, specifier => globalThis.__seekdeep_require__(specifier, {filename}), {filename}, {directory}); return module.exports; }})()"
    );
    javascript.eval(Source::from_bytes(&program))
}

fn resolve_commonjs_path(specifier: &str, parent: &Path) -> anyhow::Result<PathBuf> {
    let base = parent
        .parent()
        .ok_or_else(|| anyhow::anyhow!("require parent has no directory"))?;
    let candidate = if Path::new(specifier).is_absolute() {
        PathBuf::from(specifier)
    } else if specifier.starts_with('.') {
        base.join(specifier)
    } else {
        let mut found = None;
        for ancestor in base.ancestors() {
            let package = ancestor.join("node_modules").join(specifier);
            if package.exists() {
                found = Some(package);
                break;
            }
        }
        found.ok_or_else(|| anyhow::anyhow!("package not found"))?
    };
    resolve_commonjs_candidate(&candidate)
}

fn resolve_commonjs_candidate(candidate: &Path) -> anyhow::Result<PathBuf> {
    if candidate.is_file() {
        return Ok(candidate.canonicalize()?);
    }
    for extension in ["cjs", "js", "json"] {
        let path = candidate.with_extension(extension);
        if path.is_file() {
            return Ok(path.canonicalize()?);
        }
    }
    if candidate.is_dir() {
        let manifest_path = candidate.join("package.json");
        if let Ok(source) = std::fs::read_to_string(&manifest_path) {
            let manifest: Value = serde_json::from_str(&source)?;
            if let Some(main) = manifest.get("main").and_then(Value::as_str) {
                return resolve_commonjs_candidate(&candidate.join(main));
            }
        }
        for name in ["index.cjs", "index.js", "index.json"] {
            let path = candidate.join(name);
            if path.is_file() {
                return Ok(path.canonicalize()?);
            }
        }
    }
    anyhow::bail!("module path does not exist: {}", candidate.display())
}

fn activate(
    javascript: &mut JavaScriptContext,
    apply: &JsObject,
    request: &ActivationRequest,
    effects: &mut HashMap<u64, ActivationEffects>,
) -> Result<Vec<HostCommand>, String> {
    let _active_services = ActiveDynamicServices::enter(&request.dynamic_services);
    let ctx = prepare_activation_context(javascript, request)?;
    let config =
        JsValue::from_json(&request.config, javascript).map_err(|error| error.to_string())?;
    let returned = apply
        .call(&JsValue::undefined(), &[ctx, config], javascript)
        .map_err(|error| error.to_string())?;
    settle_returned(&returned, javascript)?;

    let command_ledger = javascript
        .global_object()
        .get(js_string!("__seekdeep_commands__"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| "JavaScript command ledger is not an array".to_owned())?;
    let disposers = javascript
        .global_object()
        .get(js_string!("__seekdeep_disposers__"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| "JavaScript disposer ledger is not an array".to_owned())?;
    let callbacks = javascript
        .global_object()
        .get(js_string!("__seekdeep_callbacks__"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| "JavaScript callback ledger is not an array".to_owned())?;
    let mut activation = ActivationEffects {
        commands: command_ledger,
        disposers,
        callbacks,
        processed_commands: 0,
        dynamic_services: request.dynamic_services.clone(),
    };
    let commands = read_commands(javascript, &mut activation)?;
    effects.insert(request.id, activation);
    cleanup_activation_globals(javascript);
    Ok(commands)
}

fn read_commands(
    javascript: &mut JavaScriptContext,
    activation: &mut ActivationEffects,
) -> Result<Vec<HostCommand>, String> {
    let length = activation
        .commands
        .get(js_string!("length"), javascript)
        .map_err(|error| error.to_string())?
        .to_length(javascript)
        .map_err(|error| error.to_string())?;
    let length = usize::try_from(length).map_err(|_| "command ledger is too large".to_owned())?;
    let mut commands = Vec::new();
    for index in activation.processed_commands..length {
        let command = activation
            .commands
            .get(PropertyKey::from(index as u64), javascript)
            .map_err(|error| error.to_string())?
            .to_json(javascript)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "JavaScript plugin command must be JSON-compatible".to_owned())?;
        if command.get("active").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        commands.push(parse_host_command(&command)?);
    }
    activation.processed_commands = length;
    Ok(commands)
}

fn parse_host_command(command: &Value) -> Result<HostCommand, String> {
    let command = command
        .as_object()
        .ok_or_else(|| "JavaScript command is not an object".to_owned())?;
    match command.get("type").and_then(Value::as_str) {
        Some("provide") => Ok(HostCommand::Provide {
            name: command
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "JavaScript provide name is not a string".to_owned())?
                .to_owned(),
            value: command
                .get("value")
                .cloned()
                .ok_or_else(|| "JavaScript provide value is not JSON-compatible".to_owned())?,
        }),
        Some("provideDynamic") => Ok(HostCommand::ProvideDynamic {
            name: command
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "JavaScript dynamic Service name is not a string".to_owned())?
                .to_owned(),
            service_id: command
                .get("serviceId")
                .and_then(Value::as_f64)
                .and_then(|value| value.to_usize())
                .ok_or_else(|| "JavaScript dynamic Service id is invalid".to_owned())?,
            methods: command
                .get("methods")
                .and_then(Value::as_array)
                .ok_or_else(|| "JavaScript dynamic Service methods are invalid".to_owned())?
                .iter()
                .map(|method| {
                    method.as_str().map(str::to_owned).ok_or_else(|| {
                        "JavaScript dynamic Service method is not a string".to_owned()
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            projection: command
                .get("projection")
                .cloned()
                .ok_or_else(|| "JavaScript dynamic Service projection is missing".to_owned())?,
        }),
        Some("on") => Ok(HostCommand::On {
            name: command
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "JavaScript event name is not a string".to_owned())?
                .to_owned(),
            callback_id: command
                .get("callbackId")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| "JavaScript event callback id is invalid".to_owned())?,
            once: command
                .get("once")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        Some("timer") => Ok(HostCommand::Timer {
            callback_id: command
                .get("callbackId")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| "JavaScript timer callback id is invalid".to_owned())?,
            delay_ms: command
                .get("delay")
                .and_then(Value::as_f64)
                .and_then(|delay| delay.to_u64())
                .ok_or_else(|| "JavaScript timer delay is invalid".to_owned())?,
            repeat: command
                .get("repeat")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        Some("registerTool") => parse_dynamic_tool(command).map(HostCommand::RegisterTool),
        Some("disposeRoot") => Ok(HostCommand::DisposeRoot),
        Some(kind) => Err(format!("unknown JavaScript plugin command {kind:?}")),
        None => Err("JavaScript plugin command has no type".to_owned()),
    }
}

fn parse_dynamic_tool(
    command: &serde_json::Map<String, Value>,
) -> Result<DynamicToolRegistration, String> {
    Ok(DynamicToolRegistration {
        tool_id: command
            .get("toolId")
            .and_then(Value::as_f64)
            .and_then(|value| value.to_usize())
            .ok_or_else(|| "JavaScript tool id is invalid".to_owned())?,
        name: command
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "JavaScript tool name is not a string".to_owned())?
            .to_owned(),
        description: command
            .get("description")
            .and_then(Value::as_str)
            .ok_or_else(|| "JavaScript tool description is not a string".to_owned())?
            .to_owned(),
        parameters: command
            .get("parameters")
            .cloned()
            .ok_or_else(|| "JavaScript tool parameters are missing".to_owned())?,
        output_schema: command
            .get("outputSchema")
            .cloned()
            .ok_or_else(|| "JavaScript tool output schema is missing".to_owned())?,
        timeout_ms: command.get("timeoutMs").and_then(Value::as_f64),
        has_presentation_meta: command
            .get("hasPresentationMeta")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the embedded Context facade is reviewed as one security unit"
)]
fn prepare_activation_context(
    javascript: &mut JavaScriptContext,
    request: &ActivationRequest,
) -> Result<JsValue, String> {
    let services = serde_json::to_string(&request.services).map_err(|error| error.to_string())?;
    let declared = serde_json::to_string(&request.declared).map_err(|error| error.to_string())?;
    let dynamic_services = request
        .dynamic_services
        .iter()
        .map(|(name, service)| {
            (
                name.clone(),
                serde_json::json!({
                    "methods": service.methods,
                    "projection": service.projection,
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let dynamic_services =
        serde_json::to_string(&dynamic_services).map_err(|error| error.to_string())?;
    let sandboxed = request.sandboxed;
    let setup = format!(
        r#"
(() => {{
  const services = {services};
  const dynamicServices = {dynamic_services};
  const declared = new Set({declared});
  const sandboxed = {sandboxed};
  const verbs = new Set(['effect', 'on', 'once', 'provide', 'timeout', 'interval', 'setTimeout', 'setInterval', 'throttle', 'debounce']);
  const timerVerbs = new Set(['timeout', 'interval', 'setTimeout', 'setInterval', 'throttle', 'debounce']);
  const commands = globalThis.__seekdeep_commands__ = [];
  const disposers = globalThis.__seekdeep_disposers__ = [];
  const callbacks = globalThis.__seekdeep_callbacks__ = [];
  const cloneJson = globalThis.__seekdeep_clone_json__ ?? (value => value);
  for (const [name, descriptor] of Object.entries(dynamicServices)) {{
    const projection = descriptor.projection !== null && typeof descriptor.projection === 'object'
      ? descriptor.projection : {{ value: descriptor.projection }};
    const methods = new Set(descriptor.methods);
    services[name] = new Proxy(projection, {{
      get(target, property) {{
        if (typeof property === 'string' && methods.has(property)) {{
          return (...args) => globalThis.__seekdeep_service_call__(name, property, args);
        }}
        return Reflect.get(target, property, target);
      }},
      set() {{ throw new TypeError(`dynamic Service "${{name}}" is read-only`); }},
    }});
  }}
  const listen = (name, callback, once) => {{
    if (typeof name !== 'string' || name.length === 0) throw new Error('ctx.on(event, callback) needs a non-empty string event name');
    if (typeof callback !== 'function') throw new Error(`ctx.${{once ? 'once' : 'on'}}("${{name}}") needs a callback function`);
    const callbackId = callbacks.push(callback) - 1;
    const command = {{ type: 'on', name, callbackId, once, active: true }};
    commands.push(command);
    return () => {{ command.active = false; callbacks[callbackId] = undefined; }};
  }};
  const schedule = (callback, delay, repeat) => {{
    if (!declared.has('timer')) throw new Error("service \"timer\" is not injected. Declare it: inject: ['timer', …] on your plugin, so cordis parks this dynamic package if the provider later goes away.");
    if (typeof callback !== 'function') throw new Error(`ctx.${{repeat ? 'interval' : 'timeout'}}(callback, delay) needs a callback function`);
    delay = Math.max(0, Math.floor(Number(delay)));
    if (!Number.isFinite(delay)) delay = 0;
    const callbackId = callbacks.push(callback) - 1;
    const command = {{ type: 'timer', callbackId, delay, repeat, active: true }};
    commands.push(command);
    return () => {{ command.active = false; callbacks[callbackId] = undefined; }};
  }};
  const toolSchemas = Array.isArray(services.tools) ? services.tools : [];
  const tools = Object.freeze({{
    register(tool) {{
      const command = globalThis.__seekdeep_tool_command__(tool);
      command.active = true;
      commands.push(command);
      return () => {{ command.active = false; }};
    }},
    schemas: () => toolSchemas.map(schema => ({{ ...schema }})),
    get: name => toolSchemas.find(schema => schema.name === name),
  }});
  const target = {{
    tools,
    get: name => name === 'tools' ? tools : services[name],
    on: (name, callback) => listen(name, callback, false),
    once: (name, callback) => listen(name, callback, true),
    timeout: (callback, delay) => schedule(callback, delay, false),
    interval: (callback, delay) => schedule(callback, delay, true),
    setTimeout: (callback, delay) => schedule(callback, delay, false),
    setInterval: (callback, delay) => schedule(callback, delay, true),
    provide: (name, value) => {{
      name = String(name);
      services[name] = value;
      const entries = value !== null && (typeof value === 'object' || typeof value === 'function')
        ? Object.entries(value) : [];
      const methods = entries.filter(([, item]) => typeof item === 'function').map(([key]) => key);
      let command;
      if (methods.length > 0) {{
        const serviceId = globalThis.__seekdeep_provided_services__.push(value) - 1;
        const projection = {{}};
        for (const [key, item] of entries) {{
          if (typeof item !== 'function') projection[key] = cloneJson(item, `ctx.provide("${{name}}").${{key}}`);
        }}
        command = {{ type: 'provideDynamic', name, serviceId, methods, projection, active: true }};
      }} else {{
        command = {{ type: 'provide', name, value: cloneJson(value, `ctx.provide("${{name}}")`), active: true }};
      }}
      commands.push(command);
      return () => {{ command.active = false; }};
    }},
    effect: execute => {{
      const disposer = execute();
      if (typeof disposer === 'function') disposers.push(disposer);
      return () => undefined;
    }},
  }};
  if (!sandboxed) {{
    target.reflect = {{ provide: target.provide }};
    target.fiber = {{
      dispose: () => {{ commands.push({{ type: 'disposeRoot' }}); return Promise.resolve(); }},
    }};
  }}
  const denyRead = name => {{
    if (Object.prototype.hasOwnProperty.call(services, name)) {{
      throw new Error(`service "${{name}}" is not injected. Declare it: inject: ['${{name}}', …] on your plugin, so cordis parks this dynamic package if the provider later goes away.`);
    }}
    throw new Error(`sandbox ctx does not expose "${{name}}". Available: ctx.tools.register / ctx.on / ctx.provide / the timer helpers after injecting timer, and any service you declared in inject. Framework internals (root, fiber, registry, extend, plugin, …) are withheld by design.`);
  }};
  const ctx = new Proxy(target, {{
    get: (object, name) => {{
      if (typeof name === 'symbol') return undefined;
      if (name in object) return object[name];
      if (sandboxed) {{
        if (verbs.has(name)) {{
          if (timerVerbs.has(name) && !declared.has('timer')) return denyRead('timer');
          return () => {{ throw new Error(`sandbox ctx verb "${{name}}" is unavailable`); }};
        }}
        if (!declared.has(name)) return denyRead(name);
      }}
      return services[name];
    }},
    set: (_object, name) => {{ throw new TypeError(`sandbox ctx is read-only; cannot assign "${{String(name)}}"`); }},
    has: (object, name) => typeof name === 'string' && (name in object
      || !sandboxed && Object.prototype.hasOwnProperty.call(services, name)
      || sandboxed && ((verbs.has(name) && (!timerVerbs.has(name) || declared.has('timer'))) || declared.has(name))),
  }});
  if (!sandboxed) target.root = ctx;
  globalThis.__seekdeep_ctx__ = ctx;
}})();
"#
    );
    javascript
        .eval(Source::from_bytes(&setup))
        .map_err(|error| error.to_string())?;
    javascript
        .global_object()
        .get(js_string!("__seekdeep_ctx__"), javascript)
        .map_err(|error| error.to_string())
}

fn invoke_callback(
    javascript: &mut JavaScriptContext,
    activation: Option<&mut ActivationEffects>,
    callback_id: usize,
) -> Result<Vec<HostCommand>, String> {
    let activation =
        activation.ok_or_else(|| "dynamic Host event callback is no longer active".to_owned())?;
    let _active_services = ActiveDynamicServices::enter(&activation.dynamic_services);
    let callback = activation
        .callbacks
        .get(PropertyKey::from(callback_id as u64), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| "dynamic Host event callback is no longer active".to_owned())?;
    let returned = callback
        .call(&JsValue::undefined(), &[], javascript)
        .map_err(|error| error.to_string())?;
    settle_returned(&returned, javascript)?;
    read_commands(javascript, activation)
}

fn dynamic_tool(javascript: &mut JavaScriptContext, tool_id: usize) -> Result<JsObject, String> {
    let tools = javascript
        .global_object()
        .get(js_string!("__seekdeep_dynamic_tools__"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| "dynamic Tool registry is unavailable".to_owned())?;
    tools
        .get(PropertyKey::from(tool_id as u64), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| format!("dynamic Tool {tool_id} is unavailable"))
}

fn clone_sandbox_value(
    javascript: &mut JavaScriptContext,
    value: JsValue,
    path: &str,
) -> Result<JsValue, String> {
    let clone = javascript
        .global_object()
        .get(js_string!("__seekdeep_clone_json__"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| "sandbox JSON cloner is unavailable".to_owned())?;
    clone
        .call(
            &JsValue::undefined(),
            &[value, JsValue::from(boa_engine::JsString::from(path))],
            javascript,
        )
        .map_err(|error| error.to_string())
}

fn invoke_service(
    javascript: &mut JavaScriptContext,
    service_id: usize,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    let services = javascript
        .global_object()
        .get(js_string!("__seekdeep_provided_services__"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| "dynamic Service registry is unavailable".to_owned())?;
    let service = services
        .get(PropertyKey::from(service_id as u64), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| format!("dynamic Service {service_id} is unavailable"))?;
    let function = service
        .get(boa_engine::JsString::from(method), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| format!("dynamic Service registered no method \"{method}\""))?;
    let args = args
        .as_array()
        .ok_or_else(|| "dynamic Service arguments must be an array".to_owned())?
        .iter()
        .map(|value| JsValue::from_json(value, javascript).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let returned = function
        .call(&service.clone().into(), &args, javascript)
        .map_err(|error| error.to_string())?;
    let settled = settle_value(&returned, javascript)?;
    let settled = clone_sandbox_value(
        javascript,
        settled,
        &format!("dynamic Service {service_id}.{method} result"),
    )?;
    let value = settled
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("dynamic Service {service_id}.{method} returned non-JSON data"))?;
    if !is_lossless_json(&value) {
        return Err(format!(
            "dynamic Service {service_id}.{method} returned non-JSON data"
        ));
    }
    Ok(value)
}

fn invoke_tool_execute(
    javascript: &mut JavaScriptContext,
    activation: Option<&mut ActivationEffects>,
    tool_id: usize,
    args: &Value,
) -> Result<(Value, Vec<HostCommand>), String> {
    let tool = dynamic_tool(javascript, tool_id)?;
    let execute = tool
        .get(js_string!("execute"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| format!("dynamic Tool {tool_id} has no execute function"))?;
    let args = JsValue::from_json(args, javascript).map_err(|error| error.to_string())?;
    let returned = execute
        .call(&tool.clone().into(), &[args], javascript)
        .map_err(|error| error.to_string())?;
    let settled = settle_value(&returned, javascript)?;
    let settled = clone_sandbox_value(javascript, settled, "harness.defineTool execute result")?;
    let value = settled
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "dynamic Tool execute result must be lossless JSON".to_owned())?;
    if !is_lossless_json(&value) {
        return Err("dynamic Tool execute result must be lossless JSON".to_owned());
    }
    let activation =
        activation.ok_or_else(|| "dynamic Tool activation is no longer active".to_owned())?;
    Ok((value, read_commands(javascript, activation)?))
}

fn invoke_tool_render(
    javascript: &mut JavaScriptContext,
    tool_id: usize,
    args: &Value,
    value: &Value,
) -> Result<Value, String> {
    let tool = dynamic_tool(javascript, tool_id)?;
    let output = tool
        .get(js_string!("output"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| format!("dynamic Tool {tool_id} has no output object"))?;
    let render = output
        .get(js_string!("render"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| format!("dynamic Tool {tool_id} has no output.render function"))?;
    let args = JsValue::from_json(args, javascript).map_err(|error| error.to_string())?;
    let value = JsValue::from_json(value, javascript).map_err(|error| error.to_string())?;
    let returned = render
        .call(&output.into(), &[args, value], javascript)
        .map_err(|error| error.to_string())?;
    let settled = settle_value(&returned, javascript)?;
    let settled = clone_sandbox_value(
        javascript,
        settled,
        "harness.defineTool output.render result",
    )?;
    settled
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "dynamic Tool output.render result must be lossless JSON".to_owned())
}

fn invoke_tool_presentation(
    javascript: &mut JavaScriptContext,
    tool_id: usize,
    args: &Value,
    value: &Value,
) -> Result<Value, String> {
    let tool = dynamic_tool(javascript, tool_id)?;
    let output = tool
        .get(js_string!("output"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| format!("dynamic Tool {tool_id} has no output object"))?;
    let projector = output
        .get(js_string!("presentationMeta"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| format!("dynamic Tool {tool_id} has no output.presentationMeta function"))?;
    let args = JsValue::from_json(args, javascript).map_err(|error| error.to_string())?;
    let value = JsValue::from_json(value, javascript).map_err(|error| error.to_string())?;
    let returned = projector
        .call(&output.into(), &[args, value], javascript)
        .map_err(|error| error.to_string())?;
    let settled = settle_value(&returned, javascript)?;
    let settled = clone_sandbox_value(
        javascript,
        settled,
        "harness.defineTool output.presentationMeta result",
    )?;
    settled
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "dynamic Tool output.presentationMeta result must be lossless JSON".to_owned()
        })
}

fn invoke_handler(
    javascript: &mut JavaScriptContext,
    activation: Option<&mut ActivationEffects>,
    method: &str,
    args: &Value,
) -> Result<(Value, Vec<HostCommand>), String> {
    let handlers = javascript
        .global_object()
        .get(js_string!("__seekdeep_handlers__"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| "dynamic Host handler registry is unavailable".to_owned())?;
    let entry = handlers
        .get(boa_engine::JsString::from(method), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .ok_or_else(|| format!("dynamic Host registered no method \"{method}\""))?;
    let handler = entry
        .get(js_string!("fn"), javascript)
        .map_err(|error| error.to_string())?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| format!("dynamic Host registered no method \"{method}\""))?;
    let args = JsValue::from_json(args, javascript).map_err(|error| error.to_string())?;
    let returned = handler
        .call(&JsValue::undefined(), &[args], javascript)
        .map_err(|error| error.to_string())?;
    let settled = settle_value(&returned, javascript)?;
    let value = settled
        .to_json(javascript)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("harness.handle(\"{method}\") result must be lossless JSON data"))?;
    if !is_lossless_json(&value) {
        return Err(format!(
            "harness.handle(\"{method}\") result must be lossless JSON data"
        ));
    }
    let commands = activation.map_or(Ok(Vec::new()), |activation| {
        read_commands(javascript, activation)
    })?;
    Ok((value, commands))
}

fn settle_returned(value: &JsValue, javascript: &mut JavaScriptContext) -> Result<(), String> {
    settle_value(value, javascript).map(|_| ())
}

fn settle_value(value: &JsValue, javascript: &mut JavaScriptContext) -> Result<JsValue, String> {
    let Some(object) = value.as_object() else {
        return Ok(value.clone());
    };
    let Ok(promise) = boa_engine::object::builtins::JsPromise::from_object(object) else {
        return Ok(value.clone());
    };
    javascript.run_jobs().map_err(|error| error.to_string())?;
    match promise.state() {
        PromiseState::Fulfilled(value) => Ok(value),
        PromiseState::Rejected(error) => Err(render_value(&error, javascript)),
        PromiseState::Pending => Err("JavaScript plugin apply did not settle".to_owned()),
    }
}

fn is_lossless_json(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number)
                if number
                    .as_f64()
                    .is_some_and(|value| value == 0.0 && value.is_sign_negative()) =>
            {
                return false;
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    true
}

fn collect_functions(
    array: &JsObject,
    javascript: &mut JavaScriptContext,
) -> Result<Vec<JsObject>, String> {
    let length = array
        .get(js_string!("length"), javascript)
        .map_err(|error| error.to_string())?
        .to_length(javascript)
        .map_err(|error| error.to_string())?;
    let mut functions = Vec::new();
    for index in 0..length {
        let value = array
            .get(PropertyKey::from(index), javascript)
            .map_err(|error| error.to_string())?;
        if let Some(function) = value.as_object().filter(JsObject::is_callable) {
            functions.push(function.clone());
        }
    }
    Ok(functions)
}

fn deactivate(
    javascript: &mut JavaScriptContext,
    mut disposers: Vec<JsObject>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    while let Some(disposer) = disposers.pop() {
        match disposer.call(&JsValue::undefined(), &[], javascript) {
            Ok(value) => {
                if let Err(error) = settle_returned(&value, javascript) {
                    failures.push(error);
                }
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn cleanup_activation_globals(javascript: &mut JavaScriptContext) {
    let _ = javascript.eval(Source::from_bytes(
        "delete globalThis.__seekdeep_ctx__; delete globalThis.__seekdeep_commands__; delete globalThis.__seekdeep_disposers__; delete globalThis.__seekdeep_callbacks__;",
    ));
}

fn render_value(value: &JsValue, javascript: &mut JavaScriptContext) -> String {
    value.to_string(javascript).map_or_else(
        |_| "JavaScript exception".to_owned(),
        |value| value.to_std_string_escaped(),
    )
}
