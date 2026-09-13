//! Cordis-free local spill storage mechanics.

use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use parking_lot::Mutex;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

static PRIVATE_ROOT: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// Returns the lazily created private per-process default spill root.
///
/// # Errors
///
/// Returns the operating system's temporary-directory creation failure.
pub fn private_root() -> io::Result<PathBuf> {
    let root = PRIVATE_ROOT.get_or_init(|| Mutex::new(None));
    let mut root = root.lock();
    if let Some(path) = root.as_ref() {
        return Ok(path.clone());
    }
    let path = tempfile::Builder::new()
        .prefix("seekdeep-spill-")
        .tempdir()?
        .keep();
    create_private_directory(&path)?;
    *root = Some(path.clone());
    Ok(path)
}

/// Encodes a Rust string injectively as one filesystem-safe UTF-16 segment.
#[must_use]
pub fn encode_segment(raw: &str) -> String {
    if raw.is_empty() {
        return "~".to_owned();
    }
    if raw == "." {
        return "~002E".to_owned();
    }
    if raw == ".." {
        return "~002E~002E".to_owned();
    }
    let mut output = String::new();
    for code in raw.encode_utf16() {
        if code != u16::from(b'~')
            && ((u16::from(b'A')..=u16::from(b'Z')).contains(&code)
                || (u16::from(b'a')..=u16::from(b'z')).contains(&code)
                || (u16::from(b'0')..=u16::from(b'9')).contains(&code)
                || matches!(code, 0x2e | 0x5f | 0x2d))
        {
            if let Some(character) = char::from_u32(u32::from(code)) {
                output.push(character);
            }
        } else {
            use std::fmt::Write as _;
            let _ = write!(output, "~{code:04X}");
        }
    }
    output
}

/// Returns `<root>/session-<first 12 hex sha256(session id)>`.
#[must_use]
pub fn session_dir(root: impl AsRef<Path>, session_id: &str) -> PathBuf {
    let digest = hex::encode(Sha256::digest(session_id.as_bytes()));
    root.as_ref().join(format!("session-{}", &digest[..12]))
}

/// Resolved inputs for one local text save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveTextOptions {
    /// Spill root directory.
    pub root: PathBuf,
    /// Owning session identity.
    pub session_id: String,
    /// Caller-suggested, untrusted base name.
    pub suggested_name: String,
    /// Full text to persist.
    pub content: String,
}

/// One written local spill file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedText {
    /// Absolute file path.
    pub path: PathBuf,
    /// Exact UTF-8 bytes written.
    pub bytes: u64,
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
}

fn save_text_file_blocking(options: &SaveTextOptions) -> io::Result<SavedText> {
    use std::io::Write as _;

    let directory = session_dir(&options.root, &options.session_id);
    create_private_directory(&directory)?;
    let safe_name = encode_segment(&options.suggested_name);
    let random = hex::encode(&Uuid::new_v4().as_bytes()[..6]);
    let path = directory.join(format!("{random}-{safe_name}"));
    let bytes = options.content.len() as u64;
    let mut file = create_private_file(&path)?;
    file.write_all(options.content.as_bytes())?;
    drop(file);
    Ok(SavedText { path, bytes })
}

/// Writes full text to one exclusive owner-only session-scoped file.
///
/// # Errors
///
/// Returns directory creation, exclusive-open, write, or task-join failures.
pub async fn save_text_file(options: SaveTextOptions) -> anyhow::Result<SavedText> {
    Ok(tokio::task::spawn_blocking(move || save_text_file_blocking(&options)).await??)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_encoding_matches_every_source_edge_and_utf16_units() {
        assert_eq!(encode_segment("web_fetch.txt"), "web_fetch.txt");
        assert_eq!(encode_segment("a-B_9.z"), "a-B_9.z");
        assert_eq!(encode_segment("../etc/passwd"), "..~002Fetc~002Fpasswd");
        assert_eq!(encode_segment("a/b"), "a~002Fb");
        assert_eq!(encode_segment("~"), "~007E");
        assert_eq!(encode_segment("."), "~002E");
        assert_eq!(encode_segment(".."), "~002E~002E");
        assert_eq!(encode_segment(""), "~");
        assert_eq!(encode_segment("💠"), "~D83D~DCA0");
    }
}
