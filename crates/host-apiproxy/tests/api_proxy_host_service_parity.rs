//! Host-domain cases ported from the pinned API Proxy workspace specification.

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_agent::AgentRegistry;
use seekdeep_cordis::Context;
use seekdeep_core::session_store::SessionStore;
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyDefaults, ApiProxyRuntime, ApiProxyService, ClientResponse,
    ModelSelection, PathOpenerInternals, RpcError, RpcId, RpcMethod, RpcReceipt, RpcRequest,
    RpcResponse, RpcResult,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_host_directory_picker::{
    DirectoryEntry, DirectoryListing, DirectoryPickerCapability, DirectoryPickerError,
    DirectoryPickerErrorCode, DirectoryPickerFailure, DirectoryPickerService,
};
use seekdeep_llm::{AbortSignal, LlmRuntime};
use seekdeep_user_questions::install as install_user_questions;
use serde_json::{Map, Value, json};

#[derive(Debug, Default)]
struct RemainingDomains {
    calls: Mutex<Vec<RpcMethod>>,
}

impl ApiProxyRuntime for RemainingDomains {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        self.calls.lock().unwrap().push(method);
        async move {
            Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success {
                    value: Some(json!({ "delegated": method.as_str() })),
                },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        async { Ok(RpcReceipt::Accepted) }.boxed()
    }

    fn mux(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<MuxFrame> {
        futures::stream::empty().boxed()
    }

    fn host(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        futures::stream::empty().boxed()
    }

    fn session_log(
        &self,
        _query: SessionLogQuery,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<seekdeep_client_connection::HttpResponse>> {
        async {
            Ok(seekdeep_client_connection::HttpResponse::text(
                501, "not used",
            ))
        }
        .boxed()
    }
}

fn defaults() -> ApiProxyDefaults {
    ApiProxyDefaults {
        default_model_selection: Arc::new(|| ModelSelection {
            provider: "test".into(),
            model: "test-model".into(),
            reasoning_effort: None,
        }),
        cwd: "/tmp/project".to_owned(),
        open_path: None,
        open_text_file: None,
        can_open_path: Some(Arc::new(|| false)),
        native_path_opener: PathOpenerInternals::default(),
        cold_blank_probe_max_bytes: None,
    }
}

fn native_picker(
    pick: impl Fn(AbortSignal) -> BoxFuture<'static, anyhow::Result<Option<String>>>
    + Send
    + Sync
    + 'static,
) -> Arc<DirectoryPickerService> {
    DirectoryPickerService::new(DirectoryPickerCapability::Native {
        pick: Arc::new(pick),
    })
}

fn browse_picker(
    list: impl Fn(
        Option<String>,
        AbortSignal,
    ) -> BoxFuture<'static, Result<DirectoryListing, DirectoryPickerFailure>>
    + Send
    + Sync
    + 'static,
    create: impl Fn(String, String) -> BoxFuture<'static, Result<String, DirectoryPickerFailure>>
    + Send
    + Sync
    + 'static,
) -> Arc<DirectoryPickerService> {
    DirectoryPickerService::new(DirectoryPickerCapability::Browse {
        list: Arc::new(list),
        create_directory: Arc::new(create),
    })
}

fn service_with(
    picker: Arc<DirectoryPickerService>,
    defaults: ApiProxyDefaults,
) -> (Arc<ApiProxyService>, Arc<RemainingDomains>) {
    let domains = Arc::new(RemainingDomains::default());
    (
        ApiProxyService::new(defaults, picker, Arc::new(|| 3), domains.clone()),
        domains,
    )
}

async fn invoke(
    service: &ApiProxyService,
    method: RpcMethod,
    payload: Value,
    signal: AbortSignal,
) -> RpcResult<Value> {
    service
        .unary(
            method,
            RpcRequest::new(RpcId::new("host-test"), payload),
            signal,
        )
        .await
        .expect("runtime call")
        .result
}

fn value(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected value success, got {other:?}"),
    }
}

fn error(result: RpcResult<Value>) -> RpcError {
    match result {
        RpcResult::Failure { error } => error,
        other @ RpcResult::Success { .. } => panic!("expected failure, got {other:?}"),
    }
}

#[tokio::test]
async fn native_picker_returns_selection_and_explicit_cancellation() {
    let (selected, _) = service_with(
        native_picker(|_| async { Ok(Some("/tmp/project".to_owned())) }.boxed()),
        defaults(),
    );
    assert_eq!(
        value(
            invoke(
                &selected,
                RpcMethod::HostPickDirectory,
                json!({}),
                AbortSignal::default(),
            )
            .await
        ),
        json!({ "path": "/tmp/project" })
    );

    let (cancelled, _) = service_with(native_picker(|_| async { Ok(None) }.boxed()), defaults());
    assert_eq!(
        value(
            invoke(
                &cancelled,
                RpcMethod::HostPickDirectory,
                json!({}),
                AbortSignal::default(),
            )
            .await
        ),
        json!({ "path": null })
    );
}

#[tokio::test]
async fn context_constructor_requires_and_composes_the_configuration_runtime() {
    let context = Context::new();
    native_picker(|_| async { Ok(None) }.boxed())
        .provide(&context)
        .unwrap();
    SessionStore::install(&context).unwrap();
    install_user_questions(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let domains = Arc::new(RemainingDomains::default());
    let missing =
        ApiProxyService::from_context(&context, defaults(), Arc::new(|| 0), domains.clone())
            .unwrap_err();
    assert!(missing.to_string().contains("llm service is required"));

    LlmRuntime::install(&context).unwrap();
    let service =
        ApiProxyService::from_context(&context, defaults(), Arc::new(|| 0), domains.clone())
            .unwrap();
    assert_eq!(
        value(
            invoke(
                &service,
                RpcMethod::LlmProviders,
                json!({}),
                AbortSignal::default(),
            )
            .await
        ),
        json!({ "providers": [] })
    );
    assert!(domains.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_picker_maps_abort_failure_and_capability_mismatch() {
    let (service, _) = service_with(
        native_picker(|signal| {
            async move {
                signal.cancelled().await;
                anyhow::bail!("aborted")
            }
            .boxed()
        }),
        defaults(),
    );
    let signal = AbortSignal::default();
    let pending = invoke(
        &service,
        RpcMethod::HostPickDirectory,
        json!({}),
        signal.clone(),
    );
    tokio::time::sleep(Duration::from_millis(1)).await;
    signal.abort();
    assert_eq!(pending.await_failure().await.code, "cancelled");

    let (failed, _) = service_with(
        native_picker(|_| async { anyhow::bail!("no chooser installed") }.boxed()),
        defaults(),
    );
    let failure = error(
        invoke(
            &failed,
            RpcMethod::HostPickDirectory,
            json!({}),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(failure.code, "internal");
    assert_eq!(
        failure.message,
        "directory picker failed: no chooser installed"
    );

    let (browse, _) = service_with(browse_stub(), defaults());
    let mismatch = error(
        invoke(
            &browse,
            RpcMethod::HostPickDirectory,
            json!({}),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(mismatch.code, "directory-picker-unavailable");
    assert_eq!(
        mismatch.details,
        Map::from_iter([("capability".to_owned(), json!("browse"))])
    );
}

trait AwaitFailure {
    async fn await_failure(self) -> RpcError;
}

impl<F> AwaitFailure for F
where
    F: Future<Output = RpcResult<Value>>,
{
    async fn await_failure(self) -> RpcError {
        error(self.await)
    }
}

fn browse_stub() -> Arc<DirectoryPickerService> {
    browse_picker(
        |path, _| {
            async move {
                if path.as_deref() == Some("/denied") {
                    return Err(DirectoryPickerError::new(
                        DirectoryPickerErrorCode::DirectoryUnreadable,
                        "/denied",
                        "cannot list /denied",
                    )
                    .into());
                }
                let target = path.unwrap_or_else(|| "/home/user".to_owned());
                Ok(DirectoryListing {
                    path: target.clone(),
                    home: "/home/user".to_owned(),
                    crumbs: vec![DirectoryEntry {
                        name: "/".to_owned(),
                        path: "/".to_owned(),
                        hidden: false,
                    }],
                    entries: vec![DirectoryEntry {
                        name: "projects".to_owned(),
                        path: format!("{target}/projects"),
                        hidden: false,
                    }],
                    truncated: false,
                })
            }
            .boxed()
        },
        |path, name| {
            async move {
                if name == "taken" {
                    return Err(DirectoryPickerError::new(
                        DirectoryPickerErrorCode::DirectoryExists,
                        format!("{path}/{name}"),
                        "already exists",
                    )
                    .into());
                }
                if name == "unwritable" {
                    return Err(DirectoryPickerFailure::Internal(anyhow::anyhow!(
                        "disk detached"
                    )));
                }
                Ok(format!("{path}/{name}"))
            }
            .boxed()
        },
    )
}

#[tokio::test]
async fn browse_picker_lists_creates_and_maps_typed_and_internal_failures() {
    let (service, _) = service_with(browse_stub(), defaults());
    let home = value(
        invoke(
            &service,
            RpcMethod::HostListDirectory,
            json!({}),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(home["path"], "/home/user");
    assert_eq!(home["home"], "/home/user");
    let listed = value(
        invoke(
            &service,
            RpcMethod::HostListDirectory,
            json!({ "path": "/home/user/projects" }),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(listed["path"], "/home/user/projects");
    assert_eq!(
        value(
            invoke(
                &service,
                RpcMethod::HostCreateDirectory,
                json!({ "path": "/home/user", "name": "fresh" }),
                AbortSignal::default(),
            )
            .await
        ),
        json!({ "path": "/home/user/fresh" })
    );

    let unreadable = error(
        invoke(
            &service,
            RpcMethod::HostListDirectory,
            json!({ "path": "/denied" }),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(unreadable.code, "directory-unreadable");
    assert_eq!(unreadable.details["path"], "/denied");
    let exists = error(
        invoke(
            &service,
            RpcMethod::HostCreateDirectory,
            json!({ "path": "/home/user", "name": "taken" }),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(exists.code, "directory-exists");
    let internal = error(
        invoke(
            &service,
            RpcMethod::HostCreateDirectory,
            json!({ "path": "/home/user", "name": "unwritable" }),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(internal.code, "internal");
    assert_eq!(internal.message, "disk detached");
}

#[tokio::test]
async fn browse_listing_abort_and_native_mismatch_use_exact_wire_codes() {
    let (service, _) = service_with(
        browse_picker(
            |_path, signal| {
                async move {
                    signal.cancelled().await;
                    Err(DirectoryPickerFailure::Internal(anyhow::anyhow!(
                        "scan aborted"
                    )))
                }
                .boxed()
            },
            |_path, _name| async { Ok("/never".to_owned()) }.boxed(),
        ),
        defaults(),
    );
    let signal = AbortSignal::default();
    let pending = invoke(
        &service,
        RpcMethod::HostListDirectory,
        json!({}),
        signal.clone(),
    );
    tokio::time::sleep(Duration::from_millis(1)).await;
    signal.abort();
    assert_eq!(pending.await_failure().await.code, "cancelled");

    let (native, _) = service_with(native_picker(|_| async { Ok(None) }.boxed()), defaults());
    for (method, payload) in [
        (RpcMethod::HostListDirectory, json!({})),
        (
            RpcMethod::HostCreateDirectory,
            json!({ "path": "/x", "name": "y" }),
        ),
    ] {
        let mismatch = error(invoke(&native, method, payload, AbortSignal::default()).await);
        assert_eq!(mismatch.code, "directory-picker-unavailable");
        assert_eq!(mismatch.details["capability"], "native");
    }
}

#[tokio::test]
async fn unknown_picker_kind_is_preserved_in_unavailable_error() {
    let picker = DirectoryPickerService::new(DirectoryPickerCapability::Unknown {
        kind: "remote-volume".to_owned(),
    });
    let (service, _) = service_with(picker, defaults());
    let failure = error(
        invoke(
            &service,
            RpcMethod::HostPickDirectory,
            json!({}),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(failure.details["capability"], "remote-volume");
    assert!(failure.message.ends_with("serves \"remote-volume\""));
}

#[tokio::test]
async fn describe_reads_live_defaults_count_and_path_capability() {
    let selection = Arc::new(Mutex::new(ModelSelection {
        provider: "first".into(),
        model: "one".into(),
        reasoning_effort: None,
    }));
    let visible = Arc::new(Mutex::new(false));
    let mut defaults = defaults();
    let selection_for_read = selection.clone();
    defaults.default_model_selection = Arc::new(move || selection_for_read.lock().unwrap().clone());
    let visible_for_read = visible.clone();
    defaults.can_open_path = Some(Arc::new(move || *visible_for_read.lock().unwrap()));
    let picker = native_picker(|_| async { Ok(None) }.boxed());
    let domains = Arc::new(RemainingDomains::default());
    let count = Arc::new(Mutex::new(2_usize));
    let count_for_read = count.clone();
    let service = ApiProxyService::new(
        defaults,
        picker,
        Arc::new(move || *count_for_read.lock().unwrap()),
        domains,
    );

    let first = value(
        invoke(
            &service,
            RpcMethod::HostDescribe,
            json!({}),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(first["version"], "0.0.1");
    assert_eq!(first["cwd"], "/tmp/project");
    assert_eq!(first["provider"], "first");
    assert_eq!(first["attachedSessions"], 2);
    assert_eq!(first["canOpenPath"], false);

    *selection.lock().unwrap() = ModelSelection {
        provider: "saved".into(),
        model: "next".into(),
        reasoning_effort: Some("high".to_owned()),
    };
    *visible.lock().unwrap() = true;
    *count.lock().unwrap() = 4;
    let second = value(
        invoke(
            &service,
            RpcMethod::HostDescribe,
            json!({}),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(second["provider"], "saved");
    assert_eq!(second["model"], "next");
    assert_eq!(second["attachedSessions"], 4);
    assert_eq!(second["canOpenPath"], true);
}

#[tokio::test]
async fn open_path_uses_injected_boundary_and_maps_abort_and_internal_failure() {
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut open_defaults = defaults();
    let opened_for_call = opened.clone();
    open_defaults.open_path = Some(Arc::new(move |path, _| {
        opened_for_call.lock().unwrap().push(path);
        async { Ok(()) }.boxed()
    }));
    open_defaults.can_open_path = None;
    let (service, _) = service_with(native_picker(|_| async { Ok(None) }.boxed()), open_defaults);
    assert_eq!(
        value(
            invoke(
                &service,
                RpcMethod::HostOpenPath,
                json!({ "path": "/tmp/a.txt" }),
                AbortSignal::default(),
            )
            .await
        ),
        json!({ "opened": true })
    );
    assert_eq!(*opened.lock().unwrap(), vec!["/tmp/a.txt"]);
    let described = value(
        invoke(
            &service,
            RpcMethod::HostDescribe,
            json!({}),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(described["canOpenPath"], true);

    let mut abort_defaults = defaults();
    abort_defaults.open_path = Some(Arc::new(|_, signal| {
        async move {
            signal.cancelled().await;
            anyhow::bail!("aborted")
        }
        .boxed()
    }));
    let (aborting, _) = service_with(
        native_picker(|_| async { Ok(None) }.boxed()),
        abort_defaults,
    );
    let signal = AbortSignal::default();
    let pending = invoke(
        &aborting,
        RpcMethod::HostOpenPath,
        json!({ "path": "/tmp/a.txt" }),
        signal.clone(),
    );
    tokio::time::sleep(Duration::from_millis(1)).await;
    signal.abort();
    let cancelled = pending.await_failure().await;
    assert_eq!(cancelled.code, "cancelled");
    assert_eq!(cancelled.message, "path open was aborted");

    let mut failed_defaults = defaults();
    failed_defaults.open_path = Some(Arc::new(|_, _| async { anyhow::bail!("boom") }.boxed()));
    let (failed, _) = service_with(
        native_picker(|_| async { Ok(None) }.boxed()),
        failed_defaults,
    );
    let failure = error(
        invoke(
            &failed,
            RpcMethod::HostOpenPath,
            json!({ "path": "/tmp/a.txt" }),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(failure.code, "internal");
    assert_eq!(failure.message, "path open failed: boom");
}

#[tokio::test]
async fn non_host_unary_is_delegated_without_rewriting_correlation() {
    let (service, domains) =
        service_with(native_picker(|_| async { Ok(None) }.boxed()), defaults());
    let response = service
        .unary(
            RpcMethod::SkillList,
            RpcRequest::new(RpcId::new("delegated-id"), json!({})),
            AbortSignal::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.rpc_id.as_str(), "delegated-id");
    assert_eq!(value(response.result)["delegated"], "skill.list");
    assert_eq!(*domains.calls.lock().unwrap(), vec![RpcMethod::SkillList]);
}
