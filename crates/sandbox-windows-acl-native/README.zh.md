# seekdeep-sandbox-windows-acl-native

[English](README.md) | 中文

这是 `seekdeep-sandbox-windows-acl` 的窄范围 Win32 FFI 适配器。所有生产策略与生命周期行为仍位于安全 Rust crate 中；只有本 crate 将类型化句柄、有界缓冲区和 UTF-16 路径转换为原始 Windows API。由于 `windows-sys` 以原始指针公开平台 ABI，本 crate 对必需的 `unsafe` 块逐一记录安全前提。
