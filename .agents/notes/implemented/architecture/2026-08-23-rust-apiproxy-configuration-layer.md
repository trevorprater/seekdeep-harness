# Agent Note: Rust ApiProxy configuration-domain layering

Status: implemented

English | [中文](2026-08-23-rust-apiproxy-configuration-layer.zh.md)

## Problem

The Rust ApiProxy had complete configuration wire schemas and complete settings, credentials, and LLM services, but no production runtime owned the corresponding RPC methods. `ApiProxyService` handled Host and Workspace methods and delegated every other method to an abstract runtime. Wire validation therefore proved only that requests could be decoded; it did not provide the [web configuration plane](2026-07-30-web-config-plane.md). A gateway that captured service `Arc`s at construction would also retain a disposed Cordis generation after reload, while a Host event stream that registered listeners without drop-owned cleanup would leak registrations when a consumer discarded the stream before polling it.

## Decision

- `ConfigurationApiProxyRuntime` is a decorator that owns the eleven `settings.*`, `credentials.*`, and `llm.*` unary methods and delegates every other ApiProxy operation unchanged. `ApiProxyService::from_context` composes this decorator beneath the Host layer, so the canonical Context constructor installs the configuration domain rather than requiring callers to discover it.
- Every operation resolves its current Cordis service from `Context`. LLM is a required composition dependency; settings and credentials are optional services whose methods return actionable business errors when absent. An in-flight operation retains the generation it resolved at dispatch, while the next operation observes a replacement generation.
- The gateway preserves the configuration-plane rules at the RPC boundary: explicit namespace exposure, redacted descriptors, pathless provider-owned document opening, revision CAS, value-free credential reads, write-only credential values, provider-local model-catalog failure containment, and discovery failures that never echo a supplied credential. Text-document opening has its own injected boundary and does not reuse the generic path opener.
- Each Host stream registers the three configuration-owned forwarded events (`settings/document-updated`, `credentials/updated`, and `llm/adapters-updated`) and converts their typed Cordis arguments to the `host/remote-event` JSON shape. The stream owns an idempotent listener guard outside its polling future, so cancellation, ordinary drop, and drop-before-first-poll all release the registrations. This is the Rust realization of the [Remote event delivery decision](2026-08-10-remote-event-delivery.md), not a process-global broadcast hub.

## Alternatives considered

**Put every domain into `ApiProxyService`.** This would recreate the source file's monolith and make Session, interaction, configuration, and Host lifecycle state one ownership unit. Decorators keep method ownership explicit while preserving one physical carrier and one exhaustive method registry.

**Capture settings, credentials, and LLM `Arc`s in the decorator.** This is simpler per call but contradicts the repository's generation-based reload boundary: a replacement service could be live while the gateway continued addressing the disposed generation. Per-operation Context resolution preserves exact-generation behavior without unloading native Rust code.

**Use one process-global event fanout.** A hub would reduce listener registrations but create a second lifetime and replay policy outside the Host stream. Per-stream listeners make subscription ownership and teardown coincide with the carrier that consumes them.

**Expose or accept the settings document path.** Returning the provider path or accepting a browser-supplied path would transfer Host filesystem authority across the RPC boundary. The pathless action keeps the provider and Host opener authoritative.

## Verification

The pinned source `api-proxy-config.spec.ts` suite passes all 30 cases. Sixteen consolidated Rust runtime tests cover optional-service failures, exposure and redaction, document preparation and cancellation, write semantics and CAS, read-only and withdrawn namespaces, credential shadowing, provider topology, catalog containment, discovery redaction, event forwarding, and listener teardown. The complete `seekdeep-host-apiproxy` and `seekdeep-settings` suites and strict Clippy pass, and the Context-constructor test proves `llm.providers` reaches the configuration decorator rather than the delegated runtime.

## Consequences

ApiProxy composition is ordered: each decorator must claim a disjoint method family and delegate the rest without rewriting correlation, cancellation, streams, responses, or downloads. Configuration calls pay one Context lookup per operation, and each open Host stream owns three Cordis listeners; those costs buy generation correctness and deterministic teardown. The configuration layer forwards only the events it can serialize from their owner types, so another layer must not forward the same names a second time. The browser never gains a Host path or credential value through this implementation.
