//! Native ownership of the compiled Rust boundary running in Node workers.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use seekdeep_code_runtime::{
    CodeBindingErrorClass, CodeBindingFailure, CodeBindingNamespace, CodeJsonString, CodeJsonValue,
    CodeRunFailureKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    sync::{mpsc, watch},
};

use crate::{
    CodeJsonToken,
    outcome::{EngineCompletion, EngineLimits, EngineOutcome},
    output_ledger::OutputLedger,
    worker_json::{decode_code_json, encode_code_json},
};

static NEXT_ENDPOINT: AtomicU64 = AtomicU64::new(1);

/// Keeps process I/O alive independently of the executor polling a caller's run future.
#[derive(Debug)]
pub(crate) struct NodeExecutor {
    pub(crate) handle: tokio::runtime::Handle,
    stopped: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    thread: parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl NodeExecutor {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("seekdeep-code-runtime-node".to_owned())
            .spawn(move || {
                runtime.block_on(async {
                    let _ = stopped.await;
                });
            })?;
        Ok(Self {
            handle,
            stopped: parking_lot::Mutex::new(Some(stop)),
            thread: parking_lot::Mutex::new(Some(thread)),
        })
    }
}

impl Drop for NodeExecutor {
    fn drop(&mut self) {
        if let Some(stop) = self.stopped.get_mut().take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.get_mut().take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct NodeEvent {
    run: u64,
    #[serde(rename = "type")]
    kind: String,
    text: Option<CodeJsonString>,
    #[serde(rename = "callId")]
    call_id: Option<String>,
    global: Option<CodeJsonString>,
    name: Option<CodeJsonString>,
    #[serde(
        default,
        deserialize_with = "seekdeep_code_runtime::json::deserialize_optional"
    )]
    args: Option<CodeJsonValue>,
    completion: Option<NodeCompletion>,
}

#[derive(Debug, Default, Deserialize)]
struct NodeCompletion {
    kind: String,
    #[serde(
        default,
        deserialize_with = "seekdeep_code_runtime::json::deserialize_optional"
    )]
    value: Option<CodeJsonValue>,
    #[serde(rename = "failureKind")]
    failure_kind: Option<String>,
    message: Option<CodeJsonString>,
    code: Option<f64>,
    reason: Option<Value>,
}

#[derive(Serialize)]
struct BindingReply {
    #[serde(rename = "type")]
    kind: &'static str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<CodeJsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<CodeJsonString>,
}

fn binding_value_is_lossless(value: &CodeJsonValue) -> bool {
    value.tokens().all(|token| {
        let CodeJsonToken::Scalar(value) = token else {
            return true;
        };
        if matches!(value.as_raw().as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
            value.as_raw().parse::<f64>().is_ok_and(|number| {
                number.is_finite() && !(number == 0.0 && number.is_sign_negative())
            })
        } else {
            true
        }
    })
}

fn binding_reply(result: anyhow::Result<CodeJsonValue>) -> BindingReply {
    match result {
        Ok(value) if binding_value_is_lossless(&value) => BindingReply {
            kind: "reply",
            ok: true,
            value: Some(encode_code_json(&value)),
            message: None,
        },
        Ok(_) => BindingReply {
            kind: "reply",
            ok: false,
            value: None,
            message: Some("binding resolution must be lossless JSON".into()),
        },
        Err(error) => BindingReply {
            kind: "reply",
            ok: false,
            value: None,
            message: Some(
                error
                    .downcast_ref::<CodeBindingFailure>()
                    .map_or_else(|| error.to_string().into(), |error| error.message.clone()),
            ),
        },
    }
}

#[derive(Serialize)]
struct NodeReply {
    #[serde(rename = "type")]
    kind: &'static str,
    run: u64,
    #[serde(rename = "callId")]
    call_id: String,
    message: BindingReply,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingDeclaration<'a> {
    global: &'a str,
    names: Vec<&'a CodeJsonString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_class: Option<&'a CodeBindingErrorClass>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerBoot<'a> {
    code: &'a str,
    namespaces: Vec<BindingDeclaration<'a>>,
    max_output_bytes: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerLimits {
    compute_ms: f64,
    max_wall_ms: f64,
    max_old_generation_size_mb: f64,
}

#[derive(Serialize)]
struct StartWorker<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    run: u64,
    boot: WorkerBoot<'a>,
    limits: WorkerLimits,
}

type Routes = Arc<parking_lot::Mutex<HashMap<u64, mpsc::UnboundedSender<NodeEvent>>>>;

/// One compatibility process owns every real Node worker for this backend.
pub(crate) struct NodeSupervisor {
    input: mpsc::UnboundedSender<String>,
    routes: Routes,
    closed: Arc<AtomicBool>,
    stop: watch::Sender<bool>,
    joined: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub(crate) enum NodeStartup {
    Ready(Arc<NodeSupervisor>),
    Stopped(EngineCompletion),
}

impl std::fmt::Debug for NodeSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeSupervisor")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl Drop for NodeSupervisor {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Finds shipped runtime assets, with an explicit override for staged installations.
pub(crate) fn assets() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("SEEKDEEP_CODE_RUNTIME_NODE_DIR") {
        return validate_assets(Path::new(&path));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let node_platform = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        };
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            other => other,
        };
        let mut targets = vec![
            format!("{}-{architecture}", std::env::consts::OS),
            format!("{node_platform}-{architecture}"),
        ];
        targets.dedup();
        let roots = [
            "code-runtime-node",
            "../code-runtime-node",
            "lib/seekdeep/code-runtime-node",
            "../lib/seekdeep/code-runtime-node",
        ];
        let candidates = targets
            .iter()
            .flat_map(|target| {
                roots
                    .iter()
                    .map(move |relative| directory.join(relative).join(target))
            })
            .chain(roots.iter().map(|relative| directory.join(relative)));
        for candidate in candidates {
            if candidate.join("loader.mjs").is_file() {
                return validate_assets(&candidate);
            }
        }
    }
    anyhow::bail!(
        "Node code-runtime assets are not installed next to this executable; install lib/seekdeep/code-runtime-node or set SEEKDEEP_CODE_RUNTIME_NODE_DIR to the packaged runtime directory"
    )
}

fn validate_assets(path: &Path) -> anyhow::Result<PathBuf> {
    for name in [
        "loader.mjs",
        "wasm-runtime.cjs",
        "seekdeep_code_runtime_node.js",
        "seekdeep_code_runtime_node_bg.wasm",
    ] {
        anyhow::ensure!(
            path.join(name).is_file(),
            "Node code-runtime asset {} is missing; build and install the compiled Rust Node boundary",
            path.join(name).display()
        );
    }
    let path = path.canonicalize()?;
    crate::node_assets::verify_manifest(&path)?;
    if crate::node_assets::bundled_node_required(&path)? {
        executable(&path)?;
    }
    Ok(path)
}

pub(crate) fn executable(directory: &Path) -> anyhow::Result<PathBuf> {
    if let Some(executable) = std::env::var_os("SEEKDEEP_NODE_BINARY") {
        return Ok(executable.into());
    }
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    for executable in [directory.join("bin").join(name), directory.join(name)] {
        if executable.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                anyhow::ensure!(
                    std::fs::metadata(&executable)?.permissions().mode() & 0o111 != 0,
                    "packaged Node binary is not executable: {}",
                    executable.display()
                );
            }
            return Ok(executable);
        }
    }
    anyhow::ensure!(
        !crate::node_assets::bundled_node_required(directory)?,
        "packaged Node executable is missing from {}",
        directory.display()
    );
    Ok(name.into())
}

struct Endpoint {
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
    #[cfg(windows)]
    listener: tokio::net::windows::named_pipe::NamedPipeServer,
    address: PathBuf,
    #[cfg(unix)]
    directory: PathBuf,
}

impl Endpoint {
    fn new() -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            let directory = loop {
                let sequence = NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed);
                let candidate =
                    std::env::temp_dir().join(format!("sdn-{}-{sequence}", std::process::id()));
                match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
                    Ok(()) => break candidate,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            };
            let address = directory.join("ipc");
            let listener = match tokio::net::UnixListener::bind(&address) {
                Ok(listener) => listener,
                Err(error) => {
                    let _ = std::fs::remove_dir(&directory);
                    return Err(error.into());
                }
            };
            Ok(Self {
                listener,
                address,
                directory,
            })
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let sequence = NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed);
            let address = PathBuf::from(format!(
                r"\\.\pipe\seekdeep-code-runtime-{}-{sequence}",
                std::process::id()
            ));
            let listener = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(&address)?;
            Ok(Self { listener, address })
        }
    }

    #[cfg(unix)]
    async fn accept(&self) -> anyhow::Result<tokio::net::UnixStream> {
        Ok(self.listener.accept().await?.0)
    }

    #[cfg(windows)]
    async fn accept(&mut self) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        self.listener.connect().await?;
        let replacement =
            tokio::net::windows::named_pipe::ServerOptions::new().create(&self.address)?;
        Ok(std::mem::replace(&mut self.listener, replacement))
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.address);
            let _ = std::fs::remove_dir(&self.directory);
        }
    }
}

impl NodeSupervisor {
    #[expect(
        clippy::too_many_lines,
        reason = "one owner coordinates process startup, I/O tasks, cancellation, and child teardown"
    )]
    pub(crate) async fn start(
        directory: &Path,
        limits: &EngineLimits,
    ) -> anyhow::Result<NodeStartup> {
        #[allow(unused_mut)]
        let mut endpoint = Endpoint::new()?;
        let executable = executable(directory)?;
        let mut child = tokio::process::Command::new(executable)
            .arg(directory.join("loader.mjs"))
            .arg(&endpoint.address)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut cancellation_poll = tokio::time::interval(Duration::from_millis(5));
        let stop = async {
            loop {
                cancellation_poll.tick().await;
                if limits.signal.is_aborted() {
                    return EngineCompletion::Abort(limits.signal.reason().unwrap_or(Value::Null));
                }
            }
        };
        let ready = async {
            let connection = tokio::select! {
                connection = endpoint.accept() => connection?,
                status = child.wait() => anyhow::bail!("Node compatibility process exited during startup: {}", status?),
            };
            let (read, write) = tokio::io::split(connection);
            let mut lines = BufReader::new(read).lines();
            let ready = tokio::select! {
                ready = lines.next_line() => ready?.ok_or_else(|| anyhow::anyhow!("Node compatibility process disconnected before readiness"))?,
                status = child.wait() => anyhow::bail!("Node compatibility process exited before readiness: {}", status?),
            };
            let ready: Value = serde_json::from_str(&ready)?;
            anyhow::ensure!(
                ready["type"] == "ready" && ready["protocol"] == 1,
                "incompatible Node code-runtime boundary"
            );
            Ok((lines, write))
        };
        let started = tokio::select! {
            biased;
            completion = stop => Ok(Err(completion)),
            result = ready => result.map(Ok),
            () = tokio::time::sleep(Duration::from_secs_f64(limits.max_wall_ms / 1_000.0)) => Ok(Err(EngineCompletion::WallTimeout)),
        };
        let (mut lines, mut write) = match started {
            Ok(Ok(connection)) => connection,
            Ok(Err(completion)) => {
                let _ = child.kill().await;
                return Ok(NodeStartup::Stopped(completion));
            }
            Err(error) => {
                let _ = child.kill().await;
                return Err(error);
            }
        };
        let (input, mut requests) = mpsc::unbounded_channel::<String>();
        let (stop, mut stopping) = watch::channel(false);
        let routes: Routes = Arc::new(parking_lot::Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let writing_stop = stop.clone();
        let writer = tokio::spawn(async move {
            loop {
                let request = tokio::select! {
                    biased;
                    _ = stopping.changed() => break,
                    request = requests.recv() => match request { Some(request) => request, None => break },
                };
                let mut encoded = request.into_bytes();
                encoded.push(b'\n');
                if write.write_all(&encoded).await.is_err() {
                    break;
                }
            }
            let _ = write.shutdown().await;
            let _ = writing_stop.send(true);
        });
        let reading_routes = routes.clone();
        let reading_stop = stop.clone();
        let reader = tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<NodeEvent>(&line) else {
                    break;
                };
                if let Some(sender) = reading_routes.lock().get(&message.run) {
                    let _ = sender.send(message);
                }
            }
            let _ = reading_stop.send(true);
        });
        let mut stopped = stop.subscribe();
        let exiting_routes = routes.clone();
        let exiting_closed = closed.clone();
        let exiting_stop = stop.clone();
        let joined = tokio::spawn(async move {
            let result = tokio::select! {
                result = child.wait() => result,
                _ = stopped.changed() => {
                    shutdown_child(&mut child).await
                }
            };
            exiting_closed.store(true, Ordering::Release);
            let _ = exiting_stop.send(true);
            let _ = writer.await;
            let _ = reader.await;
            let message = result.map_or_else(
                |error| error.to_string(),
                |status| format!("Node compatibility process exited: {status}"),
            );
            for (id, sender) in exiting_routes.lock().iter() {
                let _ = sender.send(NodeEvent {
                    run: *id,
                    kind: "terminal".to_owned(),
                    completion: Some(NodeCompletion {
                        kind: "worker-error".to_owned(),
                        message: Some(message.clone().into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                let _ = sender.send(NodeEvent {
                    run: *id,
                    kind: "stopped".to_owned(),
                    ..Default::default()
                });
            }
            drop(endpoint);
        });
        Ok(NodeStartup::Ready(Arc::new(Self {
            input,
            routes,
            closed,
            stop,
            joined: parking_lot::Mutex::new(Some(joined)),
        })))
    }

    pub(crate) async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let joined = self.joined.lock().take();
        if let Some(joined) = joined {
            let _ = joined.await;
        }
    }

    fn send(&self, message: impl Serialize) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.closed.load(Ordering::Acquire),
            "Node compatibility process is closed"
        );
        self.input
            .send(serde_json::to_string(&message)?)
            .map_err(|_| anyhow::anyhow!("Node compatibility input is closed"))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one event loop owns binding replies, cancellation, output admission, and terminal ordering"
    )]
    pub(crate) async fn run(
        &self,
        id: u64,
        program: &str,
        limits: EngineLimits,
        bindings: Vec<CodeBindingNamespace>,
    ) -> anyhow::Result<EngineOutcome> {
        let (events, mut receiver) = mpsc::unbounded_channel();
        self.routes.lock().insert(id, events);
        let _registration = RunRegistration {
            id,
            routes: self.routes.clone(),
        };
        let body = crate::typescript::stripped_body(program)?;
        let declarations = bindings
            .iter()
            .map(|namespace| {
                let mut indexed = Vec::new();
                let mut ordinary = Vec::new();
                for name in namespace.functions.keys() {
                    if let Some(index) = name.as_str().and_then(|text| {
                        text.parse::<u32>()
                            .ok()
                            .filter(|index| *index != u32::MAX && index.to_string() == text)
                    }) {
                        indexed.push((index, name));
                    } else {
                        ordinary.push(name);
                    }
                }
                indexed.sort_by_key(|(index, _)| *index);
                let names = indexed
                    .into_iter()
                    .map(|(_, name)| name)
                    .chain(ordinary)
                    .collect::<Vec<_>>();
                BindingDeclaration {
                    global: &namespace.global,
                    names,
                    error_class: namespace.error_class.as_ref(),
                }
            })
            .collect::<Vec<_>>();
        self.send(StartWorker {
            kind: "start",
            run: id,
            boot: WorkerBoot {
                code: body,
                namespaces: declarations,
                max_output_bytes: limits.max_output_bytes,
            },
            limits: WorkerLimits {
                compute_ms: limits.compute_ms,
                max_wall_ms: limits.max_wall_ms,
                max_old_generation_size_mb: limits.max_old_generation_size_mb,
            },
        })?;
        let mut logs = Vec::new();
        let mut stray_logs = Vec::new();
        let mut ledger = OutputLedger::new(limits.max_output_bytes);
        let mut completion = None;
        let mut output_limit = false;
        let mut abort_sent = false;
        let mut answered = HashSet::new();
        let mut pending =
            FuturesUnordered::<BoxFuture<'static, (String, anyhow::Result<CodeJsonValue>)>>::new();
        let mut cancellation_poll = tokio::time::interval(Duration::from_millis(5));
        loop {
            tokio::select! {
                biased;
                _ = cancellation_poll.tick(), if !abort_sent && completion.is_none() => {
                    if limits.signal.is_aborted() {
                        abort_sent = true;
                        self.send(json!({"type":"stop", "run":id, "completion":{"kind":"abort", "reason":limits.signal.reason().unwrap_or(Value::Null)}}))?;
                    }
                }
                reply = pending.next(), if !pending.is_empty() => {
                    if let Some((call_id, result)) = reply
                        && completion.is_none()
                    {
                        let message = binding_reply(result);
                        self.send(NodeReply { kind: "reply", run: id, call_id, message })?;
                    }
                }
                event = receiver.recv() => {
                    let Some(event) = event else { anyhow::bail!("Node worker event stream ended before teardown") };
                    match event.kind.as_str() {
                        kind @ ("log" | "pipe") => {
                            if output_limit || kind == "log" && completion.is_some() { continue }
                            let Some(text) = event.text.as_ref() else { continue };
                            let sink = if kind == "pipe" { &mut stray_logs } else { &mut logs };
                            if !ledger.admit(text, sink) {
                                sink.push(text.clone());
                                output_limit = true;
                                self.send(json!({"type":"stop", "run":id, "completion":{"kind":"output-limit"}}))?;
                            }
                        }
                        "call" if completion.is_none() => {
                            let Some(call_id) = event.call_id else { continue };
                            if !answered.insert(call_id.clone()) { continue }
                            let global = event.global.unwrap_or_else(|| "".into());
                            let name = event.name.unwrap_or_else(|| "".into());
                            let function = global.as_str().and_then(|global| bindings.iter().find(|namespace| namespace.global == global)).and_then(|namespace| namespace.functions.get(&name));
                            let argument = event.args.as_ref().and_then(decode_code_json);
                            let operation = match (function, argument) {
                                (None, _) => {
                                    let mut units = global.to_utf16();
                                    units.push(u16::from(b'.'));
                                    units.extend(name.to_utf16());
                                    let qualified = CodeJsonString::from_utf16(&units);
                                    let error = anyhow::anyhow!("unknown binding {}", qualified.as_raw());
                                    Box::pin(async move { Err(error) }) as BoxFuture<'static, anyhow::Result<CodeJsonValue>>
                                }
                                (Some(_), None) => Box::pin(async { Err(anyhow::anyhow!("binding arguments must be lossless JSON")) }),
                                (Some(function), Some(argument)) => function(argument),
                            };
                            pending.push(Box::pin(async move { (call_id, operation.await) }));
                        }
                        "terminal" if completion.is_none() => {
                            if let Some(event) = event.completion { completion = Some(parse_completion(event)?); }
                        }
                        "stopped" => {
                            let completion = if output_limit { EngineCompletion::OutputLimit } else { completion.unwrap_or_else(|| EngineCompletion::Exception("worker stopped without a completion".into())) };
                            logs.extend(stray_logs);
                            return Ok(EngineOutcome { logs, completion });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

async fn shutdown_child(
    child: &mut tokio::process::Child,
) -> std::io::Result<std::process::ExitStatus> {
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        result
    } else {
        let _ = child.start_kill();
        child.wait().await
    }
}

struct RunRegistration {
    id: u64,
    routes: Routes,
}

impl Drop for RunRegistration {
    fn drop(&mut self) {
        self.routes.lock().remove(&self.id);
    }
}

fn parse_completion(value: NodeCompletion) -> anyhow::Result<EngineCompletion> {
    Ok(match value.kind.as_str() {
        "success" => {
            if let Some(wire) = value.value {
                decode_code_json(&wire).map_or(EngineCompletion::InvalidOutput, |value| {
                    EngineCompletion::Success(Some(value))
                })
            } else {
                EngineCompletion::Success(None)
            }
        }
        "failure" => {
            let kind = match value.failure_kind.as_deref() {
                Some("exception") => CodeRunFailureKind::Exception,
                Some("invalid-output") => CodeRunFailureKind::InvalidOutput,
                Some("output-limit") => CodeRunFailureKind::OutputLimit,
                _ => anyhow::bail!("invalid Node worker failure kind"),
            };
            EngineCompletion::ForgedFailure(kind, value.message.unwrap_or_else(|| "".into()))
        }
        "invalid-output" => EngineCompletion::InvalidOutput,
        "output-limit" => EngineCompletion::OutputLimit,
        "worker-error" => EngineCompletion::ForgedFailure(CodeRunFailureKind::WorkerExit, {
            let mut units = "worker error: ".encode_utf16().collect::<Vec<_>>();
            if let Some(message) = value.message {
                units.extend(message.to_utf16());
            }
            CodeJsonString::from_utf16(&units)
        }),
        "worker-exit" => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Node worker exit codes are signed 32-bit integers"
            )]
            let code = value.code.unwrap_or(0.0) as i32;
            EngineCompletion::WorkerExit(code)
        }
        "compute-timeout" => EngineCompletion::ComputeTimeout,
        "wall-timeout" => EngineCompletion::WallTimeout,
        "abort" => EngineCompletion::Abort(value.reason.unwrap_or(Value::Null)),
        _ => anyhow::bail!("invalid Node worker completion"),
    })
}
