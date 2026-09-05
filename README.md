# SeekDeep Harness

English | [中文](README.zh.md)

SeekDeep Harness is the all-Rust, behavior-compatible port of DeepSeek Harness. The port is in progress; `SOURCE_SNAPSHOT` identifies the exact source revision used as the parity oracle, and `porting/parity.json` records each translated surface and its evidence. Deliberate deviations from the oracle are recorded in `porting/DEVIATIONS.md`.

The finished application will expose the `seekdeep` command while preserving the source harness's plugin composition, durable session log, model/tool lifecycle, configuration, server, client, SDK, sandbox, and web behavior.

Runtime code reload uses native Rust as the host, preserves the source's model-authored dynamic package surface through Rust-owned compatibility infrastructure, and uses explicit WebAssembly or process boundaries for reloadable binary code. See [`porting/DYNAMIC_PLUGIN_RELOAD.md`](porting/DYNAMIC_PLUGIN_RELOAD.md) for the proposed source-driven architecture, mechanism-specific lifecycle rules, open decisions, and verification requirements.
