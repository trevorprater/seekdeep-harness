# seekdeep-llm-deepseek

English | [中文](README.zh.md)

The direct DeepSeek chat-completions adapter for SeekDeep Harness. It uses
`reqwest` and a strict SSE parser to translate DeepSeek's official streaming
wire format into `seekdeep_llm::StreamChunk` values.

The crate owns the `deepseek-official` provider route. That route is deliberately
different from the library-backed adapter's `deepseek` catalog name, so both can
be installed in one composition. Registering a second adapter for
`deepseek-official` fails with `DUPLICATE_ADAPTER`.

The crate root exposes the Cordis plugin, `DeepSeekAdapter`, and public
configuration/wire types. Serialization, SSE, translation, and HTTP-classifier
helpers remain under their explicit source modules rather than being flattened
onto the root API. All runtime behavior is implemented in Rust.

## Configuration

The serialized configuration keeps the source contract's camel-case field names:

```yaml
apiKeyEnv: DEEPSEEK_API_KEY
baseURL: https://api.deepseek.com
thinking: enabled
reasoningEffort: high
maxTokens: 256000
streamIdleTimeoutMs: 300000
retryPolicy:
  mode: always
  backoff:
    initialDelayMs: 500
    maxDelayMs: 10000
    jitterRatio: 0.1
defaultContextWindow: 1000000
models:
  - id: deepseek-v4-flash
    name: DeepSeek-V4-Flash
  - id: private-reasoner
    description: Company-hosted reasoning model
    contextWindow: 512000
```

`DeepSeekConfig::default()` resolves the credential reference
`DEEPSEEK_API_KEY`, then `DEEPSEEK_BASE_URL`, then the public endpoint
`https://api.deepseek.com`. The default catalog advertises
`deepseek-v4-flash` and `deepseek-v4-pro`, each with a 1,000,000-token context
window. An explicit `models` list replaces the defaults; `models: []` advertises
none. The catalog is advisory: an unlisted model id still passes through to the
wire request unchanged.

Exact-model resolution uses a configured model's `contextWindow` and
`maxTokens`, falling back to `defaultContextWindow` (1,000,000) and the
route-wide `maxTokens` (256,000). The runtime materializes the output default
before `request/header` is logged. Explicit request values win, and the adapter
does not clamp output tokens against the context window.

When thinking is enabled, exact-model metadata publishes `off`, `high`, and
`max`; the configured default is `high` when omitted. `high` and `max` serialize
as `reasoning_effort`. `off` serializes as `thinking: { type: "disabled" }` and
omits `reasoning_effort`. `thinking: disabled` is a deployment lock: only `off`
is valid, and attempts to enable reasoning fail before network I/O. Session-title
requests also force thinking off.

All numeric bounds and cross-field invariants are checked before registration.
`streamIdleTimeoutMs` must be positive, finite, and no greater than
2,147,483,647. Catalog ids must be nonempty and unique.

## Dynamic settings and credentials

`install()` registers a live `llm-deepseek` settings section. Connection facts
are resolved once per operation, so a new base URL, catalog, request default,
idle timeout, or credential reference affects the next operation while an
in-flight stream retains its starting generation. A live settings snapshot that
violates a resolver invariant leaves the last good generation serving. Changing
the retry policy replaces the registered route atomically with the same adapter
instance.

The configuration stores a credential reference, never a literal API key. Each
stream resolves that reference through the credentials service, or through the
captured launch environment when no credentials service is installed. Missing
keys fail with `MISSING_CREDENTIAL`; values that cannot safely enter an HTTP
header fail with `INVALID_CREDENTIAL`. Neither diagnostic includes key material.
The provider and advisory catalog remain available without a key so onboarding
can store a credential without restarting.

## Transport and lifecycle

Each `stream()` call makes exactly one HTTP request. One stable cancellation
signal owns the request and every body read. Caller cancellation produces
`ABORTED`; an idle read timeout produces `TIMEOUT`; connection and midstream
transport failures produce `TRANSPORT` and retain their causes. SSE comments
count as transport activity and reset an outstanding idle read but never become
model chunks or log events.

The adapter registers its retry policy as provider metadata. Durable retry
execution belongs to the agent loop, so the adapter itself never creates a
second provider request. Plugin disposal unregisters both the adapter route and
its configurable-provider directory entry.

## Request identity and attribution

Every request includes the shared SeekDeep `User-Agent` attribution and a stable
anonymous identity in `x-seekdeep-harness-user-id`. A request with a session id
also includes `x-seekdeep-harness-session-id`. Compaction calls add
`x-seekdeep-harness-compact: 1`. These headers are sent to the resolved endpoint,
including configured gateways, and are not model-visible request content.

## Wire behavior

- Requests are streaming and always set `stream_options.include_usage`.
- Usage is held until `[DONE]`, then emitted before the terminal finish chunk.
  Nothing is emitted after finish.
- An initial empty `reasoning_content` does not create a spurious block.
- Reasoning is passed back only on assistant turns that contain tool calls, as
  required by DeepSeek thinking mode; other prior reasoning is omitted.
- Cache reads map from `prompt_cache_hit_tokens` or
  `prompt_tokens_details.cached_tokens`. DeepSeek has no cache-write metric here.
- Unknown finish reasons become error finishes. A successful stop with no opened
  content blocks becomes `EMPTY_RESPONSE`.
- User and tool-result content is flattened to text. Unknown plugin block types
  are skipped, and empty tool output becomes `(no output)`.

## Errors

Non-success responses map to stable codes: `AUTH` for 401/403, `QUOTA` for
recognized exhausted balance or credits, `RATE_LIMIT` for other 429 responses,
`CONTEXT_WINDOW_EXCEEDED` for recognized context-overflow 400 responses,
`INVALID_REQUEST` for other 400 responses, `SERVER` for 5xx, and `HTTP_<status>`
otherwise. Serializable failure data retains the status, a valid positive
`Retry-After`, and `x-request-id` or `x-deepseek-request-id` when present.
Malformed JSON events fail with `MALFORMED_RESPONSE`; a stream without `[DONE]`
fails with `STREAM_CLOSED`.

## Verification boundary

Mock HTTP integration tests cover request serialization, exact headers, dynamic
configuration and credential rotation, HTTP error mapping, SSE framing,
cancellation, timeouts, and teardown without contacting DeepSeek. The
credential-gated source live-API suite is a separate external verification step;
it is not implied by the offline test suite and must not run unintentionally.

Known source limitations remain: `models` arrays replace wholesale, `tool_choice`
is outside the core vocabulary, the adapter does not share a proxy/interception
service, and content serialization supports the core text/tool vocabulary only.
