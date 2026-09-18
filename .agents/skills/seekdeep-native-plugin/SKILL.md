---
name: seekdeep-native-plugin
description: Use when adding, porting, or extending a SeekDeep Harness plugin as a Rust crate under crates/ — a model-facing tool, a service provider or consumer, an LLM adapter, a persistence, sandbox, or OS integration, or any built-in the seekdeep binary ships — and whenever new harness behavior needs a home and the choice between a native Rust crate and a TypeScript file plugin is still open; covers the crate skeleton, Plugin::new with config validation, typed services and reversible effects, catalog and bundle registration, composition and oracle tests, README pairs, generated catalogs, the parity manifest, and the Rust gates.
---

# SeekDeep Native Plugin

Guide an agent through adding or changing a plugin crate so the result matches the crates the port already ships. This is guidance, not a script: the contracts live in the documents below, and this skill only orders the work and names the evidence each step needs.

## Read the contracts

- [crates/AGENTS.md](../../../crates/AGENTS.md) — the crate shape, wiring, tests, and gates.
- [packages/AGENTS.md](../../../packages/AGENTS.md) — the behavioral rules every plugin obeys regardless of language.
- Root [AGENTS.md](../../../AGENTS.md) — parity, naming, and Rust conventions.
- [Where new behavior goes](../../../docs/architecture.md#where-new-behavior-goes) — which service or event a capability attaches to.
- [Runtime placement](../../../porting/DYNAMIC_PLUGIN_RELOAD.md#runtime-placement) — what stays native and what stays JavaScript.

## Decide the runtime first

Write Rust when the plugin is part of the product: anything the `seekdeep` binary ships, anything on the hot path or touching filesystem, subprocess, sandbox, persistence, or OS integration, anything with a typed cross-plugin contract, and anything the parity, determinism, or invariant machinery must verify. A native crate is compiled into the binary and registered at boot; there is no dynamic native loading.

Write a TypeScript file plugin when the plugin arrives at runtime: a path in `cordis.yml` that must load without a rebuild, hot reload on save, or an out-of-tree extension. Model-authored `cordis_define` packages are JavaScript by contract. Keep those surfaces intact; they are preserved source behavior, not legacy ([your first plugin](../../../docs/user/develop/basic/index.md)).

During the port, a capability absent from the pinned source is post-parity work; port and preserve first ([POST_PARITY](../../../porting/POST_PARITY.md)).

## Workflow

1. Name the seam. Pick the service or event the capability attaches to from the architecture table; a new capability needs its Service Definition, provider, and consumer roles, and its name follows the [naming rules](../../../docs/cookbook/adding-a-package.md#name-the-role-that-exists).
2. Start from a shipped crate of the same role rather than from scratch: `crates/tool-todo` for a tool, `crates/cordis-timer` for a service provider, `crates/cordis/examples/hello_cordis.rs` for the lifecycle in isolation. Copy the shape, then replace the behavior.
3. Implement `NAME`, `INJECT`, `plugin()`, the config `Schema` and its validator, and an `apply` whose every registration is an effect the fiber unwinds. Prove disposal in a test that disposes the fiber and observes removal.
4. Wire it: the boot roster in `apps/seekdeep/src/profile_boot.rs` under both names, the bundle row with its config, and the package manifest and README pair beside the crate README.
5. Test it: `tests/loader_composition.rs` through the real Loader, unit tests for the behavior, oracle tests only for a ported surface, and snapshot tests for model-visible text.
6. Regenerate and record: the catalogs the change touches, the `porting/parity.json` entry for a ported surface, and an Agent Note for any decision a maintainer may revisit.
7. Verify with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test -p seekdeep-<name> --all-features`, the catalog `--check` runs, and `verify-cordis-config`; then select the rest through [seekdeep-pre-push-checks](../seekdeep-pre-push-checks/SKILL.md).

## Traps

- A `packages/<group>/<pkg>/src/index.ts` is not where runtime lands; the TypeScript cookbook describes the manifest and README half of a plugin.
- `context.get` on an injected service inside the body is guaranteed to succeed; `get_relaxed` exists for observers that must see a provider while it is still loading.
- Config that names an unavailable provider or resource fails at the earliest resolvable point, in the validator or the first line of `apply`, never later.
- Crates in the determinism perimeter never call `Instant::now()` or `SystemTime::now()`; take a clock through a seam.
- The oracle tests need the checkout named in `SOURCE_SNAPSHOT`; a container without it fails those binaries, which says nothing about your change.

## Report

State which seam the plugin attaches to, the roster and bundle rows added, the composition and unit tests, the catalogs regenerated, and every gate run with its result. Name any deviation from the pinned source and where it is recorded.
