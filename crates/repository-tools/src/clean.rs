//! Repository-owned build-output cleanup with fail-closed path validation.

use std::{
    collections::{BTreeSet, HashSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context as _, anyhow, bail};
use serde_json::Value;

const KNOWN_ORPHAN_ENTRIES: [&str; 3] = ["node_modules", "lib", ".typecheck"];

/// Plans and removes generated build state without crossing the repository boundary.
#[derive(Clone, Debug)]
pub struct RepositoryCleaner {
    root: PathBuf,
}

impl RepositoryCleaner {
    /// Creates a cleaner rooted at one repository checkout.
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: absolute_lexical(root.as_ref()),
        }
    }

    /// Removes generated build state and safe manifest-less package residue.
    ///
    /// Planning validates every target before the first removal, so one unsafe
    /// orphan prevents all mutation.
    ///
    /// # Errors
    ///
    /// Returns filesystem, malformed tsconfig, unsafe path, or unknown-orphan failures.
    pub fn clean(&self) -> anyhow::Result<Vec<String>> {
        let targets = self.plan_paths()?;
        for target in &targets {
            remove_target(target)?;
        }
        targets
            .iter()
            .map(|target| repository_path(&self.root, target))
            .collect()
    }

    /// Validates and reports every deletion target without mutating it.
    ///
    /// # Errors
    ///
    /// Returns the same planning failures as [`Self::clean`].
    pub fn plan(&self) -> anyhow::Result<Vec<String>> {
        self.plan_paths()?
            .iter()
            .map(|target| repository_path(&self.root, target))
            .collect()
    }

    fn plan_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        let mut targets = BTreeSet::<PathBuf>::new();
        let mut unsafe_orphans = Vec::<String>::new();
        let canonical_root = std::fs::canonicalize(&self.root)
            .with_context(|| format!("clean: resolve repository {}", self.root.display()))?;

        Self::add_if_present(&mut targets, self.root.join(".typecheck"), &canonical_root)?;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tsbuildinfo")
            {
                targets.insert(entry.path());
            }
        }
        Self::add_if_present(
            &mut targets,
            self.root.join("native/landlock-run/tsconfig.tsbuildinfo"),
            &canonical_root,
        )?;

        for output in self.build_output_directories()? {
            Self::add_if_present(&mut targets, output, &canonical_root)?;
        }

        for group in child_directories(&self.root.join("packages"))? {
            for package in child_directories(&group)? {
                if package.join("package.json").try_exists()? {
                    continue;
                }
                let mut unknown = Vec::new();
                for entry in std::fs::read_dir(&package)? {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !KNOWN_ORPHAN_ENTRIES.contains(&name.as_str())
                        && !name.ends_with(".tsbuildinfo")
                    {
                        unknown.push(repository_path(&self.root, &entry.path())?);
                    }
                }
                if unknown.is_empty() {
                    Self::add_if_present(&mut targets, package, &canonical_root)?;
                } else {
                    unsafe_orphans.extend(unknown);
                }
            }
        }
        if !unsafe_orphans.is_empty() {
            unsafe_orphans.sort();
            let mut message = "clean: refusing to remove package directories without package.json; unknown entries remain:".to_owned();
            for path in unsafe_orphans {
                message.push_str("\n  ");
                message.push_str(&path);
            }
            bail!(message);
        }
        Ok(targets.into_iter().collect())
    }

    fn build_output_directories(&self) -> anyhow::Result<BTreeSet<PathBuf>> {
        let mut outputs = BTreeSet::new();
        let mut pending = vec![self.root.join("tsconfig.json")];
        let mut visited = HashSet::new();
        let native_entry = self.root.join("native/landlock-run/packages/entry/lib");
        while let Some(config_path) = pending.pop() {
            let config_path = normalize_lexical(&config_path);
            if !visited.insert(config_path.clone()) {
                continue;
            }
            let parsed = parse_config(&config_path, &mut HashSet::new())?;
            if let Some(types_directory) = parsed.out_dir {
                let output = if types_directory.file_name() == Some(OsStr::new("types")) {
                    types_directory
                        .parent()
                        .expect("types output has a parent")
                        .to_owned()
                } else if types_directory == native_entry {
                    types_directory
                } else {
                    bail!(
                        "clean: expected TypeScript outDir to end in /types: {}",
                        repository_path(&self.root, &types_directory)?
                    );
                };
                self.assert_repository_target(&output)?;
                outputs.insert(output);
            }
            pending.extend(parsed.references);
        }
        Ok(outputs)
    }

    fn assert_repository_target(&self, target: &Path) -> anyhow::Result<()> {
        assert_descendant(&self.root, target, target)
    }

    fn add_if_present(
        targets: &mut BTreeSet<PathBuf>,
        target: PathBuf,
        canonical_root: &Path,
    ) -> anyhow::Result<()> {
        match std::fs::symlink_metadata(&target) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let parent = target
            .parent()
            .ok_or_else(|| anyhow!("clean: target has no parent: {}", target.display()))?;
        let canonical_parent = std::fs::canonicalize(parent)?;
        let canonical_target = canonical_parent.join(
            target
                .file_name()
                .ok_or_else(|| anyhow!("clean: target has no basename: {}", target.display()))?,
        );
        assert_descendant(canonical_root, &canonical_target, &target)?;
        targets.insert(target);
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct ParsedConfig {
    out_dir: Option<PathBuf>,
    references: Vec<PathBuf>,
}

fn parse_config(path: &Path, stack: &mut HashSet<PathBuf>) -> anyhow::Result<ParsedConfig> {
    let path = normalize_lexical(path);
    if !stack.insert(path.clone()) {
        bail!("clean: cyclic TypeScript config extends {}", path.display());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("clean: read TypeScript config {}", path.display()))?;
    let value: Value = serde_json::from_str(&jsonc_to_json(&contents))
        .with_context(|| format!("clean: cannot parse TypeScript config {}", path.display()))?;
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("clean: config has no parent: {}", path.display()))?;
    let inherited = value
        .get("extends")
        .and_then(Value::as_str)
        .map(|extends| parse_config(&resolve_config(directory, extends), stack))
        .transpose()?;
    let out_dir = value
        .pointer("/compilerOptions/outDir")
        .and_then(Value::as_str)
        .map(|out_dir| normalize_lexical(&directory.join(out_dir)))
        .or_else(|| inherited.and_then(|config| config.out_dir));
    let references = value
        .get("references")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|reference| reference.get("path").and_then(Value::as_str))
        .map(|reference| resolve_config(directory, reference))
        .collect();
    stack.remove(&path);
    Ok(ParsedConfig {
        out_dir,
        references,
    })
}

fn resolve_config(directory: &Path, value: &str) -> PathBuf {
    let path = normalize_lexical(&directory.join(value));
    if path.extension().is_some() {
        path
    } else {
        path.join("tsconfig.json")
    }
}

fn jsonc_to_json(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if string {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            string = true;
            output.push(byte);
            index += 1;
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(b' ');
                index += 1;
            }
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            output.extend_from_slice(b"  ");
            index += 2;
            while index < bytes.len() {
                if bytes.get(index..index + 2) == Some(b"*/") {
                    output.extend_from_slice(b"  ");
                    index += 2;
                    break;
                }
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
            continue;
        }
        if byte == b',' {
            let mut lookahead = index + 1;
            while bytes.get(lookahead).is_some_and(u8::is_ascii_whitespace) {
                lookahead += 1;
            }
            if matches!(bytes.get(lookahead), Some(b'}' | b']')) {
                output.push(b' ');
                index += 1;
                continue;
            }
        }
        output.push(byte);
        index += 1;
    }
    String::from_utf8(output).expect("JSONC cleanup preserves UTF-8")
}

fn child_directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

fn remove_target(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn assert_descendant(root: &Path, target: &Path, display: &Path) -> anyhow::Result<()> {
    let root = normalize_lexical(root);
    let target = normalize_lexical(target);
    if target == root || !target.starts_with(&root) {
        bail!(
            "clean: refusing deletion target outside repository: {}",
            display.display()
        );
    }
    Ok(())
}

fn repository_path(root: &Path, path: &Path) -> anyhow::Result<String> {
    Ok(path
        .strip_prefix(root)?
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn absolute_lexical(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_lexical(path)
    } else {
        normalize_lexical(
            &std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path),
        )
    }
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                output.push(component.as_os_str());
            }
        }
    }
    output
}
