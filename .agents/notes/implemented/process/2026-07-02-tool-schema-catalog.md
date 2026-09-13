# Agent Note: Generated tool-schema catalog (boot-and-harvest)

Status: implemented

English | [中文](2026-07-02-tool-schema-catalog.zh.md)

## Problem

The repository had no single reference for the names, descriptions, and JSON Schemas actually exposed to the model. Source declarations are scattered and runtime-composed, while the existing Cordis reference and subsystem pages cover wiring and vocabulary rather than tools.

## Decision

Generate the catalog by **booting each Rust tool package and reading its registered schemas**, not by parsing source. [`xtask/src/tool_catalog.rs`](../../../../xtask/src/tool_catalog.rs) mounts each package on a fresh Cordis `Context` with `SystemPrompt`, `ToolRuntime`, and the services its registration reads; calls `ToolRuntime::schemas()`, which returns the exact ordered `ToolSchema` values sent to the model; disposes the context to quiescence; and renders one `## <package>` section with a ` ```json ` `parameters` block per tool. `cargo xtask tool-catalog` regenerates the deterministic manifest-ordered, name-sorted output, while `--check` rejects a stale committed copy. The `verify-tool-catalog` package script invokes that Rust check inside `doc-sync`, so documentation changes and CI use the same freshness path.

### Why boot, not parse (the crux)

The pinned source's Cordis catalog can use a static-source pass because every event and service name round-trips to a declaration. **Tool schemas are not statically knowable**, so static TypeScript or Rust analysis would produce a document that lies:

- `tool-todo` expands its status constants into `["pending","in_progress","completed"]` at runtime; syntax inspection sees the construction, not the registered literals.
- Descriptions incorporate resolved caps and configuration, so the model reads final strings rather than their source fragments.
- `tool-subagent` chooses its tool name at load time, including the shipped `subagent_fork` alias.
- An MCP compatibility plugin may register **raw JSON Schema** without the typed `define_tool` helper, so enumerating helper call sites under-counts.

The only faithful source of truth is the schema the registry actually holds after the plugin loads. Booting is the [testing-policy discipline](../../../../docs/testing.md) "verify the world, not the self-report" applied to a doc generator: read the shipped artifact, not a re-derivation of it.

### Restoring "nothing silently omitted"

Booting has no declaration set to enumerate, so a new tool package could be forgotten. `assert_manifest_complete` restores the guarantee by comparing the Rust boot manifest with every `packages/*/tool-*` directory in the pinned source checkout. Any omitted source package fails the generator, and therefore `doc-sync`, until its Rust boot recipe is present.

### A hand-maintained boot manifest is the irreducible policy

The pinned filesystem discovers the required package inventory and the completeness guard rejects omissions. `tool_packages()` still owns an explicit Rust boot recipe for each package because required Service Providers, scoped registration, and configuration choices are policy, not facts that layout or injection names can determine safely.

### Scope

The manifest covers every shipped product tool corresponding to the pinned `packages/*/tool-*` inventory, plus schemas owned by the core tool registry, plan mode, and Schedule. Each package boots with its deployment default unless a required choice is recorded in its catalog note. Example-only tools are excluded.

The catalog unit is a package, not every configured tool instance. Each package boots once with default config; load-time aliases such as `subagent_fork` are noted without enumerating every deployment permutation. A deployment inventory is a separate, unbounded surface.

### A plain `json` fence

Schema blocks use ` ```json `, not a bespoke `ts`-family fence. `doc-typecheck` only extracts `ts*` fences, so a JSON block is invisible to it — no `BlockKind` wiring is needed (unlike the cordis catalog's `ts cordis-catalog` fence, which had to be allowlisted so a bare signature fragment isn't compiled).

## Verification

The Rust generator tests boot every package, assert the complete 52-name catalog, exercise scoped Schedule and report registrations, retain the runtime-expanded todo enum, verify Rust source attribution and the `subagent_fork` note, reject incomplete manifests and empty harvests, and pin Markdown rendering. The source generator guarantee suite remains the oracle, while the generated catalog differential requires every tool name, description, and pretty-printed JSON Schema block to be byte-identical to the pinned source; this also pins schema-object key order that affects serialized model requests and request-cache bytes.

## Alternatives considered

- **Static TypeScript or Rust source analysis** — tool schemas are not statically knowable: runtime values, resolved descriptions, configured names, and raw registrations make a syntax-derived document lie.
- **Inferring each package's boot recipe from its injects** — the "too clever" path [the discover-package-inventory proposal](../../proposed/process/2026-06-20-discover-package-inventory.md) warns against; the recipe stays hand-written policy while the inventory is discovered and completeness-guarded.
- **A bespoke `ts`-family fence for schema blocks** — unnecessary: a plain ` ```json ` fence is invisible to `doc-typecheck`, so no `BlockKind` allowlisting is needed.

## Consequences

- The catalog cannot drift: a tool schema change the committed file doesn't reflect fails `verify-tool-catalog` in `doc-sync` and CI. A new `tool-*` package not added to the manifest fails the completeness guard outright.
- Tool description prose has a single home — the `defineTool` `description` at the source — and the generated entry is only as good as it, the same forcing function the cordis catalog applies to event JSDoc.
- The generator links and executes the Rust workspace packages through `cargo xtask`; it requires no Node runtime or separately built package artifacts.
- A new capability seam behind a future tool means a new manifest recipe entry (which seams to mount). This is the deliberate hand-written cost called out above; it changes only when a tool package is added.
