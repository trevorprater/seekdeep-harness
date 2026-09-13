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

    write_ui_catalogs(&target, &source)?;
    write_host_catalogs(&target, &source)
}

fn write_host_catalogs(target: &Path, source: &Path) -> anyhow::Result<()> {
    let host_output = target.join("crates/tool-cordis/data");
    std::fs::create_dir_all(&host_output)?;
    let host_catalog = run_tsx(
        source,
        concat!(
            "import {SERVICE_API,EVENT_API,TYPE_API,INHERITED_CTX_API} from ",
            "'./packages/extensions/tool-cordis/src/api-catalog.ts';",
            "console.log(JSON.stringify({services:SERVICE_API,events:EVENT_API,",
            "types:TYPE_API,inheritedContext:INHERITED_CTX_API}))",
        ),
    )?;
    write_json(
        &host_output.join("api-catalog.json"),
        &target_identity(&host_catalog)?,
    )?;
    let host_fixtures = run_tsx(
        source,
        concat!(
            "import {SERVICE_API,EVENT_API,queryServiceApi,queryEventApi} from ",
            "'./packages/extensions/tool-cordis/src/api-catalog.ts';",
            "console.log(JSON.stringify({serviceCatalog:queryServiceApi(),",
            "services:Object.fromEntries(SERVICE_API.map(x=>[x.key,queryServiceApi(x.key)])),",
            "eventCatalog:queryEventApi(),events:Object.fromEntries(",
            "EVENT_API.map(x=>[x.name,queryEventApi(x.name)])),",
            "hostEventCatalog:queryEventApi(undefined,EVENT_API.filter(x=>!x.name.startsWith('cordis/'))),",
            "hostEvents:Object.fromEntries(EVENT_API.filter(x=>!x.name.startsWith('cordis/')).map(",
            "x=>[x.name,queryEventApi(x.name,EVENT_API.filter(y=>!y.name.startsWith('cordis/')))]))}))",
        ),
    )?;
    write_json(
        &host_output.join("api-query-fixtures.json"),
        &target_identity(&host_fixtures)?,
    )?;
    let prompt = run_tsx(
        source,
        concat!(
            "import {CORDIS_SYSTEM_PROMPT} from ",
            "'./packages/extensions/tool-cordis/src/prompt.ts';",
            "process.stdout.write(CORDIS_SYSTEM_PROMPT)",
        ),
    )?;
    write_text(
        &host_output.join("system-prompt.txt"),
        &target_identity(&prompt)?,
    )?;
    let tool_definitions = run_tsx(
        source,
        concat!(
            "import {apply} from './packages/extensions/tool-cordis/src/index.ts';",
            "const definitions=[];const sections=[];",
            "const ctx={systemPrompt:{section:x=>sections.push(x)},",
            "cordisInspect:{register:()=>()=>{}},",
            "tools:{register:x=>{definitions.push(x);return ()=>{}},schemas:()=>[]},",
            "dynamicCordisRunner:{},effect:f=>f(),on:()=>()=>{}};",
            "apply(ctx);console.log(JSON.stringify({sections,definitions:definitions.map(x=>({",
            "name:x.name,description:x.description,parameters:x.parameters,",
            "outputSchema:x.output.schema}))}))",
        ),
    )?;
    write_json(
        &host_output.join("tool-definitions.json"),
        &target_identity(&tool_definitions)?,
    )?;
    Ok(())
}

fn target_identity(raw: &[u8]) -> anyhow::Result<Vec<u8>> {
    Ok(String::from_utf8(raw.to_vec())?
        .replace("DSH Node.js process", "SeekDeep Harness Host process")
        .replace("DSH process", "SeekDeep Harness process")
        .replace("DSH objects", "SeekDeep Harness objects")
        .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("dsh-", "seekdeep-")
        .replace("DshEnvironment", "SeekdeepEnvironment")
        .replace("DSH_", "SEEKDEEP_")
        .into_bytes())
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

fn write_ui_catalogs(target: &Path, source: &Path) -> anyhow::Result<()> {
    let output = target.join("crates/client-ui-cordis/data");
    std::fs::create_dir_all(&output)?;
    let locales = run_tsx(
        source,
        concat!(
            "import {NS,en,zh} from ",
            "'./packages/extensions/ui-cordis/src/client/locales.ts';",
            "console.log(JSON.stringify({namespace:NS,en,zh}))",
        ),
    )?;
    write_json(&output.join("locales.json"), &locales)?;
    let mut styles = String::new();
    for (name, prefix) in [
        ("CordisDefineRow.module.css", "seekdeep-cordis-define-"),
        ("CordisRunRow.module.css", "seekdeep-cordis-run-"),
        ("CordisPanel.module.css", "seekdeep-cordis-panel-"),
    ] {
        let css = std::fs::read_to_string(
            source
                .join("packages/extensions/ui-cordis/src/client")
                .join(name),
        )?;
        styles.push_str(&scope_css_classes(&css, prefix));
        styles.push('\n');
    }
    write_text(&output.join("styles.css"), styles.as_bytes())
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

fn write_text(path: &Path, raw: &[u8]) -> anyhow::Result<()> {
    let mut text = String::from_utf8(raw.to_vec())?;
    while text.ends_with('\n') {
        text.pop();
    }
    text.push('\n');
    std::fs::write(path, text)?;
    Ok(())
}

fn scope_css_classes(source: &str, prefix: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(source.len() + source.len() / 8);
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'.'
            || bytes
                .get(at + 1)
                .is_none_or(|byte| !byte.is_ascii_alphabetic() && *byte != b'_')
        {
            output.push(bytes[at]);
            at += 1;
            continue;
        }
        output.push(b'.');
        output.extend_from_slice(prefix.as_bytes());
        at += 1;
        while at < bytes.len()
            && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'-'))
        {
            output.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(output).expect("scoping UTF-8 CSS preserves every source byte")
}

#[cfg(test)]
mod tests {
    use super::{scope_css_classes, target_identity};

    #[test]
    fn host_catalog_identity_matches_the_target_public_surface() {
        let source = b"@deepseek-ai/dsh-scope dsh-tools DSH_ENV_PREFIX \
            __DSH_BOOT__ DshEnvironmentKey DSH Node.js process DSH objects";
        assert_eq!(
            String::from_utf8(target_identity(source).unwrap()).unwrap(),
            "@seekdeep-ai/seekdeep-scope seekdeep-tools SEEKDEEP_ENV_PREFIX \
            __SEEKDEEP_BOOT__ SeekdeepEnvironmentKey SeekDeep Harness Host process \
            SeekDeep Harness objects"
        );
    }

    #[test]
    fn css_scoping_renames_classes_without_touching_decimals_or_utf8() {
        assert_eq!(
            scope_css_classes(
                ".card .row:hover { letter-spacing: 0.04em; } /* 终端 .card */",
                "cordis-",
            ),
            ".cordis-card .cordis-row:hover { letter-spacing: 0.04em; } /* 终端 .cordis-card */"
        );
    }
}
