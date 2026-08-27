# Agent Note: Semantic event gates and generated routing contracts

Status: implemented

English | [中文](2026-07-14-typescript-program-backed-semantic-gates.zh.md)

## Problem

Repository gates sometimes need facts that TypeScript syntax does not carry by itself: whether a receiver is a Cordis `Context`, which concrete event names reach a forwarding helper, and whether declaration merging changed an event signature.

The existing gates use TypeScript's single-file syntax model and maintain these facts through naming conventions, handwritten tables, and JSDoc.

The repository needs one semantic source of truth without introducing runtime package cycles, broad fallback heuristics, or machine-readable annotations that restate information already available to TypeScript.

## Decision

The documentation graph gate combines project-wide type information through `ts.Program` and uses `TypeChecker` to extract **strongly typed** facts, reducing reliance on naming conventions, handwritten tables, and JSDoc metadata. The Rust scoped-event runtime carries its routing subject as a typed token and generates a closed requirement catalog from the pinned oracle.

These two mechanisms preserve semantic ownership on their respective execution planes.

### One project model expands the root solution

[`TypeScriptProject`](../../../../scripts/ts-project.ts) parses the root `tsconfig.json`, recursively expands every project reference, and combines the referenced source roots into one no-emit semantic program. A normal program created from the solution config can redirect referenced projects to built declarations; explicit expansion keeps the package `src` files available for AST traversal and symbol identity.

The wrapper owns config diagnostics, semantic compiler options, repository-relative paths, source lookup, and the shared checker. Individual gates do not glob package sources or construct partial programs independently.

### A. Event relations follow receiver and value types

[`gen-doc-graphs`](../../../../scripts/gen-doc-graphs.ts) classifies calls by assignability to the repository's actual `Context`, `AgentEventDispatch`, and Cordis `EventsService` types. Variable names and property spellings do not determine whether a call is an event operation.

Context and agent-dispatch calls contribute only finite string-literal event sets. Direct `EventsService.dispatch()` calls recover the event slot through array literals, constant aliases, conditional branches, and resolved call sites of non-exported local helpers. Generic forwarding parameters are not concrete producers: attribution stays with the call sites that supply a closed event value.

Semantic queries run only where a branch can consume them: calls are prefiltered by the closed event-API method-name set before receiver classification, and helper call sites are indexed on demand instead of eagerly resolving every call in every package source. The demand-driven index proves locality per helper — a helper that is non-exported, sits in a real ES module, and whose every same-file reference is a direct callee has all of its calls in that file by module scoping, so only that file is indexed. Any unproven premise (an export modifier, a global script file, an aliasing or otherwise unclassifiable reference) falls back to the original full package-source index, which is the unchanged original semantics; the proof affects cost, never results. A lazy single global index was rejected because the helper-parameter path is reached on the current tree, so it would still pay nearly the whole `getResolvedSignature` sweep.

Every declared harness event must have a discovered producer. A missing producer fails generation as dead vocabulary or an unsupported semantic dispatch shape; listener-free extension points remain valid. `internal/dispatch` instrumentation is not treated as a subscription to every event it observes, so the matrix contains direct product listeners rather than manually asserted indirect relationships.

### B. Scoped-event routing uses generated Rust subject requirements

Rust `EventArgs` embeds the optional scope subject when a scoped payload is constructed. The invariant can therefore compare one typed subject token with the carrier key without generating source-specific parameter indexes or property paths. Events whose public payload intentionally omits that key require carrier presence only.

The Rust [`gen-scoped-events`](../../../../crates/repository-tools/src/scoped_events_generator.rs) generator preserves the pinned source oracle as twenty `Subject` and six `Presence` events. It writes the runtime [`scoped_events`](../../../../crates/scope/src/scoped_events.rs) module beside the dispatch contract and fails freshness checks when the committed source differs.

The `seekdeep-scope/invariant` companion consumes this generated map. Neither `seekdeep-scope` nor `seekdeep-invariants` acquires dependencies on every event owner, and the target no longer needs a flattened TypeScript Program to recover routing subjects already carried by `EventArgs`.

### Semantic gaps fail explicitly

The TypeScript documentation graph generator rejects missing declarations, config diagnostics, widened or generic event names, and unresolved dataflow. The Rust scoped-event generator rejects stale output, while runtime construction and invariant tests pin subject-token and carrier behavior directly.

## Verification

`verify-doc-graphs` freshness-checks semantic producer/listener discovery. `verify-scoped-events` byte-checks the generated Rust catalog; generator and runtime suites pin the complete twenty/six partition and unscoped fallback. Workspace and runtime-closure checks keep event-owner aggregation out of deployment dependencies.

## Alternatives considered

- **Keep syntax-only scans with receiver allowlists and manual overrides.** This is simple per exception but makes renames and new helper shapes update a second representation. Completeness can detect a missing producer, but it cannot prove that the override still describes the source.

## Consequences

- Event relation generation follows semantic receiver identity and closed event values instead of local naming conventions.
- Scoped-event membership and runtime invariant coverage come from the generated pinned oracle; subject extraction is fused into Rust `EventArgs` construction.
- Refactors that change the supported scoped-event vocabulary update the generator input and runtime tests together.
- Building a flattened Program costs more startup time and memory than parsing isolated files, and semantic gates depend on a valid root project graph.
- Generated Rust remains committed source: scoped-event catalog changes must regenerate it and update affected documentation.
