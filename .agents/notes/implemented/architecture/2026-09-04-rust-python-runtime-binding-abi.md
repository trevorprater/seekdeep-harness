# Agent Note: Rust Python runtime binding ABI

Status: implemented

English | [中文](2026-09-04-rust-python-runtime-binding-abi.zh.md)

## Problem

The Python runtime package exposes importable lookup functions and interpreter-specific exceptions, while the port requires their decisions to execute in Rust. A replacement that merely returns equivalent JSON can lose caller object behavior, exception identity, or the distinction between an explicit executable and the development carrier. A CPython-specific extension would also introduce an interpreter ABI dimension into the existing platform-only runtime wheel contract.

## Decision

The [safe SDK crate](../../../../crates/python-sdk/src/lib.rs) owns runtime selection, artifact lookup, and synchronous SDK policies. The [native binding crate](../../../../crates/python-sdk-ffi/src/lib.rs) exposes a versioned C ABI using borrowed request bytes, opaque callback context IDs, and allocator-owned response handles. It does not link Python ABI symbols. Unsafe code is restricted to this native crate because C callers cannot express Rust byte borrows or callable lifetimes; the remaining SDK code retains the workspace's unsafe-code prohibition.

Response handles increase monotonically and are never reused. Borrowed response bytes remain valid until their handle is freed or consumed by a callback return. Freeing an unknown or stale handle is inert, and a later allocation cannot inherit an earlier handle. Buffer destruction occurs outside the registry lock. Native panics become binding failures rather than unwinding through C.

The [Rust generator](../../../../crates/python-sdk/src/bindings.rs) emits the Python runtime declarations and generic `ctypes` marshalling. Runtime-mode objects remain opaque while Rust chooses which interpreter equality or representation operation to request. Accepted modes therefore do not call `repr`, and a failed comparison or representation returns the original Python exception object. Nested runtime lookups retain the existing replaceable `bundled_package_dir` and `_current_platform_tag` functions. Invocation-owned object tables are released after the response is decoded.

The [executable builder](../../../../crates/python-release/src/executable/pipeline.rs) compiles the binding library with each native target. Its architecture-qualified library filename prevents products for different targets from colliding. It also generates the host checkout's runtime declarations and Hatch binding. Wheel staging selects exactly one matching library and regenerates declarations for that target; the hook rejects missing or mixed native binding payloads. The published platform tags and import namespace remain unchanged. Generated Python files and native libraries are build outputs, not an alternative implementation source.

The Hatch binding forwards policy to the Rust release tool. Release staging supplies an explicit tool path; an editable checkout can invoke the same tool through its repository Cargo manifest. It does not duplicate platform or payload policy in Python.

## Verification

[Binding tests](../../../../crates/python-sdk-ffi/tests/runtime_binding.rs) load the real shared library from Python, exercise nested and concurrent calls, preserve callback exception identity, and check unknown modes including a lone-surrogate string. The [source comparison](../../../../crates/python-sdk-ffi/examples/runtime_source_parity.rs) runs the pinned runtime-resolution test file against generated bindings with product-identity substitutions. Native tests verify buffer ownership and stale-handle refusal. [Release tests](../../../../crates/python-release/tests/release_parity.rs) cover target-specific library staging and wheel payload checks.

This decision does not establish complete Python SDK parity. The synchronous Rust client has native subprocess tests, but its Python client classes, mutable notification and event identity, complete Python value edge cases, and installed full-turn SDK smoke remain separate work. The Linux release matrix also requires native execution evidence.

## Alternatives considered

**Keep runtime lookup logic in Python.** That would create a second implementation of mode precedence, platform aliases, and missing-artifact behavior, contrary to Rust ownership.

**Ship a CPython extension as the SDK wheel.** That would change the platform-independent SDK distribution and introduce Python ABI-specific build and installation requirements. A C ABI library remains part of the existing native runtime distribution.

**Serialize every callback argument first.** Interpreter equality and representation can execute caller code and raise exact exception objects. Opaque references preserve those operations and their ordering.

**Return allocator addresses as reusable ownership identities.** An allocator can reuse a freed address, allowing a stale release to affect a later response. Non-reused handles separate ownership from storage addresses.

## Consequences

Python's public runtime API delegates selection and filesystem decisions to compiled Rust without acquiring or downloading a runtime. The binding requires the matching native library and ABI, while the standalone runtime executable still needs no Python installation.

The [packaged-runtime assembly](2026-09-04-rust-packaged-sdk-runtime-assembly.md), [single-executable distribution](2026-07-10-single-file-executable-sdk-runtime-distribution.md), and [publication workflow](../process/2026-08-11-python-publication-workflow.md) notes remain active. This ABI does not change plugin reload rules, configuration ownership, publication authorization, or the evidence required for a complete SDK release.
