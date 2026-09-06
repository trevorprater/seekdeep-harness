//! Real Web profile with the source scaffold's route-only adapter and fixture attachment.

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

fn write_overlay(home: &Path, data: &Path) -> anyhow::Result<PathBuf> {
    let overlay = data.join("keyless.patch.yml");
    std::fs::write(
        &overlay,
        serde_json::to_string_pretty(&json!([
            {"id":"webserver","config":{"host":"127.0.0.1","port":0}},
            {"id":"web-runtime","config":{"printUrl":false,"surfaceContext":true}},
            {"id":"llm-deepseek","disabled":true},
            {"id":"agent-instructions","disabled":true},
            {"id":"session-title-llm","disabled":true},
            {"id":"session-telemetry-otel","disabled":true},
            {"id":"settings","config":{"seekdeepHome":home}},
            {"id":"credentials","config":{"seekdeepHome":home}},
            {"id":"storage-json","config":{"root":data.join("storages")}},
            {"id":"session-persistence-jsonl","config":{"root":data.join("sessions")}},
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
    let owner = context.clone();
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture has no Sessions"))?;
    let session = server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/fixture/session".to_owned(),
        handler: Arc::new(move |request| {
            let owner = owner.clone();
            let sessions = sessions.clone();
            Box::pin(async move {
                let id = request
                    .uri()
                    .path()
                    .strip_prefix("/fixture/session/")
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("fixture session id absent"))?;
                let id = SessionId::new(id);
                let session = if request.method().as_str() == "POST" {
                    sessions.create(&owner, Some(id), CreateSessionOptions::default())?
                } else {
                    sessions
                        .get(&id)
                        .ok_or_else(|| anyhow::anyhow!("fixture session absent"))?
                };
                Ok(response(
                    200_u16.try_into()?,
                    serde_json::to_vec(&session.events())?,
                ))
            })
        }),
    })?;
    Ok(vec![count, session])
}

fn isolated_environment(home: &Path) -> LaunchEnvironmentSnapshot {
    create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([
            (
                "SEEKDEEP_HOME".to_owned(),
                home.to_string_lossy().into_owned(),
            ),
            (
                "SEEKDEEP_AGENTS_HOME".to_owned(),
                home.join("agents").to_string_lossy().into_owned(),
            ),
            ("SEEKDEEP_TELEMETRY_DISABLED".to_owned(), "1".to_owned()),
        ]),
    }])
}

fn install_keyless_routes(context: &Context, calls: &Arc<AtomicUsize>) -> anyhow::Result<()> {
    let llm = context
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("Web profile has no llm"))?;
    llm.register_adapter(
        &["deepseek-official".to_owned()],
        Arc::new(RouteOnly(calls.clone())),
    )?;
    let calls = calls.clone();
    llm.register_stream_middleware(
        context,
        Arc::new(move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            LlmStream::new(futures::stream::once(async {
                anyhow::bail!("keyless Web fixture refuses model calls on every provider")
            }))
        }),
        true,
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        matches!(arguments.len(), 3 | 4),
        "expected harness-home, workspace, seed id, and optional isolated data root"
    );
    let home = PathBuf::from(&arguments[0]).canonicalize()?;
    let workspace = PathBuf::from(&arguments[1]).canonicalize()?;
    let seed = SessionId::new(arguments[2].to_string_lossy());
    let data = arguments.get(3).map_or_else(|| home.clone(), PathBuf::from);
    std::fs::create_dir_all(&data)?;
    std::env::set_current_dir(&workspace)?;
    let environment = isolated_environment(&home);
    let overlay = write_overlay(&home, &data)?;
    let catalog = framework_profile_catalog(&workspace, &home, &environment)?;
    let plan = compose_profile_at(
        "web",
        &[overlay],
        &workspace,
        &home,
        &home.join("profiles/.seekdeep-installation/package.json"),
        &shipped_preset_root(),
        Some("1"),
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
    install_keyless_routes(context, &calls)?;
    let registry = context
        .get(WORKSPACE_REGISTRY)
        .ok_or_else(|| anyhow::anyhow!("Web profile has no workspace registry"))?;
    let server = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("Web profile has no server"))?;
    let settings_routes = settings_fixture_routes(context, &server)?;
    let attach = server.register(WebRoute {
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
    })?;
    println!("seekdeep web: http://127.0.0.1:{}", server.port());
    tokio::signal::ctrl_c().await?;
    attach.dispose();
    for route in settings_routes {
        route.dispose();
    }
    application.dispose().await?;
    std::fs::write(
        data.join("model-call-audit.json"),
        serde_json::to_vec(&json!({"calls":calls.load(Ordering::SeqCst)}))?,
    )?;
    anyhow::ensure!(
        calls.load(Ordering::SeqCst) == 0,
        "keyless Web scenario made a model call"
    );
    Ok(())
}
