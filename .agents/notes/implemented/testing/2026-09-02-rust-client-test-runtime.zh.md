# Agent Note: Rust 客户端测试运行时

Status: implemented

[English](2026-09-02-rust-client-test-runtime.md) | 中文

## 问题

客户端功能测试共享一组带行为的测试替身，用于 Cordis 服务、可观察存储、locale 选择、翻译、Session 与 Workspace fixture、稳定化和 DOM 快照。若在每个 Rust/WASM 功能测试旁重复实现这些 helper，同一测试约定会在多个 crate 之间漂移；若保留 TypeScript helper 包作为可执行基础设施，则会违反 Rust 生产边界，并让后续 Web 测试移植继续依赖第二套运行时。

## 决策

`seekdeep-client-test-runtime` 是可跨目标使用的 Rust 客户端测试替身真源。生产代码已有 Rust 约定时，每个 helper 都实现该约定，并且只增加显式测试控制，例如发布、调用记录或不透明事件分发器。仅浏览器需要的环境控制只在 `wasm32` 下编译；该 crate 不进入产品插件图。

Remote 测试替身发布 `remote` Cordis 服务，把不透明的转发参数交给按注册顺序截取的 listener 快照，使订阅 dispose 幂等，让未知事件保持无动作，并以源实现的诊断拒绝挂载生成 namespace。Listener 错误会有意向外传播，因此该测试替身不能充当生产 Remote 服务错误隔离策略的证据。

Settings stub 实现 `ClientSettingsScope`，以源实现的 loading、只读、Host 快照启动，按顺序记录 `set` 与 `unset` 调用，只替换发布时显式提供的字段，并同步通知 listener 快照。Translator 使用首个字典命中、保留未命中 key、ASCII word placeholder，以及与 JavaScript 一致的参数字符串转换。WASM 浏览器语言 guard 安装相同的可配置自有 `navigator.languages` 和 `navigator.language` 值，并在幂等清理时删除两者，从而重新显露继承的 accessor。

Workspaces 测试替身持有生产 `SnapshotStore<RuntimeWorkspaceListState>`，通过注入的稳定化 owner 执行列表变更，把精确的取消 signal 转发给目录 stub，并把每项 action 记录成带类型的有序值。无动作默认实现保留源实现的 echo、从根到目标的 home breadcrumb、目录取消和 archive 列表发布；带类型的替换 stub 则显式表达 action 失败与联动测试行为。

快照规范化器只折叠符合 `_<local>_<lowercase-hash>` 的 CSS-module token，按 JavaScript UTF-16 code unit 使用 wrapping FNV-1a 计算 SVG `data-content` 指纹，只修改深克隆，并保留没有 child 的 SVG 元素。Rust 快照测试直接调用该规范化器，而不安装 Vitest serializer。

Session fixture 为一个 identity 加上品牌类型，并携带针对生产 `SessionSnapshot` 与 `RuntimeSessionSummary` 的强类型变更，以及不透明的行为 override。原生静稳 Session 构造器遵循目标 object model，WASM 构造器则保留源浏览器对象的每个字段，以及 `null`、`undefined`、Array 和 Map 区别。Workspace fixture 默认值与 Workspaces 测试替身共用。

## 验证

聚焦源测试固定 Remote 测试替身、settings stub、translator 消费方、浏览器语言消费方、Workspaces face、快照规范化和 fixture 默认值。原生 Rust 测试固定订阅顺序、dispose、错误传播、settings 发布与写入记录、翻译转换、Workspace 稳定化、全部 action 默认实现与 stub、浏览取消、archive 发布、class 折叠、UTF-16 指纹和目标 Session 默认值；实时 WASM 测试固定 Remote 与 Workspaces 服务 face、浏览器语言恢复、只修改克隆的 DOM 规范化和精确浏览器 fixture 形状。`cargo xtask parity` 只映射具有直接源端与目标端证据的 helper；其余 Session 和运行时 assembly helper 继续保持 pending。

## 曾考虑的替代方案

**把 test-runtime 行为内联到每个功能 crate。** 不予采纳，因为共享默认值、dispose 语义和失败策略会拥有多个权威；生产 face 变化后，互不相关的 fake 也可能悄然不一致。

**因该包仅用于测试而保留 TypeScript。** 不予采纳，因为可执行测试基础设施参与 parity 和组装 Web 验证；保留它会在后续源测试依赖最集中的位置继续维持第二种实现语言。

**每个测试都使用生产服务。** 不予采纳，因为聚焦功能测试需要确定性的发布与失败注入，而且源 Remote 测试替身有意传播 listener 错误，生产实现则会隔离错误。需要生成 namespace 或网络行为的测试改用真实服务和已构建集成门禁。

## 后果

功能测试移植获得一个可复用的 Rust 受控环境真源，并针对与生产相同的可移植约定编译。该 crate 增加了显式测试专用 API，必须与源 helper 变化和生产 face 变化同步维护。它不会让尚未移植的 test-runtime helper 自动完成；只有 Rust 实现与聚焦证据落地后，对应 manifest 行才会离开 pending。
