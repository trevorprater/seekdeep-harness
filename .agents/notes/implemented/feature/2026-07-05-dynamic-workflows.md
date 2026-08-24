# Agent Note: Dynamic workflows — a script-driven multi-agent orchestration seam

Status: implemented

English | [中文](2026-07-05-dynamic-workflows.zh.md)

## Problem

The harness can delegate ONE task to ONE child (`seekdeep-tool-subagent`), but work that fans out across many independent pieces — an audit over many files, a migration, multi-angle research, adversarial verification of findings — forces the model to orchestrate turn by turn: every intermediate result lands in the parent context, the plan lives nowhere durable, and coordination costs a model round-trip per step. Claude Code ships this capability as [dynamic workflows](https://code.claude.com/docs/en/workflows): the model writes a JavaScript orchestration script, a runtime executes it, and the script — not the conversation — holds the loop, the branching, and the intermediate results.

## Decision

A workflow capability family at `packages/workflow/` in the bash seam shape (Service Definition / Service Provider / Consumer), plus the structured-output foundation it needs on the subagent seam.

### The script contract (Claude Code-compatible)

A workflow call contains JSON `meta` (`name`, `description`, and optional `whenToUse`/`phases`) and a JavaScript `script` body with top-level `await` that returns a JSON value. Metadata is validated as data and never evaluated. The body receives `agent(prompt, options)`, `parallel(thunks)`, `pipeline(items, ...stages)`, `phase(title)`, `log(message)`, and `args`. Pipeline stages receive `(prev, item, index)` with no cross-stage barrier; failed children and ordinary stage errors resolve the affected item to `null` and skip its remaining stages. Claude Code's determinism restrictions are deferred with journaling, so compatible bodies may use clock and randomness after moving their meta header into the parameter.

One deliberate strictness DIVERGENCE from CC: hook misuse — unknown or deferred options (`effort`/`isolation`/`agentType`), malformed arguments, schemas outside the supported subset, tripped caps, seam start failures — throws a `WorkflowError` with `fatal: true`, and the combinators RE-THROW fatal errors instead of nulling the item. Without this, a typo'd option dissolves into a `null` indistinguishable from a child failure — the accepted-then-ignored failure mode this repo bans. One addition: the tool's `args` parameter is a JSON OBJECT (a bare list is wrapped as a field) so the wire schema stays honest.

### The seam (seekdeep-workflow)

`ctx.workflowEngine` is an abstract `WorkflowEngine` in the bash shape — one engine per context, no named-provider registry (engines are deployment swaps, not co-residents). `start(request)` throws synchronously for a script that cannot begin; a returned `WorkflowRun`'s `result` NEVER rejects (failures resolve as `stopReason: 'error' | 'cancelled'`). The `workflow/*` events are observe-only emits carrying DATA SNAPSHOTS (id + meta; `workflow/end` omits the result value), per-listener contained, mirroring `subagent/start`/`subagent/end` — control stays with the run's holder. Vocabulary details: [subsystems/workflow.md](../../../../docs/subsystems/workflow.md).

### The engine (seekdeep-workflow-worker-thread): one killable Rust helper process per run

**Trust premise**: workflow scripts have the same trust as the model's bash access. The Rust-owned Boa evaluator exposes only the documented workflow globals; it exposes no Node `process`, `require`, ambient environment, or credential API. The helper process contains buggy scripts, guarantees settled results and JSON-safe values, and gives cancellation a real OS termination boundary. It shares the host user's filesystem and kernel authority, so it is containment rather than a complete sandbox.

**Why Boa in a compiled helper process**: production execution stays compiled Rust while preserving the model-authored JavaScript body. Each run owns one unpooled process, a loop-budgeted Boa context, a root-promise-aware async job executor, and a typed child bridge over newline-delimited JSON stdio. Synchronous guest work cannot block the host; nested `parallel()`/`pipeline()` jobs wake the executor without polling; cancellation rejects queued work and drives published children to disposal.

The host validates metadata and parses the body before publication. It owns pending starts, idempotently disposed child records, one cancellation signal, first-terminal-result precedence, exact start/end pairing, and `disposeGraceMs`-bounded caller settlement. A late provider result is refused and disposed; worker death synthesizes missing cancelled ends before `workflow/end`. Closed message enums define both directions of the stdio protocol; unknown call ids are accepted no-ops during teardown races. The [agent-scope runtime-design Agent Note](../architecture/2026-07-12-agent-scope-runtime-design.md#workflow-children-are-pending-starts-or-published-records) owns the child-race algorithms.

Release composition supplies the worker either as the sibling `seekdeep-workflow-worker` executable or as the private worker mode of the installed `seekdeep` binary; engine construction fails when neither killable entry is available. Development builds may use the same in-process typed evaluator when no compiled entry exists. Focused tests pin all three paths.

**Meta is data**: the schema-validated `meta` field reaches the seam as JSON and is only shape-validated. The host never evaluates a metadata literal, which would let script-controlled accessors run outside the worker's isolation.

**Value boundary**: `materializeFromRealm` copies outbound values and rejects functions, symbols, nested `undefined`, exotic prototypes, cycles, sparse arrays, and non-finite numbers. Data-property copies make `"__proto__"` safe; getters are read normally and a throwing getter fails loudly. `args` crosses through `workerData` and is cloned again before exposure. Realm functions are invoked rather than copied, and thrown values use a total renderer so `result` cannot reject. Hook errors are host-realm `WorkflowError`s, so scripts branch on `name` or `code` rather than `instanceof Error`, as documented in the engine README. Concurrency, total-agent, item, timeout, and grace limits are validated config.

### The Consumer (`seekdeep-tool-workflow`)

A `workflow` tool mirroring `seekdeep-tool-subagent`'s synchronous shape: start, await, `try/finally` dispose, abort-bridge `exec.signal`, non-`completed` → `isError`. Render intent: a `generic` card titled by the call's `meta.name` parameter (presentation is a pure function of args). The tool description IS the model-facing authoring spec. The usage policy ships with the tool as its own `tool:<toolName>` prompt section (explicit-ask-only guidance — tool guidance lives in tool plugins, never in the deployment persona); the harness has no ultracode-style effort gate.

For a top-level tool execution, the same consumer also writes the run and actual member lifecycle into the calling parent Session as four log-only `tool-workflow/*` events. The recording path observes rather than controls execution: its first append failure disables later writes for that run and leaves a legal prefix without changing the tool result. [`ui-workflow-run`](../../../../packages/client/ui-workflow-run/README.md) rebuilds those facts through the Conversation Node engine as a separate keyed Chat row; the existing generic tool row remains its own presentation owner. The detailed persistence, replay, disclosure, and live-navigation decision lives in [durable workflow runs in Chat](2026-08-10-durable-workflow-runs-in-chat.md).

### The foundation: structured output on the subagent seam

`SubagentStartRequest.outputSchema` is implemented by `seekdeep-subagent-in-process-driver` for both in-process backends. Each structured child receives its own scoped capture tool, instruction, and enforcement registrations on `child.ctx`; concurrent children can use different schemas without sharing mutable policy, and disposing the child removes the entire attachment.

An output schema makes a schema-valid committed capture mandatory for successful child completion. The scoped runtime presents the capture tool and instruction, commits only a successful final outcome—including the enclosing `run_code` outcome for an SDK call—denies later side effects after capture becomes pending, and stops the child without another model step after commit. A validation failure remains a retryable tool error; clean completion without a committed capture settles as an error.

`ObjectJsonSchema` is the object-rooted consumer view of the unified enforceable raw JSON Schema subset in `seekdeep-tools`; unsupported keywords fail loudly because that wire data becomes the capture tool's parameters verbatim. The [unified JSON-value schema Agent Note](../architecture/2026-07-20-unified-json-value-schema-dsl.md) owns the vocabulary and validation semantics, while the [agent-scope runtime-design Agent Note](../architecture/2026-07-12-agent-scope-runtime-design.md#structured-output-commits-only-authoritative-outcomes) owns the assembly, commit, guard, and terminal-stop algorithms.

## Testing

Worker-side logic runs through typed in-process child ports and the same closed protocol used by the compiled stdio worker. Host tests independently drive pending starts, late readiness, slow or failed disposal, process death, grace termination, external signals, HMR unload, and lifecycle pairing. Unit tests cover script helpers, unforgeable fatal errors, nullable failures, JSON boundaries, caps, cancellation, child ownership, and structured output through real loops. Scrubbed-environment smokes drive the standalone worker and the installed single-binary worker mode; live-provider execution remains a separate parity-manifest boundary.

## Deferred (documented non-goals)

- **Background collection** (start tool → run id → completion notice → collect), designed alongside shell/subagent background unification.
- **Journaling + resume** (`resumeFromRunId`, cached agent() prefixes) — implementing it reintroduces CC's determinism bans as a script-contract tightening (scripts may read the clock today).
- **Saved/bundled workflows** (a `.deepseek/workflows/` registry, slash-command API) and **script persistence to a run directory** (the tool-call event already records the script durably).
- **Nested `workflow()`**, **token `budget`**, and the `effort`/`isolation`/`agentType` agent options (each rejects loud with a message naming it deferred).
- **An overall run wall-clock timeout** — cancellation always frees the caller (result settles within the grace), so a cap on total run time is a policy knob for the background redesign, not a correctness need here.
- **Stronger isolation beyond the helper process**: OS resource limits, a dedicated sandbox profile, or an epoch-interruptible component runtime can add memory and syscall policy. The helper already supplies hard termination but deliberately retains the host user's ordinary workspace authority.
- **ACP-backend structured output** and **`toolFilter`** (both still capability-gated `false`).

## Alternatives considered

- **Hostile-value containment in the host** (trap-free proxy rejection, accessor-never-invoked descriptor walks, realm-side pre-rendering of thrown values, realm-built promises/arrays/error clones with structural fatal recognition): rejected because every defense targets an author the trust premise accepts, while the thread's serialization boundary already makes cross-realm values total by construction.
- **Run Boa on the host thread**: mechanically simplest, but `start()` would let synchronous guest work block the caller and would entangle the guest scheduler with host progress. The helper process preserves the JavaScript API, isolates scheduling, scrubs ambient state, and makes grace termination real. A development-only in-process fallback exists for focused builds, not release composition.
- **Background execution as the default** (CC's shape): deferred; foreground-synchronous matches `seekdeep-tool-subagent`'s cut, and background semantics should be designed ONCE across shell/subagent/workflow rather than per-tool.
- **Workflow-layer JSON parsing for `agent({schema})`**: duplicating a seam concern at one consumer while the seam's capability flag stayed dishonestly `false`.
- **Meta embedded in the script as `export const meta = {...}`** (CC's exact format): keeps scripts self-contained and CC scripts drop-in, but obtaining meta requires evaluating model-written text on the host. Even an empty timed vm context cannot bound script-controlled getters when the host reads the resulting object. A JSON parameter removes the scanner, evaluation, and host-spin hole; the cost is that a CC script's meta header must move into the parameter (the body stays drop-in).
- **`ValueSchemaSpec` as the `outputSchema` wire type**: the author form now has equivalent vocabulary, but a workflow supplies realm-foreign raw JSON Schema data; pretending that runtime data is a trusted author declaration would skip the raw-schema assertion boundary.
- **A schema-object library (zod, or the repo's schemastery) for the structured-output subset**: the schema is wire data — plain JSON that crosses the vm realm boundary in `agent({schema})` and lands verbatim in the forced tool's parameters — exactly where live schema objects cannot sit; consuming raw JSON Schema at runtime would need a third-party converter on top (zod core only emits JSON Schema, not the reverse), and it would put a second schema language beside schemastery's config role.
- **ajv for value validation**: it validates FULL JSON Schema, so the subset gate — the module's actual point, since every accepted keyword must be one the harness enforces — would remain hand-written regardless; it compiles validators through `new Function`; and it would be seekdeep-tools' first runtime dependency, all to replace the ~70-line value walker while the path-qualified, every-violation error reporting stays custom either way.
- **Provider JSON mode instead of the capture tool:** it guarantees valid JSON, not schema conformance, and its interaction with tool calling is unclear. The capture tool preserves in-turn validation retries. Provider-side strict tool schemas can later narrow the accepted subset without changing this design.

## Consequences

Fan-out plans live in rerunnable scripts, and `outputSchema` provides authoritative structured child results. Each run pays helper-process and stdio RPC costs, while host startup stays non-blocking, cancellation can terminate the worker, and Rust types plus explicit materialization enforce the value boundary. The helper process is not a complete sandbox. Invalid options fail rather than degrading to Claude Code's `null`; consumers retain control through the run handle while observers receive snapshots only. Top-level Web users also receive a durable, replayable workflow record without widening the execution seam or coupling the original tool card to workflow-specific UI.
