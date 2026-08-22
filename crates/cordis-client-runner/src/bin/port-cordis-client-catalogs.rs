//! Regenerates Client Cordis catalogs from the pinned source checkout.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn main() -> anyhow::Result<()> {
    let target = std::env::current_dir()?;
    let source = std::env::args_os().nth(1).map_or_else(
        || target.parent().unwrap_or(&target).join("deepseek-harness"),
        PathBuf::from,
    );
    verify_source(&target, &source)?;
    let output = target.join("crates/cordis-client-runner/data");
    std::fs::create_dir_all(&output)?;

    let raw = run_tsx(
        &source,
        concat!(
            "import {SERVICE_API,EVENT_API,TYPE_API,INHERITED_CTX_API} from ",
            "'./packages/extensions/cordis-client-runner/src/client/api-catalog.ts';",
            "console.log(JSON.stringify({services:SERVICE_API,events:EVENT_API,",
            "types:TYPE_API,inheritedContext:INHERITED_CTX_API}))",
        ),
    )?;
    write_json(&output.join("api-catalog.json"), &raw)?;
    let fixtures = run_tsx(
        &source,
        concat!(
            "import {SERVICE_API,EVENT_API,queryServiceApi,queryEventApi} from ",
            "'./packages/extensions/cordis-client-runner/src/client/api-catalog.ts';",
            "console.log(JSON.stringify({serviceCatalog:queryServiceApi(),",
            "services:Object.fromEntries(SERVICE_API.map(x=>[x.key,queryServiceApi(x.key)])),",
            "eventCatalog:queryEventApi(),events:Object.fromEntries(",
            "EVENT_API.map(x=>[x.name,queryEventApi(x.name)]))}))",
        ),
    )?;
    write_json(&output.join("api-query-fixtures.json"), &fixtures)?;
    let slots = run_tsx(
        &source,
        concat!(
            "import {CLIENT_NOTES,CLIENT_SLOT_API} from ",
            "'./packages/extensions/cordis-client-runner/src/client/slot-catalog.ts';",
            "console.log(JSON.stringify({notes:CLIENT_NOTES,entries:CLIENT_SLOT_API}))",
        ),
    )?;
    write_json(&output.join("slot-catalog.json"), &slots)?;
    Ok(())
}

fn verify_source(target: &Path, source: &Path) -> anyhow::Result<()> {
    let snapshot = std::fs::read_to_string(target.join("SOURCE_SNAPSHOT"))?;
    let expected = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("SOURCE_SNAPSHOT has no commit field"))?;
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(source)
        .output()?;
    anyhow::ensure!(output.status.success(), "git rev-parse failed");
    let actual = String::from_utf8(output.stdout)?.trim().to_owned();
    anyhow::ensure!(
        actual == expected,
        "source checkout is {actual}, expected {expected}"
    );
    Ok(())
}

fn run_tsx(source: &Path, program: &str) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("pnpm")
        .args(["exec", "tsx", "-e", program])
        .current_dir(source)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "pinned source catalog extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn write_json(path: &Path, raw: &[u8]) -> anyhow::Result<()> {
    let value: serde_json::Value = serde_json::from_slice(raw)?;
    let mut rendered = serde_json::to_string_pretty(&value)?;
    rendered.push('\n');
    std::fs::write(path, rendered)?;
    Ok(())
}
