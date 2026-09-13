//! Repository Mermaid fence extraction and pinned-parser orchestration.

use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

use crate::{
    markdown_util::markdown_fences,
    repo_files::{archived_agent_note_path, unique_repo_files},
};

const PATTERNS: &[&str] = &[
    "README.md",
    "README.zh.md",
    ".agents/notes/**/*.md",
    "docs/**/*.md",
    "packages/*/*.md",
    "packages/*/*/*.md",
    "examples/**/*.md",
    "AGENTS.md",
    "packages/AGENTS.md",
    ".agents/skills/**/*.md",
];

/// One authored Mermaid code fence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MermaidBlock {
    /// Repository-relative Markdown file.
    pub file: String,
    /// Opening fence line.
    pub line: usize,
    /// Mermaid source body.
    pub source: String,
}

/// One Mermaid parser violation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidViolation {
    /// Repository-relative Markdown file.
    pub file: String,
    /// Opening fence line.
    pub line: usize,
    /// Normalized parser diagnostic.
    pub message: String,
}

/// Full Mermaid gate report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MermaidReport {
    /// Unique Markdown files checked.
    pub checked_files: usize,
    /// Mermaid blocks parsed.
    pub blocks: usize,
    /// Parser failures.
    pub violations: Vec<MermaidViolation>,
}

/// Extracts every in-scope Mermaid fence.
///
/// # Errors
///
/// Returns traversal, path, read, or Markdown parser failures.
pub fn collect_mermaid_blocks(root: &Path) -> anyhow::Result<(usize, Vec<MermaidBlock>)> {
    let files = unique_repo_files(root, PATTERNS, archived_agent_note_path)?;
    let mut blocks = Vec::new();
    for file in &files {
        let relative = file
            .absolute
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        for fence in markdown_fences(&std::fs::read_to_string(&file.absolute)?)
            .map_err(anyhow::Error::msg)?
        {
            if fence.lang.as_deref() == Some("mermaid") {
                blocks.push(MermaidBlock {
                    file: relative.clone(),
                    line: fence.line,
                    source: fence.code,
                });
            }
        }
    }
    Ok((files.len(), blocks))
}

/// Runs the complete gate through Mermaid's own parser in Node.
///
/// # Errors
///
/// Returns discovery, staging, Node, dependency, protocol, or JSON failures.
pub fn verify_mermaid(root: &Path) -> anyhow::Result<MermaidReport> {
    verify_mermaid_with(root, |blocks| parse_with_node(root, blocks))
}

/// Runs the gate through an injected ordered parser result boundary.
///
/// # Errors
///
/// Returns discovery, parser, or result-cardinality failures.
pub fn verify_mermaid_with(
    root: &Path,
    mut parser: impl FnMut(&[MermaidBlock]) -> anyhow::Result<Vec<Option<String>>>,
) -> anyhow::Result<MermaidReport> {
    let (checked_files, blocks) = collect_mermaid_blocks(root)?;
    let results = parser(&blocks)?;
    if results.len() != blocks.len() {
        anyhow::bail!(
            "verify-mermaid: parser returned {} result(s) for {} block(s)",
            results.len(),
            blocks.len()
        );
    }
    let violations = blocks
        .iter()
        .zip(results)
        .filter_map(|(block, message)| {
            message.map(|message| MermaidViolation {
                file: block.file.clone(),
                line: block.line,
                message: normalize_message(&message),
            })
        })
        .collect();
    Ok(MermaidReport {
        checked_files,
        blocks: blocks.len(),
        violations,
    })
}

/// Renders the source-compatible report.
#[must_use]
pub fn render_mermaid_report(report: &MermaidReport) -> String {
    if report.violations.is_empty() {
        return format!(
            "verify-mermaid: {} mermaid block(s) parsed across {} file(s).\n",
            report.blocks, report.checked_files
        );
    }
    let mut output = "verify-mermaid: Mermaid syntax errors found:\n".to_owned();
    for violation in &report.violations {
        let _ = writeln!(
            output,
            "  {}:{}  {}",
            violation.file, violation.line, violation.message
        );
    }
    output
}

fn parse_with_node(root: &Path, blocks: &[MermaidBlock]) -> anyhow::Result<Vec<Option<String>>> {
    let temporary = MermaidBridge::new(root)?;
    std::fs::write(
        &temporary.script,
        "import { JSDOM } from 'jsdom';\nconst { window } = new JSDOM('');\nObject.defineProperty(globalThis,'window',{value:window});\nObject.defineProperty(globalThis,'document',{value:window.document});\nObject.defineProperty(globalThis,'navigator',{value:window.navigator});\nconst mermaid=(await import('mermaid')).default;\nmermaid.initialize({startOnLoad:false,maxEdges:2000});\nlet text=''; for await (const chunk of process.stdin) text += chunk;\nconst blocks=JSON.parse(text); const results=[];\nfor (const block of blocks) { try { await mermaid.parse(block.source,{suppressErrors:false}); results.push(null); } catch (error) { results.push(String(error instanceof Error ? error.message : error).replace(/\\s+/g,' ').trim()); } }\nprocess.stdout.write(JSON.stringify(results));\n",
    )?;
    let mut child = Command::new(node_executable())
        .arg(&temporary.script)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let input = serde_json::to_vec(blocks)?;
    let input_result = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(&input))
        .transpose();
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!(
            "verify-mermaid: parser bridge failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    input_result?;
    Ok(serde_json::from_slice(&output.stdout)?)
}

struct MermaidBridge {
    directory: PathBuf,
    script: PathBuf,
}

impl MermaidBridge {
    fn new(root: &Path) -> anyhow::Result<Self> {
        for attempt in 0..1_000_u16 {
            let directory = root.join(format!(
                ".seekdeep-mermaid-{}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&directory) {
                Ok(()) => {
                    let script = directory.join("parse.mjs");
                    return Ok(Self { directory, script });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("verify-mermaid: could not allocate parser bridge directory")
    }
}

impl Drop for MermaidBridge {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn normalize_message(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn node_executable() -> std::ffi::OsString {
    std::env::var_os("npm_node_execpath").unwrap_or_else(|| "node".into())
}
