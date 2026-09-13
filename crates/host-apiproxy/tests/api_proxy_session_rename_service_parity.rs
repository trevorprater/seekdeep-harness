//! Production `session.rename` service delegation and error classification.

use std::sync::Arc;

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ModelSelection, PathOpenerInternals,
    PresetApiProxyOptions, PresetApiProxyRuntime, RpcId, RpcMethod, RpcReceipt, RpcReceiptReason,
    RpcRequest, RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_session_title::{SessionTitleConfig, SessionTitleService};
use serde_json::{Value, json};

#[derive(Debug)]
struct TerminalDomains;

impl ApiProxyRuntime for TerminalDomains {
    fn unary(
        &self,
        _method: RpcMethod,
        request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        async move {
            Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success { value: None },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        async {
            Ok(RpcReceipt::Rejected {
                reason: RpcReceiptReason::NotPending,
            })
        }
        .boxed()
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
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        async { Ok(HttpResponse::text(501, "not used")) }.boxed()
    }
}

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    runtime: Arc<PresetApiProxyRuntime>,
}

impl Harness {
    fn new(with_titles: bool) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        if with_titles {
            SessionTitleService::install(
                &context,
                SessionTitleConfig {
                    fallback_max_words: 5,
                    fallback_max_bytes: 40,
                    max_title_bytes: 40,
                },
            )
            .unwrap();
        }
        let runtime = PresetApiProxyRuntime::from_context(
            &context,
            PresetApiProxyOptions {
                default_model_selection: Arc::new(|| ModelSelection {
                    provider: "provider".to_owned(),
                    model: "model".to_owned(),
                    reasoning_effort: None,
                }),
                save_default_model_selection: None,
                open_path: None,
                can_open_path: None,
                native_path_opener: PathOpenerInternals::default(),
            },
            Arc::new(TerminalDomains),
        );
        Self {
            context,
            sessions,
            agents,
            runtime,
        }
    }

    fn live(&self, id: &str) -> Arc<Agent> {
        let session = self
            .sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        self.publish(session)
    }

    fn publish(&self, session: Arc<Session>) -> Arc<Agent> {
        let scope = create_scope(&self.context, ScopeKey::new(), None).unwrap();
        let scope_key = seekdeep_scope::scope_of(&scope.context).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            scope.context,
            scope_key,
        ));
        self.agents.register(&self.context, &agent, None).unwrap();
        agent
    }
}

async fn rename(
    runtime: &PresetApiProxyRuntime,
    session_id: &str,
    title: &str,
) -> RpcResult<Value> {
    runtime
        .unary(
            RpcMethod::SessionRename,
            RpcRequest::new(
                RpcId::new("rename-test"),
                json!({ "sessionId": session_id, "title": title }),
            ),
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .result
}

#[tokio::test]
async fn accepted_title_is_normalized_logged_as_user_and_echoes_the_event_seq() {
    let harness = Harness::new(true);
    let agent = harness.live("rename");
    let result = rename(&harness.runtime, "rename", "  new   name  ").await;
    let value = match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected rename success, got {other:?}"),
    };
    assert_eq!(value["title"], "new name");
    let event = agent
        .session()
        .events()
        .into_iter()
        .find(|event| event.event_type == "session/title")
        .unwrap();
    assert_eq!(value["seq"], event.seq);
    assert_eq!(event.data["title"], "new name");
    assert_eq!(event.data["source"]["kind"], "user");
}

#[tokio::test]
async fn only_empty_normalization_is_title_invalid() {
    let harness = Harness::new(true);
    harness.live("invalid");
    let result = rename(&harness.runtime, "invalid", " \u{200b} ").await;
    match result {
        RpcResult::Failure { error } => {
            assert_eq!(error.code, "title-invalid");
            assert_eq!(
                error.message,
                "session title must contain visible characters"
            );
            assert_eq!(error.details["sessionId"], "invalid");
        }
        other @ RpcResult::Success { .. } => panic!("expected title failure, got {other:?}"),
    }
}

#[tokio::test]
async fn stale_session_and_missing_title_service_are_internal_failures() {
    let stale_harness = Harness::new(true);
    let stale = Session::create(&SessionId::new("stale"), None, None).unwrap();
    stale_harness.publish(stale);
    let stale_result = rename(&stale_harness.runtime, "stale", "name").await;
    assert!(matches!(
        stale_result,
        RpcResult::Failure { ref error } if error.code == "internal"
    ));

    let missing = Harness::new(false);
    missing.live("missing");
    let missing_result = rename(&missing.runtime, "missing", "name").await;
    assert!(matches!(
        missing_result,
        RpcResult::Failure { ref error }
            if error.code == "internal" && error.message.contains("mounts no session-title service")
    ));
}
