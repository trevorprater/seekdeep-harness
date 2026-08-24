//! Pure contracts and real ripgrep/tool-runtime integration parity.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_spill::{SaveTextSpill, SpillBackend, SpillLocator, SpillRef, SpillStore};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use seekdeep_tool_fs_search::{
    Config, GlobInput, GrepInput, RgPathCache, SearchError, SearchErrorCode, apply,
    build_glob_command, build_grep_command, parse_glob_args, parse_grep_args, parse_grep_matches,
    preview_line, sample_across_top_level, to_workdir_relative,
};
use seekdeep_tools::{
    SearchResultView, ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolResult,
    ToolResultView, ToolRuntime, ToolRuntimeConfig,
};
use serde_json::{Value, json};

#[test]
fn argument_argv_parsing_preview_sampling_and_relative_paths_match_source() {
    assert!(
        parse_glob_args(GlobInput {
            pattern: " ".into(),
            path: None
        })
        .is_err()
    );
    assert!(
        parse_grep_args(GrepInput {
            pattern: " ".into(),
            path: None,
            include: Some("*.{rs,toml}".into())
        })
        .is_ok()
    );
    assert!(
        parse_grep_args(GrepInput {
            pattern: "x".into(),
            path: None,
            include: Some("*.rs,*.toml".into())
        })
        .is_err()
    );
    let glob = build_glob_command(&GlobInput {
        pattern: "*.rs".into(),
        path: Some("-root".into()),
    });
    assert_eq!(
        &glob[..5],
        [
            "--files",
            "--glob=*.rs",
            "--sort=modified",
            "--no-ignore",
            "--hidden"
        ]
    );
    assert_eq!(&glob[glob.len() - 2..], ["--", "-root"]);
    assert_eq!(
        build_grep_command(&GrepInput {
            pattern: "-x".into(),
            path: Some("src".into()),
            include: Some("*.rs".into())
        }),
        ["--json", "--regexp=-x", "--glob=*.rs", "--", "src"]
    );
    let output = r#"{"type":"begin","data":{}}
{"type":"match","data":{"path":{"text":"src/lib.rs"},"lines":{"text":"hello\r\n"},"line_number":7}}
{"type":"match","data":{"path":{"text":"bin.dat"},"lines":{"bytes":"AA=="},"line_number":2}}
"#;
    let parsed = parse_grep_matches(output).unwrap();
    assert_eq!(parsed[0].line, "hello");
    assert_eq!(parsed[1].line, "(line is not valid UTF-8)");
    assert!(
        parse_grep_matches("not json\n")
            .unwrap_err()
            .downcast_ref::<SearchError>()
            .is_some()
    );
    assert_eq!(preview_line("ééé", 5), "éé (line truncated)");
    let paths = vec!["a/1".into(), "a/2".into(), "b/1".into(), "c/1".into()];
    assert_eq!(
        sample_across_top_level(&paths, 3, ".").0,
        ["a/1", "b/1", "c/1"]
    );
    assert_eq!(
        to_workdir_relative("/ws/src/lib.rs", std::path::Path::new("/ws")),
        "src/lib.rs"
    );
    assert_eq!(
        to_workdir_relative("relative", std::path::Path::new("/ws")),
        "relative"
    );
}

#[tokio::test]
async fn missing_packaged_ripgrep_failure_is_lazy_typed_and_memoized() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cache = RgPathCache::default();
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let calls = calls.clone();
        let error = cache
            .resolve_with(move || async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Err("platform package missing".to_owned())
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<SearchError>().unwrap().code(),
            SearchErrorCode::Failed.as_str()
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[derive(Default)]
struct MemorySpill(Mutex<Vec<SaveTextSpill>>);

#[async_trait]
impl SpillBackend for MemorySpill {
    async fn save_text(&self, input: SaveTextSpill) -> anyhow::Result<SpillRef> {
        self.0.lock().push(input.clone());
        Ok(SpillRef {
            locator: SpillLocator::new("spill://search"),
            bytes: input.content.len() as u64,
            retrieval_hint: "Use read.".into(),
        })
    }
}

struct Harness {
    context: Context,
    tools: Arc<ToolRuntime>,
    agent: Arc<Agent>,
    spill: Arc<MemorySpill>,
    _temp: tempfile::TempDir,
}

fn harness(config: &Config) -> Harness {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join(".git")).unwrap();
    std::fs::write(temp.path().join("a.rs"), "needle one\n").unwrap();
    std::fs::write(temp.path().join("b.rs"), "needle two\n").unwrap();
    std::fs::write(temp.path().join(".hidden.rs"), "needle hidden\n").unwrap();
    std::fs::write(temp.path().join(".git/secret.rs"), "needle secret\n").unwrap();
    let context = Context::new();
    let prompt = seekdeep_system_prompt::install(
        &context,
        seekdeep_system_prompt::SystemPromptConfig::default(),
    )
    .unwrap();
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..Default::default()
        },
    )
    .unwrap();
    LocalSubprocessRuntime::install(&context).unwrap();
    let spill = Arc::new(MemorySpill::default());
    Arc::new(SpillStore::new(spill.clone()))
        .provide(&context)
        .unwrap();
    apply(&context, config).unwrap();
    let id = SessionId::new("search-session");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(temp.path().to_string_lossy().into_owned());
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let agent = Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session.clone(),
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    Harness {
        context,
        tools,
        agent,
        spill,
        _temp: temp,
    }
}

fn config() -> Config {
    Config {
        sample_over_cap_glob_results: Some(true),
        ..Config::default()
    }
}

async fn call(harness: &Harness, name: &str, arguments: Value) -> ToolExecutionResult {
    harness
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new(format!("{name}-call")),
                name,
                arguments,
                AbortSignal::default(),
            )
            .with_agent(harness.agent.clone()),
        )
        .await
}

fn text(result: &ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn real_glob_and_grep_use_session_cwd_hidden_files_and_vcs_exclusion() {
    let harness = harness(&config());
    let glob = call(&harness, "glob", json!({"pattern":"*.rs"})).await;
    assert!(!glob.is_error(), "{:?}", glob.error());
    let glob_text = text(&glob);
    assert!(
        glob_text.contains("a.rs") && glob_text.contains(".hidden.rs"),
        "{glob_text:?}"
    );
    assert!(!glob_text.contains(".git/secret.rs"));
    let grep = call(
        &harness,
        "grep",
        json!({"pattern":"needle", "include":"*.rs"}),
    )
    .await;
    assert!(!grep.is_error(), "{:?}", grep.error());
    let grep_text = text(&grep);
    assert!(grep_text.contains("Line 1: needle one"));
    assert!(!grep_text.contains("secret"));
    assert!(glob.meta().is_some() && grep.meta().is_some());
}

#[tokio::test]
async fn invalid_pattern_is_typed_and_over_cap_direct_results_spill_once() {
    let harness = harness(&Config {
        sample_over_cap_glob_results: Some(false),
        glob_max_results: Some(1),
        grep_max_matches: Some(1),
        ..Config::default()
    });
    let invalid = call(&harness, "grep", json!({"pattern":"["})).await;
    assert!(invalid.is_error());
    assert_eq!(
        invalid
            .error()
            .and_then(|error| error.info.as_ref())
            .map(|info| info.code.as_str()),
        Some(SearchErrorCode::InvalidPattern.as_str()),
        "{:?}",
        invalid.error()
    );
    let glob = call(&harness, "glob", json!({"pattern":"*.rs"})).await;
    assert!(text(&glob).contains("spill://search"));
    assert_eq!(harness.spill.0.lock().len(), 1);
    assert!(harness.spill.0.lock()[0].content.contains("a.rs"));
    let grep = call(&harness, "grep", json!({"pattern":"needle"})).await;
    assert!(text(&grep).contains("spill://search"));
    assert_eq!(harness.spill.0.lock().len(), 2);

    let definition = harness.tools.get("grep", None).unwrap();
    let projected = ToolResult {
        content: grep.content().to_vec(),
        is_error: false,
        meta: grep.meta().cloned(),
    };
    assert!(matches!(
        definition.present_result.as_ref().unwrap()(&json!({"pattern":"needle"}), &projected),
        Some(ToolResultView::Search(SearchResultView::Matches(_)))
    ));
}

#[tokio::test]
async fn raw_output_overflow_and_zero_results_use_distinct_success_and_error_routes() {
    let overflow = harness(&Config {
        sample_over_cap_glob_results: Some(false),
        raw_output_max_bytes: Some(1),
        ..Config::default()
    });
    let result = call(&overflow, "glob", json!({"pattern":"*.rs"})).await;
    assert_eq!(
        result
            .error()
            .and_then(|error| error.info.as_ref())
            .map(|info| info.code.as_str()),
        Some(SearchErrorCode::RawOutputOverflow.as_str())
    );

    let normal = harness(&config());
    let empty = call(&normal, "glob", json!({"pattern":"*.never"})).await;
    assert!(!empty.is_error());
    assert_eq!(text(&empty), "No files found");
}

#[tokio::test]
async fn config_timeout_and_plugin_disposal_preserve_loader_lifecycle() {
    let harness = harness(&config());
    let schemas = harness.tools.schemas(None);
    assert_eq!(
        schemas
            .iter()
            .map(|schema| schema.name.as_str())
            .collect::<Vec<_>>(),
        ["glob", "grep"]
    );
    assert_eq!(
        harness.tools.get("glob", None).unwrap().timeout_ms,
        Some(30_000.0)
    );
    harness.context.fiber().dispose().await.unwrap();
}
