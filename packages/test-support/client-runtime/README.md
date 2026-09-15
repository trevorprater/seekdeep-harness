# @seekdeep-ai/seekdeep-client-test-runtime

English | [中文](README.zh.md)

Rust/WASM browser Slot test runtime for Client feature specs. Its ESM entry initializes the compiled `seekdeep-client-test-runtime` crate, whose WASM links compiled Cordis and the compiled Web React renderer, and supplies only React, Testing Library, Vitest, and Immer as framework-bound adapters. Lifecycle, fixture, observable, ownership, and assembly behavior remains in Rust.

`SlotTestRuntime.create()` assembles a real compiled Cordis `Context`, the production Rust Slot and Conversation registries, the production Web React renderer, and Rust-owned Session and Workspace doubles. Feature suites exercise declaration, registration, scope, Store identity and pruning, inject, rendering, updates, Fiber disposal, and complete teardown without rebuilding those mechanisms per suite. Feature mounts receive caller-bound registry faces so registrations, declared children, provided services, and Conversation definitions share the feature Fiber's lifetime.

The public doubles retain the oracle's faces: `TestSessions`, `TestWorkspaces`, `FixtureSession`, `TestRemote`, and `stubSettingsScope`; provide-bundle materialization delegates to the production Rust `SessionProvideChannel`. Fixtures feed ordinary list rows, immutable conversation snapshots, projection values, and explicit behavior overrides. Unstubbed Session verbs fail with the method name, Remote listener failures propagate deliberately, and settings writes remain Vitest spies around Rust-owned observable state.

For local DOM snapshots, `declare(children)` installs an automatic frame whose per-key `<div data-slot>` wrappers are snapshot roots; `renderSlot(key, owner)` returns the local container, scoped Testing Library queries, and an in-place `update(owner)`. The Vitest serializer delegates class folding and SVG fingerprinting to Rust over a clone. Suites needing a custom page frame use `root.declare(children, Frame)`; `mount(plugin)` performs fail-loud service preflight, and `dispose()` tears down views, feature Fibers, minted Session scopes, and persisted Store state on one axis.

Build the distributable test library with `cargo xtask wasm-package --package seekdeep-client-test-runtime --artifact seekdeep_client_test_runtime --module-id @seekdeep-ai/seekdeep-client-test-runtime --out-dir packages/test-support/client-runtime/lib`. The ignored `lib/` directory contains an ESM wrapper with embedded optimized WASM bytes, the inspectable standalone WASM artifacts, declarations, and an invariant companion generated from Rust sources.

Not part of the product plugin graph (no `seekdeep.client`); feature packages depend on it in `devDependencies` only.

## Model Experience

None, as this package is browser-side test infrastructure; nothing here reaches a model request.

#### KV Cache effect

None; this package neither assembles nor sends a provider request.

## Known Limitations and Deferred Work

- **Browser test environment required.** The ESM entry imports Vitest and Testing Library and initializes embedded WASM bytes without a network fetch. Consume it through an ESM-aware browser test runner or bundler with DOM globals; it is not a plain Node utility module and never enters the product plugin graph.
- **Conversation snapshots are fixture data, not replayed history.** `updateSnapshot` writes the snapshot store directly; the wire-to-snapshot computation stays covered by the runtime package's own tests and the replay e2e. A fixture can therefore express states the production projection would never produce.
