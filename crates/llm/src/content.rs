//! Content-block structural helpers.

use crate::ContentBlock;

/// Whether content contains an image at any nested tool-result depth.
#[must_use]
pub fn content_has_image(content: &[ContentBlock]) -> bool {
    let mut pending = content.iter().collect::<Vec<_>>();
    while let Some(block) = pending.pop() {
        match block {
            ContentBlock::Image { .. } => return true,
            ContentBlock::ToolResult { content, .. } => pending.extend(content),
            ContentBlock::Text { .. }
            | ContentBlock::Reasoning { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::Unknown { .. } => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::CallId;

    #[test]
    fn finds_images_inside_nested_tool_results() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_call_id: CallId::new("c"),
            is_error: None,
            content: vec![ContentBlock::Image {
                attachment: json!({"id": "image"}),
            }],
        }];
        assert!(content_has_image(&blocks));
    }
}
