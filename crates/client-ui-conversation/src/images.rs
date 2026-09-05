//! Image attachment limit and rejection copy.

use seekdeep_attachment::ImageAttachmentLimits;

/// Supported GUI locale for built-in image error copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageCopyLocale {
    /// Simplified Chinese.
    Zh,
    /// English.
    En,
}

/// Formats bytes as rounded user-facing mebibytes.
#[must_use]
pub fn image_size_text(bytes: f64) -> String {
    let mebibytes = bytes / (1024.0 * 1024.0);
    if mebibytes.fract() == 0.0 {
        format!("{mebibytes:.0}MB")
    } else {
        format!("{mebibytes:.1}MB")
    }
}

fn image_size_text_u64(bytes: u64) -> String {
    const MEBIBYTE: u128 = 1024 * 1024;
    let bytes = u128::from(bytes);
    if bytes % MEBIBYTE == 0 {
        return format!("{}MB", bytes / MEBIBYTE);
    }
    let tenths = (bytes * 10 + MEBIBYTE / 2) / MEBIBYTE;
    format!("{}.{:01}MB", tenths / 10, tenths % 10)
}

/// Resolves product copy for one Host attachment rejection.
#[must_use]
pub fn attachment_error_text(
    locale: ImageCopyLocale,
    reason: &str,
    limits: Option<&ImageAttachmentLimits>,
) -> String {
    match (locale, reason, limits) {
        (ImageCopyLocale::Zh, "MODEL_DOES_NOT_SUPPORT_IMAGES", _) => {
            "当前模型不支持图片，请切换支持图片的模型".to_owned()
        }
        (ImageCopyLocale::En, "MODEL_DOES_NOT_SUPPORT_IMAGES", _) => {
            "The current model does not support images; switch to a model that does".to_owned()
        }
        (ImageCopyLocale::Zh, "SUBAGENT_IMAGE_UNSUPPORTED", _) => {
            "子智能体会话暂不支持图片".to_owned()
        }
        (ImageCopyLocale::En, "SUBAGENT_IMAGE_UNSUPPORTED", _) => {
            "Subagent sessions do not support images yet".to_owned()
        }
        (ImageCopyLocale::Zh, "IMAGE_TOO_MANY_PIXELS", _) => {
            "图片分辨率过大，请压缩后重试".to_owned()
        }
        (ImageCopyLocale::En, "IMAGE_TOO_MANY_PIXELS", _) => {
            "Image resolution is too high; compress it and try again".to_owned()
        }
        (ImageCopyLocale::Zh, "INVALID_IMAGE" | "IMAGE_TYPE_MISMATCH", _) => {
            "仅支持 PNG、JPG、WebP、GIF 格式的图片".to_owned()
        }
        (ImageCopyLocale::En, "INVALID_IMAGE" | "IMAGE_TYPE_MISMATCH", _) => {
            "Only PNG, JPG, WebP, and GIF images are supported".to_owned()
        }
        (ImageCopyLocale::Zh, "TOO_MANY_IMAGES", Some(limits)) => {
            format!("一条消息最多添加 {} 张图片", limits.max_images_per_message)
        }
        (ImageCopyLocale::En, "TOO_MANY_IMAGES", Some(limits)) => {
            format!(
                "A message can include up to {} images",
                limits.max_images_per_message
            )
        }
        (ImageCopyLocale::Zh, "IMAGE_TOO_LARGE", Some(limits)) => {
            format!(
                "单张图片不能超过 {}",
                image_size_text_u64(limits.max_image_bytes)
            )
        }
        (ImageCopyLocale::En, "IMAGE_TOO_LARGE", Some(limits)) => format!(
            "Each image must be smaller than {}",
            image_size_text_u64(limits.max_image_bytes)
        ),
        (ImageCopyLocale::Zh, "IMAGES_TOO_LARGE", Some(limits)) => format!(
            "图片总大小超过 {}，请移除部分图片",
            image_size_text_u64(limits.max_message_image_bytes)
        ),
        (ImageCopyLocale::En, "IMAGES_TOO_LARGE", Some(limits)) => format!(
            "Images exceed {} in total; remove some and try again",
            image_size_text_u64(limits.max_message_image_bytes)
        ),
        (ImageCopyLocale::Zh, _, _) => {
            format!("图片发送失败（{reason}），请重新添加图片后再试")
        }
        (ImageCopyLocale::En, _, _) => {
            format!("Sending images failed ({reason}); re-add them and try again")
        }
    }
}
