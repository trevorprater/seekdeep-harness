# Agent Note: 原生 cordis crate 附带一个由测试固定的可运行示例

Status: implemented

[English](2026-09-18-native-cordis-runnable-example.md) | 中文

## 问题

内置的 host 插件是原生 Rust，cordis crate 是它们挂载进入的运行时，然而每一份 Cordis 讲解都面向 TypeScript 一侧：教程运行 `vendor/cordis/bin.js`，实操手册搭建的是 `packages/<group>/<pkg>` TypeScript 包，没有任何文档提到 `Plugin::new`。唯一端到端演练提供方、注入依赖的消费方和事件监听器的 Rust 代码，是固定源码语义的 oracle 测试，新人无法把它们当作入门来读。

## 决定

`crates/cordis/examples/hello_cordis.rs` 是该 crate 的一个 cargo 示例。它在同一个根上下文上挂载两个 `greeter` 提供方、一个注入 `greeter` 的 app 和两个 `greet` 观察者，dispose（资源释放）其中一个提供方，再挂载另一个，并打印每个阶段的输出以及 app fiber 的状态。该 crate 的 `Cargo.toml` 以 `test = true` 声明这个示例，因此示例内部的一个测试固定了这些输出和 fiber 状态，而该包的 `cargo test` 会运行它。crate 级文档和 Cordis 教程索引都指向这个示例。

该示例通过一个占位 `main` 为 `wasm32-unknown-unknown` 编译，因为浏览器构建没有可以驱动它的运行时。

## 考虑过的替代方案

**在 Cordis 教程中加一章 Rust。** 教程有意把读者带入 harness 的 TypeScript 组合（[教程决定](../../archived/process/2026-07-22-cordis-tutorial-docs.md)）；一个面向原生 crate 的章节会打破它单一启动器和真实输出的纪律。否决，改为从教程索引添加指向。

**不带测试的示例。** Cargo 在 `cargo test` 期间构建示例但不运行它们，打印的输出会悄然漂移。否决：`test = true` 只需一条 target 声明。

**放在 `crates/cordis/tests` 下的集成测试。** 测试文件能固定行为，但无法通过 `cargo run --example` 运行，而且该 crate 现有的测试是 oracle 移植而非入门。否决。

## 后果

- 原生 crate 的读者拥有一个输出被固定的可运行入门；改变生命周期顺序或 `FiberState` 转换会使示例的测试失败。
- 以 `test = true` 声明的示例 target 会给 `cargo test --workspace` 增加一个测试二进制。
- 该示例仅限原生；浏览器构建把它编译为一个空程序。
