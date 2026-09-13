# Agent Note: The Cordis catalog generators are Rust gates over the pinned source tree

Status: implemented

English | [中文](2026-09-07-rust-cordis-catalog-generators.zh.md)

## Problem

The source repository generates its Cordis documentation and model-facing catalogs with four TypeScript scripts: `gen-cordis-catalog.ts` (per-subsystem service/event regions, the inherited-tier page, the runtime API module, and the five detailed core API pages), `gen-client-catalog.ts` (the browser slot surface), `gen-cordis-inspect-catalog.ts` (the Client inspect catalog), and the shared `cordis-walk.ts`, `slot-walk.ts`, `cordis-core-api.ts`, and `jsdoc.ts` helpers. The port committed the generated documentation and the JSON data the Rust runtime embeds, but the JSON was captured by evaluating the source's generated TypeScript modules under `tsx`, and no Rust code could regenerate or freshness-check any of it.

## Decision

Every generator is Rust. The pure judgement lives in `seekdeep-repository-tools` and is proven by ports of the source specs: `jsdoc` (prose, tags, parameter and return completeness), `cordis_catalog_partition` (`spliceRegion`, `walkPartitionProblems`, `maybeRecordPair`), `client_catalog` (contract validation, entry projection, the per-slot report budget, the data module renderer), and `cordis_core_api` (the five page renderers). The lexical scans (`cordis_walk`, `slot_walk`) parse TypeScript with `oxc` and reproduce the compiler conventions the source relies on — `getStart` with and without `JSDoc`, `getFullStart` leading-trivia rules, computed-name text, line numbers — through `ts_lexical`.

`cargo xtask cordis-catalog`, `cargo xtask client-catalog`, and `cargo xtask cordis-inspect-catalog` orchestrate them against the pinned source checkout (`--source`, defaulting to the oracle), analyzing the workspace with the native Typert analyzer and comparing or writing the port's artifacts: the bilingual `docs/subsystems/*.md` regions (re-recording a pair only when the write is region-confined), `docs/cordis-api/inherited.md`, the five core API pages, `crates/tool-cordis/data/api-catalog.json`, `crates/cordis-client-runner/data/slot-catalog.json`, and `crates/cordis-client-runner/data/api-catalog.json`. `--check` is the freshness gate.

The curated policy tables (`SERVICE_PAGE`, the walk exemptions, `EVENT_SCOPE_PAGE`, `LINK_MAP`, `TYPE_LINK_EXEMPTIONS`, `CORDIS_CATALOG_POLICY`, `CLIENT_SERVICES`, `CLIENT_EVENTS`) are Rust constants and are pinned against the source scripts on every run: the command reads the source constants through the pinned TypeScript library and fails before rendering when any table drifts.

Documentation and data keep the identities the port already committed: data artifacts use the data rename (`@deepseek-ai/dsh-` → `@seekdeep-ai/seekdeep-`, `dsh-` → `seekdeep-`, …), and documentation additionally renames the `dsh.client` manifest field and the `@dshScopeScan` tag.

## Alternatives considered

**Keep capturing catalog data by evaluating source-generated TypeScript.** Rejected: the artifacts then depend on a Node toolchain the port does not own, and the generators' partition, contract, and completeness checks stay unported.

**Scan with the hosted TypeScript compiler instead of `oxc`.** Rejected: the scans are lexical by design (the source deliberately avoids a type-checker program for them), and `oxc` keeps them fast and independent of the compiler-backed projection they backstop.

## Consequences

- The generators require the pinned source checkout and its `node_modules/typescript`; the port carries no TypeScript package sources of its own to scan.
- A curated-table edit must be made in both repositories or the gate fails loudly, which is the intended fail-closed partition behavior.
- `cargo xtask cordis-catalog` writes bilingual page regions and refreshes `.i18n.yaml` records only for region-confined writes, so translation drift is still surfaced by the pairing gate rather than silently repaired.
