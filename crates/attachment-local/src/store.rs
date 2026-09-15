//! Content-addressed, owner-private local attachment storage.

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use parking_lot::Mutex;
use seekdeep_attachment::{
    AttachmentError, AttachmentId, ImageAttachmentLimits, ImageAttachmentRef, SaveImageAttachment,
    StoredImageAttachment,
};
use seekdeep_util::abort::AbortSignal;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::image::{DetectedImage, detect_image, probe_image};

static DURABLE_HOMES: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
#[cfg(test)]
static SYNCED_DIRECTORIES: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
#[cfg(test)]
static TEST_FS_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn test_fs_lock() -> &'static tokio::sync::Mutex<()> {
    TEST_FS_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Preserved cancellation reason from an attachment read.
#[derive(Clone, Debug, Error, PartialEq)]
#[error("attachment read aborted: {reason}")]
pub struct AttachmentReadAborted {
    /// First JSON-visible reason carried by the signal.
    pub reason: Value,
}

fn abort_error(signal: &AbortSignal) -> anyhow::Error {
    AttachmentReadAborted {
        reason: signal.reason().unwrap_or(Value::Null),
    }
    .into()
}

fn throw_if_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if let Some(signal) = signal.filter(|signal| signal.is_aborted()) {
        return Err(abort_error(signal));
    }
    Ok(())
}

fn digest(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn is_ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn truncate_utf16(value: &str, max_units: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|character| {
            let next = units + character.len_utf16();
            if next > max_units {
                return false;
            }
            units = next;
            true
        })
        .collect()
}

fn display_name(value: Option<&str>) -> Option<String> {
    let value = value?;
    let leaf = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| !matches!(*character, '\u{0000}'..='\u{001F}' | '\u{007F}'))
        .collect::<String>();
    let clean = leaf.trim_matches(is_ecmascript_whitespace);
    let clean = truncate_utf16(clean, 255);
    (!clean.is_empty()).then_some(clean)
}

fn object_path(root: &Path, sha256: &str) -> PathBuf {
    root.join("objects").join(&sha256[..2]).join(sha256)
}

fn ensure_reference(reference: &ImageAttachmentRef) -> Result<&str, AttachmentError> {
    let value = reference.attachment_id.as_str();
    let Some(sha256) = value.strip_prefix("sha256:") else {
        return Err(AttachmentError::new(
            "Attachment reference is invalid.",
            "INVALID_ATTACHMENT_REF",
        ));
    };
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AttachmentError::new(
            "Attachment reference is invalid.",
            "INVALID_ATTACHMENT_REF",
        ));
    }
    Ok(sha256)
}

fn inspect_metadata(
    data: &[u8],
    declared_media_type: seekdeep_attachment::ImageMediaType,
    max_pixels: Option<u64>,
) -> Result<DetectedImage, AttachmentError> {
    if data.is_empty() {
        return Err(AttachmentError::new("Image is empty.", "INVALID_IMAGE"));
    }
    let detected = detect_image(data, max_pixels)?;
    if detected.media_type != declared_media_type {
        return Err(AttachmentError::new(
            "Declared image type does not match its bytes.",
            "IMAGE_TYPE_MISMATCH",
        ));
    }
    Ok(detected)
}

/// Runs the full admission policy without touching storage.
///
/// # Errors
///
/// Returns stable attachment validation errors for oversized, malformed,
/// unsupported, mismatched, or pixel-amplifying images.
pub fn validate_image_file(
    input: &SaveImageAttachment,
    limits: &ImageAttachmentLimits,
) -> Result<(), AttachmentError> {
    if input.data.len() as u64 > limits.max_image_bytes {
        return Err(AttachmentError::new(
            "Image exceeds the configured byte limit.",
            "IMAGE_TOO_LARGE",
        ));
    }
    inspect_metadata(&input.data, input.media_type, Some(limits.max_image_pixels))?;
    Ok(())
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn open_private_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private_file(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    SYNCED_DIRECTORIES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .push(path.to_path_buf());
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

fn ensure_durable_directory(path: &Path, boundary: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    set_directory_permissions(path)?;
    let mut level = path.to_path_buf();
    while level != boundary {
        let Some(parent) = level.parent() else {
            return Ok(());
        };
        sync_directory(parent)?;
        if parent == level {
            return Ok(());
        }
        level = parent.to_path_buf();
    }
    Ok(())
}

fn ensure_durable_home(path: &Path) -> io::Result<PathBuf> {
    let home = path.to_path_buf();
    let durable = DURABLE_HOMES.get_or_init(|| Mutex::new(HashSet::new()));
    let mut durable = durable.lock();
    if !durable.contains(&home) {
        let boundary = home.ancestors().last().unwrap_or(Path::new("/"));
        ensure_durable_directory(&home, boundary)?;
        durable.insert(home.clone());
    }
    Ok(home)
}

fn save_image_file_blocking(
    root: &Path,
    input: &SaveImageAttachment,
    limits: &ImageAttachmentLimits,
) -> anyhow::Result<ImageAttachmentRef> {
    if input.data.len() as u64 > limits.max_image_bytes {
        return Err(AttachmentError::new(
            "Image exceeds the configured byte limit.",
            "IMAGE_TOO_LARGE",
        )
        .into());
    }
    let metadata = inspect_metadata(&input.data, input.media_type, Some(limits.max_image_pixels))
        .map_err(anyhow::Error::from)?;
    let sha256 = digest(&input.data);
    let bucket = root.join("objects").join(&sha256[..2]);
    let staging = root.join("tmp");
    let Some(home) = root.parent().and_then(Path::parent) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attachment storage root has no harness-home ancestor",
        )
        .into());
    };
    // These failures precede the source's publication try/catch and retain
    // their native filesystem identity.
    let boundary = ensure_durable_home(home)?;
    ensure_durable_directory(&bucket, &boundary)?;
    ensure_durable_directory(&staging, &boundary)?;
    let temporary = staging.join(Uuid::new_v4().to_string());
    let target = object_path(root, &sha256);

    let operation = || -> Result<(), AttachmentError> {
        use std::io::Write as _;

        let mut file = open_private_file(&temporary).map_err(write_failed)?;
        file.write_all(&input.data).map_err(write_failed)?;
        file.sync_all().map_err(write_failed)?;
        drop(file);

        match std::fs::hard_link(&temporary, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = std::fs::read(&target).map_err(write_failed)?;
                if digest(&existing) != sha256 {
                    return Err(AttachmentError::new(
                        "Stored attachment failed integrity verification.",
                        "ATTACHMENT_CORRUPT",
                    ));
                }
            }
            Err(error) => return Err(write_failed(error)),
        }
        sync_directory(&bucket).map_err(write_failed)?;
        sync_directory(&root.join("objects")).map_err(write_failed)?;
        std::fs::remove_file(&temporary).map_err(write_failed)?;
        Ok(())
    };

    if let Err(error) = operation() {
        match std::fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => {}
            Err(cleanup) => return Err(write_failed(cleanup).into()),
        }
        return Err(error.into());
    }

    Ok(ImageAttachmentRef {
        attachment_id: AttachmentId::new(format!("sha256:{sha256}")),
        media_type: metadata.media_type,
        bytes: input.data.len() as u64,
        width: metadata.width,
        height: metadata.height,
        name: display_name(input.name.as_deref()),
    })
}

fn write_failed(error: io::Error) -> AttachmentError {
    AttachmentError::with_source(
        "Unable to persist image attachment.",
        "ATTACHMENT_WRITE_FAILED",
        error,
    )
}

/// Saves and verifies immutable image bytes below a versioned attachment root.
///
/// # Errors
///
/// Returns validation errors unchanged, `ATTACHMENT_CORRUPT` for a conflicting
/// object, or `ATTACHMENT_WRITE_FAILED` for unexpected publication failures.
pub async fn save_image_file(
    root: impl AsRef<Path>,
    input: SaveImageAttachment,
    limits: ImageAttachmentLimits,
) -> anyhow::Result<ImageAttachmentRef> {
    let root = root.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || save_image_file_blocking(&root, &input, &limits))
        .await
        .map_err(|error| {
            AttachmentError::with_source(
                "Unable to persist image attachment.",
                "ATTACHMENT_WRITE_FAILED",
                error,
            )
        })?
}

async fn cancellable_read(path: &Path, signal: Option<&AbortSignal>) -> anyhow::Result<Vec<u8>> {
    throw_if_aborted(signal)?;
    let read = tokio::fs::read(path);
    let data = if let Some(signal) = signal {
        tokio::select! {
            biased;
            () = signal.cancelled() => return Err(abort_error(signal)),
            result = read => result,
        }
    } else {
        read.await
    };
    match data {
        Ok(data) => Ok(data),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(AttachmentError::new(
            "Attachment object is missing.",
            "ATTACHMENT_NOT_FOUND",
        )
        .into()),
        Err(error) => Err(AttachmentError::with_source(
            "Unable to read image attachment.",
            "ATTACHMENT_READ_FAILED",
            error,
        )
        .into()),
    }
}

/// Reads and verifies one content-addressed image.
///
/// # Errors
///
/// Preserves cancellation as [`AttachmentReadAborted`], returns stable
/// attachment errors for invalid, missing, unreadable, corrupt, or
/// metadata-mismatched objects, and never re-applies current admission limits.
pub async fn read_image_file(
    root: impl AsRef<Path>,
    reference: &ImageAttachmentRef,
    signal: Option<AbortSignal>,
) -> anyhow::Result<StoredImageAttachment> {
    throw_if_aborted(signal.as_ref())?;
    let sha256 = ensure_reference(reference)?;
    let data = cancellable_read(&object_path(root.as_ref(), sha256), signal.as_ref()).await?;
    throw_if_aborted(signal.as_ref())?;
    if digest(&data) != sha256 {
        return Err(AttachmentError::new(
            "Stored attachment failed integrity verification.",
            "ATTACHMENT_CORRUPT",
        )
        .into());
    }
    let metadata = probe_image(&data)?;
    throw_if_aborted(signal.as_ref())?;
    if metadata.media_type != reference.media_type
        || data.len() as u64 != reference.bytes
        || metadata.width != reference.width
        || metadata.height != reference.height
    {
        return Err(AttachmentError::new(
            "Stored attachment metadata does not match its reference.",
            "ATTACHMENT_CORRUPT",
        )
        .into());
    }
    Ok(StoredImageAttachment {
        reference: reference.clone(),
        data,
    })
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tempfile::TempDir;

    use super::*;

    fn png() -> Vec<u8> {
        STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap()
    }

    fn limits() -> ImageAttachmentLimits {
        ImageAttachmentLimits {
            max_image_bytes: 1024,
            max_images_per_message: 2,
            max_message_image_bytes: 2048,
            max_image_pixels: 16,
            media_types: vec![
                seekdeep_attachment::ImageMediaType::Png,
                seekdeep_attachment::ImageMediaType::Jpeg,
                seekdeep_attachment::ImageMediaType::Webp,
                seekdeep_attachment::ImageMediaType::Gif,
            ],
        }
    }

    fn input(data: Vec<u8>) -> SaveImageAttachment {
        SaveImageAttachment {
            data,
            media_type: seekdeep_attachment::ImageMediaType::Png,
            name: None,
        }
    }

    fn attachment_code(error: &anyhow::Error) -> Option<&str> {
        error
            .downcast_ref::<AttachmentError>()
            .map(|error| error.code.as_str())
    }

    async fn save(root: &Path, input: SaveImageAttachment) -> ImageAttachmentRef {
        save_image_file(root, input, limits()).await.unwrap()
    }

    #[test]
    fn display_names_strip_both_path_styles_controls_and_js_whitespace() {
        assert_eq!(
            display_name(Some(r"C:\private/path\pixel.png")),
            Some("pixel.png".to_owned())
        );
        assert_eq!(display_name(Some("\u{feff}\0 \n")), None);
        let long = "💠".repeat(200);
        assert_eq!(
            display_name(Some(&long)).unwrap().encode_utf16().count(),
            254
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn syncs_every_ancestor_and_publishes_private_content_addressed_object() {
        use std::os::unix::fs::PermissionsExt as _;

        let _guard = test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("attachments/v1");
        let sha256 = digest(&png());
        let objects = root.join("objects");
        let bucket = objects.join(&sha256[..2]);
        SYNCED_DIRECTORIES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .clear();

        let mut named = input(png());
        named.name = Some("/private/tmp/pixel.png".to_owned());
        let first = save(&root, named).await;
        let second = save(&root, input(png())).await;
        let target = object_path(&root, &sha256);

        assert_eq!(first.attachment_id.as_str(), format!("sha256:{sha256}"));
        assert_eq!(first.media_type, seekdeep_attachment::ImageMediaType::Png);
        assert_eq!(first.bytes, png().len() as u64);
        assert_eq!((first.width, first.height), (1, 1));
        assert_eq!(first.name.as_deref(), Some("pixel.png"));
        assert_eq!(second.attachment_id, first.attachment_id);
        assert_eq!(std::fs::read(&target).unwrap(), png());
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for directory in [&bucket, &objects, &root, &root.join("tmp")] {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700,
                "{}",
                directory.display()
            );
        }

        let mut expected = Vec::new();
        let mut level = temp.path().to_path_buf();
        while level.parent().is_some() {
            level = level.parent().unwrap().to_path_buf();
            expected.push(level.clone());
        }
        expected.extend([
            objects.clone(),
            root.clone(),
            temp.path().join("attachments"),
            temp.path().to_path_buf(),
            root.clone(),
            temp.path().join("attachments"),
            temp.path().to_path_buf(),
            bucket.clone(),
            objects.clone(),
            // The equal-byte save re-walks bucket and staging and repeats the
            // publication sync pair, while the process-proven home is cached.
            objects.clone(),
            root.clone(),
            temp.path().join("attachments"),
            temp.path().to_path_buf(),
            root.clone(),
            temp.path().join("attachments"),
            temp.path().to_path_buf(),
            bucket,
            objects,
        ]);
        assert_eq!(*SYNCED_DIRECTORIES.get().unwrap().lock(), expected);
        assert!(root.join("tmp").read_dir().unwrap().next().is_none());
    }

    #[tokio::test]
    async fn rejects_validation_failures_without_creating_storage() {
        let _guard = test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("attachments/v1");

        let empty = save_image_file(&root, input(Vec::new()), limits())
            .await
            .unwrap_err();
        assert_eq!(attachment_code(&empty), Some("INVALID_IMAGE"));
        let malformed = save_image_file(&root, input(vec![1, 2, 3]), limits())
            .await
            .unwrap_err();
        assert_eq!(attachment_code(&malformed), Some("INVALID_IMAGE"));
        let mut mismatched = input(png());
        mismatched.media_type = seekdeep_attachment::ImageMediaType::Jpeg;
        let error = save_image_file(&root, mismatched, limits())
            .await
            .unwrap_err();
        assert_eq!(attachment_code(&error), Some("IMAGE_TYPE_MISMATCH"));
        let mut tiny = limits();
        tiny.max_image_bytes = 1;
        let error = save_image_file(&root, input(png()), tiny)
            .await
            .unwrap_err();
        assert_eq!(attachment_code(&error), Some("IMAGE_TOO_LARGE"));
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn creates_a_missing_nested_home_and_keeps_admitted_history_readable() {
        let _guard = test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("missing/nested/home/attachments/v1");
        let reference = save(&root, input(png())).await;
        assert_eq!(
            read_image_file(&root, &reference, None).await.unwrap().data,
            png()
        );
    }

    #[tokio::test]
    async fn reads_ignore_new_limits_but_fail_closed_for_invalid_missing_and_corrupt_objects() {
        let _guard = test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("attachments/v1");
        let reference = save(&root, input(png())).await;
        let sha256 = reference
            .attachment_id
            .as_str()
            .trim_start_matches("sha256:");
        let target = object_path(&root, sha256);

        let stored = read_image_file(&root, &reference, None).await.unwrap();
        assert_eq!(stored.reference, reference);
        assert_eq!(stored.data, png());
        let live_signal = AbortSignal::default();
        assert_eq!(
            read_image_file(&root, &reference, Some(live_signal))
                .await
                .unwrap()
                .data,
            png()
        );

        let mut invalid = reference.clone();
        invalid.attachment_id = AttachmentId::new("bad");
        let error = read_image_file(&root, &invalid, None).await.unwrap_err();
        assert_eq!(attachment_code(&error), Some("INVALID_ATTACHMENT_REF"));

        let mut forged = reference.clone();
        forged.width += 1;
        let error = read_image_file(&root, &forged, None).await.unwrap_err();
        assert_eq!(attachment_code(&error), Some("ATTACHMENT_CORRUPT"));

        std::fs::write(&target, [1, 2, 3]).unwrap();
        let error = read_image_file(&root, &reference, None).await.unwrap_err();
        assert_eq!(attachment_code(&error), Some("ATTACHMENT_CORRUPT"));

        std::fs::remove_file(&target).unwrap();
        let error = read_image_file(&root, &reference, None).await.unwrap_err();
        assert_eq!(attachment_code(&error), Some("ATTACHMENT_NOT_FOUND"));

        std::fs::create_dir(&target).unwrap();
        let error = read_image_file(&root, &reference, None).await.unwrap_err();
        assert_eq!(attachment_code(&error), Some("ATTACHMENT_READ_FAILED"));
    }

    #[tokio::test]
    async fn rejects_conflicts_maps_publication_failures_and_preserves_cancellation() {
        let _guard = test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("attachments/v1");
        let sha256 = digest(&png());
        let target = object_path(&root, &sha256);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, [1, 2, 3]).unwrap();
        let error = save_image_file(&root, input(png()), limits())
            .await
            .unwrap_err();
        assert_eq!(attachment_code(&error), Some("ATTACHMENT_CORRUPT"));

        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&target).unwrap();
        let error = save_image_file(&root, input(png()), limits())
            .await
            .unwrap_err();
        assert_eq!(attachment_code(&error), Some("ATTACHMENT_WRITE_FAILED"));

        std::fs::remove_dir(&target).unwrap();
        let reference = save(&root, input(png())).await;
        let signal = AbortSignal::default();
        signal.abort_with_reason(serde_json::json!({ "code": "USER_CANCEL" }));
        let error = read_image_file(&root, &reference, Some(signal))
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<AttachmentReadAborted>()
                .map(|error| &error.reason),
            Some(&serde_json::json!({ "code": "USER_CANCEL" }))
        );
        assert!(attachment_code(&error).is_none());
    }

    #[tokio::test]
    async fn concurrent_equal_writes_deduplicate_and_leave_no_staging_files() {
        let _guard = test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("attachments/v1");
        let (first, second) = tokio::join!(
            save_image_file(&root, input(png()), limits()),
            save_image_file(&root, input(png()), limits())
        );
        assert_eq!(first.unwrap().attachment_id, second.unwrap().attachment_id);
        assert!(root.join("tmp").read_dir().unwrap().next().is_none());
    }
}
