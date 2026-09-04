//! Native executable build targets, source-compatible CLI parsing, and artifact assembly.

mod pipeline;
mod target;

pub use pipeline::{
    BuildReport, ENTRY_BIN, RUNTIME_DIRECTORY, build_executables, validate_native_artifact,
};
pub use target::{Arch, BuildOptions, CliError, CliOutcome, Host, Platform, Target, parse_cli};

/// Native build command usage; legacy target spelling remains accepted.
pub fn usage() -> &'static str {
    "Usage: build-exe-for-python-sdk [flags]\n\n  --targets=<t1,t2,...>  targets, e.g. node24-linux-x64,node24-linux-arm64,node24-macos-arm64.\n                         Default: the host platform only.\n  --skip-build           use existing Cargo release artifacts.\n  --dry-run              print planned commands and writes without executing.\n  --help                 print this help.\n\nBuild route: pinned Rust toolchain; the node<major> segment is retained target spelling.\nStages the dev-only Node launch binding and native carrier under python/sdk-runtime/src/deepseek_harness_runtime/runtime/node; writes executable products to dist-exe/."
}
