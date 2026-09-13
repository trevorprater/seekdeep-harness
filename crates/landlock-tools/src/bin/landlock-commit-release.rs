//! Bump and commit the native package family without creating a tag.

use seekdeep_landlock_tools::{
    process::NativeRunner,
    release::{ReleaseEnvironment, cli_repository_args, commit_release, finish_cli},
};

fn main() {
    finish_cli((|| {
        let (repository, args) = cli_repository_args()?;
        commit_release(
            &repository,
            args.first().map_or("", String::as_str),
            &ReleaseEnvironment::from_environment(),
            &mut NativeRunner,
        )?;
        Ok(())
    })());
}
