//! Focused model-tool integration with a real provider and timeout policy.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use indexmap::IndexMap;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_fs_local::{Config as FsConfig, LocalFileSystem};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_lsp::{Lsp, LspProvider, LspProviderId, LspProviderQuery, LspQueryResult};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::SystemPromptConfig;
use seekdeep_tool_lsp::{Config, apply};
use seekdeep_tools::{
    ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolRuntimeConfig,
};
use seekdeep_util::abort::AbortSignal as UtilAbortSignal;
use serde_json::json;

fn source_server_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .expect("workspace parent")
        .join(
            "deepseek-harness/packages/lsp/lsp-stdio/node_modules/.bin/typescript-language-server",
        )
}

fn agent(context: &Context, cwd: &str) -> Arc<Agent> {
    let id = SessionId::new("tool-lsp-integration");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(cwd.to_owned());
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

fn text(result: &ToolExecutionResult) -> Option<&str> {
    let ToolExecutionResult::Success(success) = result else {
        return None;
    };
    let ContentBlock::Text { text } = success.content.first()? else {
        return None;
    };
    Some(text)
}

#[tokio::test]
async fn definition_round_trips_through_real_typescript_provider_and_renders() {
    let server = source_server_binary();
    assert!(server.is_file());
    let root = tempfile::tempdir().unwrap();
    let canonical = tokio::fs::canonicalize(root.path()).await.unwrap();
    let workspace = canonical.join("ws");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(
        workspace.join("tsconfig.json"),
        json!({"compilerOptions": {"strict": true}}).to_string(),
    )
    .await
    .unwrap();
    tokio::fs::write(workspace.join("a.ts"), "const x = 1\nconst y = x\n")
        .await
        .unwrap();
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
    LocalFileSystem::install(
        &context,
        FsConfig {
            cwd: Some(canonical.to_string_lossy().into_owned()),
            diff_basis_max_bytes: None,
        },
    )
    .unwrap();
    seekdeep_subprocess_local::LocalSubprocessRuntime::install(&context).unwrap();
    seekdeep_lsp_stdio::apply(
        &context,
        serde_json::from_value(json!({"servers": {"typescript": {
            "command": server,
            "args": ["--stdio"],
            "extensionToLanguage": {".ts": "typescript"}
        }}}))
        .unwrap(),
    )
    .await
    .unwrap();
    seekdeep_tool_timeout_policy::install(&context, &tools).unwrap();
    apply(&context, &Config::default()).unwrap();
    let input = ToolExecutionInput::new(
        CallId::new("definition"),
        "lsp",
        json!({
            "operation": "goToDefinition",
            "file_path": "a.ts",
            "line": 2,
            "character": 11
        }),
        AbortSignal::default(),
    )
    .with_agent(agent(&context, &workspace.to_string_lossy()));
    let result = tools.execute(input).await;
    assert!(!result.is_error());
    assert_eq!(text(&result), Some("a.ts:1:7"));
    context.fiber().restart().await.unwrap();
}

#[derive(Debug)]
struct HangingProvider {
    id: LspProviderId,
    extensions: IndexMap<String, String>,
}

#[async_trait]
impl LspProvider for HangingProvider {
    fn id(&self) -> &LspProviderId {
        &self.id
    }

    fn extension_to_language(&self) -> &IndexMap<String, String> {
        &self.extensions
    }

    async fn query(
        &self,
        _request: LspProviderQuery,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        let signal = signal.ok_or_else(|| anyhow::anyhow!("timeout signal missing"))?;
        signal.cancelled().await;
        Err(seekdeep_lsp_stdio::abort_error(&signal))
    }
}

#[tokio::test]
async fn timeout_policy_enforces_the_tool_owned_budget() {
    let context = Context::new();
    let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).unwrap();
    let tools = seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).unwrap();
    let lsp = Arc::new(Lsp::new());
    lsp.provide(&context).unwrap();
    lsp.register_provider(
        &context,
        Arc::new(HangingProvider {
            id: LspProviderId::new("hang"),
            extensions: IndexMap::from([(".ts".to_owned(), "typescript".to_owned())]),
        }),
    )
    .unwrap();
    seekdeep_tool_timeout_policy::install(&context, &tools).unwrap();
    apply(
        &context,
        &Config {
            timeout_ms: Some(100.0),
            ..Config::default()
        },
    )
    .unwrap();
    let input = ToolExecutionInput::new(
        CallId::new("timeout"),
        "lsp",
        json!({
            "operation": "goToDefinition",
            "file_path": "a.ts",
            "line": 1,
            "character": 1
        }),
        UtilAbortSignal::default(),
    )
    .with_agent(agent(&context, "/virtual/workspace"));
    let result = tools.execute(input).await;
    let ToolExecutionResult::Failure(failure) = result else {
        panic!("expected timeout")
    };
    assert_eq!(failure.error.info.unwrap().code, "TOOL_TIMEOUT");
    context.fiber().restart().await.unwrap();
}
