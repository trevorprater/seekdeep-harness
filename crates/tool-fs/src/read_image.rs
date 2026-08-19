//! The model-facing `read_image` tool: pure path/media handling and formatting.

use seekdeep_attachment::{AttachmentId, ImageAttachmentRef, ImageMediaType};
use serde::{Deserialize, Serialize};

/// Canonical image metadata in the `read_image` output schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageMetadata {
    /// Durable attachment identity.
    pub attachment_id: String,
    /// Verified media type.
    pub media_type: ImageMediaType,
    /// Exact encoded byte length.
    pub bytes: u64,
    /// Intrinsic encoded width in pixels.
    pub width: u64,
    /// Intrinsic encoded height in pixels.
    pub height: u64,
    /// Optional display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The canonical `read_image` outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageReadValue {
    /// Resolved model-facing path.
    pub path: String,
    /// Canonical image metadata.
    pub image: ImageMetadata,
}

/// Maps a model-supplied path to its declared image media type by extension.
#[must_use]
pub fn image_media_type_for_path(file_path: &str) -> Option<ImageMediaType> {
    let ext = std::path::Path::new(file_path).extension()?.to_str()?;
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some(ImageMediaType::Png),
        "jpg" | "jpeg" => Some(ImageMediaType::Jpeg),
        "webp" => Some(ImageMediaType::Webp),
        "gif" => Some(ImageMediaType::Gif),
        _ => None,
    }
}

/// Re-brands a canonical image outcome into the durable attachment reference.
#[must_use]
pub fn image_ref_from_value(image: &ImageMetadata) -> ImageAttachmentRef {
    ImageAttachmentRef {
        attachment_id: AttachmentId::new(image.attachment_id.clone()),
        media_type: image.media_type,
        bytes: image.bytes,
        width: image.width,
        height: image.height,
        name: image.name.clone(),
    }
}

/// Formats an image read as the model-facing envelope beside its image block.
#[must_use]
pub fn format_image_read_output(display_path: &str, image: &ImageMetadata) -> String {
    format!(
        "<path>{display_path}</path>\n<type>image</type>\n<content>\n{} image, {}x{} px, {} bytes\n</content>",
        image.media_type.as_str(),
        image.width,
        image.height,
        image.bytes
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_extensions_case_insensitively() {
        assert_eq!(
            image_media_type_for_path("a.PNG"),
            Some(ImageMediaType::Png)
        );
        assert_eq!(
            image_media_type_for_path("a.jpeg"),
            Some(ImageMediaType::Jpeg)
        );
        assert_eq!(
            image_media_type_for_path("a.webp"),
            Some(ImageMediaType::Webp)
        );
        assert_eq!(
            image_media_type_for_path("a.gif"),
            Some(ImageMediaType::Gif)
        );
        assert_eq!(image_media_type_for_path("a.txt"), None);
        assert_eq!(image_media_type_for_path("noext"), None);
    }

    #[test]
    fn ref_and_output_render_canonically() {
        let image = ImageMetadata {
            attachment_id: "id1".to_owned(),
            media_type: ImageMediaType::Png,
            bytes: 100,
            width: 20,
            height: 10,
            name: Some("a.png".to_owned()),
        };
        let reference = image_ref_from_value(&image);
        assert_eq!(reference.attachment_id.as_str(), "id1");
        assert_eq!(reference.media_type, ImageMediaType::Png);

        let rendered = format_image_read_output("/a.png", &image);
        assert!(rendered.contains("<type>image</type>"));
        assert!(rendered.contains("image/png image, 20x10 px, 100 bytes"));
    }
}
