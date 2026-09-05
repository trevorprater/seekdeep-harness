# Agent Note: The configurable-provider directory withholds unsupported OAuth-only providers

Status: implemented

English | [中文](2026-08-13-oauth-only-providers-withheld.zh.md)

## Problem

The Models page offered `openai-codex` like any other pi-ai route, with the placeholder every pi-ai provider carries: enter a key, or leave it blank to authenticate from the environment. Configuring it that way and sending a message failed the turn with `Provider is not configured: openai-codex`, reported as the adapter's catch-all `PI_AI_ERROR`.

The posture the placeholder invited could not work on that route. pi-ai's `resolveProviderAuth` reaches an OAuth provider through one path — a credential already in the collection's `CredentialStore` — and has no ambient fallback for it, while `openai-codex` is the one installed provider declaring `auth.oauth` with no `auth.apiKey` beside it. Before the later Codex bridge, `PiAiAdapter.current()` constructed its collection with `createModels()` and no options, so the store was pi-ai's default `InMemoryCredentialStore`: empty at every boot, and rebuilt from scratch each time a configuration change produced a new snapshot. No code here ran `Models.login()`, and pi-ai's library half did not read Codex's own `~/.codex/auth.json` either — its OAuth module is a PKCE login flow whose credential the *host* application persists, which is what the pi CLI supplied and this adapter did not.

So the page advertised, with the keyless posture its own placeholder describes, a provider that has no keyless posture — and the failure named the configuration key rather than the missing capability. The one thing that does authenticate the route is a ChatGPT OAuth token pasted into the key field, which is not what the offer describes and expires with nothing here to refresh it.

## Decision

The directory offers only what this adapter can authenticate. `catalogProviderTakesApiKey(provider)` answers whether pi-ai's installed provider for a route declares an api-key method — the general method the Harness can feed, since it resolves a key through its own credential seam and hands it over as the request's `apiKey` override — and `directoryEntries()` skips OAuth-only catalog routes unless the adapter names an explicit supported exception.

Generic OAuth support is not attempted. It needs a provider-scoped persistent credential owner, a login flow, and a surface to run it from; shipping an offer without those parts is what produced the report. The later [file-backed Codex ChatGPT OAuth decision](../architecture/2026-08-14-file-backed-codex-chatgpt-oauth.md) supplies those prerequisites for `openai-codex` alone, with the official Codex CLI owning login and logout.

Two boundaries keep the withholding narrow:

- **Catalog membership is unchanged.** `catalogProviderIds()` still answers what pi-ai ships, so the `declared` flag on a directory entry keeps meaning "no installed provider answers for this route" rather than "this route is not offered".
- **The profile half of the union is unconditional.** A route a settings document already names keeps its entry, so a stored `openai-codex` profile stays visible, editable, and deletable instead of being stranded in the document with nothing on the page to remove it.

Resolution is untouched. A profile naming `apiKeyEnv` on an OAuth-only route still builds a working provider — `routeAuth` adds the harness api-key method beside the catalog's OAuth, and pi-ai's Codex API derives the account id from the token itself — so a deployment that writes one into `settings.yaml` or `cordis.yml` keeps that path. Enforcing the withholding in `resolveProfiles` instead would have refused such a profile at registration, and because `validate` runs at boot as well as at write time, a document already naming a keyless OAuth route would fail the whole namespace's registration rather than one provider.

## Alternatives considered

- **Rejecting a keyless OAuth-only route in `resolveProfiles`.** This is where the repo normally enforces a decision, and the directory filter is a surface that a `cordis.yml` entry bypasses. It was refused for the boot behavior above: an existing stored profile would take down every other route in the namespace with it, which for a release trades a one-provider defect for a total one. The gap is that the offer, not the capability, is what got fixed — a deployment can still hand-write the route it can no longer add from the page.
- **Keeping the offer and correcting only the placeholder text.** The field would then have to say the provider needs a login this build cannot run, which is a card whose only honest content is that it does not work.
- **Mapping `Provider is not configured` to a named `LlmError`.** Worth doing, and reachable for reasons this change does not remove — any api-key route left blank whose provider finds nothing in the process environment produces the same message. Deferred as a separate change: it improves a diagnostic rather than removing a broken offer.
- **Reading `~/.codex/auth.json` into a pi-ai `CredentialStore`.** It makes Codex work without a Harness login flow, and pi-ai owns the refresh. It also binds the Harness to another tool's file format for one provider, which was a decision for separate OAuth work rather than this release fix; the later [Codex OAuth decision](../architecture/2026-08-14-file-backed-codex-chatgpt-oauth.md) adopts it with provider scoping, validation, preserving writes, and explicit CLI ownership.

## Consequences

An installed provider that offers OAuth alone disappears from the provider picker unless this adapter has an explicit credential source for it. The `openai-codex` exception now remains visible because its file-backed Codex bridge makes that route serviceable. Providers that offer OAuth *beside* an api-key method (`anthropic`, `github-copilot`, `kimi-coding`, `openrouter`, `radius`, `xai`) keep their entries and their key path. A future OAuth-only provider stays withheld automatically rather than being offered on the strength of OAuth metadata alone.

Two adjacent gaps remain and are recorded in the package README: a route naming no credential still resolves through the catalog provider's own discovery, which reads process environment variables only — not `~/.aws/credentials`, and not the harness credential seam — and the resulting failure is still the catch-all `PI_AI_ERROR`.

## Testing

Package tests pin both halves of the union: unsupported OAuth-only routes are absent while API-key routes stay, and the explicit `openai-codex` exception produces a full entry with `declared: false` and `authentication: 'codex-oauth'`. Resolution tests continue to prove that a hand-written keyed profile can serve an otherwise withheld route. The Models browser snapshot records the Codex exception's distinct setup instead of presenting it as an ordinary API-key provider.
