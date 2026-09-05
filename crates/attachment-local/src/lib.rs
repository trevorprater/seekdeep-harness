//! Private content-addressed attachment storage below `SEEKDEEP_HOME`.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use seekdeep_attachment::{
    AttachmentBackend, AttachmentStore, ImageAttachmentLimits, ImageAttachmentRef, ImageMediaType,
    SaveImageAttachment, StoredImageAttachment,
};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_util::{abort::AbortSignal, home_paths::resolve_process_seekdeep_home};
use serde::{Deserialize, Serialize};

pub mod image;
pub mod store;

pub use image::{DetectedImage, detect_image, probe_image};
pub use store::{AttachmentReadAborted, read_image_file, save_image_file, validate_image_file};

/// Default maximum encoded bytes for one image.
pub const DEFAULT_MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// Default maximum images in one prompt.
pub const DEFAULT_MAX_IMAGES_PER_MESSAGE: u64 = 20;
/// Default maximum aggregate image bytes in one prompt.
pub const DEFAULT_MAX_MESSAGE_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
/// Default maximum intrinsic pixels for one image.
pub const DEFAULT_MAX_IMAGE_PIXELS: u64 = 40_000_000;
/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "attachment-local";
/// Local attachment storage has no service prerequisites.
pub const PLUGIN_INJECT: &[&str] = &[];

/// Local attachment backend configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalAttachmentConfig {
    /// Explicit harness home; omitted follows `SEEKDEEP_HOME`, then `~/.seekdeep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seekdeep_home: Option<PathBuf>,
    /// Maximum encoded bytes accepted for one image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_image_bytes: Option<u64>,
    /// Maximum image count accepted in one submitted message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_images_per_message: Option<u64>,
    /// Maximum aggregate encoded image bytes accepted in one submitted message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_message_image_bytes: Option<u64>,
    /// Maximum intrinsic width multiplied by height accepted for one image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_image_pixels: Option<u64>,
}

fn positive(value: Option<u64>, default: u64, field: &str) -> anyhow::Result<u64> {
    let value = value.unwrap_or(default);
    anyhow::ensure!(value > 0, "attachment-local: {field} must be at least 1");
    Ok(value)
}

/// Persistent content-addressed local attachment backend.
pub struct LocalAttachmentStore {
    /// Absolute versioned storage root.
    pub root: PathBuf,
    image_limits: ImageAttachmentLimits,
}

impl fmt::Debug for LocalAttachmentStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalAttachmentStore")
            .field("root", &self.root)
            .field("image_limits", &self.image_limits)
            .finish()
    }
}

impl LocalAttachmentStore {
    /// Resolves configuration and constructs a backend without creating storage.
    ///
    /// # Errors
    ///
    /// Returns for an unavailable process home/current directory or any zero limit.
    pub fn new(config: &LocalAttachmentConfig) -> anyhow::Result<Self> {
        let configured = config.seekdeep_home.as_deref().map(Path::as_os_str);
        let home = resolve_process_seekdeep_home(configured)?;
        let root = home.join("attachments").join("v1");
        Ok(Self {
            root,
            image_limits: ImageAttachmentLimits {
                max_image_bytes: positive(
                    config.max_image_bytes,
                    DEFAULT_MAX_IMAGE_BYTES,
                    "maxImageBytes",
                )?,
                max_images_per_message: positive(
                    config.max_images_per_message,
                    DEFAULT_MAX_IMAGES_PER_MESSAGE,
                    "maxImagesPerMessage",
                )?,
                max_message_image_bytes: positive(
                    config.max_message_image_bytes,
                    DEFAULT_MAX_MESSAGE_IMAGE_BYTES,
                    "maxMessageImageBytes",
                )?,
                max_image_pixels: positive(
                    config.max_image_pixels,
                    DEFAULT_MAX_IMAGE_PIXELS,
                    "maxImagePixels",
                )?,
                media_types: vec![
                    ImageMediaType::Png,
                    ImageMediaType::Jpeg,
                    ImageMediaType::Webp,
                    ImageMediaType::Gif,
                ],
            },
        })
    }
}

#[async_trait]
impl AttachmentBackend for LocalAttachmentStore {
    fn image_limits(&self) -> &ImageAttachmentLimits {
        &self.image_limits
    }

    async fn validate_image(&self, input: &SaveImageAttachment) -> anyhow::Result<()> {
        let input = input.clone();
        let limits = self.image_limits.clone();
        tokio::task::spawn_blocking(move || validate_image_file(&input, &limits))
            .await
            .map_err(anyhow::Error::from)??;
        Ok(())
    }

    async fn save_image(&self, input: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef> {
        save_image_file(&self.root, input, self.image_limits.clone()).await
    }

    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        read_image_file(&self.root, reference, signal).await
    }
}

/// Installs the local backend as the lifecycle-owned `attachments` service.
///
/// # Errors
///
/// Returns configuration, path-resolution, or Cordis service-registration failures.
pub fn install(
    context: &Context,
    config: &LocalAttachmentConfig,
) -> anyhow::Result<Arc<LocalAttachmentStore>> {
    let backend = Arc::new(LocalAttachmentStore::new(config)?);
    let service = Arc::new(AttachmentStore::new(backend.clone()));
    service.provide(context)?;
    Ok(backend)
}

/// Builds the Loader-compatible local attachment plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        |context, config| {
            Box::pin(async move {
                let config: LocalAttachmentConfig = serde_json::from_value(config)?;
                install(&context, &config)?;
                Ok(())
            })
        },
    )
}

/// Registers the local attachment package's explained empty invariant companion.
///
/// Immutable writes and verified reads are enforced directly at the backend boundary.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "seekdeep-attachment-local",
        InvariantInstaller::new(["attachments"], |_, _| async { Ok(()) }),
    )
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use seekdeep_attachment::{ATTACHMENTS, AttachmentError};
    use tempfile::TempDir;

    use super::*;

    fn png() -> Vec<u8> {
        STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap()
    }

    fn config(temp: &TempDir) -> LocalAttachmentConfig {
        LocalAttachmentConfig {
            seekdeep_home: Some(temp.path().to_path_buf()),
            ..LocalAttachmentConfig::default()
        }
    }

    fn input(data: Vec<u8>) -> SaveImageAttachment {
        SaveImageAttachment {
            data,
            media_type: ImageMediaType::Png,
            name: None,
        }
    }

    #[test]
    fn defaults_are_explicit_and_constructor_touches_no_storage() {
        let temp = TempDir::new().unwrap();
        let backend = LocalAttachmentStore::new(&config(&temp)).unwrap();
        assert_eq!(
            backend.image_limits,
            ImageAttachmentLimits {
                max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
                max_images_per_message: DEFAULT_MAX_IMAGES_PER_MESSAGE,
                max_message_image_bytes: DEFAULT_MAX_MESSAGE_IMAGE_BYTES,
                max_image_pixels: DEFAULT_MAX_IMAGE_PIXELS,
                media_types: vec![
                    ImageMediaType::Png,
                    ImageMediaType::Jpeg,
                    ImageMediaType::Webp,
                    ImageMediaType::Gif,
                ],
            }
        );
        assert!(!backend.root.exists());
        for field in [
            "maxImageBytes",
            "maxImagesPerMessage",
            "maxMessageImageBytes",
            "maxImagePixels",
        ] {
            let mut invalid = config(&temp);
            match field {
                "maxImageBytes" => invalid.max_image_bytes = Some(0),
                "maxImagesPerMessage" => invalid.max_images_per_message = Some(0),
                "maxMessageImageBytes" => invalid.max_message_image_bytes = Some(0),
                _ => invalid.max_image_pixels = Some(0),
            }
            assert!(
                LocalAttachmentStore::new(&invalid)
                    .unwrap_err()
                    .to_string()
                    .contains(field)
            );
        }
    }

    #[tokio::test]
    async fn service_installs_saves_reads_and_disposes() {
        let _guard = crate::store::test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let context = Context::new();
        let backend = install(&context, &config(&temp)).unwrap();
        let service = context.get(ATTACHMENTS).expect("service");
        let reference = service.save_image(input(png())).await.unwrap();
        let stored = service.read_image(&reference, None).await.unwrap();
        assert_eq!(stored.reference, reference);
        assert_eq!(stored.data, png());
        assert_eq!(backend.root, temp.path().join("attachments/v1"));
        context.fiber().dispose().await.unwrap();
        assert!(context.get(ATTACHMENTS).is_none());
    }

    #[tokio::test]
    async fn validation_never_creates_storage() {
        let _guard = crate::store::test_fs_lock().lock().await;
        let temp = TempDir::new().unwrap();
        let backend = LocalAttachmentStore::new(&config(&temp)).unwrap();
        let error = backend
            .validate_image(&input(vec![1, 2, 3]))
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<AttachmentError>()
                .map(|error| error.code.as_str()),
            Some("INVALID_IMAGE")
        );
        backend.validate_image(&input(png())).await.unwrap();
        assert!(!backend.root.exists());
    }

    #[tokio::test]
    async fn invariant_waits_for_attachment_service_and_releases_ownership() {
        let context = Context::new();
        let registry = Arc::new(
            InvariantRegistry::new(&context, &seekdeep_invariants::InvariantConfig::default())
                .unwrap(),
        );
        let registration = register_invariant(&registry).unwrap();
        assert!(registry.is_registered("seekdeep-attachment-local"));
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                registration.await_ready(),
            )
            .await
            .is_err()
        );

        let backend = Arc::new(
            LocalAttachmentStore::new(&LocalAttachmentConfig {
                seekdeep_home: Some(tempfile::tempdir().unwrap().path().to_path_buf()),
                ..LocalAttachmentConfig::default()
            })
            .unwrap(),
        );
        let service = Arc::new(AttachmentStore::new(backend));
        service.provide(&context).unwrap();
        registration.await_ready().await.unwrap();
        registration.dispose().await.unwrap();
        assert!(!registry.is_registered("seekdeep-attachment-local"));
    }
}
