# seekdeep-command-feedback

[English](README.md) | 中文

与模型无关的 `/feedback` 命令，用于记录人类对会话的意见。

- 记录一条仅进入日志的 append-only `feedback/record` 会话事件，其中携带 trim 后的自由文本意见。该事件具有权威性；它不会进入模型上下文、有序 surface 或派生 history，也不携带 `surfaceOp`。
- 确认信息包含接收会话 id 与匿名用户 id。在 `session-telemetry` crate 完成移植之前，会话共享 disclosure 当前报告“not configured”。
- 空输入或仅含空白的输入会作为失败的命令记录被拒绝，不记录事件，也不执行用户 id 查询。

## 模型体验

token 与 KV-cache 影响均为零：无论是已接受的 feedback 还是用法错误，都不会触及模型请求路径。

## 限制

没有检索／管理 surface、结构化字段、修改或撤回，也没有显式 durability barrier；确认发生在 append 之后，而不是 flush 之后。
