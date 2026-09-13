# Agent Note: 移除独立的 CLI demo

Status: implemented

[English](2026-08-08-remove-cli-demo.md) | 中文

## 问题

在 [`seekdeep --profile headless`](../architecture/2026-08-06-app-owned-command-line.md) 成为产品的一次性命令后，`@seekdeep-ai/seekdeep-cli-demo` 仍是承担同一工作的第二个应用包。它另行拥有一套可执行文件、参数语法、应用组装、取消生命周期、文本／JSON／stream-JSON 输出约定、构建产物、配套文档和测试套件。两个入口组装的树也不相同，因此 demo 成功不能证明已交付的 `headless` profile 可用，用户还必须在功能重叠的命令之间作出选择。

回放套件仍需要规范会话事件来固定组装后的后端行为。这一测试需求不需要已发布命令或兼容性约定。

## 决策

彻底删除 `@seekdeep-ai/seekdeep-cli-demo`：包括它的包、bin、解析器、应用插件、输出格式、测试、workspace 引用、生成目录条目和现行文档。不保留别名或兼容包。用户通过 `seekdeep --profile headless` 调用产品命令；stdout 上的最终文本、stderr 上的失败诊断、持久化、退出状态和关闭行为均由该命令负责。

`examples/headless-agent` 是显式测试组装。其 Loader 配置把 agent（智能体）主干、一个根 agent、JSONL 持久化和检查点策略挂载为独立配置行，不将其隐藏在应用组合包之后。支持层的 `seekdeep-loader-smoke` Rust crate 负责共享的直接 Agent 轮次辅助函数和隔离的子进程运行器。其 `src` 与 `lib` 兼容值分别选择开发产物和发布形态的已编译 Rust 产物；两种模式都不会启动 Node 或 tsx。三个 fixture 可执行文件仅通过该 crate 的 `fixture-bins` 功能启用。示例本地 driver 仍仅供测试使用，并且不定义受支持的产品输出格式。

## 考虑过的替代方案

- **保留 `seekdeep-cli-demo` 作为 `seekdeep --profile headless` 的别名或包装层。** 不予采纳：第二个 bin 和包会让同一功能继续存在两个可发现的归属方，却没有增加任何能力。
- **把 JSON 和 stream-JSON 标志移到 `seekdeep --profile headless`。** 不予采纳：当前没有产品消费方需要这些标志；沿用旧 demo 协议，只会为了保留测试机制而扩大规范 CLI（命令行界面）约定。
- **随包一并删除规范事件快照。** 不予采纳：这些快照固定了模型可见的组装行为，而只检查最终文本的产品验收无法观察这些行为。
- **保留应用插件，只删除它的 bin。** 不予采纳：隐藏的组装仍会重复显式的 headless profile，并掩盖测试叶节点挂载了哪些服务。

## 后果

这是有意为之的破坏性变更。`seekdeep-cli-demo`、它的 `--output-format` 选项以及对 `@seekdeep-ai/seekdeep-cli-demo/src/cli.ts` 的导入都不再可解析。本变更不提供公开的事件流替代接口；调用方使用 `seekdeep --profile headless` 执行一次性任务，需要结构化自动化时则必须选择现有的协议接口。

仓库通过仅供测试的基础设施保留后端回放覆盖，产品冒烟测试和 built-bin 验收则运行 `seekdeep --profile headless`。只有当独立的一次性包负责一套真正独立、带版本且不能归产品启动器所有的协议时，它才可以重新引入；第二种命令写法或输出 shim 并不足以构成理由。

## 验证

聚焦的 Loader 冒烟测试通过开发产物和发布形态的 Rust 产物覆盖显式组装。共享轮次辅助函数会驱动真实的 Agent 与 Session 事件路径；子进程运行器使用已编译 fixture 固定 stdin 关闭、输出捕获、精确退出状态、截止时间终止、准备／检查顺序和隔离目录清理。快照测试对比规范 JSONL 和持久化日志，产品验收覆盖 `seekdeep --profile headless`，文档检查及生成图谱／目录门禁则拒绝对已移除包的活跃引用。冻结的 Agent Note 归档保留为历史证据，不会被重写。
