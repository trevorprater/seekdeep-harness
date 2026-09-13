# seekdeep-llm-retry

English | [中文](README.zh.md)

Provider-routed model-request recovery for SeekDeep Harness. The Rust plugin
listens at the durable agent loop's `agent/request-error` waterfall; it does not
wrap `LlmRuntime::stream()`. One adapter call is always one provider attempt,
and direct raw-stream consumers remain single-attempt.

Provider registrations own their resolved `retryPolicy`. The exact policy that
served a prepared call travels with its failure, so route replacement or
disposal cannot retroactively change an in-flight decision. A failure before a
final adapter registration is selected carries no policy and delegates.

## Policies

Omitted provider configuration resolves to normal mode: two retries for
`EMPTY_RESPONSE`, `RATE_LIMIT`, `SERVER`, `TIMEOUT`, and `TRANSPORT`, with
exponential backoff from 500 ms to 10 seconds and 10 percent symmetric jitter.
Normal mode can replace the retry count, eligible codes, and backoff:

```yaml
- id: llm-deepseek
  name: seekdeep-llm-deepseek
  config:
    retryPolicy:
      mode: normal
      maxRetries: 3
      retryableCodes: [RATE_LIMIT, SERVER, TIMEOUT, TRANSPORT]
      backoff:
        initialDelayMs: 1000
        maxDelayMs: 30000
        jitterRatio: 0.2

- id: llm-retry
  name: seekdeep-llm-retry
```

Always mode asks downstream recovery middleware first. If downstream chooses a
retry, that decision wins. Otherwise it retries every model-request failure
without an attempt limit; even a synchronous or asynchronous downstream error
cannot disable the fallback. Success, turn cancellation, or plugin disposal
ends the chain.

The executor itself accepts only `{}`. A nested `retryPolicy` on this plugin
fails with an actionable diagnostic because policy belongs to each provider;
every other unknown key is also rejected.

## Delay selection

Local delay is bounded exponential backoff with a symmetric jitter multiplier.
Full jitter can produce exactly zero milliseconds at its lower boundary. A
positive finite provider `providerRetryAfterMs` at or below `maxDelayMs` replaces
local delay verbatim. An over-cap provider delay makes normal mode delegate;
always mode uses local backoff instead, preserving its unbounded contract.

## Durable events

Before waiting, the plugin appends a non-surface `llm/retry` event containing:

- a stable `retryId` shared by one provider-policy chain;
- the current turn and still-open step;
- the exact provider route and mode;
- a canonical policy key containing every behavior-affecting field;
- the one-based retry number and, for normal mode, `maxRetries`;
- the chosen `delayMs` and complete provider-neutral failure.

Normal-mode codes are sorted in the policy key because membership is
order-independent. Numbering continues only for the same turn, step, provider,
and complete policy key. A changed policy or provider begins a new chain.

After the cancellable wait completes, `llm/retry-started` records the same
identity, turn, step, and retry number immediately before the waterfall returns
`RequestErrorAction::Retry`. Cancellation writes no started event. The loop
rebuilds the request from durable surface history and repeats it inside the same
open step; failed chunks remain diagnostic events but never become an assistant
message, tool effect, or later model context.

The public `types` module contains serde-compatible payloads for browser or
foreign-language bindings without loading timing policy. `RetryId`, provider
identity, and the canonical policy identity are transparent Rust newtypes while
remaining ordinary strings on disk and over JSON.

## Lifecycle

One plugin lifetime owns its listener, all backoffs, and delegated recovery.
Disposal unregisters the listener, aborts active waits, and drains every already
captured operation before completing. A callback captured by a waterfall before
disposal observes the closed lifetime and fails closed. Turn cancellation also
wins before a scheduled retry can mutate later state.

## Invariants

The separately registered invariant companion validates existing histories and
every candidate append. It requires retry events to be inside the named open
turn and step, match the effective request-header provider, carry a complete
valid failure and timer delay, obey mode-specific bounds and exact numbering,
and preserve one unique identity per provider-policy chain.
`llm/retry-started` must correlate to exactly one scheduled attempt and may not
repeat.

## Model experience

Retry events, provider errors, delays, and failed partial output are not
model-visible. Each retry can repeat input-token billing, but an unchanged
reconstructed prefix remains eligible for provider cache reuse. Normal mode has
a finite request budget; always mode can consume requests until success or
cancellation, so deployments own its cost and latency policy.

Offline integration tests exercise the shipping loop, real DeepSeek HTTP/SSE
adapter, refused connections, partial disconnects, empty completions, clean EOF,
idle timeout, budget exhaustion, JSONL/SQLite persistence, loader composition,
append-time invariants, cancellation, and disposal.
