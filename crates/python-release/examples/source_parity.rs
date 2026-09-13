//! Live source comparison for release versions, tags, and platform manifest diagnostics.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use seekdeep_python_release::{
    load_platforms, pep440_version, repository_version, validate_release_tag,
};
use serde_json::{Value, json};

const ORACLE: &str = r#"
import json, pathlib, runpy, sys
release = runpy.run_path(str(pathlib.Path(sys.argv[1]) / "scripts/build-python-release.py"))
results = []
for case in json.load(sys.stdin):
    try:
        if case["kind"] == "pep":
            value = release["pep440_version"](case["value"])
        elif case["kind"] == "tag":
            value = release["validate_release_tag"](case.get("tag"), case["value"])
        elif case["kind"] == "repository":
            value = release["repository_version"](pathlib.Path(case["path"]))
        else:
            value = release["load_platforms"](pathlib.Path(case["path"]))
        results.append({"ok": value})
    except (ValueError, RuntimeError) as error:
        results.append({"error": str(error)})
print(json.dumps(results))
"#;

fn main() -> anyhow::Result<()> {
    let source = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("pinned source root required"))?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin missing"))?;
    anyhow::ensure!(
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == pin,
        "oracle differs from SOURCE_SNAPSHOT"
    );
    let temporary = tempfile::tempdir()?;
    let cases = cases(temporary.path())?;
    let mut child = Command::new("python3")
        .args(["-c", ORACLE])
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("oracle stdin missing"))?
        .write_all(&serde_json::to_vec(&cases)?)?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(expected.len() == cases.len(), "source case count differs");
    for (index, (case, expected)) in cases.iter().zip(expected).enumerate() {
        let actual = match native(case) {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"error":error.to_string()}),
        };
        anyhow::ensure!(actual == expected, "case {index}: {actual} != {expected}");
    }
    println!(
        "{} release metadata cases match the pinned Python source",
        cases.len()
    );
    Ok(())
}

fn native(case: &Value) -> anyhow::Result<Value> {
    match case["kind"].as_str().unwrap_or_default() {
        "pep" => Ok(json!(pep440_version(
            case["value"].as_str().unwrap_or_default()
        )?)),
        "tag" => {
            validate_release_tag(
                case["tag"].as_str(),
                case["value"].as_str().unwrap_or_default(),
            )?;
            Ok(Value::Null)
        }
        "repository" => Ok(json!(repository_version(Path::new(
            case["path"].as_str().unwrap_or_default()
        ))?)),
        "platforms" => {
            let platforms = load_platforms(Path::new(case["path"].as_str().unwrap_or_default()))?;
            Ok(Value::Object(
                platforms
                    .into_iter()
                    .map(|(name, platform)| (name, json!([platform.tag, platform.executable])))
                    .collect(),
            ))
        }
        _ => anyhow::bail!("unknown fixture"),
    }
}

fn cases(root: &Path) -> anyhow::Result<Vec<Value>> {
    let mut cases = Vec::new();
    for value in [
        "1.2.3",
        "1.2.3-rc.1",
        "1.2.3-alpha.2",
        "1.2.3-beta.10",
        "1.2.3-a1",
        "1.2.3-b2",
        "1.2.3-c3",
        "1.2.3-pre4",
        "1.2.3-preview.5",
        "1.2.3-rc.01",
        "1.2.3-rc.١",
        "1.2.3-nightly",
        "1.2.3-rc",
        "1.2.3-rc.1.2",
        "1.2.3-",
        "",
        "1.2.3-RC.1",
        "1.2.3-rc.1\n",
        "1.2.3-\u{0000}\u{0008}\u{000b}\u{000c}\u{007f}",
        "1.2.3-\u{0085}\u{00a0}\u{200b}\u{2028}\u{e000}\u{f0000}",
    ] {
        cases.push(json!({"kind":"pep","value":value}));
    }
    for tag in [
        None,
        Some("python-v1.2.3"),
        Some("python-v1.2.4"),
        Some(""),
        Some("python-v1.2.3'\""),
    ] {
        cases.push(json!({"kind":"tag","value":"1.2.3","tag":tag}));
    }
    for (index, value) in [
        json!("1.2.3"),
        json!("1.2.3-rc.1"),
        json!("01.2.3"),
        json!("١.٢.٣"),
        json!("v1.2"),
        json!("1.2.3+build"),
        json!(null),
        json!(42),
        json!(false),
        json!([1, 2]),
        json!({"bad":"version"}),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = root.join(format!("version-{index}"));
        std::fs::create_dir(&directory)?;
        std::fs::write(
            directory.join("package.json"),
            serde_json::to_vec(&json!({"version":value}))?,
        )?;
        cases.push(json!({"kind":"repository","path":directory}));
    }
    let missing = root.join("missing");
    std::fs::create_dir(&missing)?;
    cases.push(json!({"kind":"repository","path":missing}));
    for (index, value) in [json!({"linux-x64":{"tag":"manylinux_2_28_x86_64","executable":"seekdeep-jsonrpc-agent-pkg-linux-x64"}}),json!({}),json!([]),json!({"x":{"tag":"t"}}),json!({"x":{"tag":1,"executable":"e"}}),json!({"x":{"tag":"t","executable":"e","extra":true}})].into_iter().enumerate() {
        let path = root.join(format!("platforms-{index}.json")); std::fs::write(&path, serde_json::to_vec(&value)?)?;
        cases.push(json!({"kind":"platforms","path":path}));
    }
    Ok(cases)
}
