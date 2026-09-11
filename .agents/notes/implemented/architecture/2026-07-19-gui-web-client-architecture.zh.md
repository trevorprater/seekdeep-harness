# Agent Note: Web 客户端架构——client cordis 插件树、slot 体系与 React-free 对象层

Status: implemented

[English](2026-07-19-gui-web-client-architecture.md) | 中文

> 分工线：通道无关的分层模型与 RPC 协议（消息模型/类型体系/约定面/客户端基类）见 [分层与 RPC 协议笔记](2026-07-19-gui-layering-and-rpc-protocol.md)；本篇 = 浏览器侧：client cordis 树如何装载、UI 插件如何经 slot 与服务组合、React-free 对象层如何以不可变快照供给 React。

## Problem

浏览器客户端受两股力塑形。其一是流式：事件驱动的对话 UI 里，若业务状态（事件窗口、流式累积、待答交互、连接状态机）散落在 React 组件与全局 store 中，每个 token 分片都会震荡渲染树，且换 UI 库等于重写业务逻辑。其二是模块化：UI 功能（布局、侧栏、对话、主题、语言包）必须是可独立装载的插件——按 host 下发的 manifest（元数据清单）在运行时组合，而非编译进单一 bundle——同时不放弃跨插件边界的编译期类型安全。

## Decision

两端都跑 cordis。host 是一棵 cordis 插件树；浏览器里跑第二棵 client 侧 cordis 树，其中每一项 UI 能力都是插件，由壳静态持有的 loader 动态装载。树内 cordis ctx 承载一切运行时事实（服务、store、会话 scope），React 是纯投影：组件对框架零 import，一切经 props 注入，经 `useSyncExternalStore`（下称 uSES）订阅不可变快照。

```
┌─ Host ─────────────────────────┐   ┌─ Browser ─────────────────────────────────────────┐
│ sessions/agents/SessionLog     │   │ client cordis root ctx                             │
│ apiproxy: RPC + mux/host 双流  │◀─▶│  ├ vendored Loader + ctx.modules（内核，壳静态持有）│
│ webserver:                     │   │  ├ immediately entries: connection/runtime/        │
│  ├ GET /plugins/<id>/client.js │   │  │   ui-theme/i18n（fetch bundle，boot 预拉）       │
│  └ GET / 注入 __SEEKDEEP_BOOT__ 图  │   │  ├ lazy entries: layout/sidebar/                   │
│                                │   │  │   conversation/trajectory（fetch bundle，按需） │
└────────────────────────────────┘   │  ├ app-shell 伪行（壳内静态注册，同一治理）        │
                                     │  └ session scope ×N（观看驱动，惰性建）            │
                                     │ React: loading 页 → settled → 整 UI 一次成型       │
                                     └────────────────────────────────────────────────────┘
```

## client cordis 树与装载链

装载链——两类包（普通包 vs seekdeep.client 插件）、模块系统/插件治理器之分、host 独家撰写的带修订号 entry 图之上的双阶段 boot、热重载——归 [client 插件装载笔记](2026-07-23-client-plugin-loading-model.md) 所有。本篇赖以立足的事实：浏览器启动编译后的 Rust/WASM `@seekdeep-ai/cordis` Context 与 `@seekdeep-ai/cordis-plugin-loader`，由 client 模块系统（`ctx.modules`，`packages/client/modules`）填上 Loader 的 `internal` 约定；凡带产品行为的单元都是 host 独家撰写的 `__SEEKDEEP_BOOT__` 图里的 entry——每个生产插件包（含基础设施）都携带 `seekdeep.client` 声明、以 fetch 到达的 `./client` Rust/WASM 闭包 bundle 供给，`immediately` 行的差别仅在 boot 第一阶段预取，而由外壳持有的普通包保持打进壳、已播种、对图不可见；图中 bundle 执行 `window.__ModuleLoader__.load({ id, factory })`，其 `require` 由 lazy CJS 模块表应答（种子词条 + 已登记工厂，首次 require 时物化并记忆化——跨插件值 import 是构建错误，协作走 Cordis 服务）；插件 CSS 内联在 bundle 里、物化时注入为 `<style data-plugin="<id>">`（CSS Modules 哈希 + 归属标记 = 隔离，重载时移除）；热重载已在 dev 图落地——webserver 对自己供给的 bundle 做 stat 轮询并广播 `rebuilt` SSE 帧，`client-hmr` 插件每帧换掉一个 fiber。settled 翻转（`loader.await()` + 一次全 ACTIVE 扫描）依旧让壳从 loading 页一次切换到真 UI——settled 意味着每个 entry 已创建、每个 fiber 都到达 ACTIVE，FAILED/PENDING 的 fiber 被大声列出；不存在部分可用模式（渐进渲染为后置工作）。

由图装载的 Rust/WASM Client package 使用同一个同步 module-table handoff。`cargo xtask wasm-package` 构建 optimized cdylib，经 wasm-bindgen classic no-modules target lowering，嵌入 WASM byte，调用 `initSync`，并把编译后的 Rust export object 注册为 package factory。生成的 package-global 唯一且使用 `var`，因此独立装载的 package 不会冲突，同一个 rebuilt script 也能在 HMR 期间再次执行；异步 WASM initialization 不会泄漏进同步 factory contract。由外壳持有的基础 package 使用 generated ESM 包装层，其顶层 WASM initialization 会在 `AppWebEntry` 之前结算；`cargo xtask web-frontend` 生成挂载绑定与 Vite 配置，因此受跟踪的浏览器行为仍由 Rust 拥有。

Client Runtime factory 以相同名称公开 source barrel 的完整 value roster 与 generated declaration。薄 compatibility binding 提供 alias、reference-stable empty constant 与原生 `Error` subclass，而 helper、service、`PendingWait`、Conversation assembler 和 Location-index 行为全部委托给编译后的 Rust；内部 wasm-bindgen export 不会取代或削弱已记录的 public face。

浏览器事件分发读取可变的 `ctx.events._hooks` 表；注册仍是由 Fiber 拥有的 effect。可选的显式接收者提供 `Context.filter` 和监听器的 `this`；隐式分发绑定 `null`。`bail` 保留同步值及返回的 Promise 标识，`events.dispatch` 消费接收者与事件名，并返回已捕获的回调列表。`once` 在重入前移除注册，并遵守拦截器提供的 disposer。Waterfall 监听器共享可变参数列表，并可否决 continuation。Serial 与 parallel 分发同步进入监听器，同时将异步失败公开为 rejection。源实现／已构建 WASM 的差分用例与 Chromium 插件释放验证这些行为；组合后的 `/export` 路径覆盖 token 消费和本地执行确认。

`Context.is` 通过编译后的 Rust 谓词读取源实现的共享 symbol 标识，包括继承的标记、可变的标识键函数、原始值 getter 接收者，以及未改写的 getter 失败。原型属性描述符与源实现一致；Chromium 验证独立 Cordis 副本和外部 realm 对象。该标识用于识别对象，不授予能力。

服务访问与 `reflect.trace`/`reflect.bind` 共用同一个 Rust/WASM tracer。源实现的 tracker 元数据控制调用方 Context 替换、origin shadow、关联属性、嵌套服务和 `noShadow`；方法调用保留显式 receiver，对直接返回值执行 tracing，但不包装 Promise。绑定后的回调在事件拦截之前对 receiver 与参数执行 tracing，也适用于构造调用。源实现对照与 Chromium 验证：通过回调服务创建的 effect 属于 listener 插件，并随其 dispose 消失。

浏览器 `Fiber.update()` 在验证前暂存原始 JavaScript 配置，再在全局 update waterfall 内运行 Fiber 本地钩子。它保留 `noSave`、同步与 Promise 否决结果、standard-schema 规范化与验证错误，以及精确的配置 identity。源实现的特殊钩子列表在 restart 与 dispose 后仍保留，直到显式移除；其缺失的 `unshift` 操作仍会拒绝 prepend 注册。可等待的挂载句柄继承底层 Fiber，因此待依赖激活读取底层原始配置，而不是句柄上遮蔽的值。底层 Fiber 同步接纳 restart，保留原始启动错误，并在不采用原生事务回滚的情况下恢复。源实现差分用例与 Chromium 双插件 restart 场景固定这些行为。

Symbol 事件名称在 Rust 事件注册表中保留 identity，并共用原生 Fiber teardown。分发调用名称实际的 `startsWith` 方法：普通 symbol 产生源实现的 TypeError，而显式提供该方法后即可分发，不会混淆描述相同的 symbol。源实现对照与 Chromium 覆盖注册拦截、不同 key、`once` 和撤销。

浏览器根上下文显式选择并发 Fiber dispose；关联的子级继承注入的调度策略。每次 teardown 只截取一次 effect 列表，按注册逆序启动 disposer，并等待所有操作完成后才允许 restart 激活下一份配置。因此，有依赖关系的异步 disposer 能够相互释放，不会因串行执行而死锁。源实现对照与 Chromium 固定该推进和结算行为；含 64 个 disposer 的原生测试固定策略继承、调用方共同等待和稳定的失败顺序。原生根上下文默认仍串行 dispose。

事件服务公开钩子记录、`register` 和 `unregister`。即使调用方替换表项，清理仍使用原始列表与回调。原生 emit、bail、serial 和 parallel 分发会观察表中的修改，同时保留具有 Rust 类型的回调；原生 bail 也保留即时 JavaScript 值与 Promise 标识。源实现比较、原生／WASM 桥接测试和 Chromium 验证记录修改、回调绑定及按所有权清理。仅适用于原生环境的测试已按目标条件限制，Cordis 的 WASM `--all-targets` strict Clippy 检查通过。

公开的 `EventsService` 构造函数创建独立表，并保留子类原型、方法参数个数及虚方法覆盖行为。注册使用服务当前的 Context 与 Fiber，也支持按结构提供接口的 Context 所有者。当回调返回 bail 值或失败时，bail 与 serial 会关闭回调迭代器；若迭代器清理也失败，原始回调错误仍优先。`isBailed` 仅对 `null`、`false` 和 `undefined` 返回 false。源实现比较和 Chromium 将这些公开导出与生成的 Remote 一起验证。

浏览器 Fiber 转换由 Rust 实现，每个接收者各有一个所有者，并共享依赖 epoch 与浏览器 disposable 列表。原生注册也进入同一列表。因此，句柄与其底层 Fiber 可以重叠执行更新，同时保留各自的状态、inertia、配置和错误字段。底层 Fiber 的状态控制服务可见性。每次 unload 同步截取其 effect；之后注册的 effect 留给后续转换处理。源实现／已构建 WASM 的执行记录验证重启、失败恢复、重叠更新，以及底层 Fiber dispose 后句柄保留自身状态的行为。原生 Fiber 保留独立的事务更新路径。

反射存储与 Fiber 的 `store`／`_store` 快照共享实现记录。可用性检查刷新依赖快照，但不隐藏显式查找；teardown 会保留自身访问，直到依赖方清理完成。浏览器发布在观察者扩展依赖前提供 PENDING 状态的 Fiber 对象；发布失败保留原始 JavaScript 异常，并在返回前启动回滚。失败发布的视图仍是根生命周期拥有的元数据，不会成为无所有者的原生提供方。

浏览器 Context 的 accessor 与 mixin 使用 Rust effect 账本完成注册、回滚和清理。计算属性的 getter 与 setter 接收调用方 Context、关联 receiver，以及原始调用方错误。Get/set waterfall 通过 continuation 保留该错误；只有抛出同一个错误传递对象时才执行 stack enhancement。可修改的定义、映射后的 mixin 方法与 setter、只读写入拒绝、特殊属性，以及根上下文元数据写入均遵循源实现规则。源实现比较与 Chromium 将这些行为和带类型的 Remote 调用一起验证。

反射通知遍历公开的 registry 记录，将当前作用域过滤器应用于依赖方和服务事件监听器，并调用各 Fiber 当前的 `_checkImpl` 与 `_refresh` 方法。提供方注册调用当前的 `fiber.effect` 和反射通知方法；原生服务记录继续支持 Rust 使用方。源实现／已构建 WASM 用例验证自定义作用域选择、跳过 setup、effect 诊断、通知只发送一次，以及携带原始错误对象的异步撤回拒绝。Chromium 将通知过滤器、提供方钩子和清理拒绝与 Remote 调用一起验证。

浏览器根上下文保留源实现的 Context 自有字段、子类原型，以及共享的反射构造函数、原型和 handler。插件上下文拥有自己的 Fiber 字段；普通元数据子对象以常规对象形式直接继承父对象。私有 weak map 将这些对象连接到 Rust，保留自有元数据的直接读取，以及继承值的上下文 tracing。Shadow extension 保留源实现额外的原型层和调用方标记。源实现比较与 Chromium 验证对象结构和 tracing 规则。

Context 元数据操作接受结构型 receiver。隔离和拦截调用 receiver 当前的 `extend` 方法，保留自定义返回值，并在使用配置值之前保留 getter 的惰性。源结构的 extension 记录将普通和覆盖方法产生的隔离连接到原生 realm，同时保持返回的 JavaScript 对象不变。独立的反射服务发布到各自的表，并保留根上下文共享的作用域标签。源实现比较与 Chromium 将这些路径和 Remote 服务使用一起验证。

Rust 浏览器 effect 解释器按源实现的顺序收集函数、promise、同步及异步可迭代对象。effect 在 setup 执行前进入所有者的 disposable 列表；setup 部分执行后失败时，会回滚已收集的 disposer。嵌套 effect 转移所有权，并通过 `getEffects()` 保留诊断树。每个 effect 按逆序串行清理其子项，而 Fiber teardown 并发启动已截取的 effect。公开 disposer 仍只执行一次，结构所有者会共同等待已开始的清理。插件启动使用同一结果解释器，同时保留其不同的列表收集语义。源实现／已构建 WASM 用例覆盖 setup 中重入 dispose、自定义 thenable、原始类型迭代记录、取消的流，以及尚未完成的子级清理。`CordisError` 保留公开原型、稳定错误码、可修改的默认消息表，以及原有的非活动操作行为。

根上下文初始化时会从 disposable 列表中清除构造期间的注册，但不执行其清理，因此内置事件钩子在根上下文重启后仍保留，也不会出现在用户 effect 诊断中。源实现比较验证重启后的持久性。真实 Host／Chromium Remote 路径还会先验证嵌套可迭代 effect、可等待的 setup、共同等待清理和非活动 Fiber 错误码，再验证 descriptor 撤销。

公开 `Fiber` 构造函数支持独立的根类型所有者和直接 runtime 挂载，也保留子类原型。直接挂载会加入所提供 runtime 的 fiber 列表，但不会插入 registry 记录，因此在该 runtime 参与 registry 遍历之前，服务通知仍保留其初始依赖快照。浏览器 `RegistryService` 在同一回调的各次挂载间共享首个描述符的名称与 schema，并向检查和调用暴露同一 runtime 记录。类入口按源实现的顺序执行构造、初始化钩子和返回的启动 effect。源实现比较与真实 Host／Chromium 路径验证构造和清理。

浏览器 Fiber facade 保留源实现的自有字段和方法原型，同时将原始 WASM 句柄保持为私有。可修改的 `_runner` 在激活、重启、失败和 teardown 之间持有 epoch、执行、收集和调用方 stack 状态。Effect 调用当前的 `_execute` 方法；生命周期转换按源实现顺序调用当前的配置、epoch、加载／卸载和状态方法。状态观察器运行前会同步原生状态。源实现比较与 Chromium 验证字段 descriptor、异步方法原型、runner 身份、回调替换和生命周期分派。

Fiber 构造在新 Fiber 的上下文字段仍为 undefined 时调用父 Context 当前的 `extend` 方法。返回新上下文或复用父上下文时，均保留源实现对应的服务所有权。Extension 记录在自定义 Context 包装器中仍被保留，其原始句柄采用构造出的 Fiber 原生所有者。原始 WASM 测试与真实 Host／Chromium 路径验证子级清理，以及父级拥有的服务继续保留。

结构型 Fiber receiver 通过同一个 Rust 生命周期实现执行激活、重启和 teardown，无需原生 controller。其自身的 disposable 列表会被清空，清理迭代开始前的失败保留 Promise 拒绝与错误身份。Registry 删除在 dispose 抛出时关闭自定义 fiber 迭代器。依赖规范化逐项写入，拒绝只读结果槽位，并在写入失败时关闭迭代器。源实现比较与 Chromium 验证这些边界。

父 Context 来自另一份 Cordis 时，Registry 构造仍保留发起方 Cordis 的 Fiber 原型。跨副本的依赖撤回、重新发布、重启和清理使用同一组源可见对象。浏览器插件先创建上下文和 runner，再注册父级 effect；runtime 成员在该 effect 的 setup 内加入。父级 effect 拒绝时保留原始异常，并阻止启动。缺失 runner stack 回调时采用源实现的默认捕获。源实现比较与 Chromium 验证这些构造和诊断边界。

`DisposableList` 公开可修改的序号、map 和弱身份存储。移除闭包读取当前 map；重复注册保留不同的移除句柄。构造函数 inject map 与 Context intercept 保留 JavaScript 值和祖先原型，也支持含函数的配置。Symbol 隔离标签保留身份，并成为对应的反射存储 key；描述相同但身份不同的 symbol 使用不同的原生隔离 realm。源实现／已构建 WASM 用例验证这些行为。

Rust/WASM `Service` 绑定处理静态提供方名称、可用性谓词、可调用对象构造、扩展、隔离过滤、配置合并，以及沿构造函数链进行的实例检查。类和方法的 `Inject` 装饰器保留继承的元数据，并注册由依赖关系管理的子插件。方法调用保留有 tracker 或无 tracker 时各自的接收者行为；依赖撤销和父级 teardown 会释放其 effect。源实现比较覆盖重复装饰器、原始构造对象作为 check 接收者，以及可修改的 tracker getter。Chromium 将可调用服务、symbol 隔离和装饰方法与真实 Remote 调用一起验证。

公开的 `createCallable`、`joinPrototype`、`withProps`、`getTraceable`、`getPropertyDescriptor`、`isObject` 和 `resolveConfig` 工具函数在 Rust/WASM 中执行。Tracing 读取当前 JavaScript tracker 与反射元数据，也支持按结构提供接口的 Context。Standard-schema 解析保留 config 和 issue getter 的调用时机、validator 接收者、同步失败的身份，以及拒绝异步验证的行为。

完整且可修改的 `symbols` 表与 Rust 使用方共享。Effect、tracing、装饰器和类初始化读取其当前 key；事件过滤和服务扩展则保留源实现对公开类元数据的独立读取。反射查找观察当前 Context 隔离 key，并传播 symbol getter 失败。公开的 `buildOuterStack` 和 `composeError` 工具函数在 JavaScript 边界捕获调用方 frame，并在 Rust 中执行惰性切片、thenable 处理、非标准错误包装和 stack 改写。源实现用例与 Chromium 验证这些公开工具函数。

Fiber 启动、即时 effect setup、迭代器执行，以及由所有者发起的 teardown 使用 Rust stack composer。Registry 调用惰性捕获调用方 frame，直接 Fiber 构造则保留所提供的 frame 回调。抛出的非 Error 值被包装成 Error，回调内部创建的错误保留所提供的调用方 frame，无效结果或不可调用的初始化钩子仍产生 TypeError。Setup 回滚与清理所有权仍由 effect 账本管理。源实现比较覆盖同步和 rejected setup、直接构造、迭代器、类初始化与 teardown；Chromium 验证启动和清理 frame 到达 logger。

默认浏览器 logger 是由 Rust 实现的可调用服务，提供可修改的调色板和 formatter、可注入的时钟边界、作用域名称与级别、可识别 shadow 的 Fiber 引用，以及源实现的消息身份语义。其 buffer 与 exporter 清理保留源实现的计数器和零上限行为。启动、清理和可用性检查失败会携带原始 JavaScript 错误对象到达对应 Context 的 logger。原生 logger 名称使用共享的 `param_case` 工具函数处理缩写、分隔符和非 ASCII 文本。Gateway 夹具通过真实 logger 上的 exporter 记录 warning；Chromium 将安装、格式化、作用域及启动错误身份与 Remote 调用一起验证。

浏览器统一导出入口已暴露固定源实现的全部 runtime 导出，但浏览器 Cordis 仍未完成。其余构造边界和公开 API 审计仍需源实现比较。原生与浏览器 logger 的证据仍分开记录。即使公开导出和集成 Remote 调用通过，parity manifest 仍保留未完成的实现条目。

活动轮次证据使用正常的 Rust Web profile、现有 Rust session-log replay 适配器，以及固定源实现的 provider catalog 和 workspace-picker 交互。浏览器调用公开的 `connectWorkspace` 导出，创建 Session、提交提示词，观察中间文本与运行态 Stop 控件，并重新加载已结算的响应。夹具在 Host 关闭后审计完整 replay 消费及冷读 JSONL 工件。Projection frame 保留 null 值，同时仍拒绝缺失的 `value`；Host 运行／空闲 frame 来自 Agent status 事件，stream 所有的监听器会在取消或 drop 时释放。此无密钥 replay 不是真实模型运行。

Host 生命周期发布还覆盖 Session 创建／移除和无轮次位置的 Agent 错误。两个独立浏览器连接通过仅用于测试的 Rust 命令观察精确的创建元数据、先添加后移除的顺序，以及未改写的错误文本；命令使用真实 Session store、Agent dispatcher 与 command registry。命令的 run/done 对保持持久化，且不会增加模型轮次。无位置错误进入源实现的 `lastAgentError` 快照字段；该路径不会另造 UI 通知。公开的 `pickDirectory()` WASM 方法保留选中的路径、以 null 表示的取消，以及 Host 业务错误。Chromium 通过 Client 模块系统加载真实 Runtime 包，并针对仅提供 browse 能力的 Host 验证一次原生选择器请求及其精确拒绝消息；该检查不操作交互式 OS 选择器。

类型宇宙在聚合层拆分——`tsconfig.host.json` 是 host program、`tsconfig.client.json` 是 client program，二者由 solution 根 `tsconfig.json` 引用，因为两侧都在相同键（`sessions`、`loader`）上对 cordis `Context` 做声明合并且服务不同；client 包经纯类型子路径（`@seekdeep-ai/seekdeep-session/types` 等）消费协议词汇，host 侧的声明合并不会搭车进入 client program。

## slot 体系：页面怎么拼

slot 体系有自己的笔记——[slot 体系标准](2026-07-22-slot-type-chain-implementation.md)——本文整体移交给它。此处只留一段定位摘要：壳只渲染 `'root'`；插件用单独一次 `register` 调用组合 UI——占用 slot、声明并授权子 slot（`children` spec 对象）、声明 store、注入业务面；组件 props 分四份额自动推导到达（`PropsRuntime<K>` / `PropsRenderSlots<S>` / `PropsStore<H>` / inject），各有唯一真源。`SlotMap` 声明合并仍是类型权威，entry 只携带 owner 份额（「谁注入的，类型归谁」）；每个被渲染的注册项都在 per-entry 错误边界之内。

实现的家：注册表核心与 props 份额类型在 `packages/client/ui-slots`，出口组件/渲染器/uSES 桥在 `packages/client/web-react`。

## 服务与 scope 寻址

服务是插件对其他插件的唯一 API（UI 组件与注入面都不是 API；无人调用的插件不挂服务——ui-trajectory 即最小插件样板：无 ctx 服务，只做视图 slot 注册）。名册：`ctx.connection`（api client + 流句柄）、`ctx.slots`（注册表包装层，发 `slots/changed`，渲染入口，渲染器安装约定）、`ctx.sessions`（列表 store、当前会话状态、scope 树）、`ctx.loader`、`ctx.theme`、`ctx.i18n`、`ctx.layout`（跨插件视图导航）、`ctx.conversation`（send/cancel/startSession）。过去住在服务 store 里的观看态（面板宽、选中、草稿）现按 [slot 体系标准](2026-07-22-slot-type-chain-implementation.md) 住 entry 声明的 store。

slot 之外不存在第二种组件注册模型——原视图环与工具环都已溶解进来。会话视图即 ui-conversation 声明的 `'conversation.view'` list slot entry，tab 元数据随注册 options（`id`/`order`/`label`）走，per-view chrome 住视图组件自身。最终 Chat 业务 Node 通过 keyed/session `'conversation.chat.node'` slot 分发；ui-tool 拥有其中的 `tool-call` entry，递归渲染传入的 `subCalls`，并声明 keyed/session `'tool.call.toolview'` 子 slot。key 空间仍在运行时开放（SlotMap 声明 slot、从不声明 key），root 与任意深度的后代都按 `entryKey: toolName` 分发，以 `GenericToolCard` 兜底。业务包通过 `ctx.slots.inject('tool.call.toolview', () => ctx.slots.register({ name: 'tool.call.toolview', key: '<tool>' }, Row))` 注册原子视图；声明本身就是加载与重载依赖（[决策](2026-08-05-slot-declaration-injection.md)）。ui-conversation 还通过 `'conversation.details.tool'` 委托 selected call 的详情正文，使 ui-tool 的 card model 保持为唯一展示所有者，同时避免 conversation 导入 Tool 组件。与 target 无关的事件注册表和视图注册表是数据组装 seam，不是平行组件注册表（[决策](2026-08-09-client-conversation-node-assembly.md)）。

**scope 寻址**与 host 侧 agent（智能体）scope 惯例同构：服务是 root 单例，方法不收 sessionId——它们读调用方 ctx 上的 scope 标（`scopeOf(ctx)`）。在会话 scope 内，`ctx.conversation.send('hi', 'queue')` 自动打到该会话；跨会话调用换 ctx 定向（`ctx.sessions.scope(id)!.conversation.send(...)`）；从 root ctx 直接调 scoped 方法即 throw。client 会话 scope 的铸造方式与 host agent scope 相同（no-op 插件 fiber + scope 键 extend），首次观看时惰性建，只有会话被移除且无人观看才拆——仅 host 会话死亡不拆 scope（冻结为只读视窗）。

Rust/WASM `SessionRuntime` 端到端拥有这条 axis：manager 投影单一 list/current snapshot；selection 连同 retained child address 一起持久化；binding 与 scope resolution 保持纯粹且 identity-stable；只有 staging 才启动 `Session.open()`；masked current gap 保留 watched scope 的冻结态；off-stage removal 立即 dispose Cordis fiber、Session binding 与 session-keyed Slot Store，而 staged removal 延迟到 stage 移动。浏览器 scope primitive 写入一个私有 Symbol 与 actx-local `Context.filter`，JavaScript provide channel 则先 rebuild 全部 live binding，再原子 republish 稳定 current selection。

Rust/WASM `applyClientRuntime` assembly 提供 Slot、Conversation registry、Session 与 Workspace face，安装 Slot standard feed 与 Typert Agent identity resolver，并通过 root Cordis effect 拥有唯一 connection loop。Mux frame 进入 Sessions；Host frame 进入 Sessions 与 Workspaces；只有 `host/remote-event` 到达 `Remote.$dispatch`；每个 connected generation 发出 `connection/reset`；`reconnecting` 在后续 frame 之前丢弃 generation-scoped interaction；fiber disposal 精确停止 loop 一次。模型编写的 JavaScript Conversation callback 通过单一 WASM adapter 进入 Rust-owned Definition 与 per-target builder：完整的 extension-bearing SessionEvent object 保持可见，`reader.previous` 只记录实际请求的 kind，而 replay、Context 与 Location ownership、publication scheduling、dependency repair 和 snapshot identity 仍由 Rust 持有。

Rust/WASM Client Remote core 把 Rust 生成的 descriptor contribution 作为唯一 method authority 挂载。它在 transport 前应用每个 descriptor 的 codec，发布带 tracing 的 Cordis namespace service，依据调用方 Context 选择 direct 或 Agent-context overload，把得到的 wire field 经 `connection.rpc` 发送到 Host Gateway，隔离 forwarded-event listener failure，并按 mount 的逆序撤销 method 与 namespace。Cordis 对属性读取与显式 `ctx.get()` 读取都执行 tracing，而 root browser Fiber face 也暴露与 child Fiber 相同的异步 disposal 边界。

公开 browser Slot face 从执行 mutation 的同一个 Rust ledger 读取每个 key 的 version。Client Runtime invariant 观察全局 `internal/dispatch`，忽略无关 event 与 slots-less boot，并拒绝不带非空 string key 或该 key version 仍为零的 `slots/changed`；notification ordering 因而直接对 mutation authority 检查，而不依赖第二份 mirror。

## 数据对象层（`packages/client/runtime/src/client/sessions/`）

帧从这里进、快照从这里出、Conversation assembler 坐在中间——React-free（零 React import，grep 可断言）：

```
mux/host frames (ConnectionController pump, injected sinks)
        │
        ▼
SessionManager.handleMuxEnvelope / handleHostEnvelope
        │ session frames target existing instances (requested waits buffer)
        ▼
Session.handleMuxEnvelope ──► contiguous Event window
        │                        │ replace / prepend / append
        │                        ▼
        │                ConversationNodeAssembler
        │                  Definitions -> Contexts -> view builders
        ▼
Notifier 微任务合批 ──► ConversationSnapshot 缓存 ──uSES──► 组件
```

Rust `seekdeep-client-runtime` 对象层通过注入的 microtask 与 animation-frame adapter 获取调度策略。它的 Notifier 把 freshness 与 pending delivery 分开，并在通知前重建；partial Assistant accumulator 只替换发生变化的 `Rc` block；Tool-call tree 对未变递归投影保持 structural sharing，并在不丢弃周边 Session data 的前提下消费 malformed cycle 或超深 edge，深度上限与源码相同，为 256 层。它的 ProjectionValueStore 对实时帧与 baseline 使用同一条严格递增 seq 规则，只清除未新于缺省 baseline 的行，截断跨代残留的 phantom 行，并通过 WASM facade 保持 per-key face、完整值与聚合快照的 identity；按 key 与粗粒度订阅都只在每个注入的 microtask 中发布一次。

- **Session**（session.ts）：懒建、常驻——建成后在后台持续吃帧，切走切回秒显。操作面：`prompt`/`cancel`（RPC 透传；失败落进快照的 `promptError`）、`open`（拉尾页 history，幂等）、`loadOlder`（向上翻页，防重入）、`resync`（重连 = 清窗口重跑 open）。订阅面：`subscribe`/`getSnapshot`（恒返缓存引用）——`implements ObservableSnapshot<ConversationSnapshot>`，构造时挂 `useSelector = bindSnapshotSelector(this)`，Session 本身就是 uSES 源。帧分发是一个 switch：`session/event` 帧按 seq 去重（唯一去重键），open 在途时缓冲，否则追加 + 增量投影；open/缝合按 seq 合并 live 缓冲并去重，`subscribed.lastSeq` 超出窗口尾则回补一次。Rust `ClientSession` core 通过注入 seam 获取 transport、时区解析、通知调度与 detached-task ownership。调用 `open()` 与 `prompt()` 时，源码可观察的 loading 与 blank→engaging 前缀会在返回 future 前同步发生；共享 open work 受 identity guard 保护，每次 history await 都重检精确 resync generation。Paging 保持单一连续窗口，live gap 在一个 tail repair 后合并，pending wait 跨 resync 重新铸造，queue baseline 只在 `session/subscribed` 清理，而 Host-computed projection 则在实例化与 reload 之间沿用同一个 higher-seq-wins store。它的 WASM facade 在不改变对象层协议的前提下适配 generated API client 与 Remote namespace：并发 open 共享同一个 JavaScript Promise，`getSnapshot()` 缓存精确的顶层对象，未变化的 Chat/pending/queue/projection reader 保持 JavaScript identity，generated RPC result 仍是 plain object，attachment bytes 仍是 `Uint8Array`，pending `respond()` 则在穿过 carrier 前恢复私有 `rpcId`。
- **ConversationSnapshot**（conversation.ts）：顶层不可变快照约定。`chat` 包含结构化 `order`、identity 稳定的 keyed Node reader、Turn/Step index 和 timeline；`nodes`、`partial`、`runningCalls`、`turnTimings`、`turnEnds` 是未迁移 Trajectory 消费方使用的兼容 slice。pending interaction、queue、running、removed、open state、paging 和 prompt error 仍是 Session 信息。**引用纪律**（memo 与 uSES 的前提）：未变化的子结构和 Node value 保持引用；单个业务更新只替换对应 key 的 value，除非它的顺序或 Location 发生变化。React 仍只订阅 Session 这一处 observable source，并由框架提供的 `useSession(selector)` 隔离 Node 与 Location 聚合更新。
- **SessionManager**（manager.ts）：实例簇 + 帧总入口 + 会话列表。带 sessionId 的帧只投已存在实例（mux 广播不得把每个会话都实例化）；例外是审批/问答 `requested` 帧——它们不落 history、open 无法回补，故缓冲进 `pendingBuffers`，实例化时回放。Rust `SessionManager` core 让 Session instance、projection store、pending status、queue/request buffer、completion reminder 与 job mirror 分处独立 lifecycle axis。List pull 使用 single-flight，并在 baseline 上 replay 全部 in-flight mutation，同时不重排已建立 ID；projection 与 job frame 可先于实例化落地；`session/subscribed` 截断 phantom projection row 并清除该 generation 省略的 mirror；answerable request 按稳定 key 压缩，并在 generation 丢失时死亡；只有直接 user message 推进 activity；running→idle 也只在 Session 未被观看时 arm reminder。Direct-child catalog 是独立保留的 read model：健康 row 铸造 durable parent/child transport address，普通 selection 不得擦除它；activity 与 expandability frame overlay 较旧的 in-flight response；activation removal 让 durable child 回到 inactive，而非删除 lineage；parent removal 即使遇到更早的 catalog response 仍在途，也会立即使每个 addressed child 的 writable availability 失效。Membership frame 对每个 selected 或 open parent 使用一个注入的 50 ms debounce；若计时器在 pull 期间触发，它会把该 response 标为 stale，并在 settlement 后精确调度一次 trailing refresh，而关闭 catalog 会取消计时器。Manager 的 WASM facade 对每个 Rust Session 缓存一个 JavaScript wrapper，并对每个 Rust snapshot 缓存一个 list object；raw mux/Host envelope 进入同一个 core，generated RPC result 保持 plain object，而 search 继续是 request-local：调用方的精确 `AbortSignal` 直接传给 `api.sessions.search`，不向 list observable 添加 query state。
- **Workspace / WorkspaceManager / WorkspaceRuntime**（`workspaces/`）：Rust entity 在单个共享、可重试的 Host materialization 中保持 local-intent identity；较新的 adopted view 会压过迟到的 create result。Manager 对 baseline 做 single-flight，replay 并发 changed/remove/order frame，在连接生命周期内永久 tombstone 已删除的 random-UUID identity，拒绝较旧 unary snapshot，并以精确 request generation 与 Host-frame generation 仲裁 optimistic reorder echo。Service 合并 Workspace 与 Session baseline，在不改 Host order 的前提下推导 recent-Workspace selection，只复用可见且已 accounted 的 blank Session，按 Workspace 合并创建，保护 archive frame 不受 stale baseline 回滚，并清除已归档的 current Session。其 WASM facade 保持 Workspace row 与 action 返回值 identity、单个共享 refresh Promise 与每个在途 Workspace create 的单个 Promise，而 immediate reuse 返回各自独立的 Promise；它还保留结构化 create/browse error、调用方精确的目录 `AbortSignal`，并让 raw Host frame 经同一个 Rust core 路由。
- **Notifier**（notifier.ts）：两条通知通道，按变更来源取用。`markDirty()`（默认；帧驱动一律用它）按微任务合批——N 次变更、一次通知、一次重渲染；flush 先重建快照缓存再通知。`notifyNow()`（仅用户手势的直接回响）同 tick 重建并通知——受控输入的回响若延到微任务，DOM 会回滚、光标跳尾。帧驱动代码用 notifyNow 会让合批塌回逐帧渲染；禁。
- **SessionProvideChannel**（provide.ts）：拥有静态 hook/prop roster、每个 Session 的确定 bundle 物化，以及原子 current-Session observable。runtime 自有的 `session` hook 始终排第一。provider 必须返回全部已声明成员且不得返回未声明成员，整个 roster 中的名称保持唯一；任何 live-bundle 失败都会先回滚注册，避免污染后续物化。selection 与 roster 变化经同一个按 identity 去重的 source 发布，同时隔离 subscriber 失败，避免一个 render boundary 饿死后续 consumer。
- **ConversationNodeAssembler**（`runtime/src/client/conversation/`）：Session 拥有的增量引擎在原始事件上运行各自独立注册的 Definition。`match(event)` 无须扫描 Context 即可选出 `(kind, id)`；start/update 构造 Definition state；引擎计算的 Location 携带 Turn/Step 关闭信息；向前查询 Context 时记录依赖，并由后续 prepend 修复；`buildViewNode(target)` 只物化 dirty Context。Chat builder 保留结构顺序和 per-key value identity，`useSession` selector 负责消费隔离，Assistant token 发布则合并到每个 animation frame 一次。[Conversation Node 决策](2026-08-09-client-conversation-node-assembly.md)拥有组装边界，[Tool 展示所有权](2026-08-08-client-tool-presentation-ownership.md)拥有 Tool 递归渲染。
- **ConnectionController**（在 `packages/client/connection`）：开 mux/host 双流、for-await 泵入，代际围栏之内指数退避重连（500ms 翻倍至 10s 封顶、抖动、无限重试）；sinks 单向注入（Controller 不认识 Session）。重连 = 重建：`onConnected` → 列表刷新 + 各已打开会话 resync。对象层只面向 `IApiClient`；Web 承载以 HTTP POST 载两个 client→server 象限、以[每逻辑流一条 WebSocket](2026-08-04-websocket-downlink-carrier.md)载两个 server→client 象限，客户端类族归分层笔记属地。

## React 面（`packages/client/web-react`）

胶水包就是整条 ctx↔React 边界；组件保持零框架依赖。

- 快照 store 引擎**住 runtime 包**（zustand vanilla + 草稿式更新，缺省 `flush: 'sync'`，可选 `'raf'` 合批，可选整值 localStorage 持久化，dev 深冻结——全部从 `runtime` 的 `./client` 主出口导出，无子路径）：store 产物是裸的可观察源，不带任何钩子成员。插件只经 [slot 体系标准](2026-07-22-slot-type-chain-implementation.md) 的 `defineStore` 声明触及引擎。web-react 在绑定处（`bindSnapshotSelector`，按源缓存）从 React 消费的唯一数据约定合成每个钩子：`ObservableSnapshot<T>`（`getSnapshot`/`subscribe`）——Session 对象与快照 store 同构满足它。业务插件包只依赖 runtime 与 ui-slots；web-react 是仅壳可用的胶水。
- `bindSnapshotSelector(source)`：把一个源绑定为经 uSES-with-selector 的带类型 selector 钩子。uSES 约定四条按构造成立：getSnapshot 恒返缓存引用；subscribe 是绑定期闭包（引用永稳）；纯 CSR 不传 server snapshot；相等性缺省 `Object.is`，按调用可选 `shallowEqual`。
- `useInvoke(fn)`：把异步动作包成引用恒定的触发器加 pending 标志；pending 走每个钩子的外部 store 经 uSES 读出（渲染路径零 setState），并发调用计数，invoke 引用永不变。
- 相等性协议，全链一致：生产端结构共享；消费方以 `Object.is` 或 `shallowEqual` 短路；`React.memo` 浅比较。深比较全链禁止。

## 目录形态

Client 包位于 `packages/client/*`，`apps/web` 是壳 boot 导出之上的薄 Vite 应用。插件包的浏览器半边在 `src/client/` 下；**一切构建产物落 `lib/`**——node 半边为 `lib/index.js`/`lib/invariant.js`，浏览器 bundle 为 `lib/client.js`（共享 tsdown client 预设两者皆出；无 `dist/` 目录，`exports["./client"]` 指向 `./lib/client.js`）。`ui-slots`、web-react 与 runtime 构成基础设施方向；功能插件通过服务与 slot 协作，不导入展示实现。

多域插件包的 client 半边还按未来包边界再拆——ui-conversation 即样板：

```
src/client/
  contract/    shared slot and cross-domain types
  service.ts   cross-domain orchestration
  skeleton/    conversation shell and details host
  conversation-nodes/ independently registered business Definitions and Chat builder
  chat/        ordered conversation view
  input/       composer state machine
  queue/       queued-message presentation
  settings/    conversation settings rows
  apply.ts     cross-domain assembly point
  index.ts     public contract surface
```

各领域实现文件不 import 兄弟领域；共享面统一经过 `contract/`。`scripts/verify-client-domain-graph.ts` 把守分层（contract=0、domain=1、apply/index=2；import 只准指向不高于自身的层级；兄弟领域依赖会失败）。Tool 展示已经拆为独立 `ui-tool` 包，只通过 ui-conversation 声明的 slot 到达 chat 与 details。

## 怎么开发

- **新 UI 功能** = 新插件包：package.json 声明 `seekdeep.client`（+ `inject` 拓扑），浏览器半边写在 `src/client/`（apply 挂服务/建 store、注册 slot），无 host 逻辑时 node 半边保持空 apply，用共享预设构建。把插件加进 host 配置；manifest 与装载随之自动跟上。
- **新 slot**：见 [slot 体系标准笔记](2026-07-22-slot-type-chain-implementation.md)——约定合并进 `SlotMap`，在父 entry 的 `children` 里声明，经自动注入的 `renderSlot` prop 渲染。永不全局导出组件。
- **消费新帧类型**：纯传输 session frame → Session 分发 switch；host 级 frame → Manager 路由表；已记录的 conversation 业务事件 → Definition 加 keyed view renderer，不增加 Session 业务分支。
- **状态住哪**：业务数据（事件、流式、待答）→ 永远对象层；父知道的 → renderSlot 现场的 owner props；单组件私有（滚动、搜索词、展开集）→ 组件状态；跨 entry 共享或跨重挂载存活（选中、草稿、面板宽）→ entry 声明的 store（[slot 体系标准](2026-07-22-slot-type-chain-implementation.md)）。
- **通知通道**：帧驱动/异步 = `markDirty` 合批；受控输入需要同 tick 的用户手势直接回响 = `notifyNow`。

## Consequences

token 流不再震荡渲染树：Assistant chunk 只更新一个业务 Context，每 animation frame 最多发布一次对应 keyed Node；无关行的 selector 结果保持原引用，因此不会重渲染。UI 功能以独立插件的粒度装载、失败、停用——一个崩溃的 slot 注册项只黑一张卡，一个装载失败的 bundle 在 UI 切入之前大声报错。接受的代价：loader/模块表机件是团队端到端自持的定制基建；一次成型启动（无渐进渲染）用首屏粒度换装配简单；双类型 program 让「这个文件归哪个聚合」成为开发者偶尔要回答的问题。

## Alternatives considered

| Rejected | One-line reason |
|---|---|
| 静态链接的单 SPA bundle | 插件必须由 host 在运行时按配置组合；单体把每个 UI 功能重新耦回一次构建 |
| window 全局变量 / import map 供共享依赖 | DI require 表让共享显式、大声失败、可替换；全局变量静默泄漏身份与版本 |
| 业务数据进 zustand 切片 | 事件窗口/累积器是行为状态机，不是扁平切片；对象层保住快照粒度与合批的可控性 |
| Tool 行使用平行的字符串键组件注册表 | ui-tool 的 keyed 子 slot 通过唯一的 slot 注册模型承载运行时开放的 Tool 名称集合（[toolview 溶解](2026-07-23-toolview-dissolution.md)） |
| 首个 web 客户端交付就做渐进/Suspense 启动 | 一次成型严格更简单；loader 的按插件状态面已保留，渐进点亮日后可落地而无需重构 |
