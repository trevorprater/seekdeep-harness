# Agent Note: 原生 Typert 分析器以库的形式托管 TypeScript 编译器

Status: implemented

[English](2026-09-06-native-typert-analyzer-hosts-the-compiler.md) | 中文

## 问题

Typert 生成器的模型、渲染器、发射器与 catalog 投影已经是 Rust，但产出模型的 workspace 分析器仍然作为 pinned 源码的 TypeScript 程序运行在 Node 下。每一份生成的 Remote 描述符都从 Rust 工具链之外捕获的模型出发，而源码的分析器测试（`type-model`、`remote-model`、`tsdown-plugin`、`tools-catalog`、`cordis-catalog`）没有可以对照运行的 Rust 实现。

没有类型检查器就无法重新实现分析器：Remote codec 投影通过 `checker.getTypeFromTypeNode` 解析声明合并后的 mapped 与 conditional 类型，跨 face 链接依赖模块解析，write 模式则用 `typeToTypeNode` 打印推断出的注解。第二套类型检查器永远不会在源码 workspace 上与 TypeScript 达成一致。

## 决策

分析器是 Rust 代码，它在 Rust 拥有的 V8 isolate 中托管 pinned 的 `typescript.js` 库（`crates/typert-generator/src/analyzer/native/`）。源码分析器做出的每一个决定都由 Rust 拥有——注册、发现、诊断门控、导出记录、service 与 event 收集、声明建模、Remote 调用契约、codec 投影、write 模式编辑、分批与合并——编译器库只通过类型化的值辅助函数提供解析、绑定、检查、打印和模块解析。

编译器只能通过用 `ts.setSys` 安装的 Rust `System`（`system.rs`）看到文件系统：识别 BOM 的读取、排序后的目录列表、规范化的 realpath 与大小写敏感性检测都镜像 Node 的 system，因此 `createCompilerHost`、`resolveModuleName` 与 `parseJsonConfigFileContent` 的行为与 Node 下一致。编译器 host、已解析的配置、注册清单和默认库解析结果保存在 `WorkspaceCaches` 中，并像源码的 memo 一样在分批程序间复用。

库的定位方式与源码包依赖相同：被分析 workspace 之上的 `node_modules/typescript/lib/typescript.js`，或者通过 `SEEKDEEP_TYPESCRIPT_LIBRARY` 显式指定。重命名后的标识与 pinned 的拼写同时被接受（`@seekdeep-ai/cordis` 与 `@deepseek-ai/cordis`，manifest 中的 `seekdeep` 与 `dsh` 键），因此 oracle workspace 无需改动即可分析。

JSON 请求运行器 `seekdeep-typert-generator` 是兼容绑定使用的唯一入口。`cargo xtask typert-corpus` 把 pinned 的生成器测试复制到适配器旁边，适配器的每一次调用都是一个运行器请求，因此源码的 Vitest 语料及其提交的快照就是可执行规范。`cargo xtask remote-contracts --source` 用 Rust 分析器分析 Remote 包，并要求 face 模型与源码分析器的结果相等后才发射描述符。

## 考虑过的替代方案

**继续在 Node 下从源码分析器捕获模型。** 不予采纳：生成的描述符会依赖一个移植版本并不拥有的 TypeScript 程序，分析器测试也仍然没有移植。

**把分析器移植到 Rust 的 TypeScript 前端上。** 不予采纳：没有 Rust 检查器能复现 TypeScript 的类型求值、声明合并与模块解析；codec 投影会偏离源码 workspace 自身编译器给出的结果。

**在嵌入引擎中直接运行 `analyzer.ts`。** 不予采纳：这只改变了进程边界，行为仍然留在 JavaScript 中，违背移植版本对 Rust 所有权的要求。

## 后果

- 原生测试在磁盘上没有 TypeScript 库时无法分析 workspace；语料与契约门控固定使用源码 checkout 的库。
- 模型依赖的 JavaScript 字符串语义（`\s`、`trim`、UTF-16 偏移、number 与 bigint 格式化）在 `jstext.rs` 中显式实现，而不是用 Rust 默认行为近似。
- 每个运行器请求都会重新加载编译器；分批分析与 catalog 分析共享一个进程和一份 memo，与源码的单进程缓存一致。
- 模型转换会在编译器调用之间递归穿过 Rust 栈帧，因此分析运行在专用的 512 MiB 线程上（`run_with_stack`），并在首次初始化时按该线程设置引擎的栈保护；真实 workspace 会溢出默认的栈保护。
