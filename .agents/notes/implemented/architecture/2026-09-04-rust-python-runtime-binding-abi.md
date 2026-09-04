# Agent Note: Rust Python runtime binding ABI

Status: implemented

English | [中文](2026-09-04-rust-python-runtime-binding-abi.zh.md)

## Problem

The Python runtime package exposes importable lookup functions and interpreter-specific exceptions, while the port requires their decisions to execute in Rust. A replacement that merely returns equivalent JSON can lose caller object behavior, exception identity, or the distinction between an explicit executable and the development carrier. A CPython-specific extension would also introduce an interpreter ABI dimension into the existing platform-only runtime wheel contract.

## Decision

The [safe SDK crate](../../../../crates/python-sdk/src/lib.rs) owns runtime selection, artifact lookup, and synchronous SDK policies. The [native binding crate](../../../../crates/python-sdk-ffi/src/lib.rs) exposes the version 3 C ABI using borrowed request bytes, opaque callback context IDs, and allocator-owned response handles. It does not link Python ABI symbols. Unsafe code is restricted to this native crate because C callers cannot express Rust byte borrows or callable lifetimes; the remaining SDK code retains the workspace's unsafe-code prohibition.

Response handles increase monotonically and are never reused. Borrowed response bytes remain valid until their handle is freed or consumed by a callback return. Freeing an unknown or stale handle is inert, and a later allocation cannot inherit an earlier handle. Buffer destruction occurs outside the registry lock. Native panics become binding failures rather than unwinding through C.

The [Rust generator](../../../../crates/python-sdk/src/bindings.rs) emits the Python runtime and SDK declarations plus generic `ctypes` marshalling. Runtime-mode objects remain opaque while Rust chooses which interpreter equality or representation operation to request. Accepted modes therefore do not call `repr`, and a failed comparison or representation returns the original Python exception object. Nested runtime lookups retain the existing replaceable `bundled_package_dir` and `_current_platform_tag` functions. Short-lived lookup tables end after response decoding; client contexts remain pinned while native reader threads use them, and retained entries release after their final native owner.

The [client dispatcher](../../../../crates/python-sdk-ffi/src/client.rs) exposes Rust-owned harness, client, subscription, and process handles. Every matching subscriber receives the same mutable Python notification object. A collected root event retains its own object reference, so replacing the notification's current event does not retarget the collected event. Rust reads current fields at the source's decision points, including after synchronous user callbacks and when constructing the final result. Mutable configuration is read at the source's operation boundaries. Falsey explicit high-level configurations are selected through interpreter truth testing exactly once, while Rust still owns the selection. Foreign exceptions carry both their local identity and owning interpreter context; a harness call that invokes its client's callback therefore rethrows the original exception or cause rather than looking in the harness's object table.

Public response projection traverses caller-owned Python objects through primitive callbacks while Rust owns the filtering and selection algorithm. This retains string subclasses, arbitrary-size integers, non-finite floats, lone surrogates, custom truth and string operations, and the exact exception those operations raise. The workspace JSON representation preserves arbitrary-precision number spelling across the low-level SDK wire; ABI decoding handles buffered floating-point fields explicitly so this broader number domain does not change timeout configuration.

The [executable builder](../../../../crates/python-release/src/executable/pipeline.rs) compiles the binding library with each native target. Its architecture-qualified library filename prevents products for different targets from colliding. It also generates the host checkout's runtime and SDK declarations plus the Hatch binding. Wheel staging selects exactly one matching library and regenerates declarations for that target; the hook rejects missing or mixed native binding payloads. The published platform tags and import namespace remain unchanged. Generated Python files and native libraries are build outputs, not an alternative implementation source.

The Hatch binding forwards policy to the Rust release tool. Release staging supplies an explicit tool path; an editable checkout can invoke the same tool through its repository Cargo manifest. At module import it asks Rust to validate and serialize the platform manifest, then supplies that immutable snapshot to later hook initialization, preserving the source's import-time boundary even if the file changes. It does not duplicate platform or payload policy in Python.

## Verification

[Binding tests](../../../../crates/python-sdk-ffi/tests/runtime_binding.rs) load the real shared library from Python, exercise nested and concurrent calls, preserve callback exception identity, and check unknown modes including a lone-surrogate string. The [runtime comparison](../../../../crates/python-sdk-ffi/examples/runtime_source_parity.rs) and [client comparison](../../../../crates/python-sdk-ffi/examples/client_source_parity.rs) run the pinned Python tests against generated bindings with product-identity substitutions. The client comparison also checks public declarations and a source-differential value/path/transport matrix: falsey config selection, symlinked and relative paths, mutable notification identity, late event replacement, live configuration, import-versus-resolver failures, custom projection callbacks, non-finite and arbitrary-precision values, bidirectional wide integers, callback cleanup, and cross-context exception causes. [Native observation tests](../../../../crates/python-sdk/tests/observation_parity.rs) pin reference lifetime and mutation ordering. Native ABI tests verify buffer ownership, direct operation decoding, and stale-handle refusal. [Release tests](../../../../crates/python-release/tests/release_parity.rs) cover target-specific library staging and wheel payload checks.

The [installed-wheel comparison](../../../../crates/python-sdk-ffi/examples/installed_source_smoke.rs) rejects imports outside an isolated environment and drives the pinned source's keyless model server through default SDK launch, custom text/code/workflow turns, the minimal two-tool configuration, the complete advanced executable snapshot, and direct runtime launch. Freshly paired ABI-v3 macOS arm64 SDK/runtime wheels pass all five scenarios and durable-log checks on Python 3.10 and 3.14. The advanced comparison includes exact cross-session notification order, including the eager workflow child's request context before its parent `tool-workflow/agent-start` record. The [bundled-runtime comparison](../../../../crates/python-sdk-ffi/examples/bundled_runtime_source_parity.rs) runs all ten pinned carrier cases against both the native executable and the development Node launcher, including unset or empty default configuration and startup failure for an absent plugin.

These checks cover every tracked Python SDK and runtime source surface. They do not establish whole-product parity, public release readiness, or the still-separate Linux release matrix.

## Alternatives considered

**Keep runtime lookup logic in Python.** That would create a second implementation of mode precedence, platform aliases, and missing-artifact behavior, contrary to Rust ownership.

**Ship a CPython extension as the SDK wheel.** That would change the platform-independent SDK distribution and introduce Python ABI-specific build and installation requirements. A C ABI library remains part of the existing native runtime distribution.

**Serialize every callback argument first.** Interpreter equality and representation can execute caller code and raise exact exception objects. Copying notification payloads also loses mutations shared by subscribers and retargets captured events after field replacement. Opaque references preserve those operations, identities, and ordering.

**Return allocator addresses as reusable ownership identities.** An allocator can reuse a freed address, allowing a stale release to affect a later response. Non-reused handles separate ownership from storage addresses.

## Consequences

Python's public runtime and client APIs delegate selection, filesystem, lifecycle, and run-collection decisions to compiled Rust without acquiring or downloading a runtime. The binding requires the matching native library and ABI, while the standalone runtime executable still needs no Python installation. The [recursive notification](../bug-fix/2026-07-24-recursive-python-sdk-session-notifications.md) and [owned-run](2026-07-30-followup-enqueue-and-owned-runs.md) policies continue to define which observations belong to a result.

The [packaged-runtime assembly](2026-09-04-rust-packaged-sdk-runtime-assembly.md), [single-executable distribution](2026-07-10-single-file-executable-sdk-runtime-distribution.md), and [publication workflow](../process/2026-08-11-python-publication-workflow.md) notes remain active. This ABI does not change plugin reload rules, configuration ownership, publication authorization, or the evidence required for a complete SDK release.
