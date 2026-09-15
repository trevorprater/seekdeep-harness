# Agent Note: Package declarations and source documentation in the Rust port

Status: implemented

English | [中文](2026-09-15-rust-package-declarations-and-source-docs.zh.md)

## Problem

Foreign-language consumers need the complete public types of the pinned source packages after their implementations move to Rust. A partial declaration set can pass a narrow consumer while omitting unrelated packages. Flattening Host and Client projects also merges distinct Cordis interfaces and changes inferred public types.

Documentation has two different inputs: compilable examples consume public package declarations, while verbatim type and JSDoc examples reproduce original source spans. Compiler-emitted declarations can change punctuation and public class projections without changing a type, so they cannot replace the latter input.

## Decision

The [declaration publisher](../../../../xtask/src/remote_contracts/declarations.rs) captures the complete pinned solution through its original project references. Generated Remote declarations enter the compiler's virtual filesystem before compilation, and every output remains in memory during capture. Host and Client retain separate programs. Capture rejects compiler diagnostics and records original paths, package metadata, declaration contents, and the source revision.

The Rust publisher writes the recorded declarations under each package's published `lib` directory with product identity renames. Every Client build route finishes with this publisher, including specialized package builders. `host-assets` stages the declarations during the Host build, and standalone `doc-typecheck` refreshes them before compiling examples. The coordinated `doc-typecheck:contracts-ready` command consumes the existing build output. Package metadata remains part of the public contract even when the implementation lives in a Rust crate.

The [consumer check](../../../../xtask/src/remote_contracts/declarations/consumer.rs) copies package manifests and publishable declaration files into isolated Host and Client fixtures. It includes neither implementation sources nor nested workspace dependency installations. Strict library checking and negative consumers enforce branded identities, concrete return types, environment names, and the separation between Host services and Client Remote access. The public lib build verifies declaration freshness and runs these consumers after all package builders finish.

The [source oracle reader](../../../../crates/repository-tools/src/source_oracle.rs) resolves `SEEKDEEP_PARITY_SOURCE`, an adjacent checkout, or the location in [SOURCE_SNAPSHOT](../../../../SOURCE_SNAPSHOT), and rejects revision drift. Declaration equivalence reads missing source files directly from the pinned Git objects and preserves the original structural and JSDoc comparisons after product renames. Package-path verification accepts specification paths present in that same commit. Relative Markdown links must still resolve in the port; links to original implementations identify the pinned source revision explicitly.

The [two-program rule](2026-07-22-tsconfig-solution-root-two-aggregates.md) and [Remote build ordering](2026-08-08-api-remotes-generated-contract-build.md) retain their project-ownership and generated-dependency rationale. This note owns their Rust compatibility-declaration realization; neither older decision is fully superseded.

## Alternatives considered

- **Flatten all source packages into one compiler program.** Host and Client declaration merges would share one identity and alter the public contract.
- **Symlink whole workspace packages into consumer fixtures.** Nested dependency installations can introduce multiple Cordis type identities and let unpublished implementation files satisfy imports.
- **Relax source-equivalence normalization to accept emitted headers.** This would discard the source's exact declaration and JSDoc comparison. Reading pinned source objects preserves that authority.

## Consequences

Declaration publication works from the committed capture without a source checkout. Refreshing the capture and checking original documentation require the pinned oracle. Declaration and documentation checks establish API and specification consistency; runtime exports still require their compiled Rust implementations and assembled execution checks.
