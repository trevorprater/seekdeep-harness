//! Owned native client handles and foreign response-model/callback adapters.

use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_python_sdk::{
    Client, Error, ErrorKind, Harness, HarnessConfig, HarnessOptions, Host, MessageId, ModelId,
    Notification, NotificationFilter, NotificationObserver, NotificationSubscription, ObjectHandle,
    ProviderId, RequestId, RequestOptions, Result, RuntimeProcess, SeededIds, SessionId,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    Callback, Reply,
    objects::{self, Object},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
struct Handle(u64);

#[derive(Clone)]
enum Entry {
    Client(ClientEntry),
    Harness(Arc<Harness>),
    Subscription(Arc<NotificationSubscription>),
    Process(Arc<RuntimeProcess>),
}

#[derive(Clone)]
struct ClientEntry {
    client: Arc<Client>,
    owner: Arc<Mutex<Callback>>,
}

static ENTRIES: LazyLock<Mutex<HashMap<Handle, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn insert(entry: Entry) -> Result<Handle> {
    let value = NEXT_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| Error::new(ErrorKind::Overflow, "native SDK handle space exhausted"))?;
    let handle = Handle(value);
    ENTRIES.lock().insert(handle, entry);
    Ok(handle)
}

fn entry(handle: Handle) -> Result<Entry> {
    ENTRIES
        .lock()
        .get(&handle)
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Value, "native SDK handle is closed"))
}

fn client(handle: Handle) -> Result<Arc<Client>> {
    match entry(handle)? {
        Entry::Client(value) => Ok(value.client),
        _ => Err(Error::new(ErrorKind::Type, "native handle is not a client")),
    }
}

fn harness(handle: Handle) -> Result<Arc<Harness>> {
    match entry(handle)? {
        Entry::Harness(value) => Ok(value),
        _ => Err(Error::new(
            ErrorKind::Type,
            "native handle is not a harness",
        )),
    }
}

fn subscription(handle: Handle) -> Result<Arc<NotificationSubscription>> {
    match entry(handle)? {
        Entry::Subscription(value) => Ok(value),
        _ => Err(Error::new(
            ErrorKind::Type,
            "native handle is not a subscription",
        )),
    }
}

fn process(handle: Handle) -> Result<Arc<RuntimeProcess>> {
    match entry(handle)? {
        Entry::Process(value) => Ok(value),
        _ => Err(Error::new(
            ErrorKind::Type,
            "native handle is not a process",
        )),
    }
}

#[derive(Deserialize)]
#[serde(tag = "op")]
enum Operation {
    #[serde(rename = "harness.new")]
    NewHarness {
        config: Option<Value>,
        keywords: Map<String, Value>,
        owner: u64,
        seed: [u8; 16],
    },
    #[serde(rename = "harness.start")]
    StartHarness { handle: Handle },
    #[serde(rename = "harness.close")]
    CloseHarness { handle: Handle },
    #[serde(rename = "harness.session")]
    StartSession {
        handle: Handle,
        session: Option<SessionId>,
    },
    #[serde(rename = "harness.run")]
    Run {
        handle: Handle,
        session: Option<SessionId>,
        input: Value,
        observer: Option<ObjectHandle>,
    },
    #[serde(rename = "harness.session_run")]
    SessionRun {
        handle: Handle,
        session: SessionId,
        input: Value,
        observer: Option<ObjectHandle>,
    },
    #[serde(rename = "client.new")]
    New {
        config: HarnessConfig,
        owner: u64,
        seed: [u8; 16],
    },
    #[serde(rename = "client.bind")]
    Bind { handle: Handle, owner: u64 },
    #[serde(rename = "client.config")]
    Config {
        handle: Handle,
        config: HarnessConfig,
    },
    #[serde(rename = "client.start")]
    Start { handle: Handle },
    #[serde(rename = "client.close")]
    Close { handle: Handle },
    #[serde(rename = "client.initialize")]
    Initialize {
        handle: Handle,
        cwd: String,
        provider: ProviderId,
        model: ModelId,
        max_tokens: Option<Value>,
        validator: ObjectHandle,
    },
    #[serde(rename = "client.request")]
    Request {
        handle: Handle,
        method: String,
        params: Option<Value>,
        validator: Option<ObjectHandle>,
        #[serde(flatten)]
        options: Options,
    },
    #[serde(rename = "client.prompt")]
    Prompt {
        handle: Handle,
        session: SessionId,
        content: Value,
        validator: ObjectHandle,
        #[serde(flatten)]
        options: Options,
    },
    #[serde(rename = "client.notify")]
    Notify {
        handle: Handle,
        method: String,
        params: Option<Value>,
    },
    #[serde(rename = "client.respond")]
    Respond {
        handle: Handle,
        id: Value,
        result: Value,
    },
    #[serde(rename = "client.respond_error")]
    RespondError {
        handle: Handle,
        id: Value,
        code: Value,
        message: String,
        data: Option<Value>,
    },
    #[serde(rename = "client.next_notification")]
    NextNotification {
        handle: Handle,
        #[serde(default)]
        nonblocking: bool,
    },
    #[serde(rename = "client.notification_count")]
    NotificationCount { handle: Handle },
    #[serde(rename = "client.next_request")]
    NextRequest { handle: Handle },
    #[serde(rename = "client.subscribe")]
    Subscribe {
        handle: Handle,
        predicate: Option<ObjectHandle>,
        session: Option<SessionId>,
    },
    #[serde(rename = "client.handle_message")]
    HandleMessage {
        handle: Handle,
        message: Value,
        original: ObjectHandle,
    },
    #[serde(rename = "client.process")]
    Process { handle: Handle },
    #[serde(rename = "client.diagnostics")]
    Diagnostics { handle: Handle },
    #[serde(rename = "subscription.next")]
    SubscriptionNext {
        handle: Handle,
        #[serde(default)]
        nonblocking: bool,
    },
    #[serde(rename = "subscription.close")]
    SubscriptionClose { handle: Handle },
    #[serde(rename = "subscription.drain")]
    SubscriptionDrain {
        handle: Handle,
        observer: ObjectHandle,
    },
    #[serde(rename = "process.poll")]
    Poll { handle: Handle },
    #[serde(rename = "process.wait")]
    Wait {
        handle: Handle,
        timeout: Option<f64>,
    },
    #[serde(rename = "process.terminate")]
    Terminate { handle: Handle },
    #[serde(rename = "process.kill")]
    Kill { handle: Handle },
    #[serde(rename = "handle.drop")]
    Drop { handle: Handle },
}

#[derive(Clone, Copy, Default, Deserialize)]
struct Options {
    timeout: Option<f64>,
    observer: Option<ObjectHandle>,
    predicate: Option<ObjectHandle>,
    subscription: Option<Handle>,
}

pub(crate) fn handles(operation: &str) -> bool {
    [
        "client.",
        "subscription.",
        "process.",
        "handle.",
        "harness.",
    ]
    .iter()
    .any(|prefix| operation.starts_with(prefix))
}

fn object(callback: Callback, handle: ObjectHandle) -> Result<Arc<Object>> {
    Object::new(callback.with_owner(handle.owner), json!(handle))
}

fn filter(callback: Callback, handle: ObjectHandle) -> Result<NotificationFilter> {
    let function = object(callback, handle)?;
    Ok(Arc::new(move |notification| {
        let argument = notification
            .object_handle()
            .ok_or_else(|| Error::new(ErrorKind::Type, "notification has no interpreter object"))?;
        function
            .invoke("filter.call", json!(argument))?
            .as_bool()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Type,
                    "notification filter did not return a boolean",
                )
            })
    }))
}

fn observer(callback: Callback, handle: ObjectHandle) -> Result<NotificationObserver> {
    let function = object(callback, handle)?;
    Ok(Arc::new(move |notification| {
        let argument = notification
            .object_handle()
            .ok_or_else(|| Error::new(ErrorKind::Type, "notification has no interpreter object"))?;
        function.invoke("observer.call", json!(argument))?;
        Ok(())
    }))
}

fn options(callback: Callback, options: Options) -> Result<RequestOptions> {
    Ok(RequestOptions {
        timeout_seconds: options.timeout,
        on_notification: options
            .observer
            .map(|value| observer(callback, value))
            .transpose()?,
        notification_filter: options
            .predicate
            .map(|value| filter(callback, value))
            .transpose()?,
        notification_subscription: options.subscription.map(subscription).transpose()?,
    })
}

fn validate(
    callback: Callback,
    validator: ObjectHandle,
    value: Map<String, Value>,
) -> Result<Arc<Object>> {
    let validator = object(callback, validator)?;
    let result = validator.invoke("model.validate", Value::Object(value))?;
    Object::new(callback.with_owner(validator.handle().owner), result)
}

fn notification_reply(notification: Notification) -> Result<Reply> {
    let handle = notification
        .object_handle()
        .ok_or_else(|| Error::new(ErrorKind::Type, "notification has no interpreter object"))?;
    Ok(Reply::object(handle, notification))
}

fn selected(owner: &Arc<Mutex<Callback>>) -> Callback {
    *owner.lock()
}

fn host(owner: &Arc<Mutex<Callback>>) -> Host {
    let launch = Arc::clone(owner);
    let config = Arc::clone(owner);
    let notification = Arc::clone(owner);
    let lifetime = Arc::clone(owner);
    let mut host = Host::native(
        Arc::new(move || {
            serde_json::from_value(selected(&launch).invoke("runtime.launch", json!([]))?)
                .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))
        }),
        Arc::new(move || {
            selected(&config)
                .invoke("runtime.config", json!([]))?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Type,
                        "default configuration path must be a string",
                    )
                })
        }),
    );
    host.notification =
        Arc::new(move |value| objects::notification(selected(&notification), value));
    host.reader_lifetime = Arc::new(move || objects::reader_lifetime(selected(&lifetime)));
    host
}

fn bind_config(client: &ClientEntry) {
    let owner = Arc::clone(&client.owner);
    client.client.set_config_reader(Arc::new(move || {
        serde_json::from_value(selected(&owner).invoke("config.read", json!([]))?)
            .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))
    }));
}

fn run_reply(result: seekdeep_python_sdk::RunResult) -> Result<Reply> {
    let object = |handle: Option<ObjectHandle>| {
        handle
            .map(|value| json!({"kind":"object","value":value}))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Type,
                    "run observation has no interpreter identity",
                )
            })
    };
    let notifications = result
        .notifications
        .iter()
        .map(|value| object(value.object_handle()))
        .collect::<Result<Vec<_>>>()?;
    let events = result
        .events
        .iter()
        .map(|value| object(value.object_handle()))
        .collect::<Result<Vec<_>>>()?;
    let value = json!({"kind":"record","value":{
        "session_id":{"kind":"json","value":result.session_id},
        "final_response":{"kind":"json","value":result.final_response},
        "finish_reason":{"kind":"json","value":result.finish_reason},
        "session_root":{"kind":"json","value":result.session_root},
        "notifications":{"kind":"array","value":notifications},
        "events":{"kind":"array","value":events},
    }});
    Ok(Reply {
        value,
        retained: vec![Box::new(result)],
    })
}

// One exhaustive table keeps ABI operations and their ownership transfers auditable together.
#[allow(clippy::too_many_lines)]
pub(crate) fn run(input: Value, callback: Callback) -> Result<Reply> {
    let operation: Operation = serde_json::from_value(input)
        .map_err(|error| Error::new(ErrorKind::Value, error.to_string()))?;
    match operation {
        Operation::NewHarness {
            config,
            keywords,
            owner,
            seed,
        } => {
            if config.is_some() && !keywords.is_empty() {
                return Err(Error::new(
                    ErrorKind::Type,
                    "pass either DeepSeekHarnessConfig or keyword options, not both",
                ));
            }
            let defaults = serde_json::to_value(HarnessOptions::default())
                .map_err(|error| Error::new(ErrorKind::Value, error.to_string()))?;
            for key in keywords.keys() {
                if defaults.get(key).is_none() {
                    return Err(Error::new(
                        ErrorKind::Type,
                        format!(
                            "DeepSeekHarnessConfig.__init__() got an unexpected keyword argument '{key}'"
                        ),
                    ));
                }
            }
            let config: HarnessOptions =
                serde_json::from_value(config.unwrap_or(Value::Object(keywords)))
                    .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))?;
            let callback = callback.with_owner(owner);
            let client_owner = Arc::new(Mutex::new(callback));
            let harness = Harness::new(
                config,
                host(&client_owner),
                Arc::new(SeededIds::new(seed)),
                Arc::new(move |value| {
                    callback.invoke("harness.initialize", json!([value]))?;
                    Ok(())
                }),
                Arc::new(move |value| {
                    callback
                        .invoke("harness.prompt", json!([value]))?
                        .as_str()
                        .map(MessageId::new)
                        .ok_or_else(|| Error::new(ErrorKind::Type, "messageId must be a string"))
                }),
            )?;
            let data = json!({"config":harness.config(),"client_config":harness.client().config()});
            harness.set_config_reader(Arc::new(move || {
                serde_json::from_value(callback.invoke("config.read", json!([]))?)
                    .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))
            }));
            let client = insert(Entry::Client(ClientEntry {
                client: Arc::clone(harness.client()),
                owner: client_owner,
            }))?;
            let handle = insert(Entry::Harness(harness))?;
            Ok(Reply::json(
                json!({"handle":handle.0,"client":client.0,"config":data["config"],"client_config":data["client_config"]}),
            ))
        }
        Operation::StartHarness { handle } => {
            harness(handle)?.start()?;
            Ok(Reply::json(Value::Null))
        }
        Operation::CloseHarness { handle } => {
            harness(handle)?.close()?;
            Ok(Reply::json(Value::Null))
        }
        Operation::StartSession { handle, session } => {
            Ok(Reply::json(json!(harness(handle)?.start_session(session)?)))
        }
        Operation::Run {
            handle,
            session,
            input,
            observer: function,
        } => {
            let observer = function
                .map(|value| observer(callback, value))
                .transpose()?;
            run_reply(harness(handle)?.run(input, session, observer.as_ref())?)
        }
        Operation::SessionRun {
            handle,
            session,
            input,
            observer: function,
        } => {
            let observer = function
                .map(|value| observer(callback, value))
                .transpose()?;
            run_reply(harness(handle)?.run_session(&session, input, observer.as_ref())?)
        }
        Operation::New {
            config,
            owner,
            seed,
        } => {
            let owner = Arc::new(Mutex::new(callback.with_owner(owner)));
            let client = ClientEntry {
                client: Client::new(config, host(&owner), Arc::new(SeededIds::new(seed))),
                owner,
            };
            bind_config(&client);
            let handle = insert(Entry::Client(client))?;
            Ok(Reply::json(json!(handle.0)))
        }
        Operation::Bind { handle, owner } => {
            let Entry::Client(client) = entry(handle)? else {
                return Err(Error::new(ErrorKind::Type, "native handle is not a client"));
            };
            if client.client.process().is_some() {
                return Err(Error::new(
                    ErrorKind::Value,
                    "cannot rebind a running native client",
                ));
            }
            *client.owner.lock() = callback.with_owner(owner);
            bind_config(&client);
            Ok(Reply::json(Value::Null))
        }
        Operation::Config { handle, config } => {
            client(handle)?.set_config(config);
            Ok(Reply::json(Value::Null))
        }
        Operation::Start { handle } => {
            client(handle)?.start()?;
            Ok(Reply::json(Value::Null))
        }
        Operation::Close { handle } => {
            client(handle)?.close()?;
            Ok(Reply::json(Value::Null))
        }
        Operation::Initialize {
            handle,
            cwd,
            provider,
            model,
            max_tokens,
            validator,
        } => {
            let result =
                client(handle)?.initialize(&cwd, &provider, &model, max_tokens, |value| {
                    validate(callback, validator, value)
                })?;
            Ok(Reply::object(result.handle(), result))
        }
        Operation::Request {
            handle,
            method,
            params,
            validator,
            options: settings,
        } => {
            let client = client(handle)?;
            let settings = options(callback, settings)?;
            match validator {
                None => Ok(Reply::json(client.request_raw(&method, params, settings)?)),
                Some(validator) => {
                    let result = validate(
                        callback,
                        validator,
                        client.request_object(&method, params, settings)?,
                    )?;
                    Ok(Reply::object(result.handle(), result))
                }
            }
        }
        Operation::Prompt {
            handle,
            session,
            content,
            validator,
            options: settings,
        } => {
            let result = client(handle)?.session_prompt_with(
                &session,
                content,
                options(callback, settings)?,
                |value| {
                    let model = validate(callback, validator, value)?;
                    let field = model.invoke("object.attribute", json!("messageId"))?;
                    field
                        .as_str()
                        .map(MessageId::new)
                        .ok_or_else(|| Error::new(ErrorKind::Type, "messageId must be a string"))
                },
            )?;
            Ok(Reply::json(json!(result)))
        }
        Operation::Notify {
            handle,
            method,
            params,
        } => {
            client(handle)?.notify(&method, params)?;
            Ok(Reply::json(Value::Null))
        }
        Operation::Respond { handle, id, result } => {
            let id = RequestId::from_value(&id).ok_or_else(|| {
                Error::new(ErrorKind::Type, "request id must be a string or integer")
            })?;
            client(handle)?.respond(&id, result)?;
            Ok(Reply::json(Value::Null))
        }
        Operation::RespondError {
            handle,
            id,
            code,
            message,
            data,
        } => {
            let id = RequestId::from_value(&id).ok_or_else(|| {
                Error::new(ErrorKind::Type, "request id must be a string or integer")
            })?;
            client(handle)?.respond_error(&id, code, &message, data)?;
            Ok(Reply::json(Value::Null))
        }
        Operation::NextNotification {
            handle,
            nonblocking,
        } => {
            let client = client(handle)?;
            notification_reply(if nonblocking {
                client.try_notification()?
            } else {
                client.next_notification()?
            })
        }
        Operation::NotificationCount { handle } => {
            Ok(Reply::json(json!(client(handle)?.notification_count())))
        }
        Operation::NextRequest { handle } => Ok(Reply::json(
            serde_json::to_value(client(handle)?.next_request()?)
                .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))?,
        )),
        Operation::Subscribe {
            handle,
            predicate,
            session,
        } => {
            let client = client(handle)?;
            let value = match session {
                Some(session) => client.subscribe_session(session),
                None => client.subscribe_notifications(
                    predicate.map(|value| filter(callback, value)).transpose()?,
                ),
            };
            Ok(Reply::json(json!(insert(Entry::Subscription(value))?.0)))
        }
        Operation::HandleMessage {
            handle,
            message,
            original,
        } => {
            let original = object(callback, original)?;
            client(handle)?.handle_message_with(&message, &|value| {
                objects::notification_from_message(
                    callback.with_owner(original.handle().owner),
                    &original,
                    value,
                    message.get("params").is_some_and(Value::is_object),
                )
            })?;
            Ok(Reply::json(Value::Null))
        }
        Operation::Process { handle } => match client(handle)?.process() {
            Some(process) => Ok(Reply::json(
                json!({"handle":insert(Entry::Process(Arc::clone(&process)))?.0,"pid":process.pid()}),
            )),
            None => Ok(Reply::json(Value::Null)),
        },
        Operation::Diagnostics { handle } => {
            Ok(Reply::json(json!(client(handle)?.runtime_diagnostics())))
        }
        Operation::SubscriptionNext {
            handle,
            nonblocking,
        } => {
            let subscription = subscription(handle)?;
            notification_reply(if nonblocking {
                subscription.try_next()?
            } else {
                subscription.next()?
            })
        }
        Operation::SubscriptionClose { handle } => {
            subscription(handle)?.close();
            Ok(Reply::json(Value::Null))
        }
        Operation::SubscriptionDrain {
            handle,
            observer: function,
        } => {
            subscription(handle)?.drain(&observer(callback, function)?)?;
            Ok(Reply::json(Value::Null))
        }
        Operation::Poll { handle } => Ok(Reply::json(json!(process(handle)?.poll()?))),
        Operation::Wait { handle, timeout } => {
            Ok(Reply::json(json!(process(handle)?.wait(timeout)?)))
        }
        Operation::Terminate { handle } => {
            process(handle)?.terminate()?;
            Ok(Reply::json(Value::Null))
        }
        Operation::Kill { handle } => {
            process(handle)?.kill()?;
            Ok(Reply::json(Value::Null))
        }
        Operation::Drop { handle } => {
            let entry = ENTRIES.lock().remove(&handle);
            drop(entry);
            Ok(Reply::json(Value::Null))
        }
    }
}
