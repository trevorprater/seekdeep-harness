# Agent Note: The Loader wrapper's default export is the plugin as a constructor

Status: implemented

English | [中文](2026-09-19-loader-wrapper-default-export-is-a-constructor.zh.md)

## Problem

The source's `@cordisjs/plugin-loader` exports its `Loader` class as the default export: the class is the plugin a Context applies, and `Loader.prototype.unwrapExports` normalizes ESM, CommonJS, and default-export module shapes to the plugin they carry. The built-package invariant gate depends on both facts from plain Node: it imports the Loader once, creates an object from the default export's prototype, and asks that object to unwrap every companion module. The port's generated `vendor/loader/lib/index.js` exported a plain plugin descriptor as its default and initialized its WebAssembly module through `fetch` of a `file:` URL, which Node rejects, so the gate failed for every one of the 32 non-compiled packages and the consumers lane skipped six dependent gates.

## Decision

The wrapper reads the module bytes through `process.getBuiltinModule('node:fs')` when that function exists and streams the URL otherwise, so browsers keep their path and Node needs no fetch. Its default export is a constructor, `LoaderPlugin`: constructing it under a Context applies the compiled plugin, exactly as the source's class did when a Context constructed it, and its prototype's `unwrapExports` delegates to the Rust rule the Loader already applies to every entry it starts, exported from `crates/client-loader` as `unwrapExports`. The constructor carries the descriptor's `name`, `inject`, and `apply` so a caller that reads the descriptor shape still finds it; the named `Loader` export stays the Rust-backed service class, and the declarations describe the constructor.

## Alternatives considered

**Give the descriptor object a `prototype` property.** Rejected: it satisfies the probe's `Object.create` call but not the contract, which is a class whose instances normalize exports; the gate's own fixture and the source script both assume a constructor.

**Export the Rust service class as the default with the descriptor's fields as statics.** Rejected: the compiled Cordis face constructs a constructor plugin, so a Context would construct the service directly and skip the plugin's `apply`, which registers the entry hook and provides the service.

**Read the bytes through a dynamic `import('node:fs')`.** Rejected: the wrapper is also bundled for the browser shell, and a literal `node:` specifier in a dynamic import is a bundler warning at best; `process.getBuiltinModule` is a synchronous property lookup that browsers simply lack.

## Consequences

- `verify-built-package-invariants` can reach the companion checks under Node 22 and later, where `process.getBuiltinModule` exists; the CI Node lines are 22.19, 24, and 26.
- A Context that applies the default export constructs `LoaderPlugin`, whose constructor runs the same `apply`; the runtime's plugin name stays `loader`.
- `wasm.unwrapExports` is a public binding of the Loader package; any JavaScript that needs the source's export normalization uses it rather than reimplementing the rule.
