# Agent Note: The native Typert analyzer hosts the TypeScript compiler as a library

Status: implemented

English | [中文](2026-09-06-native-typert-analyzer-hosts-the-compiler.zh.md)

## Problem

The Typert generator's model, renderer, emitter, and catalog projection were Rust, but the workspace analyzer that produces the model still ran as the pinned source's TypeScript program under Node. Every generated Remote descriptor therefore started from a model captured outside the Rust toolchain, and the source analyzer specs (`type-model`, `remote-model`, `tsdown-plugin`, `tools-catalog`, `cordis-catalog`) had no Rust implementation to run against.

The analyzer cannot be reimplemented without a type checker: Remote codec projection resolves declaration-merged mapped and conditional types through `checker.getTypeFromTypeNode`, cross-face links depend on module resolution, and write mode prints inferred annotations with `typeToTypeNode`. A second type checker would never agree with TypeScript on the source workspace.

## Decision

The analyzer is Rust code that hosts the pinned `typescript.js` library inside a Rust-owned V8 isolate (`crates/typert-generator/src/analyzer/native/`). Rust owns every decision the source analyzer makes — registrations, discovery, diagnostics gating, export records, service and event collection, declaration modeling, Remote invocation contracts, codec projection, write-mode edits, batching and merging — and the compiler library supplies parsing, binding, checking, printing, and module resolution through typed value helpers.

The compiler sees the filesystem only through a Rust `System` installed with `ts.setSys` (`system.rs`): BOM-aware reads, sorted directory listings, canonical realpaths, and case-sensitivity detection mirror the Node system, so `createCompilerHost`, `resolveModuleName`, and `parseJsonConfigFileContent` behave as they do under Node. Compiler hosts, parsed configs, registration inventories, and default-library parses live in `WorkspaceCaches` and are reused across batched programs exactly as the source memoizes them.

The library is resolved like the source package dependency: `node_modules/typescript/lib/typescript.js` above the analyzed workspace, or `SEEKDEEP_TYPESCRIPT_LIBRARY` for an explicit pin. Renamed identities are accepted alongside the pinned spellings (`@seekdeep-ai/cordis` and `@deepseek-ai/cordis`, the `seekdeep` and `dsh` manifest keys) so the oracle workspace analyzes unchanged.

`seekdeep-typert-generator`, a JSON request runner, is the only entry point compatibility bindings use. `cargo xtask typert-corpus` copies the pinned generator specs beside adapters whose every call is a runner request, so the source Vitest corpus and its committed snapshots are the executable specification. `cargo xtask remote-contracts --source` analyzes the Remote packages with the Rust analyzer and requires the face model to equal the source analyzer's before emitting descriptors.

## Alternatives considered

**Keep capturing the model from the source analyzer under Node.** Rejected: the generated descriptors then depend on a TypeScript program the port does not own, and the analyzer specs stay unported.

**Port the analyzer onto a Rust TypeScript front end.** Rejected: no Rust checker reproduces TypeScript's type evaluation, declaration merging, and module resolution; codec projections would drift from what the source workspace's own compiler reports.

**Run `analyzer.ts` itself inside the embedded engine.** Rejected: that keeps the behavior in JavaScript and only changes the process boundary, contrary to the port's Rust-ownership requirement.

## Consequences

- Native tests cannot analyze a workspace without a TypeScript library on disk; the corpus and contract gates pin the source checkout's library.
- JavaScript string semantics the model depends on (`\s`, `trim`, UTF-16 offsets, number and bigint formatting) are implemented explicitly in `jstext.rs` rather than approximated with Rust defaults.
- Each runner request loads the compiler afresh; batched and catalog analyses share one process and one memo, matching the source's single-process caches.
- Model conversion recurses through Rust frames between compiler calls, so analysis runs on a dedicated 512 MiB thread (`run_with_stack`) and the engine's stack guard is sized to it at first initialization; real workspaces overflow the default guard.
