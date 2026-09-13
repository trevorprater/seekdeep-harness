# seekdeep-util timeout

English | [中文](README.zh.md)

The zero-provider timeout primitive in `seekdeep_util::timeout`. It owns timeout
arithmetic, first-cause cancellation fusion, typed timeout classification, and
an idle watchdog. It only notifies through `AbortSignal`; each capability owns
the mechanism that stops work and translates the reason into its public result.

## Timeout arithmetic

`clamp_timeout(requested, default, maximum, name)` validates only a supplied
hint as positive and finite, then returns JavaScript-compatible
`min(requested.unwrap_or(default), maximum)`. Zero is not a caller-visible
disable sentinel. Backend defaults and caps remain backend-owned and are not
silently revalidated by this helper.

`MAX_TIMER_DELAY_MS` is `2_147_483_647`, the largest delay the source runtime
schedules without clamping it to one millisecond. Timer-bearing APIs reject
non-finite, non-positive, or larger values with the source field-specific
diagnostic.

## Deadlines and classification

`deadline(upstream, timeout_ms, code)` returns a stable fused signal and a
dispose-once `Deadline`. The first of upstream cancellation and the timer wins,
and its exact typed reason remains authoritative. Dropping or explicitly
disposing the deadline cancels the timer. A non-positive timeout is the
internal no-timer sentinel: it forwards only the upstream signal or creates a
never-aborting signal.

An elapsed timer carries `TimeoutReason { code, timeout_ms }`, renders as
`<code> after <milliseconds>ms`, and is also represented in the signal's JSON
reason with `name`, `message`, `code`, and `timeoutMs`. `timeout_of` recognizes
only the typed reason, optionally with an exact code. A structurally similar
JSON object cannot spoof classification, and a nested outer deadline is not
misclassified as the inner capability's timeout.

## Idle watchdog

`IdleWatchdog::new(upstream, timeout_ms, code)` creates one stable signal. Its
timer is armed only while `next(stream)` has an outstanding provider demand;
consumer think time does not count. `pulse()` rearms that same outstanding
demand after out-of-band transport activity. Concurrent demand and demand after
disposal fail explicitly. Disposal is idempotent and clears an armed timer.

## Model and cache effect

None directly. Consumers decide whether timeout outcomes become model-visible
and own any request-prefix impact.

## Limitation

Notification is cooperative. A timer aborts the signal but does not terminate
a process, socket, provider stream, or tool body by itself.
