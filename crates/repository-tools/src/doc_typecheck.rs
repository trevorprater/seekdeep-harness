//! Compilation of Markdown TypeScript fences against workspace declarations.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use path_clean::PathClean;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    agent_note_tree::is_archived_agent_note_path,
    doc_typecheck_paths::built_declaration_path,
    markdown_util::markdown_fences,
    paired_markdown_derivatives::partition_paired_markdown_derivatives,
    ts_project::{RepositoryCompiler, VirtualSource, semantic_compiler_options},
};

/// Complete source scope shared by snippet and declaration-equivalence gates.
pub const MARKDOWN_GLOBS: &[&str] = &[
    "README.md",
    ".agents/notes/**/*.md",
    "docs/**/*.md",
    "packages/*/*.md",
    "packages/*/*/*.md",
];

/// Gate owning one recognized TypeScript fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockKind {
    /// Compile this block against the workspace API.
    Check,
    /// Count this unchecked sketch in the opt-out ratio.
    Ignore,
    /// A source declaration checked by the type-equivalence gate.
    TypeEquiv,
    /// A generated Cordis catalog fragment.
    CordisCatalog,
    /// A generated persistence catalog fragment.
    PersistenceCatalog,
    /// A generated configuration catalog fragment.
    ConfigCatalog,
}

impl BlockKind {
    /// Source fingerprint key used for paired-block identity.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Ignore => "ignore",
            Self::TypeEquiv => "type-equiv",
            Self::CordisCatalog => "cordis-catalog",
            Self::PersistenceCatalog => "persistence-catalog",
            Self::ConfigCatalog => "config-catalog",
        }
    }

    fn from_info(info: &str) -> Option<Self> {
        Some(match info {
            "ts" => Self::Check,
            "ts ignore-check" => Self::Ignore,
            "ts type-equiv" | "ts public-api" => Self::TypeEquiv,
            "ts cordis-catalog" => Self::CordisCatalog,
            "ts persistence-catalog" => Self::PersistenceCatalog,
            "ts config-catalog" => Self::ConfigCatalog,
            _ => return None,
        })
    }
}

/// One recognized TypeScript fence with its authored location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocBlock {
    /// Repository-relative Markdown path.
    pub file: String,
    /// One-based line of the opening fence.
    pub line: usize,
    /// Gate ownership or opt-out classification.
    pub kind: BlockKind,
    /// Parsed fence contents without delimiters.
    pub code: String,
}

/// Declaration freshness ownership for the snippet compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileMode {
    /// Invoke the TypeScript project builder with Host references.
    Standalone,
    /// Consume declarations produced by a coordinated build, without emit.
    BuiltTypes,
}

/// Source-compatible output channels and result of one gate execution.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocTypecheckReport {
    /// Whether the snippets and the opt-out guard passed.
    pub passed: bool,
    /// Normal command output.
    pub stdout: String,
    /// Compiler or opt-out diagnostics.
    pub stderr: String,
    /// Compile-eligible primary blocks.
    pub checked: usize,
    /// Explicitly unchecked primary blocks.
    pub ignored: usize,
    /// Primary catalog or declaration blocks owned by another gate.
    pub skipped: usize,
    /// Byte-identical paired blocks reusing the primary check.
    pub derivatives: usize,
}

/// Extracts all recognized TypeScript fence kinds in source order.
///
/// # Errors
/// Returns Markdown parsing errors.
pub fn extract_blocks(file: &str, source: &str) -> anyhow::Result<Vec<DocBlock>> {
    Ok(markdown_fences(source)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .filter_map(|fence| {
            Some(DocBlock {
                file: file.to_owned(),
                line: fence.line,
                kind: BlockKind::from_info(&fence.info)?,
                code: fence.code,
            })
        })
        .collect())
}

/// Enumerates the exact Markdown path scope, excluding frozen Agent Notes.
///
/// # Errors
/// Returns glob or filesystem traversal failures.
pub fn markdown_files(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    for pattern in MARKDOWN_GLOBS {
        let absolute = root.join(pattern).to_string_lossy().into_owned();
        for matched in glob::glob_with(&absolute, options)? {
            let matched = matched?;
            let relative = matched
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            if !is_archived_agent_note_path(&relative) {
                files.push(relative);
            }
        }
    }
    files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(files)
}

/// Redirects workspace source aliases to coordinated declaration outputs.
///
/// # Errors
/// Rejects absent `paths` or an unsupported source alias, preserving diagnostics.
pub fn built_type_compiler_options(mut options: Value) -> anyhow::Result<Value> {
    let paths = options
        .get_mut("paths")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow::anyhow!("doc-typecheck: host tsconfig has no workspace paths"))?;
    for candidates in paths.values_mut() {
        let candidates = candidates
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("workspace paths must contain candidate arrays"))?;
        for candidate in candidates {
            let value = candidate
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("workspace path candidate is not a string"))?;
            *candidate = Value::String(built_declaration_path(value)?);
        }
    }
    semantic_compiler_options(&mut options);
    let object = options
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("doc-typecheck: host tsconfig has no workspace paths"))?;
    object.insert("noUnusedLocals".to_owned(), Value::Bool(false));
    object.insert("noUnusedParameters".to_owned(), Value::Bool(false));
    object.remove("tsBuildInfoFile");
    Ok(options)
}

/// Generates the standalone child project while preserving Host references.
///
/// # Errors
/// Rejects a malformed Host reference list.
pub fn temp_tsconfig(host_config: &Value) -> anyhow::Result<Value> {
    let references = host_config
        .get("references")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("doc-typecheck: host tsconfig has no project references"))?;
    let references = references
        .iter()
        .map(|reference| {
            let path = reference
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("doc-typecheck: project reference has no path"))?;
            Ok(json!({ "path": format!("../{}", path.strip_prefix("./").unwrap_or(path)) }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(json!({
        "extends": "../tsconfig.host.json",
        "compilerOptions": {
            "noUnusedLocals": false,
            "noUnusedParameters": false,
            "tsBuildInfoFile": "./tsconfig.tsbuildinfo",
        },
        "include": ["block-*.ts"],
        "references": references,
    }))
}

/// Replaces virtual or temporary block locations with Markdown fence locations.
pub fn remap_block_paths(output: &str, blocks: &[DocBlock]) -> String {
    static LOCATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:[^\s:()]*[/\\])?block-(\d+)\.ts\((\d+),(\d+)\)")
            .expect("static block diagnostic regex")
    });
    LOCATION
        .replace_all(output, |matched: &regex::Captures<'_>| {
            matched[1]
                .parse::<usize>()
                .ok()
                .and_then(|index| blocks.get(index))
                .map_or_else(
                    || format!("block-{}.ts({},{})", &matched[1], &matched[2], &matched[3]),
                    |block| {
                        format!(
                            "{} (block at line {}, +{}:{})",
                            block.file, block.line, &matched[2], &matched[3]
                        )
                    },
                )
        })
        .into_owned()
}

/// Runs snippet compilation and the source's excessive-opt-out guard.
///
/// # Errors
/// Returns discovery, parsing, configuration, filesystem, or compiler failures.
pub fn check_documentation(root: &Path, mode: CompileMode) -> anyhow::Result<DocTypecheckReport> {
    check_documentation_with_compiler(root, mode, None)
}

/// Runs the gate with an optional explicit compiler dependency for fixtures.
///
/// # Errors
/// Returns discovery, parsing, configuration, filesystem, or compiler failures.
pub fn check_documentation_with_compiler(
    root: &Path,
    mode: CompileMode,
    library: Option<&Path>,
) -> anyhow::Result<DocTypecheckReport> {
    let root = absolute_path(root)?;
    let mut extracted = Vec::new();
    for file in markdown_files(&root)? {
        extracted.extend(extract_blocks(
            &file,
            &std::fs::read_to_string(root.join(&file))?,
        )?);
    }
    let partition = partition_paired_markdown_derivatives(
        &extracted,
        |block| block.file.clone(),
        |block| format!("{}\0{}", block.kind.as_str(), block.code),
    );
    let checked = partition
        .primary
        .iter()
        .filter(|block| block.kind == BlockKind::Check)
        .cloned()
        .collect::<Vec<_>>();
    let ignored = partition
        .primary
        .iter()
        .filter(|block| block.kind == BlockKind::Ignore)
        .count();
    let mut report = DocTypecheckReport {
        checked: checked.len(),
        ignored,
        skipped: partition.primary.len() - checked.len() - ignored,
        derivatives: partition.derivatives.len(),
        ..DocTypecheckReport::default()
    };
    if checked.is_empty() {
        report.passed = true;
        "doc-typecheck: no ts code blocks to check.\n".clone_into(&mut report.stdout);
        return Ok(report);
    }
    let mut compiler =
        library.map_or_else(|| RepositoryCompiler::new(&root), RepositoryCompiler::load)?;
    let error = match mode {
        CompileMode::BuiltTypes => {
            let config = compiler.parse_config(&root.join("tsconfig.host.json"), &root)?;
            let options = built_type_compiler_options(config.options)?;
            let sources = checked
                .iter()
                .enumerate()
                .map(|(index, block)| VirtualSource {
                    file_name: root
                        .join(".doc-typecheck")
                        .join(format!("block-{index}.ts"))
                        .to_string_lossy()
                        .into_owned(),
                    text: terminated_code(&block.code),
                })
                .collect::<Vec<_>>();
            let diagnostics = compiler.compile_no_emit(&root, &options, &sources)?;
            (!diagnostics.is_empty()).then(|| remap_block_paths(&diagnostics.formatted, &checked))
        }
        CompileMode::Standalone => compile_standalone(&root, &checked, &mut compiler)?,
    };
    if let Some(error) = error {
        report.stderr =
            format!("doc-typecheck: documentation code blocks failed to compile.\n\n{error}\n");
        return Ok(report);
    }
    let denominator = checked.len() + ignored;
    let percent = (100 * ignored * 2 + denominator) / (2 * denominator);
    report.stdout = format!(
        "doc-typecheck: {} block(s) compiled, {ignored} ignored ({percent}% opt-out), {} type-equiv/catalog (checked elsewhere), {} paired derivative(s).\n",
        checked.len(),
        report.skipped,
        report.derivatives
    );
    report.passed = denominator < 4 || ignored * 2 <= denominator;
    if !report.passed {
        report.stderr = format!(
            "doc-typecheck: too many blocks opt out of checking ({ignored}/{denominator}). Make them compile or delete them.\n"
        );
    }
    Ok(report)
}

fn compile_standalone(
    root: &Path,
    blocks: &[DocBlock],
    compiler: &mut RepositoryCompiler,
) -> anyhow::Result<Option<String>> {
    let temporary = TemporaryProject::new(root)?;
    let outcome = (|| {
        let path = root.join("tsconfig.host.json");
        let config = compiler.read_config(&path).map_err(|error| {
            anyhow::anyhow!("doc-typecheck: cannot read {}: {error}", path.display())
        })?;
        std::fs::write(
            temporary.path.join("tsconfig.json"),
            serde_json::to_string(&temp_tsconfig(&config)?)?,
        )?;
        for (index, block) in blocks.iter().enumerate() {
            std::fs::write(
                temporary.path.join(format!("block-{index}.ts")),
                terminated_code(&block.code),
            )?;
        }
        let workspace_tsc = root.join("node_modules/typescript/bin/tsc");
        let tsc = if workspace_tsc.is_file() {
            workspace_tsc
        } else {
            compiler
                .library()
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| {
                    anyhow::anyhow!("doc-typecheck: compiler library has no package directory")
                })?
                .join("bin/tsc")
        };
        let output = Command::new("node")
            .arg(tsc)
            .arg("-b")
            .arg(temporary.path.join("tsconfig.json"))
            .current_dir(root)
            .output();
        match output {
            Ok(output) if output.status.success() => Ok(None),
            Ok(output) => Ok(Some(remap_block_paths(
                &format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
                blocks,
            ))),
            Err(_) => Ok(Some(String::new())),
        }
    })();
    temporary.remove()?;
    outcome
}

fn terminated_code(code: &str) -> String {
    if code.ends_with('\n') {
        code.to_owned()
    } else {
        format!("{code}\n")
    }
}

fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean())
}

struct TemporaryProject {
    path: PathBuf,
}

impl TemporaryProject {
    fn new(root: &Path) -> anyhow::Result<Self> {
        for attempt in 0..1_000_u16 {
            let path = root.join(format!(".doc-typecheck-{}-{attempt}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("doc-typecheck: could not allocate temporary project directory")
    }

    fn remove(self) -> anyhow::Result<()> {
        std::fs::remove_dir_all(&self.path)?;
        Ok(())
    }
}

impl Drop for TemporaryProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
