# @seekdeep-ai/seekdeep-client-test-runtime

[English](README.md) | 中文

面向 Client 功能测试的 Rust/WASM 浏览器 Slot 测试运行时。其 ESM 入口初始化已编译的 `seekdeep-client-test-runtime` crate；该 WASM 链接已编译 Cordis 与已编译 Web React 渲染器，并且只把 React、Testing Library、Vitest 与 Immer 作为框架绑定 adapter 注入。生命周期、fixture、observable、ownership 与 assembly 行为均留在 Rust 中。

`SlotTestRuntime.create()` 组装真实的已编译 Cordis `Context`、生产 Rust Slot 与 Conversation registry、生产 Web React 渲染器，以及由 Rust 持有的 Session 和 Workspace 测试替身。功能套件无需逐套件重建这些机制，即可测试声明、注册、scope、Store identity 与清理、inject、渲染、更新、Fiber dispose 和完整 teardown。挂载功能会获得绑定到调用方的 registry face，因此注册项、已声明子 Slot、已提供服务和 Conversation definition 都与功能 Fiber 共用生命周期。

公开测试替身保留 oracle 的 face：`TestSessions`、`TestWorkspaces`、`FixtureSession`、`TestRemote` 和 `stubSettingsScope`；provide bundle 物化委托给生产 Rust `SessionProvideChannel`。fixture 灌入普通列表行、不可变 conversation 快照、projection 值与显式行为 override。未提供 stub 的 Session 动词会带方法名立即失败，Remote listener 错误会有意传播，settings 写入则仍是包裹 Rust 所持 observable 状态的 Vitest spy。

对于局部 DOM 快照，`declare(children)` 安装自动 frame，以逐 key 的 `<div data-slot>` 包裹层作为快照根；`renderSlot(key, owner)` 返回局部 container、限定范围的 Testing Library 查询与原位 `update(owner)`。Vitest serializer 在克隆对象上把 class 折叠与 SVG 指纹计算委托给 Rust。需要自定义页面 frame 的套件使用 `root.declare(children, Frame)`；`mount(plugin)` 对所需服务执行立即失败的预检，`dispose()` 则沿单一轴拆除视图、功能 Fiber、已创建的 Session scope 和持久化 Store 状态。

使用 `cargo xtask wasm-package --package seekdeep-client-test-runtime --artifact seekdeep_client_test_runtime --module-id @seekdeep-ai/seekdeep-client-test-runtime --out-dir packages/test-support/client-runtime/lib` 构建可分发测试库。被忽略的 `lib/` 目录包含嵌入优化 WASM byte 的 ESM wrapper、可供检查的独立 WASM artifact、declaration，以及从 Rust source 生成的 invariant companion。

不属于产品插件图（无 `seekdeep.client`）；feature 包仅以 `devDependencies` 依赖之。

## 模型体验

无；本包是浏览器侧测试基础设施，无一物到达模型请求。

#### KV Cache effect

无；本包既不组装也不发送提供方请求。

## 已知限制与延期工作

- **需要浏览器测试环境。** ESM 入口导入 Vitest 与 Testing Library，并在不发起网络 fetch 的情况下初始化嵌入的 WASM byte。请通过具有 DOM 全局对象的 ESM 感知浏览器测试 runner 或 bundler 使用；它不是纯 Node utility module，也绝不进入产品 plugin graph。
- **会话快照是 fixture 数据，不是重放历史。** `updateSnapshot` 直写快照 store；wire 到快照的运算仍由 runtime 包自身测试与 replay e2e 把守。因此 fixture 可以表达生产投影永不产出的状态。
