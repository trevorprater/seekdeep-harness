# seekdeep-sandbox

[English](README.md) | 中文

进程沙箱服务定义。该 crate 负责 `sandbox` Cordis 服务约定（`SandboxProvider`）与 SeekDeep Harness 共享的限制词汇：`SandboxMode`（`read-only`／`workspace-write`／`danger-full-access`，仅限文件操作）、`SandboxEnforcement`（`full`／`partial`）、`SandboxExecutionPolicy`（每次调用的完整模式及工作区根目录）、`SandboxPolicy`（其中受限制的子集），以及故障时拒绝放行的 `SANDBOX_UNAVAILABLE` 错误。它只依赖平台无关接口，不依赖具体后端。

`SandboxProvider::confine(argv, policy)` 返回应当取代原始 argv 的 argv，使进程及其派生进程都在限制下运行；还包含后端的强制执行完整度、拒绝方言（`denial_signatures`）和结构化 runner 失败证据（`runner_failure_rules`）。没有可用后端时，它返回错误，绝不会让原始 argv 不受限制地运行。

策略随调用传递，而不属于提供方：多个消费方可同时按不同策略运行；获批的升权重试是使用更宽策略的新调用。

**只支持同世界限制。** 后端与宿主共享文件系统和内核；`workspace_root` 指向文件系统规范化后的真实目录。系统先解析目录身份，再做词法规范化，因此包含 `symlink/..` 的 cwd 会授权 `chdir` 实际到达的目录。容器、microVM 与远程执行器应替换整个能力提供方。

## 模型体验

无法强制执行请求模式时，消费方暴露 `SANDBOX_UNAVAILABLE` 与以下精确错误。执行期 runner 失败会追加 ` Runner failure: <detail>`。

```markdown
sandbox mode "<mode>" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access.
```

错误只追加到历史，位于可复用请求前缀之后，不会使已有 KV Cache 失效。

## 已知限制与暂缓事项

- 策略词汇只涵盖文件操作，不表达网络、进程、系统调用、设备或凭据限制。
- 容器、microVM 与远程执行需要替换能力实现。
- 拒绝报告使用 stderr 方言，消费方从子进程输出推断分类。
- Runner 诊断使用带内通道，可能造成诊断误归因但不能绕过限制；带外状态通道暂缓。
- 每个上下文只有一个提供方；多机制组合需要提供方级阶梯或独立 Cordis 上下文。
