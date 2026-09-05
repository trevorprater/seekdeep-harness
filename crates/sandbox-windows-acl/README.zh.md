# seekdeep-sandbox-windows-acl

[English](README.md) | 中文

这是 Windows `WRITE_RESTRICTED` 令牌与 DACL 能力后端的原生 Rust 移植。安全 crate 负责经过审计的 ABI 常量、精确 Win32 错误身份、确定性工作区 SID、域分离的私有临时 SID、规范路径隔离、带锁的精确 ACE DACL 合并与撤销、受限令牌创建、默认 DACL 能力授权、`CreateProcessW` 命令行引用、job 管理的 spawn 与管道读取生命周期、runner 语法以及故障关闭的关联选项验证。

同级的 `seekdeep-sandbox-windows-acl-native` crate 是唯一狭窄的 `unsafe` 边界。它通过 `windows-sys` 把上述状态机连接到 Win32；所有原始句柄和指针都会立刻转换成有类型的安全 newtype，所有由内核拥有的 ACL 或 SID 视图也会先复制到有边界的 Rust 缓冲区，再由安全代码检查。编译后的 `windows-acl-run` 二进制实现稳定的 seam runner：继承 stdio、忽略 runner 自身的 Ctrl+C、使用关闭即终止的 job、完整宽度地传播子进程退出码、管理 seam 提供或独立创建的私有临时目录，并以 `windows-acl-run:` 签名和退出码 127 报告 runner 失败。

可移植的注入测试覆盖成功、回滚、清理聚合、幂等等待与选项顺序不变量。平台门控的原生测试会交叉检查 `windows-sys` ABI 布局，并检验真实 DACL 编辑、受限令牌访问检查、编译后 runner 的进程效果，以及组装后的沙箱 PowerShell 执行器路径。PR 的 `windows-native` job 与 master 的 `serial-windows` 备用 job 会在 Windows 内核上运行两个 ACL crate；非 Windows 开发环境可对相同测试目标执行面向 `x86_64-pc-windows-msvc` 的交叉 clippy。
