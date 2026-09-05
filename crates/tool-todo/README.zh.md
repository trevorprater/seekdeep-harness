# seekdeep-tool-todo

[English](README.md) | 中文

面向模型的 whole-list `todo_write` 工具与 `todos` 会话投影。

- 每次调用都会向调用 agent 的会话追加一个 `todo/write` 快照；回放采用 last-write-wins。条目形状为 `{ content, status }`，状态包括 `pending`、`in_progress`、`completed`。非 agent 调用方会被拒绝。
- `allowParallelInProgress` 决定是否可同时有多个 todo 处于 `in_progress`；为 false 时，标记多个活跃项的调用会被拒绝。
- `todos` 投影折叠最新的完整列表，在每次 `turn/start` 时清为 null（standing plan），并在 `turn/end` 时保留已完成 checklist；`stateVersion` 为 2。

## 渲染

规范结果为 `{ todos, counts: { pending, inProgress, completed } }`；Native renderer 返回精简的更新确认。

## 模型体验

schema 成本固定；append-only 且 prefix-stable。稳定失败会指出空／重复 content、活跃项策略，或缺少所属 agent 会话。

## 限制

仅支持单一 owner scope；条目形状刻意保持最小；whole-list replacement 是唯一操作。
