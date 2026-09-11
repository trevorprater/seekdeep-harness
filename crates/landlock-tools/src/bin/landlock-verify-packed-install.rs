//! Verify a local tarball consumer and the installed launcher boundary.

use seekdeep_landlock_tools::{
    process::NativeRunner,
    release::{
        PackedInstallOptions, cli_repository_args, finish_cli, resolve_cli_path,
        verify_packed_install,
    },
    repo::host_platform,
};

fn main() {
    finish_cli((|| {
        let (repository, args) = cli_repository_args()?;
        let options = PackedInstallOptions {
            tarball_dir: resolve_cli_path(
                args.iter()
                    .find(|arg| !arg.starts_with("--"))
                    .map(String::as_str),
                &repository.root.join("dist/npm"),
            )?,
            current_platform_only: args.iter().any(|arg| arg == "--current-platform-only"),
            host_platform: host_platform(),
            require_landlock: std::env::var("NALR_REQUIRE_LANDLOCK")
                .is_ok_and(|value| value == "1"),
        };
        verify_packed_install(&repository, &options, &mut NativeRunner)?;
        Ok(())
    })());
}
