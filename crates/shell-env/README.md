# seekdeep-shell-env

English | [中文](README.zh.md)

The tool-independent managed shell-environment plugin. It owns the
`shellEnv` service: a registry of trusted, per-execution `SEEKDEEP_*` values
that model-facing shell tools collect into foreground and background calls.
Built-ins belong to the registry; plugins can add effect-scoped declarations.
Duplicate ownership, malformed declarations, undeclared runtime keys, and
non-string runtime values fail before a snapshot escapes.

The crate exports the Loader plugin (`plugin`), direct installer (`apply`),
`ShellEnvRegistry`, contributor vocabulary, the `SHELL_ENV` typed service seat,
and the explained-empty invariant companion.

## Config

```yaml
- id: shell-env
  name: seekdeep-shell-env
  config:
    seekdeepHome: C:\Users\me\.seekdeep # default: $SEEKDEEP_HOME, then ~/.seekdeep
```

The Loader accepts null as default configuration, validates `seekdeepHome` as
a string before plugin effects begin, and tolerates forward-compatible unknown
fields like the source object schema.

## Managed environment

Every call receives a newly collected, immutable, lexically sorted overlay:

- `SEEKDEEP_HOME`: absolute harness home, using explicit config, then the
  nonblank ambient variable, then the operating-system home plus `.seekdeep`.
- `SEEKDEEP_SHELL=1`: identifies a managed child.
- `SEEKDEEP_SESSION_ID`: exact live agent session id; absent for agentless work.
- `SEEKDEEP_SESSION_JSONL`: absolute target path when the current optional
  persistence provider reports a `jsonl` location for the live agent.

The JSONL path is a location hint, not a credential. It can precede the first
flush and need not contain the currently buffered turn.

Contributors declare a stable name, insertion-ordered keys and descriptions,
and a synchronous resolver over the exact `ToolExecution`. Registration checks
the exact `SEEKDEEP_` prefix, uppercase suffix grammar, reserved built-ins,
blank descriptions, duplicate names, and duplicate owners atomically. The
returned effect is an explicit disposer and is also owned by the registering
Cordis context. `list()` sorts declarations by key without running resolvers.

Collection snapshots contributors, runs them in name order, verifies every
returned key was declared and every value is a string, then produces a
`SeekDeepEnvironment`. The local executors will use the dedicated
`ShellExecRequest.seekdeep_env` channel to discard inherited managed keys before
merging the trusted snapshot; this registry never mutates the parent process
environment.

## Model and cache effect

The plugin has no direct model-visible content and does not invalidate a KV
cache prefix. Shell-tool consumers decide how the generic `$SEEKDEEP_*`
convention appears in their descriptions and requests.

## Limitation

`list()` intentionally reports contributor declarations only. Registry-owned
built-ins (`SEEKDEEP_HOME`, `SEEKDEEP_SHELL`, and `SEEKDEEP_SESSION_ID`) are not
included, so diagnostics and UI code must not treat it as an exhaustive catalog.
