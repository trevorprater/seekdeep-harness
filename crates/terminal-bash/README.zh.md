# seekdeep-terminal-bash

[English](README.md) | 中文

这是一个基于 `SubprocessRuntime::spawn_terminal`、为 `seekdeep-terminal` 提供的持久 shell 后端。它在共享 `seekdeep-sandbox-policy` 下启动交互式 shell，保留有界逐行输出并检测就绪；进程提供方负责 PTY 分配、环境清理、前台进程组、信号发送和完整会话清理。因此，同一个后端可与本地或远程执行世界提供方组合。

## 插件（`terminal-bash`）

插件注入 `terminals`、`sandboxPolicy` 和 `subprocess`，然后注册配置的后端类型。`danger-full-access` 无需沙箱便可直接启动；受限模式要求同一世界存在 `sandbox` 服务，并通过它包装确切 shell argv，未挂载时在 spawn 前失败。一次策略解析同时给出实际模式与工作区根目录。确切所有者存在开放 PTY 或正在 spawn 时，系统在事件提交前拒绝改变实际模式；该限制在保留会话的提供方重载后仍有效。

就绪检测结合前台验证的私有 Bash 提示符标记、stdin 等待事实、静默回退与绝对超时。只有最新自有标记后的可打印尾部与受控 `PS1` 完全相等才算就绪，即使跨回调拆分也一样。写入前收集的提示符与静默证据会在写入边界丢弃。Bash 在前台交接发布前打印标记时，轮询会将候选保留到 `handoffGraceMs`。未知前台状态不是精确空闲证据；send 前已有的 stdin 等待也不是写入后就绪。启动回退要求已观察到输出，零输出不能发布空会话。

取消会关闭尚未发布的 shell，并保留调用方确切中止原因；`TerminalBackendCleanupError` 单独保留清理失败。初始化先同步建立启动 reservation，再让外层中止竞态可分离，从而保持第一条提示符的 MOTD 归属。控制序列受 `maxReadBytes` 限制；错误 UTF-8 使用替换字符，末尾回车跨回调保留。

取消 send 会先标记排队输入已取消，再向当前前台进程组发送真正的 `SIGINT`。在途写入完成前不发送信号；写入拒绝时不发信号。取消的 send 保留独占位置直到写入和信号结算，后继 send 不会收到延迟字节或信号。永不结算时恢复手段是关闭会话。取消绝不通过写入 `\x03` 模拟中断，因此 raw 模式程序仍可取消。关闭会等待完整会话终止后，把活跃 send 结算为 `session_exit`。

## 模型体验

策略归属方贡献 `sandbox:policy`。模型通过终端工具可能收到有界 MOTD、send 增量、scrollback 页、就绪原因与清理错误。消费方返回有界输出前，scrollback 不进入模型历史；策略变化和消费方结果都保持仅追加。

## 已知限制与暂缓事项

- 输出按行规范化；不支持全屏备用缓冲区。
- 精确 stdin 等待检测取决于进程提供方；无法证明时使用提示符和静默／超时。
- 清理保证以 `SubprocessTerminalHandle` 为准。
- SeekDeep Harness 进程退出后，会话无法继续存在。
