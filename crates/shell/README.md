# seekdeep-shell

English | [中文](README.zh.md)

`ShellExecutor` defines what a shell backend does—run foreground commands and start background processes—without choosing how it does so. Job IDs, ownership, collection, cancellation notices, and model-facing schemas belong to consumers and providers outside this task-free capability seam.

The Rust port keeps the source split between four roles:

| Crate | Role |
|---|---|
| `seekdeep-shell` | Service definition, executor trait, process handles, and shared vocabulary |
| local/sandbox shell providers | Concrete subprocess execution and optional confinement |
| model-facing shell tools | Schemas and presentation over the `shell` service |
| jobs runtime | Long-lived task identity, ownership, polling, and notices |

Exactly one provider may occupy the typed `SHELL` service seat in a context. `ShellService::provide` fails loudly with the source-compatible duplicate-service diagnostic and returns a reversible lifecycle effect. `shell_settings_namespace()` returns the shared `shell` settings namespace owned by the capability rather than any one provider.

## Service API

| Member | Semantics |
|---|---|
| `resolve(request)` | Applies provider defaults and caps and returns a fully specified `ShellExecSpec`; provider validation failures are explicit errors. |
| `run(spec)` | Foreground execution. Infrastructure failures reject; nonzero exits, timeout kills, and caller abort kills resolve as `ShellRunResult`. |
| `start(spec)` | Starts background work and immediately returns a task-free `ShellProcess` handle. Background execution has no executor timeout. |
| `sandbox_mode()` | Capability fact for the consumer; the default returns `None` for an unsandboxed executor. |
| `ShellProcess::read_output()` | Consuming incremental read. Consecutive reads do not redeliver bytes; lossy reads expose spill paths when available. |
| `ShellProcess::kill()` | Kills the process group and returns `false` after it has settled. |
| `ShellProcess::done()` | Waits for close and never rejects; spawn failures are represented by a killed handle and captured stderr. |

Provider teardown owns all live processes it created: it must stop each running process and await close. Reloading only an executor may leave subprocess-owned handles running when the surrounding subprocess service remains live, matching the source lifecycle boundary.

## Vocabulary

`ShellExecRequest` contains the command plus optional workdir, timeout, stdout budget, abort signal, one-shot stdin, ordinary environment, trusted `SEEKDEEP_*` environment, and sandbox policy. `resolve` produces `ShellExecSpec` with required workdir, timeout, and stdout budget. Managed environment keys are validated newtypes, and session identity in a sandbox policy remains the `SessionId` newtype.

`ShellRunResult` carries exit code or an extensible signal newtype, first-cause timeout/abort flags, effective timeout, captured stdout/stderr, and optional sandbox facts. `ShellSandboxInfo` reports the actual mode, denial, enforcement completeness, and runner failure independently of command exit status. `CollectedOutput` contains the retained text tail, truncation flag, and optional complete-stream spill path.

The exported `parse_exit_status` is the shared inverse of the final `[exit code: N]` and `[killed by signal: X]` markers used by shell-tool renderers. It consumes only a newline-prefixed marker at the end of the string. Timeout and policy markers remain in the body because terminal presentation has no separate pill for them.

## Model Experience

This crate affects model input only through a named consumer such as a shell tool, which owns schemas, rendered guidance, and retained tool-result tokens. The seam itself does not alter request prefixes or KV-cache reuse.

## Known Limitations and Deferred Work

- **No interactive-input vocabulary** — stdin is written once at spawn and closed; there is no channel for later input and no PTY session concept.
- **Foreground deadlines are executor-owned** — the request carries a timeout plus caller cancellation; there is no separate caller-owned deadline mode at this seam.
- **Providers own OS details** — tree termination, environment scrubbing, spill mechanics, and confinement enforcement are provider contracts, not duplicated in this service-definition crate.
