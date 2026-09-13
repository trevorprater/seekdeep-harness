# seekdeep-landlock-run

English | [中文](README.zh.md)

Native Rust `landlock-run` launcher and entry API for SeekDeep Harness. The launcher parses `--ro <path>` and `--rw <path>` grants, installs a Linux Landlock ABI 5 allow-list on itself, and then replaces itself with the exact argv after `--`. The restriction crosses `exec` and is inherited by descendants. It never selects a binary from environment variables and never executes a command when parsing, grant setup, ruleset creation, or enforcement fails.

Every launcher-owned failure prints `landlock-run: <detail>` and exits 125. Because a confined child may also exit 125, consumers attribute runner failure only when both status 125 and the fatal prefix are present. `--probe` performs real restriction and prints exactly `landlock: fully enforced` or `landlock: partially enforced (older ABI)`; missing binaries, timeouts, disabled Landlock, and unenforcing kernels all classify as `unusable` through the Rust entry API.

The Rust crate builds both a library (`seekdeep_landlock_run`) and the `landlock-run` binary. Installed applications place the launcher beside `seekdeep`; resolution is absolute and environment-independent. On non-Linux hosts the binary remains a deterministic fail-closed stub so cross-platform packaging and CLI tests can run without advertising confinement.
