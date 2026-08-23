//! Model-facing web search and fetch tools over the provider-neutral web seam.

use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_llm::ContentBlock;
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, TOOLS, ToolCallKind, ToolCallView,
    ToolDefinition, ToolResult, ToolResultView, WebFetchResultView, WebResultView,
    WebSearchResultView, WebSource, define_tool,
};
use seekdeep_web::{
    WEB, WebFetchBody, WebFetchRequest, WebFetchResult, WebRuntime, WebSearchRequest,
    WebSearchResult, WebSearchSource,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Loader plugin name.
pub const NAME: &str = "tool-web";
/// Required services.
pub const INJECT: &[&str] = &["tools", "web", "systemPrompt"];
/// Default cooperative tool-call timeout.
pub const DEFAULT_WEB_TOOL_TIMEOUT_MS: f64 = 30_000.0;
/// Default complete fetch-output and conversion-input cap.
pub const DEFAULT_FETCH_MAX_OUTPUT_CHARS: u64 = 200_000;
/// Default returned search-source cap.
pub const WEB_SEARCH_MAX_RESULTS: u64 = 8;
const MAX_CONVERSION_DEPTH: usize = 512;
const TRUNCATION_FOOTER: &str =
    "\n\n(Content truncated. Fetch a more specific URL or section for the full text.)";

/// Enabled tools and deployment-owned limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Register search; defaults true.
    pub search: Option<bool>,
    /// Register fetch; defaults true.
    pub fetch: Option<bool>,
    /// Maximum search sources.
    pub search_max_results: Option<u64>,
    /// Fetch tool-call timeout.
    pub fetch_timeout_ms: Option<f64>,
    /// Search tool-call timeout.
    pub search_timeout_ms: Option<f64>,
    /// Complete fetch output and synchronous source cap.
    pub fetch_max_output_chars: Option<u64>,
}

#[derive(Clone, Copy)]
struct ResolvedConfig {
    search: bool,
    fetch: bool,
    search_max_results: u64,
    fetch_timeout_ms: f64,
    search_timeout_ms: f64,
    fetch_max_output_chars: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct SearchArgs {
    query: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct FetchArgs {
    url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchMeta {
    sources: Vec<WebSource>,
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    answer: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FetchMeta {
    url: String,
    status_code: u16,
    truncated: bool,
}

#[derive(Clone, Debug)]
struct RenderedFetch {
    text: String,
    truncated: bool,
}

fn positive_integer(name: &str, value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && value >= 1.0,
        "tool-web: {name} must be a positive integer"
    );
    Ok(())
}

fn resolve_config(config: Config) -> anyhow::Result<ResolvedConfig> {
    let search_max_results = config.search_max_results.unwrap_or(WEB_SEARCH_MAX_RESULTS);
    let fetch_timeout_ms = config
        .fetch_timeout_ms
        .unwrap_or(DEFAULT_WEB_TOOL_TIMEOUT_MS);
    let search_timeout_ms = config
        .search_timeout_ms
        .unwrap_or(DEFAULT_WEB_TOOL_TIMEOUT_MS);
    let fetch_max_output_chars = config
        .fetch_max_output_chars
        .unwrap_or(DEFAULT_FETCH_MAX_OUTPUT_CHARS);
    anyhow::ensure!(
        search_max_results > 0,
        "tool-web: searchMaxResults must be a positive integer"
    );
    positive_integer("fetchTimeoutMs", fetch_timeout_ms)?;
    positive_integer("searchTimeoutMs", search_timeout_ms)?;
    anyhow::ensure!(
        fetch_max_output_chars > 0,
        "tool-web: fetchMaxOutputChars must be a positive integer"
    );
    Ok(ResolvedConfig {
        search: config.search.unwrap_or(true),
        fetch: config.fetch.unwrap_or(true),
        search_max_results,
        fetch_timeout_ms,
        search_timeout_ms,
        fetch_max_output_chars: usize::try_from(fetch_max_output_chars).unwrap_or(usize::MAX),
    })
}

fn parse_search(args: SearchArgs) -> anyhow::Result<SearchArgs> {
    anyhow::ensure!(
        !args.query.trim().is_empty(),
        "query must be a non-empty string"
    );
    Ok(args)
}

fn parse_fetch(args: FetchArgs) -> anyhow::Result<FetchArgs> {
    anyhow::ensure!(
        !args.url.trim().is_empty(),
        "url must be a non-empty string"
    );
    Ok(args)
}

fn source_label(source: &WebSearchSource) -> String {
    if let Some(title) = &source.title
        && !title.is_empty()
    {
        return title.clone();
    }
    url::Url::parse(&source.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| source.url.clone())
}

/// Formats one search outcome with citeable Markdown sources.
#[must_use]
pub fn format_search_output(result: &WebSearchResult) -> String {
    let mut parts = Vec::new();
    if let Some(content) = &result.content
        && !content.is_empty()
    {
        parts.push(content.clone());
    }
    if result.sources.is_empty() {
        if result.content.as_ref().is_none_or(String::is_empty) {
            parts.push("No results found.".to_owned());
        }
    } else {
        let lines = result
            .sources
            .iter()
            .map(|source| {
                let mut metadata = Vec::new();
                if let Some(snippet) = &source.snippet
                    && !snippet.is_empty()
                {
                    metadata.push(snippet.clone());
                }
                if let Some(published) = &source.published_at
                    && !published.is_empty()
                {
                    metadata.push(format!("({published})"));
                }
                format!(
                    "- [{}]({}){}",
                    source_label(source),
                    source.url,
                    if metadata.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", metadata.join(" "))
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Sources:\n{lines}"));
    }
    if result.truncated {
        parts.push(format!(
            "(Showing the first {} sources. Refine the query for more.)",
            result.sources.len()
        ));
    }
    parts.push("Cite the relevant URLs above as markdown links in your answer.".to_owned());
    parts.join("\n\n")
}

fn project_source(source: &WebSearchSource) -> WebSource {
    WebSource {
        url: source.url.clone(),
        title: source.title.clone(),
        snippet: source.snippet.clone(),
        published_at: source.published_at.clone(),
    }
}

fn search_meta(result: &WebSearchResult) -> SearchMeta {
    SearchMeta {
        sources: result.sources.iter().map(project_source).collect(),
        truncated: result.truncated,
        answer: result.content.clone(),
    }
}

fn utf16_prefix(text: &str, max_units: usize) -> (String, bool) {
    let units = text.encode_utf16().collect::<Vec<_>>();
    if units.len() <= max_units {
        return (text.to_owned(), false);
    }
    (String::from_utf16_lossy(&units[..max_units]), true)
}

fn exceeds_conversion_depth(html: &str) -> bool {
    let bytes = html.as_bytes();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let Some(relative) = html[index..].find('<') else {
            break;
        };
        index += relative + 1;
        if html[index..].starts_with("!--") {
            if let Some(end) = html[index + 3..].find("-->") {
                index += 3 + end + 3;
                continue;
            }
            break;
        }
        let closing = bytes.get(index) == Some(&b'/');
        if closing {
            index += 1;
        }
        let start = index;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            index += 1;
        }
        if index == start {
            continue;
        }
        let name = html[start..index].to_ascii_lowercase();
        let Some(end) = html[index..].find('>') else {
            break;
        };
        let tag_end = index + end;
        if closing {
            depth = depth.saturating_sub(1);
        } else if !matches!(
            name.as_str(),
            "area"
                | "base"
                | "br"
                | "col"
                | "embed"
                | "hr"
                | "img"
                | "input"
                | "link"
                | "meta"
                | "param"
                | "source"
                | "track"
                | "wbr"
        ) && bytes.get(tag_end.wrapping_sub(1)) != Some(&b'/')
        {
            depth += 1;
            if depth > MAX_CONVERSION_DEPTH {
                return true;
            }
        }
        index = tag_end + 1;
    }
    false
}

fn render_body(body: &WebFetchBody, max_chars: usize) -> (String, bool) {
    let content = match body {
        WebFetchBody::Html { content } | WebFetchBody::Text { content } => content,
    };
    let (content, truncated) = utf16_prefix(content, max_chars);
    match body {
        WebFetchBody::Text { .. } => (content, truncated),
        WebFetchBody::Html { .. } if exceeds_conversion_depth(&content) => (content, truncated),
        WebFetchBody::Html { .. } => {
            let config = html2md_rs::structs::ToMdConfig {
                ignore_rendering: vec![
                    html2md_rs::structs::NodeType::Script,
                    html2md_rs::structs::NodeType::Style,
                ],
            };
            let markdown =
                html2md_rs::to_md::safe_from_html_to_md_with_config(content.clone(), &config)
                    .map(|markdown| convert_raw_tables(&markdown).replace("\\&", "&"))
                    .unwrap_or(content);
            (markdown, truncated)
        }
    }
}

fn convert_raw_tables(markdown: &str) -> String {
    static TABLE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<table\b[^>]*>(.*?)</table>").unwrap());
    static ROW: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<tr\b[^>]*>(.*?)</tr>").unwrap());
    static CELL: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<(?:th|td)\b[^>]*>(.*?)</(?:th|td)>").unwrap());
    static TAG: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<[^>]+>").unwrap());
    TABLE
        .replace_all(markdown, |table: &regex::Captures<'_>| {
            let rows = ROW
                .captures_iter(&table[1])
                .map(|row| {
                    CELL.captures_iter(&row[1])
                        .map(|cell| {
                            TAG.replace_all(&cell[1], "")
                                .trim()
                                .replace('|', "\\|")
                                .replace(['\n', '\r'], "<br>")
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|row| !row.is_empty())
                .collect::<Vec<_>>();
            if rows.is_empty() {
                return table[0].to_owned();
            }
            let width = rows.iter().map(Vec::len).max().unwrap_or(0);
            let cells = |row: &[String]| {
                (0..width)
                    .map(|index| row.get(index).map_or("", String::as_str))
                    .collect::<Vec<_>>()
                    .join(" | ")
            };
            let mut lines = vec![
                format!("| {} |", cells(&rows[0])),
                format!("| {} |", vec!["---"; width].join(" | ")),
            ];
            lines.extend(rows.iter().skip(1).map(|row| format!("| {} |", cells(row))));
            format!("\n{}\n", lines.join("\n"))
        })
        .into_owned()
}

fn render_fetch(result: &WebFetchResult, max_chars: usize) -> RenderedFetch {
    let header = format!("Fetched {} (HTTP {})\n\n", result.url, result.status_code);
    let (body, source_truncated) = render_body(&result.body, max_chars);
    let prefix = format!("{header}{body}");
    let mut truncated =
        result.truncated || source_truncated || prefix.encode_utf16().count() > max_chars;
    let full = format!("{prefix}{}", if truncated { TRUNCATION_FOOTER } else { "" });
    if full.encode_utf16().count() <= max_chars {
        return RenderedFetch {
            text: full,
            truncated,
        };
    }
    truncated = true;
    if max_chars < TRUNCATION_FOOTER.encode_utf16().count() {
        return RenderedFetch {
            text: utf16_prefix(&full, max_chars).0,
            truncated,
        };
    }
    let available = max_chars - TRUNCATION_FOOTER.encode_utf16().count();
    RenderedFetch {
        text: format!("{}{TRUNCATION_FOOTER}", utf16_prefix(&prefix, available).0),
        truncated,
    }
}

/// Formats one fetched body with header and effective truncation footer.
#[must_use]
pub fn format_fetch_output(result: &WebFetchResult, max_chars: usize) -> String {
    render_fetch(result, max_chars).text
}

fn generic_call(title: String, kind: ToolCallKind) -> ToolCallView {
    ToolCallView::Generic(GenericCallView {
        raw_input: Some(Value::String(title.clone())),
        title,
        kind: Some(kind),
        content: None,
        locations: None,
    })
}

fn search_definition(
    web: Arc<WebRuntime>,
    resolved: ResolvedConfig,
) -> anyhow::Result<ToolDefinition> {
    let output = DefineToolOutput::new(
        json!({
            "type": "object", "additionalProperties": false,
            "properties": {
                "content": { "type": "string" },
                "sources": { "type": "array", "required": true, "items": {
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "url": { "type": "string", "required": true },
                        "title": { "type": "string" },
                        "snippet": { "type": "string" },
                        "publishedAt": { "type": "string" }
                    }
                }},
                "truncated": { "type": "boolean", "required": true }
            }
        }),
        Arc::new(|_args: &SearchArgs, value: &WebSearchResult| {
            Ok(vec![ContentBlock::Text {
                text: format_search_output(value),
            }])
        }),
    )
    .presentation_meta(Arc::new(|_args: &SearchArgs, value: &WebSearchResult| {
        Ok(serde_json::to_value(search_meta(value))?)
    }));
    define_tool(
        DefineToolOptions::new(
            "web_search",
            "Search the web for current information. Returns an optional summary answer and a list of source URLs.",
            json!({ "query": { "type": "string", "required": true, "description": "The search query." } }),
            output,
            Arc::new(move |args: SearchArgs, run| {
                let web = web.clone();
                Box::pin(async move {
                    let args = parse_search(args)?;
                    web.search(
                        &WebSearchRequest {
                            query: args.query,
                            max_results: Some(resolved.search_max_results),
                        },
                        Some(run.signal()),
                    )
                    .await
                })
            }),
        )
        .timeout_ms(resolved.search_timeout_ms)
        .concurrency_safe(Arc::new(|_| true))
        .present_call(Arc::new(|args: &SearchArgs| {
            Some(generic_call(args.query.clone(), ToolCallKind::Search))
        }))
        .present_result(Arc::new(|args: &SearchArgs, result: &ToolResult| {
            if result.is_error {
                return None;
            }
            let meta: SearchMeta = serde_json::from_value(result.meta.clone()?).ok()?;
            Some(ToolResultView::Web(WebResultView::Search(
                WebSearchResultView {
                    title: Some(args.query.clone()),
                    sources: meta.sources,
                    answer: meta.answer,
                    truncated: meta.truncated,
                },
            )))
        })),
    )
}

fn fetch_definition(
    web: Arc<WebRuntime>,
    resolved: ResolvedConfig,
) -> anyhow::Result<ToolDefinition> {
    let cache: Arc<Mutex<Option<(WebFetchResult, RenderedFetch)>>> = Arc::new(Mutex::new(None));
    let render_cache = Arc::clone(&cache);
    let meta_cache = Arc::clone(&cache);
    let output = DefineToolOutput::new(
        json!({
            "type": "object", "additionalProperties": false,
            "properties": {
                "url": { "type": "string", "required": true },
                "statusCode": { "type": "integer", "required": true },
                "body": { "required": true, "oneOf": [
                    { "type": "object", "additionalProperties": false, "properties": {
                        "kind": { "type": "string", "required": true, "const": "html" },
                        "content": { "type": "string", "required": true }
                    }},
                    { "type": "object", "additionalProperties": false, "properties": {
                        "kind": { "type": "string", "required": true, "const": "text" },
                        "content": { "type": "string", "required": true }
                    }}
                ]},
                "truncated": { "type": "boolean", "required": true }
            }
        }),
        Arc::new(move |_args: &FetchArgs, value: &WebFetchResult| {
            let rendered = render_fetch(value, resolved.fetch_max_output_chars);
            *render_cache.lock() = Some((value.clone(), rendered.clone()));
            Ok(vec![ContentBlock::Text {
                text: rendered.text,
            }])
        }),
    )
    .presentation_meta(Arc::new(
        move |_args: &FetchArgs, value: &WebFetchResult| {
            let rendered = meta_cache
                .lock()
                .as_ref()
                .filter(|(cached, _)| cached == value)
                .map_or_else(
                    || render_fetch(value, resolved.fetch_max_output_chars),
                    |(_, rendered)| rendered.clone(),
                );
            Ok(serde_json::to_value(FetchMeta {
                url: value.url.clone(),
                status_code: value.status_code,
                truncated: rendered.truncated,
            })?)
        },
    ));
    define_tool(
        DefineToolOptions::new(
            "web_fetch",
            "Fetch the content of a specific HTTP(S) URL and return it decoded to text.",
            json!({ "url": { "type": "string", "required": true, "description": "The HTTP(S) URL to fetch." } }),
            output,
            Arc::new(move |args: FetchArgs, run| {
                let web = web.clone();
                Box::pin(async move {
                    let args = parse_fetch(args)?;
                    web.fetch(&WebFetchRequest { url: args.url }, Some(run.signal())).await
                })
            }),
        )
        .timeout_ms(resolved.fetch_timeout_ms)
        .concurrency_safe(Arc::new(|_| true))
        .present_call(Arc::new(|args: &FetchArgs| {
            Some(generic_call(args.url.clone(), ToolCallKind::Fetch))
        }))
        .present_result(Arc::new(|args: &FetchArgs, result: &ToolResult| {
            if result.is_error {
                return None;
            }
            let meta: FetchMeta = serde_json::from_value(result.meta.clone()?).ok()?;
            Some(ToolResultView::Web(WebResultView::Fetch(WebFetchResultView {
                title: Some(args.url.clone()),
                url: meta.url,
                status_code: meta.status_code,
                truncated: meta.truncated,
            })))
        })),
    )
}

fn rollback(effects: Vec<EffectHandle>, error: anyhow::Error) -> anyhow::Error {
    let failures = effects
        .into_iter()
        .rev()
        .filter_map(|effect| futures::executor::block_on(effect.dispose()).err())
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        error
    } else {
        anyhow::anyhow!(
            "{error:#}; web tool rollback failed: {}",
            failures.join("; ")
        )
    }
}

/// Registers enabled web tools and their prompt guidance.
///
/// # Errors
///
/// Returns invalid config, missing-service, schema, prompt, or duplicate-tool failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    let resolved = resolve_config(config)?;
    let web = context
        .get(WEB)
        .ok_or_else(|| anyhow::anyhow!("tool-web requires web"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-web requires tools"))?;
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-web requires systemPrompt"))?;
    let mut effects = Vec::new();
    if resolved.search {
        let guidance = if resolved.fetch {
            "Use the web_search tool to discover current information on the web. It returns an optional answer plus a list of source URLs. Follow up with web_fetch when you need the full content of a specific result, and cite the relevant URLs as markdown links."
        } else {
            "Use the web_search tool to discover current information on the web. It returns an optional answer plus a list of source URLs. Use the returned source snippets when available, and cite the relevant URLs as markdown links."
        };
        match prompt.section(
            context,
            PromptSection::new("tool:web_search", 110.0, guidance),
        ) {
            Ok(effect) => effects.push(effect),
            Err(error) => return Err(error),
        }
        match tools.register(context, search_definition(web.clone(), resolved)?) {
            Ok(effect) => effects.push(effect),
            Err(error) => return Err(rollback(effects, error)),
        }
    }
    if resolved.fetch {
        match prompt.section(
            context,
            PromptSection::new(
                "tool:web_fetch",
                111.0,
                "Use the web_fetch tool to retrieve the content of a specific HTTP(S) URL (for example a result from web_search). It returns the page content decoded to text. Cite the URL as a markdown link when you use its content.",
            ),
        ) {
            Ok(effect) => effects.push(effect),
            Err(error) => return Err(rollback(effects, error)),
        }
        match tools.register(context, fetch_definition(web, resolved)?) {
            Ok(effect) => effects.push(effect),
            Err(error) => return Err(rollback(effects, error)),
        }
    }
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let mut config = if value.is_null() {
        Config::default()
    } else {
        serde_json::from_value::<Config>(value.clone())?
    };
    config.search.get_or_insert(true);
    config.fetch.get_or_insert(true);
    config
        .search_max_results
        .get_or_insert(WEB_SEARCH_MAX_RESULTS);
    config
        .fetch_timeout_ms
        .get_or_insert(DEFAULT_WEB_TOOL_TIMEOUT_MS);
    config
        .search_timeout_ms
        .get_or_insert(DEFAULT_WEB_TOOL_TIMEOUT_MS);
    config
        .fetch_max_output_chars
        .get_or_insert(DEFAULT_FETCH_MAX_OUTPUT_CHARS);
    resolve_config(config)?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible web tool plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value::<Config>(config)?;
            apply(&context, config)
        })
    })
    .with_config_validator(normalize_config)
}
