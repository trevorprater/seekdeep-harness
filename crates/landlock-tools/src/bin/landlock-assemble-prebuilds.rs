//! Assemble and verify downloaded native platform artifacts.

use seekdeep_landlock_tools::{
    process::NativeRunner,
    release::{assemble_prebuilds, cli_repository_args, finish_cli, resolve_cli_path},
};

fn main() {
    finish_cli((|| {
        let (repository, args) = cli_repository_args()?;
        let artifact_root = resolve_cli_path(
            args.first().map(String::as_str),
            std::path::Path::new(".release/prebuild-artifacts"),
        )?;
        assemble_prebuilds(&repository, &artifact_root, &mut NativeRunner)
    })());
}
