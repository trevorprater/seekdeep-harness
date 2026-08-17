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

## Explicit non-deviations

These port at full parity; they are listed so removal never creeps beyond DEV-001:

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
