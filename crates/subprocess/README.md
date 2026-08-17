# seekdeep-subprocess

English | [中文](README.zh.md)

The provider-neutral subprocess seam is the process half of one execution
world. `SubprocessRuntime` exposes executable lookup, immediate ordinary
process spawning, and one terminal-process primitive. Its Rust vocabulary
covers raw and collected stdio, process and terminal handles, exit facts,
tree/session cleanup, and the managed `SEEKDEEP_*` namespace. A local native
provider is a separate porting slice.

## Service contract

`SubprocessService` publishes exactly one runtime on the typed `SUBPROCESS`
seat. A duplicate provider fails with `service "subprocess" has been
registered`; disposing its owning context withdraws that exact registration.
Providers implement these lifecycle rules:

- `spawn(spec)` returns immediately with a live handle. `done()` settles at
  direct-process close with only `SubprocessOutcome` exit facts; collected
  output and caller-owned cause classification are separate. Spawn-level
  failures are represented by a rejecting `done()` on the returned handle.
- Working directories and executable paths belong to the provider's execution
  world. `resolve_executable` verifies absolute paths or resolves bare names
  against that world's scrubbed PATH plus explicit lookup overrides.
- The spawn spec is fully explicit: argv, cwd, every stdio disposition, grace,
  cancellation, and optional environment. Nothing is shell-interpreted; a
  consumer wanting a shell supplies `bash -c` or its platform equivalent.
- `Pipe` exposes raw async streams, `Inherit` passes the parent descriptor, and
  `Collect` retains a bounded tail with optional full-stream spill recovery.
  `read_from` uses whole-stream byte offsets and never consumes shared state,
  so independent readers cannot steal deltas. Lossy reads report when the
  offset fell outside the retained window and retain an intact spill path when
  one exists. Readers remain valid after settlement.
- `terminate()` is the sole ordinary-process termination verb. Providers make
  it idempotent and tree-scoped, escalating TERM through the configured grace
  to KILL. The spec signal triggers the same reaction. `wait_for_exit` observes
  whole-tree liveness and lets a consumer bound the wait without assigning a
  timeout/cancellation cause to this seam.
- Providers terminate every still-running managed tree and await quiescence
  when their service lifecycle ends.

## Terminal primitive

`spawn_terminal` owns a real terminal allocation and returns a handle with
UTF-8 output, exact writes, foreground-process-group inspection, the closed
signal vocabulary (`SIGINT`, `SIGTERM`, `SIGKILL`, `SIGTSTP`, `SIGHUP`), and an
awaited idempotent session termination operation. The allocation signal applies
only until publication; afterward the handle owns the session lifetime. PTY
readiness, scrollback, and persistent-shell policy remain consumer concerns.

## Environment policy

`scrubbed_parent_env()` is the shared ambient base for all providers. It drops
keys containing `KEY`, `PASSWORD`, `SECRET`, or `TOKEN`, and every
`SEEKDEEP_*` key, case-insensitively. Ordinary execution facts such as `PATH`,
`HOME`, locale, and proxy configuration otherwise survive. A caller's explicit
environment merges afterward; `None` values are tombstones and explicit
credential or current managed values are deliberate opt-ins.

`seekdeep-shell` re-exports this crate's `CollectedOutput`,
`SeekDeepEnvironment`, key newtype, signal newtype, and managed prefix so the
subprocess seam remains their single owner.

## Model and cache effect

This seam has no direct model-visible content and no KV-cache effect. Consumers
such as shell executors own rendering, lifecycle descriptions, and request
prefixes.

## Limitations

- SDK-managed transports whose SDK owns its internal spawn cannot route that
  process through this service; they can still share the scrub function.
- The seam supplies signalling and whole-tree waiting, not one universal
  teardown ladder. Each protocol consumer owns its cooperation sequence.
- Platform process-tree and terminal-session observability belongs to the
  concrete provider and must be documented and tested there.
