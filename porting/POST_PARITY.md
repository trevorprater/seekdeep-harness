# Post-parity work register

Status: deferred-work reference, 2026-08-21.

This file names work that is explicitly out of scope until `cargo xtask parity` passes and the full verification commands run green. Nothing here is authorization to build during the port. Each item lists the parity-era provisions that keep it possible; those provisions live in [`DYNAMIC_PLUGIN_RELOAD.md`](DYNAMIC_PLUGIN_RELOAD.md) and the root [`AGENTS.md`](../AGENTS.md) and are binding now, while everything else in this file is not.

Every item below is an additive mode under the additive-modes principle in the reload ADR's preamble: explicitly enabled, byte-identical parity behavior when disabled, and built only on instruction.

## Determinism spike (first, and the gate for the rest)

Record one real session's boundary transcript (every host-import call's arguments and results, plus the ordering of async completions under a session sequencer) and replay it bit-exact through the dynamic evaluator. Module bytes and engine flags pinned; simulation bounds guest work by fuel, production keeps the source-visible timeout semantics.

This spike is the falsifier for every other item in this file. If bit-exact replay fails, the transcript, simulation, and replay items below reprice before any of them is built.

Parity-era provisions: the determinism perimeter and no-ambient-randomness conventions in the root `AGENTS.md`; the deterministic-mode requirement on ADR open decision 2; the seeded-scheduler requirement on ADR open decision 1 and on overlapped disposal.

## Effect transcripts

An opt-in recorder that serializes every plugin registration, teardown, and cross-generation completion as ordered, diffable data. Uses the substrate half of the two-layer teardown-observability contract in the ADR's Cordis portability section: `EffectHandle` and fiber disposal already aggregate labeled failure outcomes; the recorder consumes that, never the parity surfaces.

Applications, in build order: differential port verification (record source and target under identical inputs, diff the transcripts), regression gating on upgrades, and trust artifacts for shared compositions.

Parity-era provisions: the teardown-observability contract; the ordered-data requirement on ADR open decision 5.

## Deterministic simulation

Seed-addressed whole-system simulation over the deterministic perimeter: virtual clock, seeded scheduler, boundary crates replayed from recorded transcripts. Enumerates crash points in the persistence path (the two-phase append, write-behind retirement, and crash-repair machinery in `crates/session-persistence-jsonl`) the way FoundationDB-style simulation does, in CI.

Parity-era provisions: the determinism perimeter; injectable scheduling in the portable Cordis core (ADR decision 1); injected clocks in blue/green health evaluation (ADR decision 7).

## Turn-per-invocation deployment mode

One agent turn per process invocation under a lease, so dormant agents cost nothing and any machine can serve the next turn. Design requirements established during the port evaluation:

- The lease ledger is a sidecar store, not the session log. `revision_identity` in `crates/session-persistence-jsonl/src/backend.rs` is single-machine filesystem metadata (`dev:ino:len:mtime:ctime`) and remains a local-integrity check only; the JSONL persistence format does not change.
- Lease semantics to specify in the future design: separate claimed-versus-executing TTLs with heartbeat, explicit abandon alongside passive expiry, deterministic rejection of late reports after reclamation, and restore-time lease validation (clock-skew bounds, timestamp ordering, counter consistency, re-lease-from-now for pre-lease entries).
- Every host wake is a restart, so intentionally process-local state (dynamic Cordis definitions, pending dual-half approvals) is absent at each turn. The turn design must state what the model is told about that absence and route any re-definition through the normal define/approval pipeline, per the restart-behavior note in the reload ADR.

## Dynamic-definition re-materialization

An explicitly invoked flow that re-submits the durably retained `cordis_define` tool-call source through the normal define, syntax-check, and approval pipeline after a host restart or when a session log is opened by a different owner. Not automatic persistence; the source's restart semantics are unchanged. Needed by the turn mode above and by any session-sharing feature. Requires its own design record and, if any observable behavior differs from the source, a `DEVIATIONS.md` entry.

## Browser-hosted deployment

The full host compiled to `wasm32` with OPFS persistence, host and client in one tab. The typed gateway then runs over an in-memory seam, which ADR open decision 4 already requires the browser ABI to tolerate. Largest greenfield item; last in order.

## Ordering

Determinism spike, then effect transcripts, then deterministic simulation, then turn-per-invocation (with re-materialization), then browser-hosted deployment. The first three also serve port verification itself; the last two are deployment modes with no parity-era footprint beyond the provisions named above.
