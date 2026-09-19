# subprocess — subprocess capability family

English | [中文](FAMILY.zh.md)

The subprocess family is the shared process substrate for one execution world:
executable lookup, fully specified managed child-process trees with raw or
collected stdio, and one deep terminal primitive that owns PTY allocation,
foreground groups, and provider-observable session cleanup. Command defaults,
shell semantics, deadlines, protocol framing, readiness, and presentation stay
with consumers such as shell, LSP, terminal, and subagent backends.

| Crate | Context key | Role |
|---|---|---|
| [`seekdeep-subprocess`](README.md) | `subprocess` | Provider-neutral service definition: lookup, ordinary spawn, terminal allocation, handle lifecycles, and shared environment/output vocabulary |
| [`seekdeep-subprocess-local`](../subprocess-local/README.md) | — | Native local provider: isolated process trees, bounded collection/spill, native PTYs, foreground/session inspection, tree signalling, and terminate-and-join disposal |

The service owns process lifetime across consumer reloads. A consumer owns what
a process means—such as a shell command or protocol server—and every default
that shapes it. The concrete spawn specs, output readers, outcomes, and managed
`SEEKDEEP_*` environment contract are documented in the service and provider
references linked above.
