# Agent Note: Rust Python 运行时绑定 ABI

Status: implemented

[English](2026-09-04-rust-python-runtime-binding-abi.md) | 中文

## 问题

Python 运行时包提供可导入的查找函数和解释器特定的异常，而移植要求这些决策在 Rust 中执行。仅返回等价 JSON 的替代实现可能丢失调用方对象的行为、异常对象身份，或显式可执行文件与开发载体之间的区别。CPython 专用扩展还会为既有的仅按平台区分的运行时 wheel 包约定引入解释器 ABI 维度。

## 决策

[安全的 SDK crate](../../../../crates/python-sdk/src/lib.rs)负责运行时选择、产物查找和同步 SDK 策略。[原生绑定 crate](../../../../crates/python-sdk-ffi/src/lib.rs)通过借用的请求字节、不透明的回调上下文 ID，以及由分配器管理的响应句柄提供带版本的 C ABI。它不链接 Python ABI 符号。由于 C 调用方无法表达 Rust 字节借用或可调用对象的生命周期，unsafe 代码被限制在这个原生 crate 内；其余 SDK 代码保留工作区禁止 unsafe 代码的要求。

响应句柄单调递增且永不复用。借用的响应字节在句柄被释放或由回调返回值消费之前保持有效。释放未知或陈旧句柄不会产生效果，后续分配也不会继承先前句柄。缓冲区析构在注册表锁之外执行。原生 panic 会转换为绑定失败，而不会跨 C 边界展开栈。

[Rust 生成器](../../../../crates/python-sdk/src/bindings.rs)生成 Python 运行时声明和通用的 `ctypes` 编组代码。Rust 决定请求哪些解释器相等性或表示操作时，运行时模式对象保持不透明。因此，已接受的模式不会调用 `repr`，失败的比较或表示操作会返回原始 Python 异常对象。嵌套运行时查找保留既有的可替换 `bundled_package_dir` 和 `_current_platform_tag` 函数。每次调用拥有的对象表在响应解码后释放。

[可执行文件构建器](../../../../crates/python-release/src/executable/pipeline.rs)会为每个原生目标编译绑定库。包含架构的库文件名可避免不同目标的产物发生冲突。它还会生成宿主平台检出中的运行时声明和 Hatch 绑定。wheel 包暂存会选择一个匹配的库，并为该目标重新生成声明；钩子拒绝缺失或混合的原生绑定载荷。发布的平台标签和导入命名空间保持不变。生成的 Python 文件和原生库是构建产物，而非另一套实现源码。

Hatch 绑定将策略转交给 Rust 发行工具。发行暂存提供显式工具路径；可编辑检出可通过仓库 Cargo manifest（元数据清单）调用同一工具。它不会在 Python 中重复实现平台或载荷策略。

## 验证

[绑定测试](../../../../crates/python-sdk-ffi/tests/runtime_binding.rs)从 Python 加载真实共享库，执行嵌套和并发调用，保留回调异常对象身份，并检查包括孤立代理项字符串在内的未知模式。[源码对比](../../../../crates/python-sdk-ffi/examples/runtime_source_parity.rs)对生成的绑定运行固定版本的运行时解析测试文件，仅替换产品身份。原生测试验证缓冲区所有权和陈旧句柄拒绝行为。[发行测试](../../../../crates/python-release/tests/release_parity.rs)覆盖按目标暂存库和 wheel 包载荷检查。

此决策并不证明 Python SDK 已完全等价。同步 Rust 客户端已有原生子进程测试，但其 Python 客户端类、可变通知与事件的对象身份、完整的 Python 值边界情况，以及已安装 SDK 的完整轮次冒烟测试仍是独立工作。Linux 发布矩阵也仍需原生执行证据。

## 考虑过的替代方案

**将运行时查找逻辑保留在 Python 中。** 这会为模式优先级、平台别名和缺失产物行为创建第二套实现，违背 Rust 所有权要求。

**将 CPython 扩展作为 SDK wheel 包发布。** 这会改变平台无关的 SDK 分发，并引入 Python ABI 特定的构建与安装要求。C ABI 库仍属于既有的原生运行时分发。

**先序列化所有回调参数。** 解释器的相等性和表示操作可执行调用方代码，并抛出特定异常对象。不透明引用可保留这些操作及其顺序。

**将分配器地址作为可复用的所有权身份返回。** 分配器可能复用已释放地址，使陈旧释放操作影响后续响应。不复用的句柄将所有权与存储地址分开。

## 后果

Python 的公开运行时 API 将选择和文件系统决策委托给编译后的 Rust，而不获取或下载运行时。绑定要求匹配的原生库和 ABI，独立运行时可执行文件仍无需安装 Python。

[打包运行时组合](2026-09-04-rust-packaged-sdk-runtime-assembly.md)、[单文件分发](2026-07-10-single-file-executable-sdk-runtime-distribution.md)和[发布工作流](../process/2026-08-11-python-publication-workflow.md)记录继续保持活跃。此 ABI 不改变插件重载规则、配置所有权、发布授权或完整 SDK 发行所需的证据。
