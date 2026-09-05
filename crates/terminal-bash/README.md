# seekdeep-terminal-bash

English | [中文](README.zh.md)

Persistent shell backend for `seekdeep-terminal` over `SubprocessRuntime::spawn_terminal`. It starts an interactive shell under shared `seekdeep-sandbox-policy`, retains bounded line-oriented output, and detects readiness while the subprocess provider owns PTY allocation, environment scrubbing, foreground process groups, signalling, and complete terminal-session cleanup. The same backend composes with local or remote execution-world providers.

## Plugin (`terminal-bash`)

The plugin injects `terminals`, `sandboxPolicy`, and `subprocess`, then registers the configured backend type (`shell`). `danger-full-access` starts directly without requiring a sandbox provider; confined modes require a same-world `sandbox` service and wrap the exact shell argv through it, failing before spawn when none is mounted. One policy resolution supplies both effective mode and session workspace root; that root is the default cwd when omitted. A change to a different effective mode is rejected before its `sandbox/mode` event commits while that exact owner has an open PTY or spawn in progress. The fence outlives provider reloads retaining existing sessions, so a terminal opened with wider access cannot survive a downgrade.

Readiness combines a foreground-verified private Bash prompt marker, provider-reported foreground stdin-wait facts, silence fallback, and absolute timeout. A marker is not ready until the printable tail after the latest owned marker exactly equals controlled `PS1`, even when marker and prompt split across callbacks; echoed input or output following an earlier prompt cannot settle the current send. Prompt and silence evidence collected before provider write—including while pre-write inspection is pending—is discarded at the write boundary. When Bash prints the marker before the provider publishes its return to the foreground group, polling retains the candidate for `handoffGraceMs` beyond the ordinary silence bound so a coincident handoff can win. An interactive child inheriting `PROMPT_COMMAND` cannot suppress inferred-idle readiness until absolute timeout. Unknown foreground state is never positive exact-idle evidence. A stdin wait existing before a send is not post-write readiness: the same group must be observed outside that wait before a later wait can settle it, while a changed foreground group is new evidence. During unpublished startup, fallback requires observed output; zero-output silence cannot publish an empty session and timeout rejects spawn.

Cancellation closes an unpublished shell and rejects with the caller's exact abort reason; `TerminalBackendCleanupError` separately preserves cleanup failure. The caller signal is forwarded to allocation and readiness initialization; after publication the handle owns its lifetime. Initialization begins synchronously through reservation before its outer abort race becomes detachable, preserving first-prompt MOTD attribution. Incomplete terminal-control sequences are bounded by `maxReadBytes` and discarded through their terminator after crossing the limit; malformed UTF-8 uses replacement characters, and trailing carriage return carries across callbacks so split CRLF becomes one newline.

Send cancellation marks queued input cancelled before asking the handle to signal the current foreground group with a real `SIGINT`; later pre-write inspection cannot execute that input. If a provider write is in flight, signalling waits for it; rejected write sends no signal. The canceled send retains its exclusive slot until write and signal settle, so a successor receives neither late bytes nor the signal. A write or signal that never settles retains the slot indefinitely; closing is recovery. The absolute deadline remains armed while cancellation waits. Signal failure is a terminal transport failure. Cancellation never emulates interruption with `\x03`, so raw-mode programs remain cancellable. Close rejects new public signals, stops readiness polling, and awaits provider-owned complete-session termination before settling the active send as `session_exit`.

## Model experience

The policy owner contributes capability-neutral `sandbox:policy` context. Through a terminal tool or another PTY consumer, the model may receive bounded MOTD, send deltas, scrollback pages, readiness reasons, and cleanup errors. Retained scrollback does not enter model history until a consumer returns bounded output. Policy changes append a superseding runtime-context snapshot; consumer results remain append-only.

## Known limitations and deferred work

- Line-oriented output is normalized; full-screen alternate-buffer interaction is unsupported.
- Exact stdin-wait detection depends on the subprocess provider; providers unable to prove it use prompt-marker and silence/timeout readiness.
- Cleanup guarantees are those of `SubprocessTerminalHandle`; provider-specific gaps belong to that implementation.
- Sessions do not survive the SeekDeep Harness process exiting.
