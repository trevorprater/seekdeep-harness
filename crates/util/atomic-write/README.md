# seekdeep-atomic-write

English | [中文](README.zh.md)

Atomic file replacement shared by file-backed SeekDeep stores that must not
leave partial, symlink-hijacked, or wider-than-intended content on disk. The
Rust implementation is exported by `seekdeep_util::atomic_write`.

## Surface

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

`write_file_atomic` commits one already-rendered byte sequence. Its contract is:

- A random-suffix same-directory temporary file is opened with exclusive
  create, preventing a planted temporary-path symlink from being followed.
- The fresh inode receives the caller's required `mode` and carries it through
  rename, narrowing a wider existing file without a `chmod` race. Fresh inode
  and directory modes remain subject to the process umask.
- Rename replaces a symlinked target entry itself and never writes through to
  its referent.
- The same-directory temporary keeps the swap on one filesystem.
- Parent directories are created. Any failure removes the temporary sibling;
  readers observe either the old complete content or the new complete content.

`with_file_lock` serializes cross-process writers of one file around a complete
read-render-commit operation. It exclusively creates `<filename>.lock` with
mode `0600`; readers never contend. Waiters back off exponentially from 20 ms
to 200 ms and fail after two seconds. A contender never steals or deletes an
existing lock because age cannot distinguish a crashed process from a paused
live owner. The lock is removed whether the protected operation succeeds or
fails, and a release failure takes precedence like a `finally` failure.

## Model experience

None. This is a filesystem primitive and contributes nothing to a model
request or KV-cache prefix.

## Known limitations

- Atomicity is not crash durability: the implementation intentionally performs
  no `fsync` of the file or parent directory.
- Orphaned locks require operator recovery after verifying no writer still owns
  the lock; file age alone is insufficient evidence.
- The lock's parent directory must already exist. `write_file_atomic` creates
  parents, but the larger read-modify-write cycle decides when to do so.
