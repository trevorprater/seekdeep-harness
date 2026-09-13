# Agent Note: Generate the config catalog in Rust

Status: implemented

English | [中文](2026-08-25-rust-config-catalog-generator.zh.md)

## Problem

The plugin config catalog is an executable parity check, not a copied document. It must classify every pinned source package, paste each plugin's complete config declaration closure with JSDoc, expose external type dependencies, and reject any enumerable Schemastery path that the declared config type omits. Leaving that authority in `scripts/gen-config-catalog.ts` would require a TypeScript and Node toolchain inside the Rust port, while keeping only the last generated Markdown would silently lose freshness and negative-path enforcement.

## Decision

`cargo xtask config-catalog` owns collection, validation, rendering, and freshness checks. The Rust generator parses the pinned `SOURCE_SNAPSHOT` package entries with OXC, converts declarations into an owned type graph, follows package-local imports and workspace re-export chains, preserves verbatim declaration spans, and aggregates every violation before returning. Its schema walk handles object, array, union, chained refinement, and intersect composition forms; its type walk handles interfaces and heritage, aliases, literals, arrays, intersections, unions, indexed access, utility wrappers, and workspace references. Unknown external types remain unknown rather than becoming false missing-field reports.

The generated English catalog applies the approved SeekDeep product renames while retaining the source declaration and path oracle. `cargo xtask config-catalog --check` compares the complete rendered artifact with `docs/config-catalog.md`. The reviewed Chinese counterpart remains paired documentation rather than generator output.

The Rust differential suite mirrors all 24 source generator cases, and a whole-corpus run must collect every eligible package in the pinned checkout before the manifest rows are verified.

## Alternatives considered

**Run the TypeScript generator from Rust.** This would preserve the old implementation but keep Node, TypeScript, and workspace package installation as production tooling dependencies, violating the Rust-port boundary.

**Treat the checked-in Markdown as the port.** A static copy cannot detect a new package, undocumented config member, type collision, or schema-only field, so it would turn executable documentation into an unaudited snapshot.

**Generate only from native Rust config structs.** Native types are the target implementation authority, but the pinned TypeScript declarations remain the differential oracle. Dropping their closure and schema cross-check would make source behavior disappear from the completion proof instead of proving the intentional Rust representation.

## Consequences

Config catalog checks need no JavaScript runtime and fail loud on syntax the owned OXC projection cannot classify. The generator carries an explicit source-type and Schemastery-expression model, which must grow when the oracle adopts a new declaration form. Generated English and reviewed Chinese pages still change together; only the English page is freshness-generated.
