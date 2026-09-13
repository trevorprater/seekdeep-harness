# seekdeep-landlock-run

[English](README.md) | 中文

这是 SeekDeep Harness 的原生 Rust `landlock-run` 启动器与入口 API。启动器解析 `--ro <path>`、`--rw <path>`，在自身安装 Linux Landlock ABI 5 允许列表，然后用 `--` 后的确切 argv 替换自身。限制会跨越 `exec` 并由后代继承。环境变量不能选择启动器；解析、授权根目录、规则集创建或强制执行失败时绝不运行命令。

启动器自身的失败都打印 `landlock-run: <detail>` 并以 125 退出。受限子进程也可能返回 125，因此消费方必须同时匹配状态与致命前缀。`--probe` 真正安装限制，并精确报告完整或旧 ABI 的部分强制执行；缺失、超时、禁用或无法强制执行都由 Rust API 归类为 `unusable`。

该 crate 同时构建库与 `landlock-run` 二进制。安装时启动器位于 `seekdeep` 旁边，解析为绝对且不依赖环境的路径。非 Linux 主机上的二进制是确定性的故障关闭 stub，只用于跨平台打包与 CLI 验证，不会宣称存在限制能力。
