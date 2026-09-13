# seekdeep-credentials

English | [中文](README.zh.md)

Credential Service Definition (`ctx.credentials`). One doctrine, three consequences:

**Configuration carries references to secrets, never the secrets.** A settings section or `cordis.yml` entry says `apiKeyEnv: DEEPSEEK_API_KEY`; the value behind that reference lives with a credential provider. So the settings document stays safe to sync and to render in a configuration UI, `describe()` can answer "is this configured, where from, can I write it" without ever holding a value, and rotating a secret touches no configuration file.

**Consumers resolve per operation.** `resolve(ref)` is called at the start of each operation (the LLM adapters resolve once per model request) and never cached across operations — that read is what makes a changed credential reach the very next request without restarting any plugin.

**An empty stored value is absent.** Everywhere: `resolve` skips it, `describe` reports it unconfigured. A blank can never masquerade as a configured secret.

## Surface

```rust
use seekdeep_credentials::{CREDENTIALS, credential_ref};

let reference = credential_ref("DEEPSEEK_API_KEY")?; // POSIX shell identifier, branded
let credentials = context.get(CREDENTIALS).expect("credentials service");
let hit = credentials.resolve(&reference).await?;     // Option<{ value, source }>
let info = credentials.describe(&reference).await?;  // { configured, source?, writable } — never the value
credentials.set(&reference, "sk-…").await?;          // provider rejects read-only shadowing
credentials.unset(&reference).await?;                 // no-op when absent; same shadowing rule
# Ok::<(), anyhow::Error>(())
```

`credentials/updated (ref)` fires after a committed change to a provider-managed source — a `set`, an `unset`, or an external edit observed in storage. Ambient process-environment changes are not observable and never emit. Consumers do not need the event (they re-resolve per operation); it exists for configuration UIs refreshing a "configured" badge. The event carries the same public `CredentialRef` newtype that the provider API accepts, so emitters and consumers share one process-safe and persistence-safe identity rather than restating a bare string contract.

The shadowing rule on `set`/`unset` is deliberate fail-loud: when a read-only source (the live process environment, in the local provider) currently supplies the reference, a write would appear to succeed while resolution keeps returning the shadowing value — the seam rejects instead, and `describe().writable` lets a UI render the reference read-only up front.

## Providers

`seekdeep-credentials-local` layers the inherited process environment over its managed `$SEEKDEEP_HOME/.credentials.yaml` document, with the launcher's project and user `.env` layers as fallbacks. The seam shape leaves room for keyring-, helper-command-, and KMS-backed providers; a remote settings provider never needs to carry secrets.

## Model experience

Indirectly, through the consuming LLM adapters: a resolved value authorizes their provider requests, and the adapter owns every model-visible surface.

#### KV cache effect

No direct invalidation; credentials never enter a request prefix.

## Known limitations and deferred work

- **No enumeration** — the seam answers questions about references it is given; configuration surfaces learn the references from settings schemas, so a `list()` has no current consumer.
- **References are environment-variable-shaped** — one flat POSIX-identifier namespace until a provider needs richer addressing.
- **Process-environment changes are invisible** — no event can fire for them; a UI only re-reads `describe()` on its own navigation.
