# seekdeep-tmux-context

English | [中文](README.zh.md)

Opt-in durable context naming the tmux session, window, and pane in which the
agent process runs, plus the window's pane-tree layout. The plugin samples once
per turn during model-request preparation. It is not part of the shipped Web or
headless composition.

## Config

```yaml
- id: tmux-context
  name: seekdeep-tmux-context
  config:
    refreshIntervalMs: 60000 # optional; omit or use 0 for every changed turn
```

`refreshIntervalMs` must be a non-negative JavaScript-safe integer. Omission or
`0` queries every eligible turn and injects only when the tmux state changed. A
positive value also suppresses queries within that many milliseconds of the
latest durable injection.

## How it reads tmux

The plugin prepends an `agent/pre-step` listener and runs only after downstream
listeners enter the first step. When due, it asks the optional `shell` service
to run one read-only command:

```sh
[ -n "$TMUX_PANE" ] || exit 1
self_tty=$(ps -o tty= -p <pid> | tr -d ' ')
[ -n "$self_tty" ] || exit 1
pane_tty=$(tmux display-message -t "$TMUX_PANE" -p '#{pane_tty}') || exit 1
[ "$pane_tty" = "/dev/$self_tty" ] || exit 1
exec tmux display-message -t "$TMUX_PANE" -p '<format>'
```

The tty check rejects processes that merely inherited `$TMUX_PANE` from a tmux
ancestor. The plugin owns no subprocess implementation: execution inherits the
deployment's shell sandbox and policy. A missing shell service, non-tmux tty,
nonzero exit, empty pane id, or malformed reading is a silent no-op. Resolver or
executor failures are contained and logged as warnings because location is
optional.

The command reports session and window identity, pane identity, active flags,
and `window_layout`. It never captures sibling-pane contents or pixel sizes.

## Timing and persistence

On a successful changed reading the plugin prepends one sourced `UserMessage`.
The agent loop records it after `step/start` with source
`{ kind: "plugin", plugin: "tmux-context" }`. Change suppression and interval
scheduling scan the raw append-only session events, so they survive compaction
and process resume and remain independent per session. A downstream rejection,
failure, cancellation, or a later step records nothing.

The model-visible text is:

```text
tmux location (turn <turn>):
session <session>, window <index> "<name>", pane <index> <pane-id>
window active=<0|1>, pane active=<0|1>, layout <window-layout>
```

Each changed reading appends until compaction shadows it. Unchanged state and
interval suppression add no tokens and no new KV-cache prefix invalidation.

## Limitations

- State changes during a turn appear on the next turn, not between steps.
- A window name containing the literal two-character sequence `\t` makes the
  tab-delimited response malformed and therefore skipped.
- Tty-based detection intentionally excludes terminals that inherited tmux
  environment variables without sharing the pane's controlling terminal.
- `ps -o tty=` is POSIX; unsupported environments produce a no-op.
