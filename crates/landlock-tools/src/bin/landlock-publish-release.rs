//! Publish verified local package tarballs with registry integrity checks.

use seekdeep_landlock_tools::{
    process::NativeRunner,
    release::{cli_repository_args, finish_cli, publish_release, resolve_cli_path},
};

fn main() {
    finish_cli((|| {
        let (repository, args) = cli_repository_args()?;
        let destination = resolve_cli_path(
            args.iter()
                .find(|arg| !arg.starts_with("--"))
                .map(String::as_str),
            &repository.root.join("dist/npm"),
        )?;
        publish_release(&destination, &std::env::current_dir()?, &mut NativeRunner)?;
        Ok(())
    })());
}
