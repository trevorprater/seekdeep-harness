//! Native release metadata and wheel build entry point.

use std::{
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use clap::{Parser, Subcommand};
use seekdeep_python_release::{
    Package, expected_wheel, hook, load_platforms, load_platforms_snapshot, pep440_version,
    repository_version, staging, validate_release_tag, wheel,
};

#[derive(Parser)]
struct Args {
    /// Workspace root containing package.json and python/.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print repository and PEP 440 versions without building artifacts.
    Version {
        /// Emit lines suitable for `GITHUB_OUTPUT`.
        #[arg(long)]
        github_output: bool,
    },
    /// Stage and build one release-shaped wheel, then verify its metadata and payload.
    Build {
        #[arg(long, value_enum)]
        package: Package,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        runtime_exe: Option<PathBuf>,
    },
    /// Execute the runtime build-hook policy for the generated Hatch binding.
    Hook {
        #[arg(long)]
        version: String,
        #[arg(long)]
        target: String,
    },
    /// Validate and serialize the platform manifest at binding import time.
    HookSnapshot,
    /// Validate a previously built wheel.
    Verify {
        #[arg(long, value_enum)]
        package: Package,
        #[arg(long)]
        version: String,
        #[arg(long)]
        platform: Option<String>,
        wheel: PathBuf,
    },
}

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    let root = args.root.canonicalize()?;
    match args.command {
        Command::Version { github_output } => {
            let repository = repository_version(&root)?;
            let version = pep440_version(&repository)?;
            if github_output {
                println!("repository-version={repository}\nversion={version}");
            } else {
                println!(
                    "{}",
                    serde_json::json!({"repositoryVersion":repository,"version":version})
                );
            }
        }
        Command::Build {
            package,
            tag,
            output_dir,
            platform,
            runtime_exe,
        } => build(
            &root,
            package,
            tag.as_deref(),
            &output_dir,
            platform.as_deref(),
            runtime_exe.as_deref(),
        )?,
        Command::Hook { version, target } => {
            let system = if cfg!(target_os = "macos") {
                "Darwin"
            } else {
                std::env::consts::OS
            };
            let requested_tag = std::env::var("SEEKDEEP_RUNTIME_PLATFORM_TAG").ok();
            let result =
                if let Ok(snapshot) = std::env::var("SEEKDEEP_INTERNAL_RUNTIME_PLATFORMS_JSON") {
                    let platforms =
                        load_platforms_snapshot(&root.join("platforms.json"), snapshot.as_bytes())?;
                    hook::initialize_with_platforms(
                        &root,
                        &platforms,
                        &version,
                        &target,
                        requested_tag.as_deref(),
                        system,
                        std::env::consts::ARCH,
                    )?
                } else {
                    hook::initialize(
                        &root,
                        &version,
                        &target,
                        requested_tag.as_deref(),
                        system,
                        std::env::consts::ARCH,
                    )?
                };
            println!("{result}");
        }
        Command::HookSnapshot => {
            let platforms = load_platforms(&root.join("platforms.json"))?;
            println!("{}", serde_json::to_string(&platforms)?);
        }
        Command::Verify {
            package,
            version,
            platform,
            wheel: path,
        } => {
            let platforms = load_platforms(&root.join("python/sdk-runtime/platforms.json"))?;
            let platform = platform
                .as_ref()
                .map(|name| {
                    platforms
                        .get(name)
                        .ok_or_else(|| anyhow::anyhow!("unknown runtime platform {name}"))
                })
                .transpose()?;
            wheel::verify_wheel(&path, package, &version, platform)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}

fn build(
    root: &Path,
    package: Package,
    tag: Option<&str>,
    output: &Path,
    platform: Option<&str>,
    executable: Option<&Path>,
) -> anyhow::Result<()> {
    let platforms = load_platforms(&root.join("python/sdk-runtime/platforms.json"))?;
    let repository = repository_version(root)?;
    validate_release_tag(tag, &repository)?;
    let version = pep440_version(&repository)?;
    let selected = match package {
        Package::Sdk => {
            anyhow::ensure!(
                platform.is_none() && executable.is_none(),
                "SDK builds do not accept --platform or --runtime-exe"
            );
            None
        }
        Package::Runtime => {
            anyhow::ensure!(
                platform.is_some() && executable.is_some(),
                "runtime builds require --platform and --runtime-exe"
            );
            Some(
                platforms
                    .get(platform.expect("validated platform"))
                    .ok_or_else(|| {
                        anyhow::anyhow!("unknown runtime platform {}", platform.unwrap_or_default())
                    })?,
            )
        }
    };
    let output = if output.is_absolute() {
        output.to_owned()
    } else {
        std::env::current_dir()?.join(output)
    };
    std::fs::create_dir_all(&output)?;
    let temporary = tempfile::Builder::new()
        .prefix("seekdeep-python-release-")
        .tempdir()?;
    let staged = temporary.path().join(match package {
        Package::Sdk => "sdk",
        Package::Runtime => "runtime",
    });
    match selected {
        None => staging::stage_sdk(root, &staged, &version)?,
        Some(platform) => staging::stage_runtime(
            root,
            &staged,
            &version,
            &executable.expect("validated executable").canonicalize()?,
            &platform.executable,
        )?,
    }
    let namespace = match package {
        Package::Sdk => "deepseek_harness",
        Package::Runtime => "deepseek_harness_runtime",
    };
    anyhow::ensure!(
        staged
            .join("src")
            .join(namespace)
            .join("__init__.py")
            .is_file(),
        "{} has no generated Python binding entry point; refusing to build an empty carrier",
        root.join("python").join(package.directory()).display()
    );
    let mut command = ProcessCommand::new("uv");
    command
        .args(["build", "--wheel", "--out-dir"])
        .arg(&output)
        .arg(&staged)
        .current_dir(root);
    if let Some(platform) = selected {
        command
            .env("SEEKDEEP_RUNTIME_PLATFORM_TAG", &platform.tag)
            .env("SEEKDEEP_PYTHON_RELEASE_TOOL", std::env::current_exe()?);
    }
    let status = command.status()?;
    anyhow::ensure!(status.success(), "uv wheel build failed with {status}");
    let expected = expected_wheel(&output, package, &version, selected);
    anyhow::ensure!(
        expected.is_file(),
        "build did not produce expected wheel: {}",
        expected.display()
    );
    wheel::verify_wheel(&expected, package, &version, selected)?;
    println!("{}", expected.display());
    Ok(())
}
