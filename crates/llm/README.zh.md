# seekdeep-llm

[English](README.md) | 中文

SeekDeep Harness 的提供方无关 LLM 词汇与运行时。本 crate 定义 agent loop、
只追加 session 日志、适配器和插件共同使用的规范协议。

## 运行时服务

`LlmRuntime` 安装在 Cordis 的 `llm` 服务键下，拥有适配器注册表，以及一条可由
middleware 包装的流式调用路径。

- `register_adapter()` 将一个适配器实例原子地注册到非空提供方路由集合。返回的
  `AdapterRegistrationHandle` 在 dispose 时撤回全部路由，也可以原子地 `replace()`
  整个集合。存活注册可以替换为空集合；初始空注册无效；已 dispose 的注册替换路由
  会以 `REGISTRATION_DISPOSED` 失败。
- `list_providers()` 按注册表顺序返回脱耦的提供方元数据。
- `register_configurable_providers()` 发布适配器可通过 settings 激活的路由，包括
  settings namespace、路径，以及 `api-key`、`provider-native` 或 `codex-oauth`
  认证方式。注册和替换都是全有或全无；`list_configurable_providers()` 返回脱耦条目。
- `register_model_discovery()` 为一个 settings namespace 提供端点查询能力。
  `discover_models()` 接受草稿路由或 base URL，并按端点顺序返回候选，丢弃空 ID
  与重复 ID。运行时不会读取、保存或采纳草稿凭据或查询结果。
- `provider_retry_policy()` 返回路由注册时捕获的不可变提供方策略。
- `list_models()` 返回经校验且脱耦的建议 catalog；catalog 成员身份绝不是路由白名单。
- `resolve_model_info()` 独立于建议 catalog 查询拥有该精确模型的适配器，校验身份、
  context、默认输出上限和 reasoning 元数据，并转发取消信号。
- `resolve_call_config()` 校验显式 reasoning effort，并填入适配器默认值，不做钳制。
- `prepare_call()` 只进行一次精确模型查询，同时捕获配置、context、默认字段标记、
  重试策略和适配器注册，返回一次性的 `PreparedLlmCall`。配置漂移或复用会以
  `INVALID_PREPARED_CALL` 失败。
- `stream()` 通过已注册 middleware 分发原始 chunk 流；用 `BlockAssembler` 组装
  内容块和终止结果。

提供方选择、精确模型解析、同步适配器分发、迭代器构造和迭代失败，会统一转换为一个
reason 为 `error` 或 `aborted` 的终止 `finish` chunk。Middleware、下游消费者和
迭代器清理失败仍是普通 Rust 错误，因为它们不是模型请求结果。原生消费者提前停止时
调用并等待 `LlmStream::close()`；兼容绑定把它映射为 async iterator 的 `return()`，
并且必须等待完成。

每次适配器或可配置目录拓扑提交，都会在新注册表已经可读后发出无载荷的
`llm/adapters-updated` 事件。观察者故障会被隔离，不能饿死后续监听器；带
`INVARIANT` code 的故障只会在 fan-out 完成后重新抛出。

`llm/stream` middleware 链是 waterfall。Middleware 接收拥有所有权的
`GenerateOptions` 与一次性 continuation，因此可以观察、改路由、包装或短路调用。
发出 chunk 后再重试没有可持久化的 attempt 边界；产品重试执行因此属于另行记录的
request-error 生命周期，而不是这个单次尝试运行时。

## 精确模型默认值

精确模型元数据是正确性查询，不是 catalog 装饰。缺少 context 表示容量未知；缺少
`default_max_tokens` 表示保留提供方默认值；缺少 reasoning 元数据表示不可选择
reasoning。无效事实分别使用 `INVALID_MODEL_INFO`、`INVALID_MODEL_CONTEXT`、
`INVALID_MODEL_MAX_TOKENS` 或 `INVALID_MODEL_REASONING`。

`default_max_tokens` 是每次请求的适配器默认值，不是模型硬限制；显式请求上限优先。
Reasoning ID 是适配器拥有的不透明 newtype，只接受精确公布值，只填入已公布默认值，
绝不做别名或钳制。Prepared call 从解析到分发保留同一注册，防止热重载把一个适配器
的能力结果与另一个适配器的请求组合起来。

## 消息与 chunk

`Message` 是不可变的 owned snapshot，包含 `MessageId`、role、content、source 和
保留的扩展字段。构造函数与调用方输入脱耦；`Message::from_existing()` 导入已有身份，
不会生成新身份。Assistant、user 与 tool-result 构造函数固定各自 role/source
不变量；tool-result 构造会让 source 与内容块引用同一个 `CallId`。

核心 `ContentBlock` 变体为 `text`、`reasoning`、`image`、`tool-call` 和
`tool-result`。未知的可扩展变体在序列化往返时保留 tag 与字段。模型 source 可以
携带适配器私有 replay state；分发前，只有历史提供方与目标提供方当前由完全相同的
适配器实例拥有时，replay state 才会保留。

原始流词汇为 `block-start`、`text-delta`、`reasoning-delta`、
`tool-call-delta`、`block-end`、`usage` 和 `finish`。`BlockAssembler` 保留
block 首次出现顺序，以第一次 close 为准，忽略 close 后的残余 delta，保留最后一项
usage 与 finish，并在 max-token finish 后丢弃未完成的 tool call。

`LlmCallConfig` 包含 provider、model、reasoning effort、temperature、最大 token
数与 stop 字符串。`call_config_equals()` 比较全部字段和 stop 位置。Rust 所有权提供
不可变请求边界；`mark_agent_loop_request()` 只标记精确请求值，让观察方区分可由日志
重建的 loop 请求与辅助调用。

## 故障、凭据与归因

`HarnessError` 是共享的 coded-error 基类，`LlmError` 增加经校验且可序列化的提供方
事实。`normalize_llm_failure()` 与 `normalize_adapter_rejection()` 保留有效事实，
且不会调用敌意 foreign accessor。`ErrorChainGraph` 让兼容绑定安全渲染 cause、
aggregate、共享节点、循环与不可渲染值；原生 Rust error 使用 `error_chain()`。

`normalize_api_key()` 去除 ECMAScript 语义的首尾空白，只接受非空、无空格的可打印
ASCII。`assert_usable_api_key()` 会指出应修复的 setting，绝不在诊断中包含被拒密钥。

每个产品适配器都添加 `attribution_headers()`。默认身份为
`SeekDeep Harness/<crate version> (+https://github.com/deepseek-ai/seekdeep-harness)`；
白标调用方可以替换身份，但不能删除归因。

## 模型体验

本服务不添加模型可见文本或 tool schema；它只校验和填入由 agent loop 记录到 session
日志的适配器配置。注册表保留请求前缀，KV cache 行为属于所选提供方。

## 已知限制

- 本 crate 只执行一次适配器尝试；重试执行、缓存和限速属于各自另行记录的生命周期组件。
- Sampling 字段只包括 temperature、最大 token 数与 stop 字符串。
- 扩展 block 在权威 `block-end` 后可以保留；未知且未闭合的 block 无法组装。
- Rust `String` 不能包含未配对 UTF-16 surrogate，因此精确的孤立 surrogate summary
  截断通过 `bound_context_summary_units()` 暴露。
