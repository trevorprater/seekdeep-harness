# Agent Note: The native cordis crate carries a test-pinned runnable example

Status: implemented

English | [中文](2026-09-18-native-cordis-runnable-example.zh.md)

## Problem

Built-in host plugins are native Rust, and the cordis crate is the runtime they mount into, yet every Cordis walkthrough targets the TypeScript face: the tutorial runs `vendor/cordis/bin.js`, the cookbook scaffolds a `packages/<group>/<pkg>` TypeScript package, and no document names `Plugin::new`. The only Rust that exercises a provider, an injecting consumer, and event listeners end to end is the oracle tests that pin source semantics, which a newcomer cannot read as an introduction.

## Decision

`crates/cordis/examples/hello_cordis.rs` is a cargo example of the crate. It mounts two `greeter` providers, an app that injects `greeter`, and two `greet` observers on one root context, disposes one provider, mounts the other, and prints each stage's output together with the app fiber's state. The crate's `Cargo.toml` declares the example with `test = true`, so a test inside the example pins that output and the fiber states, and `cargo test` for the package runs it. The crate-level docs and the Cordis tutorial index point to the example.

The example compiles for `wasm32-unknown-unknown` through a stub `main`, because the browser build has no runtime to drive it.

## Alternatives considered

**A Rust chapter in the Cordis tutorial.** The tutorial deliberately lands readers inside the harness's TypeScript composition ([tutorial decision](../../archived/process/2026-07-22-cordis-tutorial-docs.md)); a chapter against the native crate would break its single launcher and its real-output discipline. Rejected in favor of a pointer from the tutorial index.

**An untested example.** Cargo builds examples during `cargo test` but does not run them, so the printed output would drift silently. Rejected: `test = true` costs one target declaration.

**An integration test under `crates/cordis/tests`.** A test file pins behavior but cannot be run through `cargo run --example`, and the crate's existing tests are oracle ports rather than introductions. Rejected.

## Consequences

- A reader of the native crate has a runnable introduction whose output is pinned; changing lifecycle ordering or `FiberState` transitions fails the example's test.
- An example target declared with `test = true` adds one test binary to `cargo test --workspace`.
- The example is native-only; the browser build compiles it as an empty program.
