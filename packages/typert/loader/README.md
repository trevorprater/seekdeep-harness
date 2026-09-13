# @seekdeep-ai/seekdeep-typert-loader

English | [中文](README.zh.md)

Native Loader integration for generated Typert artifacts. The plugin requires `ctx.loader`, `ctx.typert`, and the compiled `ctx.typertArtifacts` directory; it provides none of those services itself.

During activation it scans existing Loader entries. It then follows Cordis `internal/plugin` lifecycle notifications, resolves each entry package in the native artifact directory, loads its compiled Host contribution, validates the typed manifest, and registers the contribution until the entry or this plugin unmounts. A factory that settles after either owner is gone is discarded.

`packages` lists additional package artifacts to register for plugins nested behind another Loader entry. Cordis fibers do not retain those nested plugins' package identities, so this boundary is explicit; every configured package must exist in `ctx.typertArtifacts` and expose a Host factory.

Discovered packages without a registered Host artifact are skipped. Missing, Host-less, loaded, and failed verdicts are cached for this loader's lifetime, so changing the artifact set requires a loader restart. A malformed artifact fails activation when already mounted; a later failure is logged without preventing unrelated packages from registering.

## Model Experience

None, as the loader only feeds [`ctx.typert`](../registry/README.md); consumers own any model-visible projection.

#### KV Cache effect

No direct effect.

## Known Limitations and Deferred Work

- Discovery loads only the Host face; Client runtimes need a separate composition owner before equivalent discovery is added.
- Native, WebAssembly, and compatibility package hosts must register their typed factory in `ctx.typertArtifacts`. Loader entries are discovered automatically; nested or non-Loader plugins require an explicit `packages` entry or direct `ctx.typert.register()` ownership.
