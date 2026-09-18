# Deviation register

The parity oracle is the source checkout pinned in [`SOURCE_SNAPSHOT`](../SOURCE_SNAPSHOT) (commit `37200a934324dd7167ec8a8d3ac1fd01e2239909`). This file is the exhaustive register of deliberate behavioral deviations: every surface not named by a `DEV-` entry ports at full parity, and identity renames governed by [`AGENTS.md`](../AGENTS.md) are not deviations. When a surface named here is ported, its `porting/parity.json` entry must carry a `note` referencing the entry, and the entry's enforcement gates are part of its verification evidence.

## Network egress inventory

Audited 2026-08-15 against the pinned commit. Every path by which the source harness can emit bytes off the machine:

| # | Egress | Destination | Default | Payload | Evidence (source path at pinned commit) |
|---|--------|-------------|---------|---------|------------------------------------------|
| 1 | Model API | `https://api.deepseek.com` or the configured provider base URL | active when credentialed, user-invoked | prompts and session context (the product's purpose) | `packages/llm/llm-deepseek/src/index.ts:104` |
| 2 | Web search provider | provider default, `DEEPSEEK_SEARCH_BASE_URL` override | user-invoked tool | search queries | `apps/cli/reference/README.md:76` |
| 3 | Session telemetry | `https://harness-telemetry.deepseeksvc.com/v1/logs` unless `DSH_TELEMETRY_OTLP_URL` overrides | mounted `DISABLED`; opt-in via `DSH_TELEMETRY_MODE` | `FULL`: every projected session event, unredacted (message text, tool arguments and results, workspace paths); `FEEDBACK_ONLY`: the session-log suffix at each recorded feedback; the OTel resource carries `user.id` = the anonymous UUID | `packages/bundle/base/cordis.patch.yml:148-161`, `packages/session/session-telemetry-otel/src/index.ts:198-204` |
| 4 | Telemetry hard opt-out | n/a | any non-empty `DSH_TELEMETRY_DISABLED` patches the row disabled at boot; config cannot re-enable it | n/a | `apps/cli/src/profile-boot.ts:56-82` |

Local-only by construction: the anonymous user id (random UUIDv4 at `$DSH_HOME/.anonymous-user-id`, never derived from hostname, network address, or git remote; deleting the file mints a fresh identity — `packages/identity/anonymous-user-id/src/index.ts`) and `/feedback` (an append-only session-log event that leaves the machine only under an opted-in telemetry mode — `packages/feedback/command-feedback/src/index.ts`). The web UI binds `127.0.0.1:3080`.

Verified absent at the pinned commit: analytics or crash-reporting SDKs (`sentry`, `posthog`, `amplitude`, `mixpanel`, `segment`, `statsig`, `bugsnag`, `datadog` have no match in `pnpm-lock.yaml`) and update checks in production sources. Mentions that read as egress but are not runtime: community QR links in the READMEs, the Node download mirror in `scripts/wine-windows-gates.sh:87`, and the publish registry in `scripts/publish-npm-baseline.ts:23`.

## DEV-001: no default telemetry collector URL

- **Source behavior.** The base bundle's `session-telemetry-otel` row resolves `exporter.url` to `DSH_TELEMETRY_OTLP_URL ?? 'https://harness-telemetry.deepseeksvc.com/v1/logs'` (`packages/bundle/base/cordis.patch.yml:154`), so enabling a telemetry mode without choosing a collector exports session data to DeepSeek's collector.
- **Ported behavior.** The base bundle resolves `exporter.url` from `SEEKDEEP_TELEMETRY_OTLP_URL` with no fallback constant. The plugin already requires and validates `url` at load for every mode other than `DISABLED`, so enabling telemetry without an explicit collector fails at boot, consistent with the charter's "misconfiguration fails at the earliest resolvable point".
- **Observable delta.** Default behavior is identical (`DISABLED`, zero egress). The only divergence: a telemetry mode enabled with no explicit URL — the source exports to `harness-telemetry.deepseeksvc.com`; the port refuses to boot with a configuration error naming the field.
- **Rationale.** The collector is DeepSeek product infrastructure, not a model-protocol field, so it does not survive the `dsh` → `seekdeep` identity rename on its own terms, and a renamed harness exporting to it would be wrong in both directions. Removing the constant makes "no bytes to DeepSeek's collector" unconditional rather than merely default.
- **Affected surfaces.** `packages/bundle/base/cordis.patch.yml` (the constant), `apps/cli/reference/README.md` and `README.zh.md` (documented default), `apps/cli/composition.md`, `.agents/notes/implemented/feature/2026-08-10-telemetry-default-off.md` and its i18n siblings (deployment stance), plus any snapshot that captures the rendered base-bundle config.

## DEV-002: unpaired UTF-16 in exported telemetry records

- **Source behavior.** The OTLP/HTTP exporter serializes log bodies and attributes with JavaScript's JSON writer, so a string or key holding an unpaired UTF-16 surrogate (which the session log accepts losslessly) reaches the collector as a `\uXXXX` JSON escape that a JSON reader decodes back to the lone code unit.
- **Ported behavior.** The native exporter carries strings through the OpenTelemetry SDK, which only represents Unicode scalar values. A string or key with an unpaired surrogate is exported as the text of that escape (`\ud83d`, six ASCII characters) and the record carries the attribute `seekdeep.telemetry.unpaired_utf16 = "escaped"`; every other record is unchanged. Before this entry the native exporter dropped such records entirely.
- **Observable delta.** Only records containing unpaired surrogates differ: the collector receives the escape as literal text instead of a lone code unit, plus the marker attribute. No record is lost.
- **Rationale.** The SDK's string type cannot hold the code unit and its JSON writer would re-escape any backslash we emit; the escape text is a faithful, reversible encoding of the source's wire bytes, and the marker lets a consumer decode it deliberately.
- **Affected surfaces.** `crates/session-telemetry-otel/src/native.rs` (conversion and marker), the native exporter tests, and the `packages/session/session-telemetry-otel/src/index.ts` parity row.

## DEV-003: a fetch body cap inside a surrogate pair

- **Source behavior.** `web-fetch-http` slices the decoded body to `maxBodyChars` UTF-16 code units with JavaScript string semantics; a cap landing between the two units of an astral character keeps the lone high surrogate at the end of the text.
- **Ported behavior.** The port counts and cuts in UTF-16 units as well, but a Rust string cannot end in a lone surrogate: the cut moves one unit earlier, before the pair, so the kept text is one unit shorter than the source's and carries no replacement character.
- **Observable delta.** Only a body cut exactly inside a surrogate pair differs: the source ends with an unpaired code unit, the port ends before the character. `truncated` is reported the same way.
- **Rationale.** A replacement character would insert text the page never contained, and carrying an unpaired unit through the tool result would need a non-string body type for one edge; dropping the split character keeps the text a faithful prefix of the page.
- **Affected surfaces.** `crates/web-fetch-http/src/provider.rs` (the fetch body cap) and its fetch specification tests; `crates/tool-skill/src/lib.rs` (`catalog_description`, the skill catalog's `maxLength - 3` cut before `...`); `crates/tool-ralph/src/index.rs` (`bound_result`, the `maxResultChars` cut before the truncation notice); `crates/tool-bash-persistent/src/lib.rs` (`maybe_truncate`, the persistent shell's `maxResultChars` cut); `crates/tool-web/src/lib.rs` (`utf16_prefix`, the web tool's fetch `maxChars` cut); `crates/tool-str-replace-editor/src/lib.rs` (`maybe_truncate`, the editor's `maxOutputChars` view cut); `crates/tool-fs/src/read_render.rs` (line truncation). The shared rule lives in `seekdeep_util::utf16::utf16_prefix`.

## DEV-004: compiled packages publish declarations only

- **Source behavior.** Every first-party package is a TypeScript library: `scripts/check-workspace-constraints.ts` requires `main: lib/index.js`, `types: lib/types/index.d.ts`, a root export with a `default` runtime target, a `./invariant` export naming `lib/invariant.js`, and a `files` list that publishes those files, and `npm pack` ships them.
- **Ported behavior.** A package whose implementation moved to a Rust crate marks its manifest `"seekdeep": {"compiled": true}`. It keeps its name, workspace dependencies, and generated declarations, so TypeScript consumers still resolve its types, but it sets no `main` or `bin`, exports only `types` conditions for its root and `./invariant` entries, and publishes `lib/types/**/*.d.ts` plus its non-JavaScript extras (a bundle's `cordis.patch.yml`, a skill's `assets`) and the runtime files it still exports (the typert remote client). The constraints gate enforces that shape for marked packages and the source's shape for every other package; `publint` and the built-package invariant gates verify the built package.
- **Observable delta.** Installing such a package from npm yields type declarations and no importable JavaScript; the source's package exposes a runtime entry. Nothing in the port imports those entries, since the behavior lives in the crates and the generated Client packages carry their own runtime.
- **Rationale.** The alternative, generating entry files for 187 packages, would either ship no-op shims (a hallucinated runtime) or require a per-package delegation contract into the compiled runtime that no consumer needs today. Declarations-only manifests promise exactly what the build emits.
- **Affected surfaces.** `crates/repository-tools/src/workspace_constraints/package.rs` (the compiled rule and its publication list), `crates/repository-tools/src/package_invariants.rs` and `crates/repository-tools/src/built_package_invariants.rs` (types-only companions), the marked manifests under `packages/`, and the `scripts/check-workspace-constraints.ts` parity row.

## Explicit non-deviations

These port at full parity; they are listed so removal never creeps beyond DEV-001, DEV-002, DEV-003, and DEV-004:

- The telemetry capability: capture coordinator, `session-telemetry/record` redaction waterfall, and the OTel backend — a vendor-neutral OTLP/HTTP exporter whose whole configuration surface is preserved.
- `anonymous-user-id` and both feedback packages; the `/feedback` acknowledgement and its sharing disclosure are user-facing surfaces.
- Provider endpoints and credentials: `PUBLIC_BASE_URL = https://api.deepseek.com`, `DEEPSEEK_API_KEY`, `DEEPSEEK_SEARCH_BASE_URL` — external protocol and provider fields stay per the charter.
- The loopback web UI bind.

## Renames adjacent to DEV-001 (rename policy, not deviations)

`DSH_TELEMETRY_MODE` → `SEEKDEEP_TELEMETRY_MODE`, `DSH_TELEMETRY_OTLP_URL` → `SEEKDEEP_TELEMETRY_OTLP_URL`, `DSH_TELEMETRY_DISABLED` → `SEEKDEEP_TELEMETRY_DISABLED`, `$DSH_HOME` (`~/.dsh`) → `$SEEKDEEP_HOME` (`~/.seekdeep`). The `.anonymous-user-id` file name and the `session-telemetry-otel` row id carry no product brand and are unchanged.

## Enforcement

1. **Manifest notes.** Every surface DEV-001 names carries a `note` referencing it in `porting/parity.json`; seeded on `packages/bundle/base/cordis.patch.yml`, added to the rest as they are ported.
2. **Zero-egress gate.** Before any telemetry surface is marked `verified`: run the default profile through keyless headless replay under a deny-all network policy and assert the process opens no non-loopback connection and issues no DNS query for any host not explicitly configured; `*.deepseeksvc.com` must never resolve. The gate is verification evidence for every surface DEV-001 names.
3. **String ban.** `deepseeksvc` must not appear in this repository outside this file. Candidate `cargo xtask` check once the in-flight xtask work settles.

Invariant: a default-configuration `seekdeep` process opens no non-loopback network connection except to explicitly configured provider endpoints.

## Withdrawn tooling surfaces

A source surface whose entire behavior is a JavaScript tool configuration has no counterpart once the port carries that behavior in Rust. `porting/parity.json` records those surfaces as `withdrawn`. The parity gate accepts the status only with a non-empty `note`, only when the surface names no targets or evidence, and it reports the count separately from the verified total, so work that exists cannot be filed as withdrawn.

| Surface | Why the port carries no counterpart | Replacement gates |
|---------|-------------------------------------|-------------------|
| `knip.json` | knip derives unused files, exports, and dependencies from TypeScript import sites. The port's consumers are Rust crates, so knip reports the workspace dependency declarations as unused (212 of them) and resolves neither the package-entry nor the source-root imports. | `verify-runtime-closure`, `verify-package-invariants`, `verify-package-paths`, `verify-vendored-links`, `check-workspace-constraints` |
