# Dynamic plugin reload architecture

Status: proposed architecture decision, 2026-08-18.

This document describes how SeekDeep Harness can preserve the pinned source harness's reload behavior while keeping the production implementation in Rust. It is not authorization to remove a source-visible language, protocol, failure mode, lifecycle state, approval step, or user-facing surface.

The source oracle remains authoritative wherever this proposal is incomplete. A stronger or more atomic implementation is still a behavioral deviation when users can observe a different intermediate state, rollback result, diagnostic, or recovery path.

Additive-modes principle: an optional, explicitly enabled mode (a recorder, a deterministic simulation harness, an alternative deployment strategy) is permitted when it does not alter what the parity path does, logs, or reports, and when disabling it restores byte-identical parity behavior. This principle authorizes design headroom — interfaces may be shaped so such modes remain possible — not construction; additive modes themselves are post-parity work and are built only on instruction.

## Decision summary

- Built-in host plugins run as native Rust.
- Browser application code and browser-side built-ins run as Rust compiled to WebAssembly.
- Runtime-loadable binary plugins use an explicit, versioned WebAssembly boundary.
- Native-only reloadable integrations use replaceable Rust subprocesses behind versioned RPC.
- Ordinary Rust dynamic-library unloading is not the general reload mechanism.
- Model-authored JavaScript accepted by the source dynamic Cordis tools remains a compatibility surface. It is runtime data evaluated or translated by Rust-owned infrastructure.
- Configuration remount, host file HMR, browser HMR, and model-defined dynamic packages retain separate source-compatible state machines.
- Model-visible state remains reconstructable from the append-only session log. This does not make intentionally process-local state persistent.

## Source reload inventory

SeekDeep must preserve four different reload mechanisms.

| Mechanism | Source behavior that remains authoritative | Primary oracle surfaces |
|---|---|---|
| Loader composition and config reload | Diff entries by stable or generated ID; mount, unmount, move, disable, isolate, or reconfigure only affected entries; dependency-missing plugins may remain PENDING; failed updates retain the source-defined last-good or rollback behavior. | vendor Loader, app-boot config reload tests, Cordis composition tutorial |
| Host file HMR | Watch source dependencies, invalidate the host module cache, replace affected plugin fibers in source order, and perform the source's best-effort rollback and diagnostics. | vendor HMR package and tests |
| Browser HMR | Consume browser reload notifications and serialize reloads in the exact source order: invalidate, prefetch while the old fiber serves, registry-first teardown, drain, remove owned styles, then refresh; preserve the source's intentional no-rollback failure states. | packages/client/hmr and tests |
| Model-defined dynamic Cordis packages | Accept Host and Client JavaScript function bodies from cordis_define; syntax-check without effects; keep immutable package versions and process-local definitions; run Host-only packages directly; route dual-half packages through human approval and per-page Client loading; preserve stop, undefine, inventory, invocation, stale-revision, and render-failure semantics. | packages/extensions/tool-cordis, cordis-host-runner, cordis-client-runner, ui-cordis, and their tests |
| Full Host or browser-core replacement | Host framework or external changes cross the full-restart boundary rather than partial HMR; browser shell or platform changes reload the browser application; restore only state the source restores and reset page-local state the source forgets. | vendor HMR restart classification, client HMR shell boundary, web reload tests |

These mechanisms may share implementation primitives, but sharing cannot erase their observable differences.

## Model-authored source compatibility

The source cordis_define tool accepts JavaScript Host and Client async-function bodies. That input schema, accepted syntax, error vocabulary, teaching diagnostics, approval flow, and resulting behavior are part of parity.

Production implementation in Rust means the evaluator, sandbox, loader, capability facade, and lifecycle owner are implemented in compiled Rust or Rust/WASM. It does not mean model-authored JavaScript ceases to be valid input.

A compatible implementation may:

1. evaluate the JavaScript with a Rust-owned interpreter;
2. translate the accepted subset into a validated intermediate form and execute it in WebAssembly;
3. use another Rust-owned sandbox that produces identical observable behavior.

It must not silently require cordis_define callers to submit Rust source or a prebuilt WebAssembly artifact. Removing the JavaScript surface requires an explicit deviation entry and a user-approved change to the 100% parity goal.

The compatibility path must preserve:

- Host and Client source as separate optional halves;
- syntax validation before a dynamic definition ID exists;
- no JSX, TypeScript syntax, or module imports where the source forbids them;
- the source symbol surfaces and teaching traps;
- declared-service guarding and PENDING activation;
- Host-to-Client sequencing and package-internal JSON RPC;
- the exact source refusal and diagnostic categories;
- source-defined synchronous timeout scope and cooperative asynchronous behavior.

## Dynamic Cordis state model

Internal generation identity supplements rather than replaces source protocol identity.

The source-visible model includes:

- a session-owned dynamic Plugin ID;
- immutable Package IDs and revisions;
- current and next package pointers;
- a Host Run and its dispatch revision;
- an optional answerable run request;
- per-page Client activation keyed by Plugin ID and revision;
- process-global Host inventory;
- page-local loaded state and page-local render failures;
- the Host's last render-failure report per definition across all pages.

A host-minted opaque generation ID owns implementation resources for one activation, but it never appears in place of the source's Plugin, Package, Run, request, or revision IDs.

### Define

Define validates and records metadata and source without running either half. Invalid source fails before an ID is minted. The registry is process memory, matching the source. The durable tool-call arguments retain the submitted Host and Client source for conversation history and replay-safe presentation, but a restarted Host intentionally does not reconstruct the process-local definition registry from those arguments. Run announcements and inventory never carry executable source.

### Run: Host-only

A Host-only package starts in the host without an approval round trip to a page. Concurrent attempts converge on one Host activation. A running matching package binds rather than evaluates twice.

### Run: dual half

A package with a Client half creates the source-compatible answerable request. The request has no independent timeout; caller cancellation is its lifetime boundary. The first valid answer wins.

The answering page performs:

1. start or bind the exact Host package revision;
2. fetch Client source for that exact active run;
3. load the Client half in that page;
4. return one resolution.

A stale success is rejected according to the source protocol. A Client load failure unwinds the Host half only when that request started it. A page failure must not stop a Host half used by other pages.

### Page-local activation

Host running state and Client loaded state are independent facts. A refreshed page starts with no dynamic Client packages loaded even when the Host remains running. Loading converges by Plugin ID and revision within one page. A newer revision replaces the prior page-local activation; a retract followed by the same revision loads afresh.

### Stop and undefine

Stop quiesces and disposes the active Host run, broadcasts retraction, and leaves each page to unload its matching Client activation asynchronously. It retains the definition and immutable packages. Repeating stop on an already stopped definition preserves the source not-running refusal. Undefine stops first and then removes the definition, packages, grants, and version pointers; repeating it preserves the source plugin-missing refusal. Both preserve exact ownership and absent-versus-forbidden behavior.

### Restart behavior

Dynamic definitions are not restored automatically after host restart, matching the source. Browser Client halves are not restored automatically after page refresh. General state migration rules elsewhere in this document must not override these source-specific semantics.

These semantics forbid automatic reconstruction, not the existence of a path back. A future additive, explicitly invoked flow that re-submits the durably retained tool-call definition source through the normal define/syntax-check/approval pipeline is not the forbidden automatic persistence. No such flow exists or should be built now; this note only prevents invariant tests from being written so broadly that they foreclose it.

## Runtime placement

~~~text
Native SeekDeep host
├── native Cordis host context
│   ├── built-in Rust plugins
│   ├── source-compatible dynamic Host evaluator
│   ├── native-hosted WebAssembly plugin actors
│   └── native integration process supervisors
└── typed API and event gateway
    ↕
Browser application
├── Rust/WASM browser Cordis context
├── source-compatible dynamic Client evaluator
└── browser WebAssembly plugin actors
~~~

- Native persistence, tools, providers, storage, sandbox policy, and operating-system integrations remain native Rust.
- Browser Cordis and browser-side built-ins compile from Rust to WebAssembly.
- Native-hosted WebAssembly components and browser WebAssembly modules have separate host adapters. The proposal does not assume browsers directly provide the same component host as the native runtime.
- Native and browser contexts are separate ownership graphs connected by typed protocol messages.

WebAssembly is a portable code boundary, not a requirement that every plugin execute through WebAssembly.

## Cordis portability decision

The Cordis rewrite is a prerequisite, not an incidental implementation detail.

The proposed direction is:

- a single-owner, target-neutral semantic core whose mutation and lifecycle APIs do not require Send;
- native Send and Sync handles that marshal work to the core owner;
- direct event-loop ownership in the browser;
- one actor owner per dynamically hosted WebAssembly generation;
- generated proxies for every service intentionally exposed across an ABI.

Before changing public Cordis bounds, write a semantic conformance suite against the current native implementation. That suite becomes the internal oracle for native Cordis after refactoring, browser Cordis, and native-hosted WebAssembly generations.

The conformance suite must pin:

- context inheritance and service shadowing;
- dependency readiness and legitimate PENDING state;
- scoped routing;
- event modes, waterfall order, bail behavior, and reentrancy;
- effect registration and exact-generation removal;
- disposer start order and concurrency;
- child-fiber teardown;
- error aggregation and settlement;
- service loss and later reactivation.

Source Cordis starts disposers in reverse registration order while async disposers may overlap. The port must not replace that with sequential reverse completion unless the source oracle proves equivalence. When overlapped disposal is implemented, its interleaving must be drivable by an injectable scheduling policy so a seeded scheduler can reproduce a given interleaving in tests and simulation; production keeps the normal executor. If deterministic whole-system replay is later abandoned as a goal, plain unseeded overlap satisfies this section.

Teardown observability is a deliberate two-layer contract. The internal substrate (`EffectHandle`, fiber-level disposal) collects, labels, and aggregates disposer failures and remembers the disposed outcome; this is intentional and must not be "corrected" back to the source's swallowing (`vendor/cordis/src/fiber.ts` `_unload` catches every disposer failure into a logger). Each source-visible mechanism (config remount, Host HMR, browser HMR, dynamic packages) then swallows or reports at its own boundary exactly where the source does, so users observe source behavior while the substrate observes complete teardown outcomes. Mechanism ports must test both halves: the substrate saw the failure, and the parity surface did not change.

Current arbitrary Any plus Send plus Sync services cannot cross a WebAssembly ABI. Cross-boundary services require explicit versioned interfaces and generated proxies.

## Host and browser WebAssembly boundaries

### Native host

The native host may use a WebAssembly component runtime with versioned interface definitions. Every interface defines:

- value and error encoding;
- unknown-value preservation;
- opaque resource handles;
- handle ownership and revocation;
- async call and stream behavior;
- callback and reentrancy rules;
- cancellation;
- size, depth, memory, and call limits;
- interface-version negotiation.

The host reads candidate bytes once, hashes that immutable snapshot, verifies exactly those bytes, and instantiates exactly those bytes. Artifact verification cannot race a second filesystem read.

### Browser

The browser uses Rust/WASM modules with generated browser bindings or an explicitly bundled component-lowering adapter. It does not assume a native component runtime is present.

Browser UI registration needs an explicit ABI. Dynamic code cannot pass Rust closures, trait objects, DOM nodes, or framework component objects between independent memories. The design must choose one or more of:

- host-owned UI component handles;
- a declarative render tree and event protocol;
- source-compatible Client evaluation inside the main browser runtime;
- generated bindings for a fixed UI slot contract.

The chosen path must preserve styles, slot ownership, observables, page-local failure reporting, and unload behavior from the source Client runner.

## Generation ownership

Every binary code generation owns:

- an opaque internal generation ID;
- one single-owner actor and execution queue;
- its WebAssembly store or native process;
- a staging or active Cordis context and root fiber;
- every contributed service, listener, tool, command, route, prompt section, and UI entry;
- all tasks, timers, streams, and pending host calls;
- capability handles;
- operation leases;
- optional versioned state.

Registrations cannot outlive their generation. Completion from an old generation can remove only resources bearing that exact generation token.

A generation lease propagates through nested tool calls, service lookups, event emissions, callbacks, and gateway calls. Pinning only the top-level dispatch is insufficient. Intentionally detached work must acquire an explicit generation-owned lease.

## Transactional publication for new binary generations

This section applies to new binary WebAssembly generations. It does not replace the distinct source HMR failure policies described later.

Current Cordis registrations publish immediately, so a child context alone is not a staging boundary. Binary generation staging requires shadow registration tables for every cross-generation surface. Nothing in a shadow table is visible to active dispatch.

One graph transaction coordinates all mutually dependent slots affected by a reload. Per-slot locks alone are insufficient for a multi-plugin dependency update.

The cutover has one linearization point:

~~~text
old generation: active -> draining
new generation: staging -> active
active graph snapshot: old -> new
~~~

The three changes occur in one commit while admission is fenced. There is no separate window in which the new pointer is published but old admission remains open.

Dispatch snapshots the immutable active graph and generation leases before schema or service resolution. Schema resolution and execution cannot come from different generations.

## Staging capabilities and side effects

Staging cannot honestly promise rollback while it can perform irreversible effects.

Before commit, a candidate receives:

- read-only capabilities;
- generation-local registration capabilities writing only to shadow tables;
- explicitly transactional storage;
- buffered outputs that publish only on commit;
- compensatable operations with a recorded abort action.

Network sends, arbitrary filesystem writes, session-log appends, process launches, and other irreversible effects are denied before commit unless the source mechanism explicitly performs them and its failure semantics are being preserved rather than strengthened.

Every staging effect belongs to a ledger. Abort must leave:

- no visible registrations;
- no committed buffered output;
- no pending host calls;
- no tasks or timers;
- no unreleased capabilities;
- no migrated state revision.

If a source HMR mechanism permits irreversible apply effects before failure, its source-compatible path documents and tests that behavior separately instead of claiming byte-for-byte rollback.

## Mechanism-specific replacement policies

### Config remount

Preserve Loader entry identity, config interpolation, disabled state, grouping, isolation, dependency waiting, move behavior, and the source's transactional tree update and rollback rules. Preserve cordis built-ins, base-relative and bare-package name resolution, and default-export/module interop at the unchanged name boundary. A non-replacement config edit follows the update waterfall. A replacement caused by name, injection, or group shape disposes the old fiber before applying the candidate; candidate failure reconstructs the prior plugin and config, and rollback failure remains an aggregate failure. Watched refreshes serialize and coalesce, report the source failure event, keep watching after a rejected edit, and settle before watcher disposal completes.

### Host file HMR

Preserve dependency-graph classification, cache invalidation, replacement ordering, log and events, and source-defined best-effort rollback. Candidate modules are prepared while old runtimes still serve. Import failure restores cache state without replacing live fibers. Affected runtimes then dispose and start in source order; candidate apply failure restores prior artifacts and performs the source's best-effort old-plugin reconstruction. The success event emits only after the whole reload set succeeds. Framework or external changes request a full Host restart instead of pretending statically linked Rust can unload. A stronger blue/green strategy may be an optional deployment mode only when it does not alter the parity path.

### Browser HMR

Preserve the source's serialized reload queue, style ownership, module invalidation, provider-dependent cascades, and intentional no-rollback result. The browser invalidates and prefetches while the old fiber still serves, then performs registry-first teardown, drains the old fiber, removes owned styles, and refreshes the entry. Prefetch failure leaves the old fiber serving with its module registration invalidated. Import failure after teardown leaves the entry fiberless. Apply failure leaves a failed fiber. These states are observable and are not replaced with Host-style rollback.

### Model-defined package update

The current package pointer changes only after the source-defined Host and Client success boundary. A failed target remains available as the next package where the source does so; the old current package and existing users are unwound only according to the source request and resolution rules.

### Full Host or browser-core replacement

Framework and external Host changes cross the source full-restart hook rather than partial plugin replacement. The owning SeekDeep launcher or supervisor defines whether that hook actually restarts the process; the architecture does not assume the source's empty default hook already does. Browser shell and platform module changes reload the browser application. Durable sessions and history recover through normal APIs, while page-local dynamic activations and UI state reset wherever the source resets them.

### New binary plugin deployment

For a reload surface with no stricter source policy, choose and declare one:

- no post-commit rollback; recovery installs another generation;
- blue/green linger for a bounded health window;
- explicit operator rollback while the old generation remains quiescent and intact.

An already disposed generation is never reactivated.

## State continuity

Rust objects, pointers, trait objects, closures, futures, and task handles never cross a generation boundary.

State uses an explicit source-compatible path:

1. reconstruct model-visible facts from the append-only session log;
2. read and write a versioned storage domain;
3. perform cooperative export/import as an optimization.

Export/import is not the guarantee. The old generation may be broken or hung.

- Export has a bounded deadline.
- A hard-killed generation cannot provide trusted export state.
- Failure falls back to the source-defined durable state, which may mean no restoration.
- Model-visible state cannot hide exclusively inside export data.

Export also needs a consistency boundary. Choose one:

- quiesce old admission and drain mutations before snapshot;
- compare-and-swap a versioned snapshot and retry on conflict;
- snapshot then replay an ordered delta through the commit point;
- use generation-qualified state with an atomic active pointer and a rule for late old writes.

Migration writes remain isolated until commit. A failed candidate cannot change the active state revision.

## Rollback and health

Rollback is mechanism-specific.

- Config and Host HMR follow their source rollback behavior.
- Browser HMR follows its source no-rollback behavior.
- Dynamic packages follow current and next package pointer semantics.
- Generic binary deployment declares no rollback or a bounded blue/green health window.

Blue/green linger retains generation N-1 quiescent and intact until the health window closes. It consumes an additional store or process and cannot be assumed for exclusive resources.

## Exclusive native resources

Reloadable native integrations declare one handover class:

- overlap-safe: candidate and old process may run together;
- explicit-handover: the old process transfers a token, descriptor, or external lease;
- stop-then-start: old stops before candidate readiness, with acknowledged downtime.

The process protocol defines readiness, drain, shutdown, crash, and retry behavior. Process restart is the native unload boundary.

RPC alone is not a sandbox. Each integration also declares whether it is trusted or constrained by an operating-system sandbox, minimal environment, restricted handles, filesystem policy, and network policy.

## Hard termination and resource control

Each hosted generation has:

- initialization deadline;
- state-export deadline;
- cooperative drain deadline;
- cancellation grace period;
- hard-termination policy;
- memory, table, and handle limits;
- guest execution fuel or epoch interruption where supported;
- bounded, cancellation-aware host imports;
- stable diagnostics for each termination phase.

The single-owner actor stops polling guest work at hard termination. Every guest invocation, callback future, and host future borrowing guest memory or a component resource must settle or be dropped before store destruction. A detached native operation may survive only after copying every input it needs and retaining no store, guest-memory, callback, or component-resource reference; its later completion remains generation-fenced so it cannot publish stale effects. Store drop happens only after this zero-borrow boundary.

Retryable cleanup belongs to a supervisor-owned idempotent resource ledger. Ordinary Cordis effects are single-shot and cannot be made retryable by calling fiber disposal twice after their disposers have been consumed. The supervisor retains the remaining resources, retry policy, backoff, terminal degraded state, and shutdown behavior until cleanup succeeds or reaches an explicit terminal policy.

The source dynamic Host evaluator's synchronous timeout and cooperative async behavior remain source-visible compatibility requirements. Stronger limits require either equivalent diagnostics or an authorized deviation.

## Capability and trust model

General WebAssembly plugins receive opaque capabilities instead of ambient native authority. The host validates every handle, generation, ID, and wire value.

The source model-defined dynamic package sandbox is explicitly not a security boundary and is treated similarly to powerful tool access. Its compatibility facade must preserve source-visible service guarding and errors. Hardening that rejects behavior the source accepts is a deviation unless separately approved.

Browser generations receive browser-appropriate capabilities and never native host authority.

Native subprocess integrations inherit only declared environment values and handles unless classified as trusted and unsandboxed.

Capability revocation occurs in phases:

1. stop new admissions;
2. retain capabilities needed by admitted calls during drain;
3. fence late completion by generation;
4. revoke and release after the drain boundary.

## Source-oracle verification matrix

Dynamic reload is not complete until tests cover the source mechanisms below.

### Loader composition and config

- stable and generated entry IDs;
- add, remove, move, disable, enable, and config update;
- nested groups, overlays, intercepts, and isolate;
- PENDING dependencies and later activation;
- invalid config and last-good behavior;
- transactional tree rollback and disposer ordering;
- canonical path aliases and watcher coalescing;
- add, change, and unlink watcher events against the deepest existing ancestor;
- cordis built-in, relative, and bare-package resolution plus export interop;
- disposal while reload work is pending.

### Host HMR

- add, change, and unlink events;
- dependency graph classification;
- external-file full restart;
- ESM and CJS cache invalidation;
- replacement order and partial failure;
- source-defined rollback;
- exact HMR events, logs, and diagnostics.

### Browser HMR

- reload notification framing;
- graph baseline and immediate rehash;
- missing and reappearing bundles with dirty-state retention;
- malformed frames logged and future unknown frame types ignored;
- serialized reload queue;
- the intentional self-reload channel gap;
- module and style invalidation;
- provider-dependent cascade;
- successful replacement;
- intentional no-rollback import and apply failures;
- teardown quiescence and error projection.

### Dynamic Cordis packages

- exact cordis_define schema and JavaScript input acceptance;
- durable tool-call source replay without automatic registry reconstruction;
- Host-only and dual-half definitions;
- syntax precheck before ID minting;
- immutable packages and version pointers;
- session ownership and absent-versus-forbidden behavior;
- approval with no independent timeout;
- first-answer-wins and cancellation;
- Host-first orchestration and Client fetch;
- idempotent concurrent Host start;
- stale revision refusal;
- per-page activation and page-refresh emptiness;
- package-internal JSON RPC and teaching errors;
- stop versus undefine;
- repeated stop and undefine refusal semantics;
- process-memory registry loss on restart;
- post-settle render failure reporting with distinct page-local and Host-across-pages state, ownership, and clearing rules.

### Portable Cordis and binary generations

- native and browser semantic conformance;
- shadow-table invisibility before commit;
- graph-wide atomic cutover;
- nested and reentrant generation leases;
- concurrent mutation during state migration;
- hostile or hung old generation;
- artifact TOCTOU prevention;
- capability denial and transactional staging;
- fuel, memory, host-call, and shutdown exhaustion;
- stale generation completion;
- exact effect-ledger emptiness;
- repeated reload without task, store, process, handle, or registration leaks.

### Native integration processes

- overlap-safe swap;
- explicit handover;
- stop-then-start downtime;
- candidate crash before readiness;
- old process crash during drain;
- protocol mismatch;
- sandbox and inherited-authority checks.

Each ported mechanism runs its source tests where possible and adds differential tests over identical inputs and event timelines. Compilation or successful construction is not reload evidence.

## Implementation phases

1. Inventory and pin every source reload test and observable state machine.
2. Write the Cordis semantic conformance suite against current native behavior.
3. Decide and implement the single-owner portable Cordis core and native handles.
4. Port Loader config remount and Host HMR with source failure semantics.
5. Port browser Cordis and browser HMR.
6. Implement the source-compatible model-defined Host and Client evaluators.
7. Define versioned native-host WebAssembly interfaces and generated proxies.
8. Define browser WebAssembly bindings and UI registration ABI.
9. Implement shadow registries, graph transactions, generation leases, and state cutover.
10. Implement native integration process generations and handover classes.
11. Run the complete source-oracle and cross-target reload matrix.

## Non-goals

- Using ordinary Rust dynamic-library unloading as the general plugin system.
- Sharing Rust ABI objects or process-local pointers across generation boundaries.
- Claiming one universal rollback policy for mechanisms that differ in the source.
- Claiming code reload when only configuration remounting works.
- Treating a child Cordis context as an atomic staging registry without shadow publication.
- Persisting source dynamic definitions or page activations when the source intentionally forgets them.
- Allowing model-visible state that cannot be reconstructed or audited through durable contracts.
- Silently removing model-authored JavaScript from the dynamic Cordis tool surface.

## Open decisions before acceptance

This ADR remains proposed until these decisions are resolved:

1. Confirm the single-owner, non-Send portable Cordis core or choose an alternative executor-generic design. Whatever design is chosen, its task and event scheduling must be injectable so a seeded deterministic scheduler can drive the core in tests and simulation while production uses the normal executor.
2. Confirm whether the implemented Rust-owned `boa_engine` Host evaluator remains the long-term compatibility strategy or is replaced by translation to WebAssembly, and select the Client evaluator strategy. The Host parity path already depends on accepting the source JavaScript surface and cannot remove it during a strategy change. Any chosen evaluator must support deterministic execution with seeded scheduling and no guest-visible ambient clock leakage. The final choice belongs to an evaluator bake-off against the source dynamic-Cordis corpus.
3. Define the native WebAssembly interface world and proxy-generation system.
4. Define the browser UI and callback ABI. The typed gateway between host and browser contexts must not assume a network transport: a deployment may place both sides in one process or one browser tab, so the ABI's reentrancy, backpressure, and failure-atomicity semantics must hold across an in-memory seam as well.
5. Choose the graph transaction and generation-lease implementation. Every cross-generation completion and cutover step must be observable as ordered data (an internal event sequence), not only as logger output, so transactions can be audited and replayed.
6. Choose migration consistency algorithms for each state class.
7. Define blue/green health policy and exclusive-resource metadata. Health-window evaluation must read time through an injected clock so window outcomes are reproducible in tests.
8. Identify any intentional behavioral deviations and record explicit authorization.

## Current implementation status

- Native Cordis contexts, services, events, fibers, and reversible effects exist.
- Native lifecycle remounting is partially implemented.
- The semantic conformance suite is incomplete.
- Rust/WASM Cordis executes the built browser registry, Remote gateway, and Client contributions; complete cross-target semantic conformance remains open.
- Browser context metadata uses a descriptor-preserving property object as the public Proxy target. Extension, plugin contexts, and isolated contexts retain inherited metadata; getters and setters receive the actual calling Context, readonly symbols remain readonly, and membership tests do not invoke getters. Source comparison and real browser Remote calls verify changing Agent identity through a live getter.
- Browser `ctx.get(name, strict)` preserves strict and relaxed provider visibility through startup, isolation, and withdrawal. Explicit service lookup remains distinct from reflected property access; source comparison and Chromium exercise both paths.
- The source's complete callable Logger interface remains a browser binding gap. Tests that install a recording logger sink do not close that obligation.
- Source-compatible dynamic Host execution runs in a Rust-owned interpreter worker. The guarded Host path covers lifecycle commands, Services, Tools, Events, callback and Promise timers, async-iterator intervals, throttle/debounce wrappers, exact-generation effect removal, cooperative callback pumping, and stack-safe lossless JSON cloning; the Client compatibility evaluator remains incomplete.
- Native and browser WebAssembly plugin hosts are not implemented.
- Shadow registries, graph transactions, generation leases, and general state migration are not implemented.
- Native integration process replacement exists only in package-specific pieces.

Until the source behavior matrix, open decisions, implementation, and verification are complete, SeekDeep supports only the native and browser lifecycle behavior already proven by the corresponding tests. It does not yet have full runtime code reload parity.
