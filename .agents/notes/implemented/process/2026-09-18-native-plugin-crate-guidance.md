# Agent Note: Native plugin crates have agent-facing authoring guidance

Status: implemented

English | [中文](2026-09-18-native-plugin-crate-guidance.zh.md)

## Problem

Every plugin the `seekdeep` binary ships is a Rust crate, yet the authoring guidance an agent finds describes the TypeScript era: the add-a-package cookbook scaffolds `packages/<group>/<pkg>/src/index.ts`, `packages/AGENTS.md` speaks of `src/types.ts`, and no subtree instructions exist under `crates/`. The crate conventions live only by example across more than two hundred crates: the `NAME`, `INJECT`, and `plugin()` exports, Loader-facing config validation, catalog registration in the boot roster, bundle rows, README triplets, composition and oracle tests, generated catalogs, and the parity manifest. An agent following the visible documents produces a package shell without runtime and misses the gates that reject it.

## Decision

`crates/AGENTS.md` carries the subtree orders for plugin crates: where runtime lives, the crate shape, the tests and evidence, and the wiring, docs, and gates. `crates/CLAUDE.md` is its symlink, `docs/AGENTS.md` lists the subtree in the instruction tier, and the budget manifest caps the file. The `seekdeep-native-plugin` skill orders the work: decide native Rust versus a TypeScript file plugin from the runtime-placement rules, start from a shipped crate of the same role, implement, wire, test, regenerate, record, and verify, then report. The skill links the contracts instead of restating them; `packages/AGENTS.md` remains the home of the language-independent behavioral rules.

## Alternatives considered

**A paired cookbook page under `docs/cookbook/`.** The right human-facing home, but it costs a Chinese counterpart, site navigation, and projection checks on every edit, and the immediate gap is agent-facing. Deferred; the skill and `crates/AGENTS.md` are the sources it would summarize.

**Extending the TypeScript cookbook in place.** Rejected: that page documents the manifest and README half of a plugin and the preserved file-plugin surface; folding crate mechanics into it would blur which half is runtime.

**A scaffolding command.** Rejected for now: a generator freezes one crate shape while ported crates still vary by role, and a copied shipped crate already gives an agent a working starting point.

## Consequences

- An agent adding a plugin crate has one ordered path and the gates it must run before claiming checks pass.
- `crates/AGENTS.md` is budgeted and English-only like the other subtree instructions; the skill is unpaired like every skill.
- The TypeScript cookbook remains authoritative for file plugins and package manifests until a paired native cookbook page exists.
