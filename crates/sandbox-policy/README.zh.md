# seekdeep-sandbox-policy：沙箱策略归属位置（`sandboxPolicy`）

[English](README.md) | 中文

沙箱策略解析的唯一归属位置：部署默认 `SandboxMode` 与回退根目录，加上每个会话的持久模式覆盖和不可变工作区根目录。每项强制执行能力在每次调用时都会收到解析完成的策略；模型在请求前收到当前策略，而不会另收能力清单。

## 为何需要共享归属位置

文件系统工具、一次性 Bash 命令和终端会话可用不同组合强制执行同一词汇。如果各自解析 `mode` 与 `workspace_root`，就可能漂移成分裂世界。每个后端消费完整策略；当前上下文只说明该策略对于任何受 SeekDeep 文件沙箱强制执行的可用操作有何含义。

## 配置

- `mode`：部署默认模式，加载时验证；默认为 `read-only`。
- `workspaceRoot`：无 agent 调用或没有 cwd 的会话使用的回退目录；默认为进程 cwd，并解析为绝对文件系统身份。普通 agent 调用使用会话头中不可变的 `cwd`。

## 接口

- `SandboxPolicyService::resolve` 按显式批准模式、最后一条 `sandbox/mode`、部署默认值的顺序解析策略。会话 cwd 先按文件系统语义规范化，再成为 `workspace_root`，所以 `symlink/..` 与真实工作目录一致。
- `default_mode` 与 `workspace_root` 是部署回退值。
- `sandbox:policy` 是直接派生自解析结果的请求时缓存安全上下文；它不枚举能力。
- `effective_sandbox_mode(events)` 纯粹折叠会话事件，最后一次切换胜出。
- `set_sandbox_mode(session, mode)` 是唯一写入路径，恰好追加一条事件。
- `SANDBOX_MODES` 列出全部封闭模式。

Invariant 配套组件在回放时以及实时提交前拒绝词汇之外的模式。agent loop 把组装后的上下文记录为带来源的用户消息，因此无需“上次告知”内存镜像也能重建策略。

## 逐会话存储

`effective = explicit grant ?? fold(events) ?? deployment default`，所以覆盖会通过回放跨重启保留，两个会话不会看到彼此状态。不可变 `SessionHeader.cwd` 已提供工作区身份。

## 模型体验

```markdown
Current SeekDeep file policy: read-only. Any available operation enforced by the SeekDeep file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns.
```

```markdown
Current SeekDeep file policy: workspace-write. Any available operation enforced by the SeekDeep file sandbox may modify files under the session workspace: "<workspace root>". Some platform temporary areas may also be writable.
```

```markdown
Current SeekDeep file policy: danger-full-access. The SeekDeep file sandbox does not restrict file modifications by available operations.
```

首次请求和策略变化会增加一条持久上下文消息；未变化的请求不增加内容。系统提示词逐字节稳定，新快照追加在历史之后，保留已有 KV Cache 前缀。

## 已知限制与暂缓事项

- 每个会话只有一个主要工作区根目录。
- 模式只管控文件操作；网络和进程策略不在此词汇中。
- 平台临时区域由后端在策略解析后选择，因此上下文只做概述。
