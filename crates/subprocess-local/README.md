# seekdeep-subprocess-local

English | [中文](README.zh.md)

Native local provider for the [`seekdeep-subprocess`](../subprocess/README.md)
capability seam. `LocalSubprocessRuntime` resolves local executables, spawns
ordinary isolated process trees with explicit stdio, and implements terminal
processes through a native PTY plus platform process inspection. It has no
independent configuration: every disposition, limit, terminal dimension,
grace, environment, and directory arrives in the calling capability spec.

## Behavior

- **Platform-correct process trees.** POSIX children own a process group and
  are signalled by negative pgid with a direct-child fallback. Windows uses
  `taskkill /PID <pid> /T /F`. `terminate()` sends TERM and escalates to KILL
  after the spec grace, is idempotent, and stops signalling after the tree is
  confirmed absent. `wait_for_exit()` observes the complete owned tree rather
  than only the leader. After a leader exits, the same grace bounds collected
  pipe draining so an inheriting descendant cannot hold `done()` forever.
- **Source-shaped stdio.** On POSIX, connected child descriptors use Unix
  socket pairs, matching Node child-process fd types; ignored stdin is the null
  character device. `Pipe` exposes the raw Tokio stream, `Inherit` passes the
  parent descriptor, and `Collect` retains the exact in-memory tail. With a
  spill cap, the complete stream is appended to a randomly named `0600` file
  below a lazily created `0700` per-process directory. Exceeding the spill cap
  invalidates and removes the incomplete spill. Close or cleanup faults are
  contained, and a final-close fault withholds the path rather than advertising
  an unreliable file.
- **Credential scrub and explicit merge.** The ambient environment loses
  credential-shaped names (`KEY`, `PASSWORD`, `SECRET`, or `TOKEN`) and every
  ambient `SEEKDEEP_*` name. Explicit spec values merge afterward, so deliberate
  credentials and current managed facts win; explicit `None` values are
  tombstones. Batch stdin is written and closed best-effort, while the child
  outcome remains authoritative if it exits without reading.
- **Offset reads.** Collected readers use whole-stream byte coordinates and
  hold no shared cursor. Independent incremental readers and full rereads can
  coexist before and after settlement. A read reports `lossy` when its offset
  has slid outside the retained tail and includes the complete spill path only
  while that spill remains trustworthy.
- **Executable lookup.** Absolute paths must name executable files. Bare names
  search the scrubbed effective PATH, including relative PATH entries resolved
  from the host cwd and case-insensitive `PATH`/`PATHEXT` handling on Windows.
  Relative command paths containing a separator fail at the seam.
- **Terminal-session ownership.** `spawn_terminal()` allocates a real PTY,
  bridges UTF-8 terminal text, inspects and signals the foreground process
  group, and exposes one joined, retryable termination operation. Cleanup
  captures exact pid/start identities, sweeps rooted descendants and observable
  POSIX session members before and after stopping the shell, and never adopts
  children from a recycled root pid. A failed automatic cleanup remains in the
  runtime live set for later normal disposal or host-exit force cleanup.
- **Terminate-and-join disposal.** The provider retains ordinary and terminal
  handles through real quiescence. Disposal starts every termination first,
  awaits every target, force-stops remaining targets after failures, clears
  ownership only after those attempts, and preserves one failure directly or
  reports the stable aggregate message `local subprocess teardown failed`.
- **Synchronous host-exit finalization.** While the service effect is active, a
  narrow native `atexit` bridge force-terminates all still-owned ordinary trees
  and observable terminal sessions without timers or async work. Every target
  failure is contained so later targets still run, the host exit status remains
  unchanged, and normal disposal reversibly removes the runtime registration.

## Model experience

The provider has no direct model-visible surface. Consumers such as shell, LSP,
and terminal executors own rendering, lifecycle classification, and request
prefix changes. Consequently this crate has no direct KV-cache invalidation.

## Known limitations

- Windows tree liveness uses the direct-child boundary after contained
  `taskkill /T /F`; terminal process inspection is implemented only for Linux
  and macOS. Linux exact syscall probes cover x86-64 and AArch64.
- A terminal descendant can escape if it becomes unobservable before capture:
  on macOS by reparenting before a rooted snapshot, or on Linux by leaving both
  the rooted tree and owned session with `setsid`.
- In-process finalization requires a path that runs native `atexit` callbacks,
  such as normal Rust termination or `std::process::exit`. `SIGKILL`, abort,
  fatal runtime failure, native crash, power loss, and other paths that cannot
  execute callbacks require an external supervisor or equivalent OS owner.
- Credential scrubbing is a name heuristic. Differently named secrets such as
  `PASSPHRASE` remain ambient unless the caller removes them.
- Completed bounded spill files and their private directory are not deleted by
  the provider. Oversize incomplete spills are removed best-effort; a cleanup
  failure can leave a bounded file behind.

The ordinary-process implementation is in `src/spawn.rs`; `src/lib.rs` owns
service wiring, `src/terminal.rs` owns PTY sessions, and
`src/process_inspector.rs` owns platform inspection.
