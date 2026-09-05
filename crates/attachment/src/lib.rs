//! Durable immutable attachment storage seam.

use std::{error::Error, fmt, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_util::abort::AbortSignal;
use serde::{Deserialize, Serialize};

seekdeep_util::string_brand!(
    /// Opaque content-addressed identifier for one immutable attachment object.
    pub struct AttachmentId;
);

/// Raster image formats accepted by the version-one attachment path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageMediaType {
    /// Portable Network Graphics.
    #[serde(rename = "image/png")]
    Png,
    /// Joint Photographic Experts Group image.
    #[serde(rename = "image/jpeg")]
    Jpeg,
    /// WebP image.
    #[serde(rename = "image/webp")]
    Webp,
    /// Graphics Interchange Format image.
    #[serde(rename = "image/gif")]
    Gif,
}

impl ImageMediaType {
    /// Returns the canonical MIME type.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }
}

impl fmt::Display for ImageMediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Durable, serializable metadata for one immutable image object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageAttachmentRef {
    /// Opaque storage identifier; never a filesystem path or bearer URL.
    pub attachment_id: AttachmentId,
    /// Media type verified from the stored bytes.
    pub media_type: ImageMediaType,
    /// Exact encoded byte length.
    pub bytes: u64,
    /// Intrinsic encoded width in pixels.
    pub width: u64,
    /// Intrinsic encoded height in pixels.
    pub height: u64,
    /// Optional display name stripped of local path information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Deployment-resolved limits used by upload admission and request buffering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageAttachmentLimits {
    /// Maximum encoded bytes accepted for one image.
    pub max_image_bytes: u64,
    /// Maximum image count accepted in one message.
    pub max_images_per_message: u64,
    /// Maximum aggregate encoded image bytes accepted in one message.
    pub max_message_image_bytes: u64,
    /// Maximum intrinsic pixels accepted for one image.
    pub max_image_pixels: u64,
    /// Accepted image media types in deployment order.
    pub media_types: Vec<ImageMediaType>,
}

/// Request to validate and durably commit one image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveImageAttachment {
    /// Complete encoded bytes.
    pub data: Vec<u8>,
    /// Caller-declared media type, checked against fully decoded bytes.
    pub media_type: ImageMediaType,
    /// Optional browser or provider display name; never interpreted as a path.
    pub name: Option<String>,
}

/// Stored image bytes returned after reference and digest verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredImageAttachment {
    /// Canonical durable reference.
    pub reference: ImageAttachmentRef,
    /// Verified complete encoded bytes.
    pub data: Vec<u8>,
}

/// Stable attachment failure suitable for host RPC error mapping.
#[derive(Debug)]
pub struct AttachmentError {
    /// Human-readable failure without raw bytes or host paths.
    pub message: String,
    /// Stable machine-routing failure code.
    pub code: String,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl AttachmentError {
    /// Creates an attachment failure without a chained cause.
    #[must_use]
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            source: None,
        }
    }

    /// Creates an attachment failure with its original cause.
    #[must_use]
    pub fn with_source(
        message: impl Into<String>,
        code: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AttachmentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Backend contract for immutable attachment storage.
///
/// Implementations fully validate bytes before publishing a reference. Batch
/// callers first invoke [`Self::validate_image`] for every member, then save
/// them, so one malformed member cannot strand earlier unreferenced objects.
#[async_trait]
pub trait AttachmentBackend: Send + Sync + 'static {
    /// Deployment-resolved image policy used by authoritative and fast-path validation.
    fn image_limits(&self) -> &ImageAttachmentLimits;

    /// Validates one encoded image without persisting it.
    async fn validate_image(&self, input: &SaveImageAttachment) -> anyhow::Result<()>;

    /// Validates and durably commits one image before its owning session event is appended.
    async fn save_image(&self, input: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef>;

    /// Reads one image and verifies that bytes still match its recorded reference.
    ///
    /// Implementations preserve a supplied cancellation reason rather than
    /// translating it into a storage failure.
    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment>;
}

/// Immutable attachment service exposed through `ctx.attachments`.
#[derive(Clone)]
pub struct AttachmentStore {
    backend: Arc<dyn AttachmentBackend>,
}

impl fmt::Debug for AttachmentStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentStore")
            .finish_non_exhaustive()
    }
}

impl AttachmentStore {
    /// Wraps one backend implementation.
    #[must_use]
    pub fn new(backend: Arc<dyn AttachmentBackend>) -> Self {
        Self { backend }
    }

    /// Deployment-resolved image policy.
    #[must_use]
    pub fn image_limits(&self) -> &ImageAttachmentLimits {
        self.backend.image_limits()
    }

    /// Validates one encoded image without persisting it.
    ///
    /// # Errors
    ///
    /// Returns the backend's validation failure unchanged.
    pub async fn validate_image(&self, input: &SaveImageAttachment) -> anyhow::Result<()> {
        self.backend.validate_image(input).await
    }

    /// Validates and durably commits one image.
    ///
    /// # Errors
    ///
    /// Returns the backend's validation or durable-publication failure unchanged.
    pub async fn save_image(
        &self,
        input: SaveImageAttachment,
    ) -> anyhow::Result<ImageAttachmentRef> {
        self.backend.save_image(input).await
    }

    /// Reads and verifies one immutable image.
    ///
    /// # Errors
    ///
    /// Returns cancellation, backend-read, or integrity failures unchanged.
    pub async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        self.backend.read_image(reference, signal).await
    }

    /// Provides this store on the `attachments` service slot for the current fiber.
    ///
    /// # Errors
    ///
    /// Returns ordinary Cordis service-registration failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(ATTACHMENTS, self.clone())
    }
}

/// Typed Cordis service slot corresponding to `ctx.attachments`.
pub const ATTACHMENTS: ServiceKey<AttachmentStore> = ServiceKey::new("attachments");

/// Registers the attachment package's explained empty invariant companion.
///
/// The seam retains no mutable relationship: concrete stores own immutable
/// object validation and the service registry owns lifecycle pairing.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "@deepseek-ai/seekdeep-attachment",
        InvariantInstaller::noop(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    #[derive(Debug)]
    struct RecordingBackend {
        limits: ImageAttachmentLimits,
        validations: AtomicUsize,
    }

    impl RecordingBackend {
        fn new() -> Self {
            Self {
                limits: ImageAttachmentLimits {
                    max_image_bytes: 5,
                    max_images_per_message: 2,
                    max_message_image_bytes: 10,
                    max_image_pixels: 20,
                    media_types: vec![ImageMediaType::Png],
                },
                validations: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl AttachmentBackend for RecordingBackend {
        fn image_limits(&self) -> &ImageAttachmentLimits {
            &self.limits
        }

        async fn validate_image(&self, _input: &SaveImageAttachment) -> anyhow::Result<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn save_image(
            &self,
            input: SaveImageAttachment,
        ) -> anyhow::Result<ImageAttachmentRef> {
            Ok(ImageAttachmentRef {
                attachment_id: AttachmentId::new("sha256:abc"),
                media_type: input.media_type,
                bytes: input.data.len() as u64,
                width: 1,
                height: 1,
                name: input.name,
            })
        }

        async fn read_image(
            &self,
            reference: &ImageAttachmentRef,
            signal: Option<AbortSignal>,
        ) -> anyhow::Result<StoredImageAttachment> {
            if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
                anyhow::bail!(
                    "cancelled: {}",
                    signal.and_then(|value| value.reason()).unwrap()
                );
            }
            Ok(StoredImageAttachment {
                reference: reference.clone(),
                data: vec![1, 2, 3],
            })
        }
    }

    fn input() -> SaveImageAttachment {
        SaveImageAttachment {
            data: vec![1, 2, 3],
            media_type: ImageMediaType::Png,
            name: Some("image.png".to_owned()),
        }
    }

    #[test]
    fn attachment_reference_has_exact_wire_shape() {
        let reference = ImageAttachmentRef {
            attachment_id: AttachmentId::new("sha256:abc"),
            media_type: ImageMediaType::Webp,
            bytes: 12,
            width: 3,
            height: 4,
            name: None,
        };
        assert_eq!(
            serde_json::to_value(&reference).unwrap(),
            json!({
                "attachmentId": "sha256:abc",
                "mediaType": "image/webp",
                "bytes": 12,
                "width": 3,
                "height": 4
            })
        );
        assert_eq!(
            serde_json::from_value::<ImageAttachmentRef>(serde_json::to_value(&reference).unwrap())
                .unwrap(),
            reference
        );
        assert!(serde_json::from_value::<ImageMediaType>(json!("image/svg+xml")).is_err());
    }

    #[test]
    fn attachment_error_preserves_code_message_and_cause() {
        let error = AttachmentError::with_source(
            "Unable to persist image attachment.",
            "ATTACHMENT_WRITE_FAILED",
            std::io::Error::other("disk"),
        );
        assert_eq!(error.to_string(), "Unable to persist image attachment.");
        assert_eq!(error.code, "ATTACHMENT_WRITE_FAILED");
        assert_eq!(
            error.source().map(ToString::to_string).as_deref(),
            Some("disk")
        );
    }

    #[tokio::test]
    async fn service_delegates_and_is_owned_by_the_mounting_fiber() {
        let context = Context::new();
        let backend = Arc::new(RecordingBackend::new());
        let store = Arc::new(AttachmentStore::new(backend.clone()));
        let effect = store.provide(&context).expect("provide");
        let visible = context.get(ATTACHMENTS).expect("visible");

        visible.validate_image(&input()).await.expect("validate");
        let reference = visible.save_image(input()).await.expect("save");
        assert_eq!(backend.validations.load(Ordering::SeqCst), 1);
        assert_eq!(reference.attachment_id.as_str(), "sha256:abc");
        assert_eq!(visible.image_limits(), &backend.limits);
        assert_eq!(
            visible
                .read_image(&reference, None)
                .await
                .expect("read")
                .data,
            vec![1, 2, 3]
        );

        effect.dispose().await.expect("dispose");
        assert!(context.get(ATTACHMENTS).is_none());
    }

    #[tokio::test]
    async fn cancellation_signal_identity_and_reason_reach_the_backend() {
        let store = AttachmentStore::new(Arc::new(RecordingBackend::new()));
        let signal = AbortSignal::default();
        signal.abort_with_reason(json!({ "code": "STOP" }));
        let reference = store.save_image(input()).await.expect("save");
        let error = store
            .read_image(&reference, Some(signal))
            .await
            .expect_err("cancelled");
        assert!(format!("{error:#}").contains(r#"{"code":"STOP"}"#));
    }

    #[test]
    fn invariant_companion_reserves_renamed_package() {
        let context = Context::new();
        let registry = Arc::new(
            InvariantRegistry::new(&context, &seekdeep_invariants::InvariantConfig::default())
                .expect("registry"),
        );
        let _registration = register_invariant(&registry).expect("register");
        assert!(registry.is_registered("@deepseek-ai/seekdeep-attachment"));
    }
}
