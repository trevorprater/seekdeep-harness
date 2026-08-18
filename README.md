# SeekDeep Harness

SeekDeep Harness is the all-Rust, behavior-compatible port of DeepSeek Harness. The port is in progress; `SOURCE_SNAPSHOT` identifies the exact source revision used as the parity oracle, and `porting/parity.json` records each translated surface and its evidence. Deliberate deviations from the oracle are recorded in `porting/DEVIATIONS.md`.

The finished application will expose the `seekdeep` command while preserving the source harness's plugin composition, durable session log, model/tool lifecycle, configuration, server, client, SDK, sandbox, and web behavior.

Runtime code reload uses native Rust as the host and Rust-produced WebAssembly as the reloadable plugin boundary. See [`porting/DYNAMIC_PLUGIN_RELOAD.md`](porting/DYNAMIC_PLUGIN_RELOAD.md) for the accepted architecture, lifecycle protocol, rollback rules, and verification requirements.
