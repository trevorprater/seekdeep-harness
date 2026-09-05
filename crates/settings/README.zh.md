# seekdeep-settings

[English](README.md) | 中文

SeekDeep Harness 的用户设置能力边界。一个提供方拥有按 namespace 分节的原始文档。插件
注册 namespace schema，并读取按 schema 默认值、注册方组合 `base`、用户分节依次分层
解析的值。没有提供方时，可选消费方继续只使用组合配置。

## 服务 API

- `document_path()` 在提供方拥有用户可编辑文件时返回绝对路径。浏览器协议只暴露派生的
  可用性，绝不暴露宿主路径。
- `prepare_document()` 准备本地文档供编辑器打开并返回路径；非文件提供方返回 `None`。
- `register(owner, ns, schema, options)` 返回受 owner 生命周期管理的 `SettingsScope`。
  owner 卸载会移除 namespace 和观察者。重复 namespace 与非法存量分节在注册时立即失败，
  这是 schema 和 owner validator 能做判断的最早时点。
- `describe(redact)` 按注册顺序返回 descriptor，包含规范 schema JSON、解析值、原始分节
  revision、分离的 `base` 与 `user` 层、生效时机，以及请求脱敏时的 secret slot。每个
  wire 接口都必须请求脱敏。
- `get(ns)` 返回当前解析值；未注册时返回 `None`。
- `update(ns, patch, expected_revision)` 只把对象 patch 深合并进用户层，校验完整解析候选，
  经提供方持久化后提交。数组和标量整体替换低层值。
- `replace(ns, section, expected_revision)` 整体替换用户层；空对象使所有键重新继承 base
  和 schema 默认值。
- `mutate(ns, ops, expected_revision)` 在写入到达队首时的分节上，按序执行路径 `set` 和
  `unset`。它适合编辑脱敏后的不完整视图，因为不会重述调用方从未收到的 secret。

Rust 调用方传入拥有所有权的 `serde_json::Value`，因此输入天然分离且具有 JSON 形状。
兼容绑定必须在构造这些值之前拒绝函数、日期、Map、大整数、非有限数字、数组中的 undefined、
类实例和循环引用，并返回带路径的错误，从而匹配源边界。

每个 descriptor 的 `revision` 是针对原始用户分节的单调计数器。陈旧期望以
`SettingsConflictError` 失败，携带稳定代码 `SETTINGS_CONFLICT` 和两个 revision。
检查发生在每 namespace 队列的队首，因此排队中的前序写入无法静默覆盖编辑器。存入相同
分节不推进 revision；存入一个等于组合 base 的覆盖值会推进 revision，即使解析值不变。

scope 返回按所有权分离的不可变快照。每个 watcher 的异步调用各自按提交顺序串行；同步 panic
和异步失败均被隔离。watcher 卸载后不会启动已排队或未来调用，已经开始的工作会结算。服务
卸载拒绝新工作，并排干排队写入和已启动 watcher。registrant 在持久化期间消失时，写入仍会
到达存储，但不会向旧 owner 提交或通知。

## 提供方约定

提供方实现可写状态、`load` 和 `persist`，并可暴露和准备一个本地文档。
`SettingsService::install` 在发布服务前完成加载，并在可回滚的子生命周期中拥有整个服务。
提供方 watcher 通过弱引用 `SettingsPublisher` 发布完整外部文档。

publish 时每个已注册 namespace 独立重解析。非法分节会保留该 namespace 的最后可用值并
告警，其他 namespace 继续处理；存储恢复合法后该 namespace 也会恢复。启动期加载失败和
注册期校验失败仍立即报错。

`install_settings_section` 是规范的可选提供方接线：把组合 entry 用作 `base`，提供方存在
时选择已注册解析 scope，观察实时提交，并在提供方消失时恢复 entry。消费方自身卸载保持静默，
包括消费方卸载期间落地的存储变更。

## 事件

`settings/updated (ns, next, previous, source)` 在解析值改变后触发；source 是封闭枚举
`update` 或 `provider`。深度相等的值保持静默。

`settings/document-updated (ns, revision)` 在原始分节改变时触发，无论解析值是否改变。
配置界面用它刷新覆盖状态和冲突 revision。

两类事件都会扇出到所有同步 listener。普通 listener 失败会被隔离并记录；invariant 失败在
其他 listener 全部运行后传播。浏览器绑定暴露同一份 client-safe namespace、source 和
descriptor wire 词汇。

## 模型体验

settings 只通过解析模型相关值的消费方间接影响模型，例如默认路由。服务本身不直接改变请求
前缀或 KV cache；该影响由各消费方拥有。

## 已知限制

- 解析只有 schema 默认值、一个组合 base 和一个用户层，不报告逐字段来源。
- `redact_secrets` 不是已证明安全的 wire 边界。它只跟随 object、dictionary 和 array；
  只能经 union、intersection 或 transform 到达的 secret 可能原样通过，序列化 schema
  默认值也可能泄露 secret。在出现 fail-closed descriptor API 前，暴露于 wire 的 namespace
  必须使用 walker 能证明安全的 schema。
- 跨进程并发由提供方定义；seam 只按 namespace 串行化进程内写入。
