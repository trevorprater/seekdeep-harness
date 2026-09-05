# seekdeep-sandbox-local

[English](README.md) | 中文

这是 `seekdeep-sandbox` 接口的原生本地提供方。每个提供方生命周期只选择一次平台阶梯：Linux 先探测 bwrap，再探测同目录的 Rust `landlock-run`；macOS 直接选择 Seatbelt；Windows 直接选择受限令牌 ACL runner。没有平台阶梯，或多级阶梯全部探测失败时，以 `SANDBOX_UNAVAILABLE` 故障关闭。

每次包装都携带确切的强制执行级别、后端拒绝方言和结构化 runner 失败规则。操作方指定 runner 时使用 bwrap 形状的 profile，必须提供非空单行致命签名，跳过内置探测并声明完整强制执行。功能探测有正数时限并缓存结果。

bwrap 使用只读宿主树和临时 `/tmp`；Landlock 对 `/` 授予读与执行、对 `/dev/null` 授予写，并在 workspace-write 下加入宿主 `/tmp` 与工作区；Seatbelt 除 `/dev/null` 和规范化去重后的可写根目录外拒绝文件写入。

在 Windows 上，提供方把所有 ACL 与受限令牌操作委托给编译后的 Rust。每个工作区只生成一个确定性的常驻授权；每个存活的“会话/工作区”组合则拥有随机、独立、可撤销的私有临时目录能力。重复调用复用两者，分叉和工作区变更获得不同的临时能力，新提供方绝不复用崩溃残留。只读和无会话调用不携带能力 SID；无会话 workspace-write runner 自行管理一次性私有临时目录。提供方销毁时会撤销所有临时 ACE、删除自己创建的临时目录、释放所有解析后的 SID，并保留工作区常驻 ACE 作为跨进程复用缓存；清理失败会被报告但不会中止销毁。由于 Windows `WRITE_RESTRICTED` 机制固有的 Everyone 与硬链接边界，该阶梯仍如实报告为 `partial`。
