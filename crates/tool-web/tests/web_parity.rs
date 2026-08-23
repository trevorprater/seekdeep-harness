//! Registration, formatting, provider dispatch, metadata, timeout, and config parity.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_system_prompt::AssembleContext;
use seekdeep_tool_web::{
    Config, DEFAULT_FETCH_MAX_OUTPUT_CHARS, DEFAULT_WEB_TOOL_TIMEOUT_MS, WEB_SEARCH_MAX_RESULTS,
    apply, format_fetch_output, format_search_output,
};
use seekdeep_tools::{ToolExecutionInput, ToolResultView, WebResultView};
use seekdeep_web::{
    WebFetchBody, WebFetchProvider, WebFetchRequest, WebFetchResult, WebRuntime, WebRuntimeConfig,
    WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource,
};
use serde_json::{Value, json};

struct SearchProvider {
    seen: Mutex<Vec<(WebSearchRequest, Option<AbortSignal>)>>,
}

#[async_trait]
impl WebSearchProvider for SearchProvider {
    fn id(&self) -> &'static str {
        "search"
    }

    fn available(&self) -> bool {
        true
    }

    async fn search(
        &self,
        request: &WebSearchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebSearchResult> {
        self.seen.lock().push((request.clone(), signal));
        Ok(WebSearchResult {
            content: Some("answer".to_owned()),
            sources: vec![WebSearchSource {
                url: "https://example.test/a".to_owned(),
                title: Some("Example".to_owned()),
                snippet: Some("snippet".to_owned()),
                published_at: Some("2026-07-20".to_owned()),
            }],
            truncated: false,
        })
    }
}

struct FetchProvider {
    seen: Mutex<Vec<(WebFetchRequest, Option<AbortSignal>)>>,
    result: Mutex<WebFetchResult>,
}

#[async_trait]
impl WebFetchProvider for FetchProvider {
    fn id(&self) -> &'static str {
        "fetch"
    }

    fn available(&self) -> bool {
        true
    }

    async fn fetch(
        &self,
        request: &WebFetchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebFetchResult> {
        self.seen.lock().push((request.clone(), signal));
        Ok(self.result.lock().clone())
    }
}

struct Harness {
    dependencies: AgentLoopTestDependencies,
    search: Arc<SearchProvider>,
    fetch: Arc<FetchProvider>,
}

impl Harness {
    fn new(config: Config) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        let web = WebRuntime::new(&context, &WebRuntimeConfig::default()).unwrap();
        let search = Arc::new(SearchProvider {
            seen: Mutex::new(Vec::new()),
        });
        let fetch = Arc::new(FetchProvider {
            seen: Mutex::new(Vec::new()),
            result: Mutex::new(WebFetchResult {
                url: "https://example.test/page".to_owned(),
                status_code: 200,
                body: WebFetchBody::Html {
                    content: "<h1>Hello</h1><p>World</p><script>drop()</script>".to_owned(),
                },
                truncated: false,
            }),
        });
        web.register_search_provider(&context, search.clone())
            .unwrap();
        web.register_fetch_provider(&context, fetch.clone())
            .unwrap();
        apply(&context, config).unwrap();
        Self {
            dependencies,
            search,
            fetch,
        }
    }

    async fn call(&self, name: &str, arguments: Value) -> seekdeep_tools::ToolExecutionResult {
        self.dependencies
            .tools
            .execute(ToolExecutionInput::new(
                CallId::new(format!("call-{name}")),
                name,
                arguments,
                AbortSignal::default(),
            ))
            .await
    }
}

fn text(result: &seekdeep_tools::ToolExecutionResult) -> String {
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
async fn defaults_register_both_parallel_tools_prompts_timeouts_and_provider_dispatch() {
    let harness = Harness::new(Config::default());
    let search = harness.dependencies.tools.get("web_search", None).unwrap();
    let fetch = harness.dependencies.tools.get("web_fetch", None).unwrap();
    assert_eq!(search.timeout_ms, Some(DEFAULT_WEB_TOOL_TIMEOUT_MS));
    assert_eq!(fetch.timeout_ms, Some(DEFAULT_WEB_TOOL_TIMEOUT_MS));
    assert!(search.is_concurrency_safe.as_ref().unwrap()(
        &json!({ "query": "q" })
    ));
    assert!(fetch.is_concurrency_safe.as_ref().unwrap()(
        &json!({ "url": "https://a" })
    ));
    assert_eq!(
        search.parameters["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["query"]
    );
    assert_eq!(
        fetch.parameters["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["url"]
    );
    let prompt = harness
        .dependencies
        .system_prompt
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    let prompt = prompt
        .sections
        .iter()
        .map(|section| section.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("Follow up with web_fetch"));

    let searched = harness.call("web_search", json!({ "query": "rust" })).await;
    assert!(!searched.is_error());
    assert!(text(&searched).contains("[Example](https://example.test/a)"));
    assert_eq!(
        harness.search.seen.lock()[0].0.max_results,
        Some(WEB_SEARCH_MAX_RESULTS)
    );
    assert!(harness.search.seen.lock()[0].1.is_some());
    let presented = search.present_result.as_ref().unwrap()(
        &json!({ "query": "rust" }),
        &seekdeep_tools::ToolResult {
            content: searched.content().to_vec(),
            is_error: false,
            meta: searched.meta().cloned(),
        },
    );
    assert!(matches!(
        presented,
        Some(ToolResultView::Web(WebResultView::Search(_)))
    ));

    let fetched = harness
        .call("web_fetch", json!({ "url": "https://example.test/page" }))
        .await;
    assert!(!fetched.is_error());
    let output = text(&fetched);
    assert!(output.contains("Fetched https://example.test/page (HTTP 200)"));
    assert!(output.contains("# Hello"));
    assert!(output.contains("World"));
    assert!(!output.contains("drop()"));
    assert!(harness.fetch.seen.lock()[0].1.is_some());
    assert!(matches!(
        fetch.present_result.as_ref().unwrap()(
            &json!({ "url": "https://example.test/page" }),
            &seekdeep_tools::ToolResult {
                content: fetched.content().to_vec(),
                is_error: false,
                meta: fetched.meta().cloned(),
            },
        ),
        Some(ToolResultView::Web(WebResultView::Fetch(_)))
    ));
}

#[tokio::test]
async fn config_controls_enablement_caps_and_search_only_guidance() {
    let harness = Harness::new(Config {
        search: Some(true),
        fetch: Some(false),
        search_max_results: Some(3),
        fetch_timeout_ms: Some(99.0),
        search_timeout_ms: Some(77.0),
        fetch_max_output_chars: Some(100),
    });
    assert!(harness.dependencies.tools.get("web_search", None).is_some());
    assert!(harness.dependencies.tools.get("web_fetch", None).is_none());
    assert_eq!(
        harness
            .dependencies
            .tools
            .get("web_search", None)
            .unwrap()
            .timeout_ms,
        Some(77.0)
    );
    let prompt = harness
        .dependencies
        .system_prompt
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    let prompt = prompt
        .sections
        .iter()
        .map(|section| section.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("source snippets"));
    assert!(!prompt.contains("Follow up with web_fetch"));
    harness.call("web_search", json!({ "query": "q" })).await;
    assert_eq!(harness.search.seen.lock()[0].0.max_results, Some(3));
}

#[tokio::test]
async fn validation_and_unavailable_provider_fail_loud_while_schemas_stay_enabled() {
    let harness = Harness::new(Config::default());
    assert!(
        harness
            .call("web_search", json!({ "query": " " }))
            .await
            .is_error()
    );
    assert!(
        harness
            .call("web_fetch", json!({ "url": "" }))
            .await
            .is_error()
    );

    let context = Context::new();
    let dependencies =
        mount_agent_loop_test_dependencies(&context, AgentLoopTestDependenciesOptions::default())
            .unwrap();
    WebRuntime::new(&context, &WebRuntimeConfig::default()).unwrap();
    apply(&context, Config::default()).unwrap();
    assert!(dependencies.tools.get("web_search", None).is_some());
    let unavailable = dependencies
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("missing"),
            "web_search",
            json!({ "query": "q" }),
            AbortSignal::default(),
        ))
        .await;
    assert!(unavailable.is_error());
    assert_eq!(
        unavailable.error().unwrap().info.as_ref().unwrap().code,
        "WEB_PROVIDER_UNAVAILABLE"
    );
}

#[test]
fn pure_formatters_cover_empty_truncated_and_complete_output_caps() {
    assert!(
        format_search_output(&WebSearchResult {
            content: None,
            sources: Vec::new(),
            truncated: false,
        })
        .starts_with("No results found.")
    );
    let search = format_search_output(&WebSearchResult {
        content: None,
        sources: vec![WebSearchSource {
            url: "not a url".to_owned(),
            title: None,
            snippet: None,
            published_at: None,
        }],
        truncated: true,
    });
    assert!(search.contains("[not a url](not a url)"));
    assert!(search.contains("Showing the first 1 sources"));

    let result = WebFetchResult {
        url: "https://a.test".to_owned(),
        status_code: 200,
        body: WebFetchBody::Text {
            content: "X".repeat(1_000),
        },
        truncated: false,
    };
    let bounded = format_fetch_output(&result, 100);
    assert!(bounded.encode_utf16().count() <= 100);
    assert!(bounded.contains("Content truncated"));
    assert_eq!(DEFAULT_FETCH_MAX_OUTPUT_CHARS, 200_000);

    let converted = format_fetch_output(
        &WebFetchResult {
            url: "https://a.test/table".to_owned(),
            status_code: 200,
            body: WebFetchBody::Html {
                content: "<h1>A &amp; B</h1><p><a href=\"https://x.test\">link</a></p><table><tr><th>H</th></tr><tr><td>V</td></tr></table><script>hidden()</script><style>.x{}</style>".to_owned(),
            },
            truncated: false,
        },
        2_000,
    );
    assert!(converted.contains("# A & B"), "{converted}");
    assert!(converted.contains("[link](https://x.test)"));
    assert!(converted.contains('H'));
    assert!(converted.contains('V'));
    assert!(!converted.contains("hidden()"));
    assert!(!converted.contains(".x{}"));

    let deep = format!("{}x{}", "<div>".repeat(513), "</div>".repeat(513));
    let degraded = format_fetch_output(
        &WebFetchResult {
            url: "https://a.test/deep".to_owned(),
            status_code: 200,
            body: WebFetchBody::Html { content: deep },
            truncated: false,
        },
        10_000,
    );
    assert!(degraded.contains("<div>"));
}
