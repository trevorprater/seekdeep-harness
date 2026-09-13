# UI-primitives 兼容性声明

[English](README.md) | 中文

每个 `*.d.ts.txt` 文件都是固定版本源包所生成声明的逐字副本。Rust/WASM 包构建器移除末尾的 `.txt`，应用所需的 DeepSeek Harness → SeekDeep Harness 包名替换，并在 `lib/types/` 下写入生成的声明目录树。

`.txt` 存储后缀使声明元数据与仓库级 Rust-only 检查所覆盖的其他语言可执行代码相区分。更新时须从固定版本的参考源一次性刷新整棵目录树；不要就地编辑单个声明文件。
