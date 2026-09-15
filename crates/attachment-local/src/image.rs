//! Raster inspection with full-decode admission and header-only verified reads.

use std::io::Cursor;

use image::{GenericImageView as _, ImageFormat, ImageReader};
use seekdeep_attachment::{AttachmentError, ImageMediaType};

/// Decoded metadata from a supported raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetectedImage {
    /// Verified image MIME type.
    pub media_type: ImageMediaType,
    /// Intrinsic image width.
    pub width: u64,
    /// Intrinsic image height.
    pub height: u64,
}

fn supported_format(data: &[u8]) -> Result<(ImageFormat, ImageMediaType), AttachmentError> {
    let format = image::guess_format(data).map_err(invalid_image)?;
    let media_type = match format {
        ImageFormat::Png => ImageMediaType::Png,
        ImageFormat::Jpeg => ImageMediaType::Jpeg,
        ImageFormat::WebP => ImageMediaType::Webp,
        ImageFormat::Gif => ImageMediaType::Gif,
        _ => {
            return Err(AttachmentError::new(
                "Unsupported or malformed image data.",
                "INVALID_IMAGE",
            ));
        }
    };
    Ok((format, media_type))
}

fn invalid_image(error: impl std::error::Error + Send + Sync + 'static) -> AttachmentError {
    AttachmentError::with_source(
        "Unsupported or malformed image data.",
        "INVALID_IMAGE",
        error,
    )
}

/// Parses a supported raster header without decoding pixels.
///
/// Digest-verified reads use this because admission already proved that these
/// exact bytes decode completely.
///
/// # Errors
///
/// Returns `INVALID_IMAGE` for unsupported, malformed, or unreadable headers.
pub fn probe_image(data: &[u8]) -> Result<DetectedImage, AttachmentError> {
    let (format, media_type) = supported_format(data)?;
    let (width, height) = ImageReader::with_format(Cursor::new(data), format)
        .into_dimensions()
        .map_err(invalid_image)?;
    Ok(DetectedImage {
        media_type,
        width: u64::from(width),
        height: u64::from(height),
    })
}

/// Fully decodes a supported raster and returns its intrinsic metadata.
///
/// # Errors
///
/// Returns `IMAGE_TOO_MANY_PIXELS` when the header exceeds `max_pixels`, or
/// `INVALID_IMAGE` when the format is unsupported or the complete raster does
/// not decode successfully.
pub fn detect_image(
    data: &[u8],
    max_pixels: Option<u64>,
) -> Result<DetectedImage, AttachmentError> {
    let detected = probe_image(data)?;
    if max_pixels.is_some_and(|limit| detected.width.saturating_mul(detected.height) > limit) {
        return Err(AttachmentError::new(
            "Image exceeds the configured decoded-pixel limit.",
            "IMAGE_TOO_MANY_PIXELS",
        ));
    }
    let (format, _) = supported_format(data)?;
    let image = image::load_from_memory_with_format(data, format).map_err(invalid_image)?;
    let (width, height) = image.dimensions();
    if u64::from(width) != detected.width || u64::from(height) != detected.height {
        return Err(AttachmentError::new(
            "Unsupported or malformed image data.",
            "INVALID_IMAGE",
        ));
    }
    Ok(detected)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

    use super::*;

    fn raster(format: ImageFormat) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, Rgba([1, 2, 3, 255])));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn decodes_every_supported_format_and_dimensions() {
        for (format, media_type) in [
            (ImageFormat::Png, ImageMediaType::Png),
            (ImageFormat::Jpeg, ImageMediaType::Jpeg),
            (ImageFormat::WebP, ImageMediaType::Webp),
            (ImageFormat::Gif, ImageMediaType::Gif),
        ] {
            assert_eq!(
                detect_image(&raster(format), None).unwrap(),
                DetectedImage {
                    media_type,
                    width: 3,
                    height: 2,
                }
            );
        }
    }

    #[test]
    fn pixel_limit_precedes_full_decode_and_truncation_fails_admission() {
        assert_eq!(
            detect_image(&raster(ImageFormat::Png), Some(5))
                .unwrap_err()
                .code,
            "IMAGE_TOO_MANY_PIXELS"
        );
        let complete = raster(ImageFormat::Png);
        let truncation = (1..complete.len())
            .find(|length| {
                probe_image(&complete[..*length]).is_ok()
                    && detect_image(&complete[..*length], None).is_err()
            })
            .expect("header-readable truncated PNG");
        let truncated = &complete[..truncation];
        assert_eq!(probe_image(truncated).unwrap().width, 3);
        assert_eq!(
            detect_image(truncated, None).unwrap_err().code,
            "INVALID_IMAGE"
        );
    }

    #[test]
    fn malformed_and_unsupported_inputs_share_stable_error() {
        for bytes in [&[1, 2, 3][..], b"II*\0\x08\0\0\0\0\0\0\0".as_slice()] {
            assert_eq!(probe_image(bytes).unwrap_err().code, "INVALID_IMAGE");
            assert_eq!(detect_image(bytes, None).unwrap_err().code, "INVALID_IMAGE");
        }
    }
}
