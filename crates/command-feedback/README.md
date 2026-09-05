# seekdeep-command-feedback

English | [中文](README.zh.md)

Model-agnostic `/feedback` command recording human remarks on a session.

- Records an append-only, log-only `feedback/record` session event carrying the trimmed free-text remark. The event is authoritative; it never enters model context, the ordered surface, or derived history, and carries no `surfaceOp`.
- Acknowledges with the receiving session id and the anonymous user id. The session-sharing disclosure currently reports "not configured" until the `session-telemetry` crate is ported.
- Rejects empty or whitespace-only input as a failed command record without recording an event or performing the user-id lookup.

## Model experience

Zero token and KV-cache effect: neither accepted feedback nor a usage error touches the model request path.

## Limitations

No retrieval/management surface, no structured fields, no amend or withdraw, and no explicit durability barrier; the acknowledgement follows the append rather than a flush.
