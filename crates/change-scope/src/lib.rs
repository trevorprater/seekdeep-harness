//! Deterministic, read-only reporting of committed and worktree change scope.

use std::{
    collections::HashSet,
    ffi::OsString,
    path::Path,
    process::{Command, Output},
};

use serde::Serialize;

const FORMAT_VERSION: u32 = 1;
const MAX_GIT_OUTPUT: usize = 64 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangeScopeReport {
    format_version: u32,
    repository_root: String,
    input: ReportInput,
    resolved: ResolvedRefs,
    paths: ReportPaths,
}

#[derive(Debug, Serialize)]
struct ReportInput {
    base: String,
    head: String,
}

#[derive(Debug, Serialize)]
struct ResolvedRefs {
    #[serde(rename = "baseSha")]
    base: String,
    #[serde(rename = "headSha")]
    head: String,
    #[serde(rename = "mergeBaseSha")]
    merge_base: String,
}

#[derive(Debug, Serialize)]
struct ReportPaths {
    committed: Vec<String>,
    staged: Vec<String>,
    unstaged: Vec<String>,
    untracked: Vec<String>,
}

#[derive(Debug)]
struct Options {
    base: String,
    head: String,
}

#[derive(Debug)]
struct GitBytesResult {
    status: Option<i32>,
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    error: Option<String>,
}

#[derive(Debug)]
struct GitTextResult {
    status: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
    error: Option<String>,
}

/// Validates command arguments and renders one complete versioned report.
///
/// `cwd` may be any path inside the Git worktree. The report never changes
/// refs, configuration, the index, or worktree contents.
///
/// # Errors
///
/// Returns argument, Git spawn/ref/topology, output-bound, or strict UTF-8 failures.
pub fn render_change_scope(args: &[String], cwd: &Path) -> anyhow::Result<String> {
    let options = parse_options(args)?;
    let report = collect_report(&options, cwd)?;
    let mut output = serde_json::to_string_pretty(&report)?;
    output.push('\n');
    Ok(output)
}

fn parse_options(args: &[String]) -> anyhow::Result<Options> {
    let mut base = None;
    let mut head = "HEAD".to_owned();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if let Some(value) = argument.strip_prefix("--base=") {
            base = Some(value.to_owned());
        } else if let Some(value) = argument.strip_prefix("--head=") {
            value.clone_into(&mut head);
        } else if argument == "--" {
            if let Some(positional) = args.get(index + 1) {
                anyhow::bail!(
                    "Unexpected argument '{positional}'. This command does not take positional arguments"
                );
            }
            break;
        } else if argument == "--base" || argument == "--head" {
            let option = argument.as_str();
            index += 1;
            let Some(value) = args.get(index) else {
                anyhow::bail!("Option '{option} <value>' argument missing");
            };
            if value.starts_with('-') {
                anyhow::bail!(
                    "Option '{option}' argument is ambiguous.\nDid you forget to specify the option argument for '{option}'?\nTo specify an option argument starting with a dash use '{option}=-XYZ'."
                );
            }
            if option == "--base" {
                base = Some(value.clone());
            } else {
                head.clone_from(value);
            }
        } else if argument.starts_with('-') {
            anyhow::bail!("Unknown option '{argument}'");
        } else {
            anyhow::bail!(
                "Unexpected argument '{argument}'. This command does not take positional arguments"
            );
        }
        index += 1;
    }
    let base = base.ok_or_else(|| anyhow::anyhow!("missing required --base <ref>"))?;
    Ok(Options { base, head })
}

fn collect_report(options: &Options, cwd: &Path) -> anyhow::Result<ChangeScopeReport> {
    let root = strip_git_line_terminator(require_git(
        cwd,
        &["rev-parse".into(), "--show-toplevel".into()],
        "cannot locate a Git worktree",
    )?);
    let root_path = Path::new(&root);
    let base_sha = resolve_commit(root_path, "base", &options.base)?;
    let head_sha = resolve_commit(root_path, "head", &options.head)?;
    let merge_base_sha = resolve_merge_base(root_path, &base_sha, &head_sha)?;
    let paths = ReportPaths {
        committed: diff_paths(
            root_path,
            &[merge_base_sha.clone().into(), head_sha.clone().into()],
            "cannot inspect committed paths",
        )?,
        staged: diff_paths(
            root_path,
            &["--cached".into()],
            "cannot inspect staged paths",
        )?,
        unstaged: diff_paths(root_path, &[], "cannot inspect unstaged paths")?,
        untracked: parse_path_set(
            &require_git_bytes(
                root_path,
                &[
                    "ls-files".into(),
                    "--others".into(),
                    "--exclude-standard".into(),
                    "-z".into(),
                    "--".into(),
                ],
                "cannot inspect untracked paths",
            )?,
            "cannot inspect untracked paths",
        )?,
    };
    Ok(ChangeScopeReport {
        format_version: FORMAT_VERSION,
        repository_root: root,
        input: ReportInput {
            base: options.base.clone(),
            head: options.head.clone(),
        },
        resolved: ResolvedRefs {
            base: base_sha,
            head: head_sha,
            merge_base: merge_base_sha,
        },
        paths,
    })
}

fn resolve_commit(root: &Path, label: &str, reference: &str) -> anyhow::Result<String> {
    let context = format!("cannot resolve {label} ref {reference:?}");
    let result = execute_git_text(
        root,
        &[
            "-c".into(),
            "core.warnAmbiguousRefs=true".into(),
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            format!("{reference}^{{commit}}").into(),
        ],
        &context,
    )?;
    if contains_ascii_word(&result.stderr, "ambiguous") {
        anyhow::bail!(
            "{label} ref {reference:?} is ambiguous; use a fully qualified ref or commit ID"
        );
    }
    if !result.success {
        anyhow::bail!(
            "{label} ref {reference:?} does not resolve to a commit: {}",
            failure_detail(&result)
        );
    }
    let commits = result
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        commits.len() == 1,
        "{label} ref {reference:?} did not resolve to exactly one commit"
    );
    Ok(commits[0].to_owned())
}

fn resolve_merge_base(root: &Path, base_sha: &str, head_sha: &str) -> anyhow::Result<String> {
    let result = execute_git_text(
        root,
        &[
            "merge-base".into(),
            "--all".into(),
            base_sha.into(),
            head_sha.into(),
        ],
        "cannot resolve the merge base",
    )?;
    if !result.success {
        anyhow::bail!(
            "base and head do not have a merge base: {}",
            failure_detail(&result)
        );
    }
    let merge_bases = result
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        merge_bases.len() == 1,
        "base and head do not have a unique merge base; found {}",
        merge_bases.len()
    );
    Ok(merge_bases[0].to_owned())
}

fn diff_paths(root: &Path, range: &[OsString], context: &str) -> anyhow::Result<Vec<String>> {
    let args = [
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--ignore-submodules=none",
        "--name-only",
        "-z",
    ]
    .into_iter()
    .map(OsString::from)
    .chain(range.iter().cloned())
    .chain([OsString::from("--")])
    .collect::<Vec<_>>();
    parse_path_set(&require_git_bytes(root, &args, context)?, context)
}

fn parse_path_set(output: &[u8], context: &str) -> anyhow::Result<Vec<String>> {
    let mut paths = HashSet::new();
    for (index, raw) in output
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .enumerate()
    {
        let record = index + 1;
        let path = std::str::from_utf8(raw)
            .map_err(|_| anyhow::anyhow!("{context}: Git path {record} is not valid UTF-8"))?;
        paths.insert(path.to_owned());
    }
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(paths)
}

fn require_git(root: &Path, args: &[OsString], context: &str) -> anyhow::Result<String> {
    let result = execute_git_text(root, args, context)?;
    anyhow::ensure!(result.success, "{context}: {}", failure_detail(&result));
    Ok(result.stdout)
}

fn require_git_bytes(root: &Path, args: &[OsString], context: &str) -> anyhow::Result<Vec<u8>> {
    let result = execute_git_bytes(root, args);
    anyhow::ensure!(
        result.success,
        "{context}: {}",
        result.error.unwrap_or_else(|| {
            let stderr = String::from_utf8_lossy(&result.stderr).trim().to_owned();
            if stderr.is_empty() {
                format_status(result.status)
            } else {
                stderr
            }
        })
    );
    Ok(result.stdout)
}

fn execute_git_text(
    root: &Path,
    args: &[OsString],
    context: &str,
) -> anyhow::Result<GitTextResult> {
    let result = execute_git_bytes(root, args);
    let stdout = String::from_utf8(result.stdout)
        .map_err(|_| anyhow::anyhow!("{context}: Git stdout is not valid UTF-8"))?;
    let stderr = String::from_utf8(result.stderr)
        .map_err(|_| anyhow::anyhow!("{context}: Git stderr is not valid UTF-8"))?;
    Ok(GitTextResult {
        status: result.status,
        success: result.success,
        stdout,
        stderr,
        error: result.error,
    })
}

fn execute_git_bytes(root: &Path, args: &[OsString]) -> GitBytesResult {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    match command.output() {
        Ok(Output {
            status,
            stdout,
            stderr,
        }) if stdout.len() <= MAX_GIT_OUTPUT && stderr.len() <= MAX_GIT_OUTPUT => GitBytesResult {
            status: status.code(),
            success: status.success(),
            stdout,
            stderr,
            error: None,
        },
        Ok(Output {
            status,
            stdout,
            stderr,
        }) => GitBytesResult {
            status: status.code(),
            success: false,
            stdout,
            stderr,
            error: Some(format!(
                "Git output exceeded the {MAX_GIT_OUTPUT}-byte per-stream limit"
            )),
        },
        Err(error) => GitBytesResult {
            status: None,
            success: false,
            stdout: Vec::new(),
            stderr: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

fn failure_detail(result: &GitTextResult) -> String {
    result.error.clone().unwrap_or_else(|| {
        let stderr = result.stderr.trim();
        if stderr.is_empty() {
            format_status(result.status)
        } else {
            stderr.to_owned()
        }
    })
}

fn format_status(status: Option<i32>) -> String {
    status.map_or_else(
        || "Git exited with status null".to_owned(),
        |status| format!("Git exited with status {status}"),
    )
}

fn strip_git_line_terminator(mut output: String) -> String {
    if output.ends_with('\n') {
        output.pop();
    }
    if cfg!(windows) && output.ends_with('\r') {
        output.pop();
    }
    output
}

fn contains_ascii_word(value: &str, needle: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| word.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_parser_requires_base_and_defaults_head() {
        let options = parse_options(&["--base".into(), "origin/main".into()]).unwrap();
        assert_eq!(options.base, "origin/main");
        assert_eq!(options.head, "HEAD");
        let options = parse_options(&["--base=foundation".into(), "--head=topic".into()]).unwrap();
        assert_eq!(options.base, "foundation");
        assert_eq!(options.head, "topic");
        assert!(
            parse_options(&[])
                .unwrap_err()
                .to_string()
                .contains("missing required --base")
        );
        for (args, expected) in [
            (
                vec!["--base".into()],
                "Option '--base <value>' argument missing",
            ),
            (
                vec!["--base".into(), "--head".into(), "HEAD".into()],
                "Option '--base' argument is ambiguous.\nDid you forget to specify the option argument for '--base'?\nTo specify an option argument starting with a dash use '--base=-XYZ'.",
            ),
            (vec!["--unknown".into()], "Unknown option '--unknown'"),
            (
                vec!["HEAD".into()],
                "Unexpected argument 'HEAD'. This command does not take positional arguments",
            ),
            (
                vec!["--base".into(), "HEAD".into(), "--".into(), "x".into()],
                "Unexpected argument 'x'. This command does not take positional arguments",
            ),
        ] {
            assert_eq!(parse_options(&args).unwrap_err().to_string(), expected);
        }
        assert_eq!(
            parse_options(&["--base".into(), "HEAD".into(), "--".into()])
                .unwrap()
                .base,
            "HEAD"
        );
    }

    #[test]
    fn path_sorting_matches_javascript_utf16_order() {
        let paths = parse_path_set("\u{e000}\0😀\0😀\0".as_bytes(), "paths").unwrap();
        assert_eq!(paths, ["😀", "\u{e000}"]);
    }
}
