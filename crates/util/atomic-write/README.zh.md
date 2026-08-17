# seekdeep-atomic-write

[English](README.md) | 中文

供 SeekDeep 文件型存储共用的原子文件替换原语，避免在磁盘上留下不完整、被符号链接劫持
或权限过宽的内容。Rust 实现由 `seekdeep_util::atomic_write` 导出。

## 接口面

```rust,no_run
use seekdeep_util::atomic_write::{
    WriteFileAtomicOptions, with_file_lock, write_file_atomic,
};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let path = "/home/u/.seekdeep/settings.yaml";
with_file_lock(path, || async {
    write_file_atomic(
        path,
        b"theme: dark\n",
        WriteFileAtomicOptions { mode: 0o600, dir_mode: Some(0o700) },
    )
    .await
})
.await??;
# Ok(())
# }
```

`write_file_atomic` 提交一份已经渲染好的字节序列，其约定如下：

- 在目标同目录以独占创建方式打开带随机后缀的临时文件，拒绝跟随预先埋下的临时路径
  符号链接。
- 全新 inode 获得调用方要求的 `mode` 并携带它完成 rename，从而在没有 `chmod` 竞态
  的情况下收窄旧文件权限。新 inode 和目录的模式仍受进程 umask 影响。
- rename 替换符号链接目标条目本身，绝不写穿到其指向的文件。
- 同目录临时文件使交换保持在同一文件系统。
- 自动创建父目录；任意失败都会移除临时同级文件。读取方只会看到完整旧内容或完整新内容。

`with_file_lock` 围绕完整的读取、渲染、提交操作，跨进程串行化同一文件的写入方。它以
`0600` 模式独占创建 `<filename>.lock`；读取方无需竞争。等待方从 20 ms 到 200 ms
指数退避，并在两秒后失败。竞争者绝不窃取或删除现有锁，因为文件龄无法区分崩溃进程和
暂停但仍存活的所有者。无论受保护操作成功还是失败都会释放锁；释放失败像 `finally`
失败一样优先返回。

## 模型体验

无。本模块是文件系统原语，不向模型请求或 KV cache 前缀贡献任何内容。

## 已知限制

- 原子性不等于崩溃持久性：实现刻意不对文件或父目录执行 `fsync`。
- 遗留锁需要操作者先确认没有写入方仍持有，再手动恢复；仅凭文件龄不能证明无人持有。
- 锁的父目录必须已存在。`write_file_atomic` 会创建父目录，但更大的读改写流程决定何时创建。
