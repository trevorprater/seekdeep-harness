# Agent Note: Rust Python 运行时绑定 ABI

Status: implemented

[English](2026-09-04-rust-python-runtime-binding-abi.md) | 中文

## 问题

Python 运行时包提供可导入的查找函数和解释器特定的异常，而移植要求这些决策在 Rust 中执行。仅返回等价 JSON 的替代实现可能丢失调用方对象的行为、异常对象身份，或显式可执行文件与开发载体之间的区别。CPython 专用扩展还会为既有的仅按平台区分的运行时 wheel 包约定引入解释器 ABI 维度。

## 决策

[安全的 SDK crate](../../../../crates/python-sdk/src/lib.rs)负责运行时选择、产物查找和同步 SDK 策略。[原生绑定 crate](../../../../crates/python-sdk-ffi/src/lib.rs)通过借用的请求字节、不透明的回调上下文 ID，以及由分配器管理的响应句柄提供版本 3 的 C ABI。它不链接 Python ABI 符号。由于 C 调用方无法表达 Rust 字节借用或可调用对象的生命周期，unsafe 代码被限制在这个原生 crate 内；其余 SDK 代码保留工作区禁止 unsafe 代码的要求。

响应句柄单调递增且永不复用。借用的响应字节在句柄被释放或由回调返回值消费之前保持有效。释放未知或陈旧句柄不会产生效果，后续分配也不会继承先前句柄。缓冲区析构在注册表锁之外执行。原生 panic 会转换为绑定失败，而不会跨 C 边界展开栈。

[Rust 生成器](../../../../crates/python-sdk/src/bindings.rs)生成 Python 运行时与 SDK 声明，以及通用的 `ctypes` 编组代码。Rust 决定请求哪些解释器相等性或表示操作时，运行时模式对象保持不透明。因此，已接受的模式不会调用 `repr`，失败的比较或表示操作会返回原始 Python 异常对象。嵌套运行时查找保留既有的可替换 `bundled_package_dir` 和 `_current_platform_tag` 函数。短期查找表在响应解码后结束；原生读取线程使用客户端上下文期间会固定保留它，而保留项在最后一个原生所有者结束后释放。

[客户端分发器](../../../../crates/python-sdk-ffi/src/client.rs)提供由 Rust 管理的 harness、客户端、订阅和进程句柄。每个匹配的订阅者都会收到同一个可变 Python 通知对象。已收集的根会话事件保留独立的对象引用，因此替换通知当前的事件不会改变已收集事件的引用目标。Rust 在源码的决策点读取当前字段，包括同步用户回调之后和构造最终结果时。可变配置也在源码对应的操作边界读取。对于显式但为假值的高层配置，Rust 仍然负责选择，同时只执行一次解释器真值测试。外部异常同时携带本地身份与所属解释器上下文；因此，harness 调用其客户端的回调时，会重新抛出原始异常或 cause，而不会到 harness 的对象表中查找。

公开的响应投影通过基础回调遍历调用方拥有的 Python 对象，而筛选与选择算法仍由 Rust 管理。这样可以保留字符串子类、任意精度整数、非有限浮点数、孤立代理项、自定义真值与字符串操作，以及这些操作抛出的原始异常。工作区 JSON 表示在底层 SDK 线路上传递任意精度数字时保留原始拼写；ABI 解码会显式处理经过缓冲的浮点字段，因此扩展后的数字域不会改变超时配置。

[可执行文件构建器](../../../../crates/python-release/src/executable/pipeline.rs)会为每个原生目标编译绑定库。包含架构的库文件名可避免不同目标的产物发生冲突。它还会生成宿主平台检出中的运行时与 SDK 声明，以及 Hatch 绑定。wheel 包暂存会选择一个匹配的库，并为该目标重新生成声明；钩子拒绝缺失或混合的原生绑定载荷。发布的平台标签和导入命名空间保持不变。生成的 Python 文件和原生库是构建产物，而非另一套实现源码。

Hatch 绑定将策略转交给 Rust 发行工具。发行暂存提供显式工具路径；可编辑检出可通过仓库 Cargo manifest（元数据清单）调用同一工具。模块导入时，它要求 Rust 验证并序列化平台清单，随后将该不可变快照提供给钩子的后续初始化，因此即使文件发生变化也能保留源码的导入时边界。它不会在 Python 中重复实现平台或载荷策略。

## 验证

[绑定测试](../../../../crates/python-sdk-ffi/tests/runtime_binding.rs)从 Python 加载真实共享库，执行嵌套和并发调用，保留回调异常对象身份，并检查包括孤立代理项字符串在内的未知模式。[运行时对比](../../../../crates/python-sdk-ffi/examples/runtime_source_parity.rs)和[客户端对比](../../../../crates/python-sdk-ffi/examples/client_source_parity.rs)对生成的绑定运行固定版本的 Python 测试，仅替换产品身份。客户端对比还检查公开声明，以及源码差分的值／路径／传输矩阵：假值配置选择、符号链接与相对路径、可变通知身份、延后的事件替换、实时配置、导入错误与解析器错误的区分、自定义投影回调、非有限值与任意精度值、双向宽整数、回调清理，以及跨上下文的异常 cause。[原生观察测试](../../../../crates/python-sdk/tests/observation_parity.rs)固定引用生命周期与修改顺序。原生 ABI 测试验证缓冲区所有权、直接操作解码和陈旧句柄拒绝行为。[发行测试](../../../../crates/python-release/tests/release_parity.rs)覆盖按目标暂存库和 wheel 包载荷检查。

[已安装 wheel 包对比](../../../../crates/python-sdk-ffi/examples/installed_source_smoke.rs)拒绝从隔离环境外导入，并通过默认 SDK 启动、自定义文本／代码／工作流轮次、minimal 双工具配置、完整的 advanced 可执行文件快照和直接运行时启动，驱动固定源码的免密钥模型服务器。它逐字节比较全部三个持久日志，并保留 SDK 结果中的每个字段和通知。[内置运行时对比](../../../../crates/python-sdk-ffi/examples/bundled_runtime_source_parity.rs)针对原生可执行文件与开发用 Node 启动器运行全部十个固定载体用例，其中包括默认配置未设置或为空，以及缺失插件导致的启动失败。

[源码调度探针](../../../../crates/python-sdk-ffi/examples/workflow_order_source_parity.rs)表明，`workflow/agent-start` 可以在子 agent 模型解析之前、请求准备与输出之间，或子 agent 完成之后到达。子 agent 独立启动，而其句柄与启动观察经过一次工作线程往返。因此，快照比较仅允许父会话对应的 `tool-workflow/agent-start` 在 `subagent.started` 之后跨越该子 agent 自身的通知。它不能跨越其他父会话记录或兄弟会话通知；事件内容、数量、会话内顺序以及开始／结束配对仍须精确一致。[负向比较测试](../../../../crates/python-sdk-ffi/tests/smoke_snapshot.rs)拒绝这些无关变化，[真实工作流测试](../../../../crates/workflow-worker-thread/tests/integration_parity.rs)则要求启动发布不依赖模型元数据就绪。

已安装的 macOS arm64 wheel 包在 Python 3.10 和 3.14 上通过这五个场景。Linux x64 与 arm64 的 release 可执行文件和绑定库在各自固定的 manylinux 2.28 镜像内构建，并在 Python 3.10 上通过已安装 wheel 包运行检查；两种载荷均不要求高于 GLIBC_2.28 的版本。这些检查覆盖了清单中所有 Python SDK 与运行时源码表面，但并不证明整个产品已等价、公开发布已就绪，或干净检出中的 CI 已具备可用的独立原生冒烟入口。

## 考虑过的替代方案

**将运行时查找逻辑保留在 Python 中。** 这会为模式优先级、平台别名和缺失产物行为创建第二套实现，违背 Rust 所有权要求。

**将 CPython 扩展作为 SDK wheel 包发布。** 这会改变平台无关的 SDK 分发，并引入 Python ABI 特定的构建与安装要求。C ABI 库仍属于既有的原生运行时分发。

**先序列化所有回调参数。** 解释器的相等性和表示操作可执行调用方代码，并抛出特定异常对象。复制通知载荷还会丢失订阅者共享的修改，并在字段替换后改变已捕获事件的引用目标。不透明引用可保留这些操作、身份及其顺序。

**将分配器地址作为可复用的所有权身份返回。** 分配器可能复用已释放地址，使陈旧释放操作影响后续响应。不复用的句柄将所有权与存储地址分开。

**在工作流启动记录发布前阻塞子 agent 流。** 源码允许子 agent 先完成，也允许模型元数据随后才解析。生产运行时中的屏障会引入额外依赖，可能使元数据解析死锁，并移除受支持的调度顺序。比较器仅处理已通过实测证明的这项调度自由度。

## 后果

Python 的公开运行时与客户端 API 将选择、文件系统、生命周期和运行区间收集决策委托给编译后的 Rust，而不获取或下载运行时。绑定要求匹配的原生库和 ABI，独立运行时可执行文件仍无需安装 Python。[递归通知](../bug-fix/2026-07-24-recursive-python-sdk-session-notifications.md)和[自主管理运行区间](2026-07-30-followup-enqueue-and-owned-runs.md)策略继续定义哪些观察结果属于一次运行结果。

[打包运行时组合](2026-09-04-rust-packaged-sdk-runtime-assembly.md)、[单文件分发](2026-07-10-single-file-executable-sdk-runtime-distribution.md)和[发布工作流](../process/2026-08-11-python-publication-workflow.md)记录继续保持活跃。此 ABI 不改变插件重载规则、配置所有权、发布授权或完整 SDK 发行所需的证据。
