# AGENTS.md — Documentation website adapter

Follow the [root instructions](../AGENTS.md), the [documentation standard](../docs/AGENTS.md), and the [documentation-site sync workflow](../.agents/skills/seekdeep-doc-site-sync/SKILL.md).

## Keep documentation content out of this tree

`website/` owns only VitePress configuration, presentation assets, and the publication manifest. This file is the only maintained Markdown file in this subtree.

Keep canonical prose and generated catalogs in their owning `docs/` tier, then expose selected pages through [docs.json](docs.json). Never add locale, route, API, or copied documentation trees such as `website/zh-CN/`, `website/en/`, or `website/api/`.

[The Rust projector](../crates/repository-tools/src/doc_site.rs) owns publication, navigation, and build preparation. [The WASM runtime](../crates/docs-site-runtime/src/lib.rs) owns edit-link validation, Markdown interpolation escaping, and sidebar scrolling. Keep [.vitepress/config.mjs](.vitepress/config.mjs) limited to VitePress calls that delegate to those compiled implementations.

The projector writes disposable Markdown to the ignored `website/.generated/` directory. Never edit or commit `.generated/`, `.cache/`, or `.dist/`.

Run `pnpm docs:check` after changing this subtree; the gate rejects additional non-ignored Markdown under `website/`.
