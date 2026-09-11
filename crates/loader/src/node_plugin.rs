//! Native lifecycle owner for file plugins sharing the compiled Node boundary.

use std::{
    collections::{BTreeSet, HashMap},
    io::{BufRead as _, BufReader, Write as _},
    net::{Shutdown, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use futures::{StreamExt as _, stream::FuturesUnordered};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use serde_json::{Value, json};

use crate::{LoaderError, javascript_plugin::LoadedPlugin};

type Reply = Result<Value, Value>;
type Pending = Arc<Mutex<HashMap<u64, Response>>>;
type Routes = Arc<Mutex<HashMap<u64, tokio::sync::mpsc::UnboundedSender<Value>>>>;
type ValueIdentities = HashMap<String, Vec<(std::sync::Weak<Value>, u64)>>;

struct CommandExecutor {
    handle: tokio::runtime::Handle,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl CommandExecutor {
    fn new() -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = thread::Builder::new()
            .name("seekdeep-plugin-effects".to_owned())
            .spawn(move || {
                runtime.block_on(async {
                    let _ = stopped.await;
                });
            })?;
        Ok(Self {
            handle,
            stop: Mutex::new(Some(stop)),
            thread: Mutex::new(Some(thread)),
        })
    }
}

impl Drop for CommandExecutor {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.get_mut().take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.get_mut().take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

enum Response {
    Sync(mpsc::SyncSender<Reply>),
    Async(tokio::sync::oneshot::Sender<Reply>),
}

impl Response {
    fn send(self, reply: Reply) {
        match self {
            Self::Sync(sender) => {
                let _ = sender.send(reply);
            }
            Self::Async(sender) => {
                let _ = sender.send(reply);
            }
        }
    }
}

pub(crate) struct NodeRealm {
    writer: Mutex<TcpStream>,
    child: Arc<Mutex<Child>>,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
    pending: Pending,
    routes: Routes,
    next_request: AtomicU64,
    next_activation: AtomicU64,
    pub(super) next_watch: AtomicU64,
    pub(super) watches: crate::node_watch::WatchRoutes,
    executor: CommandExecutor,
    value_identities: Mutex<ValueIdentities>,
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for NodeRealm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodePluginRealm")
            .field("process", &self.child.lock().id())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct NodeService {
    realm: Arc<NodeRealm>,
    service: u64,
}

struct ModuleLease {
    realm: Arc<NodeRealm>,
    module: u64,
}

impl Drop for ModuleLease {
    fn drop(&mut self) {
        let _ = self
            .realm
            .write(&json!({"action":"release","module":self.module}));
    }
}

pub(crate) fn load_error(error: Value) -> LoaderError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map_or_else(|| error.to_string(), str::to_owned);
    LoaderError::StructuredModuleLoad { message, error }
}

fn event_arguments(args: &EventArgs, event: &str) -> Vec<Value> {
    (0..args.len()).map(|index| {
        if args.get::<crate::HostHmrReload>(index).is_some() { return json!({"kind":"hmrReload"}); }
        if let Some(path) = args.get::<std::path::PathBuf>(index) {
            let value = if event == "hmr/change" { url::Url::from_file_path(path.as_path()).map_or_else(|()| path.to_string_lossy().into_owned(), |url| url.to_string()) } else { path.to_string_lossy().into_owned() };
            return json!({"kind":"json","value":value});
        }
        if let Some(value) = args.get::<String>(index) { return json!({"kind":if event == "hmr/config-update-failed" && index == 1 { "error" } else { "json" },"value":*value}); }
        json!({"kind":"json","value":args.get::<Value>(index).map_or(Value::Null, |value| (*value).clone())})
    }).collect()
}

impl NodeRealm {
    pub(crate) fn start() -> Result<Arc<Self>, LoaderError> {
        Self::spawn()
            .map_err(|error| LoaderError::ModuleLoad(format!("Node plugin realm: {error:#}")))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the process owner admits the child, transport, readiness, and reader together"
    )]
    fn spawn() -> anyhow::Result<Arc<Self>> {
        let assets = seekdeep_code_runtime_worker_thread::node_runtime_assets()?;
        let bootstrap = assets.join("plugin-loader.cjs");
        anyhow::ensure!(
            bootstrap.is_file(),
            "compiled Node plugin loader is missing: {}",
            bootstrap.display()
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = json!({"host":"127.0.0.1","port":listener.local_addr()?.port()});
        let mut child =
            Command::new(seekdeep_code_runtime_worker_thread::node_runtime_executable(&assets)?)
                .arg("--expose-internals")
                .arg(&bootstrap)
                .env(
                    "SEEKDEEP_PLUGIN_NODE_SOCKET",
                    serde_json::to_string(&address)?,
                )
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()?;
        let started = std::time::Instant::now();
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.try_wait()? {
                        anyhow::bail!("Node plugin realm exited during bootstrap: {status}");
                    }
                    if started.elapsed() > std::time::Duration::from_secs(20) {
                        let _ = child.kill();
                        let _ = child.wait();
                        anyhow::bail!("Node plugin realm did not connect");
                    }
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
        };
        // Accepted sockets inherit O_NONBLOCK from the polling listener on macOS.
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        let mut input = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        input.read_line(&mut line)?;
        anyhow::ensure!(
            serde_json::from_str::<Value>(&line)?["ready"] == true,
            "Node plugin realm did not become ready"
        );
        let pending: Pending = Arc::default();
        let routes: Routes = Arc::default();
        let watches: crate::node_watch::WatchRoutes = Arc::default();
        let pending_reader = pending.clone();
        let routes_reader = routes.clone();
        let watches_reader = watches.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let reader_closed = closed.clone();
        let child = Arc::new(Mutex::new(child));
        let reader_child = child.clone();
        let reader = thread::Builder::new()
            .name("seekdeep-plugin-node".to_owned())
            .spawn(move || {
                loop {
                    line.clear();
                    match input.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if let Some(id) = message["id"].as_u64() {
                        if let Some(reply) = pending_reader.lock().remove(&id) {
                            reply.send(
                                message
                                    .get("error")
                                    .cloned()
                                    .map_or_else(|| Ok(message["result"].clone()), Err),
                            );
                        }
                    } else if message.get("watch").is_some() {
                        crate::node_watch::route(&watches_reader, &message);
                    } else if let Some(id) = message["activation"].as_u64()
                        && let Some(sender) = routes_reader.lock().get(&id)
                    {
                        let _ = sender.send(message);
                    }
                }
                reader_closed.store(true, Ordering::Release);
                for (_, reply) in pending_reader.lock().drain() {
                    reply.send(Err(json!({"message":"Node plugin realm closed"})));
                }
                crate::node_watch::closed(&watches_reader);
                for (_, sender) in routes_reader.lock().drain() {
                    let _ = sender.send(json!({"type":"realmClosed"}));
                }
                let _ = input.get_mut().shutdown(Shutdown::Both);
                let mut child = reader_child.lock();
                let _ = child.kill();
                let _ = child.wait();
            })?;
        Ok(Arc::new(Self {
            writer: Mutex::new(stream),
            child,
            reader: Mutex::new(Some(reader)),
            pending,
            routes,
            next_request: AtomicU64::new(1),
            next_activation: AtomicU64::new(1),
            next_watch: AtomicU64::new(1),
            watches,
            executor: CommandExecutor::new()?,
            value_identities: Mutex::default(),
            closed,
        }))
    }

    pub(super) fn write(&self, message: &Value) -> Result<(), LoaderError> {
        let mut bytes = serde_json::to_vec(message)
            .map_err(|error| LoaderError::ModuleLoad(error.to_string()))?;
        bytes.push(b'\n');
        self.writer.lock().write_all(&bytes).map_err(|error| {
            LoaderError::ModuleLoad(format!("Node plugin realm transport: {error}"))
        })
    }

    fn send(&self, mut message: Value, response: Response) -> Result<(), LoaderError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(LoaderError::ModuleLoad(
                "Node plugin realm closed".to_owned(),
            ));
        }
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        message["id"] = json!(id);
        self.pending.lock().insert(id, response);
        if self.closed.load(Ordering::Acquire) {
            self.pending.lock().remove(&id);
            return Err(LoaderError::ModuleLoad(
                "Node plugin realm closed".to_owned(),
            ));
        }
        if let Err(error) = self.write(&message) {
            self.pending.lock().remove(&id);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn request(&self, message: Value) -> Result<Value, LoaderError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.send(message, Response::Sync(sender))?;
        receiver
            .recv()
            .map_err(|_| LoaderError::Unavailable)?
            .map_err(load_error)
    }

    pub(super) async fn request_async(&self, message: Value) -> Result<Value, LoaderError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.send(message, Response::Async(sender))?;
        receiver
            .await
            .map_err(|_| LoaderError::Unavailable)?
            .map_err(load_error)
    }

    pub(crate) fn load(self: &Arc<Self>, path: &Path) -> Result<LoadedPlugin, LoaderError> {
        self.loaded(&self.request(json!({"action":"import","path":path}))?)
    }

    pub(crate) fn resolve(
        &self,
        specifier: &str,
        base: &str,
    ) -> Result<std::path::PathBuf, LoaderError> {
        let path = self.request(json!({"action":"resolve","specifier":specifier,"base":base}))?;
        path.as_str().map(std::path::PathBuf::from).ok_or_else(|| {
            LoaderError::ModuleLoad("Node module resolution returned no file path".to_owned())
        })
    }

    fn loaded(self: &Arc<Self>, metadata: &Value) -> Result<LoadedPlugin, LoaderError> {
        let module = metadata["module"]
            .as_u64()
            .ok_or_else(|| LoaderError::ModuleLoad("Node module identity is missing".to_owned()))?;
        let name = metadata["name"]
            .as_str()
            .unwrap_or("JavaScript plugin")
            .to_owned();
        let inject = metadata["inject"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let dependencies = metadata["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(std::path::PathBuf::from)
            .collect();
        let lease = Arc::new(ModuleLease {
            realm: self.clone(),
            module,
        });
        let declared = inject.clone();
        let plugin = Plugin::new(name, inject, move |context, config| {
            let lease = lease.clone();
            let declared = declared.clone();
            Box::pin(async move {
                lease
                    .realm
                    .activate(&context, lease.module, &declared, config)
                    .await
            })
        });
        Ok(LoadedPlugin {
            plugin,
            dependencies,
        })
    }

    pub(crate) fn prepare(
        self: &Arc<Self>,
        paths: &[std::path::PathBuf],
        roots: &[String],
        externals: &BTreeSet<std::path::PathBuf>,
        fibers: &Value,
    ) -> Result<Vec<(String, LoadedPlugin)>, LoaderError> {
        let candidates = self
            .request(json!({"action":"begin","paths":paths,"roots":roots,"externals":externals,"fibers":fibers}))
            .map_err(|failure| match failure {
                LoaderError::StructuredModuleLoad { message, error } => {
                    LoaderError::StructuredModuleLoad {
                        message: format!(
                            "{}: {message}",
                            paths
                                .iter()
                                .map(|path| path.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        error,
                    }
                }
                failure => failure,
            })?;
        candidates
            .as_array()
            .ok_or_else(|| {
                LoaderError::ModuleLoad("invalid Node HMR candidate response".to_owned())
            })?
            .iter()
            .map(|metadata| {
                Ok((
                    metadata["path"].as_str().unwrap_or_default().to_owned(),
                    self.loaded(metadata)?,
                ))
            })
            .collect()
    }

    pub(crate) fn finish(&self, rollback: bool) -> Result<(), LoaderError> {
        self.request(json!({"action":if rollback { "rollback" } else { "commit" }}))?;
        Ok(())
    }

    pub(crate) fn contains(&self, path: &Path) -> Result<bool, LoaderError> {
        Ok(self.request(json!({"action":"contains","path":path}))? == true)
    }

    async fn activate(
        self: &Arc<Self>,
        context: &Context,
        module: u64,
        declared: &[String],
        config: Value,
    ) -> anyhow::Result<()> {
        let id = self.next_activation.fetch_add(1, Ordering::Relaxed);
        let services = context.expression_service_snapshot();
        let mut dynamic = services
            .keys()
            .chain(declared.iter())
            .filter_map(|name| {
                context
                    .get_named::<NodeService>(name)
                    .filter(|service| Arc::ptr_eq(&service.realm, self))
                    .map(|service| (name.clone(), json!({"service":service.service})))
            })
            .collect::<serde_json::Map<_, _>>();
        for name in services.keys() {
            if let Some(value) = context.get_named::<Value>(name)
                && let Some(service) =
                    self.value_identities
                        .lock()
                        .get(name)
                        .and_then(|identities| {
                            identities.iter().find_map(|(owner, id)| {
                                owner
                                    .upgrade()
                                    .filter(|owner| Arc::ptr_eq(owner, &value))
                                    .map(|_| *id)
                            })
                        })
            {
                dynamic.insert(name.clone(), json!({"service":service}));
            }
        }
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Value>();
        self.routes.lock().insert(id, sender);
        let owner = context.clone();
        let weak = Arc::downgrade(self);
        let effects = Arc::new(Mutex::new(HashMap::<u64, EffectHandle>::new()));
        let task = self.executor.handle.spawn(async move {
            let mut running = FuturesUnordered::new();
            loop {
                tokio::select! {
                    message = receiver.recv() => {
                        let Some(message) = message else { break; };
                        let Some(realm) = weak.upgrade() else { break; };
                        let owner = owner.clone();
                        let effects = effects.clone();
                        running.push(async move {
                            let result = realm.command(&owner, id, &effects, &message).await;
                            let reply = match result { Ok(value) => json!({"call":message["call"],"result":value}), Err(error) => json!({"call":message["call"],"error":format!("{error:#}")}) };
                            let _ = realm.write(&reply);
                        });
                    }
                    _ = running.next(), if !running.is_empty() => {}
                }
            }
            while running.next().await.is_some() {}
        });
        let cleanup = self.clone();
        let task = Arc::new(Mutex::new(Some(task)));
        context.own(EffectHandle::new(
            "Node JavaScript plugin effects",
            move || {
                let cleanup = cleanup.clone();
                let task = task.clone();
                Box::pin(async move {
                    let result = cleanup
                        .request_async(json!({"action":"deactivate","activation":id}))
                        .await;
                    cleanup.routes.lock().remove(&id);
                    let task = task.lock().take();
                    if let Some(task) = task {
                        task.await?;
                    }
                    result?;
                    Ok(())
                })
            },
        ))?;
        let tools = context.get(seekdeep_tools::TOOLS);
        let tool_schemas = tools.as_ref().map(|tools| tools.schemas(None));
        self.request_async(json!({"action":"activate","module":module,"activation":id,"nativeFiber":context.fiber().id().to_string(),"services":services,"dynamic":dynamic,"config":config,"baseUrl":context.meta("loader.base_url"),"entry":{"id":context.meta("loader.entry_id"),"options":{"name":context.meta("loader.entry_name"),"config":config}},"hasTools":tools.is_some(),"toolSchemas":tool_schemas})).await?;
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive dispatcher owns the native effect and its reply"
    )]
    async fn command(
        self: &Arc<Self>,
        context: &Context,
        activation: u64,
        effects: &Mutex<HashMap<u64, EffectHandle>>,
        command: &Value,
    ) -> anyhow::Result<Value> {
        let name = command["name"].as_str().unwrap_or_default();
        let id = command["effect"].as_u64().unwrap_or_default();
        let effect = match command["type"].as_str().unwrap_or_default() {
            "realmClosed" => {
                let fiber = context
                    .registry()
                    .values()
                    .into_iter()
                    .flat_map(|runtime| runtime.fibers)
                    .find(|fiber| Arc::ptr_eq(fiber.fiber(), context.fiber()));
                if let Some(fiber) = fiber {
                    self.executor.handle.spawn(async move {
                        let _ = fiber.dispose().await;
                    });
                }
                None
            }
            "registerTool" => Some(crate::node_tools::register(
                self, context, activation, command,
            )?),
            "provide" => {
                let value = Arc::new(command["value"].clone());
                let id = command["service"].as_u64().unwrap_or_default();
                {
                    let mut identities = self.value_identities.lock();
                    let identities = identities.entry(name.to_owned()).or_default();
                    identities.retain(|(value, _)| value.strong_count() > 0);
                    identities.push((Arc::downgrade(&value), id));
                }
                Some(context.provide_named(name, value)?)
            }
            "provideDynamic" => {
                let service = NodeService {
                    realm: self.clone(),
                    service: command["service"].as_u64().unwrap_or_default(),
                };
                Some(context.provide_named_projected(
                    name,
                    Arc::new(service),
                    command["projection"].clone(),
                )?)
            }
            "on" => {
                let realm = self.clone();
                let once = command["once"] == true;
                let fired = Arc::new(AtomicBool::new(false));
                let event = name.to_owned();
                Some(context.events().on(context, name, move |_, args| {
                    let realm = realm.clone();
                    let fired = fired.clone();
                    let event = event.clone();
                    Box::pin(async move {
                        if once && fired.swap(true, Ordering::AcqRel) { return Ok(EventReply::Undefined); }
                        let args = event_arguments(&args, &event);
                        let value = realm.request_async(json!({"action":"invoke","activation":activation,"callback":id,"args":args,"argumentKinds":true})).await?;
                        Ok(match value { Value::Null => EventReply::Undefined, Value::Bool(false) => EventReply::False, value => EventReply::Value(Arc::new(value)) })
                    })
                }, EventOptions::default())?)
            }
            "disposeEffect" => {
                let effect = effects.lock().remove(&id);
                if let Some(effect) = effect {
                    effect.dispose().await?;
                }
                None
            }
            "disposeRoot" => {
                let root = context.root_fiber().clone();
                tokio::spawn(async move {
                    let _ = root.dispose().await;
                });
                None
            }
            "event" => {
                let args = EventArgs::from_values(
                    command["args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|value| Arc::new(value.clone()) as seekdeep_cordis::events::EventValue)
                        .collect(),
                );
                match command["mode"].as_str().unwrap_or_default() {
                    "emit" => context.events().emit(context, name, &args)?,
                    "parallel" => context.events().parallel(context, name, &args).await?,
                    _ => {
                        let value = context.events().serial(context, name, &args).await?;
                        return Ok(value
                            .downcast::<Value>()
                            .map_or(Value::Null, |value| (*value).clone()));
                    }
                }
                None
            }
            "log" => {
                let args = command["args"].as_array().cloned().unwrap_or_default();
                let logger = context.logger(None);
                match command["level"].as_str().unwrap_or_default() {
                    "debug" => {
                        logger.debug(args);
                    }
                    "warn" => {
                        logger.warn(args);
                    }
                    "error" => {
                        logger.error(args);
                    }
                    _ => {
                        logger.info(args);
                    }
                }
                None
            }
            kind => anyhow::bail!("unknown Node plugin effect {kind}"),
        };
        if let Some(effect) = effect {
            effects.lock().insert(id, effect);
        }
        Ok(Value::Null)
    }
}

impl Drop for NodeRealm {
    fn drop(&mut self) {
        let _ = self.writer.get_mut().shutdown(Shutdown::Both);
        let mut child = self.child.lock();
        let _ = child.kill();
        let _ = child.wait();
        drop(child);
        if let Some(reader) = self.reader.get_mut().take() {
            let _ = reader.join();
        }
    }
}
