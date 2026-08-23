//! Production model directory, selection, and prompt/request epoch coupling.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentControlError, AgentController, AgentEvents, AgentOptions,
    AgentRegistry, CancelOptions, Inbox, InboxTarget, MaintenanceReservation,
    NoopInboxNotifications, assemble_context_for,
};
use seekdeep_attachment::{
    AttachmentBackend, AttachmentId, AttachmentStore, ImageAttachmentLimits, ImageAttachmentRef,
    ImageMediaType, SaveImageAttachment, StoredImageAttachment,
};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ModelSelection, PathOpenerInternals,
    PresetApiProxyOptions, PresetApiProxyRuntime, RpcId, RpcMethod, RpcReceipt, RpcReceiptReason,
    RpcRequest, RpcResponse, SaveDefaultModelSelection,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{
    AbortSignal, AdapterStream, ContentBlock, GenerateOptions, LlmAdapter, LlmCallConfig,
    LlmModelInfo, LlmModelReasoningInfo, LlmReasoningEffortInfo, LlmResolvedModelInfo, LlmRuntime,
    ModelId, ModelModality, ProviderId, ReasoningEffortId, UserMessage,
};
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use serde_json::{Value, json};

struct TestController {
    id: SessionId,
    inbox: Arc<Inbox>,
}

struct MemoryAttachments {
    limits: ImageAttachmentLimits,
    operations: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl AttachmentBackend for MemoryAttachments {
    fn image_limits(&self) -> &ImageAttachmentLimits {
        &self.limits
    }

    async fn validate_image(&self, input: &SaveImageAttachment) -> anyhow::Result<()> {
        self.operations
            .lock()
            .push(format!("validate:{}", input.data[0]));
        Ok(())
    }

    async fn save_image(&self, input: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef> {
        self.operations
            .lock()
            .push(format!("save:{}", input.data[0]));
        Ok(ImageAttachmentRef {
            attachment_id: AttachmentId::new(format!("att-{}", input.data[0])),
            media_type: input.media_type,
            bytes: u64::try_from(input.data.len()).unwrap(),
            width: 1,
            height: 1,
            name: input.name,
        })
    }

    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        let byte = reference
            .attachment_id
            .as_str()
            .strip_prefix("att-")
            .unwrap()
            .parse::<u8>()?;
        Ok(StoredImageAttachment {
            reference: reference.clone(),
            data: vec![byte],
        })
    }
}

impl AgentController for TestController {
    fn send(
        &self,
        message: UserMessage,
        target: InboxTarget,
        _wakeup: bool,
    ) -> Result<(), AgentControlError> {
        self.inbox
            .append(target, message)
            .map_err(|error| AgentControlError::Inbox(error.to_string()))
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Err(AgentControlError::ActiveWork(self.id.clone()))
    }
}

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

#[derive(Clone)]
enum Catalog {
    Models(Vec<LlmModelInfo>),
    Error(&'static str),
}

struct CatalogAdapter {
    name: &'static str,
    catalog: Catalog,
    reasoning: Option<LlmModelReasoningInfo>,
    exact_error: Option<&'static str>,
    modalities: Option<Vec<ModelModality>>,
}

#[async_trait]
impl LlmAdapter for CatalogAdapter {
    fn provider_info(&self, provider: &str) -> seekdeep_llm::LlmProviderInfo {
        seekdeep_llm::LlmProviderInfo {
            id: ProviderId::new(provider),
            name: self.name.to_owned(),
        }
    }

    async fn list_models(&self, _provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        match &self.catalog {
            Catalog::Models(models) => Ok(models.clone()),
            Catalog::Error(message) => anyhow::bail!(*message),
        }
    }

    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        if let Some(message) = self.exact_error {
            anyhow::bail!(message);
        }
        Ok(LlmResolvedModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new(model),
            name: model.to_owned(),
            description: None,
            input_modalities: self.modalities.clone(),
            context: None,
            default_max_tokens: None,
            reasoning: self.reasoning.clone(),
        })
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(futures::stream::empty())
    }
}

fn reasoning() -> LlmModelReasoningInfo {
    LlmModelReasoningInfo {
        efforts: ["off", "high", "max"]
            .into_iter()
            .map(|id| LlmReasoningEffortInfo {
                id: ReasoningEffortId::new(id),
                name: match id {
                    "off" => "Off",
                    "high" => "High",
                    "max" => "Max",
                    _ => unreachable!(),
                }
                .to_owned(),
                description: None,
            })
            .collect(),
        default_effort: Some(ReasoningEffortId::new("high")),
    }
}

fn model(provider: &str, id: &str, name: &str, description: Option<&str>) -> LlmModelInfo {
    LlmModelInfo {
        provider: ProviderId::new(provider),
        id: ModelId::new(id),
        name: name.to_owned(),
        description: description.map(ToOwned::to_owned),
        input_modalities: None,
    }
}

struct Harness {
    context: Context,
    prompt: Arc<SystemPrompt>,
    agent: Arc<Agent>,
    runtime: Arc<PresetApiProxyRuntime>,
    defaults: Arc<Mutex<ModelSelection>>,
}

impl Harness {
    fn new(logged: Option<ModelSelection>, save: Option<SaveDefaultModelSelection>) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        prompt.provide(&context).unwrap();
        let llm = LlmRuntime::install(&context).unwrap();
        register_catalogs(&llm);
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("models")),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        if let Some(logged) = logged {
            let mut config = json!({
                "provider": logged.provider,
                "model": logged.model,
            });
            if let Some(effort) = logged.reasoning_effort {
                config["reasoningEffort"] = Value::String(effort);
            }
            session
                .append(
                    "request/header",
                    json!({ "header": { "config": config }, "reason": "initial" }),
                    AppendOptions::default(),
                )
                .unwrap();
        }
        let scope = create_scope(&context, ScopeKey::new(), None).unwrap();
        let scope_key = seekdeep_scope::scope_of(&scope.context).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox.clone(),
            scope.context,
            scope_key,
        ));
        agent
            .install_controller(Arc::new(TestController {
                id: agent.id().clone(),
                inbox,
            }))
            .unwrap();
        agents.register(&context, &agent, None).unwrap();
        let defaults = Arc::new(Mutex::new(ModelSelection {
            provider: "deepseek-official".to_owned(),
            model: "deepseek-chat".to_owned(),
            reasoning_effort: None,
        }));
        let runtime = PresetApiProxyRuntime::from_context(
            &context,
            PresetApiProxyOptions {
                default_model_selection: {
                    let defaults = defaults.clone();
                    Arc::new(move || defaults.lock().clone())
                },
                save_default_model_selection: save,
                open_path: None,
                can_open_path: None,
                native_path_opener: PathOpenerInternals::default(),
            },
            Arc::new(TerminalDomains),
        );
        Self {
            context,
            prompt,
            agent,
            runtime,
            defaults,
        }
    }
}

fn register_catalogs(llm: &Arc<LlmRuntime>) {
    llm.register_adapter(
        &["deepseek-official".to_owned()],
        Arc::new(CatalogAdapter {
            name: "DeepSeek",
            catalog: Catalog::Models(vec![
                model("deepseek-official", "deepseek-chat", "DeepSeek Chat", None),
                model(
                    "deepseek-official",
                    "deepseek-reasoner",
                    "DeepSeek Reasoner",
                    Some("Reasoning model"),
                ),
            ]),
            reasoning: Some(reasoning()),
            exact_error: None,
            modalities: None,
        }),
    )
    .unwrap();
    for (provider, adapter) in [
        (
            "broken",
            CatalogAdapter {
                name: "Broken Provider",
                catalog: Catalog::Error("catalog offline"),
                reasoning: None,
                exact_error: None,
                modalities: None,
            },
        ),
        (
            "metadata-broken",
            CatalogAdapter {
                name: "Metadata Broken",
                catalog: Catalog::Models(vec![model("metadata-broken", "listed", "Listed", None)]),
                reasoning: None,
                exact_error: Some("reasoning metadata offline"),
                modalities: None,
            },
        ),
        (
            "duplicate",
            CatalogAdapter {
                name: "Duplicate Provider",
                catalog: Catalog::Models(vec![
                    model("duplicate", "same", "Same", None),
                    model("duplicate", "same", "Same Again", None),
                ]),
                reasoning: None,
                exact_error: None,
                modalities: None,
            },
        ),
        (
            "text-only",
            CatalogAdapter {
                name: "Text Only",
                catalog: Catalog::Models(Vec::new()),
                reasoning: None,
                exact_error: None,
                modalities: Some(vec![ModelModality("text".to_owned())]),
            },
        ),
    ] {
        llm.register_adapter(&[provider.to_owned()], Arc::new(adapter))
            .unwrap();
    }
}

async fn invoke(
    runtime: &PresetApiProxyRuntime,
    method: RpcMethod,
    payload: Value,
) -> RpcResult<Value> {
    runtime
        .unary(
            method,
            RpcRequest::new(RpcId::new("models-test"), payload),
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
async fn directory_groups_valid_providers_contains_failures_and_keeps_unlisted_selection() {
    let harness = Harness::new(
        Some(ModelSelection {
            provider: "deepseek-official".to_owned(),
            model: "private-preview".to_owned(),
            reasoning_effort: Some("max".to_owned()),
        }),
        None,
    );
    let catalog = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionModels,
            json!({ "sessionId": harness.agent.id() }),
        )
        .await,
    );
    assert_eq!(catalog["current"]["model"], "private-preview");
    assert_eq!(catalog["current"]["reasoningEffort"], "max");
    assert_eq!(catalog["routable"], true);
    assert_eq!(catalog["groups"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["groups"][0]["id"], "deepseek-official");
    assert_eq!(catalog["groups"][0]["models"].as_array().unwrap().len(), 2);
    assert_eq!(catalog["failures"].as_array().unwrap().len(), 3);
    assert!(
        catalog["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|failure| {
                failure["id"] == "duplicate"
                    && failure["message"].as_str().unwrap().contains("duplicate")
            })
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One chronology proves selection, assembly, request, and persistence ordering.
async fn live_default_logged_override_and_selection_epoch_are_exact() {
    let saved = Arc::new(Mutex::new(Vec::new()));
    let reject_save = Arc::new(Mutex::new(false));
    let save: SaveDefaultModelSelection = {
        let saved = saved.clone();
        let reject_save = reject_save.clone();
        Arc::new(move |selection| {
            saved.lock().push(selection);
            let reject = *reject_save.lock();
            async move {
                if reject {
                    anyhow::bail!("read-only document");
                }
                Ok(())
            }
            .boxed()
        })
    };
    let harness = Harness::new(None, Some(save));
    let initial = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionModels,
            json!({ "sessionId": harness.agent.id() }),
        )
        .await,
    );
    assert_eq!(initial["current"]["model"], "deepseek-chat");
    harness.defaults.lock().model = "deepseek-reasoner".to_owned();
    let moved_default = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionModels,
            json!({ "sessionId": harness.agent.id() }),
        )
        .await,
    );
    assert_eq!(moved_default["current"]["model"], "deepseek-reasoner");

    harness
        .prompt
        .assemble(assemble_context_for(&harness.agent, None))
        .await
        .unwrap();
    let seed = LlmCallConfig {
        provider: ProviderId::new("seed"),
        model: ModelId::new("seed"),
        reasoning_effort: None,
        temperature: Some(0.2),
        max_tokens: None,
        stop: None,
    };
    let selected = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionSelectModel,
            json!({
                "sessionId": harness.agent.id(),
                "provider": "deepseek-official",
                "model": "private-preview",
                "reasoningEffort": "max"
            }),
        )
        .await,
    );
    assert_eq!(selected["selected"]["reasoningEffort"], "max");
    let before: LlmCallConfig = AgentEvents::new(harness.context.clone(), harness.agent.clone())
        .waterfall("agent/request", (), {
            let seed = seed.clone();
            move || async move { Ok(seed) }
        })
        .await
        .unwrap();
    assert_eq!(before.model.as_str(), "deepseek-reasoner");
    harness
        .prompt
        .assemble(assemble_context_for(&harness.agent, None))
        .await
        .unwrap();
    let after: LlmCallConfig = AgentEvents::new(harness.context.clone(), harness.agent.clone())
        .waterfall("agent/request", (), move || async move { Ok(seed) })
        .await
        .unwrap();
    assert_eq!(after.model.as_str(), "private-preview");
    assert_eq!(after.reasoning_effort.as_ref().unwrap().as_str(), "max");
    assert_eq!(saved.lock().len(), 1);

    let unsupported = invoke(
        &harness.runtime,
        RpcMethod::SessionSelectModel,
        json!({
            "sessionId": harness.agent.id(),
            "provider": "deepseek-official",
            "model": "private-preview",
            "reasoningEffort": "medium"
        }),
    )
    .await;
    assert!(
        matches!(unsupported, RpcResult::Failure { ref error } if error.code == "model-unavailable")
    );
    assert_eq!(saved.lock().len(), 1);
    *reject_save.lock() = true;
    let still_selected = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionSelectModel,
            json!({
                "sessionId": harness.agent.id(),
                "provider": "deepseek-official",
                "model": "deepseek-chat"
            }),
        )
        .await,
    );
    assert_eq!(still_selected["selected"]["reasoningEffort"], "high");
    assert_eq!(saved.lock().len(), 2);
}

#[tokio::test]
async fn text_only_selection_is_blocked_until_visible_images_are_replaced() {
    let harness = Harness::new(None, None);
    let image = ContentBlock::Image {
        attachment: ImageAttachmentRef {
            attachment_id: AttachmentId::new("att-history"),
            media_type: ImageMediaType::Png,
            bytes: 1,
            width: 1,
            height: 1,
            name: None,
        },
    };
    let event = harness
        .agent
        .session()
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![image],
                seekdeep_llm::MessageSource::user(),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let blocked = invoke(
        &harness.runtime,
        RpcMethod::SessionSelectModel,
        json!({
            "sessionId": harness.agent.id(),
            "provider": "text-only",
            "model": "plain"
        }),
    )
    .await;
    assert!(
        matches!(blocked, RpcResult::Failure { ref error } if error.code == "model-unavailable")
    );

    harness
        .agent
        .session()
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "image summarized".to_owned(),
                }],
                seekdeep_llm::MessageSource::plugin("compact"),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(0, event.seq)),
                source_event_seqs: Some(vec![event.seq]),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let accepted = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionSelectModel,
            json!({
                "sessionId": harness.agent.id(),
                "provider": "text-only",
                "model": "plain"
            }),
        )
        .await,
    );
    assert_eq!(accepted["selected"]["model"], "plain");
}

#[tokio::test]
async fn prompt_validates_the_whole_image_batch_before_save_and_attachment_reads_are_authorized() {
    let harness = Harness::new(None, None);
    let operations = Arc::new(Mutex::new(Vec::new()));
    Arc::new(AttachmentStore::new(Arc::new(MemoryAttachments {
        limits: ImageAttachmentLimits {
            max_image_bytes: 4,
            max_images_per_message: 2,
            max_message_image_bytes: 4,
            max_image_pixels: 4,
            media_types: vec![ImageMediaType::Png],
        },
        operations: operations.clone(),
    })))
    .provide(&harness.context)
    .unwrap();

    let prompted = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionPrompt,
            json!({
                "sessionId": harness.agent.id(),
                "mode": "queue",
                "content": [
                    { "type": "image", "mediaType": "image/png", "data": "AQ==", "name": "first.png" },
                    { "type": "text", "text": "compare" },
                    { "type": "image", "mediaType": "image/png", "data": "Ag==" }
                ]
            }),
        )
        .await,
    );
    assert_eq!(prompted, json!({ "accepted": true }));
    assert_eq!(
        *operations.lock(),
        ["validate:1", "validate:2", "save:1", "save:2"]
    );
    let pending = harness.agent.inbox().next_turn();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].content()[0].block_type(), "image");
    assert_eq!(pending[0].content()[1].block_type(), "text");

    let allowed = value(
        invoke(
            &harness.runtime,
            RpcMethod::SessionAttachment,
            json!({ "sessionId": harness.agent.id(), "attachmentId": "att-1" }),
        )
        .await,
    );
    assert_eq!(allowed["attachment"]["attachmentId"], "att-1");
    assert_eq!(allowed["data"], "AQ==");
    let denied = invoke(
        &harness.runtime,
        RpcMethod::SessionAttachment,
        json!({ "sessionId": harness.agent.id(), "attachmentId": "att-other" }),
    )
    .await;
    assert!(matches!(
        denied,
        RpcResult::Failure { ref error }
            if error.code == "attachment-error"
                && error.details["reason"] == "ATTACHMENT_NOT_REFERENCED"
    ));

    let too_many = invoke(
        &harness.runtime,
        RpcMethod::SessionPrompt,
        json!({
            "sessionId": harness.agent.id(),
            "mode": "queue",
            "content": [
                { "type": "image", "mediaType": "image/png", "data": "AQ==" },
                { "type": "image", "mediaType": "image/png", "data": "AQ==" },
                { "type": "image", "mediaType": "image/png", "data": "AQ==" }
            ]
        }),
    )
    .await;
    assert!(matches!(
        too_many,
        RpcResult::Failure { ref error }
            if error.code == "attachment-error" && error.details["reason"] == "TOO_MANY_IMAGES"
    ));
    assert_eq!(operations.lock().len(), 4);
}
