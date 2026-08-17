//! Shared filesystem path helpers for `SeekDeep` user data.

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

use path_clean::PathClean;
use thiserror::Error;

/// Directory name for the default `SeekDeep` home under the OS home.
pub const SEEKDEEP_HOME_DIR_NAME: &str = ".seekdeep";
/// Stable user-facing display form for the default `SeekDeep` home.
pub const DEFAULT_SEEKDEEP_HOME_DISPLAY: &str = "~/.seekdeep";
/// Environment variable overriding the default `SeekDeep` home.
pub const SEEKDEEP_HOME_ENV: &str = "SEEKDEEP_HOME";

/// Failure to resolve the process or operating-system home directory.
#[derive(Debug, Error)]
pub enum HomePathError {
    /// The operating system did not expose a home directory.
    #[error("operating-system home directory is unavailable")]
    HomeUnavailable,
    /// The process current directory could not be read.
    #[error("failed to resolve the current directory: {0}")]
    CurrentDirectory(#[source] io::Error),
}

fn os_home() -> Result<PathBuf, HomePathError> {
    dirs::home_dir().ok_or(HomePathError::HomeUnavailable)
}

/// Returns the absolute default `SeekDeep` home.
///
/// # Errors
///
/// Returns [`HomePathError::HomeUnavailable`] when the platform provides no
/// user home directory.
pub fn default_seekdeep_home() -> Result<PathBuf, HomePathError> {
    Ok(os_home()?.join(SEEKDEEP_HOME_DIR_NAME))
}

/// Expands the supported `~`, `~/`, and `~\` prefixes.
///
/// # Errors
///
/// Returns [`HomePathError::HomeUnavailable`] only when expansion requires a
/// platform home directory and none is available.
pub fn expand_home_path(path: impl AsRef<OsStr>) -> Result<PathBuf, HomePathError> {
    let path = path.as_ref();
    let lossy = path.to_string_lossy();
    if lossy == "~" {
        return os_home();
    }
    if let Some(suffix) = lossy
        .strip_prefix("~/")
        .or_else(|| lossy.strip_prefix("~\\"))
    {
        return Ok(os_home()?.join(suffix));
    }
    Ok(PathBuf::from(path))
}

fn resolve_absolute(path: &Path) -> Result<PathBuf, HomePathError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(HomePathError::CurrentDirectory)?
            .join(path)
    };
    Ok(absolute.clean())
}

/// Resolves the configured home with explicit > environment > default
/// precedence.
///
/// # Errors
///
/// Returns when a required OS home or current directory is unavailable.
pub fn resolve_seekdeep_home<S: std::hash::BuildHasher>(
    configured: Option<&OsStr>,
    environment: &HashMap<OsString, OsString, S>,
) -> Result<PathBuf, HomePathError> {
    let from_environment = environment.get(OsStr::new(SEEKDEEP_HOME_ENV));
    let selected = if let Some(configured) = configured {
        PathBuf::from(configured)
    } else if let Some(value) = from_environment.filter(|value| {
        !value
            .to_string_lossy()
            .trim_matches(char::is_whitespace)
            .is_empty()
    }) {
        PathBuf::from(value)
    } else {
        default_seekdeep_home()?
    };
    resolve_absolute(&expand_home_path(selected.as_os_str())?)
}

/// Resolves the home against the process environment.
///
/// # Errors
///
/// Returns when a required OS home or current directory is unavailable.
pub fn resolve_process_seekdeep_home(configured: Option<&OsStr>) -> Result<PathBuf, HomePathError> {
    let environment: HashMap<OsString, OsString> = std::env::vars_os().collect();
    resolve_seekdeep_home(configured, &environment)
}

/// Joins segments onto the process-resolved `SeekDeep` home.
///
/// # Errors
///
/// Returns when the process home cannot be resolved.
pub fn seekdeep_home_path<I, P>(segments: I) -> Result<PathBuf, HomePathError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut path = resolve_process_seekdeep_home(None)?;
    path.extend(segments);
    Ok(path)
}

/// Returns the symbolic, machine-path-free display label for a resolved home.
///
/// # Errors
///
/// Returns when the default home cannot be resolved for comparison.
pub fn seekdeep_home_display(resolved_home: &Path) -> Result<&'static str, HomePathError> {
    if resolved_home == resolve_absolute(&default_seekdeep_home()?)? {
        Ok(DEFAULT_SEEKDEEP_HOME_DISPLAY)
    } else {
        Ok("$SEEKDEEP_HOME")
    }
}

/// Canonicalizes the deepest existing watcher ancestor and restores any
/// missing suffix without requiring the final target to exist.
///
/// # Errors
///
/// Propagates traversal errors other than absence. If a suffix is absent, the
/// deepest existing ancestor must also be an enumerable directory.
pub async fn canonicalize_watch_path(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let mut current = if path.as_ref().is_absolute() {
        path.as_ref().to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean();
    let mut missing = Vec::<OsString>::new();

    loop {
        match tokio::fs::canonicalize(&current).await {
            Ok(canonical) => {
                if !missing.is_empty() {
                    let mut directory = tokio::fs::read_dir(&canonical).await?;
                    // Force an actual enumeration syscall instead of merely
                    // constructing the handle, mirroring `opendir`'s proof.
                    let _ = directory.next_entry().await?;
                }
                let mut result = canonical;
                for component in missing.iter().rev() {
                    result.push(component);
                }
                return Ok(result);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = current.parent().map(Path::to_path_buf);
                let Some(parent) = parent.filter(|parent| parent != &current) else {
                    return Err(error);
                };
                let Some(name) = current.file_name() else {
                    return Err(error);
                };
                missing.push(name.to_os_string());
                current = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use tempfile::tempdir;

    use super::*;

    fn environment(entries: &[(&str, &str)]) -> HashMap<OsString, OsString> {
        entries
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    #[test]
    fn renamed_default_and_tilde_contract() {
        let home = os_home().unwrap();
        assert_eq!(SEEKDEEP_HOME_DIR_NAME, ".seekdeep");
        assert_eq!(DEFAULT_SEEKDEEP_HOME_DISPLAY, "~/.seekdeep");
        assert_eq!(default_seekdeep_home().unwrap(), home.join(".seekdeep"));
        assert_eq!(expand_home_path("~").unwrap(), home);
        assert_eq!(
            expand_home_path("~/.seekdeep").unwrap(),
            home.join(".seekdeep")
        );
        assert_eq!(
            expand_home_path("~\\.seekdeep").unwrap(),
            home.join(".seekdeep")
        );
        assert_eq!(
            expand_home_path("/tmp/.seekdeep").unwrap(),
            Path::new("/tmp/.seekdeep")
        );
        assert_eq!(
            expand_home_path("~other/.seekdeep").unwrap(),
            Path::new("~other/.seekdeep")
        );
    }

    #[test]
    fn precedence_and_blank_environment_match_source() {
        let home = os_home().unwrap();
        let env = environment(&[(SEEKDEEP_HOME_ENV, "~/env-seekdeep")]);
        assert_eq!(
            resolve_seekdeep_home(Some(OsStr::new("/tmp/explicit-seekdeep")), &env).unwrap(),
            Path::new("/tmp/explicit-seekdeep")
        );
        assert_eq!(
            resolve_seekdeep_home(None, &env).unwrap(),
            home.join("env-seekdeep")
        );
        assert_eq!(
            resolve_seekdeep_home(None, &HashMap::new()).unwrap(),
            default_seekdeep_home().unwrap()
        );
        assert_eq!(
            resolve_seekdeep_home(None, &environment(&[(SEEKDEEP_HOME_ENV, "   ")])).unwrap(),
            default_seekdeep_home().unwrap()
        );
    }

    #[test]
    fn configured_empty_string_resolves_to_current_directory() {
        assert_eq!(
            resolve_seekdeep_home(Some(OsStr::new("")), &HashMap::new()).unwrap(),
            std::env::current_dir().unwrap().clean()
        );
    }

    #[test]
    fn display_is_symbolic() {
        assert_eq!(
            seekdeep_home_display(&default_seekdeep_home().unwrap()).unwrap(),
            "~/.seekdeep"
        );
        assert_eq!(
            seekdeep_home_display(Path::new("/some/other/root")).unwrap(),
            "$SEEKDEEP_HOME"
        );
    }

    #[tokio::test]
    async fn watcher_path_canonicalizes_alias_and_preserves_missing_suffix() {
        let temporary = tempdir().unwrap();
        let target = temporary.path().join("target");
        let alias = temporary.path().join("alias");
        fs::create_dir(&target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&target, &alias).unwrap();

        assert_eq!(
            canonicalize_watch_path(alias.join("later/config.yml"))
                .await
                .unwrap(),
            fs::canonicalize(&target).unwrap().join("later/config.yml")
        );

        let file = temporary.path().join("file");
        fs::File::create(&file)
            .unwrap()
            .write_all(b"not a directory")
            .unwrap();
        let error = canonicalize_watch_path(file.join("child"))
            .await
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::NotADirectory | io::ErrorKind::Other
        ));
    }
}
