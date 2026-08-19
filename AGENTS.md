# AGENTS.md

SeekDeep Harness is the Rust port of DeepSeek Harness. The parity oracle is the clean source checkout at `/Users/trevor/ws/deepseek-harness`, pinned in `SOURCE_SNAPSHOT`.

## Hard requirements

- Preserve every observable behavior, failure mode, lifecycle invariant, protocol, persistence format, configuration field, command, and user-facing surface from the pinned source.
- Rename product-facing `DeepSeek Harness` / `dsh` identities to `SeekDeep Harness` / `seekdeep`. Provider names and external protocol fields that refer to DeepSeek models remain unchanged when required for compatibility.
- Production implementation is Rust. Browser code is Rust compiled to WebAssembly. Native integrations are Rust. Compatibility bindings may expose foreign-language APIs but must delegate all behavior to compiled Rust.
- Runtime code reload design is tracked in [`porting/DYNAMIC_PLUGIN_RELOAD.md`](porting/DYNAMIC_PLUGIN_RELOAD.md). Preserve the source's distinct config, Host HMR, browser HMR, and model-defined Host/Client package semantics while the proposal's open decisions remain unresolved. Do not remove model-authored JavaScript compatibility or use ordinary Rust dynamic-library unloading as the general reload mechanism.
- Treat the source tests, snapshots, examples, and generated catalogs as executable specifications. Port them and add differential tests wherever both implementations can be driven with the same input.
- Model-visible data must be reconstructable from the append-only session log. Registrations are reversible effects. Misconfiguration fails at the earliest resolvable point.
- Do not claim parity from compilation or unit tests alone. `cargo xtask parity` is the final manifest gate and the full verification commands must be run fresh.

## Rust conventions

- The workspace uses Rust 2024 and forbids unsafe code unless a narrowly scoped native crate documents why the platform API cannot be called safely otherwise.
- Public IDs crossing process, persistence, or protocol boundaries use newtypes, never bare strings.
- Exhaustively match closed enums. Extensible wire enums preserve unknown values explicitly.
- Every async lifecycle operation has one owner and deterministic cancellation, rollback, and teardown.
- Files end with exactly one trailing newline. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features` before completion claims.
