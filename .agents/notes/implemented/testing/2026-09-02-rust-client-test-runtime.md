# Agent Note: Rust client test runtime

Status: implemented

English | [中文](2026-09-02-rust-client-test-runtime.zh.md)

## Problem

Client feature tests share behavior-bearing doubles for Cordis services, observable stores, locale selection, translation, Session and Workspace fixtures, stabilization, and DOM snapshots. Recreating those helpers beside each Rust/WASM feature test would make the same test contract drift across crates, while retaining the TypeScript helper package as executable infrastructure would violate the Rust-production boundary and keep later Web-test ports dependent on a second runtime.

## Decision

`seekdeep-client-test-runtime` is the target-neutral Rust owner of reusable Client test doubles. Each helper exposes the production Rust contract when one exists and adds only explicit test controls such as publication, call records, or an opaque event dispatcher. Browser-only environment controls compile behind `wasm32`; the crate does not enter the product plugin graph.

The Remote double publishes the `remote` Cordis service, delivers opaque forwarded arguments to a registration-order listener snapshot, makes subscription disposal idempotent, leaves unknown events inert, and rejects generated namespace mounting with the source diagnostic. Listener errors intentionally propagate so this double cannot be used as evidence for the production Remote service's containment policy.

The settings stub implements `ClientSettingsScope`, starts with the source loading/read-only/Host snapshot, records ordered `set` and `unset` calls, replaces only supplied publication fields, and synchronously notifies a listener snapshot. The translator uses first-dictionary ownership, visible-key fallback, ASCII word placeholders, and JavaScript-compatible parameter stringification. The WASM browser-language guard installs the same configurable own `navigator.languages` and `navigator.language` values and deletes both during idempotent cleanup so inherited accessors become visible again.

## Verification

Focused source tests pin the Remote double, settings stub, translator consumers, and browser-language consumers. Native Rust tests pin subscription ordering, disposal, failure propagation, settings publication and write records, and translation conversion; a live WASM test pins language preference order and inherited-accessor restoration. `cargo xtask parity` maps only helpers with direct source and target evidence; the remaining Session, Workspace, fixture, stabilizer, and snapshot helpers stay pending.

## Alternatives considered

**Keep test-runtime behavior inline in each feature crate.** Rejected because shared defaults, disposal semantics, and failure policy would acquire multiple authorities, and a production face change could leave unrelated fakes silently inconsistent.

**Retain the TypeScript package because it is test-only.** Rejected because executable test infrastructure participates in parity and assembled Web validation; keeping it would preserve a second implementation language precisely where later source tests depend on it.

**Use production services for every test.** Rejected because focused feature tests need deterministic publication and failure injection, and the source Remote double deliberately propagates listener errors while production contains them. Tests that need generated namespaces or network behavior use the real service and built integration gates instead.

## Consequences

Feature-test ports gain one reusable Rust owner for their controlled environment and compile against the same portable contracts as production. The crate adds an explicit test-only API that must track source helper changes and production face changes together. It does not make unported test-runtime helpers complete: their manifest rows remain pending until their Rust implementations and focused evidence land.
