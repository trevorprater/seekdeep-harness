# Python 贡献者工作流

[English](development.md) | 中文

根据所需的贡献者成果选择工作流：构建运行时产物、验证 SDK、从源码运行或构建分发包。包行为分别见 [SDK 参考](sdk/README.md) 和[运行时载体参考](sdk-runtime/README.md)。

Python 工作流既需要原生运行时产物，也需要由 Rust 支撑的 Python 包入口。仅通过原生冒烟测试并不能验证 Python 安装；[原生组合记录](../.agents/notes/implemented/architecture/2026-09-04-rust-packaged-sdk-runtime-assembly.md)定义这一验证边界。

## 构建运行时产物

各平台可执行文件是构建产物，不检入 git。请在仓库根目录运行构建：

```sh
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
cargo run --locked -p seekdeep-python-release --bin build-exe-for-python-sdk --
```

所需 Cargo release 产物已存在时使用 `--skip-build`；如需选择目标，请使用 `--targets=node24-macos-arm64`。既有的 `node<major>` 字段仍被接受；实现由固定版本的 Rust 工具链决定。产物写入 `dist-exe/`，构建器会将所选可执行文件和宿主平台开发载体同步到 `python/sdk-runtime/`。macOS 构建还会同步配套的 Rust PTY spawn 辅助程序。Linux 目标要求同架构的 Linux 宿主平台；发布工作流在固定版本的 manylinux 2.28 容器内构建它们。`--dry-run` 输出计划执行的命令与写入操作，但不执行。

## 验证 SDK

请将虚拟环境放在 `python/` 之外，安装测试组，然后运行 Python 测试套件：

```sh
export UV_PROJECT_ENVIRONMENT="$PWD/tmp/py-sdk-venv"
uv sync --project python/sdk --group test
uv run --project python/sdk pytest
```

`python/sdk/tests/test_bundled_runtime.py` 会运行可用的内置载体；某个载体的产物尚未构建时，会跳过该载体。仓库级测试政策见 [测试](../docs/testing.md)。

交互式冒烟测试需要环境变量或仓库根目录 `.env` 中存在 `DEEPSEEK_API_KEY`：

```python
from deepseek_harness import DeepSeekHarness

with DeepSeekHarness() as harness:
    print(harness.run("say hi").final_response)
```

## 运行开发载体

仓库贡献者可以选择以下任一原生开发入口：

- 设置 `SEEKDEEP_RUNTIME_MODE=node`，在系统 Node `>=22.19` 上使用已构建的启动绑定。它将 Node 替换为原生运行时；分发物绝不会包含或自动选择该载体。
- 设置 `launch_args_override=(absolute_executable_path,)`，以运行本地构建的 Rust 可执行文件。默认配置不合适时，请提供 `cordis=...`。

[源码模型冒烟测试](../crates/jsonrpc-demo/examples/packaged_source_smoke.rs)通过 Rust SDK 客户端验证两种载体，不依赖 Python 包安装。

## 构建分发包

根目录 `package.json` 的版本是两个 Python 分发包的权威版本。Rust 暂存命令会将该版本注入两个 wheel 包，并将 SDK 固定到同版本的 `seekdeep-harness-runtime-bin`。必需的 Python 绑定入口缺失时，它会拒绝构建仅含元数据的 wheel 包。

纯 SDK wheel 包只需构建一次；每个原生平台分别构建一个运行时 wheel 包：

```sh
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
version="$(cargo run --quiet --locked -p seekdeep-python-release -- version --github-output | sed -n 's/^version=//p')"
cargo run --locked -p seekdeep-python-release -- build --package sdk --output-dir dist-python
cargo run --locked -p seekdeep-python-release -- build --package runtime --platform macos-arm64 --runtime-exe dist-exe/seekdeep-jsonrpc-agent-pkg-macos-arm64 --output-dir dist-python
pip install --find-links dist-python seekdeep-harness-sdk=="$version"
```

运行时分发包仅提供 wheel 包。发布流水线会连同纯 SDK wheel 包一起发布三个平台 wheel 包：Linux x64、Linux arm64 和 macOS 14 或更高版本的 arm64。只有与仓库版本匹配时，才接受 `python-v<repository-version>` 标签；`0.0.1-rc.1` 之类的仓库预发布版本在 wheel 包文件名和元数据中使用规范化的 PEP 440 写法，例如 `0.0.1rc1`。

## 验证候选发行版

为拉取请求添加 `python-release-dry-run` 标签，或手动运行 GitHub 的 `Release (Python)` 工作流并设置 `publish=false`，即可构建全部四个 wheel 包，在 Python 3.10 和 3.14 上安装 Linux 发行集合，检查精确文件名和元数据，执行 PyPI 默认单文件大小限制，并保留一份带 SHA-256 哈希的汇总产物。两条路径都没有注册表凭据，拉取请求运行无法进入任何发布作业。

公开发布从私有自动化仓库运行；包元数据指向独立的只读公开源码镜像，该镜像不运行发布 Actions。私有仓库把仓库变量 `PYPI_PUBLISHER_REPOSITORY` 定义为自身的 `owner/name`，并且只在有意发布期间把 `PUBLIC_PYPI_RELEASE_ENABLED` 从 `false` 改为 `true`。

独立的运行时与 SDK 作业使 SDK 上传失败后可以继续执行，而无需重新发送不可变的运行时文件。只有工作流从配置的发布仓库、匹配的 `python-v*` 标签运行，且受保护的 `pypi-runtime` 和 `pypi` 环境分别批准运行时与 SDK 作业时，才接受 `publish=true`。PyPI Trusted Publishing 仍会提供短期 OIDC 凭据，但公开 attestation 会披露私有发布仓库身份，因此将其禁用。
