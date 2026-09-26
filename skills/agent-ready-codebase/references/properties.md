# The five properties, and how to test each

Each section gives what the property means, a concrete way to check it against
a real repository, and the usual fix with its ladder rung. Test rather than
infer — the point of an assessment is evidence the team can argue with.

## 1. One paved path

**Means:** for each common task — add an endpoint, add a screen, read config,
log something, talk to the database, write a test — there is exactly one
blessed way, and it is discoverable.

**Why it matters more for agents than for people:** a human who finds three
patterns asks which one to use. An agent picks whichever is nearest in its
context and adds a fourth instance. Variation is not neutral; it compounds.

**How to test:** pick three common tasks. For each, find every existing
implementation. Three ways to construct a database handle is a finding; so is
two HTTP clients, two config loaders, two test-fixture styles, two date
libraries.

```
git grep -l 'createClient\|new HttpClient\|requests.Session'
```

**Good:** one way, used everywhere, with an obvious exemplar to copy.
**Bad:** several coexisting eras, none marked as preferred, all live.

**Usual fix:** pick the winner and say so (rung 3), lint the losers so new
instances fail (rung 2), then migrate. Consolidating behind one module so the
alternatives are not importable is rung 1 and best where it fits.

## 2. Self-verification

**Means:** an agent can find out whether its change worked without asking a
person.

**How to test:** clone fresh and try to answer, using only what is in the
repository: how do I build this, how do I run the tests, how do I run just the
tests for what I changed, how do I exercise the thing end to end and see the
result. Count the steps you had to infer.

**Good:** one documented command per question, they work from a clean clone,
and failures print something an agent can act on.
**Bad:** commands live in a CI config, a wiki, or someone's memory; the test
suite takes forty minutes with no subset; failures surface as a stack trace
with no indication of what to do.

**Usual fix:** a task runner with named targets (rung 2 once CI runs them),
plus the evidence-collection surface described in `verification.md`. This is
usually the highest-value single investment — see the SKILL.md section on why.

## 3. Legible boundaries

**Means:** the directory tree tells you where code runs and what it may
depend on, without reading the code.

**Why:** an agent with a narrow view of the repo has to guess whether an import
is allowed. Guessing wrong produces changes that look right in the open file
and break something three layers away — exactly the class of bug that survives
review at volume.

**How to test:** name two layers that must not mix (UI and data access, main
process and renderer, domain and transport). Then check whether anything
mechanically prevents the import, or whether it is convention.

```
git grep -n 'from .*infrastructure' -- src/domain/
```

**Good:** layers are directories, and a check fails when the boundary is
crossed — module system, build config, or a lint rule.
**Bad:** the boundary exists in a diagram and in senior engineers' heads.

**Usual fix:** an import-boundary lint rule (rung 2 — `eslint-plugin-boundaries`,
`import-linter`, `go-arch-lint`, ArchUnit, Rust's crate graph). Splitting into
separately compiled units is rung 1.

## 4. Local edits stay correct

**Means:** a change that looks correct in the file you have open cannot
silently violate an invariant maintained somewhere else.

**Why:** this is the defining property of agent-friendliness. Agents work with
a narrow view by construction. If correctness requires knowing about four other
files, an agent with three of them in context produces a confident, wrong diff.

**How to test:** look for invariants maintained by convention across files —
"if you add a variant here, also register it there", "remember to update the
migration", "keep this list in sync with that enum". Each one is a trap. Ask
the team which registration steps people forget; those are the live ones.

**Good:** adding a case fails to compile until every place that must handle it
does. Exhaustive matches, discriminated unions, a registry that derives from
one declaration, a generated file with a check that it is current.
**Bad:** parallel lists kept in sync by hand, string keys that must match
across modules, "don't forget to also…" in a doc.

**Usual fix:** rung 1 by nature — make the second location derive from the
first, or make omission a type error. Where that is impractical, a test that
asserts the two stay in sync is a solid rung 2.

## 5. Clean exemplars

**Means:** the code most likely to be read and copied is code you want copied.

**Why:** agents pattern-match on what they open. The most-read file in the
repository is its de facto style guide, whatever the written one says.

**How to test:** ask which file someone would open to learn how to add a
feature. Read it as if you were about to copy it wholesale. Then run
`find_repeated_blocks.py` and see what is *actually* being copied — the answer
is often not the file you expected.

**Good:** a canonical example per common task, kept current, ideally exercised
by tests so it cannot rot silently.
**Bad:** the obvious example is the oldest one; the cleanest code is in a
module nobody browses.

**Usual fix:** fix or delete the bad exemplar. This is the cheapest high-impact
move available and it is routinely overlooked in favour of writing a rule that
tells agents not to copy the thing that is still sitting there.

---

## Recording findings

For each property, record: current state, the evidence (paths, commands, grep
output), the gap, the proposed rung, and the cost. A finding with no path
behind it is a guess — drop it rather than padding the report.
