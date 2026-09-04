# Agent Note: Rust 打包 SDK 运行时组合

Status: implemented

[English](2026-09-04-rust-packaged-sdk-runtime-assembly.md) | 中文

## 问题

原生 JSON-RPC 入口即使能够编译，也可能只提供 Python 运行时 manifest（元数据清单）声明的一小部分插件。可执行文件被复制到构建目录之外后，还可能找不到单独发现的工作流辅助程序，或导入周围无关项目中的包。即使各个底层 Rust 插件的测试通过，这些问题仍会破坏外部配置与单文件交付。

## 决策

[编译后的运行时目录](../../../../crates/jsonrpc-demo/src/runtime_catalog.rs)将具体 Rust 插件工厂与既有包名配对，并通过内嵌的[运行时 manifest](../../../../python/sdk-runtime/package.json)筛选注册项。注册只提供工厂；只有外部 Cordis 配置才会挂载服务。打包目录排除开发用回放适配器，并禁用未注册裸插件名的文件系统回退。显式的相对路径、绝对路径和 file-URL 兼容模块保留 Loader 既有的解析行为。

这是对[单文件分发记录](2026-07-10-single-file-executable-sdk-runtime-distribution.md)中封闭插件集合与外部配置要求，在 Rust 生产代码和[原生／进程部署规则](../../../../porting/DYNAMIC_PLUGIN_RELOAD.md)下的原生实现。它不替换源码中配置、Host、浏览器和模型定义包各自独立的重载策略。

[JSON-RPC launcher](../../../../crates/jsonrpc-demo/src/runner.rs)把自身可执行文件的绝对路径传给工作流插件。每个工作流都以清理后的环境启动一个可被终止的子进程，通过 `SEEKDEEP_INTERNAL_WORKFLOW_WORKER=1` 让同一二进制在应用启动前处理既有工作线程协议。因此，移动或改名后的可执行文件既不需要相邻的工作流辅助程序，也不需要通过 PATH 查找。代码执行使用既有的 Rust 自主管理 V8 后端。对外服务插件仍会等待 Loader 完整启动后再分发 SDK 请求。

插件依赖保留源码含义。skill（技能）工具要求 `agents`、`tools` 和 `skills`。审批本身不以系统提示词服务为前提：策略上下文内容会跟随确切的可选提示词提供方处理延迟出现、替换与撤销，而不替换审批服务。可重入的服务通知会被合并，提示词注册与 dispose（资源释放）在绑定锁之外执行。策略决定、失败时拒绝的应答处理以及持久审计事件仍由[审批服务](../feature/2026-07-06-approval-seam.md)负责。

[原生可执行文件构建器](../../../../crates/python-release/src/executable/mod.rs)保留源码的目标与选项语法，串行编译各个请求的平台，并暂存可执行产物和开发载体。既有的 `node<major>` 目标字段仍被接受并输出；决定实现的是固定版本的 Rust 工具链，而非该字段。`--skip-build` 使用已有的 Cargo release 产物。在暂存宿主平台可执行文件之前，构建器会检查其原生格式、架构、可执行权限、仓库版本和内嵌运行时 manifest。输出的父路径必须位于仓库内且不得经过符号链接；替换操作仅限于生成的 Node 载体目录。

开发载体保留 Node 入口路径，但只包含启动绑定和宿主平台原生产物。绑定通过 `process.execve` 将 Node 替换为 Rust 进程，保留参数、环境和标准流。原生 launcher 在读取 SDK 协议前清除继承标准流上的 `O_NONBLOCK`；否则，从文件加载的 Node 入口可能将非阻塞描述符交给 Rust 的阻塞 I/O。[Rust PTY 辅助程序](../../../../crates/pty-spawn-helper/src/main.rs)打开继承的终端，应用请求的工作目录，再以请求的程序替换自身。其 macOS 伴随文件保留源码 node-pty 调用方的控制终端契约，而无需交付 C 实现。

GitHub 和 GitLab 的运行时构建作业在按架构选择、按摘要固定的 manylinux 2.28 容器内编译 Linux 可执行文件，并使用相同镜像验证已安装的 wheel 包。容器编译与宿主平台 wheel 工具使用独立的 Cargo 目标目录。macOS 编译默认采用 13.5 部署目标，既有部署目标门禁会依据已发布的平台标签检查两个原生产物。

## 验证

[目录测试](../../../../crates/jsonrpc-demo/tests/runtime_catalog_parity.rs)覆盖环境包拒绝、显式文件加载、清理与重新打开后的 SQLite 持久化，以及在空环境中通过移动后的可执行文件执行工作线程协议。[源码对比](../../../../crates/jsonrpc-demo/examples/catalog_source_parity.rs)通过源码路径别名加载固定版本模块，并检查具体工厂的可用性和必需服务声明。[源码模型冒烟测试](../../../../crates/jsonrpc-demo/examples/packaged_source_smoke.rs)通过移动后的 SDK 进程执行文本、代码执行和零 agent（智能体）工作流轮次，并检查持久日志。[审批测试](../../../../crates/user-approval/tests/approval_policy_parity.rs)固定独立服务可用性、可选提供方的生命周期以及可重入通知处理。

[构建器对比](../../../../crates/python-release/examples/executable_source_parity.rs)依据固定版本的源码解析器检查 60 个目标、宿主平台和选项用例。[产物测试](../../../../crates/python-release/tests/executable_parity.rs)覆盖 dry-run 隔离和无效原生文件。[源码 PTY 调用方](../../../../crates/pty-spawn-helper/examples/source_pty_parity.rs)通过固定版本的 node-pty 实现验证编译后的辅助程序。真实的 macOS arm64 release 构建通过移动后的可执行文件和生成的 Node 载体，均完成三轮源码模型冒烟测试及持久日志检查。延迟发送的 SDK 请求固定标准流交接行为。

[Python 运行时绑定 ABI](2026-09-04-rust-python-runtime-binding-abi.md)提供由 Rust 支撑的载体查找和按目标生成的原生库。这些检查并不证明 Python 分发已完全等价：抽象服务构造器兼容性、Python 客户端类绑定、完整的已安装 SDK 冒烟测试以及发布平台矩阵的实际执行仍是独立缺口。静态工作流检查不能替代原生 Linux 构建或已安装 Python 包的导入。

## 考虑过的替代方案

**注册整个 CLI（命令行界面）目录。** 这会暴露运行时 manifest 之外的包，并把仅属于浏览器应用的内容引入 SDK 分发。SDK 目录只选择其声明的具体实现。

**从附近的包目录解析未知裸名称。** 无关安装可能扩展交付的插件集合。保留显式文件输入，并不需要允许环境中的裸包名解析。

**根据可执行文件名或相邻文件发现工作流辅助程序。** 两者都依赖正在运行的产物之外的部署布局。传入 launcher 的确切路径可保留移动能力，并让既有进程所有者终止不配合的工作。

**让提示词展示成为审批的前置条件。** 无头组合仍需要确定性的策略与失败时拒绝的决定。可选展示不能导致该服务不可用。

**让 Node 继续作为开发模式的监控进程。** 这会增加一个进程所有者，并改变信号和退出传播。仅负责启动的进程替换让运行时行为与生命周期留在 Rust 中。

**在 runner 上编译 Linux 可执行文件，仅在 manylinux 中测试。** 这可能在验证开始前就引入更高版本的 libc 要求。编译和已安装 wheel 包检查共用固定的基线镜像。

## 后果

打包后的原生进程可以组合已声明的具体插件，并在仓库之外运行自身的工作流子进程角色。新增具体内置插件时，必须同时提供编译后的工厂并将其纳入 manifest；源码对比会暴露缺失注册。这些工厂通过原生链接替代源码 VFS（虚拟文件系统）实现，而包名、配置所有权、工作线程协议和持久结果仍是兼容要求。

分发与启动记录继续保持活跃：其 Python 载体、发布、平台和就绪要求仅被这项原生组合决策部分覆盖。此处不改变任何发布授权或发布设置。
