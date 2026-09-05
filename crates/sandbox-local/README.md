# seekdeep-sandbox-local

English | [中文](README.zh.md)

Native local provider for the `seekdeep-sandbox` seam. It selects a platform ladder once per provider lifetime: Linux probes bwrap then the sibling Rust `landlock-run`; macOS selects Seatbelt (`sandbox-exec`) directly; Windows selects the restricted-token ACL runner directly. A platform with no ladder or a multi-rung ladder with no successful probe fails closed with `SANDBOX_UNAVAILABLE`.

Each wrap carries its exact enforcement level, backend denial dialect, and structured runner-failure rules. A configured operator runner uses the bwrap-shaped profile, requires its own non-empty single-line fatal signatures, skips all built-in probing, and asserts full enforcement. Functional probes are positively bounded and cached; a zero timeout is rejected because it would mean unbounded execution in the source runtime.

Profiles preserve backend-specific semantics: bwrap provides a read-only host tree and ephemeral `/tmp`; Landlock grants read/execute on `/`, write to `/dev/null`, and under workspace-write the host `/tmp` plus workspace; Seatbelt denies every file write except `/dev/null` and canonical deduplicated writable roots.

On Windows, the provider delegates every ACL and restricted-token operation to compiled Rust. It materializes one deterministic standing workspace grant per workspace and one random, distinct, revocable private-temp capability per live session/workspace pair. Repeated calls reuse both, forks and workspace changes receive distinct temp capabilities, and a fresh provider never reuses crash residue. Read-only and agentless calls carry no capability SID; an agentless workspace-write runner owns its one-shot private-temp lifecycle. Provider teardown revokes all temp ACEs, removes all provider-owned temp directories, releases every parsed SID, preserves standing workspace ACEs as the cross-process reuse cache, and reports cleanup failures without aborting teardown. The rung remains `partial` because the underlying Windows `WRITE_RESTRICTED` mechanism retains its documented Everyone and hard-link boundaries.
