# Agent Note: Latch wake-ups that land in the cancel-convergence window

Status: implemented

English | [中文](2026-08-07-cancel-convergence-wake-latch.zh.md)

## Problem

`Agent.cancel(cause, { keepInbox: true })` returns immediately after firing the abort signal, but the active driver may not have converged to `idle` yet: LLM stream teardown, tool cancellation, and the `turn/end` append all unwind asynchronously after `abort()` returns. A waking send arriving in that window was placed into `next-turn` while `wakeDriver()` returned early on the still-`running` phase, and the exiting driver never replayed the wake — the message stayed parked until another waking send arrived. The same dropped-wake window existed around aborted `runMaintenance` activities. Several tests enshrined the parked behavior ("waits for another wakeup"); the bug broke both `session.cancel` and the `subagent.interrupt` composition path (issue #1838). The owning cancellation and send contracts are the [explicit turn cancellation](../architecture/2026-07-16-explicit-turn-cancellation.md) and [unified send](../architecture/2026-07-22-unified-send-and-coalesced-user-messages.md) decisions; the production `keepInbox` consumer is [web stop preserves queue](2026-07-31-web-stop-preserves-queue.md).

## Decision

The `running` phase carries a `wakeRequested` latch, mirroring the existing `maintenance` phase field. Every waking send that lands during a non-disposal activity sets the latch, including a live driver that may already have performed its final inbox claim. The exiting activity replays the latch at its own convergence boundary (`kick`'s `finally` and `runMaintenance`'s `finally`): this placement guarantees `turn/end N` lands before a replacement driver opens `turn/start N+1`, and that `whenIdle()` sees the replacement through its `activityDone` loop. The replay sites also require `inbox.hasPending`; when the live driver consumed the newly queued work, the latch causes no empty replacement, while a wake that arrived after the final claim starts one. A wake sent while the agent is already idle keeps its turn boundary even when its message is cleared before the driver claims — that `idle → running → idle` transition is an observable contract: the goal-round-driver driver's pause/disarm fallback fires on the `idle` transition after a cancelled reservation. The first cancellation clears the pre-abort latch even with `keepInbox`, preserving the queued-tail parking contract; a waking send after abort re-arms it. `cancel()` without `keepInbox` also clears the inbox.

The `signal.aborted` discriminator is load-bearing: it separates pre-abort queued work — whose latch the first `keepInbox` cancellation clears so it parks for a later wake — from post-abort explicit wakes, which must run after convergence. A repeated cancellation does not erase that later wake because the signal already carries its first abort reason.

## Alternatives considered

**Have `cancel()` set the phase to `idle` immediately.** Rejected: the driver is still unwinding, so this overlaps two drivers. The replay lives in the old driver's `finally`, which then never runs — 14 of 83 tests failed, several deadlocked. Repairing it requires identity-based phase ownership plus a turn-open quiescence barrier, which is strictly more machinery and is the latch in disguise.

**Treat every latched wake as a mandatory replacement driver.** Rejected because a live driver commonly consumes the new message before it exits. Requiring `inbox.hasPending` at convergence preserves the latch across the final-claim gap without opening an empty follow-up turn when the current driver already delivered the wake.

**Replay through a chained promise (`activityDone.then(...)`).** Rejected: the replay would run outside the activity's own settlement, so `whenIdle()`'s loop can resolve before the replayed driver starts; fixing that requires replacing `activityDone` at send time and depends on microtask reaction ordering — more fragile than a synchronous flag.

**Wait for quiescence in the subagent adapter.** Rejected by the issue scope: the cancel/wake state machine owns the fix, not a consumer.

## Consequences

The `running` phase gains a `wakeRequested` field; first cancellation clears its pre-abort value, `cancel()` without `keepInbox` also clears the inbox, and a `disposed` cancel never latches. A wake landing after disposal begins stays parked and `whenIdle()` does not wait on a full model turn over the session being torn down. The latch closes the gap between the live driver's final inbox claim and exit: pending work starts a replacement driver, while already-consumed work does not. Between an aborted turn and the replacement driver, status transitions emit a transient `idle → running` pair. A waking send issued while already idle whose message is cleared before any driver claims it still opens an empty completed turn, preserving the observable wake boundary.
