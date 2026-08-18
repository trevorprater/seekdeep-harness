# Dynamic plugin reload architecture

Status: accepted architecture decision, 2026-08-18.

SeekDeep Harness preserves the source harness's ability to replace plugin generations while the process is running. Native Rust remains the host implementation, but native Rust dynamic libraries are not the reload boundary. Reloadable executable plugins use Rust compiled to WebAssembly; native-only integrations use replaceable Rust subprocesses behind a stable protocol.

This is an implementation architecture, not a behavioral deviation. Reload timing, visibility, rollback, service/event ordering, state continuity, failure diagnostics, and teardown must remain compatible with the pinned source oracle.

## Two meanings of reload

### Lifecycle reload

The plugin implementation is already linked into the host. A configuration, composition, or scope change disposes one plugin instance and mounts a fresh instance of the same compiled implementation.

Native Rust handles lifecycle reload directly through Cordis contexts, fibers, and reversible effects. This is the normal path for built-in host plugins.

### Code reload

The executable implementation itself changes while the harness remains running. A new artifact must be loaded, initialized, made visible atomically, and substituted for the prior generation.

SeekDeep does not implement code reload by unloading ordinary Rust dynamic libraries. Rust has no stable language ABI, and unloading a library while callbacks, futures, task-local state, trait objects, or registrations refer to it cannot satisfy deterministic teardown without a much narrower foreign ABI. The reloadable code format is a Rust-produced WebAssembly component with an explicit versioned interface.

## Runtime placement

```text
Native SeekDeep host
├── native Cordis context
│   ├── built-in Rust plugins
│   ├── persistence, tools, providers, and sandbox policy
│   └── native integration process supervisors
├── WebAssembly plugin stores
│   └── reloadable Rust plugin generations
└── typed API/RPC gateway
    ↕
Browser WebAssembly application
└── browser Cordis context
    └── browser-side Rust/WASM plugins
```

- Built-in host, CLI, persistence, provider, storage, sandbox, and tool implementations run as native Rust.
- Browser code and browser-side plugins run as Rust compiled to WebAssembly.
- Plugins that need runtime code replacement are distributed as Rust-produced WebAssembly components.
- Native integrations that cannot execute in WebAssembly run as separately replaceable Rust processes behind versioned RPC.
- Native and browser Cordis contexts are separate ownership graphs. The typed API gateway bridges them; Rust pointers and trait objects never do.

Not every plugin is forced through WebAssembly. WebAssembly is the portable reload boundary, not the universal execution substrate.

## Cordis target structure

Cordis must expose one semantic contract on native and browser targets:

- context inheritance and service lookup;
- plugin dependency readiness;
- event ordering and waterfall semantics;
- scoped routing;
- reversible registrations;
- fiber-owned effects;
- deterministic cancellation, rollback, and reverse-order disposal;
- generation-aware service and callback identity.

The current `seekdeep-cordis` crate is native-oriented: it directly uses `tokio`, `parking_lot`, and native `Send`/`Sync` assumptions and has no `wasm32` integration. The intended implementation is a target-portable semantic core with target-specific execution support:

- a native executor for host Rust plugins;
- a browser executor for `wasm32`;
- a native WebAssembly-component host for dynamically loaded generations.

Target-specific task spawning, timers, synchronization, and host I/O must not change observable Cordis ordering or lifecycle behavior.

## Generation model

Every code-loaded plugin instance has a host-minted opaque generation ID. A generation owns:

- its WebAssembly store and instance;
- its child Cordis context and root fiber;
- every service, listener, prompt section, tool, command, route, and other registration it contributes;
- its tasks, timers, streams, and pending host calls;
- its granted capability handles;
- its optional versioned persisted state.

Registrations cannot outlive their generation. The host never accepts a raw plugin-provided generation ID or a process-local pointer as durable identity.

## Atomic reload protocol

One owner coordinates a reload for a plugin slot:

1. Detect and read the candidate artifact without changing the active generation.
2. Verify artifact identity, interface version, declared capabilities, configuration, and compatibility metadata.
3. Instantiate the candidate in a new isolated store.
4. Create a staging Cordis context and fiber owned by the candidate generation.
5. Provide only the approved host capability handles.
6. Initialize the candidate and await its declared readiness boundary.
7. Validate that all required services and registrations exist and no prohibited collision is present.
8. Atomically commit the routing/registration generation so new work sees the candidate.
9. Stop admitting new work to the old generation.
10. Drain or cancel old in-flight work according to each operation's lifecycle contract.
11. Dispose the old generation's effects in reverse ownership order.
12. Drop its capability handles, WebAssembly instance, and store.

Before step 8, any failure disposes only the staging generation and leaves the active generation unchanged. After step 8, a failure during old-generation teardown is reported and retried without making the old generation visible to new work again. A rollback that reactivates an old generation is allowed only when its store and registrations have intentionally remained quiescent and intact; reloading an already disposed instance is forbidden.

Concurrent reload requests for one slot serialize by generation. Completion from an older request cannot remove or overwrite a newer generation. This uses the same exact-generation guard required for all reversible Cordis effects.

## In-flight calls

Dispatch snapshots the selected generation before entering plugin code. The call remains attributed to that generation until it settles.

- A committed generation swap affects only newly admitted work.
- Quiescent operations drain normally.
- Cooperatively cancellable work receives the generation's cancellation signal and must settle after owned work stops.
- A hard-kill boundary is the WebAssembly store or native integration process, never an unloaded Rust library containing live objects.
- Teardown waits for the generation's required drain boundary before releasing registrations and state.

No caller may observe a mixed call in which schema/service resolution came from one generation and execution came from another.

## State continuity

Rust objects, pointers, trait objects, futures, closures, and task handles never cross a generation boundary.

State uses one of these explicit paths:

1. Reconstruct from the append-only session log.
2. Read and write through a versioned SeekDeep storage domain.
3. Export a versioned, lossless data value from the old generation and import it into the staging generation before commit.

Model-visible state must remain reconstructable from the session log. An export/import hook cannot become a hidden model-memory channel. Unknown state versions fail before the candidate becomes visible. Migration writes are transactional or generation-qualified so a failed staging generation cannot corrupt active state.

## Capability boundary

WebAssembly plugins receive opaque host capabilities rather than ambient native access. Capabilities may cover:

- logging and diagnostics;
- session reads and append requests;
- storage-domain operations;
- tool, command, prompt, and service registration;
- approved network routes;
- approved filesystem roots;
- time, randomness, and task scheduling;
- typed API gateway calls.

The host validates every handle, generation, resource ID, and wire value. Capability revocation is part of generation disposal. Browser plugins receive browser-appropriate capabilities and never inherit native host access.

## Native-only reloadable integrations

An integration requiring native libraries, subprocess control, unrestricted filesystem APIs, operating-system dialogs, or another non-WebAssembly facility may be a separately launched Rust process.

The host treats the process as one generation:

1. start the candidate process;
2. negotiate a versioned protocol and capabilities;
3. await readiness;
4. atomically switch routing;
5. drain the old process;
6. terminate it after the protocol shutdown boundary.

Process restart is the unload mechanism. The protocol carries versioned values and opaque IDs, never Rust ABI objects.

## Development versus production

- Configuration and composition edits use native lifecycle reload when code has not changed.
- Reloadable plugin code changes produce a new WebAssembly artifact and generation.
- Browser application core changes may replace the browser WebAssembly application while restoring durable UI/session state.
- Changes to statically linked native host code replace or restart the host binary and restore sessions; SeekDeep does not pretend that linked Rust code can be safely unloaded.

## Required verification

Dynamic reload is not complete until automated tests prove:

1. A native built-in can be disposed and remounted without residual registrations.
2. A WebAssembly generation can register every supported Cordis surface.
3. A successful generation swap is atomic for new dispatch.
4. In-flight calls finish against the generation they entered.
5. Failed candidate initialization leaves the active generation byte-for-byte and behaviorally unchanged.
6. Failed state migration leaves active persisted state unchanged.
7. Old-generation teardown drains, cancels, and disposes in the required order.
8. Stale completion from an older reload cannot remove a newer generation.
9. Missing or excessive capabilities fail before visibility.
10. Unknown interface and state versions fail with stable diagnostics.
11. Repeated reload cycles leak no services, listeners, tasks, stores, processes, or capability handles.
12. Native and browser Cordis implementations pass the same semantic conformance suite.
13. Browser reload preserves the source application's user-visible behavior.
14. Native integration process replacement obeys the same atomic swap and rollback rules.

The final parity gate must exercise real reloads, not infer reload correctness from plugin construction or compilation.

## Non-goals

- Loading JavaScript or TypeScript as production plugin behavior.
- Treating native Rust dynamic-library unloading as the general plugin system.
- Sharing memory or Rust ABI objects between generations.
- Letting every plugin access every host capability.
- Claiming code reload when only configuration remounting works.
- Using reload state that cannot be reconstructed or audited through durable contracts.

## Current implementation status

- Native Cordis contexts, services, events, fibers, and reversible effects exist.
- Native lifecycle remounting is partially implemented and covered by package parity tests.
- Browser Cordis/WASM execution is not implemented.
- The native WebAssembly component loader and capability ABI are not implemented.
- Generation-aware dynamic routing and state migration are not implemented as a general subsystem.
- Native integration subprocess replacement exists only in package-specific lifecycle pieces, not yet as the general reload protocol described here.

Until those remaining pieces and verification cases land, SeekDeep supports native plugin lifecycle replacement but not full runtime code reload parity.
