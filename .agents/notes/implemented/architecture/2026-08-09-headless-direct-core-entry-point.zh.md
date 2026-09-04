# Agent Note: headless 是直接使用核心服务的入口

Status: implemented

[English](2026-08-09-headless-direct-core-entry-point.md) | 中文

## 问题

`headless` 的产品约定是一个本地任务：最终 assistant 文本写入 stdout，退出状态反映成功与否，成功时 stderr 为空，并且不打开监听端口。包含 Workspace Host 服务、ApiProxy、HTTP、Web 运行时或浏览器插件的组合违背这一约定，也使本地完成状态依赖无关的传输树。

直接入口仍需要与 Web 所创建 Agent 相同的部署模型状态。独立的提供方／模型默认值会让同一部署产生两种答案，而在 Agent 与会话持久化完全停稳之前推导完成状态，会让 stdout 与退出状态观察到不完整状态。

## 决策

随附的 `headless` profile 包含 `seekdeep-base` 与 `seekdeep-headless`。headless 组合包提供自身的 persona 与工具模式、禁用 HMR（热模块替换）、显式挂载 Code Mode worker，并插入 `headless-runner`。其插件树不包含任何 `@seekdeep-ai/seekdeep-host-*` 包、ApiProxy、HTTP server、Web 运行时或浏览器客户端。Code Mode 与会话持久化均为独立于 Web 呈现的一次性 Agent 能力。

每次 CLI（命令行界面）profile 调用都使用按顺序排列的组合包、profile、主目录及显式叠加层。未指定 `--patch` 的 `headless` 调用也使用同一 Loader 路径，包括已编译的启动提供方与运行器注册。最小化的类型化 `HeadlessApplication` API 可供显式编程调用方及测试使用，但不属于 CLI 分派路径。

`headless-runner` 是直接使用核心服务的入口。Loader 完全加载后，它读取 `ctx.agentDefaultModel.currentSelection()`，通过 `ctx.agents.create` 创建一个新的持久化 Agent，在 Agent 作用域中安装该 `ModelSelection`，等待启动工作完全停稳，锚定会话事件序号，提交一条普通用户消息，再次等待完全停稳。随后，它等待 `ctx.sessions.flush`，折叠自身持有的持久事件区间，以取得最后一条非空 assistant 文本和最终 `turn/end` 结束原因，将文本连同一个换行写入 stdout，并且仅在结束原因为 `completed` 时请求启动器以退出状态 0 有界关闭。结束原因为 `error` 时，其持久化错误码与消息写入 stderr；驱动器的意外失败也写入 stderr 并以 1 退出。

插件的优雅 dispose（资源释放）以 `disposed` 原因取消其拥有的 Agent，并等待运行器完成完全停稳、flush 与最终输出。启动器由首个信号确定的退出状态仍具有权威性：SIGINT 以 130 退出，SIGTERM 以 0 退出。启动获准前的取消会释放运行器而不启动任务。

`@seekdeep-ai/seekdeep-agent-default-model` 拥有与传输无关的默认值，供没有会话级选择的 Agent 使用。`AgentDefaultModelConfig` 提供 `ctx.agentDefaultModel` 并注册 `agent-default-model` Settings 分节。组合配置提供 `{provider, model}`，用户设置还可以提供 `reasoningEffort`。`currentSelection()` 返回当前的完整选择，`saveSelection()` 则写入完整分节，因此不含强度的选择会清除已存强度。`seekdeep-base` 提供组合条目。直接入口与 ApiProxy 入口均消费该服务；只有 ApiProxy 负责会话级优先级、模型校验与已接受 Web 选择的持久化。

`loadProfile` 识别安装过程拥有的精确 headless 元组（`seekdeep-base`、`seekdeep-web-app`、`seekdeep-headless`），将其规范化为随附的 headless 模板，并保留 manifest（元数据清单）的其他所有字段。带额外项、缺少项或顺序不同的组合包列表归用户所有，保持不变。

本 Agent Note 负责 headless 的传输与完成约定。[应用持有自己的命令行](2026-08-06-app-owned-command-line.md)负责当前的 `seekdeep --profile headless` 语法；原 [`seekdeep run` 决策](../../archived/feature/2026-08-08-seekdeep-run-headless-command.md)记录已被取代的启动器持有语法，[GUI 分层与 RPC 协议](2026-07-19-gui-layering-and-rpc-protocol.md)负责浏览器网关边界，[Web 配置树启动与传输分层](2026-07-24-web-config-tree-boot-and-transport-layering.md)负责 Web 插件树，[默认模型跟随选择器](../feature/2026-08-07-default-model-follows-the-picker.md)负责共享 Agent 默认值的持久化。

## 验证

包测试围绕脚本化 Agent 工厂使用真实的会话存储与 Agent 注册表，固定空闲态到空闲态的聚合、延迟异步完成、终止态模型诊断、其他未完成退出、直接失败、Loader 加载期间的 dispose，以及退出前 flush 的顺序。Source 等价的 coding harness 会在确定性模型脚本周围加入真实本地 subprocess、Bash executor、shell environment、Bash tool、todo tool 与 JSONL 持久化。它会证明一次 Bash 往返、修复一个失败的 Node 程序并独立重跑其未改动的测试，以及记录精确的并行 todo 计划。Cold-resume 测试随后会关闭第一个 context，在第二个 context 中打开同一 JSONL 根目录，通过 AgentLoop 恢复精确 Session，并要求下一次模型请求同时包含此前两条 fact message。Semantic-checkpoint 覆盖会预置一个结果未匹配的副作用 tool call，在继续执行前验证持久化的 `TOOL_OUTCOME_UNKNOWN` 修复与安全指引，并交付 source 中的安全回答。真实 loop compaction 测试会为早期 surface node 加上 bracket 并将其 shadow，以 checkpoint 替换旧历史，同时仍产出最终 assistant message 与 completed turn。组装后的无密钥快照通过回放的工具往返驱动 `seekdeep --profile headless`，记录一条带 `source.kind: 'user'` 的 `user/message`，并在 stderr 暴露终止态模型失败。构建后二进制验收通过已发布入口访问 mock 提供方，并要求最终文本出现在 stdout、退出状态为 0 且 stderr 为空。配置转储验收排除随附 headless 树中的所有 Host、Web 与 Client 包；PTY 关闭覆盖要求不出现观察行，并在有界时间内完成 dispose。

Rust 无密钥 Loader 冒烟测试挂载真实服务插件、执行 Bash、流式输出其持有的 Session 区间、记录模型默认及下一步骤的推理强度、聚合 source 中精确的 token 用量，并打开已 flush 的 Zstandard 头。Fixture 测试保留首次 pre-step 中仅在缺失时创建 goal 的种子及其唯一持久化变更、经 AppBoot 传递的原始激活消息与确定性堆栈，以及带 listener teardown 的根父级 settlement barrier。该 barrier 忽略子级与无关 inbox 事件，并在 manager notice 到达后保持开放。

[Rust 源快照回放](../../../../crates/headless/tests/replay_snapshot_parity.rs)通过已编译的 Loader 目录比较完整的规范化事件流及冷打开的 JSONL 历史。它覆盖提供方重试、溢出压缩、严格的目标缺失错误、全新的 Ralph 子级、持久 PTY 输出，以及无需父级轮询的可继续子级结果交付。凭据场景使用真实的 DeepSeek 适配器，保留不泄露密钥的诊断及适配器默认值，并要求在 HTTP I/O 前失败。回环流验证 keep-alive 注释及生效的请求默认值。静态回放主目录禁用设置、凭据与 skill（技能）文件监听；文件可变行为仍由这些提供方的原生测试套件负责。

[原生进程探针](../../../../apps/seekdeep/tests/headless_process.rs)通过未指定 `--patch` 的 CLI profile 执行 Bash 与 `todo_write`，保留终止态模型错误输出，并在没有命令行叠加层时应用用户 profile 补丁。回环提供方将辅助标题请求与任务的工具往返分别处理。冷日志检查及保持不变的用户补丁字节固定持久性与配置所有权。

[产品 profile 快照](../../../../apps/seekdeep/tests/headless_profile_snapshot.rs)使用受控适配器、捕获的输出与 fixture 自有主目录挂载完整的随附层，再将 stdout、stderr 及整个冷打开的日志与源实现比较。开发回退测试隔离可执行文件发现，并要求子会话写入器在工作流 worker 完成 teardown 后仍能存活。

## 考虑过的替代方案

| 替代方案 | 约定不匹配之处 |
|---|---|
| 保留 `seekdeep-web-app`，但隐藏观察行 | 进程仍会打开端口并携带 Host、Web 与浏览器插件树。 |
| 围绕 ApiProxy 构建纯 Host 一次性组合包 | ApiProxy 是客户端协议网关，而本地一次性入口没有客户端边界。 |
| 使用 `InProcessApiClient` 实现产品级协议覆盖 | 产品执行会仅为测试无关协议而依赖该协议。 |
| 为 headless 单独提供提供方／模型配置 | 直接创建与 Web 创建会拥有彼此独立的默认值和持久化。 |
| 将不带 `--patch` 的 headless 调用路由到较小的硬编码组合 | 除非提供命令行叠加层，否则同一 profile 命令会缺少工具，并忽略 profile／主目录补丁。 |
| 在插件 dispose 时中止运行器任务 | 被中断的 Agent 即使能够协作停止，也可能丢失最终输出与持久性检查点。 |
| 省略 Code Mode 与会话持久化 | 两项能力都属于一次性 Agent 执行，而不是 Web 呈现。 |
| 规范化所有包含 Web 与 headless 组合包的元组 | 组合包列表是扩展面；只有精确的安装过程所属元组可以安全分类。 |

## 后果

`seekdeep --profile headless` 提供本地 Agent 任务，而不是浏览器观察、Host API 或 HTTP。需要这些能力的用户选择 `seekdeep web`。成功时 stderr 为空，完成结果在持久化 flush 后推导，持久化会话仍可供后续工具使用。初始用户消息记录 `source.kind: 'user'`，因此不携带 ApiProxy `rpcId`。

ApiProxy 载体覆盖保留在 ApiProxy 包中。自定义一次性 profile 可以显式包含 Host 或 Web 组合包；随附 profile 与可识别的安装过程所属元组均不含 Web。
