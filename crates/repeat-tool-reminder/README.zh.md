# seekdeep-repeat-tool-reminder

[English](README.md) | 中文

这是一个仅提供建议的循环中断器，而非面向模型的工具：它不会出现在工具列表中，不会否决或改写调用，只增加一种行为。它监视每个 agent（智能体）的工具调用流，统计以完全相同的规范化参数连续调用同一工具的次数；达到所配置的连续次数时，它会注入逐级增强的提示，要求模型停止重复、重新阅读上一次结果，并改用其他方案或结束任务。最终决定仍完全由模型作出：合理的重复调用既不会延迟，也不会受阻。

## 配置

```yaml
- id: repeat-tool-reminder
  name: repeat-tool-reminder
  config:
    thresholds: [3, 5, 8]        # default; consecutive counts that trigger a reminder
    include: []                  # tool-name patterns to track; empty means all tools
    exclude: [todo_write]        # tool-name patterns transparent to the chain
    argumentsPreviewChars: 500   # default; cap on arguments quoted in a detailed reminder
```

插件加载时，`thresholds` 会对错误配置快速失败：空列表、非整数、小于 2 的值或重复值都会返回错误，绝不静默回退到默认值；`argumentsPreviewChars` 同样只接受大于等于 1 的整数。系统会将阈值按升序规范化；第一个阈值发送简短的通用提醒，后续每个阈值发送详细版本，列出工具、连续次数和规范参数。参数内容截取前 `argumentsPreviewChars` 个字符并附带省略字符数标记，避免循环中的写入／编辑载荷无限制进入下一次请求；检测始终比较完整的规范字符串。

`include`／`exclude` 条目支持 `*` 通配符，并针对调用时看到的工具执行谓词判断，而不是引用注册表条目。因此，与当前任何已注册工具都不匹配的模式仍然有效；例如，未加载 MCP 工具的部署中，`exclude: [mcp_*]` 仍然合法。

## 链语义

链键为「`(tool name, canonical arguments)`」。规范化会对 JavaScript 键进行深度排序，然后执行与紧凑型 `JSON.stringify` 兼容的序列化，因此仅属性顺序不同的参数对象视为相同。若调用与上一条受跟踪调用相同，该 agent 的连续计数器递增；换成另一条受跟踪调用则重置为 1。

- **不受跟踪的调用对链透明。** 被 `include` 或 `exclude` 排除的调用既不递增也不重置计数器；因此，排除 `todo_write` 后，`grep X → todo_write → grep X` 仍算作连续两次 `grep X`。
- **被拒绝的调用也计数。** 检测位于 `tools/post-execute`；即便调用被 `tools/pre-execute` 监听器拒绝，该阶段仍会运行。模型反复尝试被拒绝的调用，正是需要打断的循环。
- **忽略没有 agent 的调用。** 直接调用 `ToolRuntime::execute()` 的调用方没有需要提醒的模型，也没有可作为键的活跃 agent 对象。
- **按 agent 分键。** agent 和 subagent 的工具调用可能交错通过同一 waterfall，因此链对每个精确的活跃 `Agent` 使用弱所有权。一个 agent 的重复调用不会触发另一个 agent 的提醒。`agent/pre-step` 观察到直接用户消息时，只重置提交该消息的 agent 链。
- **仅驻留内存。** 从持久化恢复的会话会从全新链开始。guard 是启发式提醒，并非有日志记录的不变量；提醒会延后，这是可接受的代价。

## 提醒传递

提醒通过 post-execute 决策中的 `additional_contexts` 传递，来源为 `{kind: "plugin", plugin: "repeat-tool-reminder", form: "notice"}`，绝不替换结果内容。用于审计的持久化 `tool/result` 仍保留工具自己的输出。循环会缓冲这段上下文，并在该步骤的工具结果之后将其作为注入的 `user/message` 追加，因此提醒对模型可见、带有来源归属，并且无需新增事件类型即可从会话日志重建。guard 始终继续调用下游，并将自己的提醒放在所有下游决策变体的上下文数组之前，包括被阻止的调用；下游各条上下文的来源和元数据都会保留。

## 模型体验

### 首个阈值的上下文消息

达到第一个配置的连续重复阈值时，对应 agent 会收到以下提醒。系统不会添加工具 schema 或普通调用文本。

```markdown
You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.
```

达到阈值前 token 影响为零。提醒会作为该 agent 的历史记录保留。内容仅追加在可复用请求前缀之后，因此不会使现有 KV Cache 条目失效。

### 后续阈值的上下文消息

达到后续阈值时，agent 会收到以下详细模板。受上限约束的参数预览严格以 `… (+<omitted> more chars)` 结尾。

```markdown
Repeated tool call detected:
- tool: <toolName>
- consecutive_calls: <count>
- arguments: <canonicalArguments>
The repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered.
```

每条提醒都会作为历史记录保留；`argumentsPreviewChars` 限制随数据变化的参数文本长度，而各 agent 使用独立计数器。内容仅追加，不会使已建立的请求前缀失效。

## 已知限制与暂缓事项

- **仅检测精确匹配**：近似变体（例如修改路径或值内空白）可以绕过链；在没有需求证据前，暂不采用模糊匹配。
- **压缩不会重置链**：跨越 compaction 检查点的链会继续计数。
- **仅提供建议**：尚未实现达到较高阈值后升级为阻止策略，但 `PostToolDecision` 已支持该能力。
- **subagent 之间不共享链**：父 agent 与 subagent 重复相同调用时不会合并计数。
- **合理的幂等轮询超过阈值后仍会收到提醒**：可通过 `thresholds` 与 `exclude` 配置调节。
- **超过最高阈值后链不再提醒**：提醒只在精确达到所配置的次数时触发。
