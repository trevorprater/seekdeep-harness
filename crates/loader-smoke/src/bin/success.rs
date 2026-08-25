//! Successful subprocess fixture for the Loader-smoke harness.

use std::io::Read as _;

fn main() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let seekdeep_home = std::env::var_os(seekdeep_util::home_paths::SEEKDEEP_HOME_ENV)
        .map(|value| value.to_string_lossy().into_owned());
    let agents_home =
        std::env::var_os("SEEKDEEP_AGENTS_HOME").map(|value| value.to_string_lossy().into_owned());
    let marker =
        std::env::var_os("LOADER_SMOKE_MARKER").map(|value| value.to_string_lossy().into_owned());
    println!(
        "{}",
        serde_json::json!({
            "configPath": std::env::args().nth(1),
            "args": std::env::args().skip(1).collect::<Vec<_>>(),
            "cwd": cwd,
            "seekdeepHome": seekdeep_home,
            "agentsHome": agents_home,
            "marker": marker,
            "input": input,
        })
    );
    eprintln!("fixture stderr");
    Ok(())
}
