# seekdeep-launch-environment

English | [中文](README.zh.md)

An immutable snapshot of one SeekDeep launch environment that records which
layer supplied every value. Consumers resolve user-facing configuration through
this snapshot instead of a flattened process environment because the layers
have different trust and provenance.

| Layer | Rust variant | Meaning |
|---|---|---|
| Inherited process environment | `Process` | Explicit intent from the launching shell, CI job, or container |
| `<invocation cwd>/.env` | `ProjectEnv` | Configuration owned by the project in which SeekDeep was launched |
| `$SEEKDEEP_HOME/.env` | `UserEnv` | User-level machine defaults |

Launchers may also materialize accepted values into the process environment for
configuration expressions and third-party libraries. That flattened view is
not authoritative for harness-owned resolution.

## Resolution

`LaunchEnvironmentSnapshot::get(name)` searches all layers in canonical trust
order. `get_from(name, sources)` searches only allowed layers while retaining
that canonical order, regardless of the slice's order. Omitting a layer is a
refusal rather than a demotion: it cannot win through caller reordering.

Names match platform semantics: exact on POSIX and case-insensitive on Windows.
This prevents a differently cased project variable from outranking the same
variable inherited from the launching process.

```rust,no_run
use seekdeep_cordis::Context;
use seekdeep_util::launch_environment::launch_environment_of;

let context = Context::new();
let endpoint = launch_environment_of(&context)
    .get("DEEPSEEK_BASE_URL")
    .map(|entry| entry.value);
```

`launch_environment_of(context)` returns the exact launcher-provided snapshot.
If no product launcher booted the composition, it freezes the inherited process
environment as the sole layer. An SDK host or bare composition discovered no
environment files, so every available value genuinely came from its process.

Layer input is copied at construction. Later map mutation, `chdir`, workspace
selection, or session resumption cannot change the launch snapshot. Empty
strings remain present so the owning consumer decides whether they are valid.
Repeated inputs for one source use the last supplied layer.

## Known limitations

- The snapshot is not a subprocess boundary. Materialized values can reach
  child processes subject to the subprocess scrub policy.
- There is no per-workspace layer. The project layer is the invoking directory
  fixed at launch; a model-selected workspace cannot mutate the harness
  environment mid-session.
