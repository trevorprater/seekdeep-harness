# seekdeep-token-meter

English | [中文](README.zh.md)

Replay-aware token measurement for SeekDeep Harness. The singleton
`TokenMeter` service advances one isolated fold per session from the durable
append-only log, so compaction and other pressure-sensitive plugins share one
accounting contract without depending on a compaction engine.

## Configuration

The estimator accepts only `{}` and intentionally uses one fixed heuristic:
four UTF-16 code units per token plus structural overhead for roles, content
blocks, and request-envelope fields. Unknown keys fail during configuration.
Model capacity belongs to the adapter that owns an exact provider/model route.

## Measurement contract

The service exposes two operations:

- `measure(session, request_header)` returns request pressure and the current
  priced surface at one consumed-log revision.
- `estimate_message(message)` prices one message with the fixed heuristic.

`measure()` synchronizes once and returns a detached snapshot. `total_tokens`
is request-and-response pressure, while `surface_tokens` is the surface-only
heuristic total and equals the sum of `nodes[].tokens`. A request-header
override affects pressure fields only; surface fields still describe the
current session. Each call clones the positional nodes, so measurement is
O(surface).

The fold tracks canonical request-header snapshots, step boundaries, surface
appends and replacements, successful assistant messages, provider usage, and
the chunk sequence numbers cited by each assistant message. Provider usage is
reused only when the latest successful call's canonical request envelope
matches the measured envelope and its total is no lower than that call's full
heuristic anchor. A later success replaces the earlier anchor. Otherwise the
complete current envelope and surface are estimated. Surface changes remain
signed relative to a matching anchor, including negative deltas after a
shrinking replacement.

Usage accounting sums disjoint input, cache-read, cache-write, and output
buckets; reasoning is not added again. Every successful call records an
assistant anchor, including content-less calls. An explicit empty
`sourceEventSeqs` list means a known empty provider stream. An absent legacy
list conservatively treats the durable assistant output as provider output.
Malformed unread history fails transactionally: the cached cursor is not
advanced past the invalid event.

## Session projections

When the composition provides `SESSION_PROJECTIONS`, token-meter dynamically
registers three units under its owned lifecycle. If that optional service is
withdrawn or replaced, the old registrations are removed before bindings are
reconciled. Disposing token-meter removes the service, listener, and all three
projection keys.

`tokenUsage` carries the complete durable log's `uncachedInputTokens`,
`outputTokens`, `cacheReadTokens`, and `cacheWriteTokens`. Usage chunks are
counted even when a request later fails. A final assistant-message usage for
the same `(turn, step)` replaces that sample rather than double-counting it.
Reasoning remains an output subdivision. Its single last-sample slot relies on
the legal session-log ordering rule that an earlier step cannot report usage
after a later step has done so.

`contextPressure` carries optional `pressureTokens`, the newest
provider-reported prompt size (uncached input plus cache reads and writes),
optional `projectedTokens`, and optional `contextWindow` from the newest
`request/context`. Provider output is excluded. Both pressure figures remain
absent until a provider reports usage, and capacity remains absent when the
route advertises none.

`projectedTokens` estimates the next request's prompt: it carries the provider
sample forward over the heuristic price of everything the surface gained or
lost since that sample, clamps at zero, and uses the same positional surface
fold as `measure()`. It therefore reacts as soon as content lands or compaction
shadows a span, even though compaction's direct model call adds no usage sample.
Occupancy displays should read this projected value.

`contextBreakdown` carries heuristic `systemTokens`, `toolsTokens`, and
`messageTokens`. Envelope values are last-wins on `request/header`; message
tokens replay the same positional fold as `measure()`, so they equal
`measure().surface_tokens` at every event boundary. These are approximate
composition rows, not a total: they need not sum to provider-anchored
`projectedTokens`, especially for CJK text or JSON schemas.

All three units use standard baselines, live frames, higher-sequence-wins
storage, and JSON checkpoints. The surface state is bounded by compaction and
prune checkpoints, preserving O(1) replay state after a shadowing operation. A
composition without the projection service retains normal measurement behavior.

### Context occupancy is deliberately approximate

The occupancy fields are independent last-wins records, not one atomic
observation. Switching models can pair fresh capacity with the previous route's
sample until the next request reports usage. `pressureTokens` describes the
last request; `projectedTokens` carries that older anchor over current surface
movement.

This percentage is a user-facing reference, not billing data or a gating input.
Nothing in the harness makes decisions from it; compaction calls `measure()` at
its own request boundary. Consumers requiring an exact same-boundary value
should do the same.

## Composition

```yaml
- name: seekdeep-token-meter
- name: seekdeep-compaction-basic
```

Both plugins have usable defaults. The meter remains independent of model
routing and optional compaction. Deployments configure capacity on the LLM
adapter and compaction policy on `seekdeep-compaction-basic`.

## Model experience

The service itself adds no prompt, message, schema, tool, or model call. It
affects the model only through consumers such as compaction. It directly
invalidates no KV cache; each consumer owns any request-prefix changes.

## Known limitations

- The fixed heuristic is approximate; it is not a provider tokenizer or exact
  request serializer.
- Every measurement clones the current surface to provide a coherent detached
  snapshot, making reads O(surface).
- Provider usage is reusable only for an identical canonical envelope. Prompt,
  prefix, tools, provider, model, or call-config changes fall back to estimation.
- Legacy assistant messages without source sequence numbers cannot distinguish
  provider output from listener rewrites and are handled conservatively.
