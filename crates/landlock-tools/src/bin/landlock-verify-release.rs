//! Verify native package versions, release tags, and optional prebuild payloads.

use seekdeep_landlock_tools::{
    process::NativeRunner,
    release::{ReleaseEnvironment, cli_repository_args, finish_cli, verify_release},
};

fn main() {
    finish_cli((|| {
        let (repository, args) = cli_repository_args()?;
        verify_release(
            &repository,
            &ReleaseEnvironment::from_environment(),
            args.iter().any(|arg| arg == "--prebuilds"),
            &mut NativeRunner,
        )?;
        Ok(())
    })());
}
