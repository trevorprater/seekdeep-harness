# Agent Note: Rust client test runtime

Status: implemented

English | [中文](2026-09-02-rust-client-test-runtime.zh.md)

## Problem

Client feature tests share behavior-bearing doubles for Cordis services, observable stores, locale selection, translation, Session and Workspace fixtures, stabilization, and DOM snapshots. Recreating those helpers beside each Rust/WASM feature test would make the same test contract drift across crates, while retaining the TypeScript helper package as executable infrastructure would violate the Rust-production boundary and keep later Web-test ports dependent on a second runtime.

## Decision

`seekdeep-client-test-runtime` is the target-neutral Rust owner of reusable Client test doubles. Each helper exposes the production Rust contract when one exists and adds only explicit test controls such as publication, call records, or an opaque event dispatcher. Browser-only environment controls compile behind `wasm32`; the crate does not enter the product plugin graph.

The Remote double publishes the `remote` Cordis service, delivers opaque forwarded arguments to a registration-order listener snapshot, makes subscription disposal idempotent, leaves unknown events inert, and rejects generated namespace mounting with the source diagnostic. Listener errors intentionally propagate so this double cannot be used as evidence for the production Remote service's containment policy.

The settings stub implements `ClientSettingsScope`, starts with the source loading/read-only/Host snapshot, records ordered `set` and `unset` calls, replaces only supplied publication fields, and synchronously notifies a listener snapshot. The translator uses first-dictionary ownership, visible-key fallback, ASCII word placeholders, and JavaScript-compatible parameter stringification. The WASM browser-language guard installs the same configurable own `navigator.languages` and `navigator.language` values and deletes both during idempotent cleanup so inherited accessors become visible again.

The Workspaces double owns a production `SnapshotStore<RuntimeWorkspaceListState>`, routes list mutations through an injected stabilization owner, forwards the exact cancellation signal to directory stubs, and records every action as a typed ordered value. Its inert defaults preserve the source echoes, root-to-target home breadcrumbs, directory cancellation, and archive-list publication; typed replacement stubs keep action failures and coupled test behavior explicit.

The snapshot normalizer folds only CSS-module tokens matching `_<local>_<lowercase-hash>`, computes the SVG `data-content` fingerprint with wrapping FNV-1a over JavaScript UTF-16 code units, mutates only a deep clone, and preserves childless SVG elements. Rust snapshot tests call the normalizer directly rather than installing a Vitest serializer.

Session fixtures brand one identity and carry strongly typed mutations over the production `SessionSnapshot` and `RuntimeSessionSummary`, plus opaque behavior overrides. The native quiescent Session constructor follows the target object model, while the WASM constructor preserves every source browser field and its `null`, `undefined`, Array, and Map distinctions. Workspace fixture defaults are shared with the Workspaces double.

The WASM Sessions double preserves stable fixture snapshots and projection faces, listener identity, fail-loud unstubbed behavior, observable list updates, action records, search controls, and injected stabilization. It creates and disposes real scoped Client contexts, prunes scoped Slot stores on removal, and delegates provider registration, materialized bundles, and current-Session publication to the production Rust provide channel so test assembly exercises the same roster and scope behavior as the product runtime.

`SlotTestRuntime` assembles a compiled Cordis Context, production Rust Slot and Conversation registries, the production Web React renderer, and the Session and Workspace doubles behind one stabilization and teardown axis. Root declarations and automatic single-Slot frames use the real registry; renderer Host capture exposes production Store instances; feature mounts preflight required services and receive Fiber-bound registry faces so entries, child declarations, Conversation definitions, and services disappear with their owner.

The package is an ESM Rust/WASM test library rather than a `tsdown` bundle. The WASM links compiled Cordis and Web React; its generated entry supplies only React, Testing Library, Vitest, and Immer as boundary adapters, exports source-compatible class and helper names, delegates settings spies and snapshot serialization to Rust state and normalization, and keeps dependency bindings out of the public export surface. Generated `lib/` bytes remain ignored.

## Verification

The full pinned source runtime suite passes all 26 tests. Native Rust tests pin subscription ordering, disposal, failure propagation, settings publication and write records, translation conversion, Workspace stabilization, every action default and stub, browse cancellation, archive publication, class folding, UTF-16 fingerprinting, and target Session defaults. Fourteen live WASM tests exercise every reusable helper plus assembled root boot order, renderer updates, Session selection, automatic Slot views, caller-bound function, object, and class feature cleanup, Store identity and pruning, public constructors, JavaScript string conversion, Vitest-shaped settings spies, and idempotent teardown. The optimized ESM package builds through `cargo xtask wasm-package`; its curated exports exclude dependency APIs, and its package metadata resolves every required artifact. A dedicated built-package gate imports the generated entry under real Vitest, React, Testing Library, and jsdom, initializes the embedded WASM without a fetch, checks class identities and helpers, creates a live runtime, mounts and disposes a class plugin, adds a Session, and disposes the runtime. The same gate type-checks a generated consumer to prove that augmented `SlotMap` keys and owner props remain enforced by the public declarations.

## Alternatives considered

**Keep test-runtime behavior inline in each feature crate.** Rejected because shared defaults, disposal semantics, and failure policy would acquire multiple authorities, and a production face change could leave unrelated fakes silently inconsistent.

**Retain the TypeScript package because it is test-only.** Rejected because executable test infrastructure participates in parity and assembled Web validation; keeping it would preserve a second implementation language precisely where later source tests depend on it.

**Use production services for every test.** Rejected because focused feature tests need deterministic publication and failure injection, and the source Remote double deliberately propagates listener errors while production contains them. Tests that need generated namespaces or network behavior use the real service and built integration gates instead.

## Consequences

Feature-test ports gain one reusable Rust owner for their controlled environment and compile against the same portable contracts as production. The crate adds an explicit test-only API that must track source helper changes and production face changes together. All source runtime helpers, the assembled runtime suite, and the former `tsdown` build row now have direct Rust/WASM evidence; downstream feature suites can move to the compiled package without retaining executable TypeScript test infrastructure.
