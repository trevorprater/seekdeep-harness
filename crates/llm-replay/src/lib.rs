//! Keyless model replay derived from durable session logs.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_stream::try_stream;
use async_trait::async_trait;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regress::Regex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_core::{chunk_rows::decode_storage_record, session::SessionEvent};
use seekdeep_llm::{
    AbortSignal, AdapterRegistrationHandle, AdapterStream, ContentBlock, GenerateOptions, LLM,
    LlmAdapter, LlmError, LlmModelContext, LlmModelInfo, LlmModelReasoningInfo, LlmProviderInfo,
    LlmReasoningEffortInfo, LlmResolvedModelInfo, LlmStream, ModelId, ModelModality, ProviderId,
    ReasoningEffortId, ResolvedRetryPolicy, StreamChunk, TokenUsage, resolve_retry_policy,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Loader plugin name.
pub const NAME: &str = "llm-replay";
/// Service dependency owned by the replay route.
pub const INJECT: &[&str] = &["llm"];
const ANONYMOUS_SESSION: &str = "\0anon\0";
const FROM_REQUEST_OPEN: &str = "{{fromRequest:";

/// One recorded model call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ReplayEntry {
    /// A normally yielded stream.
    Chunks {
        /// Complete recorded chunk sequence.
        chunks: Vec<StreamChunk>,
    },
    /// Prefix chunks followed by a typed model failure.
    Throw {
        /// Chunks emitted before failure.
        chunks: Vec<StreamChunk>,
        /// Failure message.
        message: String,
        /// Stable failure code.
        code: String,
    },
    /// A partial stream that waits until cancellation.
    Hang {
        /// Optional readiness marker written immediately before waiting.
        #[serde(rename = "readyFile", default, skip_serializing_if = "Option::is_none")]
        ready_file: Option<PathBuf>,
    },
}

/// One replay-only model catalog row.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayModelConfig {
    /// Model route id.
    pub id: String,
    /// Selector label.
    #[serde(default)]
    pub name: Option<String>,
    /// Selector description.
    #[serde(default)]
    pub description: Option<String>,
    /// Combined context capacity.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Accepted modalities.
    #[serde(default)]
    pub input_modalities: Option<Vec<String>>,
    /// Adapter-owned output default.
    #[serde(default)]
    pub default_max_tokens: Option<u64>,
    /// Accepted reasoning effort ids.
    #[serde(default)]
    pub reasoning_efforts: Option<Vec<String>>,
    /// Adapter-owned reasoning default.
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
}

/// One replay-only provider catalog row.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayProviderConfig {
    /// Provider route id.
    pub id: String,
    /// Selector label.
    #[serde(default)]
    pub name: Option<String>,
    /// Advisory model rows.
    #[serde(default)]
    pub models: Vec<ReplayModelConfig>,
    /// Provider retry policy in the normal LLM wire shape.
    #[serde(default)]
    pub retry_policy: Option<Value>,
}

/// Fully resolved replay input.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayConfig {
    /// Primary session JSONL.
    pub file: PathBuf,
    /// Optional primary override sidecar.
    pub override_file: Option<PathBuf>,
    /// Recorded child-session JSONL files.
    pub child_files: Vec<PathBuf>,
    /// Optional discoverable provider catalog.
    pub providers: Vec<ReplayProviderConfig>,
    /// Per-chunk pacing delay.
    pub pace_ms: f64,
}

/// Loader-facing config with environment fallbacks.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Primary fixture path; defaults `SEEKDEEP_SNAPSHOT_FILE`.
    pub file: Option<PathBuf>,
    /// Override sidecar; defaults `SEEKDEEP_SNAPSHOT_OVERRIDE`.
    pub override_file: Option<PathBuf>,
    /// Child fixtures; defaults the path-delimited `SEEKDEEP_SNAPSHOT_CHILD_FILES`.
    pub child_files: Option<Vec<PathBuf>>,
    /// Replay-only provider catalog.
    pub providers: Option<Vec<ReplayProviderConfig>>,
    /// Optional per-chunk pacing delay.
    pub pace_ms: Option<f64>,
}

/// Recorded calls and ordering identity for one session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionScript {
    /// Recorded id, used only for diagnostics and stable sorting.
    pub recorded_id: String,
    /// Recorded creation time.
    pub created_at: f64,
    /// Calls in stream invocation order.
    pub entries: Vec<ReplayEntry>,
    /// Whether this is the primary fixture.
    pub primary: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayOverridePatch {
    at: Value,
    entry: ReplayEntry,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayOverridePatches {
    patches: Vec<ReplayOverridePatch>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ReplayOverrideDoc {
    Entries(Vec<ReplayEntry>),
    Patches(ReplayOverridePatches),
}

/// Parses a session JSONL buffer, skipping its header and expanding packed rows.
///
/// # Errors
///
/// Returns malformed JSON, packed-row, or event-envelope failures.
pub fn parse_session_log(text: &str) -> anyhow::Result<Vec<SessionEvent>> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let _ = lines.next();
    let mut events = Vec::new();
    for line in lines {
        let record: Value = serde_json::from_str(line)?;
        for event in decode_storage_record(record)? {
            events.push(serde_json::from_value(event)?);
        }
    }
    Ok(events)
}

/// Reads replay ordering facts from the first non-empty JSONL line.
///
/// # Errors
///
/// Returns malformed JSON or wrong-typed present header facts.
pub fn parse_session_header(text: &str) -> anyhow::Result<(String, f64, usize)> {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("{}");
    let facts: Value = serde_json::from_str(line)?;
    Ok((
        facts
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        facts
            .get("createdAt")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        facts
            .get("seedLength")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(f64::trunc)
            .and_then(|value| format!("{value:.0}").parse::<usize>().ok())
            .unwrap_or(0),
    ))
}

/// Reconstructs one recorded stream call per terminal chunk sequence.
///
/// # Errors
///
/// Returns malformed chunk data or an unterminated recorded call.
pub fn derive_replay_script(events: &[SessionEvent]) -> anyhow::Result<Vec<ReplayEntry>> {
    fn close(
        script: &mut Vec<ReplayEntry>,
        key: Option<&str>,
        chunks: &mut Vec<StreamChunk>,
    ) -> anyhow::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            matches!(chunks.last(), Some(StreamChunk::Finish { .. })),
            "llm-replay: model call {} ended without a finish chunk (a thrown stream); this scenario needs a replay.override.json sidecar",
            key.unwrap_or("undefined")
        );
        script.push(ReplayEntry::Chunks {
            chunks: std::mem::take(chunks),
        });
        Ok(())
    }

    let mut script = Vec::new();
    let mut current_key: Option<String> = None;
    let mut current = Vec::new();
    for event in events {
        if event.event_type == "compaction/summary" {
            close(&mut script, current_key.as_deref(), &mut current)?;
            current_key = None;
            if event.data.get("llmStreamCall") == Some(&Value::Bool(true)) {
                let output = event.data.get("rawOutput").ok_or_else(|| {
                    anyhow::anyhow!(
                        "llm-replay: compaction/summary marks an LLM stream call without rawOutput"
                    )
                })?;
                let output: Vec<ContentBlock> = serde_json::from_value(output.clone())?;
                let mut chunks = Vec::new();
                for (index, block) in output.into_iter().enumerate() {
                    let index = u64::try_from(index)?;
                    chunks.push(StreamChunk::BlockStart {
                        index,
                        block_type: block.block_type().to_owned(),
                    });
                    chunks.push(StreamChunk::BlockEnd { index, block });
                }
                if let Some(usage) = event.data.get("usage") {
                    chunks.push(StreamChunk::Usage {
                        usage: serde_json::from_value::<TokenUsage>(usage.clone())?,
                    });
                }
                chunks.push(StreamChunk::Finish {
                    reason: seekdeep_llm::FinishReason::Stop,
                    replay_state: None,
                });
                script.push(ReplayEntry::Chunks { chunks });
            }
            continue;
        }
        if event.event_type != "assistant/chunk" {
            continue;
        }
        let turn = render_key(event.data.get("turn"));
        let step = render_key(event.data.get("step"));
        let key = format!("{turn}/{step}");
        if !current.is_empty() && current_key.as_deref() != Some(&key) {
            close(&mut script, current_key.as_deref(), &mut current)?;
        }
        if current.is_empty() {
            current_key = Some(key);
        }
        let chunk: StreamChunk = serde_json::from_value(
            event
                .data
                .get("chunk")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("llm-replay: assistant/chunk has no chunk"))?,
        )?;
        let finished = matches!(chunk, StreamChunk::Finish { .. });
        current.push(chunk);
        if finished {
            close(&mut script, current_key.as_deref(), &mut current)?;
            current_key = None;
        }
    }
    close(&mut script, current_key.as_deref(), &mut current)?;
    Ok(script)
}

fn render_key(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => "undefined".to_owned(),
    }
}

/// Loads the primary derived or overridden script.
///
/// # Errors
///
/// Returns missing fixtures, malformed sidecars, invalid entries, or derivation failures.
pub fn load_replay_script(config: &ReplayConfig) -> anyhow::Result<Vec<ReplayEntry>> {
    if let Some(override_file) = config.override_file.as_ref().filter(|path| path.exists()) {
        let raw = std::fs::read_to_string(override_file)?;
        let document: ReplayOverrideDoc = serde_json::from_str(&raw).map_err(|error| {
            anyhow::anyhow!(
                "llm-replay: invalid override {}: {error}",
                override_file.display()
            )
        })?;
        match document {
            ReplayOverrideDoc::Entries(entries) => {
                validate_entries(&entries, override_file)?;
                return Ok(entries);
            }
            ReplayOverrideDoc::Patches(document) => {
                let mut script = derive_script_from_file(&config.file)?;
                let length = script.len();
                let mut seen = BTreeSet::new();
                for (patch_index, patch) in document.patches.into_iter().enumerate() {
                    let at = replay_patch_index(&patch.at, override_file, patch_index)?;
                    anyhow::ensure!(
                        at <= length,
                        "llm-replay: override patch index {} out of range (derived script has {length} call(s); == length appends): {}",
                        at,
                        override_file.display()
                    );
                    anyhow::ensure!(
                        seen.insert(at),
                        "llm-replay: duplicate override patch index {}: {}",
                        at,
                        override_file.display()
                    );
                    validate_entries(std::slice::from_ref(&patch.entry), override_file)?;
                    if at == script.len() {
                        script.push(patch.entry);
                    } else {
                        script[at] = patch.entry;
                    }
                }
                return Ok(script);
            }
        }
    }
    derive_script_from_file(&config.file)
}

fn replay_patch_index(value: &Value, file: &Path, patch: usize) -> anyhow::Result<usize> {
    let number = value.as_f64().unwrap_or(f64::NAN);
    anyhow::ensure!(
        number.is_finite()
            && number.fract() == 0.0
            && (0.0..=9_007_199_254_740_991.0).contains(&number),
        "llm-replay: invalid override {}: patch {patch} at must be a non-negative safe integer",
        file.display()
    );
    let integer = format!("{number:.0}").parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "llm-replay: invalid override {}: patch {patch} at must be a non-negative safe integer",
            file.display()
        )
    })?;
    usize::try_from(integer).map_err(|_| {
        anyhow::anyhow!(
            "llm-replay: invalid override {}: patch {patch} at must fit this platform's index range",
            file.display()
        )
    })
}

fn validate_entries(entries: &[ReplayEntry], file: &Path) -> anyhow::Result<()> {
    for entry in entries {
        match entry {
            ReplayEntry::Throw { message, code, .. } => {
                anyhow::ensure!(
                    !message.is_empty(),
                    "llm-replay: invalid override {}: message must be a non-empty string",
                    file.display()
                );
                anyhow::ensure!(
                    !code.is_empty(),
                    "llm-replay: invalid override {}: code must be a non-empty string",
                    file.display()
                );
            }
            ReplayEntry::Hang {
                ready_file: Some(path),
            } => anyhow::ensure!(
                !path.as_os_str().is_empty(),
                "llm-replay: invalid override {}: readyFile must be a non-empty string",
                file.display()
            ),
            ReplayEntry::Chunks { .. } | ReplayEntry::Hang { ready_file: None } => {}
        }
    }
    Ok(())
}

fn derive_script_from_file(file: &Path) -> anyhow::Result<Vec<ReplayEntry>> {
    anyhow::ensure!(
        file.exists(),
        "llm-replay: fixture not found: {} — run `pnpm run test:snapshot:record` first",
        file.display()
    );
    derive_replay_script(&parse_session_log(&std::fs::read_to_string(file)?)?)
}

/// Loads primary and child scripts in first-call binding order.
///
/// # Errors
///
/// Returns any primary/child file, header, or derivation failure.
pub fn load_session_scripts(config: &ReplayConfig) -> anyhow::Result<Vec<SessionScript>> {
    let primary_entries = load_replay_script(config)?;
    let (recorded_id, created_at, _) = if config.file.exists() {
        parse_session_header(&std::fs::read_to_string(&config.file)?)?
    } else {
        (String::new(), 0.0, 0)
    };
    let primary = SessionScript {
        recorded_id,
        created_at,
        entries: primary_entries,
        primary: true,
    };
    let mut children = Vec::new();
    for file in &config.child_files {
        anyhow::ensure!(
            file.exists(),
            "llm-replay: child fixture not found: {} — re-record the scenario",
            file.display()
        );
        let text = std::fs::read_to_string(file)?;
        let (recorded_id, created_at, seed_length) = parse_session_header(&text)?;
        let events = parse_session_log(&text)?;
        let own = events.get(seed_length..).unwrap_or_default();
        children.push(SessionScript {
            recorded_id,
            created_at,
            entries: derive_replay_script(own)?,
            primary: false,
        });
    }
    children.sort_by(|left, right| {
        left.created_at
            .total_cmp(&right.created_at)
            .then_with(|| left.recorded_id.cmp(&right.recorded_id))
    });
    let mut scripts = vec![primary];
    scripts.extend(children);
    Ok(scripts)
}

fn collect_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(values) => {
            for value in values {
                collect_strings(value, output);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_strings(value, output);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn resolve_from_request(pattern: &str, corpus: &str) -> anyhow::Result<String> {
    let regex = Regex::new(pattern).map_err(|error| {
        anyhow::anyhow!("llm-replay: fromRequest has an invalid pattern {pattern:?}: {error}")
    })?;
    let matched = regex.find_iter(corpus).last().ok_or_else(|| {
        anyhow::anyhow!(
            "llm-replay: fromRequest pattern {pattern:?} matched nothing in the request"
        )
    })?;
    let range = matched.group(1).unwrap_or_else(|| matched.range());
    Ok(corpus[range].to_owned())
}

fn substitute_string(text: &str, corpus: &str) -> anyhow::Result<String> {
    let mut result = String::new();
    let mut cursor = 0;
    loop {
        let Some(relative_open) = text[cursor..].find(FROM_REQUEST_OPEN) else {
            result.push_str(&text[cursor..]);
            return Ok(result);
        };
        let open = cursor + relative_open;
        let pattern_start = open + FROM_REQUEST_OPEN.len();
        let Some(relative_close) = text[pattern_start..].find("}}") else {
            anyhow::bail!("llm-replay: fromRequest placeholder is unterminated in {text:?}");
        };
        let mut close = pattern_start + relative_close;
        while text.as_bytes().get(close + 2) == Some(&b'}') {
            close += 1;
        }
        result.push_str(&text[cursor..open]);
        result.push_str(&resolve_from_request(&text[pattern_start..close], corpus)?);
        cursor = close + 2;
    }
}

fn substitute_value(value: &mut Value, corpus: &str) -> anyhow::Result<()> {
    match value {
        Value::String(text) if text.contains(FROM_REQUEST_OPEN) => {
            *text = substitute_string(text, corpus)?;
        }
        Value::Array(values) => {
            for value in values {
                substitute_value(value, corpus)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                substitute_value(value, corpus)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

/// Resolves request-derived placeholders in a detached scripted entry.
///
/// # Errors
///
/// Returns invalid, unmatched, or unterminated pattern failures.
pub fn resolve_scripted_entry(
    entry: &ReplayEntry,
    messages: &[seekdeep_llm::Message],
) -> anyhow::Result<ReplayEntry> {
    let mut value = serde_json::to_value(entry)?;
    if !value.to_string().contains(FROM_REQUEST_OPEN) {
        return Ok(entry.clone());
    }
    let mut strings = Vec::new();
    collect_strings(&serde_json::to_value(messages)?, &mut strings);
    substitute_value(&mut value, &strings.join("\n"))?;
    Ok(serde_json::from_value(value)?)
}

#[derive(Clone, Debug)]
struct BoundScript {
    entries: Vec<ReplayEntry>,
    cursor: usize,
}

#[derive(Debug)]
struct ReplayState {
    scripts: Vec<SessionScript>,
    bound: IndexMap<String, BoundScript>,
    next_script: usize,
}

#[derive(Debug)]
struct ReplayInvocation {
    entry: Option<ReplayEntry>,
    messages: Vec<seekdeep_llm::Message>,
    signal: Option<AbortSignal>,
    pace_ms: f64,
    unrecorded: bool,
    seen_sessions: usize,
    total_scripts: usize,
    index: usize,
    script_length: usize,
}

#[derive(Debug)]
struct ReplayMachine {
    state: Mutex<ReplayState>,
    pace_ms: f64,
}

impl ReplayMachine {
    fn claim(&self, options: &GenerateOptions) -> ReplayInvocation {
        let key = options
            .session_id
            .as_ref()
            .map_or_else(|| ANONYMOUS_SESSION.to_owned(), ToString::to_string);
        let mut state = self.state.lock();
        if !state.bound.contains_key(&key) {
            if let Some(script) = state.scripts.get(state.next_script).cloned() {
                state.next_script += 1;
                state.bound.insert(
                    key.clone(),
                    BoundScript {
                        entries: script.entries,
                        cursor: 0,
                    },
                );
            } else {
                return ReplayInvocation {
                    entry: None,
                    messages: options.messages.clone(),
                    signal: options.signal.clone(),
                    pace_ms: self.pace_ms,
                    unrecorded: true,
                    seen_sessions: state.next_script,
                    total_scripts: state.scripts.len(),
                    index: 0,
                    script_length: 0,
                };
            }
        }
        let seen_sessions = state.next_script;
        let total_scripts = state.scripts.len();
        let bound = state.bound.get_mut(&key).expect("bound above");
        let index = bound.cursor;
        bound.cursor += 1;
        ReplayInvocation {
            entry: bound.entries.get(index).cloned(),
            messages: options.messages.clone(),
            signal: options.signal.clone(),
            pace_ms: self.pace_ms,
            unrecorded: false,
            seen_sessions,
            total_scripts,
            index,
            script_length: bound.entries.len(),
        }
    }
}

fn replay_stream(
    invocation: ReplayInvocation,
) -> impl futures::Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static {
    try_stream! {
        if invocation.unrecorded {
            Err::<(), anyhow::Error>(anyhow::anyhow!(
                "llm-replay: a model call arrived from an unrecorded session (#{}); the scenario recorded only {} session(s) — re-record it",
                invocation.seen_sessions + 1,
                invocation.total_scripts
            ))?;
        }
        let entry = invocation.entry.ok_or_else(|| anyhow::anyhow!(
            "llm-replay: script exhausted — session requested model call #{} but its script has only {}; re-record the scenario",
            invocation.index + 1,
            invocation.script_length
        ))?;
        let entry = resolve_scripted_entry(&entry, &invocation.messages)?;
        match entry {
            ReplayEntry::Chunks { chunks } => {
                for chunk in chunks {
                    ensure_not_aborted(invocation.signal.as_ref())?;
                    pace(invocation.pace_ms, invocation.signal.as_ref()).await?;
                    yield chunk;
                }
            }
            ReplayEntry::Throw { chunks, message, code } => {
                for chunk in chunks {
                    ensure_not_aborted(invocation.signal.as_ref())?;
                    pace(invocation.pace_ms, invocation.signal.as_ref()).await?;
                    yield chunk;
                }
                Err::<(), anyhow::Error>(LlmError::simple(message, code).into())?;
            }
            ReplayEntry::Hang { ready_file } => {
                yield StreamChunk::BlockStart { index: 0, block_type: "text".to_owned() };
                yield StreamChunk::TextDelta { index: 0, text: "partial".to_owned() };
                if let Some(path) = ready_file {
                    std::fs::write(path, [])?;
                }
                match invocation.signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
                Err::<(), anyhow::Error>(anyhow::anyhow!("aborted"))?;
            }
        }
    }
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    anyhow::ensure!(!signal.is_some_and(AbortSignal::is_aborted), "aborted");
    Ok(())
}

async fn pace(milliseconds: f64, signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if milliseconds <= 0.0 {
        return Ok(());
    }
    let normalized = if (1.0..=2_147_483_647.0).contains(&milliseconds) {
        milliseconds
    } else {
        1.0
    };
    let duration = Duration::from_secs_f64(normalized / 1_000.0);
    if let Some(signal) = signal {
        tokio::select! {
            () = tokio::time::sleep(duration) => Ok(()),
            () = signal.cancelled() => anyhow::bail!("aborted"),
        }
    } else {
        tokio::time::sleep(duration).await;
        Ok(())
    }
}

#[derive(Debug)]
struct ReplayAdapter {
    providers: IndexMap<String, PreparedProvider>,
    replay: Arc<ReplayMachine>,
}

#[derive(Clone, Debug)]
struct PreparedProvider {
    config: ReplayProviderConfig,
    retry_policy: Option<ResolvedRetryPolicy>,
}

#[async_trait]
impl LlmAdapter for ReplayAdapter {
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        let configured = &self.providers[provider].config;
        LlmProviderInfo {
            id: ProviderId::new(provider),
            name: configured
                .name
                .clone()
                .unwrap_or_else(|| provider.to_owned()),
        }
    }

    fn provider_retry_policy(&self, provider: &str) -> Option<ResolvedRetryPolicy> {
        self.providers[provider].retry_policy.clone()
    }

    async fn list_models(&self, provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        Ok(self.providers[provider]
            .config
            .models
            .iter()
            .map(|model| LlmModelInfo {
                provider: ProviderId::new(provider),
                id: ModelId::new(&model.id),
                name: model.name.clone().unwrap_or_else(|| model.id.clone()),
                description: model.description.clone(),
                input_modalities: model
                    .input_modalities
                    .as_ref()
                    .map(|modalities| modalities.iter().cloned().map(ModelModality).collect()),
            })
            .collect())
    }

    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        let configured = self.providers[provider]
            .config
            .models
            .iter()
            .find(|candidate| candidate.id == model);
        Ok(LlmResolvedModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new(model),
            name: configured
                .and_then(|model| model.name.clone())
                .unwrap_or_else(|| model.to_owned()),
            description: configured.and_then(|model| model.description.clone()),
            input_modalities: configured
                .and_then(|model| model.input_modalities.as_ref())
                .map(|modalities| modalities.iter().cloned().map(ModelModality).collect()),
            context: configured
                .and_then(|model| model.context_window)
                .map(|context_window| LlmModelContext { context_window }),
            default_max_tokens: configured.and_then(|model| model.default_max_tokens),
            reasoning: configured
                .and_then(|model| {
                    model
                        .reasoning_efforts
                        .as_ref()
                        .map(|efforts| (model, efforts))
                })
                .map(|(model, efforts)| LlmModelReasoningInfo {
                    efforts: efforts
                        .iter()
                        .map(|effort| LlmReasoningEffortInfo {
                            id: ReasoningEffortId::new(effort),
                            name: effort.clone(),
                            description: None,
                        })
                        .collect(),
                    default_effort: model
                        .default_reasoning_effort
                        .as_ref()
                        .map(ReasoningEffortId::new),
                }),
        })
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(replay_stream(self.replay.claim(&options)))
    }
}

enum ReplayDisposer {
    Adapter(AdapterRegistrationHandle),
    Middleware(EffectHandle),
}

/// Live replay registration and strict fixture-consumption audit.
pub struct ReplayHandle {
    state: Arc<ReplayMachine>,
    disposer: ReplayDisposer,
}

impl ReplayHandle {
    /// Removes this replay contribution.
    ///
    /// # Errors
    ///
    /// Returns the owning registration cleanup failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        match &self.disposer {
            ReplayDisposer::Adapter(handle) => handle.dispose().await,
            ReplayDisposer::Middleware(handle) => handle.dispose().await,
        }
    }

    /// Fails unless every script was bound and every bound call consumed.
    ///
    /// # Errors
    ///
    /// Returns a complete underrun diagnostic.
    pub fn assert_consumed(&self) -> anyhow::Result<()> {
        let state = self.state.state.lock();
        let mut problems = Vec::new();
        if state.next_script < state.scripts.len() {
            problems.push(format!(
                "{} recorded script(s) never bound to a live session",
                state.scripts.len() - state.next_script
            ));
        }
        for (key, bound) in &state.bound {
            if bound.cursor < bound.entries.len() {
                let who = if key == ANONYMOUS_SESSION {
                    "the anonymous session".to_owned()
                } else {
                    format!("session {key}")
                };
                problems.push(format!(
                    "{who} consumed {}/{} recorded call(s)",
                    bound.cursor,
                    bound.entries.len()
                ));
            }
        }
        anyhow::ensure!(
            problems.is_empty(),
            "llm-replay: fixture not fully consumed — {}; the scenario drove fewer model calls than recorded",
            problems.join("; ")
        );
        Ok(())
    }
}

/// Installs positional replay through a routed adapter or catch-all middleware.
///
/// # Errors
///
/// Returns invalid config, fixture, catalog, or registration failures.
pub fn install_llm_replay(context: &Context, config: ReplayConfig) -> anyhow::Result<ReplayHandle> {
    anyhow::ensure!(
        config.pace_ms.is_finite() && config.pace_ms >= 0.0 && config.pace_ms.fract() == 0.0,
        "llm-replay: paceMs must be a non-negative integer, got {}",
        config.pace_ms
    );
    validate_modalities(&config.providers)?;
    let scripts = load_session_scripts(&config)?;
    let replay = Arc::new(ReplayMachine {
        state: Mutex::new(ReplayState {
            scripts,
            bound: IndexMap::new(),
            next_script: 0,
        }),
        pace_ms: config.pace_ms,
    });
    let llm = context
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("llm-replay requires llm"))?;
    let disposer = if config.providers.is_empty() {
        let replay_for_middleware = Arc::clone(&replay);
        ReplayDisposer::Middleware(llm.register_stream_middleware(
            context,
            Arc::new(move |options, _next| {
                LlmStream::new(replay_stream(replay_for_middleware.claim(&options)))
            }),
            false,
        )?)
    } else {
        let mut prepared = IndexMap::new();
        for provider in config.providers {
            let retry_policy = provider
                .retry_policy
                .as_ref()
                .map(|policy| {
                    resolve_retry_policy(
                        Some(policy),
                        &format!("llm-replay: provider {:?} retryPolicy", provider.id),
                    )
                })
                .transpose()?;
            prepared.insert(
                provider.id.clone(),
                PreparedProvider {
                    config: provider,
                    retry_policy,
                },
            );
        }
        let routes = prepared.keys().cloned().collect::<Vec<_>>();
        let adapter: Arc<dyn LlmAdapter> = Arc::new(ReplayAdapter {
            providers: prepared,
            replay: Arc::clone(&replay),
        });
        ReplayDisposer::Adapter(llm.register_adapter(&routes, adapter)?)
    };
    Ok(ReplayHandle {
        state: replay,
        disposer,
    })
}

fn validate_modalities(providers: &[ReplayProviderConfig]) -> anyhow::Result<()> {
    for provider in providers {
        for model in &provider.models {
            if let Some(modalities) = &model.input_modalities {
                anyhow::ensure!(
                    modalities
                        .iter()
                        .all(|modality| matches!(modality.as_str(), "text" | "image")),
                    "llm-replay: provider {:?} model {:?} inputModalities must be an array containing only \"text\" and \"image\"",
                    provider.id,
                    model.id
                );
            }
        }
    }
    Ok(())
}

/// Resolves environment fallbacks and installs replay.
///
/// # Errors
///
/// Returns missing fixture or any installation failure.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<ReplayHandle> {
    let file = config
        .file
        .or_else(|| std::env::var_os("SEEKDEEP_SNAPSHOT_FILE").map(PathBuf::from))
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "llm-replay: a fixture path is required (Config.file or $SEEKDEEP_SNAPSHOT_FILE)"
            )
        })?;
    let override_file = config
        .override_file
        .or_else(|| std::env::var_os("SEEKDEEP_SNAPSHOT_OVERRIDE").map(PathBuf::from))
        .filter(|path| !path.as_os_str().is_empty());
    let child_files = config.child_files.unwrap_or_else(|| {
        std::env::var("SEEKDEEP_SNAPSHOT_CHILD_FILES")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| {
                value
                    .split(if cfg!(windows) { ';' } else { ':' })
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    });
    install_llm_replay(
        context,
        ReplayConfig {
            file,
            override_file,
            child_files,
            providers: config.providers.unwrap_or_default(),
            pace_ms: config.pace_ms.unwrap_or(0.0),
        },
    )
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config: Config = serde_json::from_value(value.clone())?;
    validate_modalities(config.providers.as_deref().unwrap_or_default())?;
    Ok(serde_json::to_value(value)?)
}

/// Builds the Loader-compatible replay plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let _ = apply(&context, serde_json::from_value(config)?)?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use seekdeep_llm::{FinishReason, LlmRuntime, Message, SessionId};
    use serde_json::json;

    use super::*;

    fn chunks(text: &str) -> Vec<StreamChunk> {
        vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: "text".to_owned(),
            },
            StreamChunk::TextDelta {
                index: 0,
                text: text.to_owned(),
            },
            StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Text {
                    text: text.to_owned(),
                },
            },
            StreamChunk::Usage {
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
            },
            StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            },
        ]
    }

    fn event(seq: u64, turn: u64, step: u64, chunk: &StreamChunk) -> SessionEvent {
        SessionEvent {
            event_type: "assistant/chunk".to_owned(),
            seq,
            time: 0,
            data: json!({"turn":turn,"step":step,"chunk":chunk}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn jsonl(id: &str, created_at: u64, calls: &[Vec<StreamChunk>]) -> String {
        let mut lines = vec![
            json!({
                "type":"session","version":0,"id":id,"createdAt":created_at
            })
            .to_string(),
        ];
        let mut seq = 1;
        for (step, call) in calls.iter().enumerate() {
            for chunk in call {
                lines.push(
                    serde_json::to_string(&event(seq, 1, u64::try_from(step + 1).unwrap(), chunk))
                        .unwrap(),
                );
                seq += 1;
            }
        }
        format!("{}\n", lines.join("\n"))
    }

    fn config(file: PathBuf) -> ReplayConfig {
        ReplayConfig {
            file,
            override_file: None,
            child_files: Vec::new(),
            providers: Vec::new(),
            pace_ms: 0.0,
        }
    }

    async fn drain(stream: LlmStream) -> anyhow::Result<Vec<StreamChunk>> {
        stream.collect::<Vec<_>>().await.into_iter().collect()
    }

    #[test]
    fn parses_blank_and_packed_rows_and_header_defaults() {
        let header = r#"{"type":"session","version":0,"id":"s","createdAt":7,"seedLength":2}"#;
        let row = json!({
            "type":"text-chunks","seq0":1,"time0":0,
            "data":{"turn":1,"step":1,"index":0,"dt":[0,0],"texts":["a","b","c"]}
        });
        let parsed = parse_session_log(&format!("{header}\n\n{row}\n")).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[2].data["chunk"]["text"], "c");
        assert_eq!(
            parse_session_header(&format!("\n{header}\n")).unwrap(),
            ("s".to_owned(), 7.0, 2)
        );
        assert_eq!(parse_session_header("").unwrap(), (String::new(), 0.0, 0));
        assert_eq!(
            parse_session_header(r#"{"seedLength":2.9}"#).unwrap(),
            (String::new(), 0.0, 2)
        );
        assert_eq!(
            parse_session_header(r#"{"seedLength":-1}"#).unwrap(),
            (String::new(), 0.0, 0)
        );
    }

    #[test]
    fn derives_retries_compaction_and_rejects_unfinished_calls() {
        let first = chunks("one");
        let second = chunks("two");
        let mut events = Vec::new();
        let mut seq = 1;
        for chunk in &first {
            events.push(event(seq, 1, 1, chunk));
            seq += 1;
        }
        events.push(SessionEvent {
            event_type: "compaction/summary".to_owned(),
            seq,
            time: 0,
            data: json!({
                "llmStreamCall":true,
                "rawOutput":[{"type":"text","text":"summary"}],
                "usage":{"inputTokens":2,"outputTokens":3}
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        });
        seq += 1;
        for chunk in &second {
            events.push(event(seq, 1, 1, chunk));
            seq += 1;
        }
        let script = derive_replay_script(&events).unwrap();
        assert_eq!(script.len(), 3);
        assert!(matches!(&script[1], ReplayEntry::Chunks { chunks } if chunks.len() == 4));
        let unfinished = [event(
            1,
            4,
            2,
            &StreamChunk::TextDelta {
                index: 0,
                text: "partial".to_owned(),
            },
        )];
        assert!(
            derive_replay_script(&unfinished)
                .unwrap_err()
                .to_string()
                .contains("4/2")
        );
    }

    #[test]
    fn loads_whole_and_patch_overrides_with_strict_indexes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("session.jsonl");
        std::fs::write(&file, jsonl("p", 1, &[chunks("one"), chunks("two")])).unwrap();
        let override_file = root.path().join("replay.override.json");
        std::fs::write(
            &override_file,
            serde_json::to_vec(&vec![ReplayEntry::Hang { ready_file: None }]).unwrap(),
        )
        .unwrap();
        let mut replay = config(file.clone());
        replay.override_file = Some(override_file.clone());
        assert!(matches!(
            load_replay_script(&replay).unwrap()[0],
            ReplayEntry::Hang { .. }
        ));
        std::fs::write(
            &override_file,
            json!({"patches":[
                {"at":0,"entry":{"kind":"throw","chunks":[],"message":"busy","code":"SERVER"}},
                {"at":2,"entry":{"kind":"chunks","chunks":[]}}
            ]})
            .to_string(),
        )
        .unwrap();
        let patched = load_replay_script(&replay).unwrap();
        assert_eq!(patched.len(), 3);
        assert!(matches!(patched[0], ReplayEntry::Throw { .. }));
        std::fs::write(
            &override_file,
            json!({"patches":[
                {"at":0,"entry":{"kind":"chunks","chunks":[]}},
                {"at":0,"entry":{"kind":"chunks","chunks":[]}}
            ]})
            .to_string(),
        )
        .unwrap();
        assert!(
            load_replay_script(&replay)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        for at in [json!(-1), json!(1.5), json!(9_007_199_254_740_992_u64)] {
            std::fs::write(
                &override_file,
                json!({"patches":[{"at":at,"entry":{"kind":"hang"}}]}).to_string(),
            )
            .unwrap();
            assert!(
                load_replay_script(&replay)
                    .unwrap_err()
                    .to_string()
                    .contains("non-negative safe integer")
            );
        }
    }

    #[test]
    fn orders_and_slices_child_scripts() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("session.jsonl");
        let child = root.path().join("session.1.jsonl");
        std::fs::write(&parent, jsonl("parent", 100, &[chunks("parent")])).unwrap();
        let seeded = chunks("seed");
        let own = chunks("child");
        let mut child_text = jsonl("child", 100, &[seeded.clone(), own.clone()]);
        child_text = child_text.replacen(
            r#""createdAt":100"#,
            &format!(r#""createdAt":100,"seedLength":{}"#, seeded.len()),
            1,
        );
        std::fs::write(&child, child_text).unwrap();
        let mut replay = config(parent);
        replay.child_files.push(child);
        let scripts = load_session_scripts(&replay).unwrap();
        assert_eq!(
            scripts
                .iter()
                .map(|s| s.recorded_id.as_str())
                .collect::<Vec<_>>(),
            ["parent", "child"]
        );
        assert!(matches!(&scripts[1].entries[0], ReplayEntry::Chunks { chunks } if chunks == &own));
    }

    #[test]
    fn substitutes_last_request_match_capture_and_brace_quantifier() {
        let entry = ReplayEntry::Chunks {
            chunks: vec![StreamChunk::TextDelta {
                index: 0,
                text: "id={{fromRequest:id=([0-9a-f]{4})}}".to_owned(),
            }],
        };
        let message: Message = serde_json::from_value(json!({
            "id":"fixed","role":"user","content":[{"type":"text","text":"id=aaaa then id=beef"}],
            "source":{"kind":"user"}
        }))
        .unwrap();
        let resolved = resolve_scripted_entry(&entry, &[message]).unwrap();
        assert!(
            matches!(resolved, ReplayEntry::Chunks { chunks } if chunks[0] == StreamChunk::TextDelta { index: 0, text: "id=beef".to_owned() })
        );
        assert!(
            resolve_scripted_entry(
                &ReplayEntry::Chunks {
                    chunks: vec![StreamChunk::TextDelta {
                        index: 0,
                        text: "{{fromRequest:missing}}".to_owned()
                    }]
                },
                &[]
            )
            .unwrap_err()
            .to_string()
            .contains("matched nothing")
        );
    }

    #[tokio::test]
    async fn catch_all_routes_positionally_and_audits_consumption() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("session.jsonl");
        let first = chunks("one");
        let second = chunks("two");
        std::fs::write(&file, jsonl("p", 1, &[first.clone(), second.clone()])).unwrap();
        let context = Context::new();
        let llm = LlmRuntime::install(&context).unwrap();
        let handle = install_llm_replay(&context, config(file)).unwrap();
        let request = || GenerateOptions::new(ProviderId::new("m"), ModelId::new("m"), Vec::new());
        assert_eq!(drain(llm.stream(request())).await.unwrap(), first);
        assert!(
            handle
                .assert_consumed()
                .unwrap_err()
                .to_string()
                .contains("1/2")
        );
        assert_eq!(drain(llm.stream(request())).await.unwrap(), second);
        handle.assert_consumed().unwrap();
        assert!(
            drain(llm.stream(request()))
                .await
                .unwrap_err()
                .to_string()
                .contains("exhausted")
        );
        handle.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn sessions_bind_independently_and_unrecorded_sessions_fail() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("session.jsonl");
        let child = root.path().join("session.1.jsonl");
        std::fs::write(&parent, jsonl("p", 1, &[chunks("p1"), chunks("p2")])).unwrap();
        std::fs::write(&child, jsonl("c", 2, &[chunks("c1")])).unwrap();
        let mut replay = config(parent);
        replay.child_files.push(child);
        let context = Context::new();
        let llm = LlmRuntime::install(&context).unwrap();
        let _handle = install_llm_replay(&context, replay).unwrap();
        let request = |id: &str| {
            let mut request =
                GenerateOptions::new(ProviderId::new("m"), ModelId::new("m"), Vec::new());
            request.session_id = Some(SessionId::new(id));
            request
        };
        assert!(
            matches!(drain(llm.stream(request("A"))).await.unwrap()[1], StreamChunk::TextDelta { ref text, .. } if text == "p1")
        );
        assert!(
            matches!(drain(llm.stream(request("B"))).await.unwrap()[1], StreamChunk::TextDelta { ref text, .. } if text == "c1")
        );
        assert!(
            matches!(drain(llm.stream(request("A"))).await.unwrap()[1], StreamChunk::TextDelta { ref text, .. } if text == "p2")
        );
        assert!(
            drain(llm.stream(request("C")))
                .await
                .unwrap_err()
                .to_string()
                .contains("unrecorded session")
        );
        assert!(
            drain(llm.stream(request("C")))
                .await
                .unwrap_err()
                .to_string()
                .contains("unrecorded session")
        );
    }

    #[tokio::test]
    async fn routed_catalog_resolves_defaults_and_disposes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("session.jsonl");
        std::fs::write(&file, jsonl("p", 1, &[chunks("ok")])).unwrap();
        let context = Context::new();
        let llm = LlmRuntime::install(&context).unwrap();
        let mut replay = config(file);
        replay.providers = vec![ReplayProviderConfig {
            id: "deepseek".to_owned(),
            name: Some("DeepSeek".to_owned()),
            models: vec![ReplayModelConfig {
                id: "flash".to_owned(),
                context_window: Some(128_000),
                input_modalities: Some(vec!["text".to_owned(), "image".to_owned()]),
                default_max_tokens: Some(64_000),
                reasoning_efforts: Some(vec!["off".to_owned(), "max".to_owned()]),
                default_reasoning_effort: Some("max".to_owned()),
                ..ReplayModelConfig::default()
            }],
            retry_policy: Some(
                json!({"mode":"normal","maxRetries":2,"backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":0}}),
            ),
        }];
        let handle = install_llm_replay(&context, replay).unwrap();
        assert_eq!(llm.list_providers()[0].name, "DeepSeek");
        let info = llm
            .resolve_model_info("deepseek", "flash", None)
            .await
            .unwrap();
        assert_eq!(info.context.unwrap().context_window, 128_000);
        assert_eq!(
            info.reasoning.unwrap().default_effort.unwrap().as_str(),
            "max"
        );
        assert_eq!(
            drain(llm.stream(GenerateOptions::new(
                ProviderId::new("deepseek"),
                ModelId::new("flash"),
                Vec::new(),
            )))
            .await
            .unwrap(),
            chunks("ok")
        );
        handle.dispose().await.unwrap();
        assert!(llm.list_providers().is_empty());
    }

    #[tokio::test]
    async fn throw_hang_abort_and_pacing_boundaries_are_real() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("session.jsonl");
        std::fs::write(&file, jsonl("p", 1, &[])).unwrap();
        let override_file = root.path().join("override.json");
        std::fs::write(
            &override_file,
            json!([
                {"kind":"throw","chunks":[{"type":"text-delta","index":0,"text":"prefix"}],"message":"busy","code":"SERVER"},
                {"kind":"hang","readyFile":root.path().join("ready")}
            ])
            .to_string(),
        )
        .unwrap();
        let context = Context::new();
        let llm = LlmRuntime::install(&context).unwrap();
        let mut replay = config(file);
        replay.override_file = Some(override_file);
        replay.pace_ms = 1.0;
        let _handle = install_llm_replay(&context, replay).unwrap();
        let request = || GenerateOptions::new(ProviderId::new("m"), ModelId::new("m"), Vec::new());
        let mut thrown = llm.stream(request());
        assert!(matches!(
            thrown.next().await.unwrap().unwrap(),
            StreamChunk::TextDelta { .. }
        ));
        assert!(
            thrown
                .next()
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("busy")
        );
        let signal = AbortSignal::default();
        let mut hanging = request();
        hanging.signal = Some(signal.clone());
        let mut stream = llm.stream(hanging);
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            StreamChunk::BlockStart { .. }
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            StreamChunk::TextDelta { .. }
        ));
        signal.abort();
        assert!(
            stream
                .next()
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("aborted")
        );
        assert!(root.path().join("ready").exists());
        tokio::time::timeout(Duration::from_millis(100), pace(f64::MAX, None))
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn loader_config_rejects_invalid_modalities_and_pacing() {
        let context = Context::new();
        LlmRuntime::install(&context).unwrap();
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("session.jsonl");
        std::fs::write(&file, jsonl("p", 1, &[])).unwrap();
        let bad_modalities = Config {
            file: Some(file.clone()),
            providers: Some(vec![ReplayProviderConfig {
                id: "p".to_owned(),
                models: vec![ReplayModelConfig {
                    id: "m".to_owned(),
                    input_modalities: Some(vec!["audio".to_owned()]),
                    ..ReplayModelConfig::default()
                }],
                ..ReplayProviderConfig::default()
            }]),
            ..Config::default()
        };
        let Err(error) = apply(&context, bad_modalities) else {
            panic!("invalid modalities were accepted");
        };
        assert!(error.to_string().contains("inputModalities"));
        let mut replay = config(file);
        replay.pace_ms = 1.5;
        let Err(error) = install_llm_replay(&context, replay) else {
            panic!("fractional pacing was accepted");
        };
        assert!(error.to_string().contains("paceMs"));
    }
}
