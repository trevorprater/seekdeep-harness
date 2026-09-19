# seekdeep-util timeout

[English](README.md) | 中文

这是 `seekdeep_util::timeout` 中无需提供方的超时原语。它拥有超时算术、首个原因
取消融合、强类型超时分类和空闲看门狗。它只通过 `AbortSignal` 发出通知；各能力
自行负责停止工作，并把原因转换成自己的公开结果。

## 超时算术

`clamp_timeout(requested, default, maximum, name)` 只验证显式提示必须为正数且有限，
然后按 JavaScript 兼容语义返回
`min(requested.unwrap_or(default), maximum)`。零不是调用方可见的禁用哨兵。后端
默认值和上限归后端所有，本辅助函数不会静默重新验证它们。

`MAX_TIMER_DELAY_MS` 为 `2_147_483_647`，即源运行时不会把它压缩为一毫秒的最大
延迟。带计时器的 API 会以源字段专用诊断拒绝非有限、非正数或更大的值。

## 截止时间与分类

`deadline(upstream, timeout_ms, code)` 返回稳定的融合信号和只释放一次的
`Deadline`。上游取消与计时器中先发生者获胜，其精确强类型原因保持权威。丢弃或
显式释放 deadline 都会取消计时器。非正超时是内部无计时器哨兵：它只转发上游
信号，或者生成永不取消的信号。

计时器到期携带 `TimeoutReason { code, timeout_ms }`，显示为
`<code> after <milliseconds>ms`，并在信号 JSON 原因中表示为 `name`、`message`、
`code` 和 `timeoutMs`。`timeout_of` 只识别强类型原因，并可要求精确 code。结构
相同的 JSON 对象不能伪造分类，嵌套的外层 deadline 也不会被误判为内层能力的超时。

## 空闲看门狗

`IdleWatchdog::new(upstream, timeout_ms, code)` 创建一个稳定信号。只有
`next(stream)` 存在未完成的提供方需求时才启用计时器，因此消费方思考时间不计入
空闲时间。`pulse()` 会在带外传输活动发生时重新为同一未完成需求计时。并发需求和
释放后的需求都会明确失败。释放是幂等的，并会清除已启用计时器。

## 模型与缓存影响

本模块不直接产生影响。消费方决定超时结果是否对模型可见，并拥有任何请求前缀变化。

## 限制

通知是协作式的。计时器会中止信号，但不会自行终止进程、套接字、提供方流或工具体。
