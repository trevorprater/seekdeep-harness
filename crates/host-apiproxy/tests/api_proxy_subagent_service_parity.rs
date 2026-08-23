//! Session-backed subagent catalog, history authorization, and direct interrupt RPCs.

use std::sync::Arc;

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, SessionId, SessionOrigin, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ModelSelection, PathOpenerInternals,
    PresetApiProxyOptions, PresetApiProxyRuntime, RpcId, RpcMethod, RpcReceipt, RpcReceiptReason,
    RpcRequest, RpcResponse, SessionApiProxyOptions, SessionApiProxyRuntime,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_session_projection::SessionProjectionRegistry;
use seekdeep_subagent::{
    SubagentDescriptorInput, SubagentRuntime, snapshot_subagent_descriptor,
    subagent_identity_projection_definition, subagent_timing_projection_definition,
};
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
    runtime: Arc<SessionApiProxyRuntime>,
}

impl Harness {
    fn new(with_projections: bool) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        if with_projections {
            let projections = SessionProjectionRegistry::install(&context).unwrap();
            projections
                .register(&context, subagent_identity_projection_definition())
                .unwrap();
            projections
                .register(&context, subagent_timing_projection_definition())
                .unwrap();
        }
        SubagentRuntime::install(&context).unwrap();
        let presets = PresetApiProxyRuntime::from_context(
            &context,
            PresetApiProxyOptions {
                default_model_selection: Arc::new(|| ModelSelection {
                    provider: "p".to_owned(),
                    model: "m".to_owned(),
                    reasoning_effort: None,
                }),
                save_default_model_selection: None,
                open_path: None,
                can_open_path: None,
                native_path_opener: PathOpenerInternals::default(),
            },
            Arc::new(TerminalDomains),
        );
        let runtime = SessionApiProxyRuntime::from_context(
            &context,
            SessionApiProxyOptions::default(),
            presets,
        )
        .unwrap();
        Self {
            context,
            sessions,
            agents,
            runtime,
        }
    }

    fn parent_and_child(&self) -> (SessionId, SessionId) {
        let parent_id = SessionId::new("parent");
        self.sessions
            .create(
                &self.context,
                Some(parent_id.clone()),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        let child_id = SessionId::new("child");
        let child = self
            .sessions
            .create(
                &self.context,
                Some(child_id.clone()),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    parent_session: Some(parent_id.clone()),
                    origin: Some(SessionOrigin::Subagent),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        child
            .append(
                "subagent/descriptor",
                serde_json::to_value(
                    snapshot_subagent_descriptor(&SubagentDescriptorInput::Continuable {
                        provider: "spawn".to_owned(),
                        label: "worker".to_owned(),
                        agent_provider: None,
                        agent_model: None,
                        persona: None,
                        tool_filter: None,
                    })
                    .unwrap(),
                )
                .unwrap(),
                AppendOptions::default(),
            )
            .unwrap();
        child
            .append(
                "user/message",
                serde_json::to_value(UserMessage::new(
                    vec![ContentBlock::Text {
                        text: "work".to_owned(),
                    }],
                    MessageSource::user(),
                ))
                .unwrap(),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .unwrap();
        (parent_id, child_id)
    }

    fn publish_agent(&self, id: &SessionId, running: bool) -> Arc<Agent> {
        let session = self.sessions.get(id).unwrap();
        let scope = create_scope(&self.context, ScopeKey::new(), None).unwrap();
        let scope_key = seekdeep_scope::scope_of(&scope.context).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            id.clone(),
            AgentOptions::default(),
            session,
            inbox,
            scope.context,
            scope_key,
        ));
        if running {
            agent.set_status(seekdeep_agent::AgentStatus::Running);
        }
        self.agents.register(&self.context, &agent, None).unwrap();
        agent
    }
}

async fn invoke(
    runtime: &SessionApiProxyRuntime,
    method: RpcMethod,
    payload: Value,
) -> RpcResult<Value> {
    runtime
        .unary(
            method,
            RpcRequest::new(RpcId::new("subagent-test"), payload),
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .result
}

fn value(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected success, got {other:?}"),
    }
}

#[tokio::test]
async fn list_uses_durable_catalog_but_activity_and_parent_availability_are_live_agent_facts() {
    let harness = Harness::new(true);
    let (parent, child) = harness.parent_and_child();
    let listed = value(
        invoke(
            &harness.runtime,
            RpcMethod::SubagentList,
            json!({ "parentSessionId": parent }),
        )
        .await,
    );
    assert_eq!(listed["parentAvailable"], false);
    assert_eq!(listed["entries"][0]["mode"], "continuable");
    assert_eq!(listed["entries"][0]["activity"], "inactive");
    harness.publish_agent(&parent, false);
    harness.publish_agent(&child, true);
    let active = value(
        invoke(
            &harness.runtime,
            RpcMethod::SubagentList,
            json!({ "parentSessionId": parent }),
        )
        .await,
    );
    assert_eq!(active["parentAvailable"], true);
    assert_eq!(active["entries"][0]["activity"], "running");
}

#[tokio::test]
async fn history_requires_the_exact_catalog_mode_and_serves_projection_watermarks() {
    let harness = Harness::new(true);
    let (parent, child) = harness.parent_and_child();
    let history = value(
        invoke(
            &harness.runtime,
            RpcMethod::SubagentHistory,
            json!({
                "parentSessionId": parent,
                "childSessionId": child,
                "mode": "continuable"
            }),
        )
        .await,
    );
    assert_eq!(history["hasMore"], false);
    assert_eq!(history["events"][0]["event"]["type"], "subagent/descriptor");
    assert!(history.get("projections").is_some());
    let wrong = invoke(
        &harness.runtime,
        RpcMethod::SubagentHistory,
        json!({
            "parentSessionId": parent,
            "childSessionId": child,
            "mode": "one-shot"
        }),
    )
    .await;
    assert!(matches!(
        wrong,
        RpcResult::Failure { ref error } if error.code == "subagent-not-found"
    ));
}

#[tokio::test]
async fn missing_projection_catalog_is_one_internal_face_and_interrupt_needs_no_live_parent() {
    let harness = Harness::new(false);
    let (parent, child) = harness.parent_and_child();
    let unavailable = invoke(
        &harness.runtime,
        RpcMethod::SubagentList,
        json!({ "parentSessionId": parent }),
    )
    .await;
    assert!(matches!(
        unavailable,
        RpcResult::Failure { ref error }
            if error.code == "internal" && error.message.contains("sessionProjections")
    ));
    let interrupted = value(
        invoke(
            &harness.runtime,
            RpcMethod::SubagentInterrupt,
            json!({
                "parentSessionId": parent,
                "childSessionId": child,
                "mode": "continuable"
            }),
        )
        .await,
    );
    assert_eq!(interrupted, json!({ "accepted": true }));
}
