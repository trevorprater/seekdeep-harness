# Building the self-verification surface

An agent that cannot check its own work asks a human or guesses. One task at a
time it asks; twenty in parallel it guesses, and the guesses arrive as pull
requests someone now has to catch by reading. That is the rung that does not
scale, so everything else is capped by this.

Two pieces are needed, and teams usually build the first and skip the second.

## Piece 1 — a way to exercise the real thing

Not "run the unit tests". A way to drive the actual software and get back
output an agent can read and reason about.

**The signal that you need this:** agents keep writing throwaway scripts to
check something. Every session reinvents a slightly different one, they vary in
quality, and none survive. When you see that, promote the script: write it
once, commit it, maintain it like product code.

What it looks like by software type:

| Type | Exercise it | Evidence to return |
| --- | --- | --- |
| HTTP service | start it, hit real routes | status, body, timing, logs |
| Library | run a real usage example | values, types, error paths |
| CLI | invoke real subcommands | exit code, stdout/stderr, files written |
| GUI / web app | drive it through automation | screenshots, DOM state, console, traces |
| Data pipeline | run on a fixture dataset | row counts, schema, sample rows |
| Batch job | run against a seeded store | before/after state diff |

Properties that matter:

- **Reproducible.** Same command, same shape of output, every time. Agents
  compare runs; noise defeats them.
- **Committed.** In the repo, versioned, reviewed. A verification tool that
  lives on someone's laptop does not exist.
- **Readable output.** Text an agent can parse and quote. A screenshot with no
  accompanying state dump is weak evidence.
- **Fast enough to use.** If it takes twenty minutes, it will be skipped. Offer
  a narrow mode alongside the full one.
- **Honest failures.** It should be able to report that something is broken.
  A tool that always succeeds teaches agents to stop reading it.

## Piece 2 — a map of what exists and how to reach it

The failure this solves is specific. A report arrives saying "the settings page
is broken" or "export is wrong". An agent can run the app but has no idea where
the settings page is or how a user gets there, so it guesses, and it guesses
badly.

A checked-in map of the surface converts a vague report into something
actionable. It is not architecture documentation — it is a directory of
reachable things.

For each entry record: what it is, how to reach it (route, command, selector,
function), what it depends on, and how to tell it is working. Keep it as
structured data next to the verification tool so both agents and the tool can
read it.

```yaml
- id: settings.export
  what: Export account data as CSV
  reach: Settings → Data → "Export"; or `app export --format csv`
  code: src/features/settings/export/
  depends_on: [auth.session, storage.blob]
  healthy_when: downloads a non-empty CSV with a header row
```

Scale the format to the software: routes for a service, public API for a
library, subcommands for a CLI, screens and flows for an app.

**Keeping it current** is the part that decides whether this survives. Pick at
least one:

- Generate it from the code where the surface is already declared — a router
  table, a command registry, an OpenAPI spec. Generated beats maintained.
- Add a check that fails when a new entry point exists with no map entry. This
  is the rung-2 version and it is usually worth the small cost.
- Have the agent that adds a feature update the map in the same change, and
  treat a missing update as an incomplete change in review.

A stale map is worse than none, because it is confidently wrong. If you cannot
commit to one of the above, keep the map narrow enough to stay true.

## What "done" looks like

From a clean clone, an agent can answer, using only the repository:

1. How do I build and test this?
2. How do I run only what my change affects?
3. How do I exercise the real thing and see what happened?
4. What does this software expose, and how do I reach a given part of it?
5. How do I tell whether what I just changed actually works?

If question 5 requires a person, the ceiling on parallel agent work is however
many diffs that person can read.
