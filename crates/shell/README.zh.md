# seekdeep-shell

[English](README.md) | 中文

`ShellExecutor` 定义 shell 后端做什么——运行前台命令并启动后台进程——但不选择具体实现方式。Job ID、所有权、收集、取消通知和面向模型的 schema 属于此无任务语义能力 seam 之外的消费方与提供方。

Rust 移植保留源包的四种职责拆分：

| Crate | 职责 |
|---|---|
| `seekdeep-shell` | 服务定义、执行器 trait、进程句柄与共享词汇 |
| 本地／沙箱 shell 提供方 | 具体子进程执行与可选限制 |
| 面向模型的 shell 工具 | 基于 `shell` 服务的 schema 与呈现 |
| jobs 运行时 | 长期任务身份、所有权、轮询与通知 |

一个 context 中只能有一个提供方占用强类型 `SHELL` 服务席位。`ShellService::provide` 会用与源兼容的服务重复诊断快速失败，并返回可逆的生命周期 effect。`shell_settings_namespace()` 返回由能力而非任一提供方拥有的共享 `shell` 设置命名空间。

## 服务 API

| 成员 | 语义 |
|---|---|
| `resolve(request)` | 应用提供方默认值与上限，返回完整的 `ShellExecSpec`；提供方校验失败会成为显式错误。 |
| `run(spec)` | 前台执行。基础设施失败会 reject；非零退出、超时终止和调用方中止终止会 resolve 为 `ShellRunResult`。 |
| `start(spec)` | 启动后台工作并立即返回无任务语义的 `ShellProcess` 句柄。后台执行不应用执行器超时。 |
| `sandbox_mode()` | 面向消费方的能力事实；默认返回 `None`，表示执行器不使用沙箱。 |
| `ShellProcess::read_output()` | 消费式增量读取。连续读取不会重复交付字节；有损读取会在可用时暴露 spill 路径。 |
| `ShellProcess::kill()` | 终止进程组；进程结算后返回 `false`。 |
| `ShellProcess::done()` | 等待进程关闭且不 reject；spawn 失败表示为 killed 句柄与捕获的 stderr。 |

提供方 teardown 负责其创建的所有实时进程：必须停止每个仍在运行的进程并等待其关闭。如果外围 subprocess 服务仍然存活，仅重新加载执行器时可让 subprocess 所有的句柄继续运行，这与源生命周期边界一致。

## 词汇

`ShellExecRequest` 包含命令以及可选工作目录、超时、stdout 预算、中止信号、一次性 stdin、普通环境、受信任 `SEEKDEEP_*` 环境与沙箱策略。`resolve` 生成工作目录、超时与 stdout 预算均已确定的 `ShellExecSpec`。受管环境 key 使用经过校验的 newtype，沙箱策略中的会话身份继续使用 `SessionId` newtype。

`ShellRunResult` 携带退出码或可扩展信号 newtype、超时／中止首因标志、有效超时、捕获的 stdout／stderr 与可选沙箱事实。`ShellSandboxInfo` 独立于命令退出状态报告实际模式、拒绝、强制完整性与 runner 失败。`CollectedOutput` 包含保留的文本尾部、截断标志和可选完整流 spill 路径。

导出的 `parse_exit_status` 是 shell 工具渲染器最终 `[exit code: N]` 与 `[killed by signal: X]` marker 的共享逆解析。它只消费位于字符串结尾且带前导换行的 marker。超时与策略 marker 会留在正文中，因为 terminal 呈现没有对应的独立 pill。

## 模型体验

本 crate 只会通过 shell 工具等具名消费方间接影响模型输入；消费方负责 schema、渲染指引与保留的工具结果 token。seam 自身不会改变请求前缀或 KV Cache 复用。

## 已知限制与暂缓事项

- **没有交互式输入词汇**：stdin 只在 spawn 时写入一次并关闭；没有后续输入通道，也没有 PTY 会话概念。
- **前台 deadline 由执行器负责**：请求携带超时与调用方取消；此 seam 没有独立的调用方 deadline 模式。
- **提供方负责 OS 细节**：进程树终止、环境清理、spill 机制与限制强制是提供方契约，不在服务定义 crate 中重复实现。
