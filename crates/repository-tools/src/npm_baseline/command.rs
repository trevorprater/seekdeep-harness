use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use super::{
    BaselinePackOptions, BaselineRunner, DEFAULT_OUTPUT_DIRECTORY, DEFAULT_REGISTRY,
    RegistryPublication, ReleaseBundle, pack_baseline, plan_baseline, run_options, strings,
};

/// Source command usage with the Rust-owned repository entry point.
#[must_use]
pub fn usage() -> String {
    format!(
        "Usage:\n  pnpm run publish:npm-baseline pack [options]\n  pnpm run publish:npm-baseline release [options] [--yes]\n  pnpm run publish:npm-baseline publish --manifest <path> [--yes]\n  pnpm run publish:npm-baseline verify --manifest <path>\n\nPack/release options:\n  --ref <git-ref>       Git commit to stage (default: HEAD)\n  --registry <url>      npm registry (default: {DEFAULT_REGISTRY})\n  --output-dir <path>   Artifact root (default: {DEFAULT_OUTPUT_DIRECTORY})\n  --yes                 pack/release without waiting for Enter"
    )
}

/// Dispatches the source command contract through injected process and clock boundaries.
///
/// # Errors
/// Returns argument, repository, pack, bundle, publication, and verification failures.
pub fn baseline_main(
    arguments: &[String],
    cwd: &Path,
    environment: &BTreeMap<OsString, OsString>,
    now: chrono::DateTime<chrono::Utc>,
    runner: &mut impl BaselineRunner,
) -> anyhow::Result<()> {
    let Some(command) = arguments.first().map(String::as_str) else {
        runner.log(&usage());
        return Ok(());
    };
    if matches!(command, "help" | "--help" | "-h")
        || arguments[1..]
            .iter()
            .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        runner.log(&usage());
        return Ok(());
    }
    let root = PathBuf::from(runner.capture(
        "git",
        &strings(&["rev-parse", "--show-toplevel"]),
        &run_options(cwd),
    )?);
    match command {
        "pack" | "release" => {
            let values = parse_arguments(
                &arguments[1..],
                &["ref", "registry", "output-dir"],
                &["yes"],
            )?;
            let output = values
                .get("output-dir")
                .map_or_else(|| root.join(DEFAULT_OUTPUT_DIRECTORY), PathBuf::from);
            let output = if output.is_absolute() {
                output
            } else {
                cwd.join(output)
            };
            let options = BaselinePackOptions {
                reference: values
                    .get("ref")
                    .cloned()
                    .unwrap_or_else(|| "HEAD".to_owned()),
                registry: values
                    .get("registry")
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_REGISTRY.to_owned()),
                output_directory: output,
            };
            let plan = plan_baseline(&root, &options, now, runner)?;
            let assume_yes = values.contains_key("yes");
            plan.confirm(runner, assume_yes)?;
            let bundle = pack_baseline(&root, &plan, environment, runner)?;
            if command == "release" {
                RegistryPublication::new(&bundle, &std::env::temp_dir(), environment)
                    .publish(runner, assume_yes)?;
            }
            Ok(())
        }
        "publish" | "verify" => {
            let values = parse_arguments(&arguments[1..], &["manifest"], &["yes"])?;
            let path = values
                .get("manifest")
                .ok_or_else(|| anyhow::anyhow!("{command} requires --manifest"))?;
            let assume_yes = values.contains_key("yes");
            if command == "verify" && assume_yes {
                anyhow::bail!("verify does not accept --yes");
            }
            let path = Path::new(path);
            let bundle = ReleaseBundle::load(
                &if path.is_absolute() {
                    path.to_owned()
                } else {
                    cwd.join(path)
                },
                runner,
            )?;
            let publication = RegistryPublication::new(&bundle, &std::env::temp_dir(), environment);
            if command == "publish" {
                publication.publish(runner, assume_yes)
            } else {
                publication.verify(runner)
            }
        }
        _ => anyhow::bail!("unknown command: {command}"),
    }
}

fn parse_arguments(
    arguments: &[String],
    string_options: &[&str],
    boolean_options: &[&str],
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            if let Some(argument) = arguments.next() {
                anyhow::bail!(
                    "Unexpected argument '{argument}'. This command does not take positional arguments"
                );
            }
            break;
        }
        let Some(option) = argument.strip_prefix("--") else {
            if argument.starts_with('-') {
                anyhow::bail!("Unknown option '{argument}'");
            }
            anyhow::bail!(
                "Unexpected argument '{argument}'. This command does not take positional arguments"
            );
        };
        let (name, inline) = option
            .split_once('=')
            .map_or((option, None), |(name, value)| (name, Some(value)));
        if boolean_options.contains(&name) {
            if inline.is_some() {
                anyhow::bail!("Option '--{name}' does not take an argument");
            }
            values.insert(name.to_owned(), "true".to_owned());
        } else if string_options.contains(&name) {
            let value = if let Some(inline) = inline {
                inline.to_owned()
            } else {
                let next = arguments
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("Option '--{name} <value>' argument missing"))?;
                if next.starts_with('-') && next != "-" {
                    anyhow::bail!(
                        "Option '--{name}' argument is ambiguous.\nDid you forget to specify the option argument for '--{name}'?\nTo specify an option argument starting with a dash use '--{name}=-XYZ'."
                    );
                }
                next.clone()
            };
            values.insert(name.to_owned(), value);
        } else {
            anyhow::bail!("Unknown option '--{name}'");
        }
    }
    Ok(values)
}
