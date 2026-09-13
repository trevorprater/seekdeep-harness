# Agent Note: Remove the separate CLI demo

Status: implemented

English | [中文](2026-08-08-remove-cli-demo.zh.md)

## Problem

After [`seekdeep --profile headless`](../architecture/2026-08-06-app-owned-command-line.md) became the product one-shot command, `@seekdeep-ai/seekdeep-cli-demo` remained a second application package for the same job. It carried another executable, argument grammar, app composition, cancellation lifecycle, text/JSON/stream-JSON output contract, built artifact, documentation surface, and test suite. The two entry points also assembled different trees, so a successful demo did not prove the shipped `headless` profile and users had to choose between overlapping commands.

The replay suites still need canonical session events to pin assembled backend behavior. That testing need does not require a published command or compatibility contract.

## Decision

Delete `@seekdeep-ai/seekdeep-cli-demo` completely: its package, bin, parser, app plugin, output formats, tests, workspace references, generated-catalog entries, and active documentation. No alias or compatibility package remains. Users invoke the product command through `seekdeep --profile headless`; it owns final-text stdout, failure diagnostics on stderr, persistence, exit status, and shutdown.

`examples/headless-agent` is an explicit test composition. Its Loader configs mount the agent spine, one root agent, JSONL persistence, and checkpoint policy as separate rows instead of hiding them behind an app bundle. The support-tier `seekdeep-loader-smoke` Rust crate owns the shared direct-Agent turn helper and isolated subprocess runner. Its `src` and `lib` compatibility values select development and publish-shaped compiled Rust artifacts; neither mode starts Node or tsx. Three fixture executables are available only through the crate's `fixture-bins` feature. Example-local drivers remain test-only and do not define a supported product output format.

## Alternatives considered

- **Keep `seekdeep-cli-demo` as an alias or wrapper around `seekdeep --profile headless`.** Rejected because a second bin and package would preserve two discoverable owners without adding capability.
- **Move JSON and stream-JSON flags onto `seekdeep --profile headless`.** Rejected because no current product consumer requires them; adopting the old demo protocol would enlarge the canonical CLI contract solely to save test machinery.
- **Delete the canonical-event snapshots with the package.** Rejected because they pin model-visible assembled behavior that final-text product acceptance cannot observe.
- **Keep the app plugin but delete only its bin.** Rejected because the hidden composition would still duplicate the explicit headless profile and conceal which services the test leaf mounts.

## Consequences

This is intentionally breaking. `seekdeep-cli-demo`, its `--output-format` choices, and imports from `@seekdeep-ai/seekdeep-cli-demo/src/cli.ts` no longer resolve. There is no public event-stream replacement in this change; callers use `seekdeep --profile headless` for one-shot execution and must choose an existing protocol API when they need structured automation.

The repository retains backend replay coverage through test-only infrastructure, while product smoke and built-bin acceptance exercise `seekdeep --profile headless`. A separate one-shot package may return only if it owns a genuinely independent, versioned protocol that cannot belong to the product launcher; a second spelling or output shim is not enough.

## Verification

Focused Loader smokes cover the explicit composition through development and publish-shaped Rust artifacts. The shared turn helper drives a real Agent and Session event path; the subprocess runner proves stdin closure, output capture, exact exit status, deadline termination, setup/inspection ordering, and isolated-directory cleanup with compiled fixtures. Snapshot tests diff canonical JSONL and persisted logs, product acceptance covers `seekdeep --profile headless`, and documentation plus generated graph/catalog checks reject live references to the removed package. The frozen Agent Note archive remains historical evidence and is not rewritten.
