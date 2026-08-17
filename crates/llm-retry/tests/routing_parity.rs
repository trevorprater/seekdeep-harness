//! Provider routing and serving-registration policy through the shipping loop.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, AgentRegistry};
use seekdeep_agent_loop::{AgentLoopServices, LoopAgent};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{
    AdapterStream, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmCallConfig,
    LlmError, LlmFailure, LlmRuntime, MessageSource, ProviderId, ResolvedRetryPolicy, StreamChunk,
    UserMessage, resolve_retry_policy,
};
use seekdeep_llm_retry::{RetryConfig, RetryId, RetryInternals, install_with_internals};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};

#[derive(Clone, Debug)]
enum Response {
    Failure { message: String, code: String },
    Success(String),
}

#[derive(Debug)]
struct RoutedAdapter {
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
    responses: Mutex<VecDeque<Response>>,
    policies: Vec<(String, ResolvedRetryPolicy)>,
}

#[async_trait]
impl LlmAdapter for RoutedAdapter {
    fn provider_retry_policy(&self, provider: &str) -> Option<ResolvedRetryPolicy> {
        self.policies
            .iter()
            .find(|(route, _)| route == provider)
            .map(|(_, policy)| policy.clone())
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let response = self
            .responses
            .lock()
            .pop_front()
            .expect("scripted response");
        let chunks = match response {
            Response::Failure { message, code } => vec![Ok(StreamChunk::Finish {
                reason: FinishReason::Error {
                    failure: LlmFailure {
                        message,
                        code,
                        status: None,
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                },
                replay_state: None,
            })],
            Response::Success(text) => vec![
                Ok(StreamChunk::TextDelta { index: 0, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ],
        };
        AdapterStream::new(stream::iter(chunks))
    }
}

#[derive(Debug)]
struct BlockingFailureAdapter {
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
    policy: ResolvedRetryPolicy,
    in_band: bool,
}

#[async_trait]
impl LlmAdapter for BlockingFailureAdapter {
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        Some(self.policy.clone())
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let entered = self.entered.clone();
        let release = self.release.clone();
        let in_band = self.in_band;
        AdapterStream::new(stream::once(async move {
            entered.add_permits(1);
            release.acquire().await.unwrap().forget();
            if in_band {
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Error {
                        failure: LlmFailure {
                            message: "old route auth failed".to_owned(),
                            code: "AUTH".to_owned(),
                            status: None,
                            provider_retry_after_ms: None,
                            request_id: None,
                        },
                    },
                    replay_state: None,
                })
            } else {
                Err(anyhow::Error::new(LlmError::simple(
                    "old route auth failed",
                    "AUTH",
                )))
            }
        }))
    }
}

fn always(delay_ms: f64) -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&serde_json::json!({
            "mode":"always",
            "backoff":{
                "initialDelayMs":delay_ms,
                "maxDelayMs":delay_ms,
                "jitterRatio":0
            }
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn normal() -> ResolvedRetryPolicy {
    resolve_retry_policy(None, "retryPolicy").unwrap()
}

fn normal_one_retry() -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&serde_json::json!({
            "mode":"normal",
            "maxRetries":1,
            "retryableCodes":["SERVER"],
            "backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":0}
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn install_services(context: &Context) -> (Arc<LlmRuntime>, AgentLoopServices) {
    let llm = LlmRuntime::install(context).unwrap();
    let system_prompt = SystemPrompt::new(context, SystemPromptConfig::default()).unwrap();
    let tools =
        ToolRuntime::new_with_system_prompt(context, &system_prompt, ToolRuntimeConfig::default())
            .unwrap();
    let services = AgentLoopServices {
        llm: llm.clone(),
        system_prompt,
        tools,
        max_parallel_tool_calls: 10,
    };
    (llm, services)
}

fn create_agent(
    context: &Context,
    services: AgentLoopServices,
    id: &str,
    provider: &str,
) -> (Arc<Session>, LoopAgent) {
    let session = Session::create(&SessionId::new(id), None, None).unwrap();
    let (agent, _driver) = LoopAgent::new_default(
        context,
        &session,
        AgentOptions {
            provider: Some(provider.into()),
            model: Some("model".into()),
            max_tokens: None,
        },
        None,
        services,
    )
    .unwrap();
    (session, agent)
}

async fn wait_for_retry(session: &Session, count: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if session
                .events()
                .iter()
                .filter(|event| event.event_type == "llm/retry")
                .count()
                >= count
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn install_retry(context: &Context) -> Arc<seekdeep_cordis::PluginFiber> {
    let minted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let minted_ids = minted.clone();
    let plugin = install_with_internals(
        context,
        RetryConfig::default(),
        RetryInternals::new(
            || 0.5,
            move || {
                RetryId::new(format!(
                    "routing-chain-{}",
                    minted_ids.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
                ))
            },
        ),
    )
    .unwrap();
    plugin.await_settled().await.unwrap();
    plugin
}

#[tokio::test]
async fn selects_policy_from_the_failed_request_provider() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let retry = install_retry(&context).await;
    let (llm, services) = install_services(&context);
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned(), "other".to_owned()],
        Arc::new(RoutedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([
                Response::Failure {
                    message: "mock auth failed".to_owned(),
                    code: "AUTH".to_owned(),
                },
                Response::Failure {
                    message: "other auth failed".to_owned(),
                    code: "AUTH".to_owned(),
                },
                Response::Success("other recovered".to_owned()),
            ])),
            policies: vec![
                ("mock".to_owned(), normal()),
                ("other".to_owned(), always(1.0)),
            ],
        }),
    )
    .unwrap();

    let (normal_session, normal_agent) =
        create_agent(&context, services.clone(), "routing-normal", "mock");
    normal_agent.agent.followup(user("normal")).unwrap();
    normal_agent.agent.when_idle().unwrap().await;
    assert!(
        !normal_session
            .events()
            .iter()
            .any(|event| event.event_type == "llm/retry")
    );

    let (always_session, always_agent) =
        create_agent(&context, services, "routing-always", "other");
    always_agent.agent.followup(user("always")).unwrap();
    always_agent.agent.when_idle().unwrap().await;
    let retry_event = always_session
        .events()
        .into_iter()
        .find(|event| event.event_type == "llm/retry")
        .unwrap();
    assert_eq!(retry_event.data["provider"], "other");
    assert_eq!(retry_event.data["mode"], "always");
    assert_eq!(retry_event.data["retry"], 1);
    assert_eq!(
        requests
            .lock()
            .iter()
            .map(|request| request.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["mock", "other", "other"]
    );

    retry.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn agent_request_rerouting_selects_the_rerouted_provider_policy() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let retry = install_retry(&context).await;
    context
        .events()
        .on_waterfall(
            &context,
            "agent/request",
            move |_, _, next| {
                Box::pin(async move {
                    let reply = next.run().await?;
                    let mut config = reply
                        .downcast::<LlmCallConfig>()
                        .map(|config| (*config).clone())
                        .ok_or_else(|| anyhow::anyhow!("agent/request did not return config"))?;
                    config.provider = ProviderId::new("other");
                    Ok(EventReply::Value(Arc::new(config)))
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    let (llm, services) = install_services(&context);
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned(), "other".to_owned()],
        Arc::new(RoutedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([
                Response::Failure {
                    message: "rerouted auth failed".to_owned(),
                    code: "AUTH".to_owned(),
                },
                Response::Success("rerouted recovery".to_owned()),
            ])),
            policies: vec![("other".to_owned(), always(1.0))],
        }),
    )
    .unwrap();
    let (session, agent) = create_agent(&context, services, "routing-waterfall", "mock");
    agent.agent.followup(user("reroute")).unwrap();
    agent.agent.when_idle().unwrap().await;

    assert_eq!(
        requests
            .lock()
            .iter()
            .map(|request| request.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["other", "other"]
    );
    let retry_event = session
        .events()
        .into_iter()
        .find(|event| event.event_type == "llm/retry")
        .unwrap();
    assert_eq!(retry_event.data["provider"], "other");
    assert_eq!(retry_event.data["mode"], "always");

    retry.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn one_step_keeps_finite_budgets_separate_when_request_routing_changes_provider() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let retry = install_retry(&context).await;
    let (llm, services) = install_services(&context);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_route = requests.clone();
    context
        .events()
        .on_waterfall(
            &context,
            "agent/request",
            move |_, _, next| {
                let requests = requests_for_route.clone();
                Box::pin(async move {
                    let reply = next.run().await?;
                    let mut config = reply
                        .downcast::<LlmCallConfig>()
                        .map(|config| (*config).clone())
                        .ok_or_else(|| anyhow::anyhow!("agent/request did not return config"))?;
                    config.provider = ProviderId::new(if requests.lock().is_empty() {
                        "mock"
                    } else {
                        "other"
                    });
                    Ok(EventReply::Value(Arc::new(config)))
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    llm.register_adapter(
        &["mock".to_owned(), "other".to_owned()],
        Arc::new(RoutedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([
                Response::Failure {
                    message: "mock failed".to_owned(),
                    code: "SERVER".to_owned(),
                },
                Response::Failure {
                    message: "other failed".to_owned(),
                    code: "SERVER".to_owned(),
                },
                Response::Success("other recovered".to_owned()),
            ])),
            policies: vec![
                ("mock".to_owned(), normal_one_retry()),
                ("other".to_owned(), normal_one_retry()),
            ],
        }),
    )
    .unwrap();
    let (session, agent) = create_agent(&context, services, "routing-provider-budgets", "mock");
    agent.agent.followup(user("switch provider")).unwrap();
    agent.agent.when_idle().unwrap().await;

    assert_eq!(
        requests
            .lock()
            .iter()
            .map(|request| request.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["mock", "other", "other"]
    );
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| event.event_type == "llm/retry")
            .map(|event| (
                event.data["provider"].as_str().unwrap(),
                event.data["retry"].as_u64().unwrap(),
            ))
            .collect::<Vec<_>>(),
        vec![("mock", 1), ("other", 1)]
    );

    retry.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn no_final_adapter_has_no_policy_and_does_not_retry() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let retry = install_retry(&context).await;
    let (_llm, services) = install_services(&context);
    let (session, agent) = create_agent(&context, services, "routing-missing", "missing");
    agent.agent.followup(user("missing route")).unwrap();
    agent.agent.when_idle().unwrap().await;

    assert!(
        !session
            .events()
            .iter()
            .any(|event| event.event_type == "llm/retry")
    );
    let end = session.events().into_iter().last().unwrap();
    assert_eq!(end.event_type, "turn/end");
    assert_eq!(
        end.data.pointer("/reason/error/code").unwrap(),
        "NO_ADAPTER"
    );

    retry.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

async fn assert_in_flight_policy_capture(in_band: bool, session_id: &str) {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let retry = install_retry(&context).await;
    let (llm, services) = install_services(&context);
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let old_requests = Arc::new(Mutex::new(Vec::new()));
    let old_registration = llm
        .register_adapter(
            &["mock".to_owned()],
            Arc::new(BlockingFailureAdapter {
                entered: entered.clone(),
                release: release.clone(),
                requests: old_requests.clone(),
                policy: always(1.0),
                in_band,
            }),
        )
        .unwrap();
    let (session, agent) = create_agent(&context, services, session_id, "mock");
    agent.agent.followup(user("replace in flight")).unwrap();
    entered.acquire().await.unwrap().forget();

    old_registration.dispose().await.unwrap();
    let replacement_requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(RoutedAdapter {
            requests: replacement_requests.clone(),
            responses: Mutex::new(VecDeque::from([
                Response::Failure {
                    message: "replacement failed".to_owned(),
                    code: "AUTH".to_owned(),
                },
                Response::Success("replacement recovered".to_owned()),
            ])),
            policies: vec![("mock".to_owned(), always(3.0))],
        }),
    )
    .unwrap();
    release.add_permits(1);
    wait_for_retry(&session, 2).await;
    agent.agent.when_idle().unwrap().await;

    let events = session
        .events()
        .into_iter()
        .filter(|event| event.event_type == "llm/retry")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data["retry"], 1);
    assert_eq!(events[0].data["delayMs"], 1.0);
    assert_eq!(events[1].data["retry"], 1);
    assert_eq!(events[1].data["delayMs"], 3.0);
    assert_ne!(events[0].data["policyKey"], events[1].data["policyKey"]);
    assert_ne!(events[0].data["retryId"], events[1].data["retryId"]);
    assert_eq!(old_requests.lock().len(), 1);
    assert_eq!(replacement_requests.lock().len(), 2);
    assert!(session.derive_messages().iter().any(|message| {
        message.content().iter().any(
            |block| matches!(block, ContentBlock::Text { text } if text == "replacement recovered"),
        )
    }));

    retry.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn thrown_and_in_band_failures_keep_their_serving_policy_across_route_replacement() {
    assert_in_flight_policy_capture(false, "routing-replacement-thrown").await;
    assert_in_flight_policy_capture(true, "routing-replacement-in-band").await;
}
