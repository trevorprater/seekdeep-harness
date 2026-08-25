//! Cordis Loader configuration file discovery.

use std::path::Path;

/// Returns sorted repository-relative Cordis Loader YAML paths below `root`.
///
/// Translation consistency records are YAML sidecars, never Loader inputs.
///
/// # Errors
///
/// Returns directory traversal or non-prefix path failures.
pub fn cordis_config_files(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut paths = Vec::new();
    let entries = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let hidden = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with('.'));
            !hidden
                && (entry.depth() != 1
                    || !entry.file_type().is_dir()
                    || !matches!(entry.file_name().to_str(), Some("node_modules" | "vendor")))
        });
    for entry in entries {
        let entry = entry?;
        if entry.depth() == 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        let comparable = if cfg!(any(target_os = "macos", windows)) {
            name.to_ascii_lowercase()
        } else {
            name.into_owned()
        };
        let extension = Path::new(&comparable)
            .extension()
            .and_then(std::ffi::OsStr::to_str);
        if !comparable.contains("cordis")
            || !matches!(extension, Some("yml" | "yaml"))
            || comparable.strip_suffix(".i18n.yaml").is_some()
        {
            continue;
        }
        paths.push(
            entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .into_owned(),
        );
    }
    paths.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(paths)
}
