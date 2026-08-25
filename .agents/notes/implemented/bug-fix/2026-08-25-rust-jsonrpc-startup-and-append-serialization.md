# Agent Note: Rust JSON-RPC startup and append serialization

Status: implemented

English | [中文](2026-08-25-rust-jsonrpc-startup-and-append-serialization.zh.md)

## Problem

A stdio JSON-RPC server becomes externally reachable as soon as its transport starts. Loader configurations may still be activating later siblings at that point, including the adapter selected for replay. An SDK client that sends `initialize` immediately can therefore observe a partial plugin tree and cause the server to install its live DeepSeek fallback before the configured adapter exists.

Rust executor threads also allow two independent session append callers to overlap while one caller has released the session data lock for pre-commit publication. Treating that overlap as synchronous reentrancy rejects valid ordered work such as a session-title append racing request-header construction. True same-thread nested append must remain invalid because it would recursively enter the same acceptance transaction.

## Decision

The process launcher registers `seekdeep-sdk-jsonrpc-server` through `deferred_plugin()`. Its request handlers queue behind a one-way readiness latch until `seekdeep-jsonrpc-agent` finishes the complete app-boot and Loader settlement transaction, then the runner calls `mark_ready()`. Queued frames retain transport order. Programmatic embeddings and explicit-stream tests continue to use the immediately ready `plugin()` and `apply_with_runtime()` APIs.

The SDK DeepSeek fallback mounts the provider plugin with its empty config object, never JSON `null`, so the ordinary default-config path remains valid when no configured adapter owns `deepseek-official`.

Each `Session` owns a reentrant append gate spanning prepare, commit, and publication. A different executor thread waits at the gate and receives the next sequence after the current append finishes. A synchronous nested call on the same thread reacquires the gate, observes the existing acceptance marker, and returns `ReentrantAppend`; the guard therefore serializes task interleaving without weakening the recursion invariant.

## Verification

The SDK server suite proves that a queued `initialize` request cannot settle before readiness and completes after the latch opens. The compiled JSON-RPC example replays all four committed SDK scenarios through the real Loader, SDK subprocess, tools, child agent, and JSONL persistence. A live loopback DeepSeek matrix pins the three max-token mapping modes, exact model-facing tools, output cap, Zstandard log, stdout purity, and invalid environment diagnostics. The Rust minimal client drives the standalone persistent-tool composition and proves final stdout plus owned-process cleanup.

## Alternatives considered

**Move the server entry to the end of every configuration.** Rejected because configurations are external and order remains user-authored; readiness is a launcher invariant, not a convention every profile must remember.

**Let the server retry fallback selection after startup.** Rejected because the first `initialize` would still observe and mutate a partial composition. No protocol operation may begin before the selected application is ready.

**Convert session append into an async API.** Rejected because append is a synchronous, broadly used commit operation and the source event loop already serializes independent callers. A process-local gate preserves that semantic without spreading async cancellation and ownership through every producer.

## Consequences

Immediate SDK clients cannot race configured adapters or other late Loader entries. Independent Rust tasks commit session events in one contiguous sequence, while recursive listener mistakes still fail at the exact append boundary. Process launchers must explicitly open deferred servers after successful boot; a launcher that forgets leaves requests pending instead of exposing partial state.
