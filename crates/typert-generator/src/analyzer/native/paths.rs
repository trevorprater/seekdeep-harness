//! Node-compatible path semantics used by the source analyzer.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

thread_local! {
    // Only existing paths are memoized: a path can come into existence later,
    // but an existing path's canonical form is stable for the thread lifetime
    // (analysis edits rewrite file contents, never the directory tree).
    static REAL_PATHS: RefCell<HashMap<PathBuf, PathBuf>> = RefCell::new(HashMap::new());
}

/// `path.resolve`: absolute against the current directory, lexically normalized.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    normalize(&absolute)
}

/// Lexical normalization of `.` and `..` segments without touching the filesystem.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component);
            }
        }
    }
    normalized
}

/// `path.join` with lexical normalization.
pub(crate) fn join(base: &Path, tail: impl AsRef<Path>) -> PathBuf {
    normalize(&base.join(tail))
}

/// Canonical form of an existing path; absent paths resolve lexically only.
pub(crate) fn real_path(path: &Path) -> PathBuf {
    let absolute = resolve(path);
    if let Some(cached) = REAL_PATHS.with(|cache| cache.borrow().get(&absolute).cloned()) {
        return cached;
    }
    if !absolute.exists() {
        return absolute;
    }
    let resolved = std::fs::canonicalize(&absolute).unwrap_or_else(|_| absolute.clone());
    REAL_PATHS.with(|cache| {
        cache.borrow_mut().insert(absolute, resolved.clone());
    });
    resolved
}

/// Whether `path` is `root` or a descendant of it after canonicalization.
pub(crate) fn is_within(path: &Path, root: &Path) -> bool {
    let absolute = real_path(path);
    let parent = real_path(root);
    absolute == parent || absolute.starts_with(&parent)
}

/// `path.relative(from, to)` with forward-slash output.
pub(crate) fn relative(from: &Path, to: &Path) -> String {
    let from = resolve(from);
    let to = resolve(to);
    let from_parts = from.components().collect::<Vec<_>>();
    let to_parts = to.components().collect::<Vec<_>>();
    let shared = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut segments = vec![".."; from_parts.len() - shared];
    let tail = to_parts[shared..]
        .iter()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    segments.extend(tail.iter().map(String::as_str));
    segments.join("/")
}

pub(crate) fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn dirname(path: &Path) -> PathBuf {
    path.parent()
        .map_or_else(|| path.to_owned(), Path::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_follow_node_semantics() {
        assert_eq!(
            relative(
                Path::new("/ws/root"),
                Path::new("/ws/root/packages/a/src/index.ts")
            ),
            "packages/a/src/index.ts"
        );
        assert_eq!(relative(Path::new("/ws/root"), Path::new("/ws/root")), "");
        assert_eq!(
            relative(Path::new("/ws/root/nested"), Path::new("/ws/other/file.ts")),
            "../../other/file.ts"
        );
    }

    #[test]
    fn normalization_and_joins_are_lexical() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(join(Path::new("/a/b"), "../c"), PathBuf::from("/a/c"));
        assert_eq!(dirname(Path::new("/a/b/c.ts")), PathBuf::from("/a/b"));
        assert_eq!(slash(Path::new("/a/b")), "/a/b");
    }

    #[test]
    fn containment_uses_canonical_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        std::fs::create_dir_all(root.join("packages/a")).unwrap();
        assert!(is_within(&root.join("packages/a"), &root));
        assert!(is_within(&root, &root));
        assert!(!is_within(temporary.path(), &root));
        assert!(!is_within(&temporary.path().join("root-sibling"), &root));
    }
}
