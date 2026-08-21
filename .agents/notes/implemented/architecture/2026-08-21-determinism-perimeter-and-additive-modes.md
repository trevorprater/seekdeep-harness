# Agent Note: Determinism perimeter and additive-modes boundary

Status: implemented

English | [中文](2026-08-21-determinism-perimeter-and-additive-modes.zh.md)

## Problem

The workspace had no stated position on determinism. Ambient `Instant::now()`/`SystemTime::now()` appears in roughly forty call sites and would keep spreading crate by crate; overlapped fiber disposal was about to be implemented on ambient tokio scheduling; and the port had silently diverged from the source on teardown observability — `crates/cordis/src/fiber.rs` collects, labels, and aggregates disposer failures where the source's `_unload` catches every disposer failure into a logger — with nothing blessing that divergence, so a later pass could regress it in either direction.

Planned post-parity work (deterministic boundary-transcript replay, effect transcripts, whole-system simulation, a turn-per-invocation deployment mode; see [porting/POST_PARITY.md](../../../../porting/POST_PARITY.md)) depends on choices that are made during the port inside the reload ADR's open decisions. A portable Cordis core designed without an injectable scheduling seam, or a dynamic-code evaluator chosen without a deterministic mode, turns replay into a re-architecture instead of a feature.

## Decision

[AGENTS.md](../../../../AGENTS.md) declares a determinism perimeter. Crates implementing the Cordis core, agent loop, session-persistence decision logic, and retry/timeout policy take wall-clock time and task-interleaving policy through injectable seams, so a seeded scheduler and clock can drive them in tests. Boundary crates — terminal, subprocess, provider clients, sandbox, native integrations — may use ambient time because their effects are observed at the call boundary rather than simulated. The rule applies to new and substantially rewritten code only; verified crates are not retrofitted. The same section bans ambient randomness: any PRNG must be injected and seedable.

[The reload ADR](../../../../porting/DYNAMIC_PLUGIN_RELOAD.md) carries the companion provisions. Its preamble states the additive-modes principle: an optional, explicitly enabled mode is permitted only when disabling it restores byte-identical parity behavior, and the principle authorizes design headroom, not construction. Overlapped disposal must be drivable by an injectable scheduling policy, conditional on the replay goal: if deterministic whole-system replay is abandoned, plain unseeded overlap satisfies the requirement. Teardown observability is a two-layer contract: the substrate (`EffectHandle`, fiber disposal) aggregates labeled failure outcomes, each source-visible mechanism swallows or reports exactly where the source does, and mechanism ports test both halves. Open decisions 1, 2, 4, 5, and 7 carry determinism and transport-neutrality requirements; decision 2 records the `boa_engine` JITless-determinism rationale while the evaluator bake-off retains the decision itself. The restart-behavior section notes that the source's intentionally process-local dynamic-definition semantics forbid automatic reconstruction, not a future explicitly invoked re-materialization flow, so invariant tests are not written broadly enough to foreclose one.

## Alternatives considered

**Defer everything to post-parity.** Rejected: the ADR's open decisions are resolved during the port. A core without a scheduler seam, or an evaluator without a deterministic mode, forecloses replay at the architecture level, and retrofitting costs re-architecture where a requirement today costs sentences.

**Workspace-wide clock injection.** Rejected: refactoring the existing ambient-time sites would churn verified parity evidence across boundary crates for no replay benefit, since boundary effects are recorded onto the transcript rather than simulated. A perimeter is a fence for new code, not a migration.

**Unconditional seeded-scheduler requirement.** Rejected in favor of the conditional form. Seeded disposal interleaving has no value outside the replay goal, so the requirement dissolves into plain overlap if the determinism spike fails, rather than surviving as dead-weight indirection.

**Revert teardown aggregation to source swallowing.** Rejected: the source-visible mechanisms preserve source swallowing at their own boundaries regardless, and the substrate's aggregated outcomes are the foundation for differential teardown verification between the two implementations.

## Consequences

Bought: replay, transcripts, and simulation remain implementable without re-architecting the core; the teardown divergence is a recorded contract rather than an accident, protected against regression in both directions; every newly ported crate lands on the correct side of the perimeter by rule instead of later archaeology.

Cost: seam boilerplate in perimeter crates is a standing port-velocity tax; every reload mechanism carries a permanent dual test obligation (the substrate observed the failure, the parity surface did not change); and if the evaluator bake-off picks translation over `boa_engine`, the determinism rationale recorded on decision 2 must be re-derived for the winning path.
