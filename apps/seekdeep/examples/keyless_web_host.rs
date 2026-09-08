//! Real Web profile with isolated settings, fixture attachment, and a model-stream guard.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use seekdeep::profile_boot::{
    boot_profile, compose_profile_at, framework_profile_catalog, shipped_preset_root,
};
use seekdeep_app_boot::BootPrepare;
use seekdeep_cmdline::{CmdlineHost, provide_cmdline};
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionId;
use seekdeep_core::session_store::{CreateSessionOptions, SESSIONS};
use seekdeep_host_webserver::{
    WEB_SERVER, WebRegistration, WebRoute, WebRouteKind, WebServer, response,
};
use seekdeep_llm::{
    AbortSignal, AdapterStream, GenerateOptions, LLM, LlmAdapter, LlmModelContext, LlmModelInfo,
    LlmProviderInfo, LlmResolvedModelInfo, LlmStream, ModelId, ProviderId,
};
use seekdeep_typert_loader::TypertArtifactRegistry;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
    SEEKDEEP_LAUNCH_ENVIRONMENT, create_launch_environment_snapshot,
};
use seekdeep_workspace::WORKSPACE_REGISTRY;
use serde_json::json;

struct RouteOnly(Arc<AtomicUsize>);

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
fn sessions_fixture_route(
    context: &Context,
    server: &WebServer,
) -> anyhow::Result<WebRegistration> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Sessions"))?;
    server.register(WebRoute {
        kind: WebRouteKind::Exact,
        path: "/fixture/sessions".to_owned(),
        handler: Arc::new(move |request| {
            let sessions = sessions.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    request.method().as_str() == "GET",
                    "fixture Session listing requires GET"
                );
                let listed = sessions
                    .list()
                    .iter()
                    .map(|session| json!({"header": session.header(), "events": session.events()}))
                    .collect::<Vec<_>>();
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
                    data: json!({}), source_event_seqs: None, surface_op: None, ignorable: None,
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
) -> anyhow::Result<()> {
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
fn install_scaffold_context_routes(
    context: &Context,
    server: &WebServer,
    routes: &mut Vec<WebRegistration>,
) -> anyhow::Result<()> {
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
    install_keyless_routes(context, &calls, mode)?;
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
    if replay.is_some() {
        settings_routes.push(idle_fixture_route(context, &server)?);
    }
    install_scaffold_context_routes(context, &server, &mut settings_routes)?;
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
