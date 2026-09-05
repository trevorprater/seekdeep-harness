# @seekdeep-ai/seekdeep-session-telemetry-otel

English | [中文](README.zh.md)

The native Rust OpenTelemetry backend for [the telemetry seam](../session-telemetry/) is the only entry a deployment loads. Its `mode` decides whether the seam follows session events live, replays the canonical log only at recorded feedback, or keeps telemetry local. Uploading modes construct an `SdkLoggerProvider` with an asynchronous batch processor and OTLP/HTTP JSON exporter, then map each handed-over record onto the Rust OTel logs API under two instrumentation scopes: ledger records on `@seekdeep-ai/seekdeep-session-telemetry-otel`, operational records on `@seekdeep-ai/seekdeep-session-telemetry-otel/ops`. Resource identity contains `service.name`/`service.version` from `seekdeep-llm`'s `APP_IDENTITY` plus this package's anonymous `user.id` (`$SEEKDEEP_HOME/.anonymous-user-id`, a random UUID created on first use and reset by deleting the file), carried once per export batch rather than per record.

## Config

```yaml
- id: session-telemetry-otel
  name: '@seekdeep-ai/seekdeep-session-telemetry-otel'
  config:
    mode: FULL                # explicit opt-in; default: DISABLED
    shutdownTimeoutMillis: 3000 # optional; defaults to 3000
    exporter:                # passed intact to the native pipeline factory
      url: https://collector.example.com/v1/logs
      headers:
        authorization: !!js `Bearer ${process.env.OTLP_TOKEN}`
    processor: {}            # optional native batch-processor options
```

| `mode` | Behavior |
|---|---|
| `FULL` | Each projected record, including lifecycle ops records, is handed to the OTel pipeline immediately. |
| `FEEDBACK_ONLY` | Each `feedback/record` replays, projects, and redacts the canonical session-log suffix through that event. Later records wait for another feedback event and remain local if none arrives. |
| `DISABLED` | Default. No coordinator, provider, processor, or exporter is constructed. No telemetry record leaves the process. A `feedback/record` logs `session telemetry is DISABLED; nothing will be shared and this feedback remains local`; the event remains in the local session log. |

Programmatic Rust installation uses `OtelTelemetryConfig`; its serialized `mode` uses the values shown above, while `SessionTelemetryMode` provides the closed internal policy vocabulary. Unknown values fail before any exporter field is read.

Upload authorization is positive and fail-closed. Only `FULL` accepts direct `sessionTelemetry.emit()` calls. `FEEDBACK_ONLY` gives its on-demand coordinator a private backend capability and treats only a `feedback/record` already committed to the canonical log as consent; an independently emitted bus value is ignored. `DISABLED` never constructs the OTel pipeline, even when exporter options are present.

The mounted service discloses the resolved mode through the seam's [`SessionTelemetrySharingStatus`](../session-telemetry/README.md#the-sharing-disclosure) `sharing` property (`full` / `feedback-only` / `disabled`), so the `/feedback` acknowledgement can report whether and how the session is shared. The disclosure is set in the constructor and is independent of capture: even `DISABLED` discloses `disabled`.

`exporter.url` is required in `FULL` and `FEEDBACK_ONLY`, has no default, and must parse as `http(s)`; it is optional and unused in `DISABLED`. In uploading modes, `shutdownTimeoutMillis` is a positive finite SeekDeep-owned outer deadline that defaults to 3000 ms, and a non-positive-integer `processor.maxExportBatchSize` fails at plugin load because it cannot drain safely. The object-safe `OtelLogPipelineFactory` receives the complete `exporter` and `processor` values. Its default native implementation maps OTLP URL, headers, request timeout, gzip, user agent, queue size, batch size, scheduled delay, and export timeout onto the Rust OTel pipeline; a platform integration may replace the factory without moving capture, consent, or redaction policy. The backend implements no `flush()`: the batch processor owns ordinary flushing. `shutdown()` drains and quiesces the provider, but the outer deadline lets Cordis disposal continue if the SDK remains pending; that deadline cannot cancel an in-flight transport, so records still pending then may be lost at process exit.

## What leaves the machine

In uploading modes, records carry the complete `event.data` as the seam's `sessionTelemetry/record` waterfall returns it — user and assistant message content, tool arguments and results (command output, file contents), the full system prompt and tool schemas (`request/header`), todo text, compaction summaries, hook `stderrSummary`, feedback text, and the session `cwd` (a local path). The seam ships no redaction rules: with no `sessionTelemetry/record` listener mounted, that is the raw captured copy, so a deployment exporting beyond a trusted boundary mounts its own rules (see [the seam README](../session-telemetry/README.md#the-redact-waterfall)). `FULL` runs redaction at append time; `FEEDBACK_ONLY` retains no telemetry copy and runs the currently mounted rules when feedback triggers canonical-log replay. Provider credentials never appear regardless: adapter API keys are constructor parameters, not session events, so they are structurally absent from the log and therefore from telemetry. `DISABLED` does not construct the SDK pipeline or hand any capture to a backend.

## Field mapping

Seam record → SDK log record: `time` → `timestamp`/`observedTimestamp`; `severity` → `severityNumber`/`severityText` (INFO 9 / WARN 13 / ERROR 17); `body` → the structured log body; `attributes` verbatim. Receivers dedupe on `(session.id, event.seq)` and alert on severity. In `FULL`, they may also detect crashes by `shutdown`-record absence: the marker is emitted at the session's own disposal or application teardown, and a marker followed by more events is a telemetry reload. In `FEEDBACK_ONLY`, a released prefix normally has no later `shutdown` marker, so its absence is not a crash signal. Streams are not self-contained across lineage: a resumed session continues its own id's stream from where the previous process left off, and a forked session's stream starts at its inherited boundary — its prefix lives in the parent's stream, stitched via `session.parent_id` + `session.seed_length`. A resumed local log may contain synthetic closers that were never exported; the wire stream stays faithful to records actually handed to the SDK.

## Model Experience

None, as the backend only forwards the seam's redacted records into the OTel pipeline; it never contributes to a model request.

#### KV Cache effect

None; this package neither assembles nor sends a provider request.

## Known Limitations and Deferred Work

- **Upstream logs API evolution** — OpenTelemetry logs APIs continue to evolve; SDK API churn lands in the native factory and does not move the seam contract.
- **Live-collector behavior belongs to the exporter** — authentication, TLS, throttling, and other real OTLP deployment behavior follow the Rust exporter rather than a package-owned transport compatibility layer.
- **Feedback-time snapshot** — `FEEDBACK_ONLY` retains no telemetry-owned copy before feedback. It reads and redacts the current canonical log when feedback is recorded; a crash before feedback uploads nothing, and policy changes before feedback affect what that replay exports.
