//! Scripted HTTP/SSE server with request capture and deterministic faults.

use std::{
    collections::BTreeMap,
    fmt,
    fmt::Write as _,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::{Notify, watch},
    task::JoinSet,
};

/// Largest millisecond delay accepted by the source timer boundary.
pub const MAX_MOCK_LLM_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_SUCCESS_TEXT: &str = "mock response recovered";
const DEFAULT_PARTIAL_TEXT: &str = "discarded partial response";
const DEFAULT_REASONING_TEXT: &str = "mock reasoning";

/// Every request-scoped behavior accepted by the mock server.
pub const MOCK_LLM_BEHAVIORS: &[&str] = &[
    "connection_reset",
    "stream_disconnect",
    "empty",
    "empty_body",
    "stream_eof",
    "partial_eof",
    "partial_disconnect",
    "stall",
    "malformed_json",
    "malformed_event",
    "wrong_content_type",
    "rate_limit",
    "server_error",
    "service_unavailable",
    "auth_error",
    "invalid_request",
    "context_overflow",
    "quota_exceeded",
    "success",
    "reasoning_success",
    "tool_call_success",
    "max_tokens",
    "slow_success",
    "random",
];

/// One scripted behavior name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockLlmBehavior {
    /// Reset before HTTP headers.
    ConnectionReset,
    /// Open SSE, then reset.
    StreamDisconnect,
    /// Terminal stop with no content.
    Empty,
    /// Empty successful HTTP body.
    EmptyBody,
    /// Role delta followed by clean HTTP EOF.
    StreamEof,
    /// Partial text followed by clean HTTP EOF.
    PartialEof,
    /// Partial text followed by a reset.
    PartialDisconnect,
    /// Open SSE until the caller or server closes.
    Stall,
    /// Invalid JSON inside one SSE event.
    MalformedJson,
    /// Structurally invalid SSE event.
    MalformedEvent,
    /// Success body advertised as JSON.
    WrongContentType,
    /// HTTP 429 rate limit.
    RateLimit,
    /// HTTP 500 server error.
    ServerError,
    /// HTTP 503 unavailable.
    ServiceUnavailable,
    /// HTTP 401 credential error.
    AuthError,
    /// HTTP 400 request error.
    InvalidRequest,
    /// HTTP 400 context overflow.
    ContextOverflow,
    /// HTTP 429 quota error.
    QuotaExceeded,
    /// Complete text stream.
    Success,
    /// Reasoning followed by complete text.
    ReasoningSuccess,
    /// Complete streamed function call.
    ToolCallSuccess,
    /// Complete text with a length finish.
    MaxTokens,
    /// Paced complete text.
    SlowSuccess,
    /// Deterministically choose a concrete behavior.
    Random,
}

impl MockLlmBehavior {
    /// Stable CLI and telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConnectionReset => "connection_reset",
            Self::StreamDisconnect => "stream_disconnect",
            Self::Empty => "empty",
            Self::EmptyBody => "empty_body",
            Self::StreamEof => "stream_eof",
            Self::PartialEof => "partial_eof",
            Self::PartialDisconnect => "partial_disconnect",
            Self::Stall => "stall",
            Self::MalformedJson => "malformed_json",
            Self::MalformedEvent => "malformed_event",
            Self::WrongContentType => "wrong_content_type",
            Self::RateLimit => "rate_limit",
            Self::ServerError => "server_error",
            Self::ServiceUnavailable => "service_unavailable",
            Self::AuthError => "auth_error",
            Self::InvalidRequest => "invalid_request",
            Self::ContextOverflow => "context_overflow",
            Self::QuotaExceeded => "quota_exceeded",
            Self::Success => "success",
            Self::ReasoningSuccess => "reasoning_success",
            Self::ToolCallSuccess => "tool_call_success",
            Self::MaxTokens => "max_tokens",
            Self::SlowSuccess => "slow_success",
            Self::Random => "random",
        }
    }
}

impl std::str::FromStr for MockLlmBehavior {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "connection_reset" => Ok(Self::ConnectionReset),
            "stream_disconnect" => Ok(Self::StreamDisconnect),
            "empty" => Ok(Self::Empty),
            "empty_body" => Ok(Self::EmptyBody),
            "stream_eof" => Ok(Self::StreamEof),
            "partial_eof" => Ok(Self::PartialEof),
            "partial_disconnect" => Ok(Self::PartialDisconnect),
            "stall" => Ok(Self::Stall),
            "malformed_json" => Ok(Self::MalformedJson),
            "malformed_event" => Ok(Self::MalformedEvent),
            "wrong_content_type" => Ok(Self::WrongContentType),
            "rate_limit" => Ok(Self::RateLimit),
            "server_error" => Ok(Self::ServerError),
            "service_unavailable" => Ok(Self::ServiceUnavailable),
            "auth_error" => Ok(Self::AuthError),
            "invalid_request" => Ok(Self::InvalidRequest),
            "context_overflow" => Ok(Self::ContextOverflow),
            "quota_exceeded" => Ok(Self::QuotaExceeded),
            "success" => Ok(Self::Success),
            "reasoning_success" => Ok(Self::ReasoningSuccess),
            "tool_call_success" => Ok(Self::ToolCallSuccess),
            "max_tokens" => Ok(Self::MaxTokens),
            "slow_success" => Ok(Self::SlowSuccess),
            "random" => Ok(Self::Random),
            _ => anyhow::bail!("unknown mock LLM behavior {value:?}"),
        }
    }
}

/// One behavior after resolving `random`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcreteMockLlmBehavior {
    /// Reset before HTTP headers.
    ConnectionReset,
    /// Open SSE, then reset.
    StreamDisconnect,
    /// Terminal stop with no content.
    Empty,
    /// Empty successful HTTP body.
    EmptyBody,
    /// Role delta followed by clean HTTP EOF.
    StreamEof,
    /// Partial text followed by clean HTTP EOF.
    PartialEof,
    /// Partial text followed by a reset.
    PartialDisconnect,
    /// Open SSE until closure.
    Stall,
    /// Invalid JSON event.
    MalformedJson,
    /// Invalid event shape.
    MalformedEvent,
    /// Success under a JSON content type.
    WrongContentType,
    /// Rate-limit response.
    RateLimit,
    /// Server-error response.
    ServerError,
    /// Unavailable response.
    ServiceUnavailable,
    /// Authentication response.
    AuthError,
    /// Invalid-request response.
    InvalidRequest,
    /// Context-overflow response.
    ContextOverflow,
    /// Quota response.
    QuotaExceeded,
    /// Complete text.
    Success,
    /// Reasoning plus text.
    ReasoningSuccess,
    /// Function call.
    ToolCallSuccess,
    /// Length finish.
    MaxTokens,
    /// Paced text.
    SlowSuccess,
}

impl ConcreteMockLlmBehavior {
    /// Stable telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        MockLlmBehavior::from_concrete(self).as_str()
    }
}

impl MockLlmBehavior {
    const fn from_concrete(value: ConcreteMockLlmBehavior) -> Self {
        match value {
            ConcreteMockLlmBehavior::ConnectionReset => Self::ConnectionReset,
            ConcreteMockLlmBehavior::StreamDisconnect => Self::StreamDisconnect,
            ConcreteMockLlmBehavior::Empty => Self::Empty,
            ConcreteMockLlmBehavior::EmptyBody => Self::EmptyBody,
            ConcreteMockLlmBehavior::StreamEof => Self::StreamEof,
            ConcreteMockLlmBehavior::PartialEof => Self::PartialEof,
            ConcreteMockLlmBehavior::PartialDisconnect => Self::PartialDisconnect,
            ConcreteMockLlmBehavior::Stall => Self::Stall,
            ConcreteMockLlmBehavior::MalformedJson => Self::MalformedJson,
            ConcreteMockLlmBehavior::MalformedEvent => Self::MalformedEvent,
            ConcreteMockLlmBehavior::WrongContentType => Self::WrongContentType,
            ConcreteMockLlmBehavior::RateLimit => Self::RateLimit,
            ConcreteMockLlmBehavior::ServerError => Self::ServerError,
            ConcreteMockLlmBehavior::ServiceUnavailable => Self::ServiceUnavailable,
            ConcreteMockLlmBehavior::AuthError => Self::AuthError,
            ConcreteMockLlmBehavior::InvalidRequest => Self::InvalidRequest,
            ConcreteMockLlmBehavior::ContextOverflow => Self::ContextOverflow,
            ConcreteMockLlmBehavior::QuotaExceeded => Self::QuotaExceeded,
            ConcreteMockLlmBehavior::Success => Self::Success,
            ConcreteMockLlmBehavior::ReasoningSuccess => Self::ReasoningSuccess,
            ConcreteMockLlmBehavior::ToolCallSuccess => Self::ToolCallSuccess,
            ConcreteMockLlmBehavior::MaxTokens => Self::MaxTokens,
            ConcreteMockLlmBehavior::SlowSuccess => Self::SlowSuccess,
        }
    }

    pub(crate) fn concrete(self) -> Option<ConcreteMockLlmBehavior> {
        match self {
            Self::ConnectionReset => Some(ConcreteMockLlmBehavior::ConnectionReset),
            Self::StreamDisconnect => Some(ConcreteMockLlmBehavior::StreamDisconnect),
            Self::Empty => Some(ConcreteMockLlmBehavior::Empty),
            Self::EmptyBody => Some(ConcreteMockLlmBehavior::EmptyBody),
            Self::StreamEof => Some(ConcreteMockLlmBehavior::StreamEof),
            Self::PartialEof => Some(ConcreteMockLlmBehavior::PartialEof),
            Self::PartialDisconnect => Some(ConcreteMockLlmBehavior::PartialDisconnect),
            Self::Stall => Some(ConcreteMockLlmBehavior::Stall),
            Self::MalformedJson => Some(ConcreteMockLlmBehavior::MalformedJson),
            Self::MalformedEvent => Some(ConcreteMockLlmBehavior::MalformedEvent),
            Self::WrongContentType => Some(ConcreteMockLlmBehavior::WrongContentType),
            Self::RateLimit => Some(ConcreteMockLlmBehavior::RateLimit),
            Self::ServerError => Some(ConcreteMockLlmBehavior::ServerError),
            Self::ServiceUnavailable => Some(ConcreteMockLlmBehavior::ServiceUnavailable),
            Self::AuthError => Some(ConcreteMockLlmBehavior::AuthError),
            Self::InvalidRequest => Some(ConcreteMockLlmBehavior::InvalidRequest),
            Self::ContextOverflow => Some(ConcreteMockLlmBehavior::ContextOverflow),
            Self::QuotaExceeded => Some(ConcreteMockLlmBehavior::QuotaExceeded),
            Self::Success => Some(ConcreteMockLlmBehavior::Success),
            Self::ReasoningSuccess => Some(ConcreteMockLlmBehavior::ReasoningSuccess),
            Self::ToolCallSuccess => Some(ConcreteMockLlmBehavior::ToolCallSuccess),
            Self::MaxTokens => Some(ConcreteMockLlmBehavior::MaxTokens),
            Self::SlowSuccess => Some(ConcreteMockLlmBehavior::SlowSuccess),
            Self::Random => None,
        }
    }
}

/// Insertion-ordered non-negative random behavior weights.
pub type MockLlmRandomWeights = IndexMap<ConcreteMockLlmBehavior, f64>;

/// Source stress-profile defaults.
pub static DEFAULT_MOCK_LLM_RANDOM_WEIGHTS: LazyLock<MockLlmRandomWeights> = LazyLock::new(|| {
    IndexMap::from([
        (ConcreteMockLlmBehavior::Success, 48.0),
        (ConcreteMockLlmBehavior::SlowSuccess, 10.0),
        (ConcreteMockLlmBehavior::MaxTokens, 2.0),
        (ConcreteMockLlmBehavior::ConnectionReset, 5.0),
        (ConcreteMockLlmBehavior::StreamDisconnect, 5.0),
        (ConcreteMockLlmBehavior::PartialDisconnect, 10.0),
        (ConcreteMockLlmBehavior::Empty, 5.0),
        (ConcreteMockLlmBehavior::Stall, 2.0),
        (ConcreteMockLlmBehavior::RateLimit, 5.0),
        (ConcreteMockLlmBehavior::ServerError, 4.0),
        (ConcreteMockLlmBehavior::ServiceUnavailable, 2.0),
        (ConcreteMockLlmBehavior::PartialEof, 1.0),
        (ConcreteMockLlmBehavior::MalformedJson, 1.0),
    ])
});

/// How one accepted request ended at the mock boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockLlmRequestOutcome {
    /// Server completed its selected response.
    Completed,
    /// Transport was reset deliberately.
    Reset,
    /// Stream remains intentionally open.
    Stalled,
    /// Client closed during a pending response.
    ClientClosed,
    /// Handler failed unexpectedly.
    ServerError,
}

/// Immutable request/start or result telemetry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MockLlmServerEvent {
    /// An accepted chat-completions request.
    Request {
        /// One-based request number.
        attempt: usize,
        /// Script entry before random resolution.
        script_behavior: String,
        /// Concrete selected behavior.
        behavior: String,
        /// Request path.
        path: String,
    },
    /// Terminal server-side request observation.
    Result {
        /// One-based request number.
        attempt: usize,
        /// Script entry before random resolution.
        script_behavior: String,
        /// Concrete selected behavior.
        behavior: String,
        /// Terminal outcome.
        outcome: MockLlmRequestOutcome,
        /// SSE event count.
        chunks_sent: usize,
    },
}

/// Captured request and its live/final outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockLlmRequestRecord {
    /// One-based accepted request number.
    pub attempt: usize,
    /// Script entry before random resolution.
    pub script_behavior: String,
    /// Concrete selected behavior.
    pub behavior: String,
    /// Original path.
    pub path: String,
    /// Detached lower-case request headers.
    pub headers: BTreeMap<String, String>,
    /// Parsed JSON body; absent for an empty request.
    pub body: Option<Value>,
    /// Number of SSE data events handed to the socket.
    pub chunks_sent: usize,
    /// Final outcome; absent while ordinary work is pending.
    pub outcome: Option<MockLlmRequestOutcome>,
}

/// Optional request telemetry observer.
pub type MockLlmEventObserver = Arc<dyn Fn(MockLlmServerEvent) + Send + Sync>;

/// Configuration for one mock server.
#[derive(Clone, Default)]
pub struct MockLlmServerOptions {
    /// Listener host; defaults to IPv4 loopback.
    pub host: Option<String>,
    /// Listener port; zero requests an OS-assigned port.
    pub port: Option<f64>,
    /// Optional exact bearer token.
    pub api_key: Option<String>,
    /// Ordered request behavior script.
    pub sequence: Vec<MockLlmBehavior>,
    /// Reuse the final script entry after exhaustion.
    pub repeat_last: bool,
    /// Optional deterministic unsigned 32-bit seed.
    pub random_seed: Option<f64>,
    /// Optional random weights.
    pub random_weights: Option<MockLlmRandomWeights>,
    /// Complete success text.
    pub success_text: Option<String>,
    /// Partial transport text.
    pub partial_text: Option<String>,
    /// Reasoning stream text.
    pub reasoning_text: Option<String>,
    /// Unicode scalar count per delta.
    pub chunk_size: Option<f64>,
    /// Slow-response delay.
    pub chunk_delay_ms: Option<f64>,
    /// Disconnect delay.
    pub disconnect_delay_ms: Option<f64>,
    /// Retry-after delay.
    pub retry_after_ms: Option<f64>,
    /// Optional provider request id.
    pub request_id: Option<String>,
    /// Tool name.
    pub tool_name: Option<String>,
    /// Raw JSON tool arguments.
    pub tool_arguments: Option<String>,
    /// Observational event sink.
    pub on_event: Option<MockLlmEventObserver>,
}

impl fmt::Debug for MockLlmServerOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockLlmServerOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("sequence", &self.sequence)
            .field("repeat_last", &self.repeat_last)
            .field("random_seed", &self.random_seed)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct ResolvedOptions {
    host: String,
    port: u16,
    api_key: Option<String>,
    sequence: Vec<MockLlmBehavior>,
    last_behavior: MockLlmBehavior,
    repeat_last: bool,
    random_seed: u32,
    random_weights: Vec<(ConcreteMockLlmBehavior, f64)>,
    success_text: String,
    partial_text: String,
    reasoning_text: String,
    chunk_size: usize,
    chunk_delay: Duration,
    disconnect_delay: Duration,
    retry_after_ms: u64,
    request_id: Option<String>,
    tool_name: String,
    tool_arguments: String,
    on_event: Option<MockLlmEventObserver>,
}

static NEXT_DEFAULT_SEED: AtomicU32 = AtomicU32::new(0x5eed_0001);

fn bounded_integer(name: &str, value: f64, min: f64, max: f64) -> anyhow::Result<u64> {
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && (min..=max).contains(&value),
        "llm-mock-server: {name} must be an integer between {min:.0} and {max:.0}"
    );
    format!("{value:.0}").parse().map_err(Into::into)
}

#[allow(clippy::too_many_lines)]
fn resolve_options(options: MockLlmServerOptions) -> anyhow::Result<ResolvedOptions> {
    let host = options.host.unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = u16::try_from(bounded_integer(
        "port",
        options.port.unwrap_or(0.0),
        0.0,
        65_535.0,
    )?)?;
    let chunk_size = usize::try_from(bounded_integer(
        "chunkSize",
        options.chunk_size.unwrap_or(8.0),
        1.0,
        MAX_SAFE_INTEGER,
    )?)?;
    let chunk_delay_ms = bounded_integer(
        "chunkDelayMs",
        options.chunk_delay_ms.unwrap_or(25.0),
        0.0,
        MAX_MOCK_LLM_TIMER_DELAY_MS,
    )?;
    let disconnect_delay_ms = bounded_integer(
        "disconnectDelayMs",
        options.disconnect_delay_ms.unwrap_or(10.0),
        0.0,
        MAX_MOCK_LLM_TIMER_DELAY_MS,
    )?;
    let retry_after_ms = bounded_integer(
        "retryAfterMs",
        options.retry_after_ms.unwrap_or(1_000.0),
        1.0,
        MAX_MOCK_LLM_TIMER_DELAY_MS,
    )?;
    let random_seed = u32::try_from(bounded_integer(
        "randomSeed",
        options
            .random_seed
            .unwrap_or_else(|| f64::from(NEXT_DEFAULT_SEED.fetch_add(1, Ordering::AcqRel))),
        0.0,
        f64::from(u32::MAX),
    )?)?;
    let success_text = options
        .success_text
        .unwrap_or_else(|| DEFAULT_SUCCESS_TEXT.to_owned());
    let partial_text = options
        .partial_text
        .unwrap_or_else(|| DEFAULT_PARTIAL_TEXT.to_owned());
    let reasoning_text = options
        .reasoning_text
        .unwrap_or_else(|| DEFAULT_REASONING_TEXT.to_owned());
    let tool_name = options.tool_name.unwrap_or_else(|| "mock_tool".to_owned());
    let tool_arguments = options
        .tool_arguments
        .unwrap_or_else(|| r#"{"value":"mock"}"#.to_owned());

    anyhow::ensure!(!host.is_empty(), "llm-mock-server: host must not be empty");
    anyhow::ensure!(
        !options.sequence.is_empty(),
        "llm-mock-server: sequence must not be empty"
    );
    anyhow::ensure!(
        options.api_key.as_deref() != Some(""),
        "llm-mock-server: apiKey must not be empty"
    );
    anyhow::ensure!(
        !success_text.is_empty(),
        "llm-mock-server: successText must not be empty"
    );
    anyhow::ensure!(
        !partial_text.is_empty(),
        "llm-mock-server: partialText must not be empty"
    );
    anyhow::ensure!(
        !reasoning_text.is_empty(),
        "llm-mock-server: reasoningText must not be empty"
    );
    anyhow::ensure!(
        !tool_name.is_empty(),
        "llm-mock-server: toolName must not be empty"
    );
    anyhow::ensure!(
        options.request_id.as_deref() != Some(""),
        "llm-mock-server: requestId must not be empty"
    );
    anyhow::ensure!(
        serde_json::from_str::<Value>(&tool_arguments).is_ok(),
        "llm-mock-server: toolArguments must be valid JSON"
    );

    let weights = options
        .random_weights
        .unwrap_or_else(|| DEFAULT_MOCK_LLM_RANDOM_WEIGHTS.clone());
    let mut random_weights = Vec::new();
    for (behavior, weight) in weights {
        anyhow::ensure!(
            weight.is_finite() && weight >= 0.0,
            "llm-mock-server: random weight for {} must be a non-negative finite number",
            behavior.as_str()
        );
        if weight > 0.0 {
            random_weights.push((behavior, weight));
        }
    }
    anyhow::ensure!(
        !random_weights.is_empty(),
        "llm-mock-server: randomWeights must contain at least one positive weight"
    );
    let last_behavior = *options.sequence.last().expect("non-empty checked above");
    Ok(ResolvedOptions {
        host,
        port,
        api_key: options.api_key,
        sequence: options.sequence,
        last_behavior,
        repeat_last: options.repeat_last,
        random_seed,
        random_weights,
        success_text,
        partial_text,
        reasoning_text,
        chunk_size,
        chunk_delay: Duration::from_millis(chunk_delay_ms),
        disconnect_delay: Duration::from_millis(disconnect_delay_ms),
        retry_after_ms,
        request_id: options.request_id,
        tool_name,
        tool_arguments,
        on_event: options.on_event,
    })
}

#[derive(Debug)]
struct BehaviorCursor {
    cursor: usize,
    random: SeededRandom,
}

#[derive(Clone, Copy, Debug)]
enum SelectedBehavior {
    Concrete(ConcreteMockLlmBehavior),
    ScriptExhausted,
}

impl SelectedBehavior {
    fn as_str(self) -> &'static str {
        match self {
            Self::Concrete(behavior) => behavior.as_str(),
            Self::ScriptExhausted => "script_exhausted",
        }
    }
}

#[derive(Debug)]
struct ServerState {
    options: ResolvedOptions,
    behavior: Mutex<BehaviorCursor>,
    requests: Mutex<Vec<MockLlmRequestRecord>>,
}

impl fmt::Debug for ResolvedOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl ServerState {
    fn emit(&self, event: MockLlmServerEvent) {
        if let Some(observer) = &self.options.on_event {
            let _ = catch_unwind(AssertUnwindSafe(|| observer(event)));
        }
    }

    fn select_behavior(&self) -> (String, SelectedBehavior) {
        let mut state = self.behavior.lock();
        let selected = self.options.sequence.get(state.cursor).copied();
        state.cursor += 1;
        let script = selected.or_else(|| {
            self.options
                .repeat_last
                .then_some(self.options.last_behavior)
        });
        let Some(script) = script else {
            return (
                "script_exhausted".to_owned(),
                SelectedBehavior::ScriptExhausted,
            );
        };
        let behavior = script.concrete().map_or_else(
            || {
                SelectedBehavior::Concrete(choose_random_behavior(
                    &self.options.random_weights,
                    &mut state.random,
                ))
            },
            SelectedBehavior::Concrete,
        );
        (script.as_str().to_owned(), behavior)
    }

    fn start_record(
        &self,
        script_behavior: String,
        behavior: SelectedBehavior,
        path: String,
        headers: BTreeMap<String, String>,
        body: Option<Value>,
    ) -> usize {
        let (index, event) = {
            let mut requests = self.requests.lock();
            let attempt = requests.len() + 1;
            let event = MockLlmServerEvent::Request {
                attempt,
                script_behavior: script_behavior.clone(),
                behavior: behavior.as_str().to_owned(),
                path: path.clone(),
            };
            requests.push(MockLlmRequestRecord {
                attempt,
                script_behavior,
                behavior: behavior.as_str().to_owned(),
                path,
                headers,
                body,
                chunks_sent: 0,
                outcome: None,
            });
            (requests.len() - 1, event)
        };
        self.emit(event);
        index
    }

    fn chunk_sent(&self, index: usize) {
        if let Some(record) = self.requests.lock().get_mut(index) {
            record.chunks_sent += 1;
        }
    }

    fn finish(&self, index: usize, outcome: MockLlmRequestOutcome) {
        let event = {
            let mut requests = self.requests.lock();
            let Some(record) = requests.get_mut(index) else {
                return;
            };
            if record.outcome.is_some() {
                return;
            }
            record.outcome = Some(outcome);
            MockLlmServerEvent::Result {
                attempt: record.attempt,
                script_behavior: record.script_behavior.clone(),
                behavior: record.behavior.clone(),
                outcome,
                chunks_sent: record.chunks_sent,
            }
        };
        self.emit(event);
    }
}

#[derive(Clone, Copy, Debug)]
struct SeededRandom(u32);

impl SeededRandom {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 15)).wrapping_mul(mixed | 1);
        mixed ^= mixed.wrapping_add((mixed ^ (mixed >> 7)).wrapping_mul(mixed | 61));
        f64::from(mixed ^ (mixed >> 14)) / 4_294_967_296.0
    }
}

fn choose_random_behavior(
    weights: &[(ConcreteMockLlmBehavior, f64)],
    random: &mut SeededRandom,
) -> ConcreteMockLlmBehavior {
    let total = weights.iter().map(|(_, weight)| weight).sum::<f64>();
    let mut draw = random.next() * total;
    for (behavior, weight) in weights {
        if draw < *weight {
            return *behavior;
        }
        draw -= weight;
    }
    weights.last().expect("positive weights checked").0
}

#[derive(Debug)]
struct ParsedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn read_request(
    stream: &mut TcpStream,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<ParsedRequest>> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = tokio::select! {
            result = stream.read(&mut chunk) => result?,
            result = shutdown.changed() => {
                let _ = result;
                return Ok(None);
            }
        };
        if count == 0 {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk[..count]);
        anyhow::ensure!(
            bytes.len() <= MAX_REQUEST_BYTES,
            "mock request exceeded byte limit"
        );
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or(target).to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let body = if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        let Some(body) = read_chunked_body(stream, shutdown, bytes[boundary..].to_vec()).await?
        else {
            return Ok(None);
        };
        body
    } else {
        let length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        anyhow::ensure!(
            length <= MAX_REQUEST_BYTES,
            "mock request exceeded byte limit"
        );
        while bytes.len() < boundary + length {
            let mut chunk = [0_u8; 4096];
            let count = tokio::select! {
                result = stream.read(&mut chunk) => result?,
                result = shutdown.changed() => {
                    let _ = result;
                    return Ok(None);
                }
            };
            anyhow::ensure!(count > 0, "request closed before its body");
            bytes.extend_from_slice(&chunk[..count]);
        }
        bytes[boundary..boundary + length].to_vec()
    };
    Ok(Some(ParsedRequest {
        method,
        path,
        headers,
        body,
    }))
}

async fn read_chunked_body(
    stream: &mut TcpStream,
    shutdown: &mut watch::Receiver<bool>,
    mut encoded: Vec<u8>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut cursor = 0;
    let mut body = Vec::new();
    loop {
        let line_end = loop {
            if let Some(relative) = encoded[cursor..]
                .windows(2)
                .position(|window| window == b"\r\n")
            {
                break cursor + relative;
            }
            if !read_request_bytes(stream, shutdown, &mut encoded).await? {
                return Ok(None);
            }
        };
        let size_text = std::str::from_utf8(&encoded[cursor..line_end])?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or_default(), 16)?;
        cursor = line_end + 2;
        if size == 0 {
            return Ok(Some(body));
        }
        anyhow::ensure!(
            body.len().saturating_add(size) <= MAX_REQUEST_BYTES,
            "mock request exceeded byte limit"
        );
        while encoded.len() < cursor + size + 2 {
            if !read_request_bytes(stream, shutdown, &mut encoded).await? {
                return Ok(None);
            }
        }
        body.extend_from_slice(&encoded[cursor..cursor + size]);
        cursor += size;
        anyhow::ensure!(
            encoded.get(cursor..cursor + 2) == Some(b"\r\n"),
            "malformed chunked request body"
        );
        cursor += 2;
    }
}

async fn read_request_bytes(
    stream: &mut TcpStream,
    shutdown: &mut watch::Receiver<bool>,
    bytes: &mut Vec<u8>,
) -> anyhow::Result<bool> {
    let mut chunk = [0_u8; 4096];
    let count = tokio::select! {
        result = stream.read(&mut chunk) => result?,
        result = shutdown.changed() => {
            let _ = result;
            return Ok(false);
        }
    };
    anyhow::ensure!(count > 0, "request closed before its chunked body");
    bytes.extend_from_slice(&chunk[..count]);
    Ok(true)
}

type Reader = ReadHalf<TcpStream>;
type Writer = WriteHalf<TcpStream>;

async fn write_response(
    writer: &mut Writer,
    status: u16,
    headers: &[(&str, String)],
    body: &[u8],
) -> anyhow::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", status_reason(status));
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    write!(
        head,
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )?;
    writer.write_all(head.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.shutdown().await?;
    Ok(())
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

async fn open_sse(writer: &mut Writer, content_type: &str) -> anyhow::Result<()> {
    let head = format!(
        concat!(
            "HTTP/1.1 200 OK\r\n",
            "content-type: {}\r\n",
            "cache-control: no-cache\r\n",
            "connection: close\r\n",
            "transfer-encoding: chunked\r\n\r\n"
        ),
        content_type
    );
    writer.write_all(head.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

async fn write_chunk(writer: &mut Writer, body: &[u8]) -> anyhow::Result<()> {
    writer
        .write_all(format!("{:x}\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(body).await?;
    writer.write_all(b"\r\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn finish_chunks(writer: &mut Writer) -> anyhow::Result<()> {
    writer.write_all(b"0\r\n\r\n").await?;
    writer.shutdown().await?;
    Ok(())
}

async fn write_sse(
    state: &ServerState,
    record: usize,
    writer: &mut Writer,
    payload: impl AsRef<str>,
) -> anyhow::Result<()> {
    let data = format!("data: {}\n\n", payload.as_ref());
    write_chunk(writer, data.as_bytes()).await?;
    state.chunk_sent(record);
    Ok(())
}

async fn write_sse_json(
    state: &ServerState,
    record: usize,
    writer: &mut Writer,
    payload: &Value,
) -> anyhow::Result<()> {
    write_sse(state, record, writer, serde_json::to_string(payload)?).await
}

async fn pause(
    duration: Duration,
    reader: &mut Reader,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    if duration.is_zero() {
        return !*shutdown.borrow();
    }
    let mut byte = [0_u8; 1];
    tokio::select! {
        () = tokio::time::sleep(duration) => true,
        result = reader.read(&mut byte) => result.is_ok_and(|count| count != 0),
        result = shutdown.changed() => {
            let _ = result;
            false
        }
    }
}

fn split_text(text: &str, size: usize) -> Vec<String> {
    let points = text.chars().collect::<Vec<_>>();
    points
        .chunks(size)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn terminal_chunk(reason: &str, output_tokens: usize) -> Value {
    json!({
        "choices":[{"index":0,"delta":{"content":""},"finish_reason":reason}],
        "usage":{"prompt_tokens":3,"completion_tokens":output_tokens}
    })
}

async fn stream_text(
    state: &ServerState,
    record: usize,
    reader: &mut Reader,
    writer: &mut Writer,
    shutdown: &mut watch::Receiver<bool>,
    text: &str,
    delay: Duration,
) -> anyhow::Result<bool> {
    for chunk in split_text(text, state.options.chunk_size) {
        write_sse_json(
            state,
            record,
            writer,
            &json!({"choices":[{"index":0,"delta":{"content":chunk},"finish_reason":null}]}),
        )
        .await?;
        if !pause(delay, reader, shutdown).await {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn complete_text(
    state: &ServerState,
    record: usize,
    reader: &mut Reader,
    writer: &mut Writer,
    shutdown: &mut watch::Receiver<bool>,
    reason: &str,
    delay: Duration,
) -> anyhow::Result<()> {
    if !stream_text(
        state,
        record,
        reader,
        writer,
        shutdown,
        &state.options.success_text,
        delay,
    )
    .await?
    {
        state.finish(record, MockLlmRequestOutcome::ClientClosed);
        return Ok(());
    }
    write_sse_json(
        state,
        record,
        writer,
        &terminal_chunk(reason, state.options.success_text.chars().count()),
    )
    .await?;
    write_sse(state, record, writer, "[DONE]").await?;
    finish_chunks(writer).await?;
    state.finish(record, MockLlmRequestOutcome::Completed);
    Ok(())
}

async fn http_error(
    state: &ServerState,
    record: usize,
    writer: &mut Writer,
    status: u16,
    message: &str,
    code: &str,
    error_type: &str,
) -> anyhow::Result<()> {
    let mut headers = vec![("content-type", "application/json".to_owned())];
    if state.requests.lock()[record].behavior == "rate_limit" {
        headers.push((
            "retry-after",
            state.options.retry_after_ms.div_ceil(1_000).to_string(),
        ));
    }
    if let Some(request_id) = &state.options.request_id {
        headers.push(("x-request-id", request_id.clone()));
    }
    let body = serde_json::to_vec(&json!({
        "error":{"message":message,"type":error_type,"code":code}
    }))?;
    write_response(writer, status, &headers, &body).await?;
    state.finish(record, MockLlmRequestOutcome::Completed);
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_behavior(
    state: &ServerState,
    record: usize,
    behavior: SelectedBehavior,
    reader: &mut Reader,
    writer: &mut Writer,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let SelectedBehavior::Concrete(behavior) = behavior else {
        return http_error(
            state,
            record,
            writer,
            500,
            "mock script exhausted",
            "MOCK_SCRIPT_EXHAUSTED",
            "mock_error",
        )
        .await;
    };
    match behavior {
        ConcreteMockLlmBehavior::ConnectionReset => {
            state.finish(record, MockLlmRequestOutcome::Reset);
            writer.shutdown().await?;
        }
        ConcreteMockLlmBehavior::StreamDisconnect => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            if pause(state.options.disconnect_delay, reader, shutdown).await {
                state.finish(record, MockLlmRequestOutcome::Reset);
            } else {
                state.finish(record, MockLlmRequestOutcome::ClientClosed);
            }
            writer.shutdown().await?;
        }
        ConcreteMockLlmBehavior::Empty => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            write_sse_json(state, record, writer, &terminal_chunk("stop", 0)).await?;
            write_sse(state, record, writer, "[DONE]").await?;
            finish_chunks(writer).await?;
            state.finish(record, MockLlmRequestOutcome::Completed);
        }
        ConcreteMockLlmBehavior::EmptyBody => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            finish_chunks(writer).await?;
            state.finish(record, MockLlmRequestOutcome::Completed);
        }
        ConcreteMockLlmBehavior::StreamEof => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            write_sse_json(
                state,
                record,
                writer,
                &json!({"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}),
            )
            .await?;
            finish_chunks(writer).await?;
            state.finish(record, MockLlmRequestOutcome::Completed);
        }
        ConcreteMockLlmBehavior::PartialEof => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            let _ = stream_text(
                state,
                record,
                reader,
                writer,
                shutdown,
                &state.options.partial_text,
                Duration::ZERO,
            )
            .await?;
            finish_chunks(writer).await?;
            state.finish(record, MockLlmRequestOutcome::Completed);
        }
        ConcreteMockLlmBehavior::PartialDisconnect => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            if !stream_text(
                state,
                record,
                reader,
                writer,
                shutdown,
                &state.options.partial_text,
                state.options.chunk_delay,
            )
            .await?
            {
                state.finish(record, MockLlmRequestOutcome::ClientClosed);
                return Ok(());
            }
            if pause(state.options.disconnect_delay, reader, shutdown).await {
                state.finish(record, MockLlmRequestOutcome::Reset);
            } else {
                state.finish(record, MockLlmRequestOutcome::ClientClosed);
            }
            writer.shutdown().await?;
        }
        ConcreteMockLlmBehavior::Stall => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            state.finish(record, MockLlmRequestOutcome::Stalled);
            let mut byte = [0_u8; 1];
            tokio::select! {
                _ = reader.read(&mut byte) => {},
                _ = shutdown.changed() => {},
            }
        }
        ConcreteMockLlmBehavior::MalformedJson => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            write_sse(state, record, writer, "{not-json").await?;
            write_sse(state, record, writer, "[DONE]").await?;
            finish_chunks(writer).await?;
            state.finish(record, MockLlmRequestOutcome::Completed);
        }
        ConcreteMockLlmBehavior::MalformedEvent => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            write_sse_json(state, record, writer, &json!({"choices":[null]})).await?;
            write_sse(state, record, writer, "[DONE]").await?;
            finish_chunks(writer).await?;
            state.finish(record, MockLlmRequestOutcome::Completed);
        }
        ConcreteMockLlmBehavior::WrongContentType => {
            open_sse(writer, "application/json").await?;
            complete_text(
                state,
                record,
                reader,
                writer,
                shutdown,
                "stop",
                Duration::ZERO,
            )
            .await?;
        }
        ConcreteMockLlmBehavior::RateLimit => {
            http_error(
                state,
                record,
                writer,
                429,
                "mock rate limit",
                "rate_limit",
                "mock_error",
            )
            .await?;
        }
        ConcreteMockLlmBehavior::ServerError => {
            http_error(
                state,
                record,
                writer,
                500,
                "mock server error",
                "server_error",
                "mock_error",
            )
            .await?;
        }
        ConcreteMockLlmBehavior::ServiceUnavailable => {
            http_error(
                state,
                record,
                writer,
                503,
                "mock service unavailable",
                "service_unavailable",
                "mock_error",
            )
            .await?;
        }
        ConcreteMockLlmBehavior::AuthError => {
            http_error(
                state,
                record,
                writer,
                401,
                "mock authentication failed",
                "invalid_api_key",
                "mock_error",
            )
            .await?;
        }
        ConcreteMockLlmBehavior::InvalidRequest => {
            http_error(
                state,
                record,
                writer,
                400,
                "mock invalid request",
                "invalid_request",
                "mock_error",
            )
            .await?;
        }
        ConcreteMockLlmBehavior::ContextOverflow => {
            http_error(
                state,
                record,
                writer,
                400,
                "mock input exceeds the model context window",
                "context_length_exceeded",
                "invalid_request_error",
            )
            .await?;
        }
        ConcreteMockLlmBehavior::QuotaExceeded => {
            http_error(
                state,
                record,
                writer,
                429,
                "mock insufficient quota",
                "insufficient_quota",
                "mock_error",
            )
            .await?;
        }
        ConcreteMockLlmBehavior::Success => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            complete_text(
                state,
                record,
                reader,
                writer,
                shutdown,
                "stop",
                Duration::ZERO,
            )
            .await?;
        }
        ConcreteMockLlmBehavior::ReasoningSuccess => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            for chunk in split_text(&state.options.reasoning_text, state.options.chunk_size) {
                write_sse_json(
                    state,
                    record,
                    writer,
                    &json!({"choices":[{"index":0,"delta":{"reasoning_content":chunk},"finish_reason":null}]}),
                )
                .await?;
            }
            complete_text(
                state,
                record,
                reader,
                writer,
                shutdown,
                "stop",
                Duration::ZERO,
            )
            .await?;
        }
        ConcreteMockLlmBehavior::ToolCallSuccess => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            let mut midpoint = (state.options.tool_arguments.len() / 2).max(1);
            while !state.options.tool_arguments.is_char_boundary(midpoint) {
                midpoint += 1;
            }
            for payload in [
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"mock-call-1","type":"function","function":{"name":state.options.tool_name,"arguments":&state.options.tool_arguments[..midpoint]}}]},"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":&state.options.tool_arguments[midpoint..]}}]},"finish_reason":null}]}),
            ] {
                write_sse_json(state, record, writer, &payload).await?;
            }
            write_sse_json(state, record, writer, &terminal_chunk("tool_calls", 2)).await?;
            write_sse(state, record, writer, "[DONE]").await?;
            finish_chunks(writer).await?;
            state.finish(record, MockLlmRequestOutcome::Completed);
        }
        ConcreteMockLlmBehavior::MaxTokens => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            complete_text(
                state,
                record,
                reader,
                writer,
                shutdown,
                "length",
                Duration::ZERO,
            )
            .await?;
        }
        ConcreteMockLlmBehavior::SlowSuccess => {
            open_sse(writer, "text/event-stream; charset=utf-8").await?;
            complete_text(
                state,
                record,
                reader,
                writer,
                shutdown,
                "stop",
                state.options.chunk_delay,
            )
            .await?;
        }
    }
    Ok(())
}

async fn handle_connection(
    state: Arc<ServerState>,
    mut stream: TcpStream,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let Some(request) = read_request(&mut stream, &mut shutdown).await? else {
        return Ok(());
    };
    let (mut reader, mut writer) = tokio::io::split(stream);
    if request.method != "POST" {
        write_response(&mut writer, 405, &[("allow", "POST".to_owned())], &[]).await?;
        return Ok(());
    }
    if !request.path.ends_with("/chat/completions") {
        write_response(&mut writer, 404, &[], &[]).await?;
        return Ok(());
    }
    if let Some(api_key) = &state.options.api_key
        && request.headers.get("authorization") != Some(&format!("Bearer {api_key}"))
    {
        let body = serde_json::to_vec(
            &json!({"error":{"message":"invalid mock bearer token","code":"invalid_api_key"}}),
        )?;
        write_response(
            &mut writer,
            401,
            &[("content-type", "application/json".to_owned())],
            &body,
        )
        .await?;
        return Ok(());
    }
    let body = if request.body.is_empty() {
        None
    } else if let Ok(body) = serde_json::from_slice(&request.body) {
        Some(body)
    } else {
        let body = serde_json::to_vec(&json!({
            "error":{"message":"request body must be valid JSON","code":"invalid_json"}
        }))?;
        write_response(
            &mut writer,
            400,
            &[("content-type", "application/json".to_owned())],
            &body,
        )
        .await?;
        return Ok(());
    };
    let (script, behavior) = state.select_behavior();
    let record = state.start_record(script, behavior, request.path, request.headers, body);
    if let Err(error) = run_behavior(
        &state,
        record,
        behavior,
        &mut reader,
        &mut writer,
        &mut shutdown,
    )
    .await
    {
        state.finish(record, MockLlmRequestOutcome::ServerError);
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Default)]
struct CloseState {
    started: AtomicBool,
    result: Mutex<Option<Result<(), String>>>,
    notify: Notify,
}

/// Running listener and captured request state.
pub struct MockLlmServer {
    /// Base URL without `/v1`.
    pub base_url: String,
    /// Bound TCP port.
    pub port: u16,
    /// Seed used for random selection.
    pub random_seed: u32,
    state: Arc<ServerState>,
    shutdown: watch::Sender<bool>,
    listener: Mutex<Option<tokio::task::JoinHandle<anyhow::Result<()>>>>,
    close: CloseState,
}

impl fmt::Debug for MockLlmServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockLlmServer")
            .field("base_url", &self.base_url)
            .field("port", &self.port)
            .field("random_seed", &self.random_seed)
            .finish_non_exhaustive()
    }
}

impl MockLlmServer {
    /// Detached captured requests in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<MockLlmRequestRecord> {
        self.state.requests.lock().clone()
    }

    /// Stop accepting requests and close every active connection. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns a listener-task failure to every joining caller.
    pub async fn close(&self) -> anyhow::Result<()> {
        if !self.close.started.swap(true, Ordering::AcqRel) {
            let _ = self.shutdown.send(true);
            let task = self.listener.lock().take();
            let result = match task {
                Some(task) => task
                    .await
                    .map_err(|error| anyhow::anyhow!(error.to_string()))
                    .and_then(|result| result),
                None => Ok(()),
            };
            *self.close.result.lock() = Some(result.map_err(|error| error.to_string()));
            self.close.notify.notify_waiters();
        }
        loop {
            let notified = self.close.notify.notified();
            if let Some(result) = self.close.result.lock().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            notified.await;
        }
    }
}

impl Drop for MockLlmServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Starts a local chat-completions server after validating every option.
///
/// # Errors
///
/// Returns invalid options or listener bind failures.
pub async fn start_mock_llm_server(options: MockLlmServerOptions) -> anyhow::Result<MockLlmServer> {
    let options = resolve_options(options)?;
    let listener = TcpListener::bind((options.host.as_str(), options.port)).await?;
    let address = listener.local_addr()?;
    let advertised_host = if options.host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{}]", options.host)
    } else {
        options.host.clone()
    };
    let state = Arc::new(ServerState {
        behavior: Mutex::new(BehaviorCursor {
            cursor: 0,
            random: SeededRandom(options.random_seed),
        }),
        requests: Mutex::new(Vec::new()),
        options: options.clone(),
    });
    let (shutdown, mut receiver) = watch::channel(false);
    let listener_state = state.clone();
    let listener_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    connections.spawn(handle_connection(
                        listener_state.clone(),
                        stream,
                        listener_shutdown.subscribe(),
                    ));
                    while connections.try_join_next().is_some() {}
                }
                changed = receiver.changed() => {
                    let _ = changed;
                    break;
                }
            }
        }
        while let Some(result) = connections.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::debug!(%error, "mock LLM connection ended with error"),
                Err(error) => tracing::debug!(%error, "mock LLM connection task failed"),
            }
        }
        Ok(())
    });
    Ok(MockLlmServer {
        base_url: format!("http://{advertised_host}:{}", address.port()),
        port: address.port(),
        random_seed: options.random_seed,
        state,
        shutdown,
        listener: Mutex::new(Some(task)),
        close: CloseState::default(),
    })
}
