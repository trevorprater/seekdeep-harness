# Agent Note: Rust packaged SDK runtime assembly

Status: implemented

English | [中文](2026-09-04-rust-packaged-sdk-runtime-assembly.zh.md)

## Problem

A native JSON-RPC entry can compile while exposing only a fraction of the plugins declared by the Python runtime manifest. An executable copied away from its build directory can also lose a separately discovered workflow helper or import packages from an unrelated surrounding project. Those failures break external configuration and single-file delivery even when each underlying Rust plugin passes its own tests.

## Decision

The [compiled runtime catalog](../../../../crates/jsonrpc-demo/src/runtime_catalog.rs) pairs concrete Rust plugin factories with their existing package names and filters registration through the embedded [runtime manifest](../../../../python/sdk-runtime/package.json). Registration exposes factories; only the external Cordis configuration mounts services. The packaged catalog excludes the development replay adapter and disables filesystem fallback for unregistered bare plugin names. Explicit relative, absolute, and file-URL compatibility modules keep the Loader's existing resolution behavior.

This is the native realization of the closed-plugin and external-configuration requirements in the [single-executable distribution note](2026-07-10-single-file-executable-sdk-runtime-distribution.md), under the Rust production and [native/process placement rules](../../../../porting/DYNAMIC_PLUGIN_RELOAD.md). It does not replace the source's distinct configuration, Host, browser, or model-defined package reload policies.

The [JSON-RPC launcher](../../../../crates/jsonrpc-demo/src/runner.rs) passes its absolute executable path to the workflow plugin. Each workflow starts a scrubbed, killable child process running the same binary with `SEEKDEEP_INTERNAL_WORKFLOW_WORKER=1`; that role handles the existing worker protocol before application boot. A relocated or renamed executable therefore needs neither a sibling workflow helper nor a PATH lookup. Code execution uses the existing Rust-owned V8 backend. The serving plugin still waits for complete Loader startup before dispatching SDK requests.

Plugin dependencies retain source meaning. The skill tool requires `agents`, `tools`, and `skills`. Approval itself has no system-prompt prerequisite: its policy contribution follows the exact optional prompt provider through late arrival, replacement, and withdrawal without replacing the approval service. Reentrant service notifications are coalesced, and prompt registration and disposal run outside the binding lock. Policy decisions, fail-closed answer handling, and durable audit events remain owned by the [approval service](../feature/2026-07-06-approval-seam.md).

## Verification

[Catalog tests](../../../../crates/jsonrpc-demo/tests/runtime_catalog_parity.rs) exercise ambient-package refusal, explicit file loading, SQLite persistence across teardown and reopening, and the worker protocol through a relocated executable with an empty environment. The [source comparison](../../../../crates/jsonrpc-demo/examples/catalog_source_parity.rs) loads the pinned modules through their source path aliases and checks concrete factory availability and required-service declarations. The [source-model smoke](../../../../crates/jsonrpc-demo/examples/packaged_source_smoke.rs) drives text, code-execution, and zero-agent workflow turns through the relocated SDK process and checks its durable log. [Approval tests](../../../../crates/user-approval/tests/approval_policy_parity.rs) pin independent service availability, optional-provider lifetimes, and reentrant notification handling.

These checks do not establish complete Python distribution parity. Abstract service constructor compatibility, the native executable packaging command, Python bindings, the development carrier, the complete installed-SDK smoke, and the release platform matrix remain separate gaps.

## Alternatives considered

**Register the entire CLI catalog.** That would expose packages outside the runtime manifest and pull browser-only application concerns into the SDK distribution. The SDK catalog selects only its declared concrete implementations.

**Resolve unknown bare names from nearby package directories.** An unrelated installation could expand the shipped plugin set. Explicit file inputs remain available without granting ambient bare-package resolution.

**Discover the workflow worker by executable name or a sibling file.** Both depend on deployment layout beyond the running artifact. Passing the launcher's exact path preserves relocation and lets the existing process owner terminate uncooperative work.

**Make prompt presentation a prerequisite for approval.** A headless composition still needs deterministic policy and fail-closed decisions. Optional presentation must not make that service unavailable.

## Consequences

The packaged native process can compose the declared concrete plugins and run its workflow child role outside the repository. Adding a concrete built-in requires both a compiled factory and manifest membership; their source comparison makes missing registrations visible. Native linkage replaces the source VFS implementation for these factories, while package names, configuration ownership, worker protocols, and durable outcomes remain compatibility requirements.

The distribution and startup notes remain active: their Python carrier, publication, platform, and readiness requirements are only partially covered by this native assembly decision. No release authorization or publication setting changes here.
