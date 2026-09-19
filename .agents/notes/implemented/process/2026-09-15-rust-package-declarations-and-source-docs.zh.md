# Agent Note: Rust 移植中的包声明与源码文档

Status: implemented

[English](2026-09-15-rust-package-declarations-and-source-docs.md) | 中文

## 问题

实现迁移到 Rust 后，外部语言消费方仍需要固定源码包的完整公开类型。局部声明集可能通过狭窄的消费方检查，却遗漏其他包。将 Host 与 Client 项目展平还会合并各自独立的 Cordis 接口，改变推断出的公开类型。

文档有两类不同输入：可编译示例消费公开包声明，而逐字类型与 JSDoc 示例复现原始源码片段。编译器输出的声明可能在类型不变的情况下改变标点和公开类投影，因此不能替代后一类输入。

## 决策

[声明发布器](../../../../xtask/src/remote_contracts/declarations.rs) 沿原有项目引用捕获完整的固定 solution。生成的 Remote 声明在编译前进入编译器虚拟文件系统，捕获期间的所有输出都留在内存中。Host 与 Client 保持独立的 Program。捕获会拒绝编译器诊断，并记录原始路径、包元数据、声明内容与源码修订版。

Rust 发布器将记录的声明写入各包发布的 `lib` 目录，并应用产品身份重命名。每条 Client 构建路径都以此发布器收尾，专用包构建器也不例外。`host-assets` 在 Host 构建中准备声明，独立运行的 `doc-typecheck` 在编译示例前刷新声明。协同运行的 `doc-typecheck:contracts-ready` 命令消费已有构建输出。即使实现位于 Rust crate，包元数据仍属于公开约定。

[消费方检查](../../../../xtask/src/remote_contracts/declarations/consumer.rs) 将包 manifest 与可发布声明文件复制到隔离的 Host、Client fixture 中，不包含实现源码或 workspace 内嵌依赖安装。严格的库检查与反例消费方会约束品牌化身份、具体返回类型、环境变量名称，以及 Host service 与 Client Remote 访问之间的隔离。公开的 lib 构建会在所有包构建器完成后验证声明新鲜度并运行这些消费方。

[源码 oracle 读取器](../../../../crates/repository-tools/src/source_oracle.rs) 依次解析 `SEEKDEEP_PARITY_SOURCE`、相邻 checkout 或 [SOURCE_SNAPSHOT](../../../../SOURCE_SNAPSHOT) 中记录的位置，并拒绝修订版漂移。声明等价检查直接从固定 Git 对象读取缺失的源码文件，在产品重命名后保留原有结构与 JSDoc 比较。包路径检查接受同一提交中存在的规范路径。相对 Markdown 链接仍必须在移植仓库内解析；指向原始实现的链接会明确标识固定源码修订版。

[双 Program 规则](2026-07-22-tsconfig-solution-root-two-aggregates.md) 与 [Remote 构建顺序](2026-08-08-api-remotes-generated-contract-build.md) 继续保留项目归属和生成依赖的理由。本记录负责它们在 Rust 兼容声明中的实现方式；两项既有决策均未被完全取代。

## 考虑过的替代方案

- **将所有源码包展平到一个编译器 Program。** Host 与 Client 的声明合并会共享同一身份，改变公开约定。
- **将整个 workspace 包符号链接到消费方 fixture。** 内嵌依赖安装可能引入多个 Cordis 类型身份，并让未发布的实现文件满足导入。
- **放宽源码等价规范化以接受输出头文件。** 这会丢弃源码对声明和 JSDoc 的精确比较。读取固定源码对象可以保留该权威。

## 后果

声明发布直接使用已提交的捕获数据，不需要源码 checkout。刷新捕获数据以及检查原始文档需要固定 oracle。声明与文档检查证明 API 和规范的一致性；运行时 export 仍需要对应的 Rust 编译实现与完整装配执行检查。
