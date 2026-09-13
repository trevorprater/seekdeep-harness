# seekdeep-process-exit-hook

[English](README.md) | 中文

为 host 正常退出时同步清理受管进程提供内部 native 支持。Rust 标准库没有暴露 `atexit`，因此本 crate 拥有 workspace 中唯一一处经过狭义说明的 unsafe 平台调用。

注册以 weak 方式持有 target，并可重复移除而不产生副作用。单个 process-global C callback 会升级仍存活的注册，同步调用每个 target，并隔离每个 target 的 panic，使一次失败不会阻止后续 target 收到 finalization。本地 subprocess provider 在 publication 前注册，并只在其 awaited 正常 disposal 达到 quiescence 后注销。

该 callback 仅提供 best-effort 保证，不执行异步工作，也不声称已 reap 被终止的进程。无法执行 `atexit` callback 的退出路径仍需要外部 supervisor。
