# seekdeep-llm

English | [中文](README.zh.md)

Provider-neutral LLM vocabulary and runtime for SeekDeep Harness. This crate
defines the canonical language shared by the agent loop, append-only session
log, adapters, and plugins.

## Runtime service

`LlmRuntime` is installed under the Cordis `llm` service key. It owns an
adapter registry and one streaming call path that middleware can wrap.

- `register_adapter()` atomically registers one adapter instance for a
  non-empty provider-route set. The returned `AdapterRegistrationHandle`
  withdraws every route on disposal and can atomically `replace()` the set.
  A live registration may replace its routes with an empty set; an empty
  initial registration is invalid. Replacing a disposed registration fails
  with `REGISTRATION_DISPOSED`.
- `list_providers()` returns detached provider metadata in registry order.
- `register_configurable_providers()` publishes the routes an adapter can
  activate through settings, including the settings namespace/path and one of
  `api-key`, `provider-native`, or `codex-oauth`. Registration and replacement
  are all-or-nothing. `list_configurable_providers()` returns detached entries.
- `register_model_discovery()` offers endpoint interrogation for one settings
  namespace. `discover_models()` accepts a draft route or base URL and returns
  endpoint-order candidates after dropping empty and duplicate IDs. The
  runtime does not read, save, or adopt the draft credential or result.
- `provider_retry_policy()` returns the immutable provider policy captured at
  route registration.
- `list_models()` returns a validated detached advisory catalog. Catalog
  membership is never a routing whitelist.
- `resolve_model_info()` asks the owning adapter about one exact model,
  independently of the advisory catalog. It validates identity, context,
  default output cap, and reasoning metadata and forwards cancellation.
- `resolve_call_config()` validates an explicit reasoning effort and
  materializes adapter-owned defaults without clamping.
- `prepare_call()` performs one exact-model lookup, captures the resulting
  config, context, default markers, retry policy, and adapter registration,
  and returns a one-shot `PreparedLlmCall`. Config drift or reuse fails with
  `INVALID_PREPARED_CALL`.
- `stream()` dispatches one raw chunk stream through registered middleware.
  Use `BlockAssembler` to assemble blocks and the terminal outcome.

Provider selection, exact-model resolution, synchronous adapter dispatch,
iterator construction, and iterator failures are normalized into one terminal
`finish` chunk with an `error` or `aborted` reason. Middleware, downstream
consumer, and iterator-cleanup failures remain ordinary Rust errors because
they are not model-request outcomes. Native consumers that stop early call
`LlmStream::close().await`; compatibility bindings map this to async-iterator
`return()` and must await it.

Every adapter or configurable-directory topology commit emits the payload-free
`llm/adapters-updated` event after the new registry is readable. Observer
failures are contained so one listener cannot starve another. An
`INVARIANT`-coded listener failure is rethrown only after fan-out finishes.

The `llm/stream` middleware chain is a waterfall. A middleware receives owned
`GenerateOptions` and a one-use continuation, so it may observe, route, wrap,
or short-circuit a call. Retry after emitting a chunk has no durable attempt
boundary; shipped retry execution therefore belongs to the separately logged
request-error lifecycle, not this single-attempt runtime.

## Exact-model defaults

Exact-model metadata is a correctness query, not a catalog decoration.
Missing context means capacity is unknown; missing `default_max_tokens` leaves
the provider default intact; missing reasoning metadata means selectable
reasoning is unavailable. Invalid facts use `INVALID_MODEL_INFO`,
`INVALID_MODEL_CONTEXT`, `INVALID_MODEL_MAX_TOKENS`, or
`INVALID_MODEL_REASONING`.

`default_max_tokens` is a per-request adapter default, not a hard model limit.
An explicit request cap wins. Reasoning IDs are opaque adapter-owned newtypes:
only an exact advertised value is accepted, an advertised default is
materialized, and no aliasing or clamping occurs. A prepared call retains the
same registration from resolution through dispatch, preventing hot reload
from combining one adapter's capabilities with another adapter's request.

## Messages and chunks

`Message` is an immutable owned snapshot with a `MessageId`, role, content,
source, and preserved extension fields. Constructors detach caller input;
`Message::from_existing()` imports an existing identity without minting a new
one. Assistant, user, and tool-result constructors fix their role/source
invariants, and tool-result construction couples the source and block to one
`CallId`.

Core `ContentBlock` variants are `text`, `reasoning`, `image`, `tool-call`, and
`tool-result`. Unknown merge-extensible variants preserve their tag and fields
across serialization. Model sources may carry adapter-private replay state.
Before dispatch, replay state survives only when the historical provider and
target provider are currently owned by the exact same adapter instance.

The raw stream vocabulary is `block-start`, `text-delta`,
`reasoning-delta`, `tool-call-delta`, `block-end`, `usage`, and `finish`.
`BlockAssembler` preserves first-seen block order, treats the first close as
authoritative, ignores stragglers after close, keeps the latest usage and
finish, and drops incomplete tool calls after a max-token finish.

`LlmCallConfig` contains provider, model, reasoning effort, temperature,
maximum tokens, and stop strings. `call_config_equals()` compares every field
and stop position. Rust ownership provides the immutable request boundary;
`mark_agent_loop_request()` marks only the exact request value so observers can
distinguish reconstructable loop calls from auxiliary calls.

## Failures, credentials, and attribution

`HarnessError` is the shared coded-error base and `LlmError` adds validated,
serializable provider facts. `normalize_llm_failure()` and
`normalize_adapter_rejection()` preserve valid facts without invoking hostile
foreign accessors. `ErrorChainGraph` lets compatibility bindings render causes,
aggregates, shared nodes, cycles, and unrenderable values safely; native Rust
errors use `error_chain()`.

`normalize_api_key()` trims ECMAScript-style surrounding whitespace and accepts
only non-empty printable ASCII without spaces. `assert_usable_api_key()` names
the setting to repair and never includes rejected secret material.

Every product adapter adds `attribution_headers()`. The default identity is
`SeekDeep Harness/<crate version> (+https://github.com/deepseek-ai/seekdeep-harness)`;
white-label callers may replace the identity but must not suppress attribution.

## Model experience

The service adds no model-visible text or tool schema. It only validates and
materializes adapter configuration that the agent loop records in the session
log. The registry preserves request prefixes; the selected provider owns KV
cache behavior.

## Known limitations

- This crate performs one adapter attempt. Retry execution, caching, and rate
  limiting live in their separately logged lifecycle components.
- Sampling fields are limited to temperature, maximum tokens, and stop strings.
- Extension blocks can be retained after an authoritative `block-end`; an
  unknown incomplete block cannot be assembled.
- Exact lone-surrogate summary truncation is exposed through
  `bound_context_summary_units()` because Rust `String` cannot contain an
  unpaired UTF-16 surrogate.
