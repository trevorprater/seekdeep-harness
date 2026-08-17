# seekdeep-llm-deepseek

[English](README.md) | 中文

这是 SeekDeep Harness 的 DeepSeek chat-completions 直连适配器。它使用
`reqwest` 和严格的 SSE 解析器，将 DeepSeek 官方流式协议转换为
`seekdeep_llm::StreamChunk`。

本 crate 拥有 `deepseek-official` 提供方路由。该名称刻意区别于库适配器的
`deepseek` catalog 名称，因此同一组合可以同时安装两者。为
`deepseek-official` 再注册适配器会以 `DUPLICATE_ADAPTER` 失败。

crate 根入口公开 Cordis 插件、`DeepSeekAdapter` 及公共配置／协议类型。序列化、SSE、
转换和 HTTP 分类 helper 保留在各自明确的源码模块中，不会平铺到根 API。所有运行时
行为均由 Rust 实现。

## 配置

序列化配置保留源实现的 camel-case 字段约定：

```yaml
apiKeyEnv: DEEPSEEK_API_KEY
baseURL: https://api.deepseek.com
thinking: enabled
reasoningEffort: high
maxTokens: 256000
streamIdleTimeoutMs: 300000
retryPolicy:
  mode: always
  backoff:
    initialDelayMs: 500
    maxDelayMs: 10000
    jitterRatio: 0.1
defaultContextWindow: 1000000
models:
  - id: deepseek-v4-flash
    name: DeepSeek-V4-Flash
  - id: private-reasoner
    description: Company-hosted reasoning model
    contextWindow: 512000
```

`DeepSeekConfig::default()` 依次解析凭据引用 `DEEPSEEK_API_KEY`、环境中的
`DEEPSEEK_BASE_URL`，最后回退到公共端点 `https://api.deepseek.com`。默认 catalog
公布 `deepseek-v4-flash` 与 `deepseek-v4-pro`，上下文窗口均为 1,000,000 token。
显式 `models` 列表会替换默认值；`models: []` 不公布任何模型。Catalog 仅供建议：
未列出的模型 id 仍会原样进入协议请求。

确切模型解析优先使用配置项的 `contextWindow` 与 `maxTokens`，否则分别回退到
`defaultContextWindow`（1,000,000）和路由级 `maxTokens`（256,000）。运行时会在记录
`request/header` 前填入输出默认值。显式请求值优先；适配器不会按上下文窗口缩减输出
token 上限。

启用思考时，确切模型元数据公布 `off`、`high` 和 `max`；省略配置时默认 `high`。
`high` 与 `max` 序列化为 `reasoning_effort`。`off` 序列化为
`thinking: { type: "disabled" }` 并省略 `reasoning_effort`。`thinking: disabled`
是部署锁定：仅 `off` 合法，启用推理的尝试会在网络 I/O 前失败。会话标题请求也会
强制关闭思考。

所有数值边界与跨字段不变量都在注册前校验。`streamIdleTimeoutMs` 必须为正有限数，
且不大于 2,147,483,647。Catalog id 必须非空且唯一。

## 动态设置与凭据

`install()` 注册可实时更新的 `llm-deepseek` settings 分节。连接事实按操作解析一次，
因此新的 base URL、catalog、请求默认值、空闲超时或凭据引用从下一次操作生效，而
进行中的流保留其起始世代。违反 resolver 不变量的实时 settings 快照不会替换最后可用
世代。重试策略变化时，会以同一适配器实例原子替换已注册路由。

配置只保存凭据引用，从不保存字面 API key。每次流调用经 credentials 服务解析该引用；
未安装服务时则使用捕获的启动环境。缺少 key 以 `MISSING_CREDENTIAL` 失败；无法安全放入
HTTP 标头的值以 `INVALID_CREDENTIAL` 失败。两类诊断都不包含密钥内容。即使没有 key，
提供方和建议 catalog 仍可浏览，因此存入凭据后无需重启。

## 传输与生命周期

每次 `stream()` 调用恰好发起一个 HTTP 请求。同一个稳定取消信号负责请求及每次 body
读取。调用方取消产生 `ABORTED`；读取空闲超时产生 `TIMEOUT`；连接或流中传输错误产生
`TRANSPORT` 并保留 cause。SSE 注释属于传输活动，会重置未完成读取的空闲计时，但绝不
变成模型分片或日志事件。

适配器把重试策略注册为提供方元数据。持久化重试由 agent loop 执行，因此适配器本身
不会发起第二个提供方请求。插件释放时会同时注销适配器路由与可配置提供方目录条目。

## 请求身份与归因

每个请求携带共享 SeekDeep `User-Agent` 归因，并通过
`x-seekdeep-harness-user-id` 携带稳定匿名身份。含会话 id 的请求还携带
`x-seekdeep-harness-session-id`。压缩调用增加 `x-seekdeep-harness-compact: 1`。
这些标头会发送到解析后的端点（包括配置的 gateway），但不是模型可见请求内容。

## 协议行为

- 请求只使用流式模式，并始终设置 `stream_options.include_usage`。
- Usage 保留到 `[DONE]`，随后在终止 finish 分片前发出；finish 后不再产生任何值。
- 初始空 `reasoning_content` 不会创建多余块。
- 仅在含工具调用的 assistant 历史轮次回传推理，这是 DeepSeek thinking mode 的要求；
  其他先前推理会省略。
- Cache read 来自 `prompt_cache_hit_tokens` 或
  `prompt_tokens_details.cached_tokens`；此协议不提供 cache-write 指标。
- 未知 finish reason 变为 error finish。成功 stop 若未打开任何内容块，则变为
  `EMPTY_RESPONSE`。
- User 与工具结果内容会展平为文本；未知插件块会跳过，空工具输出变为 `(no output)`。

## 错误

非成功响应映射为稳定 code：401/403 为 `AUTH`；可识别的余额或点数耗尽为 `QUOTA`；
其他 429 为 `RATE_LIMIT`；可识别的上下文溢出 400 为
`CONTEXT_WINDOW_EXCEEDED`；其他 400 为 `INVALID_REQUEST`；5xx 为 `SERVER`；
其余为 `HTTP_<status>`。可序列化 failure 保留状态码、有效的正 `Retry-After`，以及
存在时的 `x-request-id` 或 `x-deepseek-request-id`。畸形 JSON 事件以
`MALFORMED_RESPONSE` 失败；缺少 `[DONE]` 的流以 `STREAM_CLOSED` 失败。

## 验证边界

Mock HTTP 集成测试覆盖请求序列化、精确标头、动态配置与凭据轮换、HTTP 错误映射、
SSE 分帧、取消、超时及释放，且不会联系 DeepSeek。源实现中需要凭据的实时 API 测试
属于独立外部验证步骤；离线测试通过并不意味着该步骤已验证，也不得意外运行它。

保留的源实现限制包括：`models` 数组整体替换；`tool_choice` 不属于核心词汇；适配器不
共享 proxy／拦截服务；内容序列化只支持核心文本与工具词汇。
