//! Real Web profile with isolated settings, fixture attachment, and a model-stream guard.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::StreamExt as _;
use seekdeep::profile_boot::{
    boot_profile, compose_profile_at, framework_profile_catalog, shipped_preset_root,
};
use seekdeep_agent::{
    AGENTS, AgentHandle, AgentOptions, CancelOptions, CreateAgentMeta, CreateAgentOptions,
};
use seekdeep_agent_presets::AGENT_PRESETS;
use seekdeep_app_boot::BootPrepare;
use seekdeep_cmdline::{CmdlineHost, provide_cmdline};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{AgentCancelCause, Session, SessionEvent, SessionId};
use seekdeep_core::session_store::{CreateSessionOptions, SESSIONS};
use seekdeep_host_webserver::{
    WEB_SERVER, WebRegistration, WebRoute, WebRouteKind, WebServer, response,
};
use seekdeep_llm::{
    AbortSignal, AdapterRegistrationHandle, AdapterStream, GenerateOptions, LLM, LlmAdapter,
    LlmModelContext, LlmModelInfo, LlmProviderInfo, LlmResolvedModelInfo, LlmStream, ModelId,
    ProviderId, StreamChunk, UserMessage,
};
use seekdeep_subagent::{
    ContinuableStartRequest, ContinuableStartSpec, SUBAGENTS, SubagentFollowupOptions,
};
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_typert_loader::TypertArtifactRegistry;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
    SEEKDEEP_LAUNCH_ENVIRONMENT, create_launch_environment_snapshot,
};
use seekdeep_workspace::WORKSPACE_REGISTRY;
use serde_json::json;

struct RouteOnly(Arc<AtomicUsize>);

/// Providers whose model calls the refusal middleware lets through: the source scaffold's
/// in-process `ctx.llm.registerAdapter(...)` test adapters, registered here over a fixture route.
type AllowedProviders = Arc<Mutex<HashSet<String>>>;

/// Driver adapters registered through the fixture route, by id, with their providers.
type AdapterRegistrations = Arc<Mutex<HashMap<String, (AdapterRegistrationHandle, Vec<String>)>>>;

/// A source test adapter living in the driver process: every `stream(options)` becomes one HTTP
/// request carrying the serialized `GenerateOptions`; the driver answers with one JSON stream
/// chunk per line (`{"error": ...}` for a thrown adapter), and an aborted turn drops the request.
struct DriverAdapter {
    endpoint: String,
    client: reqwest::Client,
}

fn aborted() -> anyhow::Error {
    anyhow::anyhow!("driver adapter: the turn was aborted")
}

async fn driver_response(
    client: reqwest::Client,
    endpoint: String,
    options: &GenerateOptions,
) -> anyhow::Result<reqwest::Response> {
    let request = client
        .post(&endpoint)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(options)?)
        .send();
    let response = match &options.signal {
        Some(signal) => tokio::select! {
            response = request => response?,
            () = signal.cancelled() => return Err(aborted()),
        },
        None => request.await?,
    };
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("driver adapter HTTP {}", response.status()));
    }
    Ok(response)
}

async fn driver_bytes(
    body: &mut (impl futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Option<bytes::Bytes>> {
    let next = match signal {
        Some(signal) => tokio::select! {
            next = body.next() => next,
            () = signal.cancelled() => return Err(aborted()),
        },
        None => body.next().await,
    };
    Ok(next.transpose()?)
}

fn driver_chunks(
    client: reqwest::Client,
    endpoint: String,
    options: GenerateOptions,
) -> impl futures::Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static {
    async_stream::try_stream! {
        let signal = options.signal.clone();
        let response = driver_response(client, endpoint, &options).await?;
        let mut body = response.bytes_stream();
        let mut buffer = Vec::new();
        while let Some(bytes) = driver_bytes(&mut body, signal.as_ref()).await? {
            buffer.extend_from_slice(&bytes);
            while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=end).collect::<Vec<u8>>();
                let line = String::from_utf8(line)?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let value: serde_json::Value = serde_json::from_str(line)?;
                if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
                    Err(anyhow::anyhow!("{error}"))?;
                }
                yield serde_json::from_value::<StreamChunk>(value)?;
            }
        }
    }
}

#[async_trait]
impl LlmAdapter for DriverAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(driver_chunks(
            self.client.clone(),
            self.endpoint.clone(),
            options,
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FixtureMode {
    RouteOnly,
    MissingCredential,
    Replay,
}

#[async_trait]
impl LlmAdapter for RouteOnly {
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo {
            id: ProviderId::new(provider),
            name: "DeepSeek".to_owned(),
        }
    }

    async fn list_models(&self, provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        Ok(vec![LlmModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new("deepseek-v4-flash"),
            name: "DeepSeek-V4-Flash".to_owned(),
            description: None,
            input_modalities: None,
        }])
    }

    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        Ok(LlmResolvedModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new(model),
            name: if model == "deepseek-v4-flash" {
                "DeepSeek-V4-Flash"
            } else {
                model
            }
            .to_owned(),
            description: None,
            input_modalities: None,
            context: (model == "deepseek-v4-flash").then_some(LlmModelContext {
                context_window: 128_000,
            }),
            default_max_tokens: None,
            reasoning: None,
        })
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        self.0.fetch_add(1, Ordering::SeqCst);
        AdapterStream::new(futures::stream::once(async {
            anyhow::bail!("keyless Web scenario issued a model call without a replay fixture")
        }))
    }
}

/// The driver pins `SEEKDEEP_TELEMETRY_DISABLED`; a scenario pinning a telemetry backend
/// disclosure unsets it and patches the row to a local dead endpoint instead.
fn telemetry_disabled() -> bool {
    std::env::var_os(seekdeep::profile_boot::TELEMETRY_DISABLED_ENV).is_some()
}

fn write_overlay(home: &Path, data: &Path, mode: FixtureMode) -> anyhow::Result<PathBuf> {
    let overlay = data.join("keyless.patch.yml");
    std::fs::write(
        &overlay,
        serde_json::to_string_pretty(&json!([
            {"id":"webserver","config":{"host":"127.0.0.1","port":0}},
            {"id":"web-runtime","config":{"printUrl":false,"surfaceContext":true}},
            {"id":"llm-deepseek","disabled":mode != FixtureMode::MissingCredential},
            {"id":"agent-instructions","disabled":true},
            {"id":"session-title-llm","disabled":true},
            {"id":"session-telemetry-otel","disabled":telemetry_disabled()},
            {"id":"settings","config":{"seekdeepHome":home}},
            {"id":"credentials","config":{"seekdeepHome":home}},
            {"id":"storage-json","config":{"root":data.join("storages")}},
            {"id":"session-persistence-jsonl","config":{"root":data.join("sessions")}},
            {"id":"session-query-sqlite","config":{"path":":memory:","openAt":"first-search"}},
            {"id":"agent-presets","config":{"default":"standard","roots":[{"path":shipped_preset_root(),"trust":"system"}],"includeUserRoot":false}},
            {"id":"skill-filesystem","config":{"seekdeepHome":home,"agentsHome":home.join("agents"),"bundledSkillDir":home.join("bundled-skills"),"watch":false}},
            {"id":"directory-picker","disabled":true},
            {"insert":[
                {"id":"directory-picker-browse","name":"@seekdeep-ai/seekdeep-host-directory-picker-browse"},
                {"id":"ui-directory-picker-browse","name":"@seekdeep-ai/seekdeep-client-ui-directory-picker-browse"}
            ]}
        ]))?,
    )?;
    Ok(overlay)
}

fn settings_fixture_routes(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<Vec<WebRegistration>> {
    let agent_loop = context
        .get(seekdeep_agent_loop::AGENT_LOOP)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agent Loop"))?;
    let cap = server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/agent-loop-cap".to_owned(),
        handler: Arc::new(move |_| {
            let agent_loop = agent_loop.clone();
            Box::pin(async move {
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&agent_loop.max_parallel_tool_calls())?,
                ))
            })
        }),
    })?;
    let loader = context
        .get(seekdeep_loader::LOADER)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Loader"))?;
    let count = server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/plugin-count".to_owned(),
        handler: Arc::new(move |_| {
            let loader = loader.clone();
            Box::pin(async move {
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(
                        &loader
                            .entries()?
                            .iter()
                            .filter(|entry| !entry.group)
                            .count(),
                    )?,
                ))
            })
        }),
    })?;
    Ok(vec![
        cap,
        count,
        session_fixture_route(context, server)?,
        sessions_fixture_route(context, server)?,
        cold_blank_fixture_route(context, server)?,
    ])
}

/// Lists every live Session with its header and events: the browser driver's stand-in for the
/// source scaffold's in-process `ctx.on('session/event')` taps and `ctx.sessions.list()`.
/// Every event appended to a Session the Host has hosted, kept past detach.
///
/// Source `ctx.on('session/event')` observes each in-process append, including the
/// last events of a continuable child whose Agent disposes and detaches its
/// Session before the driver's next listing poll; the ledger lets the listing
/// keep delivering those sessions after they leave the live registry.
#[derive(Default)]
struct SessionLedger {
    entries: BTreeMap<String, LedgerEntry>,
}

struct LedgerEntry {
    header: serde_json::Value,
    events: BTreeMap<u64, SessionEvent>,
}

impl SessionLedger {
    /// Records one Session's header and events. Callers read the Session before taking the
    /// ledger lock: the `session/event` listener runs inside the appending Session, so a
    /// listing that held the ledger while reading a Session could deadlock against it.
    fn record(
        &mut self,
        id: String,
        header: serde_json::Value,
        events: impl IntoIterator<Item = SessionEvent>,
    ) {
        let entry = self.entries.entry(id).or_insert_with(|| LedgerEntry {
            header: header.clone(),
            events: BTreeMap::new(),
        });
        entry.header = header;
        for event in events {
            entry.events.insert(event.seq, event);
        }
    }
}

fn sessions_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Sessions"))?;
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    let ledger: Arc<Mutex<SessionLedger>> = Arc::default();
    let recorder = ledger.clone();
    let listener = context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let (Some(session), Some(event)) =
                (args.get::<Session>(0), args.get::<SessionEvent>(1))
            else {
                return Ok(EventReply::Undefined);
            };
            let id = session.id().to_string();
            let header = json!(session.header());
            recorder.lock().expect("session ledger poisoned").record(
                id,
                header,
                [event.as_ref().clone()],
            );
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;
    let listener = Arc::new(listener);
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/sessions".to_owned(),
        handler: Arc::new(move |request| {
            let sessions = sessions.clone();
            let agents = agents.clone();
            let ledger = ledger.clone();
            let _listener = listener.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "GET",
                    "fixture Session listing requires GET"
                );
                // Source `ctx.agents.get(id)` answers only for live Agents; the listing carries
                // each Session's live Agent status so the driver can tell the two apart.
                // Read every live Session before touching the ledger (see `record`).
                let live = sessions
                    .list()
                    .iter()
                    .map(|session| {
                        (
                            session.id().to_string(),
                            json!(session.header()),
                            session.events(),
                            agents.get(session.id()).map(|agent| {
                                json!({
                                    "status": agent.status(),
                                    "inbox": {"nextTurn": agent.inbox().next_turn()},
                                })
                            }),
                        )
                    })
                    .collect::<Vec<_>>();
                let mut ledger = ledger.lock().expect("session ledger poisoned");
                for (id, header, events, _) in &live {
                    ledger.record(id.clone(), header.clone(), events.iter().cloned());
                }
                let mut listed = live
                    .iter()
                    .map(|(_, header, events, agent)| {
                        json!({
                            "header": header,
                            "events": events,
                            "agent": agent,
                        })
                    })
                    .collect::<Vec<_>>();
                let live_ids = live
                    .iter()
                    .map(|(id, ..)| id.clone())
                    .collect::<HashSet<_>>();
                listed.extend(
                    ledger
                        .entries
                        .iter()
                        .filter(|(id, _)| !live_ids.contains(id.as_str()))
                        .map(|(_, entry)| {
                            json!({
                                "header": entry.header,
                                "events": entry.events.values().collect::<Vec<_>>(),
                                "agent": serde_json::Value::Null,
                            })
                        }),
                );
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&listed)?))
            })
        }),
    })
}

/// Reads one JSON request body.
async fn json_body(
    request: seekdeep_host_webserver::WebRequest,
) -> anyhow::Result<serde_json::Value> {
    let body = http_body_util::BodyExt::collect(request.into_body())
        .await?
        .to_bytes();
    Ok(serde_json::from_slice(&body)?)
}

fn required_str<'a>(value: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("fixture request omitted {key}"))
}

// The source scaffold hands cases its in-process `ctx.tools.execute` with the live Agent of an
// open Session; the Rust Host exposes the same call over a fixture route.
fn tool_execute_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let agents = context
        .get(seekdeep_agent::AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    let tools = context
        .get(seekdeep_tools::TOOLS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Tools"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/tool/execute".to_owned(),
        handler: Arc::new(move |request| {
            let agents = agents.clone();
            let tools = tools.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture tool execution requires POST"
                );
                let body = json_body(request).await?;
                let agent = agents
                    .get(&SessionId::new(required_str(&body, "sessionId")?))
                    .ok_or_else(|| anyhow::anyhow!("fixture tool execution: Agent absent"))?;
                let mut input = seekdeep_tools::ToolExecutionInput::new(
                    seekdeep_llm::CallId::new(required_str(&body, "callId")?),
                    required_str(&body, "name")?,
                    body.get("arguments").cloned().unwrap_or(json!({})),
                    AbortSignal::default(),
                );
                input.agent = Some(agent);
                let result = tools.execute(input).await;
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&json!({
                        "isError": result.is_error(),
                        "content": result.content(),
                        "meta": result.meta(),
                        "value": result.value(),
                    }))?,
                ))
            })
        }),
    })
}

// Source: `scaffold.ctx.sessionPersistence.create(header)` + `append(id, events)`: the body is one
// complete log whose header (origin, parent, depth, preset) is persisted as written.
fn persist_fixture_route(context: &Context, server: &WebServer) -> anyhow::Result<WebRegistration> {
    let persistence = context
        .get(seekdeep_session_persistence::SESSION_PERSISTENCE)
        .ok_or_else(|| anyhow::anyhow!("fixture has no persistence"))?
        .persistence();
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/persist".to_owned(),
        handler: Arc::new(move |request| {
            let persistence = persistence.clone();
            Box::pin(async move {
                // Source `ctx.sessionPersistence.load(id)`: the durable header and event log.
                if let Some(id) = request
                    .uri()
                    .path()
                    .strip_prefix("/fixture/persist/")
                    .and_then(|rest| rest.strip_suffix("/load"))
                {
                    anyhow::ensure!(
                        request.method().as_str() == "GET",
                        "fixture persist load requires GET"
                    );
                    // The source rejects the load Promise with the persistence error; the
                    // driver's shim asserts on the status and quotes the text.
                    return Ok(match persistence.load(&SessionId::new(id)).await {
                        Ok(loaded) => response(
                            200_u16.try_into()?,
                            serde_json::to_vec(
                                &json!({"meta": loaded.meta, "events": loaded.events}),
                            )?,
                        ),
                        Err(error) => {
                            response(400_u16.try_into()?, format!("{error:#}").into_bytes())
                        }
                    });
                }
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture persist requires POST"
                );
                let body = http_body_util::BodyExt::collect(request.into_body())
                    .await?
                    .to_bytes();
                let text = String::from_utf8(body.to_vec())?;
                let header_line = text
                    .lines()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("persist fixture header absent"))?;
                // The source header line carries the `type: "session"` discriminator.
                let mut header_value: serde_json::Value = serde_json::from_str(header_line)?;
                if let Some(object) = header_value.as_object_mut() {
                    object.remove("type");
                }
                let header: seekdeep_core::session::SessionHeader =
                    serde_json::from_value(header_value)?;
                let events = seekdeep_llm_replay::parse_session_log(&text)?;
                let id = header.id.clone();
                persistence.create(&header).await?;
                if !events.is_empty() {
                    persistence.append(&id, &events).await?;
                }
                persistence.inspect(&id, None).await?;
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&header)?))
            })
        }),
    })
}

// Source: `scaffold.ctx.jobs.kill(jobId, agent, reason)`.
fn job_kill_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let agents = context
        .get(seekdeep_agent::AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    let jobs = context
        .get(seekdeep_jobs::JOBS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Jobs"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/job/kill".to_owned(),
        handler: Arc::new(move |request| {
            let agents = agents.clone();
            let jobs = jobs.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture job kill requires POST"
                );
                let body = json_body(request).await?;
                let agent = agents
                    .get(&SessionId::new(required_str(&body, "sessionId")?))
                    .ok_or_else(|| anyhow::anyhow!("fixture job kill: Agent absent"))?;
                let outcome = jobs.kill(
                    &seekdeep_jobs::JobId::new(required_str(&body, "jobId")?),
                    Some(&agent),
                    body.get("reason").and_then(serde_json::Value::as_str),
                )?;
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&outcome)?))
            })
        }),
    })
}

// Source: `await scaffold.ctx.credentials.set(ref, value)`.
fn credential_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let credentials = context
        .get(seekdeep_credentials::CREDENTIALS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Credentials"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/credential".to_owned(),
        handler: Arc::new(move |request| {
            let credentials = credentials.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture credential requires POST"
                );
                let body = json_body(request).await?;
                credentials
                    .set(
                        &seekdeep_credentials::CredentialRef::new(required_str(&body, "ref")?),
                        required_str(&body, "value")?,
                    )
                    .await?;
                Ok(response(200_u16.try_into()?, b"{}".to_vec()))
            })
        }),
    })
}

// Source: `vi.spyOn(scaffold.ctx.apiProxy.host, 'openPath').mockImplementation(...)` replaces the
// Host method behind `/api/host.openPath` and records its payloads. The exact route shadows the
// API prefix route for the same wire request; GET lists the recorded payloads.
fn open_path_stub_fixture_route(
    server: &WebServer,
) -> anyhow::Result<(WebRegistration, WebRegistration)> {
    let calls: Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let stub = server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/api/host.openPath".to_owned(),
        handler: Arc::new(move |request| {
            let recorded = recorded.clone();
            Box::pin(async move {
                let body = json_body(request).await?;
                let rpc_id = body
                    .get("rpcId")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                recorded
                    .lock()
                    .map_err(|_| anyhow::anyhow!("open-path stub poisoned"))?
                    .push(
                        body.get("payload")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&json!({
                        "type": "server-response",
                        "rpcId": rpc_id,
                        "result": {"ok": true, "value": {"opened": true}},
                    }))?,
                ))
            })
        }),
    })?;
    let listing = server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/open-path".to_owned(),
        handler: Arc::new(move |request| {
            let calls = calls.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "GET",
                    "open-path listing requires GET"
                );
                let listed = calls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("open-path stub poisoned"))?
                    .clone();
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&listed)?))
            })
        }),
    })?;
    Ok((stub, listing))
}

fn cold_blank_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let persistence = context
        .get(seekdeep_session_persistence::SESSION_PERSISTENCE)
        .ok_or_else(|| anyhow::anyhow!("fixture has no persistence"))?
        .persistence();
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Sessions"))?;
    let cwd = std::env::current_dir()?.join("cold-blank-workspace");
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/cold-blank".to_owned(),
        handler: Arc::new(move |request| {
            let persistence = persistence.clone(); let sessions = sessions.clone(); let cwd = cwd.clone();
            Box::pin(async move {
                anyhow::ensure!(request.method().as_str() == "POST", "cold blank fixture requires POST");
                tokio::fs::create_dir_all(&cwd).await?;
                let mut header = seekdeep_core::session::SessionHeader::new(SessionId::new("cold-blank-session-web-e2e"));
                header.created_at = header.created_at.saturating_sub(60_000);
                header.cwd = Some(cwd.to_string_lossy().into_owned()); header.delegation_depth = Some(0);
                persistence.create(&header).await?;
                persistence.append(&header.id, &[seekdeep_core::session::SessionEvent {
                    event_type: "session/end-seed".to_owned(), seq: 0, time: header.created_at.try_into()?,
                    data: json!({}).into(), source_event_seqs: None, surface_op: None, ignorable: None,
                }]).await?;
                let location = persistence.locate(&header).ok_or_else(|| anyhow::anyhow!("blank fixture has no artifact"))?;
                let size = tokio::fs::metadata(&location.path).await?.len();
                let listed = persistence.list(None).await?.iter().any(|candidate| candidate.id == header.id);
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&json!({
                    "size":size,"listed":listed,"cold":sessions.get(&header.id).is_none(),
                    "compressed":location.path.extension().is_some_and(|extension| extension == "zstd")
                }))?))
            })
        }),
    })
}

fn session_fixture_route(context: &Context, server: &WebServer) -> anyhow::Result<WebRegistration> {
    let owner = context.clone();
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Sessions"))?;
    let defaults = context
        .get(seekdeep_agent_default_model::AGENT_DEFAULT_MODEL)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agent default"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/session".to_owned(),
        handler: Arc::new(move |request| {
            let owner = owner.clone();
            let sessions = sessions.clone();
            let defaults = defaults.clone();
            Box::pin(async move {
                let id = request
                    .uri()
                    .path()
                    .strip_prefix("/fixture/session/")
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("fixture session id absent"))?;
                // Source: `scaffold.ctx.sessions.list()[i].append(type, data)` on a live Session.
                if let Some(id) = id.strip_suffix("/append") {
                    anyhow::ensure!(
                        request.method().as_str() == "POST",
                        "fixture session append requires POST"
                    );
                    let session = sessions
                        .get(&SessionId::new(id))
                        .ok_or_else(|| anyhow::anyhow!("fixture session absent"))?;
                    let body = json_body(request).await?;
                    // Source: `append(type, data, { surfaceOp, sourceEventSeqs, ignorable })`.
                    let options = seekdeep_core::session::AppendOptions {
                        surface_op: body
                            .get("surfaceOp")
                            .map(|value| serde_json::from_value(value.clone()))
                            .transpose()?,
                        source_event_seqs: body
                            .get("sourceEventSeqs")
                            .map(|value| serde_json::from_value(value.clone()))
                            .transpose()?,
                        ignorable: body
                            .get("ignorable")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    };
                    session.append(
                        required_str(&body, "type")?,
                        body.get("data").cloned().unwrap_or(json!({})),
                        options,
                    )?;
                    return Ok(response(
                        200_u16.try_into()?,
                        serde_json::to_vec(&session.events())?,
                    ));
                }
                let id = SessionId::new(id);
                let session = if request.method().as_str() == "POST" {
                    sessions.create(&owner, Some(id), CreateSessionOptions::default())?
                } else {
                    sessions
                        .get(&id)
                        .ok_or_else(|| anyhow::anyhow!("fixture session absent"))?
                };
                if request.method().as_str() == "PUT" {
                    // The source default-model scenario seeds a logged route without a model call.
                    let selection = defaults.current_selection();
                    let mut config = json!({"provider":selection.provider,"model":selection.model});
                    if let Some(effort) = selection.reasoning_effort {
                        config["reasoningEffort"] = json!(effort);
                    }
                    session.append(
                        "request/header",
                        json!({"header":{"config":config},"reason":"initial"}),
                        seekdeep_core::session::AppendOptions::default(),
                    )?;
                }
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&session.events())?,
                ))
            })
        }),
    })
}

fn seed_log_fixture_route(
    context: &Context,
    server: &WebServer,
    path: PathBuf,
    id: SessionId,
    workspace: PathBuf,
) -> anyhow::Result<WebRegistration> {
    let persistence = context
        .get(seekdeep_session_persistence::SESSION_PERSISTENCE)
        .ok_or_else(|| anyhow::anyhow!("fixture has no persistence"))?
        .persistence();
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/seed-log".to_owned(),
        handler: Arc::new(move |request| {
            let persistence = persistence.clone();
            let path = path.clone();
            let id = id.clone();
            let workspace = workspace.clone();
            Box::pin(async move {
                let result: anyhow::Result<_> = async {
                    anyhow::ensure!(
                        request.method().as_str() == "POST",
                        "fixture seed requires POST"
                    );
                    let query = request.uri().query().map(str::to_owned);
                    let id = if request.uri().path() == "/fixture/seed-log" {
                        id
                    } else {
                        SessionId::new(
                            request
                                .uri()
                                .path()
                                .strip_prefix("/fixture/seed-log/")
                                .filter(|suffix| !suffix.is_empty() && !suffix.contains('/'))
                                .ok_or_else(|| anyhow::anyhow!("invalid fixture Session path"))?,
                        )
                    };
                    // A non-empty body is the fixture text itself: the source scaffold's
                    // `seedSession(scaffold, fixtureText, id)` for generated recordings.
                    let body = http_body_util::BodyExt::collect(request.into_body())
                        .await?
                        .to_bytes();
                    let text = if body.is_empty() {
                        tokio::fs::read_to_string(path).await?
                    } else {
                        String::from_utf8(body.to_vec())?
                    };
                    let mut raw = text
                        .replace("{{sessionId}}", id.as_str())
                        .replace("{{cwd}}", &workspace.to_string_lossy());
                    let fixture_header: serde_json::Value = serde_json::from_str(
                        raw.lines()
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("fixture header absent"))?,
                    )?;
                    // The source `seedSession` keeps the realized events under its own header,
                    // so a recording minted under another id seeds fine; only the cwd is rebased.
                    if let Some(cwd) = fixture_header.get("cwd") {
                        let cwd = cwd
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("fixture cwd must be a string"))?;
                        raw = raw.replace(cwd, &workspace.to_string_lossy());
                    }
                    let events = seekdeep_llm_replay::parse_session_log(&raw)?;
                    let mut header = seekdeep_core::session::SessionHeader::new(id.clone());
                    header.created_at = header.created_at.saturating_sub(60_000);
                    header.cwd = Some(workspace.to_string_lossy().into_owned());
                    header.delegation_depth = Some(0);
                    // Source `seedSession(scaffold, text, id, agentPreset)`: the seeded Session
                    // records the preset it was created under.
                    if let Some(preset) = query
                        .as_deref()
                        .and_then(|query| query.strip_prefix("agentPreset="))
                        .filter(|preset| !preset.is_empty())
                    {
                        header.agent_preset = Some(preset.to_owned());
                    }
                    anyhow::ensure!(
                        events
                            .last()
                            .is_some_and(|event| event.event_type == "turn/end"),
                        "fixture has no closed final turn"
                    );
                    persistence.create(&header).await?;
                    persistence.append(&id, &events).await?;
                    persistence.inspect(&id, None).await?;
                    let persisted = persistence
                        .read_raw(&id, None)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("fixture has no raw artifact"))?;
                    anyhow::ensure!(
                        serde_json::to_value(seekdeep_llm_replay::parse_session_log(
                            &persisted.content
                        )?)? == serde_json::to_value(&events)?,
                        "fixture raw persisted events differ"
                    );
                    Ok(response(
                        200_u16.try_into()?,
                        serde_json::to_vec(&json!({"events":events.len()}))?,
                    ))
                }
                .await;
                result.inspect_err(|error| eprintln!("fixture seed failed: {error:#}"))
            })
        }),
    })
}

fn attach_seed_fixture_route(
    context: &Context,
    server: &WebServer,
    workspace: PathBuf,
    seed: SessionId,
) -> anyhow::Result<WebRegistration> {
    let registry = context
        .get(WORKSPACE_REGISTRY)
        .ok_or_else(|| anyhow::anyhow!("Web profile has no workspace registry"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/attach-seed".to_owned(),
        handler: Arc::new(move |request| {
            let registry = registry.clone();
            let workspace = workspace.clone();
            let seed = seed.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture attachment requires POST"
                );
                registry
                    .resolve_by_path(&workspace.to_string_lossy())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("fixture workspace is not registered"))?
                    .attach_session(seed)
                    .await?;
                Ok(response(200_u16.try_into()?, "attached"))
            })
        }),
    })
}

fn isolated_environment(home: &Path) -> LaunchEnvironmentSnapshot {
    let pinned = [
        (
            "SEEKDEEP_HOME".to_owned(),
            home.to_string_lossy().into_owned(),
        ),
        (
            "SEEKDEEP_AGENTS_HOME".to_owned(),
            home.join("agents").to_string_lossy().into_owned(),
        ),
    ];
    let telemetry = telemetry_disabled().then(|| {
        (
            seekdeep::profile_boot::TELEMETRY_DISABLED_ENV.to_owned(),
            "1".to_owned(),
        )
    });
    create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: pinned
            .into_iter()
            .chain(telemetry)
            .collect::<BTreeMap<_, _>>(),
    }])
}

fn idle_fixture_route(context: &Context, server: &WebServer) -> anyhow::Result<WebRegistration> {
    let agents = context
        .get(seekdeep_agent::AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Sessions"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/idle".to_owned(),
        handler: Arc::new(move |request| {
            let agents = agents.clone();
            let sessions = sessions.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "idle barrier requires POST"
                );
                let id = SessionId::new(
                    request
                        .uri()
                        .path()
                        .strip_prefix("/fixture/idle/")
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| anyhow::anyhow!("idle Session id absent"))?,
                );
                let agent = agents
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("idle Agent absent"))?;
                agent.when_idle()?.await?;
                let session = sessions
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("idle Session absent"))?;
                sessions.flush(&session).await?;
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&session.events())?,
                ))
            })
        }),
    })
}

fn install_fixture_replay(
    context: &Context,
    file: &Path,
    override_file: Option<PathBuf>,
    child_files: Vec<PathBuf>,
) -> anyhow::Result<seekdeep_llm_replay::ReplayHandle> {
    seekdeep_llm_replay::install_llm_replay(
        context,
        seekdeep_llm_replay::ReplayConfig {
            file: file.to_path_buf(),
            override_file,
            child_files,
            // Source scaffold `replayContextWindow`: the replay provider's advertised window.
            providers: serde_json::from_value(json!([{
                "id":"deepseek-official","name":"DeepSeek",
                "models":[{"id":"deepseek-v4-flash","name":"DeepSeek-V4-Flash","contextWindow":match std::env::var("SEEKDEEP_KEYLESS_REPLAY_CONTEXT_WINDOW") {
                    Ok(value) => value.parse::<u64>()?,
                    Err(_) => 128_000,
                }}]
            }]))?,
            // Source scaffold `paceMs`: the scenario's own replay pacing.
            pace_ms: match std::env::var("SEEKDEEP_KEYLESS_REPLAY_PACE_MS") {
                Ok(value) => value.parse()?,
                Err(_) => 5.0,
            },
        },
    )
}

async fn finish_fixture(
    application: seekdeep::profile_boot::ProfileBootApplication,
    replay: Option<seekdeep_llm_replay::ReplayHandle>,
    calls: &AtomicUsize,
    data: &Path,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    let mut audit = json!({"calls":calls.load(Ordering::SeqCst)});
    if let Some(replay) = replay {
        let consumed = replay.assert_consumed();
        audit["replayConsumed"] = json!(consumed.is_ok());
        if let Err(error) = consumed {
            errors.push(error);
        }
        if let Err(error) = replay.dispose().await {
            errors.push(error);
        }
    }
    if let Err(error) = application.dispose().await {
        errors.push(error);
    }
    if let Err(error) = std::fs::write(
        data.join("model-call-audit.json"),
        serde_json::to_vec(&audit)?,
    ) {
        errors.push(error.into());
    }
    if calls.load(Ordering::SeqCst) != 0 {
        errors.push(anyhow::anyhow!(
            "keyless Web scenario made a non-replay model call"
        ));
    }
    anyhow::ensure!(
        errors.is_empty(),
        "fixture teardown: {}",
        errors
            .iter()
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>()
            .join("; ")
    );
    Ok(())
}

/// Mirrors the source scaffold's prepended `approval/request` listener: with
/// `SEEKDEEP_KEYLESS_AUTO_APPROVE=allowed-once`, every approval resolves allowed-once before any
/// browser answerer sees it, so a policy scenario drives escalations without a composer gesture.
fn install_auto_approval(context: &Context) -> anyhow::Result<()> {
    let Ok(policy) = std::env::var("SEEKDEEP_KEYLESS_AUTO_APPROVE") else {
        return Ok(());
    };
    anyhow::ensure!(
        policy == "allowed-once",
        "SEEKDEEP_KEYLESS_AUTO_APPROVE only supports allowed-once"
    );
    context.events().on_waterfall(
        context,
        "approval/request",
        |_, _, _| {
            Box::pin(async {
                Ok(seekdeep_cordis::EventReply::Value(Arc::new(
                    seekdeep_user_approval::ApprovalAnswer::Outcome(
                        seekdeep_user_approval::ApprovalOutcome::AllowedOnce,
                    ),
                )))
            })
        },
        seekdeep_cordis::EventOptions {
            prepend: true,
            global: true,
        },
    )?;
    Ok(())
}

fn install_keyless_routes(
    context: &Context,
    calls: &Arc<AtomicUsize>,
    mode: FixtureMode,
    allowed: &AllowedProviders,
) -> anyhow::Result<()> {
    let allowed = allowed.clone();
    let llm = context
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("Web profile has no llm"))?;
    if mode == FixtureMode::RouteOnly {
        llm.register_adapter(
            &["deepseek-official".to_owned()],
            Arc::new(RouteOnly(calls.clone())),
        )?;
    }
    let calls = calls.clone();
    llm.register_stream_middleware(
        context,
        Arc::new(move |options, next| {
            if mode == FixtureMode::Replay
                && options.provider.as_str() == "deepseek-official"
                && options.model.as_str() == "deepseek-v4-flash"
            {
                return next(options);
            }
            if allowed
                .lock()
                .expect("allowed providers")
                .contains(options.provider.as_str())
            {
                return next(options);
            }
            calls.fetch_add(1, Ordering::SeqCst);
            LlmStream::new(futures::stream::once(async {
                anyhow::bail!("keyless Web fixture refuses model calls on every provider")
            }))
        }),
        true,
    )?;
    Ok(())
}

/// Replay fixture, the source scaffold's `replayOverride` sidecar, and `replayChildFixtures`.
type ReplayFiles = (PathBuf, Option<PathBuf>, Vec<PathBuf>);

fn replay_files(arguments: &[std::ffi::OsString]) -> anyhow::Result<Option<ReplayFiles>> {
    let Some(path) = arguments.get(7) else {
        return Ok(None);
    };
    let override_file = arguments
        .get(8)
        .filter(|value| value.to_string_lossy() != "-")
        .map(|value| Path::new(value).canonicalize())
        .transpose()?;
    let child_files = arguments
        .iter()
        .skip(9)
        .map(|value| Path::new(value).canonicalize())
        .collect::<Result<Vec<_>, _>>()?;
    // Source: an override-only replay names a fixture path that never exists; the override
    // document carries every script.
    let fixture = Path::new(path);
    let fixture = if fixture.exists() {
        fixture.canonicalize()?
    } else {
        std::path::absolute(fixture)?
    };
    Ok(Some((fixture, override_file, child_files)))
}

/// Routes standing in for the source scaffold's in-process `ctx` calls (tools, jobs,
/// credentials) and its `vi.spyOn(apiProxy.host, 'openPath')` stub.
/// Source `ctx.llm.registerAdapter(providers, adapter)` for a driver-hosted adapter: the driver
/// posts the providers and its stream endpoint; the returned id later unregisters the adapter.
fn adapter_fixture_route(
    context: &Context,
    server: &WebServer,
    allowed: &AllowedProviders,
) -> anyhow::Result<WebRegistration> {
    let llm = context
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("fixture has no llm"))?;
    let allowed = allowed.clone();
    let registrations: AdapterRegistrations = Arc::default();
    let counter = Arc::new(AtomicUsize::new(0));
    let client = reqwest::Client::new();
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/adapter".to_owned(),
        handler: Arc::new(move |request| {
            let llm = llm.clone();
            let allowed = allowed.clone();
            let registrations = registrations.clone();
            let counter = counter.clone();
            let client = client.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture adapter routes require POST"
                );
                let action = request.uri().path().to_owned();
                let body = json_body(request).await?;
                if action == "/fixture/adapter/register" {
                    let providers = body
                        .get("providers")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| anyhow::anyhow!("fixture adapter omitted providers"))?
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let endpoint = required_str(&body, "endpoint")?.to_owned();
                    let handle = llm.register_adapter(
                        &providers,
                        Arc::new(DriverAdapter { endpoint, client }),
                    )?;
                    allowed
                        .lock()
                        .expect("allowed providers")
                        .extend(providers.iter().cloned());
                    let id = format!("adapter-{}", counter.fetch_add(1, Ordering::SeqCst) + 1);
                    registrations
                        .lock()
                        .expect("adapter registrations")
                        .insert(id.clone(), (handle, providers));
                    return Ok(response(
                        200_u16.try_into()?,
                        serde_json::to_vec(&json!({"id": id}))?,
                    ));
                }
                anyhow::ensure!(
                    action == "/fixture/adapter/unregister",
                    "unknown fixture adapter route {action}"
                );
                let id = required_str(&body, "id")?;
                let removed = registrations
                    .lock()
                    .expect("adapter registrations")
                    .remove(id);
                let Some((handle, providers)) = removed else {
                    anyhow::bail!("fixture adapter {id} is not registered");
                };
                handle.dispose().await?;
                let mut allowed = allowed.lock().expect("allowed providers");
                for provider in providers {
                    allowed.remove(&provider);
                }
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&json!({}))?,
                ))
            })
        }),
    })
}

/// Source `ctx.agents.create({ sessionId, meta, agentOptions, setup })`, `handle.dispose()`,
/// `handle.agent.followup(message)`, and `ctx.agents.roots()`.
/// Source `ctx.agents.roots()`: the live root Agents' session ids.
fn agent_roots_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/agents/roots".to_owned(),
        handler: Arc::new(move |_request| {
            let agents = agents.clone();
            Box::pin(async move {
                let roots = agents
                    .roots()
                    .iter()
                    .map(|agent| agent.session().id().to_string())
                    .collect::<Vec<_>>();
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&roots)?))
            })
        }),
    })
}

/// Live agents created through the fixture route, kept for `handle.dispose()`.
type FixtureAgentHandles = Arc<Mutex<HashMap<String, AgentHandle>>>;

async fn create_fixture_agent(
    agents: &seekdeep_agent::AgentRegistry,
    roster: &Arc<seekdeep_agent_presets::AgentPresetRegistry>,
    handles: &FixtureAgentHandles,
    body: &serde_json::Value,
) -> anyhow::Result<SessionId> {
    let session_id = SessionId::new(required_str(body, "sessionId")?);
    let mut options = CreateAgentOptions::new(session_id.clone());
    let agent_preset = body
        .pointer("/meta/agentPreset")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    options.meta = CreateAgentMeta {
        cwd: body
            .pointer("/meta/cwd")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        agent_preset: agent_preset.clone(),
        ..CreateAgentMeta::default()
    };
    options.agent_options = AgentOptions {
        provider: body
            .pointer("/agentOptions/provider")
            .and_then(serde_json::Value::as_str)
            .map(ProviderId::new),
        model: body
            .pointer("/agentOptions/model")
            .and_then(serde_json::Value::as_str)
            .map(ModelId::new),
        ..AgentOptions::default()
    };
    if body.get("setup").and_then(serde_json::Value::as_str) == Some("agentPresets") {
        let roster = roster.clone();
        // Source: `ctx.agentPresets.mount(agentCtx, id?)`; the id is the meta's preset.
        options.setup = Some(Arc::new(move |agent_context| {
            let roster = roster.clone();
            let agent_preset = agent_preset.clone();
            Box::pin(async move {
                roster
                    .mount(&agent_context, agent_preset.as_deref())
                    .await?;
                Ok(None)
            })
        }));
    }
    let handle = agents.create(options).await?;
    handles
        .lock()
        .expect("agent handles")
        .insert(session_id.to_string(), handle);
    Ok(session_id)
}

async fn fixture_agent_action(
    agents: &seekdeep_agent::AgentRegistry,
    handles: &FixtureAgentHandles,
    id: &str,
    action: &str,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    let live = || {
        agents
            .get(&SessionId::new(id))
            .ok_or_else(|| anyhow::anyhow!("fixture agent {id} is not live"))
    };
    match action {
        "dispose" => {
            let removed = handles.lock().expect("agent handles").remove(id);
            let Some(handle) = removed else {
                anyhow::bail!("fixture agent {id} was not created here");
            };
            handle.dispose().await?;
        }
        "followup" => {
            let message: UserMessage = serde_json::from_value(
                body.get("message")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("followup omitted message"))?,
            )?;
            live()?.followup(message)?;
        }
        "cancel" => {
            let cause: AgentCancelCause = body
                .get("cause")
                .cloned()
                .map_or(Ok(AgentCancelCause::User), serde_json::from_value)?;
            live()?.cancel(cause, CancelOptions::default())?;
        }
        other => anyhow::bail!("unknown fixture agent action {other}"),
    }
    Ok(())
}

fn agent_fixture_route(context: &Context, server: &WebServer) -> anyhow::Result<WebRegistration> {
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    let roster = context
        .get(AGENT_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no agent presets"))?;
    let handles: FixtureAgentHandles = Arc::default();
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/agent".to_owned(),
        handler: Arc::new(move |request| {
            let agents = agents.clone();
            let roster = roster.clone();
            let handles = handles.clone();
            Box::pin(async move {
                let path = request.uri().path().to_owned();
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture agent routes require POST"
                );
                let body = json_body(request).await?;
                if path == "/fixture/agent/create" {
                    let session_id =
                        create_fixture_agent(&agents, &roster, &handles, &body).await?;
                    return Ok(response(
                        200_u16.try_into()?,
                        serde_json::to_vec(&json!({"sessionId": session_id}))?,
                    ));
                }
                let rest = path
                    .strip_prefix("/fixture/agent/")
                    .ok_or_else(|| anyhow::anyhow!("unknown fixture agent route {path}"))?;
                let (id, action) = rest
                    .rsplit_once('/')
                    .ok_or_else(|| anyhow::anyhow!("fixture agent route needs an action"))?;
                fixture_agent_action(&agents, &handles, id, action, &body).await?;
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&json!({}))?,
                ))
            })
        }),
    })
}

/// Source `ctx.workspaceRegistry.resolveByPath(path)` and `workspace.attachSession(id)`.
fn workspace_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let registry = context
        .get(WORKSPACE_REGISTRY)
        .ok_or_else(|| anyhow::anyhow!("fixture has no workspace registry"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/workspace".to_owned(),
        handler: Arc::new(move |request| {
            let registry = registry.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture workspace routes require POST"
                );
                let action = request.uri().path().to_owned();
                let body = json_body(request).await?;
                let workspace = registry
                    .resolve_by_path(required_str(&body, "path")?)
                    .await?;
                match action.as_str() {
                    "/fixture/workspace/resolve" => Ok(response(
                        200_u16.try_into()?,
                        serde_json::to_vec(
                            &workspace.map(|workspace| json!({"id": workspace.id()})),
                        )?,
                    )),
                    "/fixture/workspace/attach" => {
                        let workspace = workspace.ok_or_else(|| {
                            anyhow::anyhow!("fixture workspace is not registered")
                        })?;
                        workspace
                            .attach_session(SessionId::new(required_str(&body, "sessionId")?))
                            .await?;
                        Ok(response(
                            200_u16.try_into()?,
                            serde_json::to_vec(&json!({}))?,
                        ))
                    }
                    other => anyhow::bail!("unknown fixture workspace route {other}"),
                }
            })
        }),
    })
}

/// The source wire shape of one `subagent.list` entry: activity from the live registry,
/// camelCase fields, the mode's label lifted beside it.
fn wire_subagent_entry(
    entry: seekdeep_subagent::SubagentListEntry,
    agents: &seekdeep_agent::AgentRegistry,
) -> serde_json::Value {
    match entry {
        seekdeep_subagent::SubagentListEntry::Child {
            id,
            mode,
            has_children,
            ..
        } => {
            let running = agents
                .get(&id)
                .is_some_and(|agent| agent.status() == seekdeep_agent::AgentStatus::Running);
            let (mode, label) = match mode {
                seekdeep_subagent::SubagentListMode::OneShot { label } => ("one-shot", label),
                seekdeep_subagent::SubagentListMode::Continuable { label } => {
                    ("continuable", Some(label))
                }
            };
            json!({
                "kind": "child",
                "id": id,
                "mode": mode,
                "activity": if running { "running" } else { "inactive" },
                "hasChildren": has_children,
                "label": label,
            })
        }
        seekdeep_subagent::SubagentListEntry::Diagnostic { id, reason } => {
            json!({"kind": "diagnostic", "id": id, "reason": reason})
        }
    }
}

/// Source `ctx.subagents.startContinuable(spec)` request body.
fn continuable_start_spec(
    parent: Arc<seekdeep_agent::Agent>,
    body: &serde_json::Value,
) -> anyhow::Result<ContinuableStartSpec> {
    Ok(ContinuableStartSpec {
        provider: required_str(body, "provider")?.to_owned(),
        label: required_str(body, "label")?.to_owned(),
        request: ContinuableStartRequest {
            prompt: serde_json::from_value(body.get("prompt").cloned().unwrap_or(json!([])))?,
            parent,
            agent_options: body
                .get("agentOptions")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?,
            max_depth: body.get("maxDepth").and_then(serde_json::Value::as_u64),
            tool_filter: None,
            persona: body
                .get("persona")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        },
        signal: AbortSignal::default(),
    })
}

/// Source `ctx.subagents.startContinuable(spec)`, `listChildren(parentId)`, and
/// `followup(parent, childId, content, options)`.
fn subagent_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no subagents"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/subagent".to_owned(),
        handler: Arc::new(move |request| {
            let agents = agents.clone();
            let subagents = subagents.clone();
            Box::pin(async move {
                let path = request.uri().path().to_owned();
                if let Some(parent) = path.strip_prefix("/fixture/subagent/children/") {
                    let listed = subagents
                        .list_children(&SessionId::new(parent), None)
                        .await?
                        .into_iter()
                        .map(|entry| wire_subagent_entry(entry, &agents))
                        .collect::<Vec<_>>();
                    return Ok(response(200_u16.try_into()?, serde_json::to_vec(&listed)?));
                }
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture subagent routes require POST"
                );
                let body = json_body(request).await?;
                let parent_id = SessionId::new(required_str(&body, "parentSessionId")?);
                let parent = agents
                    .get(&parent_id)
                    .ok_or_else(|| anyhow::anyhow!("fixture parent {parent_id} is not live"))?;
                match path.as_str() {
                    "/fixture/subagent/start" => {
                        let started = subagents
                            .start_continuable(continuable_start_spec(parent, &body)?)
                            .await?;
                        Ok(response(
                            200_u16.try_into()?,
                            serde_json::to_vec(&json!({
                                "childId": started.child_id,
                                "messageId": started.message_id,
                            }))?,
                        ))
                    }
                    "/fixture/subagent/followup" => {
                        let message_id = subagents
                            .followup(
                                &parent,
                                &SessionId::new(required_str(&body, "childSessionId")?),
                                serde_json::from_value(
                                    body.get("content").cloned().unwrap_or(json!([])),
                                )?,
                                SubagentFollowupOptions {
                                    source: serde_json::from_value(
                                        body.get("source")
                                            .cloned()
                                            .unwrap_or(json!({"kind": "user"})),
                                    )?,
                                    signal: AbortSignal::default(),
                                },
                            )
                            .await?;
                        Ok(response(
                            200_u16.try_into()?,
                            serde_json::to_vec(&json!({"messageId": message_id}))?,
                        ))
                    }
                    other => anyhow::bail!("unknown fixture subagent route {other}"),
                }
            })
        }),
    })
}

/// Source `ctx.sessionProjectionCache.coldSnapshot(id)`: warms the durable projection checkpoint
/// of a persisted-only session so listings carry its projections.
fn cold_snapshot_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let cache = context
        .get(seekdeep_session_projection_cache::SESSION_PROJECTION_CACHE)
        .ok_or_else(|| anyhow::anyhow!("fixture has no session projection cache"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/cold-snapshot".to_owned(),
        handler: Arc::new(move |request| {
            let cache = cache.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture cold snapshot requires POST"
                );
                let id = request
                    .uri()
                    .path()
                    .strip_prefix("/fixture/cold-snapshot/")
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("fixture cold snapshot needs a session id"))?;
                let snapshot = cache.cold_snapshot(&SessionId::new(id), None).await?;
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&json!({"asOfSeq": snapshot.as_of_seq}))?,
                ))
            })
        }),
    })
}

/// Source `ctx.systemPrompt.section({ name, order, text })` and its disposer.
fn prompt_section_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("fixture has no system prompt"))?;
    let owner = context.clone();
    let handles: Arc<Mutex<HashMap<String, seekdeep_cordis::fiber::EffectHandle>>> = Arc::default();
    let counter = Arc::new(AtomicUsize::new(0));
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/prompt/section".to_owned(),
        handler: Arc::new(move |request| {
            let prompt = prompt.clone();
            let owner = owner.clone();
            let handles = handles.clone();
            let counter = counter.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture prompt section requires POST"
                );
                let path = request.uri().path().to_owned();
                if let Some(id) = path
                    .strip_prefix("/fixture/prompt/section/")
                    .and_then(|rest| rest.strip_suffix("/dispose"))
                {
                    let removed = handles.lock().expect("prompt sections").remove(id);
                    let Some(handle) = removed else {
                        anyhow::bail!("fixture prompt section {id} is not registered");
                    };
                    handle.dispose().await?;
                    return Ok(response(
                        200_u16.try_into()?,
                        serde_json::to_vec(&json!({}))?,
                    ));
                }
                let body = json_body(request).await?;
                let order = body
                    .get("order")
                    .and_then(serde_json::Value::as_f64)
                    .ok_or_else(|| anyhow::anyhow!("prompt section omitted order"))?;
                let handle = prompt.section(
                    &owner,
                    PromptSection::new(
                        required_str(&body, "name")?,
                        order,
                        required_str(&body, "text")?,
                    ),
                )?;
                let id = format!("section-{}", counter.fetch_add(1, Ordering::SeqCst) + 1);
                handles
                    .lock()
                    .expect("prompt sections")
                    .insert(id.clone(), handle);
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&json!({"id": id}))?,
                ))
            })
        }),
    })
}

/// Source `ctx.agentPresets.serviceFor(agent, name)` for the `fs` and `compaction` services,
/// and `ctx.tools.schemas(agent)`.
fn preset_service_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Agents"))?;
    let roster = context
        .get(AGENT_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no agent presets"))?;
    let tools = context
        .get(seekdeep_tools::TOOLS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no tools"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/preset".to_owned(),
        handler: Arc::new(move |request| {
            let agents = agents.clone();
            let roster = roster.clone();
            let tools = tools.clone();
            Box::pin(async move {
                let path = request.uri().path().to_owned();
                if path == "/fixture/preset/roots" {
                    return Ok(response(
                        200_u16.try_into()?,
                        serde_json::to_vec(roster.roots())?,
                    ));
                }
                if let Some(id) = path.strip_prefix("/fixture/preset/tool-schemas/") {
                    let agent = agents
                        .get(&SessionId::new(id))
                        .ok_or_else(|| anyhow::anyhow!("fixture agent {id} is not live"))?;
                    let schemas = tools.schemas(Some(agent.scope_key()));
                    return Ok(response(200_u16.try_into()?, serde_json::to_vec(&schemas)?));
                }
                anyhow::ensure!(
                    request.method().as_str() == "POST" && path == "/fixture/preset/service",
                    "unknown fixture preset route {path}"
                );
                let body = json_body(request).await?;
                let id = required_str(&body, "sessionId")?;
                let agent = agents
                    .get(&SessionId::new(id))
                    .ok_or_else(|| anyhow::anyhow!("fixture agent {id} is not live"))?;
                let value = match required_str(&body, "name")? {
                    "fs" => roster.service_for(&agent, seekdeep_fs::FS).map(|service| {
                        // Source: an absent sandbox mode is an absent property, not null.
                        service
                            .filesystem()
                            .sandbox_mode()
                            .map_or_else(|| json!({}), |mode| json!({"sandboxMode": mode}))
                    }),
                    "compaction" => roster
                        .service_for(&agent, seekdeep_compaction::service::COMPACTION)
                        .map(|_| json!({})),
                    other => anyhow::bail!("fixture preset service {other} is not shimmed"),
                };
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&value)?))
            })
        }),
    })
}

/// Source `ctx.sessions.flush(session)`.
fn session_flush_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Sessions"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/flush".to_owned(),
        handler: Arc::new(move |request| {
            let sessions = sessions.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "POST",
                    "fixture flush requires POST"
                );
                let id = request
                    .uri()
                    .path()
                    .strip_prefix("/fixture/flush/")
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("fixture flush needs a session id"))?;
                let session = sessions
                    .get(&SessionId::new(id))
                    .ok_or_else(|| anyhow::anyhow!("fixture session absent"))?;
                let flushed = sessions.flush(&session).await?;
                Ok(response(200_u16.try_into()?, serde_json::to_vec(&flushed)?))
            })
        }),
    })
}

fn install_scaffold_context_routes(
    context: &Context,
    server: &WebServer,
    routes: &mut Vec<WebRegistration>,
    allowed: &AllowedProviders,
) -> anyhow::Result<()> {
    routes.push(adapter_fixture_route(context, server, allowed)?);
    routes.push(agent_roots_fixture_route(context, server)?);
    routes.push(agent_fixture_route(context, server)?);
    routes.push(workspace_fixture_route(context, server)?);
    routes.push(subagent_fixture_route(context, server)?);
    routes.push(session_flush_fixture_route(context, server)?);
    routes.push(cold_snapshot_fixture_route(context, server)?);
    routes.push(prompt_section_fixture_route(context, server)?);
    routes.push(preset_service_fixture_route(context, server)?);
    routes.push(tool_execute_fixture_route(context, server)?);
    routes.push(persist_fixture_route(context, server)?);
    routes.push(job_kill_fixture_route(context, server)?);
    routes.push(credential_fixture_route(context, server)?);
    if std::env::var_os("SEEKDEEP_KEYLESS_STUB_OPEN_PATH").is_some() {
        let (stub, listing) = open_path_stub_fixture_route(server)?;
        routes.push(stub);
        routes.push(listing);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        arguments.len() >= 3,
        "expected harness-home, workspace, seed id, optional isolated data root, mode, extra overlay, seed log, replay log, replay override (or -), and child replay logs"
    );
    let mode = match arguments
        .get(4)
        .map(|value| value.to_string_lossy())
        .as_deref()
    {
        None | Some("route-only") => FixtureMode::RouteOnly,
        Some("missing-credential") => FixtureMode::MissingCredential,
        Some("replay") => FixtureMode::Replay,
        Some(value) => anyhow::bail!("unknown keyless fixture mode {value:?}"),
    };
    anyhow::ensure!(
        (mode == FixtureMode::Replay) == (arguments.len() >= 8),
        "replay mode requires a replay log"
    );
    let home = PathBuf::from(&arguments[0]).canonicalize()?;
    let workspace = PathBuf::from(&arguments[1]).canonicalize()?;
    let seed = SessionId::new(arguments[2].to_string_lossy());
    let data = arguments.get(3).map_or_else(|| home.clone(), PathBuf::from);
    std::fs::create_dir_all(&data)?;
    std::env::set_current_dir(&workspace)?;
    let environment = isolated_environment(&home);
    let overlay = write_overlay(&home, &data, mode)?;
    let mut overlays = vec![overlay];
    if let Some(extra) = arguments.get(5) {
        overlays.push(PathBuf::from(extra).canonicalize()?);
    }
    let catalog = framework_profile_catalog(&workspace, &home, &environment)?;
    let plan = compose_profile_at(
        "web",
        &overlays,
        &workspace,
        &home,
        &home.join("profiles/.seekdeep-installation/package.json"),
        &shipped_preset_root(),
        telemetry_disabled().then_some("1"),
    )?;
    let prepare: BootPrepare = Arc::new(move |context| {
        let environment = environment.clone();
        Box::pin(async move {
            context.provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(environment))?;
            TypertArtifactRegistry::install(&context)?;
            provide_cmdline(
                &context,
                CmdlineHost::new(Vec::<String>::new(), |code| {
                    anyhow::bail!("keyless Web Host requested exit {code}")
                }),
            )?;
            Ok(())
        })
    });
    let application = boot_profile(plan, &catalog, Some(prepare)).await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let context = application.context();
    let allowed: AllowedProviders = Arc::default();
    install_keyless_routes(context, &calls, mode, &allowed)?;
    install_auto_approval(context)?;
    let replay = if let Some((fixture, override_file, child_files)) = replay_files(&arguments)? {
        match install_fixture_replay(context, &fixture, override_file, child_files) {
            Ok(replay) => Some(replay),
            Err(error) => {
                application.dispose().await?;
                return Err(error);
            }
        }
    } else {
        None
    };
    let server = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("Web profile has no server"))?;
    let mut settings_routes = settings_fixture_routes(context, &server)?;
    settings_routes.push(idle_fixture_route(context, &server)?);
    install_scaffold_context_routes(context, &server, &mut settings_routes, &allowed)?;
    if let Some(path) = arguments.get(6) {
        settings_routes.push(seed_log_fixture_route(
            context,
            &server,
            PathBuf::from(path).canonicalize()?,
            seed.clone(),
            workspace.clone(),
        )?);
    }
    let attach = attach_seed_fixture_route(context, &server, workspace, seed)?;
    println!("seekdeep web: http://127.0.0.1:{}", server.port());
    tokio::signal::ctrl_c().await?;
    attach.dispose();
    for route in settings_routes {
        route.dispose();
    }
    finish_fixture(application, replay, &calls, &data).await
}
