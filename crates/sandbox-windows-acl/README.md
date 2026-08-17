# seekdeep-sandbox-windows-acl

English | [中文](README.zh.md)

Native Rust port of the Windows `WRITE_RESTRICTED` token and DACL capability backend. The safe crate owns audited ABI constants, exact Win32 error identity, deterministic workspace and domain-separated private-temp SIDs, canonical path separation, locked exact-ACE DACL merge/revoke, restricted-token construction, default-DACL capability grants, `CreateProcessW` command-line quoting, job-owned spawn and pipe-drain lifecycles, runner grammar, and fail-closed dependent-option validation.

The sibling `seekdeep-sandbox-windows-acl-native` crate is the sole narrow `unsafe` boundary. It binds those state machines to Win32 using `windows-sys`; every raw handle and pointer is converted immediately to typed safe newtypes, and every kernel-owned ACL or SID view is copied into a bounded Rust buffer before safe code inspects it. The compiled `windows-acl-run` binary implements the stable seam runner: inherited stdio, runner-owned Ctrl+C suppression, a kill-on-close job, exact full-width child exit propagation, seam-managed or standalone private temp, and runner failures under the `windows-acl-run:` signature with exit 127.

Portable injected tests verify success, rollback, cleanup aggregation, idempotent waiting, and option-order invariants. The native crate and runner cross-compile and pass strict clippy for `x86_64-pc-windows-msvc`. Real Windows ACL access checks and crash/job world effects still require execution on a Windows host before this backend can be marked verified end to end.
