//! Bump the native package family and refresh its lockfile.

use seekdeep_landlock_tools::{
    process::NativeRunner,
    release::{ReleaseEnvironment, bump_release, cli_repository_args, finish_cli},
};

fn main() {
    finish_cli((|| {
        let (repository, args) = cli_repository_args()?;
        bump_release(
            &repository,
            args.first().map_or("", String::as_str),
            &ReleaseEnvironment::from_environment(),
            &mut NativeRunner,
        )?;
        Ok(())
    })());
}
