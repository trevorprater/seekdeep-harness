# `seekdeep-host-frontend-static`

English | [中文](README.zh.md)

SPA dist server for the Web shell: a plugin configured with `{ distIndex }` that claims the webserver's single fallback seat and serves the built frontend directory with the shell's locked semantics. Traversal outside the dist root is 403, any miss falls back to `index.html` with HTTP 200, unknown extensions ship as `application/octet-stream`, and non-GET/HEAD requests without a matching named route receive 405. Every index response runs through the webserver's registered index taps (`apply_index_taps`), which is how the boot manifest reaches the page. `distIndex` is an assembly fact supplied by the composing application; deployments do not hardcode it.

The fallback seat is single-owner and effect-scoped. Disposing the plugin fiber releases the seat, after which the unclaimed webserver answers 404.

## Model experience

None. The crate serves browser assets; nothing here reaches a model request.

#### KV cache effect

None; this crate neither assembles nor sends a provider request.

## Known limitations and deferred work

- **The starter MIME table is minimal** — it covers the emitted asset set plus the shipped PWA manifest; other extensions fall back to `application/octet-stream` until an asset class actually ships.
