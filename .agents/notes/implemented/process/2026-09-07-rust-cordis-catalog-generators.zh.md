# Agent Note: Cordis catalog 生成器是覆盖 pinned 源码树的 Rust 门禁

Status: implemented

[English](2026-09-07-rust-cordis-catalog-generators.md) | 中文

## 问题

源码仓库用四个 TypeScript 脚本生成 Cordis 文档与面向模型的 catalog：`gen-cordis-catalog.ts`（各子系统的 service/event 区域、继承层页面、运行时 API 模块与五个详细的核心 API 页面）、`gen-client-catalog.ts`（浏览器 slot 表面）、`gen-cordis-inspect-catalog.ts`（Client inspect catalog），以及共享的 `cordis-walk.ts`、`slot-walk.ts`、`cordis-core-api.ts` 和 `jsdoc.ts` 辅助模块。移植仓库提交了生成的文档和 Rust 运行时内嵌的 JSON 数据，但 JSON 是通过在 `tsx` 下求值源码生成的 TypeScript 模块捕获的，没有任何 Rust 代码能够重新生成或做新鲜度检查。

## 决定

每个生成器都是 Rust。纯判定逻辑位于 `seekdeep-repository-tools`，并由源码规范的移植版本证明：`jsdoc`（散文、标签、参数与返回值完整性）、`cordis_catalog_partition`（`spliceRegion`、`walkPartitionProblems`、`maybeRecordPair`）、`client_catalog`（契约校验、条目投影、每个 slot 的报告预算、数据模块渲染器）以及 `cordis_core_api`（五个页面渲染器）。词法扫描（`cordis_walk`、`slot_walk`）用 `oxc` 解析 TypeScript，并通过 `ts_lexical` 复现源码依赖的编译器约定——带与不带 `JSDoc` 的 `getStart`、`getFullStart` 的前导 trivia 规则、计算属性名文本、行号。

`cargo xtask cordis-catalog`、`cargo xtask client-catalog` 与 `cargo xtask cordis-inspect-catalog` 针对 pinned 源码检出（`--source`，默认为 oracle）编排它们：用原生 Typert 分析器分析 workspace，然后比较或写入移植仓库的产物——双语的 `docs/subsystems/*.md` 区域（仅在写入局限于区域内时重新记录配对）、`docs/cordis-api/inherited.md`、五个核心 API 页面、`crates/tool-cordis/data/api-catalog.json`、`crates/cordis-client-runner/data/slot-catalog.json` 与 `crates/cordis-client-runner/data/api-catalog.json`。`--check` 是新鲜度门禁。

精选策略表（`SERVICE_PAGE`、walk 豁免表、`EVENT_SCOPE_PAGE`、`LINK_MAP`、`TYPE_LINK_EXEMPTIONS`、`CORDIS_CATALOG_POLICY`、`CLIENT_SERVICES`、`CLIENT_EVENTS`）是 Rust 常量，并在每次运行时与源码脚本对钉：命令通过 pinned TypeScript 库读取源码常量，任何表漂移都会在渲染之前失败。

文档与数据保持移植仓库已经提交的标识：数据产物使用数据重命名（`@deepseek-ai/dsh-` → `@seekdeep-ai/seekdeep-`、`dsh-` → `seekdeep-` 等），文档额外重命名 `dsh.client` manifest 字段与 `@dshScopeScan` 标签。

## 考虑过的替代方案

**继续通过求值源码生成的 TypeScript 来捕获 catalog 数据。** 否决：产物将依赖移植仓库并不拥有的 Node 工具链，而且生成器的分区、契约与完整性检查仍然没有移植。

**用托管的 TypeScript 编译器而不是 `oxc` 做扫描。** 否决：这些扫描在设计上就是词法的（源码刻意不为它们建立类型检查程序），`oxc` 让它们保持快速，并独立于它们所兜底的编译器投影。

## 后果

- 生成器需要 pinned 源码检出及其 `node_modules/typescript`；移植仓库自身没有可扫描的 TypeScript 包源码。
- 精选表的修改必须在两个仓库中同时进行，否则门禁会大声失败，这正是预期的 fail-closed 分区行为。
- `cargo xtask cordis-catalog` 写入双语页面区域，并且只为局限于区域内的写入刷新 `.i18n.yaml` 记录，因此翻译漂移仍由配对门禁暴露，而不是被悄悄修复。
