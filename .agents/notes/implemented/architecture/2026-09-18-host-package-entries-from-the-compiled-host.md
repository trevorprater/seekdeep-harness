# Agent Note: Host package entries are generated from the compiled Host

Status: implemented

English | [中文](2026-09-18-host-package-entries-from-the-compiled-host.zh.md)

## Problem

Every `packages/<group>/<package>` manifest still declares the npm entry points the pinned source built from TypeScript: `main` and `exports["."]` name `lib/index.js`, `exports["./invariant"]` names `lib/invariant.js`, and a few packages add `lib/types/types.js`, `lib/worker.cjs`, `lib/startup.js`, `lib/bin.js`, or the Typert artifact `lib/typert.host.js`. The port compiles the 173 Host packages' runtime into the `seekdeep` binary and `cargo xtask remote-declarations` already writes their `lib/types/*.d.ts` from the captured declaration model, but nothing emitted the JavaScript files. `publint` therefore failed every Host manifest in CI, the seven gates behind it (built-package invariants, lint and duplication, the two snapshot lanes, the documentation typecheck, NodeNext types, and the built-bin smoke) never ran, and the NodeNext consumer would have failed next because the four `./typert` exports had no declaration either.

## Decision

`cargo xtask host-entries` writes, for every depth-two package that `build-client` does not compile, each JavaScript file its manifest declares under `main`, `bin`, or `exports`. The invariant companion keeps the Loader contract the source published: `name` is the canonical companion name taken from the captured `invariant.d.ts`, `inject` is `['invariants']`, and `apply` registers the package with the invariant registry. Its installer is the no-op the pinned source had wherever `crates/invariants/src/noop/catalog.rs` says so and otherwise throws, naming the compiled Host, because those checks run in Rust. Every other runtime entry throws as soon as it is loaded, naming the package, the entry, and the `seekdeep` executable, so a consumer never runs a silent stand-in for compiled behavior. `cargo xtask host-assets` runs the command after the declarations, so `pnpm run build` produces them, and `--check` verifies them. `remote-declarations` also writes `lib/typert.host.js` and `lib/typert.host.d.ts` from the same face model emitter that already produced the Remote pair, and the nine manifests whose `exports["."]` listed `default` before `types` now list `types` first, as publint requires.

## Alternatives considered

**Drop the entry fields from the Host manifests and narrow the gates to Client packages.** Rejected: `check-workspace-constraints`, `verify-package-invariants`, the built-package invariant probe, the release families, and the cookbook all enforce those fields, the manifests are verified configuration surfaces, and a published package without an entry point is not a compatibility binding.

**Loader-shaped stubs whose `apply` does nothing.** Rejected: a no-op `apply` for a plugin whose behavior lives in Rust would let a Node consumer believe the plugin mounted. The Client packages keep that shape only where the browser bundle carries the behavior.

**Generate the entries from each crate's Rust constants.** Rejected for now: the companion names are already captured verbatim in the declaration model, and reading them there keeps `xtask` free of a dependency on every runtime crate.

## Consequences

- `publint`, `verify-node-next-types`, and the release pack see complete Host packages after `pnpm run build`; the gates behind `publint` run again in CI.
- The built-package invariant probe still fails for every package: the Rust/WASM Loader package initializes through `fetch` of a `file:` URL, which Node rejects, and its class exposes no `unwrapExports`. That gate needs its own change to the Loader wrapper and the probe.
- Any new Host manifest export that names a JavaScript file is covered automatically; a manifest whose companion the declaration model does not know fails the build with the package named.
