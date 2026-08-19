# Agent Note: File-backed Codex ChatGPT OAuth

Status: implemented

English | [中文](2026-08-14-file-backed-codex-chatgpt-oauth.zh.md)

## Problem

The pi-ai adapter already ships the `openai-codex` provider and its Codex Responses implementation, but the provider authenticates only from an OAuth credential in pi-ai's `CredentialStore`. The Harness supplied API keys per request and intentionally left that store empty, so the route could not use a ChatGPT subscription. Asking users to paste an access token into an API-key field would create an expiring, unrefreshable credential and misstate the setup.

The official Codex CLI already owns a ChatGPT browser login, token refresh, logout, and a configurable credential store under `$CODEX_HOME` or `~/.codex`. Gollem demonstrates the provider behavior and login class, but its application-owned credential file is a separate session rather than the user's official Codex login. The Harness needs one explicit owner for login and one safe adapter into the provider without making pi-ai's credential store a second source for API-key routes.

The earlier [OAuth-only provider withholding decision](../bug-fix/2026-08-13-oauth-only-providers-withheld.md) remains the default for providers without such an adapter; this note supersedes it only for `openai-codex`.

## Decision

The official Codex CLI owns login and logout. `llm-pi-ai` adapts its file-backed ChatGPT session into a `CredentialStore` that returns a value only for `openai-codex`. `$CODEX_HOME` is read from the immutable launch-environment snapshot, with `~/.codex` as Codex's default, and the selected `auth.json` is read again for every request. The Harness neither starts an OAuth callback server nor creates or deletes the shared credential; a missing session directs the user to configure `cli_auth_credentials_store = "file"` and run `codex login`, while removal directs them to `codex logout`.

The file is an external credential boundary. Reads require a regular owner-only file no larger than one mebibyte, current `auth_mode: "chatgpt"`, complete access/refresh/ID token fields, a valid access-token expiry, and one ChatGPT account identity shared by the token claim and the stored account field. Missing or non-ChatGPT files mean no credential. Invalid content fails closed without including credential values in diagnostics. The bridge preserves unknown root and token fields so a refresh does not narrow a newer Codex document.

pi-ai owns expiry detection, OAuth refresh, and Codex request authentication. Its refresh callback runs inside the bridge's `modify` operation. The bridge serializes its own writers with the repository file lock, rejects a changed account, rereads the external file after the network callback, retains an official Codex rotation it observes, and atomically replaces the document with mode `0600` only when the observed tokens are still current. The access token's claim supplies both the persisted expiry and the `chatgpt-account-id` used by pi-ai's Codex Responses transport.

API-key routes remain on the Harness credential seam and pass their resolved key as the request override; the injected store exposes no value for them. The configurable-provider directory marks setup explicitly with `authentication: 'api-key' | 'provider-native' | 'codex-oauth'`. A profile that explicitly names `apiKeyEnv` remains `api-key`, including the older keyed Codex path; an empty `openai-codex` profile is `codex-oauth`. That value crosses `llm.providers` to the Models page, where `codex-oauth` shows Codex login instructions, omits the API-key input, and never derives or writes an API-key reference. The behavior follows adapter metadata rather than a hardcoded provider name in the browser.

## Alternatives considered

- **Use Gollem's application-owned OAuth store and login flow.** This proves the ChatGPT backend but creates another login and does not consume the user's official Codex session. It would also require a Harness interaction surface and lifecycle for browser/device login before the provider could be offered.
- **Copy the current access token into `ctx.credentials`.** A copied token expires and cannot carry refresh/account metadata through the API-key seam. Refresh-token rotation would then leave either Codex or the Harness holding stale state.
- **Give pi-ai a general persistent credential store.** This would reintroduce a second credential source for every API-key provider and its ambient fallback, weakening the named-reference failure semantics. The selected store is deliberately incapable of serving any id except `openai-codex`.
- **Shell out to `codex` for every request or refresh.** The CLI exposes login commands, not a request-time credential service. A child process would add latency and still need an undocumented token exchange or transport protocol.
- **Read credentials from the operating-system keyring.** Codex can choose a keyring, but it does not expose a portable read API for another application. Requiring its documented file store keeps the data source explicit and testable; `auto` works only when it materializes `auth.json`.

## Consequences

An existing file-backed `codex login` now authenticates an `openai-codex` profile without an API key, including pi-ai's native token refresh and Codex Responses headers. Login after Harness startup is observed on the next request. Missing, malformed, overexposed, or account-inconsistent state fails before provider network I/O with a setup-specific credential code.

The adapter now depends on the current official Codex file fields. An incompatible future file format fails closed until this adapter is updated. `$CODEX_HOME` itself is fixed at Harness launch even though file contents are live. Other OAuth-only pi-ai providers stay withheld until each has an explicit provider-scoped credential owner.

The Harness file lock cannot make the official Codex process participate in the same writer protocol. A Codex rotation observed before the atomic commit wins, including the common reused-refresh-token race; an external writer that starts after the final reread remains outside that guarantee. Preserving account identity, complete documents, and owner-only replacement limits the consequence to rotation coordination rather than credential disclosure or cross-account use.

## Testing

The credential-store tests use isolated `CODEX_HOME` directories and synthetic unsigned JWTs. They cover provider scoping, absence, non-ChatGPT mode, malformed JSON, mismatched accounts, permission rejection, preserving unknown and Codex-owned fields, owner-only atomic replacement, changed-account refusal, concurrent official rotation, and login/logout ownership.

A real Loader composition mounts `LlmRuntime`, settings-file, credentials-local, and `llm-pi-ai`, then sends an `openai-codex` request to a local Codex Responses endpoint. It asserts the bearer token, `chatgpt-account-id`, endpoint, and assembled assistant text. A second assembled path expires the initial token, lets pi-ai execute its real refresh implementation against a local response, verifies the refreshed token reaches the Codex request, and reads the shared file back to prove the rotation and preserved fields. The missing-session path proves `MISSING_CREDENTIAL` before network access. Host wire tests and browser tests pin `authentication`, the Codex instructions, absence of an API-key field/write, provider selection, and the recorded Models-page output.
