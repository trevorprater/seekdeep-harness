# Rungs by ecosystem

Name real tools when proposing fixes. "Add a linter" is not a recommendation;
"add `import-linter` with a contract forbidding `domain -> infrastructure`" is.

Each section lists what rung 1 (make it unrepresentable) and rung 2 (catch it
mechanically) look like concretely, plus the mechanism for adopting a check on
a codebase that already violates it.

## TypeScript / JavaScript

- **Rung 1:** `strict` plus `noUncheckedIndexedAccess`; discriminated unions
  with exhaustive `switch` returning `never`; branded types for IDs that must
  not be interchangeable; `readonly`; separate packages so a forbidden import
  cannot resolve.
- **Rung 2:** ESLint or Biome; `eslint-plugin-boundaries` or
  `eslint-plugin-import` `no-restricted-paths` for layering; `dependency-cruiser`
  for graph rules; `knip` for dead exports; `tsc --noEmit` in CI.
- **Legacy adoption:** `--max-warnings` ratchet, `eslint-nibble`, or run ESLint
  over changed files only in CI. Per-directory overrides let you make a rule
  an error in new code and a warning in old.

## Python

- **Rung 1:** type hints with `mypy --strict` or Pyright; `NewType` for IDs;
  frozen dataclasses; `Literal` and `Enum` over strings; `__all__` plus package
  layout to keep internals unimportable.
- **Rung 2:** Ruff (fast, covers most of flake8 plus import rules);
  `import-linter` for layer contracts; `mypy` in CI; `pytest` with markers for
  a fast subset.
- **Legacy adoption:** `mypy` per-module overrides tightened directory by
  directory; Ruff `per-file-ignores`; `# type: ignore` with a burn-down count
  checked in CI.

## Rust

- **Rung 1:** the type system is the main instrument — newtypes, enums with
  exhaustive matching, `#[non_exhaustive]`, typestate, crate boundaries with
  `pub(crate)`. Splitting into crates makes illegal dependencies fail to
  compile, which is the strongest available boundary.
- **Rung 2:** `clippy` with `-D warnings`; workspace lints in `Cargo.toml`;
  `cargo deny` for dependency policy; `cargo machete` for unused deps.
- **Legacy adoption:** `#![allow(...)]` at module scope with a tracking issue,
  removed as areas are cleaned; clippy `allow` counts asserted in CI.

## Go

- **Rung 1:** unexported types; distinct named types over bare strings;
  interfaces defined at the consumer; internal packages, which the compiler
  enforces as a real boundary.
- **Rung 2:** `golangci-lint` with an explicit enabled set; `go vet`;
  `depguard` for forbidden imports; `errcheck`.
- **Legacy adoption:** `golangci-lint --new-from-rev=origin/main` reports only
  issues in changed code — the cleanest changed-files-only mechanism in any
  ecosystem.

## Java / Kotlin

- **Rung 1:** sealed interfaces with exhaustive switches; records; the module
  system or Gradle module boundaries; package-private by default.
- **Rung 2:** ArchUnit for layering rules as tests; Checkstyle or ktlint;
  SpotBugs or Error Prone; NullAway.
- **Legacy adoption:** ArchUnit `freeze()` records current violations and fails
  only on new ones — the purpose-built stop-the-bleeding tool.

## C#

- **Rung 1:** nullable reference types enabled; `sealed`; records; `internal`
  plus assembly boundaries.
- **Rung 2:** Roslyn analyzers with severity in `.editorconfig`;
  `NetArchTest` for layering; warnings as errors.
- **Legacy adoption:** per-project `<WarningsNotAsErrors>` shrunk over time, or
  a `.globalconfig` per directory.

## Any stack

- **Task runner** — `make`, `just`, `task`, or package scripts. Named targets
  are what make rung 2 reachable, because CI and agents run the same command.
- **CI** — a check that does not run in CI is a suggestion.
- **Pre-commit hooks** — fast feedback, but not enforcement: they are
  skippable, so pair them with the CI check rather than relying on them.
- **Generated code with a freshness check** — generate, then fail CI if the
  committed output differs. Turns a whole class of "remember to regenerate"
  into rung 2.
- **Codemods** — once a check defines "done", agents are effective at bulk
  migration. Add the check first, then migrate.
