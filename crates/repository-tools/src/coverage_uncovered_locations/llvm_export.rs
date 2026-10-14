//! Native LLVM response files keep complete workspace exports below process argument limits.

use std::{
    ffi::OsString,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Context as _;
use walkdir::WalkDir;

pub(super) fn export(repository: &Path, instrumented: &Path, output: &Path) -> anyhow::Result<()> {
    let (profiles, objects) = inputs(instrumented)?;
    anyhow::ensure!(
        !profiles.is_empty(),
        "run-coverage: no instrumented profiles in {}",
        instrumented.display()
    );
    anyhow::ensure!(
        !objects.is_empty(),
        "run-coverage: no native coverage objects in {}",
        instrumented.display()
    );
    let destination = output
        .parent()
        .context("coverage output needs a parent directory")?;
    let profdata = destination.join("workspace.profdata");
    let mut merge = vec![
        OsString::from("-sparse"),
        OsString::from("-o"),
        profdata.clone().into_os_string(),
    ];
    merge.extend(profiles.into_iter().map(PathBuf::into_os_string));
    run_llvm(
        repository,
        &llvm_tool(repository, "llvm-profdata", "LLVM_PROFDATA")?,
        "merge",
        &merge,
        destination,
        None,
    )?;
    let mut export = vec![
        OsString::from("-format=text"),
        OsString::from("-instr-profile"),
        profdata.into_os_string(),
    ];
    for object in &objects {
        export.push("-object".into());
        export.push(object.as_os_str().to_owned());
    }
    run_llvm(
        repository,
        &llvm_tool(repository, "llvm-cov", "LLVM_COV")?,
        "export",
        &export,
        destination,
        Some(output),
    )?;
    println!(
        "run-coverage: exported {} native objects through LLVM response files",
        objects.len()
    );
    Ok(())
}

fn inputs(root: &Path) -> anyhow::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut profiles = Vec::new();
    let mut objects = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_entry(|entry| {
        !entry.file_type().is_dir()
            || !matches!(
                entry.file_name().to_str(),
                Some(
                    "build" | "incremental" | ".fingerprint" | "node_modules" | "code-runtime-node"
                )
            )
    }) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "profraw")
        {
            profiles.push(path.to_owned());
        } else if is_object(path) {
            objects.push(path.to_owned());
        }
    }
    profiles.sort();
    objects.sort();
    Ok((profiles, objects))
}

#[cfg(windows)]
fn is_object(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("dll")
    })
}

#[cfg(unix)]
fn is_object(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(any(unix, windows)))]
fn is_object(_path: &Path) -> bool {
    false
}

fn llvm_tool(repository: &Path, name: &str, override_name: &str) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(override_name) {
        return Ok(path.into());
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "target-libdir"])
        .current_dir(repository)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "run-coverage: rustc could not locate LLVM tools"
    );
    let library = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let tool = library
        .parent()
        .context("Rust target library directory has no parent")?
        .join("bin")
        .join(name);
    anyhow::ensure!(
        tool.is_file(),
        "run-coverage: LLVM tool is missing at {}; install llvm-tools-preview",
        tool.display()
    );
    Ok(tool)
}

fn run_llvm(
    repository: &Path,
    tool: &Path,
    operation: &str,
    arguments: &[OsString],
    directory: &Path,
    output: Option<&Path>,
) -> anyhow::Result<()> {
    let mut response = tempfile::Builder::new()
        .prefix("llvm-arguments-")
        .suffix(".rsp")
        .tempfile_in(directory)?;
    for argument in arguments {
        let argument = argument
            .to_str()
            .context("LLVM arguments must be Unicode")?;
        let argument = if cfg!(windows) {
            argument.replace('"', "\\\"")
        } else {
            argument.replace('\\', "\\\\").replace('"', "\\\"")
        };
        writeln!(response, "\"{argument}\"")?;
    }
    response.flush()?;
    let mut response_arg = OsString::from("@");
    response_arg.push(response.path());
    let mut command = Command::new(tool);
    command
        .arg(operation)
        .arg(response_arg)
        .current_dir(repository)
        .stdin(Stdio::null());
    if let Some(output) = output {
        command.stdout(std::fs::File::create(output)?);
    }
    let status = command
        .status()
        .with_context(|| format!("run-coverage: execute {} {operation}", tool.display()))?;
    anyhow::ensure!(
        status.success(),
        "run-coverage: {} {operation} exited with {status}",
        tool.display()
    );
    Ok(())
}
