# seekdeep-sandbox-windows-acl-native

English | [中文](README.zh.md)

The narrowly scoped Win32 FFI adapter for `seekdeep-sandbox-windows-acl`. All production policy and lifecycle behavior remains in the safe Rust crate; this crate alone translates typed handles, bounded buffers, and UTF-16 paths to raw Windows APIs. Its documented `unsafe` blocks are required because `windows-sys` exposes the platform ABI as raw pointers.
