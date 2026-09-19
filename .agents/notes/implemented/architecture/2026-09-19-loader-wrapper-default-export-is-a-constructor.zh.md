# Agent Note: Loader 包装器的默认导出是作为构造函数的插件

Status: implemented

[English](2026-09-19-loader-wrapper-default-export-is-a-constructor.md) | 中文

## 问题

源码的 `@cordisjs/plugin-loader` 把它的 `Loader` 类作为默认导出：这个类就是 Context 所应用的插件，而 `Loader.prototype.unwrapExports` 把 ESM、CommonJS 和默认导出的模块形状归一化为它们承载的插件。已构建包不变量门禁在纯 Node 中依赖这两个事实：它导入 Loader 一次，从默认导出的原型创建一个对象，并让该对象解包每个伴随模块。移植生成的 `vendor/loader/lib/index.js` 把一个普通插件描述符作为默认导出，并通过对 `file:` URL 的 `fetch` 初始化其 WebAssembly 模块，而 Node 拒绝这种 fetch，于是门禁对全部 32 个未编译包都失败，consumers 通道跳过了六个依赖它的门禁。

## 决定

包装器在 `process.getBuiltinModule('node:fs')` 存在时通过它读取模块字节，否则以流方式加载 URL，因此浏览器保持原有路径，Node 不再需要 fetch。它的默认导出是一个构造函数 `LoaderPlugin`：在某个 Context 下构造它就会应用编译后的插件，正如源码的类在被 Context 构造时所做的那样；其原型上的 `unwrapExports` 委托给 Loader 已经对它启动的每个条目应用的 Rust 规则，该规则从 `crates/client-loader` 以 `unwrapExports` 导出。构造函数携带描述符的 `name`、`inject` 和 `apply`，所以读取描述符形状的调用方仍能找到它们；具名导出 `Loader` 仍是 Rust 支撑的服务类，声明文件描述这个构造函数。

## 考虑过的备选方案

**给描述符对象加一个 `prototype` 属性。** 已否决：它满足探针的 `Object.create` 调用，却不满足契约——契约是一个其实例能归一化导出的类；门禁自身的夹具和源码脚本都假定它是构造函数。

**把 Rust 服务类作为默认导出，并把描述符字段作为静态成员。** 已否决：编译后的 Cordis 外观会构造构造函数型插件，于是 Context 会直接构造服务，跳过插件的 `apply`，而 `apply` 才负责注册条目钩子并提供服务。

**通过动态 `import('node:fs')` 读取字节。** 已否决：包装器同时会被打包进浏览器外壳，动态导入中的字面 `node:` 说明符至少会引发打包器警告；`process.getBuiltinModule` 是同步的属性查找，浏览器里根本不存在。

## 后果

- `verify-built-package-invariants` 在 Node 22 及更高版本（存在 `process.getBuiltinModule`）下能进入伴随模块检查；CI 的 Node 版本线是 22.19、24 和 26。
- 应用默认导出的 Context 会构造 `LoaderPlugin`，其构造函数运行同一个 `apply`；运行时的插件名仍为 `loader`。
- `wasm.unwrapExports` 是 Loader 包的公开绑定；任何需要源码导出归一化的 JavaScript 都使用它，而不是重新实现这条规则。
