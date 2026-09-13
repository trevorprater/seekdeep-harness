# UI-attachment 兼容性声明

[English](README.md) | 中文

每个 `*.d.ts.txt` 文件都是固定版本源包所生成声明的逐字副本。Rust/WASM 包构建器移除末尾的 `.txt`，应用所需的包标识替换，并在 `lib/types/` 下写入包含六个文件的声明目录树。

该存储后缀使声明元数据不属于 `cargo xtask parity` 检查的其他语言可执行代码。更新时须从固定版本的参考源一次性刷新整棵目录树。
