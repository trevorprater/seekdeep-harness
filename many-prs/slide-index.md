# Slide index — every canvas view paired to its concept

28 distinct canvas views were detected across the 38-minute talk. The deck is a
tldraw canvas, so several entries below are the same artwork at a different
zoom; those are grouped. Timestamps are where the view first settles.

Images live in `slides/`. Full spoken context is in `transcript.md` at the same
timestamps.

---

## 1 — Title · `slide-01_00m00s.jpg` · 00:00

> i shipped 2000 PRs **(good)** last month — lauren tan (@poteto), Grok Bot @ SpaceXAI

The parenthetical "(good)" is doing real work: the whole talk is a defense
against the obvious objection that volume and quality trade off.

## 2 — Agenda · `slide-02_00m21s.jpg` · 00:21

1. trust 2. how to trust your agents more 3. your codebase is memory 4. automations

The four sections are one argument, not four topics: trust is the bottleneck,
and 2–4 are the three places you buy it.

## 3 — The Michelin kitchen · `slide-03_00m57s.jpg` · 00:57

The governing metaphor, illustrated. She explicitly rejects "software factory"
— assembly lines mass-produce, kitchens do creative work to a standard. You
stop cooking each dish and become responsible for the kitchen: the training,
the equipment, the ratio of line cooks to dishwashers.

## 4 — The receipts · `slide-04_02m09s.jpg` · 02:09

Three GitHub contribution graphs (3,159 / 4,681 / 5,284 commits; internal ranks
#6, #3, #3), captioned **"i shipped 5000+ PRs in 6 months"**. All three curves
are flat until roughly Jan '26, then near-vertical.

Note the number drift: the title says 2,000/month, this slide says 5,000+ over
six months, the X post says 2,500 last month. See the report's caveats.

## 5, 27 — The trust curve · `slide-05_05m12s.jpg`, `slide-27_35m24s.jpg` · 05:12, 35:24

Trust (y) against number of agents (x), saturating, with ticks at
**1 · 1 to 5 · 5 to 10 · 10 to 20 · hundreds · thousands**.

The talk's spine. Slide 27 is the closing callback. Her claim is that the
1-to-5 band is the hardest to escape, and that the escape is not a better model
but infrastructure you build yourself.

## 6, 7, 8, 10, 11 — How do i trust my agents more? · `slide-06_06m51s.jpg` · 06:51–15:33

- verification for correctness
- high quality skills that teach agents to work like real software engineers: eg pstack
- refactoring/rewriting architecture to be agent friendly *(greenfield vs brownfield)*

The spine slide, returned to four times as she finishes each bullet. Slides 7,
8, 10 and 11 are the same artwork at different zooms.

## 9 — High-quality verification · `slide-09_09m09s.jpg` · 09:09

> A feature map gives context. A CLI gives control.

**FEATURE MAP** (*what exists · how users reach it*) **+ CLI**
(`$ drive settings`, `$ capture proof` — *run the real app · collect proof*)
→ **THE AGENT CAN VERIFY ITS OWN WORK**

The most concrete mechanism in the talk. The CLI lives in the skill directory
so agents stop writing throwaway scripts; the feature map is "materialized
memory" of the app's surface, maintained by its own automation. Combined, an
agent can act on a vague bug report and prove its own fix.

## 12, 14, 28 — The correction ladder · `slide-12_15m39s.jpg` · 15:39, 21:42, 36:09

**whenever you correct your agent:**
1. codebase
2. static analysis (lint/compiler/ci)
3. rules/bugbot
4. skills
5. "style guide"

Her single most important slide, by her own statement at 36:06. Read it as a
priority order: push every correction as far up as it will go. Level 5 is the
one that does not scale.

## 13 — A framework designed for agents · `slide-13_19m03s.jpg` · 19:03

*minimal context* + a box marked "what the agent sees" containing one
`workaround`, arrows fanning out to three more. Captioned **your codebase is memory**.

Introduces Dune, and the thesis that an agent's context window is mostly just
the files it opened.

## 15, 16, 17 — Each copy makes the next copy likelier · `slide-17_22m24s.jpg` · 22:00–24:27

**ONE WORKAROUND** —copy→copy→copy— **THE PATTERN**, drawn as weeds spreading
through a garden bed.

The mechanism behind "codebase is memory" running in reverse: anti-patterns are
self-amplifying because agents extend what they see. Her worked example is
agents leaving explanatory comments — which she banned in Dune, because the
comments became the agent's justification for papering over a problem instead
of fixing it.

## 18 — Gardeners · `slide-18_24m30s.jpg` · 24:30

**delete tech debt → keep one paved path → lint against anti-patterns**, under
**your team needs gardeners**.

A proposed role, not just a practice. The lint rule is triage — "stop the
bleeding" — even when you can't clean up the instances yet.

## 19 — Dune · `slide-19_26m45s.jpg` · 26:45

> **Dune** — Architecture for agent-sized context.
> A narrow local edit can stay correct for the whole Electron app.

## 20 — Five nouns organize every app · `slide-20_27m03s.jpg` · 27:03

Around a **DUNE APP** core: **Feature** (one owned folder for product UI),
**Entrypoint** (one view a user can open), **Transcript card** (feature-owned
body for one entry type), **Client** (durable renderer state behind hooks and
commands, *one writer*), **Host** (always-on behavior behind a typed contract).

> Each noun has one place in the tree and one job at runtime.

## 21 — Process boundaries are visible in the tree · `slide-21_27m48s.jpg` · 27:48

> A folder tells an agent where code runs and which imports are legal.

`renderer/` (Feature UI, Navigation *links only*, Client *replica + commands*)
↔ **typed edge** ↔ serving processes (Host extensions, Electron main).
`shared/` holds cross-process types; each process imports only its allowed layers.

Born from a specific Cursor bug class: slow code accidentally imported into the
renderer thread, blowing the 16ms (or 8ms) frame budget. Dune makes that
import illegal rather than reviewable.

## 22 — Host-backed feature blueprint · `slide-22_29m27s.jpg` · 29:27

A vertical slice across **Feature UI** / **Client** / **Shared edge** /
**Host extension**, each with its file path convention.

> The component never handles IPC channels, Host peer lookup, retry ordering, or process startup.
>
> **Agent-friendly means a correct open-file edit preserves the whole app's invariants.**

That last line is the cleanest one-sentence definition of the whole architecture
section.

## 23, 24 — Building a michelin kitchen · `slide-23_30m48s.jpg` · 30:48

**grok bot (new!)** — *individual automation, line cook agent*
**cloud agents**
**automations and Agent SDK** — *team automation, factory agent*

Note she crosses out "software factory" live at 33:30, preferring the kitchen.

## 25, 26 — The outer loop, running · `slide-25_34m48s.jpg`, `slide-26_35m03s.jpg` · 34:48

Slack threads in `#glass-oncall-assistant` where a bot posts a triage verdict —
**"Reproduced but already fixed on main"**, with the reproduction steps, the
originating `#issues-glass` thread, and a `[view cloud agent]` link.

The payoff screenshot: bug reports reproduced and PRs opened without a human in
the loop, because the verification skill and feature map from slide 9 are what
let the bot decide "reproduced" at all.
