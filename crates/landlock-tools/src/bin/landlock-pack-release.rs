//! Pack native platform packages and their entry bindings in publication order.

use seekdeep_landlock_tools::{
    process::NativeRunner,
    release::{PackOptions, cli_repository_args, finish_cli, pack_release, resolve_cli_path},
    repo::host_platform,
};

fn main() {
    finish_cli((|| {
        let (repository, args) = cli_repository_args()?;
        let options = PackOptions {
            destination: resolve_cli_path(
                args.iter()
                    .find(|arg| !arg.starts_with("--"))
                    .map(String::as_str),
                &repository.root.join("dist/npm"),
            )?,
            current_platform_only: args.iter().any(|arg| arg == "--current-platform-only"),
            host_platform: host_platform(),
        };
        pack_release(&repository, &options, &mut NativeRunner)?;
        Ok(())
    })());
}
