---
name: agent-ready-codebase
description: >-
  Reshape a codebase so coding agents produce correct work in it by default,
  and convert recurring agent mistakes into permanent structural fixes instead
  of repeated corrections. Use this whenever someone wants to prepare, audit,
  or harden a repository for AI agents; says agents keep making the same
  mistake, keep ignoring a convention, or keep needing the same explanation;
  asks about agent-friendly architecture, paved paths, guardrails, or
  "why does the agent keep doing X"; is about to run many agents in parallel
  and worries about quality; or is setting up AGENTS.md, CLAUDE.md, cursor
  rules or similar agent instructions. Also reach for it proactively when you
  notice you have been corrected on the same point twice in one session, or
  when you are about to write an agent instruction file as a fix for something
  a type or a lint rule could have caught.
---

# Agent-ready codebase

## Why codebases decide agent quality

An agent's behavior is dominated by what is in front of it. The files it opens
become its context, and the strongest signal in that context is what the
surrounding code already does. Agents extend existing patterns — that is the
single most reliable thing about them.

This cuts both ways, and the second direction is the one teams miss:

- A good pattern in the codebase is free, permanent instruction that every
  future agent receives without anyone writing it down.
- A bad pattern is *also* instruction. One workaround gets copied, the copy
  makes the next copy likelier, and within weeks it is the house style. Nobody
  decided this. The codebase taught it.

So the codebase is the highest-bandwidth channel into agent behavior — higher
than any instructions file, because it is always in context and never skipped.
Work on the channel, not on the reminders.

The practical consequence: the lever is *structure*, not *prose*. Teams reach
for an instructions file because it is the cheapest thing to write, and then
wonder why the agent ignores it on the twentieth task. Prose is the weakest
rung available. Use it when nothing stronger fits, not first.

## The correction ladder

This is the core tool. When an agent produces something wrong, the instinct is
to correct it in the conversation. That correction dies with the session. Ask
instead:

> **What would have made this mistake impossible?**

Then place the fix at the highest rung that can actually hold it:

| Rung | The mistake is... | Costs | Fails when |
| --- | --- | --- | --- |
| **1. Impossible** | unrepresentable — types, data structures, module boundaries, API shape | once | you can't express the constraint |
| **2. Caught** | expressible but mechanically rejected — lint rule, compiler flag, test, CI check | once, plus CI time | the check is ambiguous or slow |
| **3. Told** | described in writing the agent usually reads — agent instructions, review bot, documented playbook | every invocation | context is long, or the file isn't read |
| **4. Noticed** | spotted by a human reading the diff | a human, every time | volume rises — this rung breaks first |

Each rung down is more expensive per use and less reliable. Rung 4 is where
most teams live by default, and it is the one that collapses the moment agents
start opening pull requests faster than people can read them.

**Push each correction as far up as it will hold.** Not further: a type that
contorts the domain to prevent a typo is worse than a lint rule. The goal is
the highest rung that fits *naturally*.

Two things that make this practical:

- **You do not have to fix the existing instances to stop the spread.** Adding
  the check first stops the bleeding; migrating the call sites is separate,
  schedulable work. Teams stall because they think rung 2 requires rung 2
  everywhere at once. It does not.
- **A fix at rung 1 or 2 deletes the need for the rung-3 text.** When you add
  the lint rule, remove the paragraph in the instructions file that was
  standing in for it. Otherwise guidance files grow without bound and the
  signal in them drops.

## Two ways in

### Reactive — a correction just happened

Someone corrected an agent, or you noticed yourself being corrected twice.
This is the highest-value moment to use this skill, and the easiest to miss.

1. State the mistake concretely: what was produced, what was wanted.
2. Ask what would have made it impossible. Work down from rung 1 until you
   reach a rung that fits naturally.
3. Check whether it has already spread — one instance is a mistake, three is a
   pattern that will keep growing. `scripts/find_repeated_blocks.py` finds
   these.
4. Propose the fix at that rung, plus a stop-the-bleeding check if instances
   already exist. Apply once approved.

Keep this small. A single correction should usually produce a single lint rule
or a single type change, not a refactoring program.

### Proactive — assess and harden a repository

Someone wants a repo made ready for agents. Work through the assessment below.

## Assessment workflow

**Ground everything in evidence from the actual repository.** The failure mode
of this work is a generic report that could have been written without reading
the code — "add a linter, write a README, improve test coverage". That is
worthless and slightly insulting. Every finding needs a file path and a
concrete instance, and every recommendation needs a named artifact.

If a finding has no file path behind it, it is a guess. Drop it.

### 1. Probe

Run the bundled probe to gather signals rather than guessing at them:

```
python scripts/probe_repo.py <repo-path>
```

It reports languages, build/test entrypoints, formatters, linters, type
checkers, CI, pre-commit hooks, existing agent instruction files, and
workaround markers. It is read-only.

Then look for propagated patterns:

```
python scripts/find_repeated_blocks.py <repo-path>
```

This finds near-duplicate code blocks across files — the signature of a
workaround that has been copied. Clusters that contain a workaround marker are
the highest-priority findings in the whole assessment, because they are
actively teaching.

Read the top findings yourself. The scripts locate candidates; they do not
judge them.

### 2. Evaluate against the five properties

See `references/properties.md` for what each one means, how to test it, and
what good and bad look like. In brief:

1. **One paved path** — exactly one blessed way to do each common thing.
2. **Self-verification** — an agent can get ground truth without a human.
3. **Legible boundaries** — the tree says what is allowed where.
4. **Local edits stay correct** — a correct-looking change to one file cannot
   silently break an invariant elsewhere.
5. **Clean exemplars** — the code most likely to be copied is code you want
   copied.

For each, record: how it stands today, the evidence, and the gap.

### 3. Rank by ladder position and blast radius

Order the work by leverage, not by how annoying each problem is:

- Actively propagating anti-patterns first. They compound while you deliberate.
- Then missing rung-2 checks for mistakes that already recur.
- Then rung-1 structural changes, which are the most valuable and the most
  expensive — be honest about cost.
- Then guidance, last and least, and only for things that genuinely resist the
  rungs above.

### 4. Propose, then apply on approval

Present the plan before changing anything. For each item give: the finding with
its evidence, the rung, the specific artifact you would add or change, and the
blast radius. Then apply what is approved.

Presenting first matters here beyond ordinary caution. These changes alter how
everyone's tooling behaves — a new lint rule can fail every open branch. The
person deciding needs to see the set, not discover it.

### 5. Leave the assessment behind

Write the findings and plan into the repository, not just the conversation, so
the next agent and the next person can pick it up. A short `AGENT_READINESS.md`
with the findings, what was done, and what was deferred is enough.

## Sequencing: greenfield versus existing code

Most of the strong advice in this area was developed on young codebases, where
"delete the tech debt and lock it down" is affordable. On a large existing
codebase that same advice is a way to produce a plan nobody executes.

**Greenfield or small:** go straight for rung 1. Set boundaries in the
structure now, while there are few call sites. This is the cheapest it will
ever be.

**Large or legacy:** invert the order.

1. **Stop the bleeding** — add checks for the patterns that are actively
   spreading, scoped to new and changed code. Most linters support warning on
   existing violations while failing on new ones; a baseline file or a
   changed-files-only CI step does the same thing. This buys time without a
   migration.
2. **Converge** — pick one paved path per common task, document it, point
   agents at a real exemplar.
3. **Migrate** — clean up instances, which agents are good at once a check
   defines "done".
4. **Then restructure** — rung-1 changes, once the bleeding has stopped and you
   know which boundaries actually matter.

Say which regime you are in and why. Getting this wrong wastes the whole
engagement.

## Self-verification is the one to build first

If the team can only do one thing, make it possible for an agent to check its
own work.

An agent that cannot verify asks a human or guesses. At one task at a time it
asks. At twenty in parallel it guesses, and the guesses land in pull requests
that a human now has to catch — which is rung 4, the rung that does not scale.
Every other investment is limited by this one.

Concretely, it needs two things:

- **A way to exercise the real thing and collect evidence.** One documented
  command that builds, runs, or drives the actual software and produces output
  an agent can read. Committed to the repository and maintained like product
  code, not reinvented per session. When agents keep writing their own
  throwaway scripts to check something, that is the signal: promote the script.
- **A map of what exists and how to reach it.** Agents fail on vague requests
  ("the settings page is broken") not because they cannot act but because they
  cannot locate. A checked-in map of the surface — endpoints, commands,
  screens, public API, whatever your software exposes — plus how to reach each
  one, converts a vague report into something actionable.

`references/verification.md` covers what this looks like for services,
libraries, CLIs and UIs, and how to keep the map from going stale.

## Ecosystem specifics

`references/ecosystems.md` maps the rungs onto concrete tooling for JavaScript
and TypeScript, Python, Rust, Go, Java and C#. Read the section for the stack
in front of you when proposing rung-2 fixes, so recommendations name real tools
and real config rather than "add a linter".

## Judgment

A few things worth holding onto, because the temptation runs the other way:

**Constraint has a cost, and it is paid by humans.** A heavily locked-down
codebase is genuinely less pleasant to work in by hand. That trade can be right
when most contributions are agent-authored, and wrong when they are not. Ask
who is actually writing the code before recommending severity.

**Not every mistake deserves a rung.** Some are one-offs. Adding machinery for
a mistake that happened once produces a codebase full of rules nobody
remembers the reason for. Two or three occurrences is a pattern; one is an
anecdote.

**Removing a bad exemplar beats adding a rule about it.** If a file is being
copied and it should not be, deleting or fixing it is usually cheaper and more
effective than a rule telling agents not to copy it.

**Resist the rewrite.** The pull of this work is toward "the architecture is
wrong, let us rebuild it agent-first". That is almost never the recommendation
the person needs, and proposing it tends to end the conversation rather than
start the work.

---

*The correction ladder, the codebase-as-memory framing, and the
self-verification pattern are generalized from lauren tan's (@poteto) talk "i
shipped 2000 PRs last month", recorded for Cursor Compile 2026. The framing
here is stack- and tooling-agnostic; the talk's specifics were Electron and
TypeScript.*
