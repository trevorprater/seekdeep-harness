# SeekDeep Harness

English | [中文](README.zh.md)

SeekDeep Harness (`seekdeep`) is the all-Rust, behavior-compatible port of [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness).

It keeps the source's architecture where **everything is a plugin**, powered by [Cordis](https://github.com/cordiverse/cordis), whose design is described in [_A Programming Paradigm for Spatiotemporal Composability_](https://github.com/cordiverse/paper).

## Developer preview

SeekDeep Harness is currently in _developer preview_ and is iterating rapidly. **THERE WILL BE COMPATIBILITY-BREAKING CHANGES.**

## Port status

`SOURCE_SNAPSHOT` identifies the exact DeepSeek Harness revision used as the parity oracle, and `porting/parity.json` records every surfaced behavior with its translation targets and evidence. Deliberate deviations from the oracle are recorded in `porting/DEVIATIONS.md`.

The port exposes the `seekdeep` command and preserves the source harness's plugin composition, durable session log, model/tool lifecycle, configuration, server, client, SDK, sandbox, and web behavior.

Runtime code reload uses native Rust as the host, preserves the source's model-authored dynamic package surface through Rust-owned compatibility infrastructure, and uses explicit WebAssembly or process boundaries for reloadable binary code. See [`porting/DYNAMIC_PLUGIN_RELOAD.md`](porting/DYNAMIC_PLUGIN_RELOAD.md) for the source-driven architecture, mechanism-specific lifecycle rules, open decisions, and verification requirements.

## Run

### Run from `npm`

Install `Node.js`, then run:

```sh
npx @seekdeep-ai/seekdeep web
```

The command starts the Web UI and prints its URL. See [Web UI guide](docs/user/guide/index.md).

### Run from source

To run from a repository checkout:

```sh
git clone https://github.com/trevorprater/seekdeep-harness.git
cd seekdeep-harness
pnpm install
pnpm run build
pnpm seekdeep web
```

## Community and support

- Feel free to submit feedback or bug reports through [GitHub issues](https://github.com/trevorprater/seekdeep-harness/issues).
- Add the [`seekdeep-plugin`](https://github.com/topics/seekdeep-plugin) topic to your plugin repository for discoverability.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Development

Start with the [development guide](docs/development.md) and [architecture documentation](docs/architecture.md).

For agents, follow [AGENTS.md](AGENTS.md).

## License

[MIT](LICENSE)

Third-party dependencies and their licenses are disclosed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
