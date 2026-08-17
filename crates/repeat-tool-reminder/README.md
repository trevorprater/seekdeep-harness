# seekdeep-repeat-tool-reminder

English | [中文](README.zh.md)

An advisory loop-breaker, not a model-facing tool: it never appears in the tool list, never vetoes or rewrites a call, and adds exactly one behavior. It watches each agent's stream of tool calls, counts runs of consecutive calls to the same tool with identical canonicalized arguments, and at configured run lengths injects an escalating advisory reminder telling the model to stop repeating itself, re-read the last result, and either change approach or conclude. The decision stays entirely with the model: a legitimately repeated call is delayed by nothing and blocked by nothing.

## Config

```yaml
- id: repeat-tool-reminder
  name: repeat-tool-reminder
  config:
    thresholds: [3, 5, 8]        # default; consecutive counts that trigger a reminder
    include: []                  # tool-name patterns to track; empty means all tools
    exclude: [todo_write]        # tool-name patterns transparent to the chain
    argumentsPreviewChars: 500   # default; cap on arguments quoted in a detailed reminder
```

`thresholds` fails loudly at plugin load: an empty list, a non-integer, a value below 2, or a duplicate returns an error rather than silently falling back to defaults. `argumentsPreviewChars` likewise accepts only an integer at least 1. Thresholds are normalized to ascending order. The first threshold delivers a short generic nudge; every later threshold delivers the detailed form naming the tool, run length, and canonical arguments. Its argument text is head-truncated at `argumentsPreviewChars` with an omitted-count marker, so a looping write/edit payload cannot enter the next request unbounded. Detection always compares the full canonical string.

`include` and `exclude` entries support `*` wildcards and are predicates over tools seen at call time, not references to registry entries. A pattern matching no currently registered tool is valid; for example, `exclude: [mcp_*]` remains legal in a deployment with no MCP tools.

## Chain semantics

The chain key is `(tool name, canonical arguments)`. Canonicalization performs a deep JavaScript-key sort followed by compact `JSON.stringify`-compatible serialization, so argument objects differing only in property order count as identical. A call identical to the previous tracked call increments the agent's consecutive counter; a different tracked call resets it to 1.

- **Untracked calls are transparent to the chain.** A call excluded by `include` or `exclude` neither increments nor resets the counter, so `grep X → todo_write → grep X` still counts as two consecutive `grep X` calls when `todo_write` is excluded.
- **Denied calls count.** Detection runs in `tools/post-execute`, which also runs for calls denied by a `tools/pre-execute` listener. A model repeatedly attempting a denied call is exactly the loop worth breaking.
- **Calls without an agent are ignored.** A direct `ToolRuntime::execute()` caller has no model to remind and no live agent object to key on.
- **Per-agent keying.** Tool calls from agents and subagents may interleave through one waterfall, so chains use weak ownership of each exact live `Agent`. One agent's repetition never trips another's reminder. A direct user message observed at `agent/pre-step` resets only the submitting agent's chain.
- **In-memory only.** A session resumed from persistence starts with a fresh chain. The guard is a heuristic nudge, not a logged invariant; later reminders are the accepted cost.

## Reminder delivery

Reminders ride the post-execute decision's `additional_contexts` with source `{kind: "plugin", plugin: "repeat-tool-reminder", form: "notice"}` and never replace result content. The durable `tool/result` remains the tool's own output for audit. The loop buffers the context and appends it as an injected `user/message` after the step's tool results, making the reminder model-visible, source-attributed, and reconstructable from the session log without a new event type. The guard always delegates and prepends its reminder to every downstream decision variant, including blocked calls; all downstream context sources and metadata survive.

## Model Experience

### First-threshold context message

At the first configured consecutive-repeat threshold, that agent receives this reminder. No tool schema or ordinary call text is added.

```markdown
You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.
```

The token effect is zero before the threshold. The reminder is retained history for that agent. It is append-only and follows the reusable request prefix, so it does not invalidate existing KV-cache entries.

### Later-threshold context message

Later thresholds receive this detailed template. A capped argument preview ends exactly `… (+<omitted> more chars)`.

```markdown
Repeated tool call detected:
- tool: <toolName>
- consecutive_calls: <count>
- arguments: <canonicalArguments>
The repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered.
```

Each reminder is retained history. `argumentsPreviewChars` bounds its data-dependent argument text, while agents keep independent counters. The content is append-only and does not invalidate the established request prefix.

## Known Limitations and Deferred Work

- **Exact-match detection only** — near-identical variants such as a changed path or whitespace within a value evade the chain; fuzzy matching is deferred pending evidence of need.
- **Compaction does not reset chains** — a chain spanning a compaction checkpoint keeps counting.
- **Advisory only** — escalation to a blocking policy at a high threshold is not implemented, though `PostToolDecision` supports it.
- **No subagent chain sharing** — a parent and subagent repeating the same call never combine their counts.
- **Legitimate idempotent polling still draws nudges** after the thresholds; `thresholds` and `exclude` are the pressure valves.
- **Past the highest threshold a chain goes silent** — reminders fire only at exact configured counts.
