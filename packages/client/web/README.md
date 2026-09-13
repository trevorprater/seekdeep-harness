# @seekdeep-ai/seekdeep-client-web

English | [中文](README.zh.md)

Web shell kernel: `new AppWebEntry(el, seams?).run()` mounts the whole client through the two-stage boot (web2). Stage one (module face): build the client module system (`@seekdeep-ai/seekdeep-client-modules`) over the host-pushed entry graph (`window.__SEEKDEEP_BOOT__`) and prefetch the `immediately` tier in parallel — bundle execution registers factories only. Stage two (plugin face): mount the compiled Rust/WASM Cordis Loader with the module system injected through its `internal` contract, create one loader entry per graph row plus the shell-own app-shell assembly entry (tree.import materializes each module), and gate AppRoot on the settle (loader quiesced + every entry fiber ACTIVE → full UI in one switch). Composition is entirely the host graph's: the roster and the immediately tier live in the composing app; the shell makes zero composition decisions.

Shell self-sufficiency (web2 hard rule): the kernel value-imports no plugin package — this crate's compiled Rust/WASM boot status store and signals keep the loading page available while (and especially when) plugins fail. The app-shell assembly (`@seekdeep-ai/seekdeep-client-app-shell`, a shell-owned pseudo entry with no npm package behind it) is the only module registered through `registerStatic`; it inject-waits on slots/sessions/layout like any plugin.

`PLATFORM_MODULES` in `crates/client-web/src/lib.rs` is the single source of truth for shell-held shared modules. The generated Web ESM wrapper seeds those exact values, while `cargo xtask web-frontend` emits only the mount binding and Vite build configuration; runtime plugin packages stay outside the optimized shell and arrive through the host graph.

The optional override parameter `seams` forwards the module system's `loadBundle` transport override (`BootSeams`) for environments where external `<script>` execution cannot reach the page context; ordinary browser callers omit it.

The shell owns browser-title projection. With a selected session carrying a durable title, it renders `<session title> — <existing HTML title>` and reacts to later title revisions; no selection or a selected untitled session preserves the existing title, and shell unmount restores it. The existing HTML title remains the configurable product suffix.

## Model Experience

None, as the entry shell boots the browser plugin tree; nothing here reaches a model request.

#### KV Cache effect

None; this package neither assembles nor sends a provider request.

## Known Limitations and Deferred Work

- **One-shot rendering by design** — the UI waits for the boot settle; a single entry failure keeps the loading page with a loud per-entry report, no partial availability (progressive rendering returns with its own project).
- **Narrow-window shell behavior lacks an assembled walkthrough** — ui-layout implements the concession chain, but this package has no shell-level narrow-viewport acceptance case.
