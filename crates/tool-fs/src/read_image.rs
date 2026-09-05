//! The model-facing `read_image` tool: media handling, formatting, and registration.

use std::sync::Arc;

use seekdeep_attachment::{
    ATTACHMENTS, AttachmentError, AttachmentId, ImageAttachmentRef, ImageMediaType,
    SaveImageAttachment,
};
use seekdeep_cordis::Context;
use seekdeep_fs::{FS, FsObservation};
use seekdeep_llm::{ContentBlock, LLM, MessageSource, UserMessage};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, FileLocation, GenericCallView, TOOLS, ToolCallKind,
    ToolCallView, ToolExecution, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::read_target::{emit_fs_observed, resolve_regular_read_target};

/// Raw schema-validated `read_image` arguments.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ReadImageArgs {
    /// Path to the image file.
    pub file_path: String,
}

/// Projects one canonical image read into its model-facing envelope and image.
#[must_use]
fn image_read_content(value: &ImageReadValue) -> Vec<ContentBlock> {
    vec![
        ContentBlock::Text {
            text: format_image_read_output(&value.path, &value.image),
        },
        ContentBlock::Image {
            attachment: image_ref_from_value(&value.image),
        },
    ]
}

/// Enforces the strict image-capability gate for the calling route.
///
/// # Errors
///
/// Returns an unresolved-route or non-image-capable-model failure.
pub async fn assert_image_capable_route(
    ctx: &Context,
    exec: &ToolExecution,
    requested_path: &str,
) -> anyhow::Result<()> {
    let routed = exec
        .agent
        .as_ref()
        .and_then(|agent| agent.session().request_header())
        .map(|header| header.config);
    let provider = routed
        .as_ref()
        .map(|config| config.provider.clone())
        .or_else(|| {
            exec.agent
                .as_ref()
                .and_then(|agent| agent.options().provider.clone())
        });
    let model = routed
        .as_ref()
        .map(|config| config.model.clone())
        .or_else(|| {
            exec.agent
                .as_ref()
                .and_then(|agent| agent.options().model.clone())
        });
    let llm = ctx.get(LLM);
    let (Some(provider), Some(model), Some(llm)) = (provider, model, llm) else {
        anyhow::bail!(
            "cannot read {requested_path:?} as an image: the current model route could not be resolved"
        );
    };
    let active = llm
        .resolve_model_info(provider.as_str(), model.as_str(), Some(&exec.signal()))
        .await?;
    let image_capable = active
        .input_modalities
        .as_ref()
        .is_some_and(|modalities| modalities.iter().any(|modality| modality.0 == "image"));
    if !image_capable {
        anyhow::bail!(
            "cannot read {requested_path:?} as an image: model {:?} does not declare image input; switch to an image-capable model to read images",
            model.as_str()
        );
    }
    Ok(())
}

/// Registers the `read_image` tool into the given context.
///
/// # Errors
///
/// Returns missing-service or tool-registration failures.
#[allow(clippy::too_many_lines)]
pub fn apply_read_image_tool(ctx: &Context) -> anyhow::Result<()> {
    let tools = ctx
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires tools"))?;
    let execute_ctx = ctx.clone();
    let output = DefineToolOutput::new(
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "required": true},
                "image": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": true,
                    "properties": {
                        "attachmentId": {"type": "string", "required": true},
                        "mediaType": {"type": "string", "enum": ["image/png", "image/jpeg", "image/webp", "image/gif"], "required": true},
                        "bytes": {"type": "integer", "required": true},
                        "width": {"type": "integer", "required": true},
                        "height": {"type": "integer", "required": true},
                        "name": {"type": "string"},
                    },
                },
            },
        }),
        Arc::new(|_args: &ReadImageArgs, value: &ImageReadValue| Ok(image_read_content(value))),
    );

    let definition = define_tool(
        DefineToolOptions::new(
            "read_image",
            "Read a PNG/JPEG/WebP/GIF file and return the image itself. Requires the current model to accept image input.",
            json!({
                "file_path": {"type": "string", "required": true, "description": "Path to the image file, resolved by the filesystem backend."},
            }),
            output,
            Arc::new(move |args: ReadImageArgs, execution| {
                let ctx = execute_ctx.clone();
                Box::pin(async move {
                    if args.file_path.trim().is_empty() {
                        anyhow::bail!("file_path must be a non-empty string");
                    }
                    let media_type = image_media_type_for_path(&args.file_path);
                    let Some(media_type) = media_type else {
                        anyhow::bail!(
                            "cannot read {:?}: read_image only accepts PNG/JPEG/WebP/GIF paths",
                            args.file_path
                        );
                    };
                    let attachments = ctx.get(ATTACHMENTS).ok_or_else(|| {
                        anyhow::anyhow!(
                            "cannot read {:?} as an image: no attachment service is mounted",
                            args.file_path
                        )
                    })?;
                    if !attachments.image_limits().media_types.contains(&media_type) {
                        anyhow::bail!(
                            "cannot read {:?}: {media_type} images are not accepted by this deployment",
                            args.file_path
                        );
                    }
                    assert_image_capable_route(&ctx, &execution, &args.file_path).await?;

                    let (target, info) =
                        resolve_regular_read_target(&ctx, &execution, &args.file_path).await?;
                    let filesystem = ctx
                        .get(FS)
                        .ok_or_else(|| anyhow::anyhow!("tool-fs requires fs"))?
                        .filesystem();
                    let limits = attachments.image_limits();
                    let byte_cap = usize::try_from(
                        limits.max_image_bytes.min(limits.max_message_image_bytes),
                    )
                    .unwrap_or(usize::MAX);
                    let data = filesystem
                        .read_bytes(&target, Some(&execution.signal()), byte_cap)
                        .await?;
                    let name = std::path::Path::new(&target.display_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned);
                    let reference = match attachments
                        .save_image(SaveImageAttachment {
                            data: data.clone(),
                            media_type,
                            name,
                        })
                        .await
                    {
                        Ok(reference) => reference,
                        Err(error) => {
                            let is_mismatch = error.downcast_ref::<AttachmentError>().is_some_and(
                                |attachment| attachment.code == "IMAGE_TYPE_MISMATCH",
                            );
                            if !is_mismatch {
                                return Err(error);
                            }
                            let extension = std::path::Path::new(&target.display_path)
                                .extension()
                                .and_then(|extension| extension.to_str())
                                .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
                                .unwrap_or_default();
                            anyhow::bail!(
                                "cannot read {:?}: the {extension} extension declares {media_type}, but the bytes use a different image format; rename the file to match its actual format if it is PNG/JPEG/WebP/GIF, or convert it to one of those formats",
                                target.display_path
                            );
                        }
                    };
                    emit_fs_observed(
                        &ctx,
                        &target,
                        FsObservation::Present {
                            version: info.version,
                        },
                        &execution,
                    )?;
                    let value = ImageReadValue {
                        path: target.display_path.clone(),
                        image: ImageMetadata {
                            attachment_id: reference.attachment_id.into_string(),
                            media_type: reference.media_type,
                            bytes: reference.bytes,
                            width: reference.width,
                            height: reference.height,
                            name: reference.name,
                        },
                    };
                    if execution.parent.is_some() {
                        execution.defer_context(UserMessage::new(
                            image_read_content(&value),
                            MessageSource::plugin("tool-fs"),
                        ));
                    }
                    Ok(value)
                })
            }),
        )
        .concurrency_safe(Arc::new(|_args: &ReadImageArgs| true))
        .present_call(Arc::new(|args: &ReadImageArgs| {
            Some(ToolCallView::Generic(GenericCallView {
                title: format!("Read image {}", args.file_path),
                kind: Some(ToolCallKind::Read),
                raw_input: None,
                content: None,
                locations: Some(vec![FileLocation {
                    path: args.file_path.clone(),
                    line: None,
                }]),
            }))
        })),
    )?;
    tools.register(ctx, definition)?;
    Ok(())
}

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
