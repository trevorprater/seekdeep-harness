# seekdeep-subprocess

[English](README.md) | 中文

这个与提供方无关的子进程 seam 是单一执行世界的进程部分。
`SubprocessRuntime` 公开可执行文件查找、立即启动普通进程和一个终端进程原语。
其 Rust 词汇覆盖原始与收集式 stdio、进程与终端句柄、退出事实、进程树／会话清理
以及受管 `SEEKDEEP_*` 命名空间。本地原生提供方属于后续独立移植切片。

## 服务约定

`SubprocessService` 在强类型 `SUBPROCESS` 槽位上发布唯一 runtime。重复提供方以
`service "subprocess" has been registered` 失败；释放所属上下文会撤销该精确注册。
提供方必须实现以下生命周期规则：

- `spawn(spec)` 立即返回活动句柄。`done()` 在直接进程关闭时只以
  `SubprocessOutcome` 退出事实结算；收集输出和调用方自有的原因分类相互独立。
  spawn 层面失败由返回句柄上拒绝的 `done()` 表示。
- 工作目录与可执行文件路径属于提供方的执行世界。`resolve_executable` 验证绝对
  路径，或使用该世界清理后的 PATH 加显式查找覆盖来解析裸名称。
- spawn spec 完全显式：argv、cwd、每个 stdio 处置方式、宽限期、取消信号和可选
  环境都由调用方给出。任何内容都不会经过 shell 解释；需要 shell 的消费方自行
  提供 `bash -c` 或平台等价形式。
- `Pipe` 公开原始异步流，`Inherit` 直通父描述符，`Collect` 保留有界尾部并可选
  完整流 spill 恢复。`read_from` 使用全流字节偏移且不消费共享状态，因此独立
  读取器不会抢走增量。偏移滑出保留窗口时会报告 lossy，并在完整 spill 仍有效时
  保留其路径。进程结算后读取器仍然有效。
- `terminate()` 是普通进程唯一的终止动词。提供方必须令它幂等、以整棵进程树为
  范围，并在配置宽限期后从 TERM 升级到 KILL。spec 信号触发同一动作。
  `wait_for_exit` 观察整棵树的存活状态，让消费方约束等待时间，但本 seam 不给原因
  分类。
- 提供方服务生命周期结束时会终止仍在运行的所有受管进程树并等待完全停稳。

## 终端原语

`spawn_terminal` 拥有真实终端分配，并返回支持 UTF-8 输出、精确写入、前台进程组
检查、闭合集合信号（`SIGINT`、`SIGTERM`、`SIGKILL`、`SIGTSTP`、`SIGHUP`）以及
须等待的幂等会话终止操作的句柄。分配取消信号只在句柄发布前有效；发布后句柄
拥有会话生命周期。PTY 就绪、scrollback 与持久 shell 策略仍归消费方所有。

## 环境策略

`scrubbed_parent_env()` 是所有提供方共享的环境基础。它以不区分大小写方式删除
名称中含 `KEY`、`PASSWORD`、`SECRET` 或 `TOKEN` 的键，以及所有 `SEEKDEEP_*`
键。`PATH`、`HOME`、locale 和代理配置等普通执行事实会保留。调用方的显式环境
在清理后合并；`None` 值表示删除环境键，显式凭据或当前受管值则是有意选择。

`seekdeep-shell` 重新导出本 crate 的 `CollectedOutput`、`SeekDeepEnvironment`、
键 newtype、信号 newtype 和受管前缀，使子进程 seam 保持这些类型的唯一所有者。

## 模型与缓存影响

本 seam 不直接生成模型可见内容，也不影响 KV 缓存。shell 执行器等消费方拥有
渲染、生命周期描述和请求前缀。

## 限制

- SDK 自己管理内部 spawn 的传输无法把该进程路由到本服务，但仍可共享清理函数。
- seam 提供信号与整树等待，而不提供统一拆卸阶梯；各协议消费方拥有自己的协作序列。
- 平台进程树与终端会话的可观察范围归具体提供方所有，必须在那里记录并测试。
