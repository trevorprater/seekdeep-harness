# `@seekdeep-ai/seekdeep`

English | [中文](README.zh.md)

The `seekdeep` command is the product launcher for profiles: ordered stacks of plugin-bundle patch layers under the user's own overrides. [`src/args.ts`](src/args.ts) owns the command grammar, and [`src/bin.ts`](src/bin.ts) loads only the selected runner. Invalid commands, options from another mode, configuration errors, and boot failures exit nonzero.

## Entry modes

| Command | Purpose |
|---|---|
| `seekdeep --profile <name>` | Boot the named profile under `$SEEKDEEP_HOME/profiles/<name>`. |
| `seekdeep --profile headless "job"` | Run one fresh persisted session, print the final answer, and exit. |
| `seekdeep web` | Alias of `--profile web`. |
| `seekdeep plugin --profile <name> <pnpm args>` | Manage a profile's plugins by forwarding to pnpm in the profile directory. |

The invoking directory is the default workspace root. The `web` and `headless` profiles auto-initialize on first use from shipped templates; any other profile must be created through `seekdeep plugin`.

## App arguments

The launcher parses only its own flags and hands everything after them to the booted profile, where any injected app plugin may parse the shared immutable snapshot ([`seekdeep-cmdline`](../../packages/boot/cmdline/README.md)). Launcher flags therefore come first, and the first token the launcher does not recognize starts the app's arguments:

```sh
seekdeep --profile web --port 8080       # --port belongs to the web app
seekdeep --profile tui --resume <id>     # example, assuming the tui profile is installed; --resume belongs to the terminal app
seekdeep --profile headless "run the tests"
seekdeep --profile web --help            # the web app's flags, not the launcher's
seekdeep --help                          # the launcher's own help
```

## Profiles

A profile directory holds a `package.json` (out-of-tree plugin dependencies plus the profile manifest `seekdeep.profile` with its ordered `bundles` list) and a `cordis.patch.yml` (the user's own patch layer).

The tree composes over an empty root:
- each bundle's patch in `seekdeep.profile.bundles` order
- then the profile's `cordis.patch.yml`, then the home-level `$SEEKDEEP_HOME/cordis.patch.yml`
- then `--patch` overlays

Bundles named in `seekdeep.profile.bundles` resolve from the seekdeep installation first (`@seekdeep-ai/seekdeep-base`, `@seekdeep-ai/seekdeep-web-app`, `@seekdeep-ai/seekdeep-headless`), then from the profile's own `node_modules`, where pnpm installs out-of-tree plugins.

Use `--dump-default-config` and `--dump-config` to inspect the composed tree without booting it.

The [CLI behavior reference](reference/README.md) owns exact layer precedence, flags, shutdown behavior, deployment defaults, and source execution.

## Development

Production runs require built package and frontend artifacts. From the repository root, run `pnpm run build` separately, then use `pnpm seekdeep <args...>` to run the TypeScript entry and forward every argument; the [source-execution reference](reference/README.md#source-execution) owns the module-resolution contract.
