# seekdeep-sandbox

English | [中文](README.zh.md)

Process-sandbox service definition. This crate owns the `sandbox` Cordis service contract (`SandboxProvider`) and the confinement vocabulary SeekDeep Harness shares: `SandboxMode` (`read-only` / `workspace-write` / `danger-full-access`, file effects only), `SandboxEnforcement` (`full` / `partial`, per kernel ABI), `SandboxExecutionPolicy` (the complete per-call mode and workspace root), `SandboxPolicy` (its confined subset), and the fail-closed `SANDBOX_UNAVAILABLE` error. It depends on provider-neutral seams, never on a concrete backend.

The contract in one line: `SandboxProvider::confine(argv, policy)` returns the argv to spawn instead of the caller's own—wrapped so the process and everything it spawns run confined—plus the selected backend's enforcement completeness, denial dialect (`denial_signatures`), and structured runner-failure evidence (`runner_failure_rules`). When no backend is usable it returns an error instead of passing argv through unconfined.

Policy rides the call, not the provider: two consumers may confine under different policies at the same instant (Bash under `read-only` while a confined child agent keeps its state directory writable), and an approved escalated retry is a new call with a wider policy.

**Same-world confinement only.** A backend shares the host filesystem and kernel (`bwrap`, Landlock, Seatbelt); `workspace_root` names the filesystem-canonical real host directory. Workspace identity is resolved before lexical normalization, so a valid cwd containing `symlink/..` grants the directory where `chdir` actually lands rather than an unrelated lexical parent. Containers, microVMs, and remote executors are not backends of this seam: they replace whole capability providers such as shell and filesystem as environment-coherent groups.

## Model experience

Consumers surface code `SANDBOX_UNAVAILABLE` and the exact error below when a requested mode cannot be enforced. An execution-time runner failure appends ` Runner failure: <detail>`.

```markdown
sandbox mode "<mode>" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access.
```

The conditional error text is visible for that call and retained until compaction. It is append-only, following the reusable request prefix without invalidating existing KV-cache entries.

## Known limitations and deferred work

- **File effects are the whole policy vocabulary**—the seam expresses no network, process, syscall, device, or credential restrictions.
- **Same-world confinement only**—containers, microVMs, and remote execution require replacing capability implementations rather than adding a provider here.
- **Denial reporting is a stderr dialect**—the seam returns backend signatures instead of a typed runtime denial channel, so consumers infer classification from child output.
- **Runner diagnostics are in-band**—exit status plus stderr evidence cannot prove which process wrote a matching line. A confined child mimicking its runner can cause availability or diagnostic false attribution, but cannot bypass confinement. An out-of-band runner-status channel is deferred.
- **One provider per context**—simultaneously composing different mechanisms requires a provider-level ladder or separate Cordis contexts; callers choose policy per call, not backend identity.
