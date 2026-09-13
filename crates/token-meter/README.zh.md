# seekdeep-token-meter

[English](README.md) | 中文

SeekDeep Harness 的回放感知 token 测量服务。单例 `TokenMeter` 从持久化的仅追加日志中，
为每个会话推进相互隔离的 fold，使压缩及其他依赖上下文压力的插件共享同一计量约定，
而无需依赖压缩引擎。

## 配置

估算器只接受 `{}`，并刻意使用一项固定启发式规则：每四个 UTF-16 代码单元估算一个
token，再加上角色、内容块和请求 envelope 字段的结构开销。未知键会在配置阶段失败。
模型容量属于拥有精确提供方／模型路由的适配器。

## 测量约定

服务公开两个操作：

- `measure(session, request_header)` 在同一个已消费日志 revision 上返回请求压力和当前
  已计价表层。
- `estimate_message(message)` 使用固定启发式规则为一条消息计价。

`measure()` 只同步一次并返回独立快照。`total_tokens` 是请求与响应压力；
`surface_tokens` 是仅表层的启发式总量，等于 `nodes[].tokens` 之和。请求标头覆盖只
影响压力字段，表层字段仍描述当前会话。每次调用都会克隆带位置的节点，因此测量为
O(surface)。

fold 跟踪规范化请求标头快照、步骤边界、表层追加与替换、成功的 assistant 消息、
提供方用量，以及每条 assistant 消息引用的分片序号。只有当最新成功调用的规范请求
envelope 与当前测量的 envelope 相同，且提供方总量不低于该调用的完整启发式锚点时，
才会复用提供方用量。后一次成功会替换旧锚点；否则完整估算当前 envelope 与表层。
相对匹配锚点的表层变更保持有符号值，包括缩减替换后的负 delta。

用量会求和互不重叠的输入、缓存读取、缓存写入和输出 bucket；不会再次添加 reasoning。
每次成功调用都会记录 assistant 锚点，包括无内容调用。显式空 `sourceEventSeqs` 表示
已知空提供方流；遗留记录缺少该列表时，会保守地把持久 assistant 输出视为提供方输出。
未读历史一旦非法便事务性失败：缓存游标不会越过非法事件。

## 会话投影

当组合提供 `SESSION_PROJECTIONS` 时，token-meter 会在其自有生命周期内动态注册三个
单元。如果这个可选服务被撤回或替换，旧注册会先被移除再重新绑定。卸载 token-meter
会移除服务、监听器和全部三个投影键。

`tokenUsage` 携带完整持久日志中的 `uncachedInputTokens`、`outputTokens`、
`cacheReadTokens` 与 `cacheWriteTokens`。即使请求后来失败，用量分片仍会计入；同一
`(turn, step)` 的最终 assistant 消息用量会替换该样本，而不是重复计数。reasoning
仍是输出的细分项。它只保留最后一个样本，依赖合法日志的顺序规则：更晚步骤报告用量后，
更早步骤不能再次报告用量。

`contextPressure` 携带可选 `pressureTokens`（提供方最新报告的提示词规模，即未缓存输入
加缓存读取与写入）、可选 `projectedTokens`，以及最新 `request/context` 中的可选
`contextWindow`。提供方输出不计入。提供方报告用量前两个压力数字保持缺失；路由未公布
容量时容量也保持缺失。

`projectedTokens` 估算下一次请求的提示词：把提供方样本沿取样后表层增减部分的启发式
价格推进，下界钳制为零，并使用与 `measure()` 相同的带位置表层 fold。因此内容落地或
压缩遮蔽区间时它会立即变化，即使压缩的直连模型调用没有追加用量样本。占用率展示应读取
该投影值。

`contextBreakdown` 携带启发式的 `systemTokens`、`toolsTokens` 与 `messageTokens`。
envelope 数值在 `request/header` 上后者胜；消息 token 重放与 `measure()` 相同的
带位置 fold，因此在每个事件边界都等于 `measure().surface_tokens`。这些是近似组成行，
不是总数；它们不必等于提供方锚定的 `projectedTokens`，尤其是 CJK 文本或 JSON schema。

三个单元均使用标准 baseline、实时 frame、序号高者胜存储与 JSON 检查点。压缩及 prune
检查点会把表层状态限制在有界大小，使遮蔽操作后的回放状态保持 O(1)。缺少投影服务的组合
仍保留正常测量行为。

### 上下文占用率刻意采用近似值

占用率字段是彼此独立的后者胜记录，并非一次原子观测。切换模型时，新容量可能暂时与上一路由
的样本配对，直到下一个请求报告用量。`pressureTokens` 描述最后一次请求；
`projectedTokens` 则把这个较旧锚点沿当前表层变更推进。

该百分比是面向用户的参考值，不是计费数据或门控输入。harness 不依据它做决策；压缩会在自己
的请求边界调用 `measure()`。需要同一边界精确数值的消费方也应这样做。

## 组合

```yaml
- name: seekdeep-token-meter
- name: seekdeep-compaction-basic
```

两个插件都有可用默认值。meter 与模型路由及可选压缩保持独立。部署在 LLM 适配器上配置容量，
在 `seekdeep-compaction-basic` 上配置压缩策略。

## 模型体验

服务自身不会添加提示词、消息、schema、工具或模型调用；它只通过压缩等消费方间接影响模型。
它不直接使 KV cache 失效；任何请求前缀变更由消费方负责。

## 已知限制

- 固定启发式规则只是近似值，并非提供方 tokenizer 或精确请求 serializer。
- 每次测量都会克隆当前表层以返回一致的独立快照，因此读取为 O(surface)。
- 仅完全相同的规范 envelope 可复用提供方用量；提示词、前缀、工具、提供方、模型或调用配置
  发生变化时会回退到估算。
- 缺少源事件序号的遗留 assistant 消息无法区分提供方输出与监听器改写，因此会保守处理。
