# seekdeep-sandbox-policy — the sandbox policy home (`sandboxPolicy`)

English | [中文](README.zh.md)

The single owner of sandbox-policy resolution: the deployment's default `SandboxMode` and fallback root, plus each session's durable mode override and immutable workspace root. Every enforcing capability receives one resolved mode-and-root policy per call; before each request, the model receives current policy without a separate capability inventory.

## Why a shared home

Filesystem tools, one-shot Bash commands, and terminal sessions may enforce the same vocabulary in different combinations. If each resolved its own `mode` and `workspace_root`, they could drift into a split world. Each backend consumes the complete owner-resolved policy, while current context describes only what that policy means for any available operation the SeekDeep file sandbox enforces.

## Config

- `mode`—the deployment default (`read-only` / `workspace-write` / `danger-full-access`), validated at load. Default: `read-only`.
- `workspaceRoot`—the fallback directory `workspace-write` may modify for agentless calls or sessions without a cwd. Default: the process cwd, resolved to absolute filesystem identity. A normal agent call uses its session header's immutable `cwd`.

## API

- `SandboxPolicyService::resolve(SandboxPolicyRequest { session, mode })` resolves a complete policy. An explicit approved mode outranks the last `sandbox/mode` event, which outranks `default_mode`; immutable session cwd is canonicalized with filesystem semantics before becoming `workspace_root`, otherwise the configured fallback applies. Canonicalization precedes lexical normalization so `symlink/..` agrees with process working-directory resolution.
- `default_mode` and `workspace_root` are deployment fallbacks.
- `sandbox:policy` is request-time cache-safe context derived directly from `resolve`. It states the capability-neutral file-effect contract and canonical workspace under `workspace-write`; tool owners retain operation-specific denial and escalation guidance.
- `effective_sandbox_mode(events)` is the pure fold of `sandbox/mode` events: the last switch wins, or no override exists.
- `set_sandbox_mode(session, mode)` is the only write path and appends exactly one `sandbox/mode` event. The switch is its event; nothing mutates mode out of band.
- `SANDBOX_MODES` advertises every closed mode.

The invariant companion rejects a forged durable `sandbox/mode` outside the vocabulary, both during replay and before a live append commits. The agent loop logs assembled runtime context as a sourced user message, so exact policy input remains reconstructable without an in-memory “last told” mirror.

## The per-session store

A runtime switch is one log-only event on its session. `effective = explicit grant ?? fold(events) ?? deployment default`, so an override survives restart by replay and two sessions never see each other's state. Workspace identity needs no second event: immutable `SessionHeader.cwd` is the root for every call in that session.

## Model experience

One `sandbox:policy` contribution appears in current runtime context for every agent session and does not enumerate mounted capabilities.

### Read-only

```markdown
Current SeekDeep file policy: read-only. Any available operation enforced by the SeekDeep file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns.
```

### Workspace-write

```markdown
Current SeekDeep file policy: workspace-write. Any available operation enforced by the SeekDeep file sandbox may modify files under the session workspace: "<workspace root>". Some platform temporary areas may also be writable.
```

### Danger-full-access

```markdown
Current SeekDeep file policy: danger-full-access. The SeekDeep file sandbox does not restrict file modifications by available operations.
```

The first request and each effective policy change add one concise durable context message; unchanged requests add nothing. The stable system prompt remains byte-identical across changes, and the new snapshot is appended after retained history so prior KV-cache prefixes remain reusable.

## Known limitations and deferred work

- **One primary workspace root per session**—extra writable roots are not part of `SandboxExecutionPolicy`.
- **File-effect modes only**—network and process policy are outside this vocabulary.
- **Temporary areas are deliberately summarized**—backends select platform temp areas after policy resolution, so current context cannot enumerate them truthfully.
