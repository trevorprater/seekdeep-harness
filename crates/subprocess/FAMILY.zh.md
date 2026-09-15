# subprocess：子进程能力家族

[English](FAMILY.md) | 中文

subprocess 家族集中提供一个执行世界的共享进程基底：可执行文件查找、具有原始
或收集式 stdio 的完全明确指定的受管子进程树，以及一项负责 PTY 分配、前台
进程组和 provider 可观察 session 清理的底层终端原语。命令默认值、shell 语义、
时限、协议分帧、就绪状态与呈现仍归 shell、LSP、terminal 与 subagent 等消费方。

| Crate | Context 键 | 角色 |
|---|---|---|
| [`seekdeep-subprocess`](README.zh.md) | `subprocess` | Provider-neutral Service Definition：可执行文件查找、普通 spawn、终端分配、句柄生命周期，以及共享环境／输出词汇 |
| [`seekdeep-subprocess-local`](../subprocess-local/README.zh.md) | 无 | 原生本地 provider：隔离进程树、有界收集／spill、原生 PTY、前台／session 检查、进程树信号发送，以及先终止再等待退出的 dispose |

即使消费方重载，进程生命周期仍由 service 负责。消费方负责定义进程的含义，
例如 shell 命令或协议 server，并拥有塑造该进程的所有默认值。具体 spawn spec、
输出读取器、结果和受管 `SEEKDEEP_*` 环境契约见上面链接的 service 与 provider
参考文档。
