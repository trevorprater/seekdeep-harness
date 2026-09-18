# Agent Note: Host 包入口由编译后的 Host 生成

Status: implemented

[English](2026-09-18-host-package-entries-from-the-compiled-host.md) | 中文

## 问题

每个 `packages/<group>/<package>` 清单仍然声明着固定源码用 TypeScript 构建出的 npm 入口点：`main` 与 `exports["."]` 指向 `lib/index.js`，`exports["./invariant"]` 指向 `lib/invariant.js`，少数包还加上 `lib/types/types.js`、`lib/worker.cjs`、`lib/startup.js`、`lib/bin.js` 或 Typert 产物 `lib/typert.host.js`。移植把 173 个 Host 包的运行时编译进了 `seekdeep` 二进制，`cargo xtask remote-declarations` 也已经从捕获的声明模型写出它们的 `lib/types/*.d.ts`，但没有任何步骤产出这些 JavaScript 文件。于是 `publint` 在 CI 中让每个 Host 清单失败，排在它之后的七个门禁（构建包不变量、lint 与重复检测、两条快照通道、文档类型检查、NodeNext 类型、构建后二进制冒烟）从未运行，而 NodeNext 消费方接下来也会失败，因为四个 `./typert` 导出同样没有声明文件。

## 决定

`cargo xtask host-entries` 为每个 `build-client` 不编译的二级包，写出其清单在 `main`、`bin` 或 `exports` 下声明的每个 JavaScript 文件。不变量伴随插件保持源码发布的 Loader 契约：`name` 取自捕获的 `invariant.d.ts` 中的规范伴随名，`inject` 为 `['invariants']`，`apply` 向不变量注册表注册该包。凡 `crates/invariants/src/noop/catalog.rs` 记录为空操作的，其安装器就是固定源码的空操作；否则安装器抛出并点名编译后的 Host，因为那些检查在 Rust 中运行。其余每个运行时入口在加载时立即抛出，点名包、入口和 `seekdeep` 可执行文件，消费方永远不会运行一个悄悄替代编译行为的替身。`cargo xtask host-assets` 在声明文件之后运行该命令，因此 `pnpm run build` 会产出它们，`--check` 负责校验。`remote-declarations` 还用已经产出 Remote 对的同一个 face 模型发射器写出 `lib/typert.host.js` 与 `lib/typert.host.d.ts`；九个在 `exports["."]` 中把 `default` 列在 `types` 之前的清单现在按 publint 的要求把 `types` 放在最前。

## 备选方案

**从 Host 清单中删除入口字段，并把门禁收窄到 Client 包。** 否决：`check-workspace-constraints`、`verify-package-invariants`、构建包不变量探针、发布族和实操手册都强制这些字段，清单是已验证的配置面，而一个没有入口点的已发布包并不是兼容绑定。

**`apply` 什么都不做的 Loader 形状桩。** 否决：行为位于 Rust 的插件若有一个空操作 `apply`，会让 Node 消费方以为插件已经挂载。Client 包仅在浏览器 bundle 承载行为的地方保留这种形状。

**从各 crate 的 Rust 常量生成入口。** 暂时否决：伴随名已经逐字捕获在声明模型中，从那里读取可以让 `xtask` 不依赖每一个运行时 crate。

## 结果

- `pnpm run build` 之后，`publint`、`verify-node-next-types` 和发布打包看到的是完整的 Host 包；`publint` 之后的门禁在 CI 中重新运行。
- 构建包不变量探针对每个包仍然失败：Rust/WASM Loader 包通过 `fetch` 一个 `file:` URL 初始化，Node 拒绝该操作，而且它的类不暴露 `unwrapExports`。该门禁需要对 Loader 包装器和探针各自做出修改。
- 任何新的、指向 JavaScript 文件的 Host 清单导出都会被自动覆盖；声明模型不认识其伴随插件的清单会让构建失败并点名该包。
