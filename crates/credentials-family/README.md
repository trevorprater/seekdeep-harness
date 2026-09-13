# credentials — credential references

English | [中文](README.zh.md)

The credential capability family separates reference resolution from its provider:

| Crate | Role | Context key |
|---|---|---|
| [`seekdeep-credentials`](../credentials/README.md) | Credential-reference seam | `credentials` |
| [`seekdeep-credentials-local`](../credentials-local/README.md) | Environment and local-file provider | registers `credentials` |

Configuration carries references, not secret values. Consumers resolve those references at their operation boundary; the child READMEs own mutation, precedence, and storage semantics.

The subsystem reference—`CredentialRef`, per-operation resolution, UI-safe `CredentialInfo`, and provider layers—is [the credentials subsystem guide](../../docs/subsystems/credentials.md).
