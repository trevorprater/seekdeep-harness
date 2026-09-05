# SeekDeep Harness

[English](README.md) | 中文

SeekDeep Harness 是 DeepSeek Harness 的全 Rust、行为兼容移植版。移植仍在进行中；`SOURCE_SNAPSHOT` 标识作为 parity oracle 的精确源修订，`porting/parity.json` 记录每个已移植 surface 及其证据。相对于 oracle 的有意偏离记录在 `porting/DEVIATIONS.md` 中。

完成后的应用将提供 `seekdeep` 命令，同时保留源 harness 的 plugin composition、durable session log、model/tool lifecycle、configuration、server、client、SDK、sandbox 与 web 行为。

Runtime code reload 以 native Rust 作为 host，通过 Rust-owned compatibility infrastructure 保留源项目中由模型编写的 dynamic package surface，并为可 reload 的 binary code 使用显式 WebAssembly 或 process boundary。拟议的 source-driven architecture、各机制的 lifecycle rule、open decision 与 verification requirement 见 [`porting/DYNAMIC_PLUGIN_RELOAD.md`](porting/DYNAMIC_PLUGIN_RELOAD.md)。
