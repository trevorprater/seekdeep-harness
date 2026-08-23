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

装载链——两类包（普通包 vs seekdeep.client 插件）、模块系统/插件治理器之分、host 独家撰写的带修订号 entry 图之上的双阶段 boot、热重载——归 [client 插件装载笔记](2026-07-23-client-plugin-loading-model.md) 所有。本篇赖以立足的事实：浏览器启动与 host 相同的 vendored `@cordisjs/plugin-loader`，由 client 模块系统（`ctx.modules`，`packages/client/modules`）填上其 `internal` 约定；凡带产品行为的单元都是 host 独家撰写的 `__SEEKDEEP_BOOT__` 图里的 entry——每个生产插件包（含基础设施）都携带 `seekdeep.client` 声明、以 fetch 到达的 `./client` tsdown 闭包 bundle 供给，`immediately` 行的差别仅在 boot 第一阶段预取，而普通包（react 家族、cordis、尚未升格的库）保持打进壳、已播种、对图不可见；bundle 执行 `window.__ModuleLoader__.load({ id, factory })`，其 `require` 由 lazy CJS 模块表应答（种子词条 + 已登记工厂，首次 require 时物化并记忆化——跨插件值 import 是构建错误，协作走 cordis 服务）；插件 CSS 内联在 bundle 里、物化时注入为 `<style data-plugin="<id>">`（CSS Modules 哈希 + 归属标记 = 隔离，重载时移除）；热重载已在 dev 图落地——webserver 对自己供给的 bundle 做 stat 轮询并广播 `rebuilt` SSE 帧，`client-hmr` 插件每帧换掉一个 fiber。settled 翻转（`loader.await()` + 一次全 ACTIVE 扫描）依旧让壳从 loading 页一次切换到真 UI——settled 意味着每个 entry 已创建、每个 fiber 都到达 ACTIVE，FAILED/PENDING 的 fiber 被大声列出；不存在部分可用模式（渐进渲染为后置工作）。

Rust/WASM Client package 使用同一个同步 module-table handoff。`cargo xtask wasm-package` 构建 optimized cdylib，经 wasm-bindgen classic no-modules target lowering，嵌入 WASM byte，调用 `initSync`，并把编译后的 Rust export object 注册为 package factory。生成的 package-global 唯一且使用 `var`，因此独立装载的 package 不会冲突，同一个 rebuilt script 也能在 HMR 期间再次执行；异步 WASM initialization 不会泄漏进同步 factory contract。

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

- **Session**（session.ts）：懒建、常驻——建成后在后台持续吃帧，切走切回秒显。操作面：`prompt`/`cancel`（RPC 透传；失败落进快照的 `promptError`）、`open`（拉尾页 history，幂等）、`loadOlder`（向上翻页，防重入）、`resync`（重连 = 清窗口重跑 open）。订阅面：`subscribe`/`getSnapshot`（恒返缓存引用）——`implements ObservableSnapshot<ConversationSnapshot>`，构造时挂 `useSelector = bindSnapshotSelector(this)`，Session 本身就是 uSES 源。帧分发是一个 switch：`session/event` 帧按 seq 去重（唯一去重键），open 在途时缓冲，否则追加 + 增量投影；open/缝合按 seq 合并 live 缓冲并去重，`subscribed.lastSeq` 超出窗口尾则回补一次。
  Rust `ClientSession` core 通过注入 seam 获取 transport、时区解析、通知调度与 detached-task ownership。调用 `open()` 与 `prompt()` 时，源码可观察的 loading 与 blank→engaging 前缀会在返回 future 前同步发生；共享 open work 受 identity guard 保护，每次 history await 都重检精确 resync generation。Paging 保持单一连续窗口，live gap 在一个 tail repair 后合并，pending wait 跨 resync 重新铸造，queue baseline 只在 `session/subscribed` 清理，而 Host-computed projection 则在实例化与 reload 之间沿用同一个 higher-seq-wins store。
  它的 WASM facade 在不改变对象层协议的前提下适配 generated API client 与 Remote namespace：并发 open 共享同一个 JavaScript Promise，`getSnapshot()` 缓存精确的顶层对象，未变化的 Chat/pending/queue/projection reader 保持 JavaScript identity，generated RPC result 仍是 plain object，attachment bytes 仍是 `Uint8Array`，pending `respond()` 则在穿过 carrier 前恢复私有 `rpcId`。
- **ConversationSnapshot**（conversation.ts）：顶层不可变快照约定。`chat` 包含结构化 `order`、identity 稳定的 keyed Node reader、Turn/Step index 和 timeline；`nodes`、`partial`、`runningCalls`、`turnTimings`、`turnEnds` 是未迁移 Trajectory 消费方使用的兼容 slice。pending interaction、queue、running、removed、open state、paging 和 prompt error 仍是 Session 信息。**引用纪律**（memo 与 uSES 的前提）：未变化的子结构和 Node value 保持引用；单个业务更新只替换对应 key 的 value，除非它的顺序或 Location 发生变化。React 仍只订阅 Session 这一处 observable source，并由框架提供的 `useSession(selector)` 隔离 Node 与 Location 聚合更新。
- **SessionManager**（manager.ts）：实例簇 + 帧总入口 + 会话列表。带 sessionId 的帧只投已存在实例（mux 广播不得把每个会话都实例化）；例外是审批/问答 `requested` 帧——它们不落 history、open 无法回补，故缓冲进 `pendingBuffers`，实例化时回放。
  Rust `SessionManager` core 让 Session instance、projection store、pending status、queue/request buffer、completion reminder 与 job mirror 分处独立 lifecycle axis。List pull 使用 single-flight，并在 baseline 上 replay 全部 in-flight mutation，同时不重排已建立 ID；projection 与 job frame 可先于实例化落地；`session/subscribed` 截断 phantom projection row 并清除该 generation 省略的 mirror；answerable request 按稳定 key 压缩，并在 generation 丢失时死亡；只有直接 user message 推进 activity；running→idle 也只在 Session 未被观看时 arm reminder。
  Direct-child catalog 是独立保留的 read model：健康 row 铸造 durable parent/child transport address，普通 selection 不得擦除它；activity 与 expandability frame overlay 较旧的 in-flight response；activation removal 让 durable child 回到 inactive，而非删除 lineage；parent removal 即使遇到更早的 catalog response 仍在途，也会立即使每个 addressed child 的 writable availability 失效。Membership frame 对每个 selected 或 open parent 使用一个注入的 50 ms debounce；若计时器在 pull 期间触发，它会把该 response 标为 stale，并在 settlement 后精确调度一次 trailing refresh，而关闭 catalog 会取消计时器。
  Manager 的 WASM facade 对每个 Rust Session 缓存一个 JavaScript wrapper，并对每个 Rust snapshot 缓存一个 list object；raw mux/Host envelope 进入同一个 core，generated RPC result 保持 plain object，而 search 继续是 request-local：调用方的精确 `AbortSignal` 直接传给 `api.sessions.search`，不向 list observable 添加 query state。
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
