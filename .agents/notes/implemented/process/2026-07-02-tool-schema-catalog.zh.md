# Agent Note: 生成的工具 schema 目录（启动并采集）

Status: implemented

[English](2026-07-02-tool-schema-catalog.md) | 中文

## 问题

仓库此前没有一份统一的参考文档来记录实际暴露给模型的工具名称、描述与 JSON Schema。源码声明分散各处且在运行时组合，而既有的 Cordis 参考和子系统页面覆盖的是接线与词汇，而非工具。

## 决策

目录通过**启动每个 Rust 工具包并读取其已注册 schema** 来生成，而不是解析源码。[`xtask/src/tool_catalog.rs`](../../../../xtask/src/tool_catalog.rs) 在全新的 Cordis `Context` 上挂载每个包，并提供 `SystemPrompt`、`ToolRuntime` 及其注册过程读取的服务；调用 `ToolRuntime::schemas()`，取得发送给模型的确切有序 `ToolSchema` 值；将上下文 dispose（资源释放）至完全停稳；然后为每个包渲染一个 `## <package>` 章节，每个工具附带一个 ` ```json ` `parameters` 块。`cargo xtask tool-catalog` 会重新生成按 manifest（元数据清单）排序、工具按名称排序的确定性输出，`--check` 则拒绝陈旧的提交副本。`verify-tool-catalog` 包脚本会在 `doc-sync`（文档同步门禁）内调用该 Rust 检查，因此文档变更和 CI 使用同一条新鲜度路径。

### 为何启动而非解析（核心要点）

已固定源的 Cordis 目录可以使用静态源码遍历，因为每个事件和服务名称都能往返映射到声明。**工具 schema 在静态层面不可知**，因此静态 TypeScript 或 Rust 分析会产出一份说谎的文档：

- `tool-todo` 在运行时将状态常量展开为 `["pending","in_progress","completed"]`；语法检查看到的是构造过程，而非注册后的字面量。
- 描述会纳入解析后的上限与配置，因此模型读取的是最终字符串，而非源码片段。
- `tool-subagent` 在加载时选择工具名称，包括随产品发布的 `subagent_fork` 别名。
- MCP 兼容插件可以不经类型化 `define_tool` 辅助函数而直接注册**原始 JSON Schema**，因此枚举辅助函数调用点会遗漏。

唯一准确的真源，是插件加载后注册表实际持有的 schema。启动插件是把[测试策略](../../../../docs/testing.md)中「验证现实，而非自我报告」的准则应用到文档生成器：读取已发布产物，而非重新推导一份。

### 恢复「不会静默遗漏」的保证

启动过程没有可枚举的声明集合，因此新工具包可能被遗忘。`assert_manifest_complete` 会将 Rust 启动 manifest 与已固定源检出中的每个 `packages/*/tool-*` 目录比较，从而恢复这项保证。任何遗漏的源包都会使生成器失败，继而使 `doc-sync` 失败，直至对应的 Rust 启动配方就位。

### 手动维护的启动 manifest 是无法省去的策略

已固定文件系统负责发现所需的包清单，完整性守卫负责拒绝遗漏。`tool_packages()` 仍然为每个包持有一份显式的 Rust 启动配方，因为所需的 Service Provider、作用域注册和配置选择属于策略，不是目录布局或注入名称能够安全确定的事实。

### 范围

manifest 覆盖与已固定 `packages/*/tool-*` 清单对应的每个随产品发布的工具，以及核心工具注册表、规划模式和 Schedule 所拥有的 schema。每个包都使用部署默认值启动；如果必须作出选择，则由对应的目录说明记录。仅用于示例的工具不在范围内。

目录的单位是包，而非经过配置的每个工具实例。每个包以默认配置启动一次；加载时的别名（如 `subagent_fork`）会注明，但不枚举所有部署配置组合。部署清单覆盖的是一个独立且无界的范围。

### 使用普通 `json` 围栏

schema 块使用 ` ```json `，而非自定义的 `ts` 系围栏。`doc-typecheck` 只提取 `ts*` 围栏，因此 JSON 块对它不可见——无需 `BlockKind` 接线（不同于 Cordis 目录的 `ts cordis-catalog` 围栏，后者需要加入白名单以避免裸签名片段被编译）。

## 验证

Rust 生成器测试会启动每个包，断言完整的 52 项名称目录，执行作用域化的 Schedule 与 report 注册，保留运行时展开的 todo 枚举，验证 Rust 源码归属和 `subagent_fork` 说明，拒绝不完整的 manifest 与空采集结果，并固定 Markdown 渲染。源生成器的保证测试套件仍是 oracle（判定准则），而生成目录的差分要求每个工具名称、描述和格式化后的 JSON Schema 块都与已固定源逐字节一致；这还会固定 schema 对象的键顺序，因为该顺序会影响序列化后的模型请求和请求缓存字节。

## 曾考虑的替代方案

- **静态 TypeScript 或 Rust 源码分析**：工具 schema 在静态层面不可知；运行时值、解析后的描述、配置选定的名称和原始注册，都会让从语法推导出的文档说谎。
- **从各包的 inject 推断启动配方**：属于[发现包清单提案](../../proposed/process/2026-06-20-discover-package-inventory.md)所警告的「过度聪明」路径；配方保持为手写策略，清单由文件系统发现并由完整性守卫把关。
- **为 schema 块使用自定义 `ts` 系围栏**：不必要。普通 ` ```json ` 围栏对 `doc-typecheck` 不可见，无需 `BlockKind` 白名单。

## 后果

- 目录不会发生漂移：提交文件未反映的工具 schema 变化会使 `doc-sync` 和 CI 中的 `verify-tool-catalog` 失败。新增的 `tool-*` 包若未加入 manifest，会直接使完整性守卫失败。
- 工具描述文本有唯一归属——源码中 `defineTool` 的 `description`——生成的条目质量取决于它，与 Cordis 目录对事件 JSDoc 施加的强制力相同。
- 生成器通过 `cargo xtask` 链接并执行 Rust 工作区包；它不需要 Node 运行时或单独构建的包产物。
- 未来某个工具背后新增一个能力 seam，意味着 manifest 中需要新增一条配方条目（声明要挂载哪些 seam）。这正是上文指出的有意为之的手写成本；仅在新增工具包时才需变更。
