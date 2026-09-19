# SeekDeep Harness

[English](README.md) | 中文

SeekDeep Harness（`seekdeep`）是 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 的全 Rust、行为兼容移植版本。

它保留了源项目**一切皆插件**的架构，并由 [Cordis](https://github.com/cordiverse/cordis) 驱动，其设计参见论文 [_A Programming Paradigm for Spatiotemporal Composability_](https://github.com/cordiverse/paper)。

## 开发者预览

SeekDeep Harness 目前处于 _开发者预览_ 阶段，正在快速迭代。**未来将出现破坏兼容性的变更。**

## 移植状态

移植仍在进行中；`SOURCE_SNAPSHOT` 标识作为 parity oracle 的精确源修订，`porting/parity.json` 记录每一项已覆盖行为及其移植目标与证据。相对于 oracle 的有意偏离记录在 `porting/DEVIATIONS.md` 中。

移植版本提供 `seekdeep` 命令，同时保留源 harness 的 plugin composition、durable session log、model/tool lifecycle、configuration、server、client、SDK、sandbox 与 web 行为。

Runtime code reload 以 native Rust 作为 host，通过 Rust-owned compatibility infrastructure 保留源项目中由模型编写的 dynamic package surface，并为可 reload 的 binary code 使用显式 WebAssembly 或 process boundary。source-driven architecture、各机制的 lifecycle rule、open decision 与 verification requirement 见 [`porting/DYNAMIC_PLUGIN_RELOAD.md`](porting/DYNAMIC_PLUGIN_RELOAD.md)。

## 运行

### 通过 `npm` 运行

安装 `Node.js`，然后运行：

```sh
npx @seekdeep-ai/seekdeep web
```

该命令会启动 Web UI 并打印其 URL。详见 [Web UI 指南](docs/user/guide/index.md)。

### 从源码运行

如需从仓库源码运行：

```sh
git clone https://github.com/trevorprater/seekdeep-harness.git
cd seekdeep-harness
pnpm install
pnpm run build
pnpm seekdeep web
```

## 社区与支持

- 欢迎通过 [GitHub issues](https://github.com/trevorprater/seekdeep-harness/issues) 提交反馈或 bug 报告。
- 为你的插件仓库添加 [`seekdeep-plugin`](https://github.com/topics/seekdeep-plugin) 话题，便于被发现。

## 参与贡献

参见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 开发

请先阅读[开发指南](docs/development.md)与[架构文档](docs/architecture.md)。

面向 agent：请遵循 [AGENTS.md](AGENTS.md)。

## 许可证

[MIT](LICENSE)

第三方依赖及其许可证见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
