# AGENTS.md — Harness Crates

These crate-specific rules supplement the repo-wide [conventions](../AGENTS.md#rust-conventions). The behavioral rules in [packages/AGENTS.md](../packages/AGENTS.md) bind a crate exactly as they bind the package manifest it implements. Use [seekdeep-native-plugin](../.agents/skills/seekdeep-native-plugin/SKILL.md) to add, port, or extend a plugin crate.

## Where a plugin lives

Runtime behavior is Rust in this tree; `packages/<group>/<pkg>/` keeps the same plugin's npm manifest, README pair, and browser build assets. A built-in is compiled into the `seekdeep` binary: [`apps/seekdeep/src/profile_boot.rs`](../apps/seekdeep/src/profile_boot.rs) registers it in the Loader's `PluginCatalog` under its bare name and its `@seekdeep-ai/` specifier, and a `packages/bundle/*/cordis.patch.yml` row selects and configures it for a profile. File-backed TypeScript plugins named by path in `cordis.yml` remain a preserved user surface and run in the Loader's Node boundary; model-authored packages run in the Rust-owned evaluator. Neither replaces a built-in ([placement](../porting/DYNAMIC_PLUGIN_RELOAD.md#runtime-placement)).

## Crate shape

- `Cargo.toml` names the crate `seekdeep-<name>`, inherits `version`, `edition`, `rust-version`, and `license` from the workspace, and sets `[lints] workspace = true`. The workspace forbids `unsafe`, denies clippy `pedantic`, and warns on missing docs, so every public item has a doc comment and every fallible public function an `# Errors` section.
- `lib.rs` exports `NAME`, `INJECT`, and `plugin() -> Plugin`. `Plugin::new(NAME, INJECT.iter().copied(), …)` deserializes the JSON config and calls an `apply(&Context, Config)` that returns the owning `EffectHandle`; `.with_config_validator` resolves the Loader-facing `Schema`, so misconfiguration fails before the body runs.
- Services are `ServiceKey<T>` constants, provided with `context.provide` and read with `context.get`. Every registration is owned by the fiber: `context.own`, the owning registry's `register`, or `context.events()` listeners, which are effects already.
- IDs crossing process, persistence, or protocol boundaries are newtypes; closed enums match exhaustively; wire enums keep unknown values.
- Crates in the determinism perimeter take clocks and scheduling through injectable seams; boundary crates may read ambient time. No crate uses ambient randomness.
- A crate that also targets the browser gates native-only code on `cfg(target_arch = "wasm32")`.

## Tests and evidence

- `tests/loader_composition.rs` boots the plugin through a real `PluginCatalog` and the Loader's `load_yaml` from a test composition; that is the composition test [testing policy](../docs/testing.md) requires, and it catches invalid Loader boundaries that hand-mounted tests miss.
- Ported surfaces keep oracle tests (`*_parity.rs`) that run the pinned checkout named in `SOURCE_SNAPSHOT`; they require that checkout.
- Invariant companions register through `seekdeep_invariants` under the manifest name.
- [`crates/cordis/examples/hello_cordis.rs`](cordis/examples/hello_cordis.rs) is the smallest runnable walkthrough of the lifecycle these tests exercise.

## Wiring, docs, and gates

- Add the roster registration and the bundle row; `verify-cordis-config` checks that rows resolve.
- Keep the crate README triplet and the package README current per the [README requirements](../docs/cookbook/adding-a-package.md#4-write-the-package-readme); re-record edited pairs.
- Regenerate the catalogs the change touches: `cargo xtask tool-catalog`, `config-catalog`, and `cordis-catalog`, each with `--check` as the freshness gate.
- A ported surface gets its `porting/parity.json` entry with evidence; `cargo xtask parity` is the final gate. Capabilities absent from the pinned source wait for parity ([POST_PARITY](../porting/POST_PARITY.md)).
- Before claiming checks pass, run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and the crate's tests; select the rest through [seekdeep-pre-push-checks](../.agents/skills/seekdeep-pre-push-checks/SKILL.md).
