# Agent Note: 使用 Rust 生成配置目录

Status: implemented

[English](2026-08-25-rust-config-catalog-generator.md) | 中文

## 问题

插件配置目录是一项可执行的 parity 检查，而不是复制的文档。它必须对钉住源代码中的每个包分类，连同 JSDoc 粘贴各插件完整的配置声明闭包，公开外部类型依赖，并拒绝配置类型未声明的任何可枚举 Schemastery 路径。如果继续让 `scripts/gen-config-catalog.ts` 拥有这项权威，Rust 端口就必须携带 TypeScript 与 Node 工具链；如果只保留最后一次生成的 Markdown，则会静默丢失新鲜度和负路径强制检查。

## 决策

`cargo xtask config-catalog` 负责收集、校验、渲染和新鲜度检查。Rust 生成器使用 OXC 解析钉住 `SOURCE_SNAPSHOT` 中的包入口，将声明转换为自有类型图，跟随包内导入和 workspace 重导出链，保留逐字声明范围，并在返回前聚合所有违规。其 schema 遍历处理对象、数组、union、链式 refinement 与 intersect 组合形式；类型遍历处理 interface 及继承、alias、literal、数组、intersection、union、indexed access、utility wrapper 和 workspace 引用。未知外部类型保持未知，不会变成错误的字段缺失报告。

生成的英文目录会应用已批准的 SeekDeep 产品重命名，同时保留源声明与路径 oracle。`cargo xtask config-catalog --check` 将完整渲染产物与 `docs/config-catalog.md` 比较。经评审的中文对侧文件仍通过文档配对维护，而不是由生成器输出。

Rust 差分测试套件镜像源生成器的全部 24 个用例；manifest 条目标记为已验证前，完整语料运行必须收集钉住 checkout 中的每个合格包。

## 考虑过的替代方案

**从 Rust 调用 TypeScript 生成器。** 这会保留旧实现，但也会把 Node、TypeScript 和 workspace 包安装保留为生产工具依赖，违反 Rust 端口边界。

**把已提交的 Markdown 当作端口。** 静态副本无法检测新包、缺少文档的配置成员、类型冲突或只存在于 schema 的字段，因此会把可执行文档变成未经审计的快照。

**只从原生 Rust 配置结构生成。** 原生类型是目标实现权威，但钉住的 TypeScript 声明仍是差分 oracle。丢弃其闭包和 schema 交叉检查，会让源行为从完成证明中消失，而不是证明有意采用的 Rust 表达。

## 影响

配置目录检查不需要 JavaScript 运行时，并会对自有 OXC 投影无法分类的语法明确失败。当 oracle 采用新的声明形式时，生成器显式维护的源类型与 Schemastery 表达式模型必须同步扩展。生成的英文页面与经评审的中文页面仍需一起变更；只有英文页面受新鲜度生成约束。
