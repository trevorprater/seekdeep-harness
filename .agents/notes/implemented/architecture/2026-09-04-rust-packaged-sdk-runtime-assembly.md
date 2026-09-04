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

The [native executable builder](../../../../crates/python-release/src/executable/mod.rs) retains the source target and flag grammar, compiles each requested platform serially, and stages executable products and the development carrier. The legacy `node<major>` target segment remains accepted and reported; the pinned Rust toolchain, not that segment, selects the implementation. `--skip-build` consumes existing Cargo release artifacts. Before staging the host executable, the builder checks its native format, architecture, executable mode, repository version, and embedded runtime manifest. Output parents must remain inside the repository without symlink traversal; replacement is limited to the generated Node carrier directory.

The development carrier preserves the Node entry path but contains only a launch binding and the host native artifacts. The binding replaces Node with the Rust process using `process.execve`, retaining arguments, environment, and standard streams. The native launcher clears `O_NONBLOCK` on inherited standard streams before reading the SDK protocol; a file-loaded Node entry can otherwise hand off nonblocking descriptors to Rust's blocking I/O. The [Rust PTY helper](../../../../crates/pty-spawn-helper/src/main.rs) opens the inherited terminal, applies the requested working directory, and replaces itself with the requested program. Its macOS sidecar preserves the source node-pty caller's controlling-terminal contract without shipping a C implementation.

The GitHub and GitLab runtime build jobs compile Linux executables inside architecture-specific, digest-pinned manylinux 2.28 containers and use the same images for installed-wheel validation. Container compilation has a separate Cargo target directory from host wheel tooling. macOS compilation defaults to deployment target 13.5, and the existing deployment-target gate checks both native artifacts against the published platform tag.

## Verification

[Catalog tests](../../../../crates/jsonrpc-demo/tests/runtime_catalog_parity.rs) exercise ambient-package refusal, explicit file loading, SQLite persistence across teardown and reopening, and the worker protocol through a relocated executable with an empty environment. The [source comparison](../../../../crates/jsonrpc-demo/examples/catalog_source_parity.rs) loads the pinned modules through their source path aliases and checks concrete factory availability and required-service declarations. The [source-model smoke](../../../../crates/jsonrpc-demo/examples/packaged_source_smoke.rs) drives text, code-execution, and zero-agent workflow turns through the relocated SDK process and checks its durable log. [Approval tests](../../../../crates/user-approval/tests/approval_policy_parity.rs) pin independent service availability, optional-provider lifetimes, and reentrant notification handling.

The [builder comparison](../../../../crates/python-release/examples/executable_source_parity.rs) checks 60 target, host, and flag cases against the pinned source parser. [Artifact tests](../../../../crates/python-release/tests/executable_parity.rs) cover dry-run isolation and malformed native files. The [source PTY consumer](../../../../crates/pty-spawn-helper/examples/source_pty_parity.rs) verifies the compiled helper through the pinned node-pty implementation. A real macOS arm64 release build passes the three-turn source-model smoke through both the relocated executable and the generated Node carrier, including their durable logs. A delayed SDK request pins standard-stream handoff behavior.

The [Python runtime binding ABI](2026-09-04-rust-python-runtime-binding-abi.md) supplies Rust-backed carrier lookup and target-specific native libraries. These checks do not establish complete Python distribution parity: abstract service constructor compatibility, Python client-class bindings, the complete installed-SDK smoke, and execution of the release platform matrix remain separate gaps. Static workflow checks do not substitute for native Linux builds or installed Python imports.

## Alternatives considered

**Register the entire CLI catalog.** That would expose packages outside the runtime manifest and pull browser-only application concerns into the SDK distribution. The SDK catalog selects only its declared concrete implementations.

**Resolve unknown bare names from nearby package directories.** An unrelated installation could expand the shipped plugin set. Explicit file inputs remain available without granting ambient bare-package resolution.

**Discover the workflow worker by executable name or a sibling file.** Both depend on deployment layout beyond the running artifact. Passing the launcher's exact path preserves relocation and lets the existing process owner terminate uncooperative work.

**Make prompt presentation a prerequisite for approval.** A headless composition still needs deterministic policy and fail-closed decisions. Optional presentation must not make that service unavailable.

**Keep Node as a supervising development process.** That adds another process owner and changes signal and exit propagation. A launch-only replacement keeps runtime behavior and lifecycle in Rust.

**Compile Linux executables on the runner and only test them in manylinux.** That can introduce newer libc requirements before validation begins. Compilation and installed-wheel checks share the pinned baseline image.

## Consequences

The packaged native process can compose the declared concrete plugins and run its workflow child role outside the repository. Adding a concrete built-in requires both a compiled factory and manifest membership; their source comparison makes missing registrations visible. Native linkage replaces the source VFS implementation for these factories, while package names, configuration ownership, worker protocols, and durable outcomes remain compatibility requirements.

The distribution and startup notes remain active: their Python carrier, publication, platform, and readiness requirements are only partially covered by this native assembly decision. No release authorization or publication setting changes here.
