//! Tool registration, execution, structured-error, and presentation parity.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_lsp::{
    LSP_UNAVAILABLE, Lsp, LspHover, LspLocation, LspOperation, LspPosition, LspProvider,
    LspProviderId, LspProviderQuery, LspQueryResult, LspRange,
};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig};
use seekdeep_tool_lsp::{
    Config, DEFAULT_LSP_TOOL_TIMEOUT_MS, LSP_OPERATIONS, LSP_PROMPT_TEXT, LSP_WORKSPACE_REQUIRED,
    apply,
};
use seekdeep_tools::{
    FileLocation, GenericCallView, ToolCallKind, ToolCallView, ToolExecutionInput,
    ToolExecutionResult, ToolPresentationMode, ToolRuntimeConfig,
};
use serde_json::{Value, json};

type ResponseFn = dyn Fn(&LspProviderQuery) -> anyhow::Result<LspQueryResult> + Send + Sync;

#[derive(Clone)]
struct StubProvider {
    id: LspProviderId,
    extensions: IndexMap<String, String>,
    response: Arc<ResponseFn>,
    seen: Arc<Mutex<Vec<LspProviderQuery>>>,
    signals: Arc<Mutex<Vec<Option<AbortSignal>>>>,
}

impl std::fmt::Debug for StubProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StubProvider")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl LspProvider for StubProvider {
    fn id(&self) -> &LspProviderId {
        &self.id
    }

    fn extension_to_language(&self) -> &IndexMap<String, String> {
        &self.extensions
    }

    async fn query(
        &self,
        request: LspProviderQuery,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        self.seen.lock().push(request.clone());
        self.signals.lock().push(signal);
        (self.response)(&request)
    }
}

impl StubProvider {
    fn new(response: LspQueryResult) -> Arc<Self> {
        Self::with_extensions(response, [(".ts", "typescript")])
    }

    fn with_extensions(
        response: LspQueryResult,
        extensions: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: LspProviderId::new("stub"),
            extensions: extensions
                .into_iter()
                .map(|(extension, language)| (extension.to_owned(), language.to_owned()))
                .collect(),
            response: Arc::new(move |_| Ok(response.clone())),
            seen: Arc::new(Mutex::new(Vec::new())),
            signals: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

struct Harness {
    context: Context,
    prompt: Arc<seekdeep_system_prompt::SystemPrompt>,
    tools: Arc<seekdeep_tools::ToolRuntime>,
}

fn locations(workspace: &str) -> LspQueryResult {
    LspQueryResult::Locations {
        locations: vec![LspLocation {
            uri: format!("file://{workspace}/a.ts"),
            range: LspRange {
                start: LspPosition {
                    line: 0.0,
                    character: 0.0,
                },
                end: LspPosition {
                    line: 0.0,
                    character: 1.0,
                },
            },
        }],
        resolved_workspace_uri: format!("file://{workspace}"),
    }
}

fn mount(provider: Option<Arc<StubProvider>>, config: Config) -> Harness {
    let context = Context::new();
    let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).unwrap();
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..ToolRuntimeConfig::default()
        },
    )
    .unwrap();
    let lsp = Arc::new(Lsp::new());
    lsp.provide(&context).unwrap();
    if let Some(provider) = provider {
        let erased: Arc<dyn LspProvider> = provider;
        lsp.register_provider(&context, erased).unwrap();
    }
    apply(&context, &config).unwrap();
    Harness {
        context,
        prompt,
        tools,
    }
}

fn agent(context: &Context, cwd: Option<&str>) -> Arc<Agent> {
    let id = SessionId::new("tool-lsp-test");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = cwd.map(str::to_owned);
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

async fn call(harness: &Harness, arguments: Value, cwd: Option<&str>) -> ToolExecutionResult {
    let mut input = ToolExecutionInput::new(
        CallId::new("tool-lsp-call"),
        "lsp",
        arguments,
        AbortSignal::default(),
    );
    if cwd.is_some() {
        input = input.with_agent(agent(&harness.context, cwd));
    }
    harness.tools.execute(input).await
}

fn text(result: &ToolExecutionResult) -> Option<&str> {
    match result {
        ToolExecutionResult::Success(success) => success.content.first().and_then(|content| {
            let ContentBlock::Text { text } = content else {
                return None;
            };
            Some(text.as_str())
        }),
        ToolExecutionResult::Failure(failure) => failure.content.first().and_then(|content| {
            let ContentBlock::Text { text } = content else {
                return None;
            };
            Some(text.as_str())
        }),
    }
}

#[tokio::test]
async fn registration_prompt_timeout_schema_and_config_validation_are_exact() {
    let workspace = "/virtual/workspace";
    let harness = mount(
        Some(StubProvider::new(locations(workspace))),
        Config::default(),
    );
    let definition = harness.tools.get("lsp", None).unwrap();
    assert_eq!(definition.timeout_ms, Some(DEFAULT_LSP_TOOL_TIMEOUT_MS));
    assert_eq!(
        definition
            .parameters
            .get("properties")
            .and_then(|properties| properties.get("operation"))
            .and_then(|operation| operation.get("enum"))
            .unwrap(),
        &json!(LSP_OPERATIONS)
    );
    let assembly = harness
        .prompt
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    assert!(
        assembly
            .sections
            .iter()
            .any(|section| section.text.contains(LSP_PROMPT_TEXT))
    );

    let override_harness = mount(
        Some(StubProvider::new(locations(workspace))),
        Config {
            timeout_ms: Some(5_000.0),
            ..Config::default()
        },
    );
    assert_eq!(
        override_harness.tools.get("lsp", None).unwrap().timeout_ms,
        Some(5_000.0)
    );

    for (config, expected) in [
        (
            Config {
                max_locations: Some(0.0),
                ..Config::default()
            },
            "maxLocations",
        ),
        (
            Config {
                timeout_ms: Some(2_147_483_648.0),
                ..Config::default()
            },
            "timeoutMs",
        ),
    ] {
        let context = Context::new();
        assert!(
            apply(&context, &config)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}

#[tokio::test]
async fn execution_converts_coordinates_threads_workspace_signal_and_renders_locations() {
    let workspace = "/virtual/workspace";
    let provider = StubProvider::new(locations(workspace));
    let harness = mount(Some(provider.clone()), Config::default());
    let result = call(
        &harness,
        json!({
            "operation": "goToDefinition",
            "file_path": "a.ts",
            "line": 3,
            "character": 5
        }),
        Some(workspace),
    )
    .await;
    assert!(!result.is_error());
    assert_eq!(text(&result), Some("a.ts:1:1"));
    let seen = provider.seen.lock();
    assert_eq!(seen[0].operation, LspOperation::GoToDefinition);
    assert_eq!(seen[0].file_path, "a.ts");
    assert_eq!(
        seen[0].position,
        LspPosition {
            line: 2.0,
            character: 4.0
        }
    );
    assert_eq!(seen[0].workspace_root, workspace);
    assert_eq!(provider.signals.lock().len(), 1);
}

#[tokio::test]
async fn canonical_values_stay_complete_while_locations_and_hover_presentations_are_capped() {
    let workspace = "/virtual/capped-workspace";
    let mut response = locations(workspace);
    let LspQueryResult::Locations { locations, .. } = &mut response else {
        unreachable!()
    };
    locations.push(LspLocation {
        uri: format!("file://{workspace}/b.ts"),
        range: LspRange {
            start: LspPosition {
                line: 1.0,
                character: 2.0,
            },
            end: LspPosition {
                line: 1.0,
                character: 3.0,
            },
        },
    });
    let harness = mount(
        Some(StubProvider::new(response.clone())),
        Config {
            max_locations: Some(1.0),
            ..Config::default()
        },
    );
    let result = call(
        &harness,
        json!({"operation":"findReferences","file_path":"a.ts","line":1,"character":1}),
        Some(workspace),
    )
    .await;
    assert_eq!(
        text(&result),
        Some("a.ts:1:1\n… 1 more location omitted (limit 1).")
    );
    let ToolExecutionResult::Success(success) = result else {
        panic!("expected success")
    };
    assert_eq!(success.value, serde_json::to_value(response).unwrap());

    for hover in [
        Some(LspHover {
            contents: "number".to_owned(),
            range: Some(LspRange {
                start: LspPosition {
                    line: 2.0,
                    character: 3.0,
                },
                end: LspPosition {
                    line: 2.0,
                    character: 7.0,
                },
            }),
        }),
        None,
    ] {
        let response = LspQueryResult::Hover {
            hover: hover.clone(),
        };
        let harness = mount(Some(StubProvider::new(response.clone())), Config::default());
        let result = call(
            &harness,
            json!({"operation":"hover","file_path":"a.ts","line":1,"character":1}),
            Some("/ws"),
        )
        .await;
        assert_eq!(
            text(&result),
            Some(
                hover
                    .as_ref()
                    .map_or("No hover information.", |hover| hover.contents.as_str())
            )
        );
        let ToolExecutionResult::Success(success) = result else {
            panic!("expected success")
        };
        assert_eq!(success.value, serde_json::to_value(response).unwrap());
    }
}

#[tokio::test]
async fn workspace_unavailable_and_invalid_arguments_keep_structured_error_codes() {
    let workspace = "/virtual/workspace";
    let harness = mount(
        Some(StubProvider::new(locations(workspace))),
        Config::default(),
    );
    let missing = call(
        &harness,
        json!({"operation":"goToDefinition","file_path":"a.ts","line":1,"character":1}),
        None,
    )
    .await;
    let ToolExecutionResult::Failure(missing) = missing else {
        panic!("expected workspace failure")
    };
    assert_eq!(missing.error.info.unwrap().code, LSP_WORKSPACE_REQUIRED);

    let unavailable = mount(
        Some(StubProvider::with_extensions(
            locations(workspace),
            [(".py", "python")],
        )),
        Config::default(),
    );
    let unavailable = call(
        &unavailable,
        json!({"operation":"goToDefinition","file_path":"a.ts","line":1,"character":1}),
        Some(workspace),
    )
    .await;
    let ToolExecutionResult::Failure(unavailable) = unavailable else {
        panic!("expected unavailable failure")
    };
    assert_eq!(unavailable.error.info.unwrap().code, LSP_UNAVAILABLE);

    let invalid = call(
        &harness,
        json!({"operation":"rename","file_path":"a.ts","line":1,"character":1}),
        Some(workspace),
    )
    .await;
    let ToolExecutionResult::Failure(invalid) = invalid else {
        panic!("expected invalid args")
    };
    assert_eq!(invalid.error.info.unwrap().code, "INVALID_ARGS");
}

#[test]
fn pending_call_projection_is_replay_safe_and_exact() {
    let harness = mount(None, Config::default());
    let definition = harness.tools.get("lsp", None).unwrap();
    let view = definition.present_call.as_ref().unwrap()(&json!({
        "operation": "hover",
        "file_path": "a.ts",
        "line": 2,
        "character": 3
    }));
    assert_eq!(
        view,
        Some(ToolCallView::Generic(GenericCallView {
            title: "LSP hover a.ts:2:3".to_owned(),
            kind: Some(ToolCallKind::Search),
            raw_input: None,
            content: None,
            locations: Some(vec![FileLocation {
                path: "a.ts".to_owned(),
                line: Some(2.0),
            }]),
        }))
    );
}
