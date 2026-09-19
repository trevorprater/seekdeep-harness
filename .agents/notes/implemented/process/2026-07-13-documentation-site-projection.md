# Agent Note: Project canonical documentation into the website

Status: implemented

English | [中文](2026-07-13-documentation-site-projection.zh.md)

## Problem

The repository needs a navigable documentation website without turning the website directory into a second documentation source. Copying package guides, architecture pages, or generated catalogs into a site-specific tree allows the two copies to drift, while pointing VitePress directly at the repository root couples public URLs and navigation to the internal file layout. Repository-relative links also need different destinations on the website: published pages stay inside the site, but source files and unpublished contributor documents belong on GitHub.

## Decision

Canonical Markdown remains in the repository tier that owns it. Product-facing guides live under `docs/user/`, generated reference remains in the existing generated catalogs, and architectural and cookbook pages remain at their existing `docs/` paths.

[The publication manifest](../../../../website/docs.json) is an explicit allowlist, validated by the Rust `DocsManifest` and `DocsPage` types. Each entry maps one canonical source file to a stable public route, sidebar, section, and order. Adding or removing a published page is therefore a reviewable manifest change rather than an implicit directory crawl.

[The Rust projector](../../../../crates/repository-tools/src/doc_site.rs), exposed by `pnpm docs:project`, writes the manifest into the ignored `website/.generated/` directory before VitePress starts or builds. The generated tree follows public routes so VitePress navigation, locale detection, and local search share the same route vocabulary. Each page receives an `editSource` frontmatter field pointing to its canonical repository file; the edit-link callback reads only that page data, so public URLs remain independent of the source layout.

`docs:prepare` materializes navigation and locale configuration in Rust and compiles [the documentation runtime](../../../../crates/docs-site-runtime/src/lib.rs) to WASM for Node and the browser. The VitePress adapter only connects those compiled callbacks: canonical edit-link validation, Markdown interpolation escaping, source-watch selection, and the owned sidebar-scroll listener. The website's dependency importer in the root lockfile supplies an isolated frozen install, so documentation builds do not require the removed TypeScript workspace implementations. Prepared configuration, dependencies, and browser assets remain under the ignored cache.

VitePress serializes theme callbacks as function text. The Rust runtime creates a self-contained edit-link delegate, and the theme awaits the shared browser runtime before hydration; server rendering uses the initialized Node bindings. Capturing a module-local runtime in the callback would lose that reference during VitePress deserialization.

Locale home projections retain only the canonical YAML frontmatter. The repository-facing body keeps its H1 and bilingual source links, while the frontmatter implements the [locale-preserving quick-start redirect](../simplification/2026-08-11-quickstart-documentation-home.md) and the site navigation owns locale switching.

The projector parses Markdown links without reserializing the document. A link to another published source becomes a site-relative route; a link to an unpublished repository file becomes a source link under the `trevorprater/seekdeep-harness` repository home; a repository image is copied into the generated tree and referenced from there ([why](2026-08-06-doc-site-carries-its-images.md)). Missing relative targets fail projection. `docs:check` runs source-differential projector and configuration tests, a production VitePress build, rendered-fragment validation, and the built WASM callback comparisons in Node and Chromium.

Verbatim declaration catalogs retain the exact declaration paths and line numbers of the pinned oracle. Their Rust generators apply [the source-link renderer](../../../../crates/repository-tools/src/doc_source_links.rs), which links those declarations to the `SOURCE_SNAPSHOT` revision in `fugue-labs/deepseek-harness`. Links to package documentation use the owning Rust crate's local path. The site projector never resolves a missing local target by silently reading a second repository.

`verify-public-repository-links` rejects references to the unavailable legacy repository from tracked files. Site preparation uses the build revision for repository source links and the current branch for canonical edit links; CI may supply those through its GitHub revision and branch metadata.

`website/AGENTS.md` is the only maintained Markdown file in the website subtree. The projector test enumerates tracked and unignored files and rejects any other website Markdown, so site-specific locale, route, API, or generated source copies cannot bypass the publication manifest.

Mermaid renders the canonical diagrams. The website workspace explicitly declares the five packages that `vitepress-plugin-mermaid` asks Vite to prebundle because pnpm's strict dependency isolation otherwise makes those transitive packages unavailable to the local development server; Knip records this runtime-only use as an intentional dependency exception.

Site publication remains separate from site construction. A dedicated GitHub Actions workflow runs the existing documentation gates, uploads `website/.dist` as a Pages artifact, and deploys only after the build succeeds. `actions/configure-pages` supplies the destination's base path to VitePress at build time, so the private Pages origin, a later public project path, and a custom domain do not require distinct checked-in configurations. Pages visibility remains a repository hosting setting rather than a workflow permission.

## Alternatives considered

**Commit copied Markdown under `website/`.** This makes VitePress setup direct, but every copied guide or API table gains two owners and requires a synchronization convention that cannot identify which copy is authoritative.

**Make `website/` the canonical home for every published page.** This keeps one copy but moves architecture, generated reference, and contributor-facing material away from their repository ownership tiers merely to satisfy a renderer.

**Discover every Markdown file automatically.** This minimizes manifest maintenance but publishes internal documents accidentally, exposes source moves as URL changes, and produces navigation from incidental directory order.

**Use filesystem symlinks.** Symlinks preserve a single source but do not solve public routing or repository-relative links, and their behavior is less predictable across local development, package tooling, and hosted CI environments.

**Build only in a deployment workflow.** A deployment job can reveal rendering failures after merge. Keeping the production build in `doc-sync` makes the same failure visible locally and in ordinary CI even when no public deployment exists.

**Hard-code the public project path.** A fixed `/seekdeep-harness/` base works for the public project URL but not for the unique origin assigned to a private Pages site or for a future custom domain. Consuming Pages metadata keeps one build contract across those destinations.

## Consequences

Documentation facts have one editable home, public routes remain stable across source moves, and the site can include generated references without committing another generated copy. Local development watches canonical inputs and regenerates the disposable projection. The layout gate makes an obsolete site-specific Markdown tree a merge failure instead of ignored build input. Merges that affect the documentation site deploy the checked result to Pages, while manual dispatch provides a recovery and validation entry point.

The publication manifest is a maintained allowlist, and link projection adds a small repository-specific build adapter. A new kind of Markdown link behavior needs a projector test. Mermaid support also increases the client bundle size, but preserves diagrams already used by the canonical documentation.
