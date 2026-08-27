//! Bounded Publint runner over exact package publication views.

use std::{path::PathBuf, process::ExitCode, sync::Arc};

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    publint_all::{
        PublintProcess, PublintStatus, publint_concurrency, render_publint_stderr,
        render_publint_stdout, run_all, run_publint, workspace_packages,
    },
};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("publint-all: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<u8> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let root = parse_packages_root(&arguments)?;
    let targets = workspace_packages(&root)?;
    let environment = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    let concurrency = publint_concurrency(
        targets.len(),
        &environment,
        std::thread::available_parallelism()?.get(),
    )?;
    println!(
        "publint-all: linting {} package(s) with {concurrency} worker(s).",
        targets.len()
    );
    let process = Arc::new(PublintProcess::from_process()?);
    let results = run_all(targets, concurrency, move |target| {
        run_publint(target, &process)
    })?;
    for result in &results {
        print!("{}", render_publint_stdout(result));
        eprint!("{}", render_publint_stderr(result));
    }
    Ok(u8::from(
        results
            .iter()
            .any(|result| result.status == PublintStatus::Failed),
    ))
}

fn parse_packages_root(arguments: &[String]) -> anyhow::Result<PathBuf> {
    let mut selected = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--packages-root" {
            index += 1;
            selected = Some(
                arguments
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--packages-root requires a value"))?
                    .clone(),
            );
        } else if let Some(value) = argument.strip_prefix("--packages-root=") {
            selected = Some(value.to_owned());
        } else {
            anyhow::bail!("unknown argument {argument:?}");
        }
        index += 1;
    }
    Ok(selected.map_or_else(|| compiled_repository_root().to_owned(), PathBuf::from))
}
