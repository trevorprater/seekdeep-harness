# Python contributor workflows

English | [中文](development.zh.md)

Follow the workflow for the contributor outcome you need: build runtime artifacts, validate the SDK, run against source, or build distributions. Package behavior belongs in the [SDK reference](sdk/README.md) and [runtime carrier reference](sdk-runtime/README.md).

Python workflows require the Rust-backed Python package entry points as well as native runtime artifacts. A native smoke test alone does not verify a Python installation; the [native assembly note](../.agents/notes/implemented/architecture/2026-09-04-rust-packaged-sdk-runtime-assembly.md) defines that verification boundary.

## Build runtime artifacts

Platform executables are build artifacts and are not checked into git. Run the build from the repository root:

```sh
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
cargo run --locked -p seekdeep-python-release --bin build-exe-for-python-sdk --
```

Use `--skip-build` when the required Cargo release artifacts already exist, or `--targets=node24-macos-arm64` to select a target. The legacy `node<major>` field remains accepted; the pinned Rust toolchain controls the implementation. Products land in `dist-exe/`, and the builder syncs the selected executables and the host development carrier into `python/sdk-runtime/`. macOS builds also sync the matching Rust PTY spawn helper. Linux targets require a same-architecture Linux host; the release workflows build them inside pinned manylinux 2.28 containers. `--dry-run` prints planned commands and writes without executing them.

## Validate the SDK

Keep the virtual environment outside `python/`, install the test group, and run the Python suite:

```sh
export UV_PROJECT_ENVIRONMENT="$PWD/tmp/py-sdk-venv"
uv sync --project python/sdk --group test
uv run --project python/sdk pytest
```

`python/sdk/tests/test_bundled_runtime.py` exercises available bundled carriers and skips a carrier when its artifact has not been built. For repository-wide test policy, see [Testing](../docs/testing.md).

An interactive smoke test needs `DEEPSEEK_API_KEY` in the environment or repository-root `.env`:

```python
from deepseek_harness import DeepSeekHarness

with DeepSeekHarness() as harness:
    print(harness.run("say hi").final_response)
```

## Run a development carrier

Repository contributors can select either native development entry:

- Set `SEEKDEEP_RUNTIME_MODE=node` to use the built launch binding on system Node `>=22.19`. It replaces Node with the native runtime; distributions never include or auto-select this carrier.
- Set `launch_args_override=(absolute_executable_path,)` to run a locally built Rust executable. Supply `cordis=...` when the default configuration is not suitable.

The [source-model smoke](../crates/jsonrpc-demo/examples/packaged_source_smoke.rs) exercises both carriers through the Rust SDK client independently of Python package installation.

## Build distributions

The root `package.json` version is authoritative for both Python distributions. The Rust staging command injects that version into both wheels and pins the SDK to the same `seekdeep-harness-runtime-bin` version. It refuses to build a metadata-only wheel when a required Python binding entry is missing.

Build the pure SDK wheel once and one runtime wheel on each native platform:

```sh
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
version="$(cargo run --quiet --locked -p seekdeep-python-release -- version --github-output | sed -n 's/^version=//p')"
cargo run --locked -p seekdeep-python-release -- build --package sdk --output-dir dist-python
cargo run --locked -p seekdeep-python-release -- build --package runtime --platform macos-arm64 --runtime-exe dist-exe/seekdeep-jsonrpc-agent-pkg-macos-arm64 --output-dir dist-python
pip install --find-links dist-python seekdeep-harness-sdk=="$version"
```

The runtime distribution is wheel-only. The release pipeline publishes three platform wheels with the pure SDK wheel: Linux x64, Linux arm64, and macOS 14 or newer on arm64. A `python-v<repository-version>` tag is accepted only when it matches the repository version; prerelease repository versions such as `0.0.1-rc.1` use their normalized PEP 440 spelling, such as `0.0.1rc1`, inside wheel filenames and metadata.

## Validate a release candidate

Label a pull request `python-release-dry-run`, or manually run the GitHub `Release (Python)` workflow with `publish=false`, to build all four wheels, install the Linux release set on Python 3.10 and 3.14, check exact filenames and metadata, enforce PyPI's default per-file size limit, and retain one aggregate artifact with SHA-256 hashes. Both paths have no registry credentials; a pull request run cannot enter either publication job.

Public publication runs from the private automation repository; package metadata points to the separate read-only public source mirror, which does not run release Actions. The private repository defines the repository variable `PYPI_PUBLISHER_REPOSITORY` as its own `owner/name` and keeps `PUBLIC_PYPI_RELEASE_ENABLED=false` except during an intentional release.

Separate runtime and SDK jobs let an SDK upload failure resume without resending immutable runtime files. They accept `publish=true` only when the workflow runs from the configured publisher repository at the matching `python-v*` tag and the protected `pypi-runtime` and `pypi` environments approve the runtime and SDK jobs, respectively. PyPI Trusted Publishing still supplies short-lived OIDC credentials, but public attestations are disabled because they would disclose the private publisher identity.
