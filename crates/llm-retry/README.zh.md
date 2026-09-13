# seekdeep-llm-retry

[English](README.md) | 中文

这是 SeekDeep Harness 按提供方路由的模型请求恢复插件。Rust 插件监听持久化 agent loop
的 `agent/request-error` waterfall；它不会包装 `LlmRuntime::stream()`。一次适配器调用始终
只代表一次提供方尝试，直接使用原始流的调用方仍然只尝试一次。

提供方注册拥有解析后的 `retryPolicy`。服务于 prepared call 的确切策略会随失败一起传递，
因此之后的路由替换或释放不能追溯改变进行中的决定。在选中最终适配器注册前发生的失败
没有策略，会委托给下游。

## 策略

省略提供方配置时使用 normal 模式：对 `EMPTY_RESPONSE`、`RATE_LIMIT`、`SERVER`、
`TIMEOUT` 和 `TRANSPORT` 重试两次，采用从 500 ms 到 10 秒的指数退避及 10% 对称抖动。
Normal 模式可以替换重试次数、可重试 code 与退避参数：

```yaml
- id: llm-deepseek
  name: seekdeep-llm-deepseek
  config:
    retryPolicy:
      mode: normal
      maxRetries: 3
      retryableCodes: [RATE_LIMIT, SERVER, TIMEOUT, TRANSPORT]
      backoff:
        initialDelayMs: 1000
        maxDelayMs: 30000
        jitterRatio: 0.2

- id: llm-retry
  name: seekdeep-llm-retry
```

Always 模式先询问下游恢复中间件。若下游选择重试，该决定优先；否则它会无限次重试每种
模型请求失败。即使下游同步或异步报错，也不会禁用该后备策略。成功、轮次取消或插件释放
会结束链。

执行器自身只接受 `{}`。在本插件上配置 `retryPolicy` 会给出明确失败，因为策略属于每个
提供方；其他未知 key 同样会被拒绝。

## 延迟选择

本地延迟使用有上界指数退避与对称抖动。完整抖动的下界可以恰好为零毫秒。若提供方的
`providerRetryAfterMs` 为正有限数且不超过 `maxDelayMs`，则原样替换本地延迟。超过上界的
提供方延迟会使 normal 模式委托下游；always 模式改用本地退避，以保持无限重试约定。

## 持久事件

等待前，插件追加非 surface 的 `llm/retry` 事件，其中包括：

- 同一提供方策略链共享的稳定 `retryId`；
- 当前 turn 与仍然打开的 step；
- 确切提供方路由与模式；
- 含所有影响行为字段的规范策略 key；
- 从一开始的重试序号，以及 normal 模式的 `maxRetries`；
- 选定的 `delayMs` 与完整的提供方中立 failure。

Normal 模式的 code 在策略 key 中排序，因为成员关系与顺序无关。只有 turn、step、provider
及完整策略 key 均相同时才延续编号；策略或提供方变化会开始新链。

可取消等待完成后，`llm/retry-started` 会在 waterfall 返回
`RequestErrorAction::Retry` 前立即记录相同 identity、turn、step 与重试序号。取消不会写
started 事件。Loop 从持久 surface 历史重建请求，并在同一打开 step 中重复；失败分片保留
为诊断事件，但绝不会成为 assistant message、工具副作用或后续模型上下文。

公共 `types` 模块提供可 serde 的 payload，浏览器或外语绑定无需加载计时策略即可使用。
`RetryId`、提供方身份和规范策略身份在 Rust 中是透明 newtype，在磁盘及 JSON 上仍为普通
字符串。

## 生命周期

一个插件生命周期拥有 listener、全部退避与委托恢复。释放会先注销 listener、取消活动
等待，并在完成前排空所有已捕获操作。Waterfall 在释放前捕获的 callback 会看到已关闭
生命周期并安全失败。Turn 取消也会在计划重试修改后续状态前胜出。

## 不变量

单独注册的 invariant companion 会校验既有历史及每个候选 append。Retry 事件必须位于其
命名的打开 turn 与 step 内，匹配生效 request header 的 provider，携带完整有效 failure 与
计时延迟，遵守模式边界及精确编号，并为每个 provider-policy 链保留唯一 identity。
`llm/retry-started` 必须关联恰好一个已计划尝试，且不能重复。

## 模型体验

Retry 事件、提供方错误、延迟及失败的部分输出都不是模型可见内容。每次重试可能重复计算
输入 token 费用，但未改变的重建前缀仍可按提供方规则复用 cache。Normal 模式具有有限请求
预算；always 模式可以持续消耗请求直至成功或取消，因此部署方拥有其成本与延迟策略。

离线集成测试覆盖 shipping loop、真实 Rust DeepSeek HTTP/SSE 适配器、拒绝连接、部分断开、
空完成、干净 EOF、空闲超时、预算耗尽、JSONL／SQLite 持久化、loader 组合、append-time
不变量、取消与释放。
