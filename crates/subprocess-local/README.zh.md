# seekdeep-subprocess-local

[English](README.md) | 中文

[`seekdeep-subprocess`](../subprocess/README.md) 能力 seam 的原生本地
provider。`LocalSubprocessRuntime` 解析本地可执行文件，以显式 stdio
spawn 隔离的普通进程树，并通过原生 PTY 与平台进程检查实现终端进程。
它没有独立配置：每项处置方式、限制、终端尺寸、宽限期、环境与目录都
来自调用方的能力 spec。

## 行为

- **适合平台的进程树。** POSIX 子进程拥有独立进程组，信号以负 pgid
  发送，并以直接子进程作为回退；Windows 使用
  `taskkill /PID <pid> /T /F`。`terminate()` 先发送 TERM，在 spec 的宽限期
  后升级为 KILL；它是幂等的，并在确认整棵树消失后停止发送信号。
  `wait_for_exit()` 观察完整的受管树，而不只观察 leader。leader 退出后，
  同一宽限期也会限制收集管道的排空时间，使继承描述符的后代无法无限期
  拖住 `done()`。
- **与源实现一致的 stdio 形状。** 在 POSIX 上，已连接的子进程描述符使用
  Unix socket pair，与 Node 子进程的 fd 类型一致；忽略的 stdin 是 null
  字符设备。`Pipe` 暴露原始 Tokio 流，`Inherit` 直通父进程描述符，
  `Collect` 保留精确的内存尾部。配置 spill 上限后，完整流会追加到按需创建
  的每进程 `0700` 目录中随机命名的 `0600` 文件。超过 spill 上限会使不完整
  spill 失效并尝试删除。关闭或清理故障会被吸收；最终关闭失败时不会公布
  不可靠文件的路径。
- **凭据清除与显式合并。** 环境中的凭据形名称（`KEY`、`PASSWORD`、
  `SECRET` 或 `TOKEN`）以及所有已有的 `SEEKDEEP_*` 名称都会被移除。
  spec 的显式值随后合并，因此有意提供的凭据和当前受管事实会胜出；显式
  `None` 是 tombstone。批量 stdin 会尽力写入并关闭；如果子进程不读取就退出，
  其退出结果仍是权威结果。
- **基于偏移量的读取。** 收集读取器使用完整流的字节坐标，不持有共享游标。
  多个独立增量读取器与完整重读可在结算前后共存。偏移量已经滑出保留尾部时，
  读取会报告 `lossy`；只有完整 spill 仍可信时才包含其路径。
- **可执行文件查找。** 绝对路径必须指向可执行普通文件。裸名称在清理后的
  有效 PATH 中搜索；相对 PATH 项从宿主 cwd 解析，Windows 上以不区分大小写
  的方式处理 `PATH`／`PATHEXT`。含路径分隔符的相对命令路径会在 seam 处失败。
- **终端会话所有权。** `spawn_terminal()` 分配真实 PTY、桥接 UTF-8 终端文本、
  检查并向前台进程组发送信号，同时公开一个可合并等待、失败后可重试的终止
  操作。清理捕获精确的 pid/start 身份，在停止 shell 前后清扫根树后代与可观察
  POSIX session 成员，而且绝不会收养 PID 已复用的 root 的子进程。自动清理失败
  的终端会留在 runtime 存活集合中，供之后的正常 dispose 或宿主退出强制清理。
- **先终止再等待退出的 dispose。** provider 会一直保留普通句柄与终端句柄，
  直到确认真正完全停稳。dispose 先启动所有终止操作，再等待每个目标；出现失败
  时强制停止剩余目标，完成这些尝试后才清空所有权。单个失败保持原样，多个失败
  使用稳定聚合消息 `local subprocess teardown failed`。
- **同步宿主退出最终清理。** service effect 有效期间，一个狭窄的原生 `atexit`
  bridge 会强制终止所有仍受管的普通进程树和可观察终端会话，不创建 timer 或异步
  工作。每个目标的故障都会单独吸收，使后续目标仍能执行；宿主退出状态不变，正常
  dispose 会可逆地移除 runtime 注册。

## 模型体验

provider 没有直接面向模型的 surface。shell、LSP 与 terminal executor 等消费方
负责渲染、生命周期分类和请求前缀变更。因此本 crate 不会直接使 KV cache 失效。

## 已知限制

- Windows 进程树在包含 `taskkill /T /F` 结果后，以直接子进程作为存活边界；
  terminal 进程检查只支持 Linux 与 macOS。Linux 精确 syscall 探针覆盖 x86-64
  与 AArch64。
- 终端后代如果在捕获前变得不可观察，仍可能逃逸：macOS 上是在根树快照之前
  reparent，Linux 上则是通过 `setsid` 同时离开根树和自有 session。
- 进程内最终清理要求退出路径能执行原生 `atexit` callback，例如正常 Rust 终止
  或 `std::process::exit`。`SIGKILL`、abort、fatal runtime 故障、native crash、
  断电，以及任何无法执行 callback 的路径都需要外部 supervisor 或等价 OS owner。
- 凭据清除依赖名称启发式规则。名称不同的 secret（例如 `PASSPHRASE`）会继续
  存在于环境中，除非调用方显式移除。
- provider 不会删除已经完成的有界 spill 文件及其私有目录。超大的不完整 spill
  会尽力删除；清理失败可能留下一个有界文件。

普通进程实现在 `src/spawn.rs`；`src/lib.rs` 负责 service 接线，
`src/terminal.rs` 负责 PTY session，`src/process_inspector.rs` 负责平台检查。
